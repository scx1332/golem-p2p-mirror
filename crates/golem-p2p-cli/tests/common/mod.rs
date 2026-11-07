//! Shared infrastructure of the CLI tests: a local https relay with a
//! self-signed certificate and QUIC address discovery, an HTTP directory, key
//! files, and the CLI as a child process with `--json` output.
//!
//! Everything binds to 127.0.0.1 with OS-assigned ports. Set `RUST_LOG` to
//! see the children's logs.

#![allow(dead_code)]

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use golem_p2p::{InMemorySigner, NodeId, RecordStore};
use iroh_relay::server::{CertConfig, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
};

/// A relay, a directory and a temporary directory for key files.
pub struct Infra {
    pub relay: Server,
    /// `https://127.0.0.1:PORT` (as given on the command line).
    pub relay_url: String,
    /// UDP port of the relay's QUIC address discovery.
    pub quic_port: u16,
    pub dir_url: String,
    pub ca_file: PathBuf,
    pub tmp: PathBuf,
    dir_task: tokio::task::JoinHandle<()>,
}

impl Infra {
    pub async fn start(name: &str) -> Self {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let dir_url = format!("http://{}/", listener.local_addr().unwrap());
        let dir_task = tokio::spawn(async move {
            golem_p2p::directory::serve(listener, Arc::new(RecordStore::default()))
                .await
                .unwrap();
        });

        let (certs, server_config) =
            iroh_relay::server::testing::self_signed_tls_certs_and_config();
        let mut relay_cfg = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay_cfg.tls = Some(TlsConfig::new(
            (Ipv4Addr::LOCALHOST, 0),
            CertConfig::Manual { server_config },
        ));
        let mut cfg = ServerConfig::default();
        cfg.relay = Some(relay_cfg);
        cfg.quic = Some(QuicConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let relay = Server::spawn(cfg).await.unwrap();
        let relay_url = format!("https://{}", relay.https_addr().unwrap());
        let quic_port = relay.quic_addr().unwrap().port();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let tmp = std::env::temp_dir().join(format!(
            "golem-p2p-cli-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let ca_file = tmp.join("relay.crt");
        std::fs::write(
            &ca_file,
            pem::encode(&pem::Pem::new("CERTIFICATE", certs[0].to_vec())),
        )
        .unwrap();
        Self {
            relay,
            relay_url,
            quic_port,
            dir_url,
            ca_file,
            tmp,
            dir_task,
        }
    }

    /// Writes a new identity key file and returns its node ID and path.
    pub fn key(&self, name: &str) -> (NodeId, PathBuf) {
        let signer = InMemorySigner::new();
        let id = signer.generate();
        let path = self.tmp.join(format!("{name}.key"));
        std::fs::write(&path, hex::encode(signer.secret(id).unwrap())).unwrap();
        (id, path)
    }

    /// Node arguments without relays: key, directory, relay CA, loopback,
    /// no port mapping.
    pub fn node_args(&self, key: &Path) -> Vec<String> {
        vec![
            "--key".into(),
            key.display().to_string(),
            "--directory".into(),
            self.dir_url.clone(),
            "--relay-ca".into(),
            self.ca_file.display().to_string(),
            "--bind".into(),
            "127.0.0.1:0".into(),
            "--no-portmapper".into(),
        ]
    }

    /// QAD connections the relay accepted so far.
    pub fn qad_connections(&self) -> u64 {
        self.relay.metrics().server.qad_connections.get()
    }
}

impl Drop for Infra {
    fn drop(&mut self) {
        self.dir_task.abort();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

/// The CLI as a child process; stdout is parsed as JSON lines.
pub struct Cli {
    pub child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    /// Every event read so far.
    pub events: Vec<Value>,
}

impl Cli {
    pub fn spawn<S: AsRef<str>>(args: &[S]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_golem-p2p"))
            .args(args.iter().map(|a| a.as_ref()))
            .stdout(Stdio::piped())
            .stderr(if std::env::var_os("RUST_LOG").is_some() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true)
            .spawn()
            .expect("spawn golem-p2p");
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            lines,
            events: Vec::new(),
        }
    }

    /// Reads the next line as an event. `None` at the end of the output.
    pub async fn next(&mut self) -> Option<Value> {
        let line = self.lines.next_line().await.unwrap()?;
        let v: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
        assert!(v.is_object(), "not a JSON object: {line}");
        self.events.push(v.clone());
        Some(v)
    }

    /// Reads events until one matches `pred`.
    pub async fn wait_for(
        &mut self,
        secs: u64,
        what: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(secs), async {
            loop {
                match self.next().await {
                    Some(v) if pred(&v) => return v,
                    Some(_) => {}
                    None => panic!("output ended before {what}: {:#?}", self.events),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:#?}", self.events))
    }

    /// Reads events until the next one named `name`.
    pub async fn wait_event(&mut self, name: &str, secs: u64) -> Value {
        self.wait_for(secs, name, |v| v["event"] == name).await
    }

    /// Reads the rest of the output and waits for the process to exit.
    pub async fn finish(mut self, secs: u64) -> (ExitStatus, Vec<Value>) {
        tokio::time::timeout(Duration::from_secs(secs), async {
            while self.next().await.is_some() {}
            let status = self.child.wait().await.unwrap();
            (status, self.events)
        })
        .await
        .expect("CLI did not exit in time")
    }

    /// Sends SIGTERM.
    pub fn terminate(&self) {
        let pid = self.child.id().expect("running").to_string();
        let ok = std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill -TERM {pid} failed");
    }
}

/// Runs the CLI to completion.
pub async fn run<S: AsRef<str>>(args: &[S], secs: u64) -> (ExitStatus, Vec<Value>) {
    Cli::spawn(args).finish(secs).await
}

/// Checks the fields every event has.
pub fn check_common(v: &Value, run: Option<&str>, node: Option<&str>) {
    let ts = v["ts"].as_str().unwrap_or_else(|| panic!("no ts: {v}"));
    // 2026-11-07T15:09:12.345Z
    let b = ts.as_bytes();
    assert!(
        b.len() == 24
            && b[4] == b'-'
            && b[7] == b'-'
            && b[10] == b'T'
            && b[13] == b':'
            && b[16] == b':'
            && b[19] == b'.'
            && b[23] == b'Z'
            && ts.chars().filter(|c| c.is_ascii_digit()).count() == 17,
        "bad ts {ts}"
    );
    assert!(v["event"].is_string(), "no event name: {v}");
    assert_eq!(v["run"].as_str(), run, "run of {v}");
    assert_eq!(v["node"].as_str(), node, "node of {v}");
}

/// All events with this name.
pub fn events<'a>(evs: &'a [Value], name: &str) -> Vec<&'a Value> {
    evs.iter().filter(|v| v["event"] == name).collect()
}

/// A number field.
pub fn num(v: &Value, key: &str) -> f64 {
    v[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} is not a number in {v}"))
}
