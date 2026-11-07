//! Connectivity: direct, relayed, multiple relays, relay failure.

mod common;

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use common::*;
use golem_p2p::{
    Discovery, DiscoveryError, Endpoint, Identities, InMemorySigner, MemoryDiscovery, NodeId,
    PathKind, TransportMode, VerifiedRecord, iroh::Watcher, signer::BoxFuture,
};

/// Two nodes without relays reach each other over direct UDP.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_connection() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let a = net
            .node(&signer, Identities::new(a_id), NodeOpts::default())
            .await;
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        spawn_echo(&b);
        spawn_echo(&a);

        assert!(b.local_record().record().relay_urls.is_empty());
        assert!(!b.local_record().record().direct_addrs.is_empty());

        let e = echo(&a, b_id, "hello").await.unwrap();
        assert_eq!(
            e,
            Echo {
                to: b_id,
                from: a_id,
                server_endpoint: b.endpoint_id().to_string(),
                data: "hello".into()
            }
        );
        assert_eq!(path_to(&a, b_id).await, PathKind::Direct);

        // The connection is usable in the other direction too.
        let e = echo(&b, a_id, "back").await.unwrap();
        assert_eq!((e.to, e.from, e.data.as_str()), (a_id, b_id, "back"));

        // A larger payload round-trips intact.
        let big = "x".repeat(300_000);
        assert_eq!(echo(&a, b_id, &big).await.unwrap().data, big);

        a.close().await;
        b.close().await;
    })
    .await;
}

/// With UDP disabled on both sides, traffic must go through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_only_connection() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let opts = NodeOpts::relays(&[0], TransportMode::RelayOnly);
        let a = net.node(&signer, Identities::new(a_id), opts.clone()).await;
        let b = net.node(&signer, Identities::new(b_id), opts).await;
        spawn_echo(&b);

        let rec = b.local_record();
        assert_eq!(rec.relay_urls(), vec![net.relays[0].url.clone()]);
        assert!(rec.record().direct_addrs.is_empty());

        let e = echo(&a, b_id, "via relay").await.unwrap();
        assert_eq!((e.to, e.from, e.data.as_str()), (b_id, a_id, "via relay"));

        let conn = a.connect(b_id).await.unwrap();
        assert_eq!(conn.path(), PathKind::Relay);
        // Every path of the connection is a relay path.
        let paths = conn.iroh_connection().paths();
        assert!(
            paths.iter().all(|p| p.is_relay()),
            "unexpected non-relay path"
        );
        // And it stays relayed after more traffic.
        for i in 0..5 {
            let e = echo(&a, b_id, &format!("msg {i}")).await.unwrap();
            assert_eq!(e.data, format!("msg {i}"));
        }
        assert_eq!(conn.path(), PathKind::Relay);

        a.close().await;
        b.close().await;
    })
    .await;
}

/// Both nodes have UDP and a relay; the connection starts with the signed
/// relay and direct addresses and ends up on a direct path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_assisted_becomes_direct() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let opts = NodeOpts::relays(&[0], TransportMode::DirectAndRelay);
        let a = net.node(&signer, Identities::new(a_id), opts.clone()).await;
        let b = net.node(&signer, Identities::new(b_id), opts).await;
        spawn_echo(&b);

        let rec = b.local_record();
        assert_eq!(rec.relay_urls(), vec![net.relays[0].url.clone()]);
        assert!(!rec.record().direct_addrs.is_empty());

        let e = echo(&a, b_id, "hi").await.unwrap();
        assert_eq!(e.data, "hi");
        let conn = a.connect(b_id).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while conn.path() != PathKind::Direct {
            assert!(tokio::time::Instant::now() < deadline, "no direct path");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(echo(&a, b_id, "direct").await.unwrap().data, "direct");

        a.close().await;
        b.close().await;
    })
    .await;
}

