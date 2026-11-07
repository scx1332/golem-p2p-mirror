//! Watches an endpoint and reports what happens as events: connections and
//! their selected network path, disconnects, the home relay and our own
//! record.
//!
//! Path changes are event driven. iroh 1.4 reports every change of a
//! connection's open paths and of the selected path on
//! `Connection::path_events`; on each event we re-read the snapshot from
//! `Connection::paths` and compare the selected path (kind and remote
//! address) with the previous one. No polling.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use golem_p2p::{
    Connection, Direction, Endpoint, Event, NodeId, PathKind,
    iroh::{TransportAddr, Watcher},
};
use n0_future::StreamExt;
use tokio::{sync::broadcast, task::JoinSet};
use tracing::{debug, info};

use crate::out::{Out, path_name};

/// The selected network path of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPath {
    pub kind: PathKind,
    /// The remote address: `ip:port` for a direct path, the relay URL for a
    /// relayed one. `None` while no path is selected.
    pub remote: Option<String>,
}

impl SelectedPath {
    pub fn of(conn: &Connection) -> Self {
        let paths = conn.iroh_connection().paths();
        let Some(p) = paths.iter().find(|p| p.is_selected()) else {
            return Self {
                kind: PathKind::Unknown,
                remote: None,
            };
        };
        // Same classification as `golem_p2p::Connection::path`, taken from
        // one snapshot together with the address.
        let kind = if p.is_relay() {
            PathKind::Relay
        } else if p.is_ip() {
            PathKind::Direct
        } else {
            PathKind::Unknown
        };
        let remote = match p.remote_addr() {
            TransportAddr::Ip(a) => a.to_string(),
            TransportAddr::Relay(u) => u.to_string(),
            other => other.to_string(),
        };
        Self {
            kind,
            remote: Some(remote),
        }
    }
}

/// The live connection that currently serves `to`, without dialing.
pub fn connection_to(ep: &Endpoint, to: NodeId) -> Option<Connection> {
    ep.connections()
        .into_iter()
        .filter(|c| c.peer().contains(&to))
        .max_by_key(|c| c.peer().seq())
}

/// The selected path towards `to`, or `Unknown` if there is no connection.
pub fn path_to(ep: &Endpoint, to: NodeId) -> PathKind {
    connection_to(ep, to)
        .map(|c| SelectedPath::of(&c).kind)
        .unwrap_or(PathKind::Unknown)
}

/// Remembers when a direct path to one peer was first seen.
pub struct FirstDirect {
    target: NodeId,
    at: OnceLock<Instant>,
}

impl FirstDirect {
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            at: OnceLock::new(),
        }
    }

    /// Records `now` if this is the first direct path to the target.
    pub fn observe(&self, peer: &Connection, kind: PathKind) {
        if kind == PathKind::Direct && peer.peer().contains(&self.target) {
            let _ = self.at.set(Instant::now());
        }
    }

    /// Records `now` if no direct path to the target was seen before.
    pub fn mark(&self) {
        let _ = self.at.set(Instant::now());
    }

    pub fn get(&self) -> Option<Instant> {
        self.at.get().copied()
    }
}

/// Background tasks reporting endpoint activity. Stopped on drop.
pub struct Observer {
    _tasks: JoinSet<()>,
}

impl Observer {
    /// Starts watching. `events` must be subscribed before anything happens
    /// that should be reported (right after binding).
    pub fn spawn(
        ep: &Endpoint,
        events: broadcast::Receiver<Event>,
        out: &Out,
        first_direct: Option<Arc<FirstDirect>>,
    ) -> Self {
        let mut tasks = JoinSet::new();
        tasks.spawn(watch_events(ep.clone(), events, out.clone(), first_direct));
        tasks.spawn(watch_home_relay(ep.clone(), out.clone()));
        Self { _tasks: tasks }
    }
}

