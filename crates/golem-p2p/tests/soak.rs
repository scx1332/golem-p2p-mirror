//! Soak tests of the library: many connections and requests in one process,
//! checking that the endpoint's state returns to its idle values.
//!
//! `connections_are_released` and `failed_connects_are_released` run in the
//! normal test suite with a few dozen connections. The `#[ignore]`d `soak_*`
//! tests run thousands of cycles, print the memory, task and table sizes at
//! checkpoints, and check that they do not grow with the number of
//! connections served:
//!
//! ```sh
//! cargo test --release -p golem-p2p --test soak -- --ignored --nocapture --test-threads 1
//! GOLEM_P2P_SOAK_CYCLES=10000 cargo test --release -p golem-p2p --test soak -- --ignored --nocapture
//! ```

mod common;

use std::time::{Duration, Instant};

use common::*;
use golem_p2p::{Endpoint, EndpointStats, Error, Identities, InMemorySigner, TransportMode};

/// Resident and anonymous resident memory of this process in KiB (Linux).
fn memory_kb() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    };
    (field("VmRSS"), field("RssAnon"))
}

fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

fn fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

/// The sizes in `s` that must be back at zero when no connection is open and
/// no connect or request is in flight. The lazily pruned connect slot and
/// dial lock of the last target may remain.
fn assert_idle(s: &EndpointStats, what: &str) {
    assert_eq!(
        s.connections, 0,
        "{what}: connection table not empty: {s:?}"
    );
    assert_eq!(s.handshakes, 0, "{what}: handshakes in progress: {s:?}");
    assert_eq!(s.queued_streams, 0, "{what}: queued streams: {s:?}");
    assert!(s.connect_slots <= 1, "{what}: connect slots kept: {s:?}");
    assert!(s.dial_locks <= 1, "{what}: dial locks kept: {s:?}");
}

async fn node(net: &TestNet, opts: NodeOpts) -> (Endpoint, InMemorySigner) {
    let signer = InMemorySigner::new();
    let id = signer.generate();
    let ep = net.node(&signer, Identities::new(id), opts).await;
    (ep, signer)
}

/// Many short-lived clients (each a new identity and endpoint key, as after
/// a restart) against one server: every connection, its tasks and its table
/// entries are released on the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connections_are_released() {
    with_timeout(120, async {
        let net = TestNet::new(0).await;
        let (server, _s) = node(&net, NodeOpts::default()).await;
        let _echo = spawn_echo(&server);
        let to = server.node_id();
        const CLIENTS: u64 = 20;
        for i in 0..CLIENTS {
            let (client, _c) = node(&net, NodeOpts::default()).await;
            for j in 0..3 {
                let e = echo(&client, to, &format!("{i}/{j}")).await.expect("echo");
                assert_eq!(e.data, format!("{i}/{j}"));
            }
            let s = client.stats();
            assert_eq!(s.connections_outgoing, 1, "{s:?}");
            client.close().await;
        }
        wait_for(20, "server connections closed", || {
            server.stats().connections == 0
        })
        .await;
        let s = server.stats();
        assert_idle(&s, "server");
        assert_eq!(s.connections_incoming, CLIENTS, "{s:?}");
        assert_eq!(s.connections_closed, CLIENTS, "{s:?}");
        assert_eq!(s.streams_accepted, 3 * CLIENTS, "{s:?}");
        assert_eq!(s.streams_refused, 0, "{s:?}");
        assert_eq!(s.handshakes_failed, 0, "{s:?}");
        assert_eq!(s.connect_slots, 0, "the server never dialed: {s:?}");
        assert!(server.connections().is_empty());

        // One long-lived client against many short-lived servers.
        let (client, _c) = node(&net, NodeOpts::default()).await;
        const SERVERS: u64 = 10;
        for i in 0..SERVERS {
            let (srv, _s) = node(&net, NodeOpts::default()).await;
            let echo_task = spawn_echo(&srv);
            echo(&client, srv.node_id(), &format!("{i}"))
                .await
                .expect("echo");
            srv.close().await;
            echo_task.abort();
        }
        wait_for(20, "client connections closed", || {
            client.stats().connections == 0
        })
        .await;
        let s = client.stats();
        assert_idle(&s, "client");
        assert_eq!(s.connections_outgoing, SERVERS, "{s:?}");
        assert_eq!(s.connections_closed, SERVERS, "{s:?}");
        client.close().await;
        server.close().await;
    })
    .await;
}

