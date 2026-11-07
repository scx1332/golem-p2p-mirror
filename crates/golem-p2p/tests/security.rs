//! Rejection of forged, stale and mismatching records, and of spoofed senders.

mod common;

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use common::*;
use golem_p2p::{
    Discovery, DiscoveryError, Endpoint, Error, Identities, InMemorySigner, MemoryDiscovery,
    MultiDiscovery, NodeId, TransportMode, VerifiedRecord,
    iroh::{
        self, RelayMode, SecretKey,
        endpoint::{Connection, presets},
    },
    record::{NodeRecord, RECORD_VERSION, sign_record, testing},
    wire::{self, ALPN, AppHeader, Control, KIND_APP, KIND_CONTROL, RejectReason, codes},
};

/// A bare iroh endpoint that speaks the wire protocol by hand.
async fn raw_endpoint(key: SecretKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// A record for `ids` bound to `key`, signed by whoever `signer` can sign for.
async fn record_for(
    signer: &InMemorySigner,
    key: &SecretKey,
    ep: &iroh::Endpoint,
    ids: &[NodeId],
) -> VerifiedRecord {
    sign_record(&raw_record(key, ep, ids), key, signer)
        .await
        .unwrap()
}

fn raw_record(key: &SecretKey, ep: &iroh::Endpoint, ids: &[NodeId]) -> NodeRecord {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    NodeRecord {
        version: RECORD_VERSION,
        seq: now.as_micros() as u64,
        expires_at: now.as_secs() + 600,
        instance_id: [7; 16],
        endpoint_id: *key.public().as_bytes(),
        default_id: ids[0],
        identities: ids.to_vec(),
        relay_urls: vec![],
        direct_addrs: ep.addr().ip_addrs().copied().collect(),
    }
}

/// Sends a Hello with `record` to `target` and returns the reply, or `None`
/// if the connection was closed without one.
async fn hello(
    attacker: &iroh::Endpoint,
    victim: &Endpoint,
    record: Vec<u8>,
) -> (Option<Control>, Connection) {
    let conn = attacker
        .connect(victim.local_record().endpoint_addr(), ALPN)
        .await
        .unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(&[KIND_CONTROL]).await.unwrap();
    wire::write_frame(
        &mut send,
        &Control::Hello {
            record,
            target: Some(victim.node_id()),
        },
    )
    .await
    .unwrap();
    let reply = wire::read_frame::<Control>(&mut recv, wire::MAX_CONTROL_FRAME)
        .await
        .ok();
    (reply, conn)
}

fn direct() -> NodeOpts {
    NodeOpts::default()
}

/// A directory that tampers with a record (pointing a victim's identity at the
/// attacker's endpoint) is caught by signature verification; the dialer never
/// contacts the attacker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_record_from_directory_rejected() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (v, p_id) = (signer.generate(), signer.generate());
        let victim = net.node(&signer, Identities::new(v), direct()).await;
        let attacker_signer = InMemorySigner::new();
        let m = attacker_signer.generate();
        let attacker = net
            .node(&attacker_signer, Identities::new(m), direct())
            .await;
        spawn_echo(&attacker);

        // Victim's genuine record with the endpoint and addresses swapped.
        let (mut rec, esig, isigs) = testing::decode_raw(victim.local_record().to_bytes());
        rec.endpoint_id = *attacker.endpoint_id().as_bytes();
        rec.direct_addrs = attacker.local_record().record().direct_addrs.clone();
        let isigs = isigs
            .iter()
            .map(|s| golem_p2p::Signature::from_slice(s).unwrap())
            .collect();
        let forged = testing::encode_raw(&rec, esig.try_into().unwrap(), isigs);

        // An HTTP "directory" that serves the forged record for every lookup.
        let app = axum::Router::new().route(
            "/v1/records/{id}",
            axum::routing::get(move || {
                let forged = forged.clone();
                async move { forged }
            }),
        );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let evil =
            golem_p2p::HttpDirectory::new(format!("http://{addr}/").parse().unwrap()).unwrap();

        assert!(matches!(
            evil.lookup(v).await,
            Err(DiscoveryError::InvalidRecord(_))
        ));

        let peer = Endpoint::builder(signer.clone(), Identities::new(p_id))
            .discovery(evil)
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .bind()
            .await
            .unwrap();
        assert!(matches!(peer.request(v, b"hi").await, Err(Error::NotFound(id)) if id == v));
        assert!(
            attacker.connections().is_empty(),
            "dialer contacted the attacker"
        );

        server.abort();
        for ep in [victim, attacker, peer] {
            ep.close().await;
        }
    })
    .await;
}