/// Node A is homed on relay 0 only, node B on relay 1 only. They still reach
/// each other, in both directions, with discovery through the HTTP directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_relays_different_homes() {
    with_timeout(60, async {
        let dir = TestDirectory::spawn().await;
        let net = TestNet::with_discovery(2, std::sync::Arc::new(dir.client())).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let a = net
            .node(
                &signer,
                Identities::new(a_id),
                NodeOpts::relays(&[0], TransportMode::RelayOnly),
            )
            .await;
        let b = net
            .node(
                &signer,
                Identities::new(b_id),
                NodeOpts::relays(&[1], TransportMode::RelayOnly),
            )
            .await;
        spawn_echo(&a);
        spawn_echo(&b);

        assert_eq!(
            a.local_record().relay_urls(),
            vec![net.relays[0].url.clone()]
        );
        assert_eq!(
            b.local_record().relay_urls(),
            vec![net.relays[1].url.clone()]
        );
        // The directory holds both records.
        assert!(dir.store.resolve(&a_id).is_some());
        assert!(dir.store.resolve(&b_id).is_some());

        let e = echo(&a, b_id, "a->b").await.unwrap();
        assert_eq!((e.to, e.from, e.data.as_str()), (b_id, a_id, "a->b"));
        assert_eq!(path_to(&a, b_id).await, PathKind::Relay);

        // B dials A on its own (fresh connection from B's side).
        for c in b.connections() {
            c.close();
        }
        for c in a.connections() {
            c.close();
        }
        wait_for(10, "connections closed", || {
            a.connections().is_empty() && b.connections().is_empty()
        })
        .await;
        let e = echo(&b, a_id, "b->a").await.unwrap();
        assert_eq!((e.to, e.from, e.data.as_str()), (a_id, b_id, "b->a"));

        a.close().await;
        b.close().await;
    })
    .await;
}

/// Both nodes know two relays. The relay that B is homed on goes down; B moves
/// to the other relay, republishes, and A reaches B again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_down_other_stays_up() {
    with_timeout(120, async {
        let mut net = TestNet::new(2).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let opts = NodeOpts::relays(&[0, 1], TransportMode::RelayOnly);
        let a = net.node(&signer, Identities::new(a_id), opts.clone()).await;
        let b = net.node(&signer, Identities::new(b_id), opts).await;
        spawn_echo(&b);

        assert_eq!(echo(&a, b_id, "before").await.unwrap().data, "before");

        let b_home = b.local_record().relay_urls();
        assert_eq!(b_home.len(), 1);
        let dead = net.relays.iter().position(|r| r.url == b_home[0]).unwrap();
        let alive_url = net.relays[1 - dead].url.clone();
        net.relays[dead].shutdown().await;

        // B notices, switches home relay and publishes a new record.
        let mut w = b.iroh().watch_addr();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while b.local_record().relay_urls() != vec![alive_url.clone()] {
            assert!(
                tokio::time::Instant::now() < deadline,
                "B did not move to the live relay"
            );
            let _ = tokio::time::timeout(Duration::from_millis(500), w.updated()).await;
        }

        let e = echo_eventually(&a, b_id, "after", 60).await;
        assert_eq!((e.to, e.from, e.data.as_str()), (b_id, a_id, "after"));
        assert_eq!(path_to(&a, b_id).await, PathKind::Relay);

        a.close().await;
        b.close().await;
    })
    .await;
}

/// Discovery that counts lookups.
#[derive(Debug, Default)]
struct CountingDiscovery {
    inner: MemoryDiscovery,
    lookups: AtomicUsize,
}

impl Discovery for CountingDiscovery {
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
        self.inner.publish(record)
    }

    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.inner.lookup(id)
    }
}

/// Many requests to a peer without a connection yet: one directory lookup
/// and one connection serve all of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_share_one_lookup_and_dial() {
    with_timeout(60, async {
        let discovery = Arc::new(CountingDiscovery::default());
        let net = TestNet::with_discovery(0, discovery.clone()).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let a = net
            .node(&signer, Identities::new(a_id), NodeOpts::default())
            .await;
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        spawn_echo(&b);

        let before = discovery.lookups.load(Ordering::SeqCst);
        let replies = n0_future::join_all((0..16).map(|_| a.request(b_id, b"hello"))).await;
        for r in replies {
            assert_eq!(Echo::parse(&r.unwrap()).to, b_id);
        }
        let lookups = discovery.lookups.load(Ordering::SeqCst) - before;
        assert_eq!(lookups, 1, "16 concurrent requests made {lookups} lookups");
        assert_eq!(a.connections().len(), 1);

        a.close().await;
        b.close().await;
    })
    .await;
}