/// Connects that fail (unknown node, dead node) leave no state behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_connects_are_released() {
    with_timeout(120, async {
        let net = TestNet::new(0).await;
        let (client, _c) = node(&net, NodeOpts::default()).await;
        let unknown = InMemorySigner::new();
        for _ in 0..20 {
            let id = unknown.generate();
            assert!(matches!(
                client.request(id, b"x").await,
                Err(Error::NotFound(_))
            ));
        }
        // A node that published its record and went away.
        let (dead, _d) = node(&net, NodeOpts::default()).await;
        let dead_id = dead.node_id();
        dead.close().await;
        drop(dead);
        let started = Instant::now();
        let res =
            tokio::time::timeout(Duration::from_secs(30), client.request(dead_id, b"x")).await;
        assert!(matches!(res, Ok(Err(_))), "{res:?}");
        assert!(started.elapsed() < Duration::from_secs(25));
        let s = client.stats();
        assert_idle(&s, "client");
        assert_eq!(s.connections_outgoing, 0, "{s:?}");
        client.close().await;
    })
    .await;
}

/// A checkpoint of a soak test.
struct Checkpoint {
    cycles: u64,
    secs: f64,
    rss_kb: u64,
    anon_kb: u64,
    tasks: usize,
    fds: usize,
    stats: EndpointStats,
}

impl Checkpoint {
    fn take(cycles: u64, start: Instant, ep: &Endpoint) -> Self {
        let (rss_kb, anon_kb) = memory_kb();
        let c = Self {
            cycles,
            secs: start.elapsed().as_secs_f64(),
            rss_kb,
            anon_kb,
            tasks: alive_tasks(),
            fds: fds(),
            stats: ep.stats(),
        };
        let s = &c.stats;
        println!(
            "cycles={:>6} t={:>7.1}s rss_kb={:>7} anon_kb={:>7} tasks={:>4} fds={:>4} conns={} \
             slots={} locks={} cached={}/{} in={} out={} closed={} streams={} refused={} failed_hs={}",
            c.cycles,
            c.secs,
            c.rss_kb,
            c.anon_kb,
            c.tasks,
            c.fds,
            s.connections,
            s.connect_slots,
            s.dial_locks,
            s.cached_identities,
            s.cached_endpoints,
            s.connections_incoming,
            s.connections_outgoing,
            s.connections_closed,
            s.streams_accepted,
            s.streams_refused,
            s.handshakes_failed,
        );
        c
    }
}

/// Least squares slope of y over x.
fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx == 0.0 { 0.0 } else { sxy / sxx }
}

