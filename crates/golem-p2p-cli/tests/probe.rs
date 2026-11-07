//! `probe`, `send` and `listen` with `--json`: the event stream a test
//! harness controller consumes (docs/cli-events.md).

mod common;

use common::*;
use serde_json::Value;

const RUN: &str = "run-1";

fn strs<const N: usize>(a: [&str; N]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Starts `listen --json` and returns it after its `ready` event.
async fn listener(infra: &Infra, name: &str, extra: &[String]) -> (Cli, Value) {
    let (_, key) = infra.key(name);
    let mut args = strs(["--json", "--run-id", RUN, "listen", "--node-name", name]);
    args.extend(infra.node_args(&key));
    args.extend(extra.iter().cloned());
    let mut cli = Cli::spawn(&args);
    let ready = cli.wait_event("ready", 30).await;
    check_common(&ready, Some(RUN), Some(name));
    // `ready` is the first line.
    assert_eq!(cli.events.len(), 1, "{:#?}", cli.events);
    (cli, ready)
}

fn probe_args(infra: &Infra, name: &str, to: &str, extra: &[String]) -> Vec<String> {
    let (_, key) = infra.key(name);
    let mut args = strs(["--json", "--run-id", RUN, "--node-name", name, "probe"]);
    args.extend(infra.node_args(&key));
    args.extend(strs(["--to", to]));
    args.extend(extra.iter().cloned());
    args
}

/// Checks the summary against the reply events and returns it.
fn check_probe_output(evs: &[Value], name: &str) -> Value {
    for v in evs {
        check_common(v, Some(RUN), Some(name));
    }
    assert_eq!(evs.first().unwrap()["event"], "ready", "{evs:#?}");
    let summary = evs.last().unwrap();
    assert_eq!(summary["event"], "summary", "{evs:#?}");
    assert_eq!(events(evs, "summary").len(), 1);

    let replies = events(evs, "reply");
    let sent = summary["sent"].as_u64().unwrap();
    let ok = summary["ok"].as_u64().unwrap();
    assert_eq!(replies.len() as u64, sent);
    assert_eq!(summary["failed"].as_u64().unwrap(), sent - ok);
    for (i, r) in replies.iter().enumerate() {
        assert_eq!(r["seq"].as_u64(), Some(i as u64 + 1), "{r}");
        assert_eq!(r["to"], summary["to"]);
        assert!(r["ok"].is_boolean() && r["new_conn"].is_boolean(), "{r}");
        if r["ok"] == true {
            assert!(num(r, "rtt_ms") > 0.0, "{r}");
        } else {
            assert!(r["rtt_ms"].is_null() && r["error_kind"].is_string(), "{r}");
        }
    }
    assert_eq!(
        replies.iter().filter(|r| r["ok"] == true).count() as u64,
        ok
    );
    // Round-trip statistics cover successful replies on an existing
    // connection.
    let samples = replies
        .iter()
        .filter(|r| r["ok"] == true && r["new_conn"] == false)
        .count();
    assert_eq!(summary["rtt_samples"].as_u64(), Some(samples as u64));
    let rtt = &summary["rtt_ms"];
    if samples > 0 {
        let (min, p50, p95, max) = (
            num(rtt, "min"),
            num(rtt, "p50"),
            num(rtt, "p95"),
            num(rtt, "max"),
        );
        assert!(0.0 < min && min <= p50 && p50 <= p95 && p95 <= max, "{rtt}");
        assert!(min <= num(rtt, "mean") && num(rtt, "mean") <= max, "{rtt}");
    } else {
        assert!(rtt["p50"].is_null(), "{rtt}");
    }
    let path = summary["final_path"].as_str().unwrap();
    assert!(["direct", "relay", "unknown"].contains(&path), "{path}");
    summary.clone()
}

/// Relay-only probe against a relay-only listener, with a bulk transfer: valid
/// JSON lines on both sides, every request relayed, a sane summary. Then
/// `send --json` against the same listener.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_relay_only_with_bulk() {
    let infra = Infra::start("probe-relay").await;
    let relay = strs(["--relay", &infra.relay_url, "--relay-only"]);
    let (mut lis, ready) = listener(&infra, "lis", &relay).await;
    let lis_id = ready["node_id"].as_str().unwrap().to_string();
    assert_eq!(
        ready["relays"],
        serde_json::json!([format!("{}/", infra.relay_url)])
    );
    assert_eq!(ready["direct_addrs"], serde_json::json!([]));
    assert_eq!(ready["transport"], "relay_only");
    assert_eq!(ready["identities"], serde_json::json!([lis_id]));
    assert!(ready["endpoint_id"].as_str().unwrap().len() == 64);

    let mut extra = relay.clone();
    extra.extend(strs([
        "--duration",
        "3s",
        "--interval",
        "250ms",
        "--payload-size",
        "100",
        "--bulk",
        "2M",
    ]));
    let (status, evs) = run(&probe_args(&infra, "prb", &lis_id, &extra), 60).await;
    assert_eq!(status.code(), Some(0), "{evs:#?}");
    let s = check_probe_output(&evs, "prb");
    assert_eq!(s["to"], lis_id.as_str());
    assert!(s["sent"].as_u64().unwrap() >= 8, "{s}");
    assert_eq!(s["ok"], s["sent"], "{s}");
    assert!(num(&s, "connect_ms") > 0.0 && num(&s, "connect_ms") < 30_000.0);
    assert!(s["time_to_direct_ms"].is_null(), "{s}");
    assert_eq!(s["direct_fraction"], 0.0);
    assert_eq!(s["final_path"], "relay");
    assert_eq!(s["payload_size"], 100);
    assert_eq!(s["interrupted"], false);
    assert_eq!(s["bulk_bytes"], 2_000_000);
    assert_eq!(s["bulk_ok"], true);
    assert_eq!(s["bulk_path"], "relay");
    assert!(num(&s, "throughput_mbps") > 0.0 && num(&s, "bulk_ms") > 0.0);
    for r in events(&evs, "reply") {
        assert_eq!((&r["path"], &r["bytes"]), (&"relay".into(), &100.into()));
    }
    // Only the first request had to connect.
    let new_conns: Vec<_> = events(&evs, "reply")
        .iter()
        .map(|r| r["new_conn"] == true)
        .collect();
    assert!(
        new_conns[0] && !new_conns[1..].contains(&true),
        "{new_conns:?}"
    );
    let conn = events(&evs, "conn");
    assert_eq!(conn.len(), 1, "{evs:#?}");
    assert_eq!(conn[0]["peer"], lis_id.as_str());
    assert_eq!(conn[0]["direction"], "out");
    assert_eq!(conn[0]["path"], "relay");
    assert_eq!(
        conn[0]["remote_addr"],
        format!("{}/", infra.relay_url).as_str()
    );
    assert!(events(&evs, "path_change").is_empty(), "{evs:#?}");
    let home = events(&evs, "home_relay");
    assert!(
        home.iter()
            .any(|h| h["connected"] == true && h["url"] == format!("{}/", infra.relay_url)),
        "{evs:#?}"
    );

    // The listener saw every request over the relay, and the bulk transfer.
    let sink = lis
        .wait_for(30, "bulk request", |v| {
            v["event"] == "request" && v["kind"] == "sink"
        })
        .await;
    assert_eq!(sink["bytes"], 2_000_000);
    assert_eq!(sink["path"], "relay");
    let echoes = events(&lis.events, "request")
        .into_iter()
        .filter(|r| r["kind"] == "echo")
        .count();
    assert_eq!(echoes as u64, s["sent"].as_u64().unwrap());
    let lis_conn = events(&lis.events, "conn");
    assert_eq!(lis_conn[0]["direction"], "in");
    assert_eq!(lis_conn[0]["path"], "relay");
    for v in &lis.events {
        check_common(v, Some(RUN), Some("lis"));
    }

    // `send --json`: reply events carry the reply text; summary last.
    let (_, key) = infra.key("snd");
    let mut args = strs(["--json", "send"]);
    args.extend(infra.node_args(&key));
    args.extend(relay.clone());
    args.extend(strs(["--to", &lis_id, "--count", "2", "hi"]));
    let (status, evs) = run(&args, 60).await;
    assert_eq!(status.code(), Some(0), "{evs:#?}");
    for v in &evs {
        check_common(v, None, None);
    }
    let replies = events(&evs, "reply");
    assert_eq!(replies.len(), 2);
    for r in &replies {
        assert_eq!(r["ok"], true);
        assert!(r["reply"].as_str().unwrap().ends_with(" echo=hi"), "{r}");
        assert_eq!(r["path"], "relay");
    }
    let s = evs.last().unwrap();
    assert_eq!(
        (&s["event"], &s["sent"], &s["ok"]),
        (&"summary".into(), &2.into(), &2.into())
    );
}

