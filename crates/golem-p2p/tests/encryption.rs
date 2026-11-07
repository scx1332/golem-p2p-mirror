//! Encryption and transport policy: only the golem-p2p ALPN is accepted,
//! relays see ciphertext only, and relays must use https.
//!
//! There is one mode, always encrypted (QUIC/TLS 1.3 from iroh), so there is
//! no plaintext variant to test.

mod common;

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use common::*;
use golem_p2p::{
    DiscoveryError, Endpoint, Error, HttpDirectory, Identities, InMemorySigner, MemoryDiscovery,
    NodeId, RecordError, TransportMode, VerifiedRecord,
    discovery::Discovery,
    iroh::{
        self, RelayConfig, RelayMode, RelayUrl, SecretKey,
        endpoint::{ConnectError, ConnectingError, ConnectionError, TransportErrorCode, presets},
        tls::CaTlsConfig,
    },
    record::{NodeRecord, RECORD_VERSION, testing},
    wire::ALPN,
};
use iroh_relay::server::{RelayConfig as RelayServerConfig, Server, ServerConfig};
use rustls_pki_types::CertificateDer;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A bare iroh endpoint on loopback that does not speak golem-p2p.
async fn raw_endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// Returns `true` if the server refused the TLS handshake because it supports
/// none of the offered ALPNs (TLS alert 120, `no_application_protocol`, sent
/// as a QUIC CRYPTO_ERROR).
fn refused_for_alpn(err: &ConnectError) -> bool {
    let source = match err {
        ConnectError::Connection { source, .. } => source,
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => source,
        _ => return false,
    };
    matches!(
        source,
        ConnectionError::ConnectionClosed(close)
            if close.error_code == TransportErrorCode::crypto(120)
    )
}

/// A QUIC peer that offers any ALPN other than golem-p2p's fails the TLS
/// handshake. The node accepts nothing else, and keeps serving golem-p2p
/// peers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn other_alpn_refused() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let b = net
            .node(&signer, Identities::new(b_id), NodeOpts::default())
            .await;
        spawn_echo(&b);
        let addr = b.local_record().endpoint_addr();
        let raw = raw_endpoint().await;

        for alpn in [
            &b"golem-p2p/0"[..],
            b"golem-p2p/2",
            b"golem-p2p",
            b"golem-p2p/1 ",
            b"h3",
            b"n0/iroh/test/1",
        ] {
            let err = raw
                .connect(addr.clone(), alpn)
                .await
                .expect_err("connection with a foreign ALPN must fail");
            assert!(
                refused_for_alpn(&err),
                "{}: unexpected error {err:?}",
                String::from_utf8_lossy(alpn)
            );
        }

        // Control: the same endpoint and address work with the golem-p2p ALPN,
        // so the failures above come from the ALPN alone.
        let conn = raw.connect(addr.clone(), ALPN).await.unwrap();
        assert_eq!(conn.alpn(), ALPN);
        conn.close(0u32.into(), b"done");

        // The node still serves golem-p2p peers, over the golem-p2p ALPN.
        let a = net
            .node(&signer, Identities::new(a_id), NodeOpts::default())
            .await;
        assert_eq!(echo(&a, b_id, "still up").await.unwrap().data, "still up");
        assert_eq!(
            a.connect(b_id).await.unwrap().iroh_connection().alpn(),
            ALPN
        );

        raw.close().await;
        a.close().await;
        b.close().await;
    })
    .await;
}

/// The bytes one TCP connection carried through the [`RecordingRelay`].
#[derive(Default)]
struct Recorded {
    /// Node to relay server. The relay client masks its WebSocket frames
    /// (RFC 6455), so this direction is not useful for content checks.
    to_relay: Vec<u8>,
    /// Relay server to node. Unmasked; contains every datagram the relay
    /// forwards to this node, verbatim, tagged with the sender's endpoint ID.
    from_relay: Vec<u8>,
}

/// A relay as an operator may deploy it: a plain-HTTP iroh relay server behind
/// a TLS-terminating proxy. Nodes use the proxy's https URL. The proxy records
/// every byte after TLS decryption, which is exactly what the relay server
/// reads and writes.
struct RecordingRelay {
    url: RelayUrl,
    certs: Vec<CertificateDer<'static>>,
    log: Arc<Mutex<Vec<Arc<Mutex<Recorded>>>>>,
    server: Option<Server>,
    proxy: JoinHandle<()>,
}

