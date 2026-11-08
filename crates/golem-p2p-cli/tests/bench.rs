//! `bench` against a local listener, as child processes with `--json`: the
//! `bench_report` event, exit codes, the listener's limits.

mod common;

use common::*;
use serde_json::Value;

const RUN: &str = "bench-run";

fn strs<const N: usize>(a: [&str; N]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// `--relay URL,quic=PORT` for the local relay.
fn relay_args(infra: &Infra) -> Vec<String> {
    vec![
        "--relay".into(),
        format!("{},quic={}", infra.relay_url, infra.quic_port),
    ]
}

/// Starts `listen --json` and returns it and its node ID.
async fn listener(infra: &Infra, name: &str, extra: &[String]) -> (Cli, String) {
    let (_, key) = infra.key(name);
    let mut args = strs(["--json", "--run-id", RUN, "--node-name", name, "listen"]);
    args.extend(infra.node_args(&key));
    args.extend(extra.iter().cloned());
    let mut cli = Cli::spawn(&args);
    let ready = cli.wait_event("ready", 30).await;
    let id = ready["node_id"].as_str().unwrap().to_string();
    (cli, id)
}

/// `bench --json` arguments with a temporary identity (no `--key`).
fn bench_args(infra: &Infra, extra: &[String]) -> Vec<String> {
    let mut args = strs(["--json", "--run-id", RUN, "--node-name", "bench", "bench"]);
    args.extend(
        infra
            .node_args(std::path::Path::new("unused"))
            .into_iter()
            .skip(2),
    );
    args.extend(extra.iter().cloned());
    args
}

/// Runs a bench; checks the event stream and returns the exit code and the
/// `bench_report`.
async fn bench(args: &[String]) -> (Option<i32>, Value, Vec<Value>) {
    let (status, evs) = run(args, 120).await;
    for v in &evs {
        check_common(v, Some(RUN), Some("bench"));
    }
    assert_eq!(evs.first().unwrap()["event"], "ready", "{evs:#?}");
    let report = evs.last().unwrap().clone();
    assert_eq!(report["event"], "bench_report", "{evs:#?}");
    assert_eq!(events(&evs, "bench_report").len(), 1);
    assert_eq!(
        report["exit_code"].as_i64().map(|c| c as i32),
        status.code()
    );
    for key in ["status", "verdict"] {
        assert!(report[key].is_string(), "{key}: {report:#}");
    }
    for key in ["issues", "hints", "unknowns"] {
        assert!(report[key].is_array(), "{key}: {report:#}");
    }
    assert!(!report["verdict"].as_str().unwrap().is_empty());
    (status.code(), report, evs)
}

/// Checks a successful transfer report.
fn check_transfer(t: &Value, bytes: u64, path: &str) {
    assert_eq!(t["ok"], true, "{t:#}");
    assert_eq!(t["bytes"].as_u64(), Some(bytes), "{t:#}");
    assert_eq!(t["requested_bytes"].as_u64(), Some(bytes), "{t:#}");
    assert_eq!(t["path"], path, "{t:#}");
    assert_eq!(t["limited_by"], "size", "{t:#}");
    assert!(num(t, "throughput_mbps") > 0.0 && num(t, "duration_ms") > 0.0);
    // Null for transfers under a second.
    let half = &t["second_half_mbps"];
    assert!(
        half.is_null() || (num(t, "duration_ms") >= 1000.0 && half.as_f64().unwrap() >= 0.0),
        "{t:#}"
    );
    assert!(t["error"].is_null());
    check_quic(&t["quic"]);
}

/// What the concurrency phase sizes its requests for: the slower transfer,
/// each by the lower of its throughput and that of its second half.
fn sustained(p: &Value) -> f64 {
    ["upload", "download"]
        .iter()
        .map(|dir| {
            let t = &p[dir];
            let m = num(t, "throughput_mbps");
            t["second_half_mbps"].as_f64().map_or(m, |h| m.min(h))
        })
        .fold(f64::INFINITY, f64::min)
}

/// Checks the QUIC counters of a phase: present, and loss consistent.
fn check_quic(q: &Value) {
    let sent = q["sent_packets"].as_u64().unwrap();
    let lost = q["lost_packets"].as_u64().unwrap();
    assert!(sent > 0 && lost <= sent, "{q:#}");
    assert!(num(q, "lost_pct") >= 0.0, "{q:#}");
}

/// Checks the latency section: every request answered, sane statistics.
fn check_latency(l: &Value, path: &str) {
    assert!(l["sent"].as_u64().unwrap() >= 3, "{l:#}");
    assert_eq!(l["ok"], l["sent"], "{l:#}");
    assert_eq!(l["failed"], 0);
    let r = &l["rtt_ms"];
    let (min, p50, p95, max) = (num(r, "min"), num(r, "p50"), num(r, "p95"), num(r, "max"));
    assert!(0.0 < min && min <= p50 && p50 <= p95 && p95 <= max, "{r}");
    assert!(l["jitter_ms"].as_f64().unwrap() >= 0.0);
    assert_eq!(l["final_path"], path);
    check_quic(&l["quic"]);
    // The loss count: from the start (the path did not change on loopback),
    // within the phase's counters.
    let loss = &l["loss"];
    assert_eq!(loss["from_ms"], 0.0, "{l:#}");
    let sent = loss["sent_packets"].as_u64().unwrap();
    assert!(
        sent > 0 && sent <= l["quic"]["sent_packets"].as_u64().unwrap(),
        "{l:#}"
    );
    assert!(loss["lost_packets"].as_u64().unwrap() <= sent, "{l:#}");
}

/// The `item`s of the report's unknowns.
fn unknown_items(r: &Value) -> Vec<&str> {
    r["unknowns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["item"].as_str().unwrap())
        .collect()
}

/// Both sides relay-only: the report says "relay only" (exit code 4), all
/// phases ran over the relay, transfers were capped for the relay, and the
/// listener saw the upload and the download.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_relay_only() {
    let infra = Infra::start("bench-relay").await;
    let mut relay = relay_args(&infra);
    relay.push("--relay-only".into());
    let (mut lis, lis_id) = listener(&infra, "lis", &relay).await;

    let mut extra = relay.clone();
    extra.extend(strs([
        "--to",
        &lis_id,
        "--latency-duration",
        "1500ms",
        "--interval",
        "250ms",
        "--transfer-size",
        "2M",
        "--relay-transfer-size",
        "1M",
        "--streams",
        "4",
        "--concurrency-size",
        "2M",
    ]));
    let (code, r, evs) = bench(&bench_args(&infra, &extra)).await;
    assert_eq!(code, Some(4), "{r:#}");
    assert_eq!(r["status"], "relay_only");
    assert_eq!(r["transport"], "relay_only");
    assert_eq!(r["temporary_identity"], true);
    assert_eq!(r["to"], lis_id.as_str());
    assert!(
        r["verdict"].as_str().unwrap().contains("--relay-only"),
        "{r:#}"
    );
    assert!(r["issues"].as_array().unwrap().is_empty(), "{r:#}");

    // Local check: the relay is reachable over TLS with the configured CA,
    // and our record was published.
    let l = &r["local"];
    let relay_url = format!("{}/", infra.relay_url);
    let rc = &l["relays"][0];
    assert_eq!(rc["url"], relay_url.as_str(), "{l:#}");
    assert_eq!(rc["ok"], true, "{rc:#}");
    assert_eq!(rc["http_status"], 200);
    assert_eq!(rc["quic_port"].as_u64(), Some(infra.quic_port as u64));
    assert_eq!(rc["quic_port_explicit"], true, "{rc:#}");
    assert!(num(rc, "tcp_ms") > 0.0 && num(rc, "tls_ms") > 0.0 && num(rc, "http_ms") > 0.0);
    assert!(rc["dns_ms"].is_null(), "an IP address needs no DNS: {rc:#}");
    assert_eq!(l["directories"][0]["own_record"], "published");
    assert_eq!(l["record_published"], true);
    assert_eq!(l["home_relay"]["url"], relay_url.as_str());
    assert_eq!(l["home_relay"]["connected"], true);
    // golem-p2p does not map ports: nothing to report but the unknown.
    assert!(l.get("port_mapping").is_none(), "{l:#}");
    // Without UDP, UDP and NAT are not tested; the relay's HTTPS latency is.
    assert!(l["net_report"].is_null(), "{l:#}");
    assert!(num(rc, "https_latency_ms") > 0.0, "{rc:#}");
    let unknowns = unknown_items(&r);
    assert!(unknowns.contains(&"port_mapping"), "{r:#}");
    assert!(unknowns.contains(&"udp"), "{r:#}");
    // 1.5 s of latency requests are too few packets to judge loss.
    assert!(unknowns.contains(&"packet_loss"), "{r:#}");

    let p = &r["peer"];
    let c = &p["connect"];
    assert_eq!(c["ok"], true, "{c:#}");
    assert_eq!(c["path"], "relay");
    assert_eq!(c["remote_addr"], relay_url.as_str());
    let (lookup, connect, handshake) = (
        num(c, "lookup_ms"),
        num(c, "connect_ms"),
        num(c, "handshake_ms"),
    );
    assert!(lookup > 0.0 && handshake > 0.0 && (lookup + handshake - connect).abs() < 0.01);
    check_latency(&p["latency"], "relay");
    assert_eq!(p["latency"]["direct_fraction"], 0.0);
    for dir in ["upload", "download"] {
        let t = &p[dir];
        check_transfer(t, 1_000_000, "relay");
        assert_eq!(t["relay_capped"], true, "{t:#}");
    }
    let cc = &p["concurrency"];
    assert_eq!(cc["ok"], true, "{cc:#}");
    assert_eq!(cc["streams"], 4);
    // Fast enough for full-size requests.
    assert_eq!(cc["request_bytes"], 262_144);
    assert!(cc["sized_for_mbps"].is_null(), "{cc:#}");
    assert!(cc["restart"].is_null(), "{cc:#}");
    // The relay cap (1M) limits the phase to 3 requests of 256 KiB.
    assert_eq!(cc["requests"], 3, "{cc:#}");
    assert_eq!(cc["total_bytes"], 3 * 262_144);
    assert!(num(cc, "throughput_mbps") > 0.0);
    let path = &p["path"];
    assert!(path["time_to_direct_ms"].is_null(), "{path:#}");
    assert_eq!(path["final_path"], "relay");
    assert_eq!(path["timeline"][0]["path"], "relay");
    let q = &p["quic"];
    assert!(q["sent_packets"].as_u64().unwrap() > 0, "{q:#}");
    assert!(q["sent_bytes"].as_u64().unwrap() > 1_000_000, "{q:#}");
    assert!(q["received_bytes"].as_u64().unwrap() > 1_000_000, "{q:#}");
    assert!(r["relay_compare"].is_null());

    // One `bench_phase` per phase, in order; `reply` events of the latency
    // phase carry the phase.
    let phases: Vec<_> = events(&evs, "bench_phase")
        .iter()
        .map(|v| v["phase"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        phases,
        [
            "local",
            "connect",
            "latency",
            "upload",
            "download",
            "concurrency"
        ]
    );
    for v in events(&evs, "bench_phase") {
        assert_eq!(v["ok"], true, "{v:#}");
        assert!(v["result"].is_object());
    }
    let replies = events(&evs, "reply");
    assert_eq!(replies.len() as u64, p["latency"]["sent"].as_u64().unwrap());
    assert!(replies.iter().all(|v| v["phase"] == "latency"));

    // The listener served the upload and the download over the relay.
    let sink = lis
        .wait_for(30, "upload", |v| {
            v["event"] == "request" && v["kind"] == "sink"
        })
        .await;
    assert_eq!(
        (sink["bytes"].as_u64(), &sink["path"]),
        (Some(1_000_000), &"relay".into())
    );
    assert!(sink["refused"].is_null(), "{sink}");
    let source = lis
        .wait_for(30, "download", |v| {
            v["event"] == "request" && v["kind"] == "source"
        })
        .await;
    assert_eq!(source["requested"], 1_000_000, "{source}");
    assert_eq!(source["bytes"], 1_000_000, "{source}");
    assert!(
        source["refused"].is_null() && num(&source, "ms") > 0.0,
        "{source}"
    );
}

/// Normal mode on loopback: UDP and QAD work, the connection is direct
/// (exit code 0); the relay-only comparison and the idle phase run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_direct_with_relay_compare_and_idle() {
    let infra = Infra::start("bench-direct").await;
    let relay = relay_args(&infra);
    let (_lis, lis_id) = listener(&infra, "lis", &relay).await;

    let mut extra = relay.clone();
    extra.extend(strs([
        "--to",
        &lis_id,
        "--latency-duration",
        "2s",
        "--interval",
        "200ms",
        "--transfer-size",
        "3M",
        "--relay-transfer-size",
        "1M",
        "--concurrency-size",
        "1M",
        "--compare-relay",
        "--idle",
        "1s",
    ]));
    let (code, r, evs) = bench(&bench_args(&infra, &extra)).await;
    assert_eq!(code, Some(0), "{r:#}");
    assert_eq!(r["status"], "healthy");
    assert!(r["issues"].as_array().unwrap().is_empty(), "{r:#}");
    assert!(
        r["verdict"]
            .as_str()
            .unwrap()
            .starts_with("Good: direct connection")
    );

    // QUIC address discovery worked against the local relay.
    let n = &r["local"]["net_report"];
    assert_eq!(n["udp_v4"], true, "{n:#}");
    assert!(
        n["public_v4"].as_str().unwrap().starts_with("127.0.0.1:"),
        "{n:#}"
    );
    assert_eq!(n["behind_nat_v4"], false, "{n:#}");
    assert_eq!(n["port_preserved_v4"], true, "{n:#}");
    assert_eq!(n["qad_relays_v4"], 1);
    // One relay cannot tell whether the NAT mapping depends on the
    // destination; without a NAT (loopback) that is not an unknown.
    assert!(n["mapping_varies_by_dest"].is_null());
    assert!(
        !r["unknowns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["item"] == "nat_mapping"),
        "{r:#}"
    );
    assert!(num(&r["local"]["relays"][0], "qad_v4_ms") > 0.0);

    let p = &r["peer"];
    check_latency(&p["latency"], "direct");
    assert_eq!(p["path"]["final_path"], "direct");
    assert!(p["connection_lost"].is_null(), "{p:#}");
    assert!(num(&p["path"], "time_to_direct_ms") >= 0.0);
    for dir in ["upload", "download"] {
        check_transfer(&p[dir], 3_000_000, "direct");
        assert_eq!(p[dir]["relay_capped"], false);
    }
    let i = &p["idle"];
    assert_eq!(
        (&i["ok"], &i["survived"]),
        (&true.into(), &true.into()),
        "{i:#}"
    );
    assert_eq!(i["attempts"], 1);
    assert!(num(i, "recovery_ms") > 0.0);

    // The relay-only comparison went through the local relay, with the
    // relay cap.
    let rc = &r["relay_compare"];
    let relay_url = format!("{}/", infra.relay_url);
    assert!(rc["skipped"].is_null(), "{rc:#}");
    assert_eq!(
        (&rc["ok"], &rc["reachable"]),
        (&true.into(), &true.into()),
        "{rc:#}"
    );
    assert_eq!(rc["relay"], relay_url.as_str());
    assert_eq!(rc["connect"]["path"], "relay");
    check_latency(&rc["latency"], "relay");
    for dir in ["upload", "download"] {
        check_transfer(&rc[dir], 1_000_000, "relay");
        assert_eq!(rc[dir]["relay_capped"], true);
    }
    assert!(
        r["hints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["code"] == "relay_fallback"),
        "{r:#}"
    );
    let phases: Vec<_> = events(&evs, "bench_phase")
        .iter()
        .map(|v| v["phase"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        phases,
        [
            "local",
            "connect",
            "latency",
            "upload",
            "download",
            "concurrency",
            "relay_compare",
            "idle"
        ]
    );
    let reply_phases: std::collections::BTreeSet<_> = events(&evs, "reply")
        .iter()
        .map(|v| v["phase"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        reply_phases.into_iter().collect::<Vec<_>>(),
        ["idle", "latency", "relay_latency"]
    );
}

/// Uploads, downloads and the concurrency phase end at `--transfer-time`. An
/// upload stopped there reports what the listener received (the listener
/// answers a stopped upload with its count), a download what arrived; the
/// concurrency phase cuts off the requests still running, which are not
/// failures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phases_end_at_the_time_limit() {
    let infra = Infra::start("bench-time-limit").await;
    let (mut lis, lis_id) = listener(&infra, "lis", &[]).await;
    let a = strs([
        "--to",
        &lis_id,
        "--skip",
        "local,latency",
        "--transfer-size",
        "90M",
        "--transfer-time",
        "200ms",
        "--streams",
        "16",
        "--concurrency-size",
        "1G",
    ]);
    let (code, r, evs) = bench(&bench_args(&infra, &a)).await;
    assert_eq!(code, Some(0), "{r:#}");
    let p = &r["peer"];
    for dir in ["upload", "download"] {
        let t = &p[dir];
        assert_eq!(
            (&t["ok"], &t["limited_by"]),
            (&true.into(), &"time".into()),
            "{t:#}"
        );
        let bytes = t["bytes"].as_u64().unwrap();
        assert!(bytes > 0 && bytes < 90_000_000, "{t:#}");
        assert!(num(t, "throughput_mbps") > 0.0, "{t:#}");
    }
    let v = lis
        .wait_for(30, "the stopped upload", |v| {
            v["event"] == "request" && v["kind"] == "sink"
        })
        .await;
    assert!(v["refused"].is_null(), "{v}");
    assert_eq!(v["bytes"], p["upload"]["bytes"], "{v}");

    let c = &p["concurrency"];
    assert_eq!((&c["ok"], &c["failed"]), (&true.into(), &0.into()), "{c:#}");
    assert_eq!(c["limited_by"], "time", "{c:#}");
    assert!(c["unfinished"].as_u64().unwrap() <= 16, "{c:#}");
    // Requests are 256 KiB, or smaller (at least 4 KiB) if the transfers
    // were too slow for that within the time limit. (If none of them
    // finished in 100 ms, the phase started over with smaller ones.)
    let size = c["request_bytes"].as_u64().unwrap();
    let first = c["restart"]["request_bytes"].as_u64().unwrap_or(size);
    if c["sized_for_mbps"].is_null() {
        assert_eq!(first, 262_144, "{c:#}");
    } else {
        assert!((4096..262_144).contains(&first), "{c:#}");
        assert_eq!(num(c, "sized_for_mbps"), sustained(p), "{c:#}");
    }
    assert!(r["issues"].as_array().unwrap().is_empty(), "{r:#}");

    // Each phase took about the time limit, not the time limit plus a
    // request timeout (10 s).
    for e in events(&evs, "bench_phase") {
        if ["upload", "download", "concurrency"].contains(&e["phase"].as_str().unwrap()) {
            assert!(num(e, "duration_ms") < 3000.0, "{e}");
        }
    }
}

/// The concurrency phase lasts until its workers are done, not until the
/// small requests sent meanwhile (every `--interval`) next notice: on fast
/// paths every duration was a multiple of the 200 ms interval (campaign 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_ends_with_its_workers() {
    let infra = Infra::start("bench-conc-end").await;
    let (_lis, lis_id) = listener(&infra, "lis", &[]).await;
    let a = strs([
        "--to",
        &lis_id,
        "--skip",
        "local,latency,upload,download",
        "--interval",
        "5s",
        "--streams",
        "4",
        "--concurrency-size",
        "1M",
    ]);
    let (code, r, evs) = bench(&bench_args(&infra, &a)).await;
    assert_eq!(code, Some(0), "{r:#}");
    let c = &r["peer"]["concurrency"];
    assert_eq!(
        (&c["ok"], &c["limited_by"]),
        (&true.into(), &"size".into()),
        "{c:#}"
    );
    // 1M allows 3 requests of 256 KiB; on loopback they take milliseconds.
    assert_eq!(c["requests"], 3, "{c:#}");
    assert!(c["restart"].is_null(), "{c:#}");
    assert!(num(c, "duration_ms") < 2500.0, "{c:#}");
    let phase = events(&evs, "bench_phase")
        .into_iter()
        .find(|e| e["phase"] == "concurrency")
        .unwrap();
    assert!(num(phase, "duration_ms") < 2500.0, "{phase}");
    // The small request sent at the start of the phase.
    assert!(c["loaded_samples"].as_u64().unwrap() <= 1, "{c:#}");
}

/// After transfers that measured a link too slow for 256 KiB requests within
/// the time limit (here: 1 kB transfers, whose throughput is mostly round
/// trips, and 64 streams in 1 s), the concurrency phase uses smaller
/// requests, so that it still measures a throughput.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_requests_fit_a_slow_link() {
    let infra = Infra::start("bench-sized").await;
    let (_lis, lis_id) = listener(&infra, "lis", &[]).await;
    let a = strs([
        "--to",
        &lis_id,
        "--skip",
        "local,latency",
        "--transfer-size",
        "1k",
        "--transfer-time",
        "1s",
        "--streams",
        "64",
        "--concurrency-size",
        "1M",
    ]);
    let (code, r, _) = bench(&bench_args(&infra, &a)).await;
    assert_eq!(code, Some(0), "{r:#}");
    let p = &r["peer"];
    let c = &p["concurrency"];
    assert_eq!(num(c, "sized_for_mbps"), sustained(p), "{c:#}");
    let size = c["request_bytes"].as_u64().unwrap();
    assert!((4096..262_144).contains(&size) && size % 1024 == 0, "{c:#}");
    assert_eq!(r["settings"]["concurrency_request_bytes"], 262_144);
    assert_eq!((&c["ok"], &c["failed"]), (&true.into(), &0.into()), "{c:#}");
    assert!(c["requests"].as_u64().unwrap() > 0, "{c:#}");
    assert!(num(c, "throughput_mbps") > 0.0, "{c:#}");
}

/// The listener's limits: uploads and downloads beyond `--max-transfer-size`
/// are refused cleanly (exit code 5, issue `transfer_refused`), within it they
/// work; `--max-transfers 0` refuses all. (`probe --bulk`: tests/probe.rs.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listener_limits() {
    let infra = Infra::start("bench-limits").await;
    let (mut lis, lis_id) = listener(
        &infra,
        "lis",
        &strs(["--max-transfer-size", "100k", "--max-transfers", "2"]),
    )
    .await;
    let quick = |size: &str| {
        let mut a = strs([
            "--to",
            &lis_id,
            "--skip",
            "local,latency,concurrency",
            "--transfer-size",
        ]);
        a.push(size.to_string());
        a
    };

    // Too large: both directions refused, with the limit in the message.
    let (code, r, _) = bench(&bench_args(&infra, &quick("1M"))).await;
    assert_eq!(code, Some(5), "{r:#}");
    assert_eq!(r["status"], "degraded");
    let issues = r["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 2, "{r:#}");
    for (issue, dir) in issues.iter().zip(["upload", "download"]) {
        assert_eq!(issue["code"], "transfer_refused");
        let t = &r["peer"][dir];
        assert_eq!(t["ok"], false);
        assert_eq!(t["error_kind"], "refused");
        assert!(
            t["error"]
                .as_str()
                .unwrap()
                .contains("at most 100000 bytes"),
            "{t:#}"
        );
    }
    assert!(r["local"].is_null() && r["peer"]["latency"].is_null());
    for _ in 0..2 {
        let v = lis
            .wait_for(30, "refused transfer", |v| {
                v["event"] == "request" && v["refused"] == "too_large"
            })
            .await;
        assert!(v["kind"] == "sink" || v["kind"] == "source", "{v}");
        // An upload is cut off right after the limit.
        assert!(v["bytes"].as_u64().unwrap() <= 1_000_000, "{v}");
    }

    // Exactly the limit works.
    let (code, r, _) = bench(&bench_args(&infra, &quick("100k"))).await;
    assert_eq!(code, Some(0), "{r:#}");
    check_transfer(&r["peer"]["upload"], 100_000, "direct");
    check_transfer(&r["peer"]["download"], 100_000, "direct");

    // A listener that serves no transfers at all.
    let (_lis0, lis0_id) = listener(&infra, "lis0", &strs(["--max-transfers", "0"])).await;
    let mut a = strs(["--to", &lis0_id, "--skip", "local,latency,concurrency"]);
    a.extend(strs(["--transfer-size", "10k"]));
    let (code, r, _) = bench(&bench_args(&infra, &a)).await;
    assert_eq!(code, Some(5), "{r:#}");
    for dir in ["upload", "download"] {
        let e = r["peer"][dir]["error"].as_str().unwrap();
        assert!(e.contains("maximum of 0 transfers"), "{dir}: {e}");
    }
}

/// No peer: only the local check. Healthy with the relay's CA; without it,
/// the relay's certificate is untrusted (exit code 5, with the advice to use
/// `--relay-ca`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_local_only() {
    let infra = Infra::start("bench-local").await;
    let (code, r, evs) = bench(&bench_args(&infra, &relay_args(&infra))).await;
    assert_eq!(code, Some(0), "{r:#}");
    assert!(r["peer"].is_null() && r["to"].is_null());
    assert_eq!(r["local"]["relays"][0]["ok"], true);
    assert!(
        r["verdict"].as_str().unwrap().contains("UDP works"),
        "{r:#}"
    );
    let phases: Vec<_> = events(&evs, "bench_phase")
        .iter()
        .map(|v| v["phase"].clone())
        .collect();
    assert_eq!(phases, ["local"]);

    // Without --relay-ca the relay's self-signed certificate is not trusted.
    let args: Vec<String> = bench_args(&infra, &relay_args(&infra));
    let i = args.iter().position(|a| a == "--relay-ca").unwrap();
    let args: Vec<String> = args[..i].iter().chain(&args[i + 2..]).cloned().collect();
    let (code, r, _) = bench(&args).await;
    assert_eq!(code, Some(5), "{r:#}");
    let rc = &r["local"]["relays"][0];
    assert_eq!(rc["ok"], false);
    assert_eq!(rc["error_kind"], "tls_untrusted", "{rc:#}");
    assert!(num(rc, "tcp_ms") > 0.0 && rc["tls_ms"].is_null());
    let issue = &r["issues"][0];
    assert_eq!(issue["code"], "relay_unreachable");
    assert!(
        issue["message"].as_str().unwrap().contains("--relay-ca"),
        "{issue}"
    );
}

/// A node that is not in the directory: exit code 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bench_unknown_node() {
    let infra = Infra::start("bench-unknown").await;
    let (nobody, _) = infra.key("nobody");
    let args = bench_args(
        &infra,
        &strs(["--to", &nobody.to_string(), "--skip", "local"]),
    );
    let (code, r, _) = bench(&args).await;
    assert_eq!(code, Some(3), "{r:#}");
    assert_eq!(r["status"], "no_connection");
    let c = &r["peer"]["connect"];
    assert_eq!(
        (&c["ok"], &c["error_kind"]),
        (&false.into(), &"not_found".into())
    );
    assert!(
        r["verdict"]
            .as_str()
            .unwrap()
            .contains("not in any directory")
    );
    assert!(r["peer"]["latency"].is_null() && r["peer"]["upload"].is_null());
}

/// SIGTERM ends a bench early: the report is still printed, last, with
/// `interrupted: true`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_ends_bench_with_report() {
    let infra = Infra::start("bench-sigterm").await;
    let (_lis, lis_id) = listener(&infra, "lis", &[]).await;
    let args = bench_args(
        &infra,
        &strs([
            "--to",
            &lis_id,
            "--latency-duration",
            "60s",
            "--interval",
            "100ms",
        ]),
    );
    let mut cli = Cli::spawn(&args);
    cli.wait_for(30, "3 replies", |v| v["event"] == "reply" && v["seq"] == 3)
        .await;
    cli.terminate();
    let (status, evs) = cli.finish(30).await;
    let r = evs.last().unwrap();
    assert_eq!(r["event"], "bench_report", "{evs:#?}");
    assert_eq!(r["interrupted"], true);
    assert!(
        r["verdict"].as_str().unwrap().starts_with("Interrupted"),
        "{r:#}"
    );
    assert!(r["peer"]["latency"]["sent"].as_u64().unwrap() >= 3, "{r:#}");
    assert!(
        r["peer"]["upload"].is_null(),
        "later phases do not run: {r:#}"
    );
    assert!(num(r, "duration_ms") < 30_000.0);
    assert_eq!(status.code(), Some(0), "{r:#}");
}

#[test]
fn invalid_arguments() {
    for args in [
        &[
            "bench",
            "--directory",
            "http://127.0.0.1:9/",
            "--skip",
            "bogus",
        ][..],
        &[
            "bench",
            "--directory",
            "http://127.0.0.1:9/",
            "--compare-relay",
        ],
        &[
            "bench",
            "--directory",
            "http://127.0.0.1:9/",
            "--idle",
            "1s",
        ],
        &["listen", "--directory", "http://127.0.0.1:9/"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_golem-p2p"))
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}