/// Concurrent requests to a peer whose record is still published but whose
/// process is gone fail together after one connect timeout, instead of
/// dialing one after another for one timeout each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_to_dead_peer_fail_together() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        b.close().await;
        let connect_timeout = Duration::from_secs(1);
        let a = Endpoint::builder(signer.clone(), Identities::new(a_id))
            .discovery(net.discovery.clone())
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .connect_timeout(connect_timeout)
            .bind()
            .await
            .unwrap();

        const N: u32 = 6;
        let start = tokio::time::Instant::now();
        let results = n0_future::join_all((0..N).map(|_| a.request(b_id, b"ping"))).await;
        let took = start.elapsed();
        for r in results {
            assert!(r.is_err());
        }
        tracing::info!("{N} concurrent requests to a dead peer failed after {took:?}");
        assert!(
            took < connect_timeout * 3,
            "{N} concurrent requests to a dead peer took {took:?}"
        );

        // A later request tries again (and fails again) instead of
        // returning the old failure.
        let start = tokio::time::Instant::now();
        assert!(a.request(b_id, b"ping").await.is_err());
        assert!(start.elapsed() >= connect_timeout / 2);

        a.close().await;
    })
    .await;
}

/// The request timeout also bounds waiting for a stream: a peer that keeps
/// all the streams it allows busy makes `request` time out, not hang, and
/// the connection stays usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_timeout_covers_waiting_for_a_stream() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let timeout = Duration::from_secs(1);
        let a = Endpoint::builder(signer.clone(), Identities::new(a_id))
            .discovery(net.discovery.clone())
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .request_timeout(timeout)
            .bind()
            .await
            .unwrap();
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        // B takes the first 255 streams and holds them without answering.
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let (b, held) = (b.clone(), held.clone());
            tokio::spawn(async move {
                for _ in 0..255 {
                    let Some(inc) = b.accept().await else { break };
                    held.lock().unwrap().push(inc);
                }
            });
        }

        // The dialer's control stream plus 255 application streams use up
        // the 256 concurrent streams B allows on the connection.
        let mut mine = Vec::new();
        for _ in 0..255 {
            mine.push(a.open_bi(b_id).await.unwrap());
        }
        wait_for(10, "B to hold 255 streams", || {
            held.lock().unwrap().len() == 255
        })
        .await;

        let start = tokio::time::Instant::now();
        let r = tokio::time::timeout(timeout * 5, a.request(b_id, b"x"))
            .await
            .expect("request() did not time out while waiting for a stream");
        assert!(matches!(r, Err(golem_p2p::Error::Timeout)), "{r:?}");
        assert!(start.elapsed() >= timeout);
        assert_eq!(a.connections().len(), 1, "connection was closed");

        // Once B lets the streams go, the same connection serves requests.
        held.lock().unwrap().clear();
        drop(mine);
        spawn_echo(&b);
        let e = echo_eventually(&a, b_id, "after", 10).await;
        assert_eq!(e.data, "after");

        a.close().await;
        b.close().await;
    })
    .await;
}

