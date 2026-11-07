//! Identity changes are normal events: default switches, aliases, restarts.

mod common;

use std::time::Duration;

use common::*;
use golem_p2p::{
    Endpoint, Error, Event, Identities, InMemorySigner, NodeId, SignError, TransportMode,
};

fn relay_opts() -> NodeOpts {
    NodeOpts::relays(&[0], TransportMode::RelayOnly)
}

/// Asserts that `peer` cannot reach `id` (unknown, withdrawn or rejected).
async fn assert_unreachable(peer: &Endpoint, id: NodeId) {
    match echo(peer, id, "nobody home?").await {
        Err(Error::NotFound(_))
        | Err(Error::NotPeerIdentity(_))
        | Err(Error::Rejected(_))
        | Err(Error::StreamRejected(_)) => {}
        other => panic!("{id} should be unreachable, got {other:?}"),
    }
}

/// Switching the default identity at runtime: the new default becomes
/// reachable, the old one stays reachable as an alias, outgoing traffic uses
/// the new default, and connected peers see the change without reconnecting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switch_default_without_restart() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (x, y, p_id) = (signer.generate(), signer.generate(), signer.generate());
        let node = net.node(&signer, Identities::new(x), relay_opts()).await;
        let peer = net.node(&signer, Identities::new(p_id), relay_opts()).await;
        spawn_echo(&node);
        spawn_echo(&peer);
        let mut events = peer.subscribe();

        assert_eq!(echo(&peer, x, "1").await.unwrap().to, x);
        let conn = peer.connect(x).await.unwrap();

        node.set_default_identity(y).await.unwrap();
        assert_eq!(node.node_id(), y);
        assert_eq!(node.identities(), vec![y, x]);

        // The connected peer learns the new record over the existing connection.
        wait_for(10, "peer update", || conn.peer().default_id() == y).await;
        let ev = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(Event::PeerUpdated { previous, current }) = events.recv().await {
                    return (previous, current);
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(ev.0.default_id(), x);
        assert_eq!(ev.1.default_id(), y);

        // Both identities reach the same node, over the same connection.
        let e = echo(&peer, y, "2").await.unwrap();
        assert_eq!((e.to, e.from), (y, p_id));
        assert_eq!(echo(&peer, x, "3").await.unwrap().to, x);
        assert_eq!(peer.connect(y).await.unwrap().stable_id(), conn.stable_id());

        // Outgoing requests now come from the new default.
        let e = echo(&node, p_id, "4").await.unwrap();
        assert_eq!((e.to, e.from), (p_id, y));

        // Dropping the old identity entirely makes it unreachable.
        node.set_identities(Identities::new(y)).await.unwrap();
        wait_for(10, "peer update", || !conn.peer().contains(&x)).await;
        assert_unreachable(&peer, x).await;
        assert_eq!(echo(&peer, y, "5").await.unwrap().to, y);

        // A third node that never saw the old record finds the new identity.
        let third_id = signer.generate();
        let third = net
            .node(&signer, Identities::new(third_id), relay_opts())
            .await;
        assert_eq!(echo(&third, y, "6").await.unwrap().to, y);
        assert_unreachable(&third, x).await;

        for ep in [node, peer, third] {
            ep.close().await;
        }
    })
    .await;
}