impl RecordingRelay {
    async fn spawn() -> Self {
        let mut relay = RelayServerConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.key_cache_capacity = Some(1024);
        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        let server = Server::spawn(config).await.expect("spawn relay");
        let relay_addr = server.http_addr().expect("http");

        let (certs, tls) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let log: Arc<Mutex<Vec<Arc<Mutex<Recorded>>>>> = Arc::default();
        let conns = log.clone();
        let proxy = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let conns = conns.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let Ok(upstream) = TcpStream::connect(relay_addr).await else {
                        return;
                    };
                    let rec = Arc::new(Mutex::new(Recorded::default()));
                    conns.lock().unwrap().push(rec.clone());
                    let (tls_r, tls_w) = tokio::io::split(tls);
                    let (up_r, up_w) = upstream.into_split();
                    tokio::join!(
                        pump(tls_r, up_w, rec.clone(), |r| &mut r.to_relay),
                        pump(up_r, tls_w, rec, |r| &mut r.from_relay),
                    );
                });
            }
        });
        Self {
            url,
            certs,
            log,
            server: Some(server),
            proxy,
        }
    }

    /// A snapshot of what every connection carried so far.
    fn recorded(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                let r = r.lock().unwrap();
                (r.to_relay.clone(), r.from_relay.clone())
            })
            .collect()
    }

    async fn shutdown(&mut self) {
        self.proxy.abort();
        if let Some(s) = self.server.take() {
            s.shutdown().await.expect("relay shutdown");
        }
    }
}

/// Copies bytes from `from` to `to`, appending them to the selected buffer.
async fn pump(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    rec: Arc<Mutex<Recorded>>,
    buf_of: fn(&mut Recorded) -> &mut Vec<u8>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf_of(&mut rec.lock().unwrap()).extend_from_slice(&buf[..n]);
        if to.write_all(&buf[..n]).await.is_err() || to.flush().await.is_err() {
            break;
        }
    }
    let _ = to.shutdown().await;
}

/// A distinctive plaintext that must never be visible to a relay.
const MARKER: &str = "<golem-p2p-confidential-marker-5f0c9e2a>";

/// Two relay-only nodes exchange a request and response full of a marker
/// string through a relay. The relay server's view (after relay TLS is
/// terminated) contains the endpoint IDs it routes by, but neither the marker
/// nor the nodes' Ethereum identities: everything else is inside the
/// end-to-end QUIC/TLS session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_sees_only_ciphertext() {
    with_timeout(90, async {
        let mut relay = RecordingRelay::spawn().await;
        let discovery = MemoryDiscovery::new();
        let signer = InMemorySigner::new();
        let (a_id, b_id) = (signer.generate(), signer.generate());
        let node = |id: NodeId| {
            Endpoint::builder(signer.clone(), Identities::new(id))
                .relay_config(RelayConfig::new(relay.url.clone(), None))
                .relay_tls(CaTlsConfig::custom_roots(relay.certs.clone()))
                .transport_mode(TransportMode::RelayOnly)
                .discovery(discovery.clone())
                .portmapper(false)
                .request_timeout(Duration::from_secs(20))
                .online_timeout(Duration::from_secs(10))
                .bind()
        };
        let a = node(a_id).await.unwrap();
        let b = node(b_id).await.unwrap();
        spawn_echo(&b);
        assert_eq!(b.local_record().relay_urls(), vec![relay.url.clone()]);

        let payload = MARKER.repeat(2000);
        let e = echo(&a, b_id, &payload).await.unwrap();
        assert_eq!((e.from, e.to), (a_id, b_id));
        assert_eq!(e.data, payload);
        let conn = a.connect(b_id).await.unwrap();
        assert!(
            conn.iroh_connection().paths().iter().all(|p| p.is_relay()),
            "unexpected non-relay path"
        );

        let recorded = relay.recorded();
        // The request reached B and the response reached A through the relay.
        let forwarded: usize = recorded.iter().map(|(_, down)| down.len()).sum();
        assert!(
            forwarded > 2 * payload.len(),
            "only {forwarded} bytes passed the relay"
        );
        // The recorder sees the relay protocol in the clear: the relay tags
        // forwarded packets with the sender's endpoint ID.
        for ep in [&a, &b] {
            let id = ep.endpoint_id();
            assert!(
                recorded
                    .iter()
                    .any(|(_, down)| contains(down, id.as_bytes())),
                "endpoint ID {id} not visible to the relay"
            );
        }
        // But not the payload, and not the Ethereum identities (they travel
        // in records, inside the QUIC/TLS session).
        for (up, down) in &recorded {
            for bytes in [up, down] {
                assert!(
                    !contains(bytes, MARKER.as_bytes()),
                    "relay saw the plaintext"
                );
                for id in [a_id, b_id] {
                    assert!(!contains(bytes, id.as_bytes()), "relay saw identity {id}");
                }
            }
        }

        a.close().await;
        b.close().await;
        relay.shutdown().await;
    })
    .await;
}