fn soak_cycles(default: u64) -> u64 {
    std::env::var("GOLEM_P2P_SOAK_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Runs `cycles` client sessions against one server, `parallel` at a time:
/// each session binds a new client (new identity, new endpoint key), sends
/// `requests` echo requests and closes. Prints checkpoints and checks that
/// the server's tables, the runtime's tasks and file descriptors return to
/// their idle values, and that memory does not grow with the sessions served.
async fn soak_server(net: &TestNet, client_opts: NodeOpts, server_opts: NodeOpts, cycles: u64) {
    const PARALLEL: u64 = 8;
    const REQUESTS: usize = 5;
    let (server, _s) = node(net, server_opts).await;
    let _echo = spawn_echo(&server);
    let to = server.node_id();
    let start = Instant::now();
    // Warm up: a few sessions so that one-time allocations are done.
    let session = |i: u64| {
        let net_opts = client_opts.clone();
        async move {
            let (client, _c) = node(net, net_opts).await;
            for j in 0..REQUESTS {
                echo_eventually(&client, to, &format!("{i}/{j}"), 20).await;
            }
            client.close().await;
        }
    };
    for i in 0..PARALLEL {
        session(i).await;
    }
    wait_for(30, "warm-up connections closed", || {
        server.stats().connections == 0
    })
    .await;
    let base = Checkpoint::take(0, start, &server);
    let mut points = vec![];
    let step = (cycles / 10).max(PARALLEL);
    let mut done = 0;
    while done < cycles {
        let batch = step.min(cycles - done);
        let mut i = 0;
        while i < batch {
            let n = PARALLEL.min(batch - i);
            let futs: Vec<_> = (0..n).map(|k| session(done + i + k)).collect();
            n0_future::join_all(futs).await;
            i += n;
        }
        done += batch;
        wait_for(30, "connections closed", || server.stats().connections == 0).await;
        let c = Checkpoint::take(done, start, &server);
        assert_idle(&c.stats, "server");
        points.push(c);
    }
    // iroh keeps per-remote state for 60 s after the last connection.
    println!("waiting 65 s for idle per-remote state to expire");
    tokio::time::sleep(Duration::from_secs(65)).await;
    let after = Checkpoint::take(done, start, &server);
    let s = &after.stats;
    assert_idle(s, "server after idle");
    assert_eq!(s.connections_closed, s.connections_incoming, "{s:?}");
    assert_eq!(
        s.streams_accepted,
        (cycles + PARALLEL) * REQUESTS as u64,
        "every request reached the server once: {s:?}"
    );
    // Memory per session after the first checkpoints (allocator warm-up).
    let tail: Vec<(f64, f64)> = points
        .iter()
        .skip(points.len() / 5)
        .map(|c| (c.cycles as f64, c.rss_kb as f64))
        .collect();
    let anon: Vec<(f64, f64)> = points
        .iter()
        .skip(points.len() / 5)
        .map(|c| (c.cycles as f64, c.anon_kb as f64))
        .collect();
    let rss_per_session = slope(&tail) * 1024.0;
    let anon_per_session = slope(&anon) * 1024.0;
    println!(
        "baseline: rss_kb={} anon_kb={} tasks={} fds={}; after {cycles} sessions and idle: \
         rss_kb={} anon_kb={} tasks={} fds={}; growth per session after warm-up: \
         rss {rss_per_session:.0} B, anon {anon_per_session:.0} B",
        base.rss_kb,
        base.anon_kb,
        base.tasks,
        base.fds,
        after.rss_kb,
        after.anon_kb,
        after.tasks,
        after.fds
    );
    assert!(
        after.tasks <= base.tasks + 4,
        "tasks did not return to idle: {} before, {} after",
        base.tasks,
        after.tasks
    );
    assert!(
        after.fds <= base.fds + 2,
        "file descriptors did not return to idle: {} before, {} after",
        base.fds,
        after.fds
    );
    // A leaked connection costs tens of KiB (buffers, tasks, crypto state).
    assert!(
        anon_per_session < 8.0 * 1024.0,
        "memory grows by {anon_per_session:.0} B per session"
    );
    server.close().await;
}

/// Direct connections: thousands of client sessions against one server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak test, minutes; run with --ignored"]
async fn soak_server_direct() {
    with_timeout(3600, async {
        let net = TestNet::new(0).await;
        soak_server(
            &net,
            NodeOpts::default(),
            NodeOpts::default(),
            soak_cycles(2000),
        )
        .await;
    })
    .await;
}

/// Relayed connections: client sessions in relay-only mode through a local
/// relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak test, minutes; run with --ignored"]
async fn soak_server_relay() {
    with_timeout(3600, async {
        let net = TestNet::new(1).await;
        let opts = NodeOpts::relays(&[0], TransportMode::RelayOnly);
        soak_server(&net, opts.clone(), opts, soak_cycles(1000)).await;
    })
    .await;
}

/// One long-lived client against many short-lived servers (new identity and
/// endpoint each): the client's tables, tasks and memory do not grow with the
/// number of peers it has talked to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak test, minutes; run with --ignored"]
async fn soak_client_many_peers() {
    with_timeout(3600, async {
        let net = TestNet::new(0).await;
        let cycles = soak_cycles(2000);
        let (client, _c) = node(&net, NodeOpts::default()).await;
        let start = Instant::now();
        let session = async |i: u64| {
            let (srv, _s) = node(&net, NodeOpts::default()).await;
            let task = spawn_echo(&srv);
            echo_eventually(&client, srv.node_id(), &format!("{i}"), 20).await;
            srv.close().await;
            task.abort();
        };
        for i in 0..8 {
            session(i).await;
        }
        wait_for(30, "connections closed", || client.stats().connections == 0).await;
        let base = Checkpoint::take(0, start, &client);
        let mut points = vec![];
        let step = (cycles / 10).max(1);
        let mut done = 0;
        while done < cycles {
            for i in 0..step.min(cycles - done) {
                session(done + i).await;
            }
            done += step.min(cycles - done);
            wait_for(30, "connections closed", || client.stats().connections == 0).await;
            let c = Checkpoint::take(done, start, &client);
            assert_idle(&c.stats, "client");
            points.push(c);
        }
        println!("waiting 65 s for idle per-remote state to expire");
        tokio::time::sleep(Duration::from_secs(65)).await;
        let after = Checkpoint::take(done, start, &client);
        assert_idle(&after.stats, "client after idle");
        let anon: Vec<(f64, f64)> = points
            .iter()
            .skip(points.len() / 5)
            .map(|c| (c.cycles as f64, c.anon_kb as f64))
            .collect();
        let per_peer = slope(&anon) * 1024.0;
        println!(
            "baseline: anon_kb={} tasks={} fds={}; after {cycles} peers and idle: anon_kb={} \
             tasks={} fds={}; anon growth per peer after warm-up: {per_peer:.0} B",
            base.anon_kb, base.tasks, base.fds, after.anon_kb, after.tasks, after.fds
        );
        assert!(
            after.tasks <= base.tasks + 4,
            "{} -> {}",
            base.tasks,
            after.tasks
        );
        assert!(after.fds <= base.fds + 2, "{} -> {}", base.fds, after.fds);
        assert!(
            per_peer < 8.0 * 1024.0,
            "memory grows by {per_peer:.0} B per peer"
        );
        client.close().await;
    })
    .await;
}