async fn watch_events(
    ep: Endpoint,
    mut events: broadcast::Receiver<Event>,
    out: Out,
    first_direct: Option<Arc<FirstDirect>>,
) {
    let watched = Arc::new(Mutex::new(HashSet::new()));
    let mut conns = JoinSet::new();
    loop {
        let ev = match events.recv().await {
            Ok(ev) => Some(ev),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                out.event("error")
                    .field("kind", "lagged")
                    .field("message", format!("missed {n} endpoint events"))
                    .emit();
                // Possibly a missed `Connected`: rescan the connections.
                None
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        match ev {
            None | Some(Event::Connected { .. }) => {}
            Some(Event::Disconnected { peer, reason }) => {
                if out.json() {
                    out.event("disconnect")
                        .field("peer", peer.default_id().to_string())
                        .field("peer_endpoint", peer.endpoint_id().to_string())
                        .field("reason", reason)
                        .emit();
                } else {
                    debug!(peer = %peer.default_id(), %reason, "disconnected");
                }
                continue;
            }
            Some(Event::PeerUpdated { .. }) => continue,
            Some(Event::LocalRecordUpdated { record }) => {
                let rec = record.record();
                out.event("record")
                    .field("seq", rec.seq)
                    .field(
                        "identities",
                        strings(record.identities().iter().map(|i| i.to_string())),
                    )
                    .field("relays", strings(rec.relay_urls.iter().cloned()))
                    .field(
                        "direct_addrs",
                        strings(rec.direct_addrs.iter().map(|a| a.to_string())),
                    )
                    .emit();
                continue;
            }
        }
        // Start a path watcher for every connection we do not watch yet. The
        // event does not carry the connection, but it is registered before
        // the event is sent.
        for c in ep.connections() {
            if watched.lock().expect("poisoned").insert(c.stable_id()) {
                conns.spawn(watch_path(
                    c,
                    out.clone(),
                    first_direct.clone(),
                    watched.clone(),
                ));
            }
        }
        while conns.try_join_next().is_some() {}
    }
}

/// Reports a connection and every change of its selected path.
async fn watch_path(
    conn: Connection,
    out: Out,
    first_direct: Option<Arc<FirstDirect>>,
    watched: Arc<Mutex<HashSet<usize>>>,
) {
    // Subscribe before reading the snapshot, so no change is missed.
    let mut events = conn.iroh_connection().path_events();
    let peer = conn.peer();
    let peer_id = peer.default_id().to_string();
    let peer_endpoint = conn.remote_endpoint_id().to_string();
    let direction = match conn.direction() {
        Direction::Outgoing => "out",
        Direction::Incoming => "in",
    };
    let mut last = SelectedPath::of(&conn);
    if out.json() {
        out.event("conn")
            .field("peer", peer_id.as_str())
            .field("peer_endpoint", peer_endpoint.as_str())
            .field("direction", direction)
            .field("path", path_name(last.kind))
            .field("remote_addr", last.remote.clone())
            .emit();
    } else {
        debug!(peer = %peer_id, direction, path = path_name(last.kind), "connected");
    }
    if let Some(f) = &first_direct {
        f.observe(&conn, last.kind);
    }
    loop {
        tokio::select! {
            ev = events.next() => if ev.is_none() { break },
            _ = conn.closed() => break,
        }
        let now = SelectedPath::of(&conn);
        if now == last {
            continue;
        }
        if out.json() {
            out.event("path_change")
                .field("peer", conn.peer().default_id().to_string())
                .field("peer_endpoint", peer_endpoint.as_str())
                .field("from", path_name(last.kind))
                .field("to", path_name(now.kind))
                .field("from_addr", last.remote.clone())
                .field("remote_addr", now.remote.clone())
                .emit();
        } else {
            info!(
                peer = %peer_id,
                from = path_name(last.kind),
                to = path_name(now.kind),
                remote_addr = now.remote.as_deref().unwrap_or("-"),
                "path changed"
            );
        }
        if let Some(f) = &first_direct {
            f.observe(&conn, now.kind);
        }
        last = now;
    }
    watched.lock().expect("poisoned").remove(&conn.stable_id());
}

/// Reports the home relay and whether we are connected to it.
async fn watch_home_relay(ep: Endpoint, out: Out) {
    let mut w = ep.iroh().home_relay_status();
    let mut last: Option<(Option<String>, bool)> = None;
    loop {
        let status = w.get();
        let home = status.first();
        let cur = (
            home.map(|s| s.url().to_string()),
            home.is_some_and(|s| s.is_connected()),
        );
        // Nothing to say until a home relay is known.
        let initial_none = last.is_none() && cur.0.is_none();
        if last.as_ref() != Some(&cur) && !initial_none {
            let error = home.and_then(|s| s.last_error()).map(|e| e.to_string());
            if out.json() {
                out.event("home_relay")
                    .field("url", cur.0.clone())
                    .field("connected", cur.1)
                    .field("error", error)
                    .emit();
            } else {
                debug!(
                    url = cur.0.as_deref().unwrap_or("-"),
                    connected = cur.1,
                    "home relay"
                );
            }
            last = Some(cur);
        }
        if w.updated().await.is_err() {
            break;
        }
    }
}

/// A JSON array of strings.
pub fn strings(it: impl IntoIterator<Item = String>) -> serde_json::Value {
    it.into_iter().map(serde_json::Value::from).collect()
}
