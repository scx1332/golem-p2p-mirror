//! Shared test infrastructure: local iroh relays, directories and nodes.
//!
//! Everything binds to 127.0.0.1 with OS-assigned ports.

#![allow(dead_code)]

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use golem_p2p::{
    Endpoint, Identities, InMemorySigner, MemoryDiscovery, NodeId, PathKind, RecordStore,
    TransportMode,
    discovery::Discovery,
    iroh::{RelayConfig, RelayUrl, tls::CaTlsConfig},
};
use iroh_relay::{
    RelayQuicConfig,
    server::{
        CertConfig, QuicConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig,
    },
};
use rustls_pki_types::CertificateDer;

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

/// Runs a test body with an overall timeout.
pub async fn with_timeout<F: Future<Output = ()>>(secs: u64, f: F) {
    init_tracing();
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .expect("test timed out");
}

/// A local iroh relay server with a self-signed certificate.
pub struct TestRelay {
    pub url: RelayUrl,
    pub config: RelayConfig,
    pub certs: Vec<CertificateDer<'static>>,
    server: Option<Server>,
}

impl TestRelay {
    pub async fn spawn() -> Self {
        let (certs, server_config) =
            iroh_relay::server::testing::self_signed_tls_certs_and_config();
        let tls = TlsConfig::new(
            (Ipv4Addr::LOCALHOST, 0),
            CertConfig::Manual { server_config },
        );
        let mut relay = RelayServerConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.tls = Some(tls);
        relay.key_cache_capacity = Some(1024);
        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        config.quic = Some(QuicConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let server = Server::spawn(config).await.expect("spawn relay");
        let url: RelayUrl = format!("https://{}", server.https_addr().expect("https"))
            .parse()
            .expect("url");
        let quic = server.quic_addr().map(|a| RelayQuicConfig::new(a.port()));
        Self {
            config: RelayConfig::new(url.clone(), quic),
            url,
            certs,
            server: Some(server),
        }
    }

    /// Stops the relay server.
    pub async fn shutdown(&mut self) {
        if let Some(s) = self.server.take() {
            s.shutdown().await.expect("relay shutdown");
        }
    }
}

/// A local HTTP record directory.
pub struct TestDirectory {
    pub url: url::Url,
    pub store: Arc<RecordStore>,
    task: tokio::task::JoinHandle<()>,
}

impl TestDirectory {
    pub async fn spawn() -> Self {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let store = Arc::new(RecordStore::default());
        let s = store.clone();
        let task = tokio::spawn(async move {
            golem_p2p::directory::serve(listener, s).await.unwrap();
        });
        Self {
            url: format!("http://{addr}/").parse().unwrap(),
            store,
            task,
        }
    }

    pub fn client(&self) -> golem_p2p::HttpDirectory {
        golem_p2p::HttpDirectory::new(self.url.clone()).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Options for a test node.
#[derive(Clone)]
pub struct NodeOpts {
    /// Indices into the relay list of the [`TestNet`].
    pub relays: Vec<usize>,
    pub mode: TransportMode,
    pub secret_key: Option<golem_p2p::iroh::SecretKey>,
}

impl Default for NodeOpts {
    fn default() -> Self {
        Self {
            relays: vec![],
            mode: TransportMode::DirectOnly,
            secret_key: None,
        }
    }
}

impl NodeOpts {
    pub fn relays(relays: &[usize], mode: TransportMode) -> Self {
        Self {
            relays: relays.to_vec(),
            mode,
            secret_key: None,
        }
    }
}

/// A set of local relays plus a shared discovery service.
pub struct TestNet {
    pub relays: Vec<TestRelay>,
    pub discovery: Arc<dyn Discovery>,
}

impl TestNet {
    /// A network with `n` relays and in-memory discovery.
    pub async fn new(n: usize) -> Self {
        Self::with_discovery(n, Arc::new(MemoryDiscovery::new())).await
    }

    pub async fn with_discovery(n: usize, discovery: Arc<dyn Discovery>) -> Self {
        let mut relays = Vec::new();
        for _ in 0..n {
            relays.push(TestRelay::spawn().await);
        }
        Self { relays, discovery }
    }

    /// TLS roots that trust every relay of this network (and nothing else).
    pub fn relay_tls(&self) -> CaTlsConfig {
        CaTlsConfig::custom_roots(self.relays.iter().flat_map(|r| r.certs.clone()))
    }

    pub async fn node(&self, signer: &InMemorySigner, ids: Identities, opts: NodeOpts) -> Endpoint {
        let mut b = Endpoint::builder(signer.clone(), ids)
            .discovery(self.discovery.clone())
            .transport_mode(opts.mode)
            .portmapper(false)
            .request_timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .online_timeout(Duration::from_secs(10));
        if !self.relays.is_empty() {
            b = b.relay_tls(self.relay_tls());
        }
        if opts.mode != TransportMode::RelayOnly {
            b = b.bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
        }
        for i in &opts.relays {
            b = b.relay_config(self.relays[*i].config.clone());
        }
        if let Some(k) = opts.secret_key {
            b = b.secret_key(k);
        }
        b.bind().await.expect("bind")
    }
}

/// What the echo server saw, sent back as the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Echo {
    pub to: NodeId,
    pub from: NodeId,
    pub server_endpoint: String,
    pub data: String,
}

impl Echo {
    pub fn parse(bytes: &[u8]) -> Self {
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        let mut it = s.splitn(4, ' ');
        Self {
            to: it.next().unwrap().parse().unwrap(),
            from: it.next().unwrap().parse().unwrap(),
            server_endpoint: it.next().unwrap().to_string(),
            data: it.next().unwrap_or_default().to_string(),
        }
    }
}

/// Answers every request with an [`Echo`] describing what the server saw.
pub fn spawn_echo(ep: &Endpoint) -> tokio::task::JoinHandle<()> {
    let ep = ep.clone();
    tokio::spawn(async move {
        let me = ep.endpoint_id().to_string();
        while let Some(mut inc) = ep.accept().await {
            let me = me.clone();
            tokio::spawn(async move {
                let Ok(req) = inc.read_request(1 << 20).await else {
                    return;
                };
                let resp = format!(
                    "{} {} {} {}",
                    inc.to(),
                    inc.from(),
                    me,
                    String::from_utf8_lossy(&req)
                );
                let _ = inc.respond(resp.as_bytes()).await;
            });
        }
    })
}

/// Sends a request and parses the echo.
pub async fn echo(from: &Endpoint, to: NodeId, data: &str) -> golem_p2p::Result<Echo> {
    from.request(to, data.as_bytes())
        .await
        .map(|b| Echo::parse(&b))
}

/// Retries `echo` until it succeeds or `secs` pass.
pub async fn echo_eventually(from: &Endpoint, to: NodeId, data: &str, secs: u64) -> Echo {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match echo(from, to, data).await {
            Ok(e) => return e,
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!("echo to {to} not yet working: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => panic!("echo to {to} failed: {e}"),
        }
    }
}

/// Waits until `f` returns true.
pub async fn wait_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Returns the path kind of the connection to the node serving `to`.
pub async fn path_to(ep: &Endpoint, to: NodeId) -> PathKind {
    ep.connect(to).await.unwrap().path()
}