/// Both nodes have UDP and the relay: the probe reports when the path became
/// direct, and ends on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_reports_direct_path() {
    let infra = Infra::start("probe-direct").await;
    let relay = strs(["--relay", &infra.relay_url]);
    let (_lis, ready) = listener(&infra, "lis", &relay).await;
    assert_eq!(ready["transport"], "direct_and_relay");
    assert!(!ready["direct_addrs"].as_array().unwrap().is_empty());
    let lis_id = ready["node_id"].as_str().unwrap();

    let mut extra = relay.clone();
    extra.extend(strs(["--duration", "4s", "--interval", "200ms"]));
    let (status, evs) = run(&probe_args(&infra, "prb", lis_id, &extra), 60).await;
    assert_eq!(status.code(), Some(0), "{evs:#?}");
    let s = check_probe_output(&evs, "prb");
    assert_eq!(s["failed"], 0);
    assert_eq!(s["final_path"], "direct", "{evs:#?}");
    let ttd = num(&s, "time_to_direct_ms");
    assert!((0.0..4_000.0).contains(&ttd), "{s}");
    assert!(num(&s, "direct_fraction") > 0.0, "{s}");
    assert!(s["bulk_bytes"].is_null() && s["throughput_mbps"].is_null());
    // The connection either started direct or switched to it.
    let conn = events(&evs, "conn");
    let switched = events(&evs, "path_change").iter().any(|p| {
        p["to"] == "direct" && p["remote_addr"].as_str().unwrap().starts_with("127.0.0.1:")
    });
    assert!(conn[0]["path"] == "direct" || switched, "{evs:#?}");
}

