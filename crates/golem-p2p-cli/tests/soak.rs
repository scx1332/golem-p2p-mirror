//! `soak` and the `stats` events of `listen` and `directory`
//! (docs/cli-events.md), as the soak harness (harness/soak/) consumes them.

mod common;

use common::*;
use serde_json::Value;

const RUN: &str = "soak-1";

fn strs<const N: usize>(a: [&str; N]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn check_proc(p: &Value) {
    for k in ["rss_kb", "hwm_kb", "anon_kb", "threads", "fds", "sockets"] {
        assert!(p[k].as_u64().is_some_and(|v| v > 0), "{k} in {p}");
    }
    assert!(p["cpu_s"].is_number(), "{p}");
    if cfg!(all(target_os = "linux", target_env = "gnu")) {
        assert!(p["heap_used_kb"].as_u64().is_some_and(|v| v > 0), "{p}");
        assert!(p["heap_free_kb"].is_u64(), "{p}");
    }
}

/// A soak client against a listener: `soak_stats` per interval and a final
/// one with consistent totals, histogram and resources; the listener's
/// `stats` count the connection and requests, and it saw no failed request.
#[tokio::test(flavor = "multi_thread")]
async fn soak_client_and_listener_stats() {
    let infra = Infra::start("soak").await;
    let (_, lkey) = infra.key("listener");
    let mut args = strs([
        "--json",
        "--run-id",
        RUN,
        "--node-name",
        "listener",
        "listen",
    ]);
    args.extend(infra.node_args(&lkey));
    args.extend(strs(["--stats-interval", "500ms"]));
    let mut listener = Cli::spawn(&args);
    let ready = listener.wait_event("ready", 30).await;
    let to = ready["node_id"].as_str().unwrap().to_string();

    let (_, ckey) = infra.key("client");
    let mut args = strs(["--json", "--run-id", RUN, "--node-name", "client", "soak"]);
    args.extend(infra.node_args(&ckey));
    args.extend(strs([
        "--to",
        &to,
        "--duration",
        "4s",
        "--interval",
        "50ms",
        "--stats-interval",
        "1s",
        "--bulk-every",
        "1s",
        "--bulk-size",
        "200k",
    ]));
    let (status, evs) = run(&args, 60).await;
    assert_eq!(status.code(), Some(0), "{evs:#?}");
    for v in &evs {
        check_common(v, Some(RUN), Some("client"));
    }
    assert_eq!(evs.first().unwrap()["event"], "ready");
    let stats = events(&evs, "soak_stats");
    assert!(stats.len() >= 4, "{stats:#?}");
    let last = evs.last().unwrap();
    assert_eq!(last["event"], "soak_stats", "{evs:#?}");
    assert_eq!(last["final"], true);
    assert_eq!(last["interrupted"], false);
    assert!(events(&evs, "error").is_empty(), "{evs:#?}");

    let (mut sent, mut ok, mut new_conns, mut bulk_ok) = (0, 0, 0, 0);
    for s in &stats {
        assert_eq!(s["to"], to.as_str());
        sent += s["sent"].as_u64().unwrap();
        ok += s["ok"].as_u64().unwrap();
        new_conns += s["new_conns"].as_u64().unwrap();
        bulk_ok += s["bulk"]["ok"].as_u64().unwrap();
        assert_eq!(s["failed"], 0, "{s}");
        // The histogram holds the round-trip times of the window's
        // requests on an existing connection.
        let hist: u64 = s["rtt_hist"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_u64().unwrap())
            .sum();
        let window_new = s["new_conns"].as_u64().unwrap();
        assert_eq!(hist, s["ok"].as_u64().unwrap() - window_new, "{s}");
        if hist > 0 {
            let r = &s["rtt_ms"];
            assert!(
                num(r, "p50") <= num(r, "p99") && num(r, "p99") <= num(r, "max"),
                "{s}"
            );
        }
        check_proc(&s["proc"]);
        assert!(s["tokio"]["alive_tasks"].as_u64().unwrap() > 0, "{s}");
        assert!(s["session"]["bind_ms"].as_f64().unwrap() > 0.0, "{s}");
    }
    let t = &last["totals"];
    assert_eq!(t["sent"].as_u64(), Some(sent));
    assert_eq!(t["ok"].as_u64(), Some(ok));
    assert_eq!(t["failed"], 0);
    assert_eq!(new_conns, 1, "one connection for the whole run");
    // About 80 requests in 4 s at 50 ms; a slow machine sends fewer.
    assert!(sent >= 20, "{sent}");
    assert!(bulk_ok >= 2, "{bulk_ok}");
    assert_eq!(t["bulk_ok"].as_u64(), Some(bulk_ok));
    assert_eq!(t["bulk_bytes"].as_u64(), Some(bulk_ok * 200_000));
    assert_eq!(last["endpoint"]["connections_outgoing"], 1, "{last}");
    assert!(last["session"]["first_ok_ms"].as_f64().is_some(), "{last}");

    // The listener counted the connection and every request, and no request
    // failed (the client finishes its last request before it exits).
    let stats = listener
        .wait_for(10, "stats after the client", |v| {
            v["event"] == "stats" && v["endpoint"]["connections_closed"] == 1
        })
        .await;
    listener.terminate();
    let (_, levs) = listener.finish(30).await;
    let ep = &stats["endpoint"];
    assert_eq!(ep["connections_incoming"], 1, "{stats}");
    assert_eq!(ep["connections"], 0, "{stats}");
    assert_eq!(ep["handshakes_failed"], 0, "{stats}");
    assert_eq!(ep["streams_refused"], 0, "{stats}");
    assert_eq!(
        ep["streams_accepted"].as_u64(),
        Some(sent + bulk_ok),
        "{stats}"
    );
    check_proc(&stats["proc"]);
    assert!(events(&levs, "error").is_empty(), "{levs:#?}");
}

/// SIGTERM ends `soak` with a final `soak_stats` (interrupted) and exit 0;
/// `directory --stats-interval` reports its store.
#[tokio::test(flavor = "multi_thread")]
async fn soak_interrupted_and_directory_stats() {
    let infra = Infra::start("soak-term").await;
    let mut dir = Cli::spawn(&strs([
        "--json",
        "directory",
        "--listen",
        "127.0.0.1:0",
        "--stats-interval",
        "300ms",
    ]));
    let url = dir.wait_event("directory", 10).await["url"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, lkey) = infra.key("listener");
    let mut args = strs(["--json", "listen"]);
    args.extend(infra.node_args(&lkey));
    // Use the CLI's directory instead of the test's.
    let pos = args.iter().position(|a| a == "--directory").unwrap();
    args[pos + 1] = url.clone();
    let mut listener = Cli::spawn(&args);
    let to = listener.wait_event("ready", 30).await["node_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, ckey) = infra.key("client");
    let mut args = strs(["--json", "soak"]);
    args.extend(infra.node_args(&ckey));
    let pos = args.iter().position(|a| a == "--directory").unwrap();
    args[pos + 1] = url;
    args.extend(strs(["--to", &to, "--interval", "100ms"]));
    let mut client = Cli::spawn(&args);
    client
        .wait_for(30, "a successful request", |v| {
            v["event"] == "soak_stats" || v["event"] == "conn"
        })
        .await;
    client.terminate();
    let (status, evs) = client.finish(30).await;
    assert_eq!(status.code(), Some(0), "{evs:#?}");
    let last = evs.last().unwrap();
    assert_eq!(last["event"], "soak_stats", "{evs:#?}");
    assert_eq!(last["final"], true);
    assert_eq!(last["interrupted"], true);
    assert!(last["totals"]["ok"].as_u64().unwrap() >= 1, "{last}");
    assert_eq!(last["totals"]["failed"], 0, "{last}");

    // Both nodes published their records to the directory.
    let stats = dir
        .wait_for(10, "both records stored", |v| {
            v["event"] == "stats" && v["store"]["identities"] == 2
        })
        .await;
    assert_eq!(stats["store"]["endpoints"], 2, "{stats}");
    check_proc(&stats["proc"]);
    assert!(stats["tokio"]["workers"].as_u64().unwrap() >= 1, "{stats}");
    listener.terminate();
    dir.terminate();
    listener.finish(30).await;
    dir.finish(30).await;
}
