//! Crash and restart of a real process.
//!
//! The node under test runs as a child process (the `golem-p2p listen` CLI),
//! is killed with SIGKILL (no goodbye of any kind), and restarted with the same
//! identity. The peer must reach the new process, and must never get an answer
//! from the old one.

use std::{
    net::Ipv4Addr,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use golem_p2p::{
    Endpoint, HttpDirectory, Identities, InMemorySigner, NodeId, RecordStore,
    iroh::tls::CaTlsConfig,
};
use iroh_relay::server::{CertConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

struct Node {
    child: Child,
    endpoint_id: String,
}

async fn start_node(key: &PathBuf, dir: &str, relay: &str, relay_ca: &PathBuf) -> Node {
    let mut child = Command::new(env!("CARGO_BIN_EXE_golem-p2p"))
        .args(["listen", "--key"])
        .arg(key)
        .args([
            "--directory",
            dir,
            "--relay",
            relay,
            "--bind",
            "127.0.0.1:0",
            "--no-portmapper",
            "--relay-ca",
        ])
        .arg(relay_ca)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn golem-p2p");
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.starts_with("READY ") {
                return line;
            }
        }
        panic!("node exited before READY");
    })
    .await
    .expect("node did not become ready");
    let endpoint_id = ready
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("endpoint_id="))
        .unwrap()
        .to_string();
    // The record lists the relay only once the node is connected to it, which
    // requires the self-signed certificate given with `--relay-ca`.
    assert!(
        ready.contains(&format!("relays=[\"{relay}/\"]")),
        "node is not homed on the https relay: {ready}"
    );
    Node { child, endpoint_id }
}

fn field<'a>(reply: &'a str, key: &str) -> &'a str {
    reply
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        .unwrap_or_else(|| panic!("no {key} in {reply}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_and_restart() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    // Directory and an https relay with a self-signed certificate, both on
    // loopback.
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let dir_url = format!("http://{}/", listener.local_addr().unwrap());
    let dir_task = tokio::spawn(golem_p2p::directory::serve(
        listener,
        Arc::new(RecordStore::default()),
    ));
    let (certs, server_config) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let mut relay_cfg = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay_cfg.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config },
    ));
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(relay_cfg);
    let relay = Server::spawn(cfg).await.unwrap();
    let relay_url = format!("https://{}", relay.https_addr().unwrap());

    // Identity key on disk, as an operator would have it.
    let signer = InMemorySigner::new();
    let node_id: NodeId = signer.generate();
    let tmp = std::env::temp_dir().join(format!(
        "golem-p2p-crash-{}-{}",
        std::process::id(),
        node_id
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let key_file = tmp.join("node.key");
    std::fs::write(&key_file, hex::encode(signer.secret(node_id).unwrap())).unwrap();
    // The relay's certificate, for the CLI's `--relay-ca`.
    let ca_file = tmp.join("relay.crt");
    std::fs::write(
        &ca_file,
        pem::encode(&pem::Pem::new("CERTIFICATE", certs[0].to_vec())),
    )
    .unwrap();

    let peer_signer = InMemorySigner::new();
    let peer_id = peer_signer.generate();
    let peer = Endpoint::builder(peer_signer, Identities::new(peer_id))
        .discovery(HttpDirectory::new(dir_url.parse().unwrap()).unwrap())
        .relay(relay_url.parse().unwrap())
        .relay_tls(CaTlsConfig::custom_roots(certs))
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .portmapper(false)
        .request_timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(5))
        .bind()
        .await
        .unwrap();

    let mut first = start_node(&key_file, &dir_url, &relay_url, &ca_file).await;
    let reply = String::from_utf8(peer.request(node_id, b"before").await.unwrap()).unwrap();
    assert_eq!(field(&reply, "endpoint"), first.endpoint_id);
    assert_eq!(field(&reply, "to"), node_id.to_string());
    assert_eq!(field(&reply, "from"), peer_id.to_string());
    let old_instance = field(&reply, "instance").to_string();

    // Crash: SIGKILL the process we started.
    first.child.kill().await.unwrap();
    let crashed = Instant::now();

    let mut second = start_node(&key_file, &dir_url, &relay_url, &ca_file).await;
    assert_ne!(second.endpoint_id, first.endpoint_id);

    // The peer still holds the dead connection. Requests may fail until it is
    // detected, but every answer must come from the new process.
    let deadline = Instant::now() + Duration::from_secs(60);
    let reply = loop {
        match peer.request(node_id, b"after").await {
            Ok(r) => break String::from_utf8(r).unwrap(),
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "node not reachable after restart: {e}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    assert_eq!(field(&reply, "endpoint"), second.endpoint_id);
    assert_ne!(field(&reply, "instance"), old_instance);
    assert_eq!(field(&reply, "echo"), "after");
    eprintln!(
        "reachable again {} ms after the crash",
        crashed.elapsed().as_millis()
    );

    // And it stays that way.
    for i in 0..5 {
        let msg = format!("again-{i}");
        let reply =
            String::from_utf8(peer.request(node_id, msg.as_bytes()).await.unwrap()).unwrap();
        assert_eq!(field(&reply, "endpoint"), second.endpoint_id);
        assert_eq!(field(&reply, "echo"), msg);
    }
    assert!(
        peer.connections()
            .iter()
            .all(|c| c.remote_endpoint_id().to_string() != first.endpoint_id),
        "peer still uses a connection to the crashed process"
    );

    second.child.kill().await.unwrap();
    peer.close().await;
    relay.shutdown().await.unwrap();
    dir_task.abort();
    let _ = std::fs::remove_dir_all(&tmp);
}