/// A node that is not in the directory: every request fails, exit code 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_unknown_node_fails() {
    let infra = Infra::start("probe-unknown").await;
    let (nobody, _) = infra.key("nobody");
    let extra = strs(["--duration", "1s", "--interval", "250ms", "--bulk", "1k"]);
    let (status, evs) = run(&probe_args(&infra, "prb", &nobody.to_string(), &extra), 60).await;
    assert_eq!(status.code(), Some(1), "{evs:#?}");
    let s = check_probe_output(&evs, "prb");
    assert!(s["sent"].as_u64().unwrap() >= 2, "{s}");
    assert_eq!(s["ok"], 0);
    assert!(s["connect_ms"].is_null() && s["direct_fraction"].is_null());
    assert_eq!(s["final_path"], "unknown");
    assert_eq!(s["bulk_ok"], false);
    for r in events(&evs, "reply") {
        assert_eq!(r["error_kind"], "not_found", "{r}");
        assert!(r["elapsed_ms"].is_number(), "{r}");
    }
    // The skipped bulk transfer is reported as an error event.
    let errors = events(&evs, "error");
    assert_eq!(errors.len(), 1, "{evs:#?}");
    assert_eq!(errors[0]["kind"], "bulk");
}

/// SIGTERM ends a probe early with a summary; SIGTERM stops a listener.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_ends_probe_with_summary() {
    let infra = Infra::start("probe-sigterm").await;
    let (lis, ready) = listener(&infra, "lis", &[]).await;
    assert_eq!(ready["transport"], "direct_only");
    let lis_id = ready["node_id"].as_str().unwrap();

    let extra = strs(["--duration", "60s", "--interval", "100ms"]);
    let mut probe = Cli::spawn(&probe_args(&infra, "prb", lis_id, &extra));
    probe
        .wait_for(30, "3 replies", |v| v["event"] == "reply" && v["seq"] == 3)
        .await;
    probe.terminate();
    let (status, evs) = probe.finish(30).await;
    assert_eq!(status.code(), Some(0), "{evs:#?}");
    let s = check_probe_output(&evs, "prb");
    assert_eq!(s["interrupted"], true);
    assert_eq!(s["final_path"], "direct");
    assert!(num(&s, "duration_ms") < 30_000.0, "{s}");

    lis.terminate();
    let (status, _) = lis.finish(30).await;
    assert_eq!(status.code(), Some(0));
}

/// `--json` works for every command, and `run`/`node` are null when unset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_keygen_and_directory() {
    let dir = std::env::temp_dir().join(format!("golem-p2p-cli-keygen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("k.key");
    let (status, evs) = run(&["keygen", "--out", key.to_str().unwrap(), "--json"], 30).await;
    assert!(status.success());
    assert_eq!(evs.len(), 1);
    check_common(&evs[0], None, None);
    assert_eq!(evs[0]["event"], "keygen");
    let id = evs[0]["node_id"].as_str().unwrap();
    assert!(id.starts_with("0x") && id.len() == 42, "{id}");
    // A failure is an error event plus a nonzero exit code.
    let (status, evs) = run(
        &[
            "--json",
            "--node-name",
            "n1",
            "keygen",
            "--out",
            key.to_str().unwrap(),
        ],
        30,
    )
    .await;
    assert_eq!(status.code(), Some(1));
    assert_eq!(evs.len(), 1);
    check_common(&evs[0], None, Some("n1"));
    assert_eq!(
        (&evs[0]["event"], &evs[0]["kind"]),
        (&"error".into(), &"fatal".into())
    );
    assert!(
        evs[0]["message"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );
    let _ = std::fs::remove_dir_all(&dir);

    let mut d = Cli::spawn(&[
        "--json",
        "--run-id",
        "r",
        "directory",
        "--listen",
        "127.0.0.1:0",
    ]);
    let v = d.wait_event("directory", 30).await;
    check_common(&v, Some("r"), None);
    assert!(
        v["url"].as_str().unwrap().starts_with("http://127.0.0.1:"),
        "{v}"
    );
    d.terminate();
    let (status, _) = d.finish(30).await;
    assert_eq!(status.code(), Some(0));
}