/// Directories and caches keep the newest record; replaying an older one is
/// refused, and a stale directory cannot hide a newer record held by another.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_record_rejected() {
    with_timeout(60, async {
        let dir = TestDirectory::spawn().await;
        let client = dir.client();
        let stale_dir = MemoryDiscovery::new();
        let net = TestNet::with_discovery(0, Arc::new(client.clone())).await;
        let signer = InMemorySigner::new();
        let (x, z) = (signer.generate(), signer.generate());
        let node = net.node(&signer, Identities::new(x), direct()).await;
        let r1 = node.local_record();
        stale_dir.publish(&r1).await.unwrap();
        let r2 = node.add_alias(z).await.unwrap();
        assert!(r2.seq() > r1.seq());

        // Replaying the older record to the directory is refused.
        assert!(matches!(
            client.publish(&r1).await,
            Err(DiscoveryError::Stale)
        ));
        assert_eq!(client.lookup(x).await.unwrap().unwrap(), r2);
        assert_eq!(dir.store.resolve(&z).unwrap(), r2);

        // A combination of a stale and a current directory returns the newest.
        let multi = MultiDiscovery::new()
            .with(stale_dir.clone())
            .with(client.clone());
        assert_eq!(multi.lookup(x).await.unwrap().unwrap(), r2);
        assert_eq!(multi.lookup(z).await.unwrap().unwrap(), r2);

        // An expired record is not accepted anywhere.
        let later = SystemTime::now() + Duration::from_secs(48 * 3600);
        assert!(VerifiedRecord::verify_at(r2.to_bytes(), later).is_err());

        node.close().await;
    })
    .await;
}

/// A handshake record that lists an identity which did not sign it is
/// refused, as is a record whose endpoint is not the connecting endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_rejects_unsigned_identity_and_foreign_record() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (v, w) = (signer.generate(), signer.generate());
        let victim = net.node(&signer, Identities::new(v), direct()).await;
        let other = net.node(&signer, Identities::new(w), direct()).await;
        let mut events = victim.subscribe();

        let attacker_signer = InMemorySigner::new();
        let m = attacker_signer.generate();
        let key = SecretKey::generate();
        let attacker = raw_endpoint(key.clone()).await;

        // 1. Record listing the attacker's identity m and someone else's
        //    identity w, signed only by m (w's slot carries m's signature).
        let rec = raw_record(&key, &attacker, &[m, w]);
        let d = testing::digest(&rec);
        let sm = attacker_signer.sign_blocking(m, &d).unwrap();
        let bytes = testing::encode_raw(&rec, key.sign(&d).to_bytes(), vec![sm, sm]);
        assert!(VerifiedRecord::verify(&bytes).is_err());
        let (reply, conn) = hello(&attacker, &victim, bytes).await;
        assert!(matches!(
            reply,
            Some(Control::Reject {
                reason: RejectReason::InvalidRecord
            })
        ));
        conn.close(codes::OK, b"");

        // 2. Another node's genuine, valid record presented over the
        //    attacker's own TLS connection.
        let (reply, conn) =
            hello(&attacker, &victim, other.local_record().to_bytes().to_vec()).await;
        assert!(matches!(
            reply,
            Some(Control::Reject {
                reason: RejectReason::InvalidRecord
            })
        ));
        match conn.closed().await {
            iroh::endpoint::ConnectionError::ApplicationClosed(c) => {
                assert_eq!(c.error_code, codes::IDENTITY_MISMATCH)
            }
            other => panic!("unexpected close: {other:?}"),
        }

        // 3. A valid record of the attacker's own, asking for an identity the
        //    victim does not serve.
        let own = record_for(&attacker_signer, &key, &attacker, &[m]).await;
        let conn = attacker
            .connect(victim.local_record().endpoint_addr(), ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(&[KIND_CONTROL]).await.unwrap();
        wire::write_frame(
            &mut send,
            &Control::Hello {
                record: own.to_bytes().to_vec(),
                target: Some(w),
            },
        )
        .await
        .unwrap();
        let reply = wire::read_frame::<Control>(&mut recv, wire::MAX_CONTROL_FRAME)
            .await
            .ok();
        assert!(matches!(
            reply,
            Some(Control::Reject {
                reason: RejectReason::UnknownIdentity
            })
        ));
        conn.close(codes::OK, b"");

        // None of these produced a connection on the victim.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(victim.connections().is_empty());
        while let Ok(ev) = events.try_recv() {
            assert!(
                !matches!(ev, golem_p2p::Event::Connected { .. }),
                "unexpected connection: {ev:?}"
            );
        }

        attacker.close().await;
        victim.close().await;
        other.close().await;
    })
    .await;
}

/// The dialer checks the acceptor too: a server that answers with someone
/// else's record (valid signatures, different endpoint) is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dialer_rejects_server_with_foreign_record() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (v, p_id) = (signer.generate(), signer.generate());
        let victim = net.node(&signer, Identities::new(v), direct()).await;
        let peer = net.node(&signer, Identities::new(p_id), direct()).await;

        // The attacker publishes a valid record for its own identity m...
        let attacker_signer = InMemorySigner::new();
        let m = attacker_signer.generate();
        let key = SecretKey::generate();
        let attacker = raw_endpoint(key.clone()).await;
        let own = record_for(&attacker_signer, &key, &attacker, &[m]).await;
        net.discovery.publish(&own).await.unwrap();

        // ...but answers handshakes with the victim's record, hoping the
        // dialer attributes the connection to the victim's identity.
        let victim_record = victim.local_record().to_bytes().to_vec();
        let server = {
            let attacker = attacker.clone();
            tokio::spawn(async move {
                while let Some(inc) = attacker.accept().await {
                    let Ok(conn) = inc.await else { continue };
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                        continue;
                    };
                    let _ = wire::read_kind(&mut recv).await;
                    let _ = wire::read_frame::<Control>(&mut recv, wire::MAX_CONTROL_FRAME).await;
                    let _ = wire::write_frame(
                        &mut send,
                        &Control::Welcome {
                            record: victim_record.clone(),
                        },
                    )
                    .await;
                    let _ = conn.closed().await;
                }
            })
        };

        assert!(matches!(
            peer.connect(m).await,
            Err(Error::IdentityMismatch)
        ));
        assert!(peer.connections().is_empty());
        assert!(
            peer.record_store().resolve(&v).is_none(),
            "victim record attributed to attacker"
        );

        server.abort();
        attacker.close().await;
        victim.close().await;
        peer.close().await;
    })
    .await;
}