/// Relay URLs given to the builder must be https; `bind` fails otherwise,
/// before anything is bound or published.
#[tokio::test]
async fn builder_rejects_non_https_relay() {
    with_timeout(30, async {
        let signer = InMemorySigner::new();
        let id = signer.generate();
        let discovery = MemoryDiscovery::new();
        for url in ["http://127.0.0.1:3340", "ws://127.0.0.1:3340"] {
            let url: RelayUrl = url.parse().unwrap();

            let err = Endpoint::builder(signer.clone(), Identities::new(id))
                .relay(url.clone())
                .discovery(discovery.clone())
                .bind()
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::InsecureRelayUrl(u) if *u == url.to_string()),
                "{err}"
            );
            assert!(err.to_string().contains("must use https"), "{err}");

            // Also through `relay_config`, next to a valid https relay, and
            // regardless of the transport mode.
            for mode in [
                TransportMode::DirectAndRelay,
                TransportMode::RelayOnly,
                TransportMode::DirectOnly,
            ] {
                let err = Endpoint::builder(signer.clone(), Identities::new(id))
                    .relay("https://relay.example.org".parse().unwrap())
                    .relay_config(RelayConfig::new(url.clone(), None))
                    .transport_mode(mode)
                    .discovery(discovery.clone())
                    .bind()
                    .await
                    .unwrap_err();
                assert!(matches!(err, Error::InsecureRelayUrl(_)), "{mode:?}: {err}");
            }
        }
        // Nothing was published.
        assert!(discovery.lookup(id).await.unwrap().is_none());
    })
    .await;
}

/// A validly signed record that lists a plain-http relay is refused by the
/// directory client, so a node never dials that relay. Control: the same
/// record with an https URL is dialed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn record_with_http_relay_never_dialed() {
    with_timeout(60, async {
        // Counts TCP connections to where the record's relay would be.
        let trap = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let trap_addr = trap.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let trap_task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = trap.accept().await {
                h.fetch_add(1, Ordering::SeqCst);
                held.push(s);
            }
        });

        // Records for `v` at an endpoint nobody runs, reachable only via the
        // trap "relay". Signed by hand: `sign_record` refuses http relays.
        let signer = InMemorySigner::new();
        let v = signer.generate();
        let key = SecretKey::generate();
        let signed = |relay: String, seq_offset: u64| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let rec = NodeRecord {
                version: RECORD_VERSION,
                seq: now.as_micros() as u64 + seq_offset,
                expires_at: now.as_secs() + 600,
                instance_id: [9; 16],
                endpoint_id: *key.public().as_bytes(),
                default_id: v,
                identities: vec![v],
                relay_urls: vec![relay],
                direct_addrs: vec![],
            };
            let d = testing::digest(&rec);
            let sig = signer.sign_blocking(v, &d).unwrap();
            testing::encode_raw(&rec, key.sign(&d).to_bytes(), vec![sig])
        };
        let http_record = signed(format!("http://{trap_addr}/"), 0);
        assert_eq!(
            VerifiedRecord::verify(&http_record).unwrap_err(),
            RecordError::Malformed("relay url must use https")
        );

        // A directory that serves whatever record is current.
        let served = Arc::new(Mutex::new(http_record));
        let s = served.clone();
        let app = axum::Router::new().route(
            "/v1/records/{id}",
            axum::routing::get(move || {
                let body = s.lock().unwrap().clone();
                async move { body }
            }),
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let dir_url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = HttpDirectory::new(dir_url.parse().unwrap()).unwrap();
        assert!(matches!(
            dir.lookup(v).await,
            Err(DiscoveryError::InvalidRecord(_))
        ));

        // A relay-only node with its own (https) relay, which would dial a
        // peer's relay from its record on demand.
        let net = TestNet::new(1).await;
        let p_signer = InMemorySigner::new();
        let p = p_signer.generate();
        let peer = Endpoint::builder(p_signer, Identities::new(p))
            .discovery(dir)
            .relay_config(net.relays[0].config.clone())
            .relay_tls(net.relay_tls())
            .transport_mode(TransportMode::RelayOnly)
            .portmapper(false)
            .connect_timeout(Duration::from_secs(3))
            .online_timeout(Duration::from_secs(10))
            .bind()
            .await
            .unwrap();
        assert!(matches!(peer.request(v, b"hi").await, Err(Error::NotFound(id)) if id == v));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0, "dialed the http relay");

        // Control: with an https URL the record verifies and the node tries
        // the relay (and fails, since the trap does not speak TLS).
        *served.lock().unwrap() = signed(format!("https://{trap_addr}/"), 1);
        assert!(peer.request(v, b"hi").await.is_err());
        wait_for(10, "a dial to the https relay", || {
            hits.load(Ordering::SeqCst) > 0
        })
        .await;

        server.abort();
        trap_task.abort();
        peer.close().await;
    })
    .await;
}
