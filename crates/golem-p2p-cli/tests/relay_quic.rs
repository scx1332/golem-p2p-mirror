//! The QUIC address discovery (QAD) port of a relay is configurable on the
//! command line and reaches iroh: the local relay runs QAD on a random UDP
//! port (not iroh's default 7842), and must see QAD connections from the node.

mod common;

use std::time::Duration;

use common::*;

/// Starts `listen --json` with `relay_args`, checks the configured QAD port
/// in `ready`, and returns the node.
async fn listen(infra: &Infra, name: &str, relay_args: &[&str], port: Option<u16>) -> Cli {
    let (_, key) = infra.key(name);
    let mut args: Vec<String> = ["--json", "listen"].map(String::from).to_vec();
    args.extend(infra.node_args(&key));
    args.extend(relay_args.iter().map(|s| s.to_string()));
    let mut cli = Cli::spawn(&args);
    let ready = cli.wait_event("ready", 30).await;
    assert_eq!(
        ready["relay_config"],
        serde_json::json!([{"url": format!("{}/", infra.relay_url), "quic_port": port}]),
        "{ready}"
    );
    cli
}

/// Waits until the relay has accepted more than `before` QAD connections.
async fn wait_for_qad(infra: &Infra, before: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while infra.qad_connections() <= before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no QAD connection reached the relay's QUIC port"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_quic_port_reaches_relay() {
    let infra = Infra::start("relay-quic").await;
    let port = infra.quic_port;
    assert_ne!(port, 7842);
    assert_eq!(infra.qad_connections(), 0);

    // `--relay-quic-port` applies to every relay.
    let p = port.to_string();
    let a = listen(
        &infra,
        "a",
        &["--relay", &infra.relay_url, "--relay-quic-port", &p],
        Some(port),
    )
    .await;
    wait_for_qad(&infra, 0).await;
    drop(a);

    // A per-relay `,quic=PORT` wins over `--relay-quic-port`.
    let before = infra.qad_connections();
    let spec = format!("{},quic={port}", infra.relay_url);
    let b = listen(
        &infra,
        "b",
        &["--relay", &spec, "--relay-quic-port", "off"],
        Some(port),
    )
    .await;
    wait_for_qad(&infra, before).await;
    drop(b);

    // `off` disables QAD; without any setting, iroh's default port is used.
    listen(
        &infra,
        "c",
        &["--relay", &infra.relay_url, "--relay-quic-port", "off"],
        None,
    )
    .await;
    listen(&infra, "d", &["--relay", &infra.relay_url], Some(7842)).await;
}

#[test]
fn invalid_quic_ports_rejected() {
    for (flag, value) in [
        ("--relay-quic-port", "0"),
        ("--relay-quic-port", "65536"),
        ("--relay-quic-port", "on"),
        ("--relay", "https://127.0.0.1:3340,quic=x"),
        ("--relay", "https://127.0.0.1:3340,port=1"),
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_golem-p2p"))
            .args([
                "listen",
                "--key",
                "/nonexistent/golem-p2p-test.key",
                "--directory",
                "http://127.0.0.1:9/",
                flag,
                value,
            ])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{flag} {value}: {stderr}");
        assert!(stderr.contains(value), "{flag} {value}: {stderr}");
    }
}