/// A request whose handler is too slow times out on its own: its stream is
/// reset (the handler learns that the requester gave up), requests running
/// at the same time on the same connection succeed, and the connection stays
/// open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_timeout_fails_only_that_request() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let timeout = Duration::from_secs(1);
        let a = Endpoint::builder(signer.clone(), Identities::new(a_id))
            .discovery(net.discovery.clone())
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .request_timeout(timeout)
            .bind()
            .await
            .unwrap();
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        // B echoes every request at once, except `slow`: that one it never
        // answers, it only waits until the requester stops reading.
        let (gave_up_tx, gave_up_rx) = tokio::sync::oneshot::channel();
        let gave_up_tx = Arc::new(std::sync::Mutex::new(Some(gave_up_tx)));
        {
            let b = b.clone();
            tokio::spawn(async move {
                while let Some(mut inc) = b.accept().await {
                    let gave_up_tx = gave_up_tx.clone();
                    tokio::spawn(async move {
                        let req = inc.read_request(1024).await.unwrap();
                        if req == b"slow" {
                            let (send, _) = inc.streams();
                            let stopped = send.stopped().await;
                            if let Some(tx) = gave_up_tx.lock().unwrap().take() {
                                let _ = tx.send(stopped.ok().flatten());
                            }
                        } else {
                            let _ = inc.respond(&req).await;
                        }
                    });
                }
            });
        }
        let conn = a.connect(b_id).await.unwrap();

        // Fast requests before, during and after the slow one's timeout.
        let fast = async {
            for i in 0..10 {
                let msg = format!("fast-{i}");
                let reply = a.request(b_id, msg.as_bytes()).await;
                assert_eq!(reply.unwrap(), msg.as_bytes(), "request {i}");
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        };
        let start = tokio::time::Instant::now();
        let (slow, ()) = tokio::join!(a.request(b_id, b"slow"), fast);
        assert!(matches!(slow, Err(golem_p2p::Error::Timeout)), "{slow:?}");
        assert!(start.elapsed() >= timeout);

        // B learned that A gave up on the slow request.
        let code = tokio::time::timeout(Duration::from_secs(5), gave_up_rx)
            .await
            .expect("the slow handler was not told that the requester gave up")
            .unwrap();
        assert_eq!(code, Some(golem_p2p::wire::codes::TIMED_OUT));

        // Same connection, still usable.
        assert!(!conn.is_closed(), "the timeout closed the connection");
        assert_eq!(a.request(b_id, b"after").await.unwrap(), b"after");
        assert_eq!(a.connect(b_id).await.unwrap().stable_id(), conn.stable_id());

        a.close().await;
        b.close().await;
    })
    .await;
}

/// What the thread running a hung test node is told to do.
enum HangCmd {
    /// Block the node's only runtime thread until the sender is dropped.
    Hang(std::sync::mpsc::Receiver<()>),
    Close,
}

/// A peer that hangs (it neither answers nor acknowledges anything) is
/// noticed within one request timeout: the request fails, the connection is
/// closed, and once the peer recovers the next request dials it afresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hung_peer_is_noticed_within_the_request_timeout() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let timeout = Duration::from_secs(1);
        let connect_timeout = Duration::from_secs(2);
        let a = Endpoint::builder(signer.clone(), Identities::new(a_id))
            .discovery(net.discovery.clone())
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .request_timeout(timeout)
            .connect_timeout(connect_timeout)
            .bind()
            .await
            .unwrap();

        // B runs on its own single-threaded runtime, so that blocking that
        // thread hangs B completely, like a stopped process.
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let discovery = net.discovery.clone();
        let b_signer = signer.clone();
        let b_thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let b = Endpoint::builder(b_signer, Identities::new(b_id))
                    .discovery(discovery)
                    .transport_mode(TransportMode::DirectOnly)
                    .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                    .bind()
                    .await
                    .unwrap();
                spawn_echo(&b);
                ready_tx.send(()).unwrap();
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        // Deliberately blocking: the whole node stops.
                        HangCmd::Hang(release) => {
                            let _ = release.recv();
                        }
                        HangCmd::Close => break,
                    }
                }
                b.close().await;
            });
        });
        ready_rx.await.unwrap();

        assert_eq!(echo(&a, b_id, "before").await.unwrap().data, "before");
        let conn = a.connect(b_id).await.unwrap();

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        cmd_tx.send(HangCmd::Hang(release_rx)).unwrap();
        // Let whatever B sent before it hung arrive.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let start = tokio::time::Instant::now();
        let r = a.request(b_id, b"while hung").await;
        let took = start.elapsed();
        assert!(matches!(r, Err(golem_p2p::Error::Timeout)), "{r:?}");
        assert!(took < timeout * 2, "took {took:?}");
        assert!(
            conn.is_closed(),
            "nothing arrived from the hung peer, but the connection stays open"
        );

        // While B still hangs, the next request dials and fails within the
        // connect timeout.
        let start = tokio::time::Instant::now();
        assert!(a.request(b_id, b"still hung").await.is_err());
        assert!(start.elapsed() < connect_timeout + timeout * 2);

        // B recovers: a new connection serves requests again.
        drop(release_tx);
        let e = echo_eventually(&a, b_id, "after", 20).await;
        assert_eq!(e.data, "after");
        assert_ne!(a.connect(b_id).await.unwrap().stable_id(), conn.stable_id());

        cmd_tx.send(HangCmd::Close).unwrap();
        a.close().await;
        tokio::task::spawn_blocking(move || b_thread.join().unwrap())
            .await
            .unwrap();
    })
    .await;
}