/// A connected peer cannot send as an identity that is not in its verified
/// record, nor address an identity the receiver does not have.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_sender_must_be_bound_to_connection() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (v, p_id, third) = (signer.generate(), signer.generate(), signer.generate());
        let victim = net.node(&signer, Identities::new(v), direct()).await;
        let peer = net.node(&signer, Identities::new(p_id), direct()).await;
        let third_node = net.node(&signer, Identities::new(third), direct()).await;
        let accepted = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let victim = victim.clone();
            let accepted = accepted.clone();
            tokio::spawn(async move {
                while let Some(mut inc) = victim.accept().await {
                    accepted.lock().unwrap().push((inc.from(), inc.to()));
                    let req = inc.read_request(1024).await.unwrap_or_default();
                    let _ = inc.respond(&req).await;
                }
            });
        }

        // A legitimate stream works.
        assert_eq!(peer.request(v, b"ok").await.unwrap(), b"ok");
        let conn = peer.connect(v).await.unwrap();

        // The high-level API refuses to send as a foreign identity.
        assert!(matches!(
            conn.open_bi(third, v).await,
            Err(Error::NotLocalIdentity(_))
        ));

        // Raw stream claiming to come from `third` (not in the peer's record).
        let raw = conn.iroh_connection();
        let try_header = |from: NodeId, to: NodeId| async move {
            let (mut send, mut recv) = raw.open_bi().await.unwrap();
            send.write_all(&[KIND_APP]).await.unwrap();
            wire::write_frame(&mut send, &AppHeader { from, to })
                .await
                .unwrap();
            let _ = send.write_all(b"payload").await;
            let _ = send.finish();
            match recv.read_to_end(1024).await {
                Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(
                    code,
                ))) => code,
                other => panic!("stream was not rejected: {other:?}"),
            }
        };
        assert_eq!(try_header(third, v).await, codes::SENDER_NOT_BOUND);
        // Spoofing the receiver's own identity as sender is just as invalid.
        assert_eq!(try_header(v, v).await, codes::SENDER_NOT_BOUND);
        // Addressing an identity the victim does not have.
        assert_eq!(try_header(p_id, third).await, codes::UNKNOWN_IDENTITY);

        // Only the legitimate stream reached the application.
        assert_eq!(accepted.lock().unwrap().as_slice(), &[(p_id, v)]);

        for ep in [victim, peer, third_node] {
            ep.close().await;
        }
    })
    .await;
}

/// New instance of a node (same identity, same endpoint key) while the
/// previous instance still looks alive: peers drop the old connections and
/// talk to the new instance only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_instance_supersedes_live_old_connection() {
    with_timeout(60, async {
        let net = TestNet::new(0).await;
        let signer = InMemorySigner::new();
        let (x, p_id) = (signer.generate(), signer.generate());
        let key = SecretKey::generate();
        let mut opts = direct();
        opts.secret_key = Some(key.clone());

        let peer = net.node(&signer, Identities::new(p_id), direct()).await;
        spawn_echo(&peer);
        let old = net.node(&signer, Identities::new(x), opts.clone()).await;
        spawn_echo(&old);
        assert_eq!(echo(&peer, x, "1").await.unwrap().to, x);
        let old_conn = peer.connect(x).await.unwrap();
        assert_eq!(old_conn.peer().instance_id(), old.instance_id());

        // The "restarted" process comes up while the old one is still around.
        let new = net.node(&signer, Identities::new(x), opts).await;
        spawn_echo(&new);
        assert_eq!(echo(&new, p_id, "2").await.unwrap().from, x);

        // The peer closed the old instance's connection.
        tokio::time::timeout(Duration::from_secs(10), old_conn.closed())
            .await
            .unwrap();
        let reason = old_conn
            .iroh_connection()
            .close_reason()
            .unwrap()
            .to_string();
        tracing::info!("old connection closed: {reason}");
        let conn = peer.connect(x).await.unwrap();
        assert_eq!(conn.peer().instance_id(), new.instance_id());
        assert_eq!(
            echo(&peer, x, "3").await.unwrap().server_endpoint,
            key.public().to_string()
        );

        for ep in [old, new, peer] {
            ep.close().await;
        }
    })
    .await;
}