/// Switching the default identity across a restart: the new process has a new
/// endpoint key; peers that cached the old record still reach the node.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switch_default_with_restart() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (x, y, p_id) = (signer.generate(), signer.generate(), signer.generate());
        let peer = net.node(&signer, Identities::new(p_id), relay_opts()).await;
        spawn_echo(&peer);

        let node = net.node(&signer, Identities::new(x), relay_opts()).await;
        spawn_echo(&node);
        let old_ep = node.endpoint_id();
        assert_eq!(
            echo(&peer, x, "1").await.unwrap().server_endpoint,
            old_ep.to_string()
        );
        node.close().await;

        // Restart with Y as default and X kept as alias.
        let node = net
            .node(&signer, Identities::with_aliases(y, [x]), relay_opts())
            .await;
        spawn_echo(&node);
        assert_ne!(node.endpoint_id(), old_ep);

        let e = echo_eventually(&peer, y, "2", 20).await;
        assert_eq!((e.to, e.from), (y, p_id));
        assert_eq!(e.server_endpoint, node.endpoint_id().to_string());
        // The cached binding of X to the old process is replaced, not reused.
        let e = echo_eventually(&peer, x, "3", 20).await;
        assert_eq!(e.server_endpoint, node.endpoint_id().to_string());
        assert_eq!(
            peer.record_store().resolve(&x).unwrap().endpoint_id(),
            node.endpoint_id()
        );
        // Outgoing traffic uses the new default.
        assert_eq!(echo(&node, p_id, "4").await.unwrap().from, y);

        // Restart again with Y only: X becomes unreachable.
        node.close().await;
        let node = net.node(&signer, Identities::new(y), relay_opts()).await;
        spawn_echo(&node);
        assert_eq!(
            echo_eventually(&peer, y, "5", 20).await.server_endpoint,
            node.endpoint_id().to_string()
        );

        node.close().await;
        peer.close().await;
    })
    .await;
}

/// A -> B -> A without restart. Each step must be reachable and the previous
/// identity withdrawn, at the peer that is connected all along and at a peer
/// that only uses discovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switch_back_a_b_a_without_restart() {
    with_timeout(90, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (a, b, p_id) = (signer.generate(), signer.generate(), signer.generate());
        let node = net.node(&signer, Identities::new(a), relay_opts()).await;
        let peer = net.node(&signer, Identities::new(p_id), relay_opts()).await;
        spawn_echo(&node);
        spawn_echo(&peer);

        assert_eq!(echo(&peer, a, "a1").await.unwrap().to, a);

        for (cur, prev, round) in [(b, a, "b"), (a, b, "a2")] {
            node.set_identities(Identities::new(cur)).await.unwrap();
            // Connected peer.
            let e = echo_eventually(&peer, cur, round, 10).await;
            assert_eq!(e.to, cur);
            assert_unreachable(&peer, prev).await;
            assert_eq!(echo(&node, p_id, round).await.unwrap().from, cur);
            // Fresh peer, discovery only.
            let fresh_id = signer.generate();
            let fresh = net
                .node(&signer, Identities::new(fresh_id), relay_opts())
                .await;
            assert_eq!(echo(&fresh, cur, round).await.unwrap().to, cur);
            assert_unreachable(&fresh, prev).await;
            fresh.close().await;
        }

        node.close().await;
        peer.close().await;
    })
    .await;
}

/// A -> B -> A with a restart at every step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switch_back_a_b_a_with_restart() {
    with_timeout(90, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (a, b, p_id) = (signer.generate(), signer.generate(), signer.generate());
        let peer = net.node(&signer, Identities::new(p_id), relay_opts()).await;
        spawn_echo(&peer);

        let mut prev: Option<NodeId> = None;
        for cur in [a, b, a] {
            let node = net.node(&signer, Identities::new(cur), relay_opts()).await;
            spawn_echo(&node);
            let e = echo_eventually(&peer, cur, "hi", 20).await;
            assert_eq!(e.to, cur);
            assert_eq!(e.server_endpoint, node.endpoint_id().to_string());
            assert_eq!(echo(&node, p_id, "back").await.unwrap().from, cur);
            if let Some(prev) = prev
                && prev != cur
            {
                // The previous identity's record points at a dead endpoint.
                assert!(echo(&peer, prev, "gone").await.is_err());
            }
            node.close().await;
            prev = Some(cur);
        }
        peer.close().await;
    })
    .await;
}