/// Discovery whose first publish takes a while.
#[derive(Debug, Default)]
struct SlowFirstPublish {
    inner: MemoryDiscovery,
    published: AtomicUsize,
}

impl Discovery for SlowFirstPublish {
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
        Box::pin(async move {
            if self.published.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            self.inner.publish(record).await
        })
    }

    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
        self.inner.lookup(id)
    }
}

/// An address change while `bind` is still publishing the first record
/// (here: the home relay connects) is not lost: the record is re-signed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn address_change_during_bind_is_published() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let a_id = signer.generate();
        let a = Endpoint::builder(signer.clone(), Identities::new(a_id))
            .discovery(SlowFirstPublish::default())
            .relay_config(net.relays[0].config.clone())
            .relay_tls(net.relay_tls())
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            // Sign the first record before the relay connects.
            .online_timeout(Duration::ZERO)
            .bind()
            .await
            .unwrap();
        let first = a.local_record();
        assert!(
            first.record().relay_urls.is_empty(),
            "the relay connected before the first record was signed"
        );
        wait_for(10, "the relay URL in the record", || {
            !a.local_record().record().relay_urls.is_empty()
        })
        .await;
        assert!(a.local_record().seq() > first.seq());
        a.close().await;
    })
    .await;
}

/// `max_response_size` bounds what `request` accepts; a larger response fails
/// with `TooLarge` and leaves the connection usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_response_size_is_configurable() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let a = Endpoint::builder(signer.clone(), Identities::new(a_id))
            .discovery(net.discovery.clone())
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .max_response_size(1000)
            .bind()
            .await
            .unwrap();
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        spawn_echo(&b);

        // The echo adds about 150 bytes to the request.
        assert!(echo(&a, b_id, &"x".repeat(500)).await.is_ok());
        assert!(matches!(
            echo(&a, b_id, &"x".repeat(2000)).await,
            Err(golem_p2p::Error::TooLarge)
        ));
        assert_eq!(a.connections().len(), 1);
        assert!(echo(&a, b_id, "small").await.is_ok());

        a.close().await;
        b.close().await;
    })
    .await;
}

/// A handler that drops the stream without answering (it failed, or gave
/// up) makes the request fail with `NOT_ANSWERED` instead of returning an
/// empty response. A handler that answers through the raw streams and then
/// drops them is not affected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_request_is_an_error_not_an_empty_reply() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let a = net
            .node(&signer, Identities::new(a_id), NodeOpts::default())
            .await;
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        {
            let b = b.clone();
            tokio::spawn(async move {
                while let Some(mut inc) = b.accept().await {
                    let req = inc.read_request(1024).await.unwrap();
                    if req == b"raw" {
                        let (send, _) = inc.streams();
                        send.write_all(b"raw answer").await.unwrap();
                        send.finish().unwrap();
                    }
                    // Dropped without `respond`.
                }
            });
        }
        match a.request(b_id, b"fail").await {
            Err(golem_p2p::Error::StreamRejected(code)) => {
                assert_eq!(code, golem_p2p::wire::codes::NOT_ANSWERED.into_inner());
            }
            other => panic!("expected NOT_ANSWERED, got {other:?}"),
        }
        assert_eq!(a.request(b_id, b"raw").await.unwrap(), b"raw answer");
        assert_eq!(a.connections().len(), 1);

        a.close().await;
        b.close().await;
    })
    .await;
}