/// An alias added at runtime is reachable without a restart, can send, and
/// disappears again when removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_and_remove_alias_at_runtime() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (x, z, p_id) = (signer.generate(), signer.generate(), signer.generate());
        let node = net.node(&signer, Identities::new(x), relay_opts()).await;
        let peer = net.node(&signer, Identities::new(p_id), relay_opts()).await;
        spawn_echo(&node);
        spawn_echo(&peer);

        assert_eq!(echo(&peer, x, "1").await.unwrap().to, x);
        assert_unreachable(&peer, z).await;

        let rec = node.add_alias(z).await.unwrap();
        assert_eq!(rec.identities(), &[x, z]);
        // Reachable from the connected peer and from a new one.
        let e = echo_eventually(&peer, z, "2", 10).await;
        assert_eq!((e.to, e.from), (z, p_id));
        let other_id = signer.generate();
        let other = net
            .node(&signer, Identities::new(other_id), relay_opts())
            .await;
        assert_eq!(echo(&other, z, "3").await.unwrap().to, z);
        // The alias can be used as sender.
        let e = node
            .request_from(z, p_id, b"from alias")
            .await
            .map(|b| Echo::parse(&b))
            .unwrap();
        assert_eq!(e.from, z);

        node.remove_alias(z).await.unwrap();
        let conn = peer.connect(x).await.unwrap();
        wait_for(10, "alias removal", || !conn.peer().contains(&z)).await;
        assert_unreachable(&peer, z).await;
        assert_unreachable(&other, z).await;
        assert!(matches!(
            node.request_from(z, p_id, b"x").await,
            Err(Error::NotLocalIdentity(_))
        ));
        assert_eq!(echo(&peer, x, "4").await.unwrap().to, x);

        for ep in [node, peer, other] {
            ep.close().await;
        }
    })
    .await;
}

/// A locked key cannot be added: the update fails and nothing changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_key_blocks_update() {
    with_timeout(30, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (x, z) = (signer.generate(), signer.generate());
        let node = net.node(&signer, Identities::new(x), relay_opts()).await;
        let before = node.local_record();
        signer.lock(z);
        match node.add_alias(z).await {
            Err(Error::Record(golem_p2p::RecordError::Sign(SignError::Locked(id)))) => {
                assert_eq!(id, z)
            }
            other => panic!("expected locked error, got {other:?}"),
        }
        assert_eq!(node.local_record(), before);
        signer.unlock(z);
        assert_eq!(node.add_alias(z).await.unwrap().identities(), &[x, z]);

        // Binding with a locked default identity fails outright.
        signer.lock(x);
        let res = golem_p2p::Endpoint::builder(signer.clone(), Identities::new(x))
            .transport_mode(TransportMode::DirectOnly)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .portmapper(false)
            .bind()
            .await;
        assert!(res.is_err());
        node.close().await;
    })
    .await;
}

/// Restart with the same identity and the same (persisted) endpoint key: the
/// new process is told apart from the old one by its instance ID and higher
/// sequence number, and connections of the old instance are dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_with_persistent_endpoint_key() {
    with_timeout(60, async {
        let net = TestNet::new(1).await;
        let signer = InMemorySigner::new();
        let (x, p_id) = (signer.generate(), signer.generate());
        let key = golem_p2p::iroh::SecretKey::generate();
        let mut opts = relay_opts();
        opts.secret_key = Some(key.clone());

        let peer = net.node(&signer, Identities::new(p_id), relay_opts()).await;
        spawn_echo(&peer);
        let node = net.node(&signer, Identities::new(x), opts.clone()).await;
        spawn_echo(&node);
        let old_instance = node.instance_id();
        let old_seq = node.local_record().seq();
        assert_eq!(echo(&peer, x, "1").await.unwrap().to, x);
        node.close().await;

        let node = net.node(&signer, Identities::new(x), opts).await;
        spawn_echo(&node);
        assert_eq!(node.endpoint_id(), key.public());
        assert_ne!(node.instance_id(), old_instance);
        assert!(node.local_record().seq() > old_seq);

        // The restarted node dials the peer; the peer must treat it as a new
        // instance and serve the request from it.
        let e = echo_eventually(&node, p_id, "2", 20).await;
        assert_eq!(e.from, x);
        let e = echo_eventually(&peer, x, "3", 20).await;
        assert_eq!(e.to, x);
        // Every connection the peer holds to this endpoint is the new instance.
        wait_for(20, "old instance connections gone", || {
            peer.connections()
                .iter()
                .filter(|c| c.remote_endpoint_id() == key.public())
                .all(|c| c.peer().instance_id() == node.instance_id())
        })
        .await;

        node.close().await;
        peer.close().await;
    })
    .await;
}
