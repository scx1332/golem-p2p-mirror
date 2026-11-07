//! The golem-p2p endpoint: Ethereum identities on top of an iroh endpoint.

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iroh::{
    EndpointId, RelayConfig, RelayMap, RelayMode, SecretKey, Watcher,
    endpoint::{
        Connection as IrohConnection, PortmapperConfig, QuicTransportConfig, RecvStream,
        SendStream, VarInt, presets,
    },
    tls::CaTlsConfig,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    addrs::{self, Interfaces},
    discovery::Discovery,
    error::{Error, Result},
    identity::NodeId,
    lookup::RecordLookup,
    record::{
        MAX_RELAYS, NodeRecord, RECORD_VERSION, RELAY_URL_SCHEME, VerifiedRecord, sign_record,
    },
    signer::Signer,
    store::{DEFAULT_CAPACITY, RecordStore},
    wire::{
        self, ALPN, AppHeader, Control, KIND_APP, KIND_CONTROL, MAX_CONTROL_FRAME,
        MAX_HEADER_FRAME, RejectReason, codes,
    },
};

/// The identities of a node: one default identity and optional aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identities {
    default: NodeId,
    aliases: Vec<NodeId>,
}

impl Identities {
    /// A node with only a default identity.
    pub fn new(default: NodeId) -> Self {
        Self {
            default,
            aliases: Vec::new(),
        }
    }

    /// A node with a default identity and aliases. Duplicates and the default
    /// itself are removed from the aliases.
    pub fn with_aliases(default: NodeId, aliases: impl IntoIterator<Item = NodeId>) -> Self {
        let mut seen = BTreeSet::from([default]);
        let aliases = aliases.into_iter().filter(|a| seen.insert(*a)).collect();
        Self { default, aliases }
    }

    /// Returns the default identity.
    pub fn default_id(&self) -> NodeId {
        self.default
    }

    /// Returns the aliases (secondary identities).
    pub fn aliases(&self) -> &[NodeId] {
        &self.aliases
    }

    /// Returns all identities, default first.
    pub fn all(&self) -> Vec<NodeId> {
        std::iter::once(self.default)
            .chain(self.aliases.iter().copied())
            .collect()
    }

    /// Returns `true` if `id` is the default or an alias.
    pub fn contains(&self, id: &NodeId) -> bool {
        self.default == *id || self.aliases.contains(id)
    }
}

/// Which transports the endpoint uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportMode {
    /// Direct UDP (with hole punching) and relays. The normal mode.
    #[default]
    DirectAndRelay,
    /// Relays only, no UDP sockets. Forces relayed connections.
    RelayOnly,
    /// Direct UDP only, no relays.
    DirectOnly,
}

/// Whether we dialed a connection or accepted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// We dialed the peer.
    Outgoing,
    /// The peer dialed us.
    Incoming,
}

/// The kind of network path a connection currently uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Direct UDP path (possibly after hole punching).
    Direct,
    /// Through a relay server.
    Relay,
    /// No path selected yet.
    Unknown,
}

/// Something that happened on the endpoint. See [`Endpoint::subscribe`].
#[derive(Debug, Clone)]
pub enum Event {
    /// A connection completed the handshake. `peer` is the verified record.
    Connected {
        /// The verified peer record.
        peer: VerifiedRecord,
        /// Who dialed.
        direction: Direction,
    },
    /// A connection closed.
    Disconnected {
        /// The last verified peer record of the connection.
        peer: VerifiedRecord,
        /// Close reason as reported by QUIC.
        reason: String,
    },
    /// A connected peer published a newer record (for example it switched its
    /// default identity or added an alias).
    PeerUpdated {
        /// The record before the update.
        previous: VerifiedRecord,
        /// The new record.
        current: VerifiedRecord,
    },
    /// Our own record was re-signed (identities or addresses changed).
    LocalRecordUpdated {
        /// The new record.
        record: VerifiedRecord,
    },
}

#[derive(Debug, Clone)]
struct Config {
    record_ttl: Duration,
    max_connections: usize,
    handshake_timeout: Duration,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_response_size: usize,
    /// Extra interface patterns whose addresses are not published.
    exclude_interfaces: Vec<String>,
}

/// Builder for an [`Endpoint`].
pub struct Builder {
    signer: Arc<dyn Signer>,
    identities: Identities,
    relays: Vec<RelayConfig>,
    relay_tls: Option<CaTlsConfig>,
    discovery: Option<Arc<dyn Discovery>>,
    mode: TransportMode,
    bind_addr: Option<SocketAddr>,
    secret_key: Option<SecretKey>,
    min_seq: u64,
    online_timeout: Duration,
    store_capacity: usize,
    config: Config,
}

impl fmt::Debug for Builder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder")
            .field("identities", &self.identities)
            .field("relays", &self.relays)
            .field("mode", &self.mode)
            .field("bind_addr", &self.bind_addr)
            .finish_non_exhaustive()
    }
}

impl Builder {
    fn new(signer: Arc<dyn Signer>, identities: Identities) -> Self {
        Self {
            signer,
            identities,
            relays: Vec::new(),
            relay_tls: None,
            discovery: None,
            mode: TransportMode::default(),
            bind_addr: None,
            secret_key: None,
            min_seq: 0,
            online_timeout: Duration::from_secs(5),
            store_capacity: DEFAULT_CAPACITY,
            config: Config {
                record_ttl: Duration::from_secs(3600),
                max_connections: 1024,
                handshake_timeout: Duration::from_secs(10),
                connect_timeout: Duration::from_secs(15),
                request_timeout: Duration::from_secs(30),
                max_response_size: 16 * 1024 * 1024,
                exclude_interfaces: Vec::new(),
            },
        }
    }

    /// Adds a relay server by URL.
    ///
    /// The URL must use `https`; [`Builder::bind`] fails with
    /// [`Error::InsecureRelayUrl`] otherwise.
    pub fn relay(mut self, url: iroh::RelayUrl) -> Self {
        self.relays.push(RelayConfig::from(url));
        self
    }

    /// Adds a relay server with full configuration (for example a QUIC port
    /// for address discovery, or an auth token).
    ///
    /// The URL must use `https`; [`Builder::bind`] fails with
    /// [`Error::InsecureRelayUrl`] otherwise.
    pub fn relay_config(mut self, config: RelayConfig) -> Self {
        self.relays.push(config);
        self
    }

    /// Sets the TLS roots used to verify relay servers. Defaults to the
    /// embedded web PKI roots. Use this for relays with a self-signed or
    /// private-CA certificate.
    pub fn relay_tls(mut self, tls: CaTlsConfig) -> Self {
        self.relay_tls = Some(tls);
        self
    }

    /// Sets the discovery service used to publish our record and find peers.
    pub fn discovery(mut self, discovery: impl Discovery) -> Self {
        self.discovery = Some(Arc::new(discovery));
        self
    }

    /// Sets which transports to use.
    pub fn transport_mode(mut self, mode: TransportMode) -> Self {
        self.mode = mode;
        self
    }

    /// Binds the UDP socket to this address instead of all interfaces.
    /// Use `127.0.0.1:0` in tests.
    pub fn bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = Some(addr);
        self
    }

    /// Uses a fixed iroh endpoint key instead of a fresh one.
    ///
    /// By default every process start generates a new key (see the design
    /// document for why). A fixed key gives a stable endpoint ID at the cost of
    /// keeping a transport secret on disk.
    pub fn secret_key(mut self, key: SecretKey) -> Self {
        self.secret_key = Some(key);
        self
    }

    /// Lower bound for the next record sequence number, for applications that
    /// persist the last published sequence number across restarts.
    pub fn min_seq(mut self, seq: u64) -> Self {
        self.min_seq = seq;
        self
    }

    /// Sets how long published records are valid. They are re-signed after a
    /// third of this time.
    pub fn record_ttl(mut self, ttl: Duration) -> Self {
        self.config.record_ttl = ttl.clamp(Duration::from_secs(60), crate::record::MAX_TTL);
        self
    }

    /// Maximum number of simultaneous connections.
    pub fn max_connections(mut self, n: usize) -> Self {
        self.config.max_connections = n.max(1);
        self
    }

    /// Timeout for [`Endpoint::request`].
    pub fn request_timeout(mut self, t: Duration) -> Self {
        self.config.request_timeout = t;
        self
    }

    /// Timeout for establishing a connection.
    pub fn connect_timeout(mut self, t: Duration) -> Self {
        self.config.connect_timeout = t;
        self
    }

    /// Leaves the addresses of more network interfaces out of the published
    /// record.
    ///
    /// By default the record already leaves out addresses that are only on
    /// interfaces whose network ends at this host: container and VM bridges
    /// such as `docker0`, Docker's `br-<network id>`, `veth*`, `virbr*`,
    /// `lxcbr*`, `cni*`, `podman*` (the full list is in the design document).
    /// The remaining addresses are ranked (public before private) before the
    /// record's limit of [`MAX_DIRECT_ADDRS`](crate::record::MAX_DIRECT_ADDRS)
    /// is applied.
    ///
    /// Use this for other interfaces, for example VPN interfaces (`wg*`,
    /// `tailscale0`, `zt*`), which are published by default, or a virtual
    /// interface the default list does not know. `pattern` is an interface
    /// name, or a prefix followed by `*`. Can be called several times.
    ///
    /// This only changes what this node publishes in its record. iroh still
    /// offers all its local addresses to connected peers during hole
    /// punching.
    pub fn exclude_interface(mut self, pattern: impl Into<String>) -> Self {
        self.config.exclude_interfaces.push(pattern.into());
        self
    }

    /// How long [`Builder::bind`] waits for a relay connection before
    /// publishing the first record anyway.
    pub fn online_timeout(mut self, t: Duration) -> Self {
        self.online_timeout = t;
        self
    }

    /// Capacity of the peer record cache.
    pub fn record_cache_capacity(mut self, n: usize) -> Self {
        self.store_capacity = n;
        self
    }

    /// Binds the endpoint, signs and publishes the first record.
    ///
    /// Fails with [`Error::InsecureRelayUrl`] if a relay URL does not use
    /// `https`.
    pub async fn bind(self) -> Result<Endpoint> {
        // Relays are https only, so a node never publishes or dials a
        // plain-http relay. Records enforce the same rule on the receiving side.
        if let Some(cfg) = self
            .relays
            .iter()
            .find(|c| c.url.scheme() != RELAY_URL_SCHEME)
        {
            return Err(Error::InsecureRelayUrl(cfg.url.to_string()));
        }
        let secret = self.secret_key.unwrap_or_else(SecretKey::generate);
        let store = Arc::new(RecordStore::new(self.store_capacity));
        let relay_map: RelayMap = self.relays.iter().cloned().collect();
        let use_relays = self.mode != TransportMode::DirectOnly && !relay_map.is_empty();
        if self.mode == TransportMode::RelayOnly && !use_relays {
            return Err(Error::Bind(
                "relay-only mode needs at least one relay".into(),
            ));
        }

        let transport = QuicTransportConfig::builder()
            .max_concurrent_bidi_streams(VarInt::from_u32(256))
            .max_concurrent_uni_streams(VarInt::from_u32(0))
            .build();
        let mut b = iroh::Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .alpns(vec![ALPN.to_vec()])
            .address_lookup(RecordLookup::new(store.clone()))
            .transport_config(transport)
            // Never map ports on the user's router (UPnP, PCP, NAT-PMP); see
            // the design document. iroh is built without its `portmapper`
            // feature, and this keeps it off if a larger build enables it.
            .portmapper_config(PortmapperConfig::Disabled);
        if let Some(tls) = self.relay_tls {
            b = b.ca_tls_config(tls);
        }
        b = if use_relays {
            b.relay_mode(RelayMode::Custom(relay_map.clone()))
        } else {
            b.relay_mode(RelayMode::Disabled)
        };
        if self.mode == TransportMode::RelayOnly {
            b = b.clear_ip_transports();
        } else if let Some(addr) = self.bind_addr {
            b = b
                .clear_ip_transports()
                .bind_addr(addr)
                .map_err(|e| Error::Bind(e.to_string()))?;
        }
        let iroh = b.bind().await.map_err(|e| Error::Bind(e.to_string()))?;

        // Give the endpoint a moment to learn its addresses, so the first
        // record is useful. Later changes are republished automatically.
        if use_relays {
            let _ = tokio::time::timeout(self.online_timeout, iroh.online()).await;
        }
        if self.mode != TransportMode::RelayOnly {
            let mut w = iroh.watch_addr();
            let _ = tokio::time::timeout(self.online_timeout, async {
                while w.get().ip_addrs().next().is_none() {
                    if w.updated().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }

        let instance_id: [u8; 16] = rand::random();
        let (incoming_tx, incoming_rx) = mpsc::channel(256);
        let (events, _) = broadcast::channel(256);

        // Sign the first record before anything else can observe the endpoint.
        let initial = build_and_sign(
            &iroh,
            &*self.signer,
            &self.identities,
            instance_id,
            self.min_seq,
            &self.config,
        )
        .await;
        let initial = match initial {
            Ok(r) => r,
            Err(e) => {
                iroh.close().await;
                return Err(e);
            }
        };
        let (record_tx, _) = watch::channel(initial.clone());

        let inner = Arc::new(Inner {
            iroh,
            signer: self.signer,
            discovery: self.discovery,
            store,
            local: tokio::sync::Mutex::new(LocalState {
                identities: self.identities,
                seq: initial.seq(),
            }),
            record_tx,
            instance_id,
            conns: Mutex::new(HashMap::new()),
            dial_locks: Mutex::new(HashMap::new()),
            connect_slots: Mutex::new(HashMap::new()),
            incoming_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            events,
            cancel: CancellationToken::new(),
            handshakes: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_HANDSHAKES)),
            relay_map,
            config: self.config,
        });
        info!(
            node_id = %initial.default_id(),
            endpoint_id = %inner.iroh.id().fmt_short(),
            "golem-p2p endpoint bound"
        );
        inner.publish(&initial).await;

        tokio::spawn(accept_loop(inner.clone()));
        tokio::spawn(address_loop(inner.clone()));
        if use_relays {
            tokio::spawn(relay_watchdog(inner.clone()));
        }
        let guard = Arc::new(ShutdownGuard {
            cancel: inner.cancel.clone(),
            iroh: inner.iroh.clone(),
        });
        Ok(Endpoint {
            inner,
            _guard: guard,
        })
    }
}

/// Maximum number of incoming handshakes in progress at once.
const MAX_PENDING_HANDSHAKES: usize = 128;

/// Minimum interval between two record updates processed from one connection
/// (accepted, stale or duplicate alike).
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

struct LocalState {
    identities: Identities,
    seq: u64,
}

/// Serializes the connection attempts to one target, see [`Inner::connect`].
#[derive(Default)]
struct ConnectSlot {
    /// When the last attempt failed, and why.
    failed: Option<(tokio::time::Instant, Error)>,
}

struct Inner {
    iroh: iroh::Endpoint,
    signer: Arc<dyn Signer>,
    discovery: Option<Arc<dyn Discovery>>,
    store: Arc<RecordStore>,
    local: tokio::sync::Mutex<LocalState>,
    record_tx: watch::Sender<VerifiedRecord>,
    instance_id: [u8; 16],
    conns: Mutex<HashMap<usize, Connection>>,
    dial_locks: Mutex<HashMap<EndpointId, Arc<tokio::sync::Mutex<()>>>>,
    connect_slots: Mutex<HashMap<NodeId, Arc<tokio::sync::Mutex<ConnectSlot>>>>,
    incoming_tx: mpsc::Sender<IncomingStream>,
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<IncomingStream>>,
    events: broadcast::Sender<Event>,
    cancel: CancellationToken,
    handshakes: Arc<tokio::sync::Semaphore>,
    relay_map: RelayMap,
    config: Config,
}

/// Our current relay URLs and the direct addresses to publish: iroh's
/// addresses without those of host-internal and excluded interfaces, best
/// first, at most [`MAX_DIRECT_ADDRS`](crate::record::MAX_DIRECT_ADDRS) (see
/// the `addrs` module).
async fn local_addrs(iroh: &iroh::Endpoint, config: &Config) -> (Vec<String>, Vec<SocketAddr>) {
    let addr = iroh.addr();
    let relays = addr
        .relay_urls()
        .take(MAX_RELAYS)
        .map(|u| u.to_string())
        .collect();
    let candidates: Vec<SocketAddr> = addr.ip_addrs().copied().collect();
    if candidates.is_empty() {
        return (relays, Vec::new());
    }
    let sel = addrs::select(
        candidates,
        &Interfaces::current().await,
        &config.exclude_interfaces,
    );
    if !sel.excluded.is_empty() {
        debug!(
            excluded = ?sel.excluded,
            "not publishing addresses of host-internal or excluded interfaces"
        );
    }
    if !sel.over_cap.is_empty() {
        debug!(dropped = ?sel.over_cap, "more direct addresses than a record holds");
    }
    (relays, sel.published)
}

/// Builds our record from the current addresses and signs it.
async fn build_and_sign(
    iroh: &iroh::Endpoint,
    signer: &dyn Signer,
    identities: &Identities,
    instance_id: [u8; 16],
    prev_seq: u64,
    config: &Config,
) -> Result<VerifiedRecord> {
    let (relay_urls, direct_addrs) = local_addrs(iroh, config).await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Time based, strictly increasing: survives restarts without persistence
    // as long as the clock does not jump back, and `min_seq` covers that case.
    let seq = (prev_seq + 1).max(now.as_micros() as u64);
    let record = NodeRecord {
        version: RECORD_VERSION,
        seq,
        expires_at: (now + config.record_ttl).as_secs(),
        instance_id,
        endpoint_id: *iroh.id().as_bytes(),
        default_id: identities.default_id(),
        identities: identities.all(),
        relay_urls,
        direct_addrs,
    };
    Ok(sign_record(&record, iroh.secret_key(), signer).await?)
}

impl Inner {
    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    fn local_record(&self) -> VerifiedRecord {
        self.record_tx.borrow().clone()
    }

    /// Re-signs our record, optionally with new identities, and distributes it.
    async fn update_record(&self, identities: Option<Identities>) -> Result<VerifiedRecord> {
        let mut st = self.local.lock().await;
        let ids = identities.unwrap_or_else(|| st.identities.clone());
        let rec = build_and_sign(
            &self.iroh,
            &*self.signer,
            &ids,
            self.instance_id,
            st.seq,
            &self.config,
        )
        .await?;
        st.identities = ids;
        st.seq = rec.seq();
        // Connected peers receive the update over each control stream.
        self.record_tx.send_replace(rec.clone());
        drop(st);
        debug!(seq = rec.seq(), ids = ?rec.identities(), "local record updated");
        self.emit(Event::LocalRecordUpdated {
            record: rec.clone(),
        });
        self.publish(&rec).await;
        Ok(rec)
    }

    async fn publish(&self, rec: &VerifiedRecord) {
        if let Some(d) = &self.discovery {
            match tokio::time::timeout(Duration::from_secs(10), d.publish(rec)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!("publishing record failed: {e}"),
                Err(_) => warn!("publishing record timed out"),
            }
        }
    }

    /// Finds the latest valid record for `to`, asking discovery first.
    async fn lookup(&self, to: NodeId) -> Result<VerifiedRecord> {
        if let Some(d) = &self.discovery {
            match tokio::time::timeout(Duration::from_secs(10), d.lookup(to)).await {
                Ok(Ok(Some(rec))) => {
                    self.store.insert(&rec);
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => warn!(%to, "discovery lookup failed: {e}"),
                Err(_) => warn!(%to, "discovery lookup timed out"),
            }
        }
        self.store.resolve(&to).ok_or(Error::NotFound(to))
    }

    /// Returns `true` if a newer instance of the record's endpoint is known,
    /// i.e. the record belongs to a process that has since been restarted.
    fn is_superseded(&self, rec: &VerifiedRecord) -> bool {
        self.store
            .by_endpoint(&rec.endpoint_id())
            .is_some_and(|latest| {
                latest.instance_id() != rec.instance_id() && latest.seq() > rec.seq()
            })
    }

    /// Returns a live connection that currently serves `to`, if any.
    fn find_connection(&self, to: &NodeId) -> Option<Connection> {
        let target = self.store.resolve(to);
        let conns = self.conns.lock().expect("poisoned");
        conns
            .values()
            .filter(|c| !c.is_closed())
            .filter(|c| {
                let peer = c.peer();
                if !peer.contains(to) {
                    return false;
                }
                // The identity moved to a different endpoint since.
                if let Some(t) = &target
                    && t.endpoint_id() != peer.endpoint_id()
                    && t.seq() > peer.seq()
                {
                    return false;
                }
                // The endpoint restarted (new instance) since.
                !self.is_superseded(&peer)
            })
            .max_by_key(|c| c.peer().seq())
            .cloned()
    }

    fn dial_lock(&self, ep: EndpointId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.dial_locks.lock().expect("poisoned");
        locks.retain(|_, l| Arc::strong_count(l) > 1);
        locks.entry(ep).or_default().clone()
    }

    fn connect_slot(&self, to: NodeId) -> Arc<tokio::sync::Mutex<ConnectSlot>> {
        let mut slots = self.connect_slots.lock().expect("poisoned");
        slots.retain(|_, s| Arc::strong_count(s) > 1);
        slots.entry(to).or_default().clone()
    }

    /// Returns a connection to `to`, looking it up and dialing if needed.
    ///
    /// Concurrent calls for the same target make one lookup and one dial:
    /// the others wait for it and then use its connection, or return its
    /// error if it failed while they waited. (Otherwise every caller looked
    /// the peer up in discovery, and for an unreachable peer the callers
    /// dialed one after another, each for a full connect timeout.)
    async fn connect(self: &Arc<Self>, to: NodeId) -> Result<Connection> {
        if self.cancel.is_cancelled() {
            return Err(Error::Closed);
        }
        if self.local_record().contains(&to) {
            return Err(Error::SelfConnect(to));
        }
        if let Some(c) = self.find_connection(&to) {
            return Ok(c);
        }
        let slot = self.connect_slot(to);
        let queued = tokio::time::Instant::now();
        let mut slot = slot.lock().await;
        if let Some(c) = self.find_connection(&to) {
            return Ok(c);
        }
        if let Some((failed_at, e)) = &slot.failed
            && *failed_at >= queued
        {
            return Err(e.clone());
        }
        let res = self.lookup_and_dial(to).await;
        slot.failed = res
            .as_ref()
            .err()
            .map(|e| (tokio::time::Instant::now(), e.clone()));
        res
    }

    async fn lookup_and_dial(self: &Arc<Self>, to: NodeId) -> Result<Connection> {
        let rec = self.lookup(to).await?;
        // Identities of one node share its endpoint: one dial per endpoint.
        let lock = self.dial_lock(rec.endpoint_id());
        let _guard = lock.lock().await;
        if let Some(c) = self.find_connection(&to) {
            return Ok(c);
        }
        self.dial(&rec, to).await
    }

    async fn dial(self: &Arc<Self>, rec: &VerifiedRecord, target: NodeId) -> Result<Connection> {
        let mut local_rx = self.record_tx.subscribe();
        let local = local_rx.borrow_and_update().clone();
        let conn = tokio::time::timeout(
            self.config.connect_timeout,
            self.iroh.connect(rec.endpoint_addr(), ALPN),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| Error::Connect(e.to_string()))?;
        if conn.remote_id() != rec.endpoint_id() {
            conn.close(codes::IDENTITY_MISMATCH, b"unexpected endpoint");
            return Err(Error::IdentityMismatch);
        }

        let handshake = async {
            let (mut send, mut recv) = conn
                .open_bi()
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;
            send.write_all(&[KIND_CONTROL])
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;
            wire::write_frame(
                &mut send,
                &Control::Hello {
                    record: local.to_bytes().to_vec(),
                    target: Some(target),
                },
            )
            .await?;
            let reply: Control = wire::read_frame(&mut recv, MAX_CONTROL_FRAME).await?;
            Ok::<_, Error>((send, recv, reply))
        };
        let (send, recv, reply) =
            match tokio::time::timeout(self.config.handshake_timeout, handshake).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    conn.close(codes::HANDSHAKE_FAILED, b"handshake failed");
                    return Err(e);
                }
                Err(_) => {
                    conn.close(codes::HANDSHAKE_FAILED, b"handshake timeout");
                    return Err(Error::Timeout);
                }
            };
        let peer = match reply {
            Control::Welcome { record } => match VerifiedRecord::verify(&record) {
                Ok(p) => p,
                Err(e) => {
                    conn.close(codes::HANDSHAKE_FAILED, b"invalid record");
                    return Err(e.into());
                }
            },
            Control::Reject { reason } => {
                conn.close(codes::OK, b"rejected");
                return Err(Error::Rejected(reason));
            }
            _ => {
                conn.close(codes::PROTOCOL, b"unexpected message");
                return Err(Error::Handshake("unexpected message".into()));
            }
        };
        // The identities of a connection come only from a verified record
        // whose endpoint ID equals the TLS-authenticated remote endpoint.
        if peer.endpoint_id() != conn.remote_id() {
            conn.close(codes::IDENTITY_MISMATCH, b"record endpoint mismatch");
            return Err(Error::IdentityMismatch);
        }
        if !peer.contains(&target) {
            conn.close(codes::UNKNOWN_IDENTITY, b"target not served");
            return Err(Error::NotPeerIdentity(target));
        }
        if self.is_superseded(&peer) {
            conn.close(codes::SUPERSEDED, b"stale instance");
            return Err(Error::Handshake(
                "peer answered as a superseded instance".into(),
            ));
        }
        self.store.insert(&peer);
        Ok(self.register(conn, peer, send, recv, local_rx, Direction::Outgoing))
    }

    async fn handle_incoming(self: Arc<Self>, incoming: iroh::endpoint::Incoming) {
        let open = self.conns.lock().expect("poisoned").len();
        let Ok(_permit) = self.handshakes.clone().try_acquire_owned() else {
            incoming.refuse();
            return;
        };
        if open >= self.config.max_connections {
            incoming.refuse();
            return;
        }
        let res = tokio::time::timeout(self.config.handshake_timeout, async {
            let conn = incoming
                .accept()
                .map_err(|e| Error::Connection(e.to_string()))?
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;
            let (send, mut recv) = match conn.accept_bi().await {
                Ok(s) => s,
                Err(e) => return Err(Error::Connection(e.to_string())),
            };
            if wire::read_kind(&mut recv).await? != KIND_CONTROL {
                conn.close(codes::PROTOCOL, b"expected control stream");
                return Err(Error::Handshake("expected control stream".into()));
            }
            let hello: Control = wire::read_frame(&mut recv, MAX_CONTROL_FRAME).await?;
            Ok((conn, send, recv, hello))
        })
        .await;
        let (conn, mut send, recv, hello) = match res {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                debug!("incoming handshake failed: {e}");
                return;
            }
            Err(_) => {
                debug!("incoming handshake timed out");
                return;
            }
        };
        let Control::Hello { record, target } = hello else {
            conn.close(codes::PROTOCOL, b"expected hello");
            return;
        };
        let peer = match VerifiedRecord::verify(&record) {
            Ok(p) if p.endpoint_id() == conn.remote_id() => p,
            Ok(_) => {
                reject(
                    &conn,
                    &mut send,
                    RejectReason::InvalidRecord,
                    codes::IDENTITY_MISMATCH,
                )
                .await;
                return;
            }
            Err(e) => {
                debug!("rejecting connection with invalid record: {e}");
                reject(
                    &conn,
                    &mut send,
                    RejectReason::InvalidRecord,
                    codes::HANDSHAKE_FAILED,
                )
                .await;
                return;
            }
        };
        let mut local_rx = self.record_tx.subscribe();
        let local = local_rx.borrow_and_update().clone();
        if let Some(t) = target
            && !local.contains(&t)
        {
            reject(
                &conn,
                &mut send,
                RejectReason::UnknownIdentity,
                codes::UNKNOWN_IDENTITY,
            )
            .await;
            return;
        }
        if self.is_superseded(&peer) {
            debug!("rejecting connection from a superseded instance");
            reject(
                &conn,
                &mut send,
                RejectReason::InvalidRecord,
                codes::SUPERSEDED,
            )
            .await;
            return;
        }
        self.store.insert(&peer);
        if let Err(e) = wire::write_frame(
            &mut send,
            &Control::Welcome {
                record: local.to_bytes().to_vec(),
            },
        )
        .await
        {
            debug!("writing welcome failed: {e}");
            conn.close(codes::HANDSHAKE_FAILED, b"handshake failed");
            return;
        }
        self.register(conn, peer, send, recv, local_rx, Direction::Incoming);
    }

    /// Registers a connection that completed the handshake and starts its tasks.
    fn register(
        self: &Arc<Self>,
        conn: IrohConnection,
        peer: VerifiedRecord,
        control_send: SendStream,
        control_recv: RecvStream,
        local_rx: watch::Receiver<VerifiedRecord>,
        direction: Direction,
    ) -> Connection {
        // The store may already hold a newer record from the same endpoint.
        let peer = match self.store.by_endpoint(&peer.endpoint_id()) {
            Some(latest)
                if latest.seq() > peer.seq() && latest.instance_id() == peer.instance_id() =>
            {
                latest
            }
            _ => peer,
        };
        let (peer_tx, peer_rx) = watch::channel(peer.clone());
        let c = Connection {
            inner: Arc::new(ConnInner {
                conn,
                peer: peer_rx,
                local: local_rx.clone(),
                direction,
            }),
        };
        {
            let mut conns = self.conns.lock().expect("poisoned");
            // A restart must never be confused with the previous process: an
            // older instance of the same endpoint is gone, drop its connections.
            for other in conns.values() {
                let op = other.peer();
                if op.endpoint_id() == peer.endpoint_id()
                    && op.instance_id() != peer.instance_id()
                    && op.seq() < peer.seq()
                {
                    other
                        .inner
                        .conn
                        .close(codes::SUPERSEDED, b"superseded by new instance");
                }
            }
            conns.insert(c.stable_id(), c.clone());
        }
        debug!(peer = %peer.default_id(), ?direction, "connection established");
        self.emit(Event::Connected {
            peer: peer.clone(),
            direction,
        });

        tokio::spawn(control_writer(
            self.clone(),
            c.clone(),
            control_send,
            local_rx,
        ));
        tokio::spawn(control_reader(
            self.clone(),
            c.clone(),
            control_recv,
            peer_tx,
        ));
        tokio::spawn(stream_acceptor(self.clone(), c.clone()));
        tokio::spawn(close_watcher(self.clone(), c.clone()));
        c
    }

    /// Opens an application stream to `to` and tries once more with a freshly
    /// resolved connection if the cached connection turns out to be unusable
    /// before anything was sent.
    async fn open_bi(
        self: &Arc<Self>,
        from: NodeId,
        to: NodeId,
    ) -> Result<(Connection, SendStream, RecvStream)> {
        if !self.local_record().contains(&from) {
            return Err(Error::NotLocalIdentity(from));
        }
        let conn = self.connect(to).await?;
        match conn.open_bi(from, to).await {
            Ok((s, r)) => Ok((conn, s, r)),
            Err(Error::Connection(e)) => {
                debug!("stream open failed on cached connection, redialing: {e}");
                conn.close();
                let conn = self.connect(to).await?;
                let (s, r) = conn.open_bi(from, to).await?;
                Ok((conn, s, r))
            }
            Err(e) => Err(e),
        }
    }
}

async fn reject(conn: &IrohConnection, send: &mut SendStream, reason: RejectReason, code: VarInt) {
    let _ = wire::write_frame(send, &Control::Reject { reason }).await;
    let _ = send.finish();
    // Give the dialer a moment to read the reason before closing.
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
    conn.close(code, b"rejected");
}

async fn accept_loop(inner: Arc<Inner>) {
    loop {
        let incoming = tokio::select! {
            _ = inner.cancel.cancelled() => break,
            i = inner.iroh.accept() => i,
        };
        let Some(incoming) = incoming else { break };
        tokio::spawn(inner.clone().handle_incoming(incoming));
    }
}

/// Re-signs and republishes our record when our addresses change, and
/// periodically before it expires.
async fn address_loop(inner: Arc<Inner>) {
    let mut w = inner.iroh.watch_addr();
    let refresh = inner.config.record_ttl / 3;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + refresh, refresh);
    loop {
        tokio::select! {
            _ = inner.cancel.cancelled() => break,
            r = w.updated() => {
                if r.is_err() {
                    break;
                }
                // Debounce bursts of address changes.
                tokio::time::sleep(Duration::from_millis(250)).await;
                // Compare what we would publish now, so that changes of
                // addresses we do not publish (a new Docker network) do not
                // re-sign the record.
                let (relays, ips) = local_addrs(&inner.iroh, &inner.config).await;
                let rec = inner.local_record();
                let relays: BTreeSet<String> = relays.into_iter().collect();
                let rec_relays: BTreeSet<String> = rec.record().relay_urls.iter().cloned().collect();
                if relays != rec_relays || ips != rec.record().direct_addrs {
                    debug!(?relays, ?ips, "addresses changed, republishing");
                    if let Err(e) = inner.update_record(None).await {
                        warn!("re-signing record after address change failed: {e}");
                    }
                }
            }
            _ = tick.tick() => {
                if let Err(e) = inner.update_record(None).await {
                    warn!("periodic record refresh failed: {e}");
                }
            }
        }
    }
}

/// Speeds up failover when the home relay goes down.
///
/// iroh re-evaluates the preferred relay on its periodic net report (every
/// 20-26 s). When no home relay is connected for a few seconds, we re-insert
/// the current relay configuration unchanged, which makes iroh run a net
/// report right away and pick a relay that is up.
async fn relay_watchdog(inner: Arc<Inner>) {
    let mut status = inner.iroh.home_relay_status();
    let mut down_since: Option<tokio::time::Instant> = None;
    let mut last_kick = tokio::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = inner.cancel.cancelled() => break,
            _ = tick.tick() => {}
        }
        let current = status.get();
        let connected = current.iter().any(|s| s.is_connected());
        if connected {
            down_since = None;
            continue;
        }
        let now = tokio::time::Instant::now();
        let since = *down_since.get_or_insert(now);
        if now - since >= Duration::from_secs(3) && now - last_kick >= Duration::from_secs(10) {
            last_kick = now;
            let url = current
                .first()
                .map(|s| s.url().clone())
                .or_else(|| inner.relay_map.urls::<Vec<_>>().into_iter().next());
            if let Some(url) = url
                && let Some(cfg) = inner.relay_map.get(&url)
            {
                info!(%url, "home relay unreachable, re-evaluating relays");
                inner.iroh.insert_relay(url, cfg).await;
            }
        }
    }
}

/// Sends our record to the peer whenever it changes.
async fn control_writer(
    inner: Arc<Inner>,
    c: Connection,
    mut send: SendStream,
    mut local_rx: watch::Receiver<VerifiedRecord>,
) {
    loop {
        tokio::select! {
            _ = inner.cancel.cancelled() => break,
            _ = c.inner.conn.closed() => break,
            r = local_rx.changed() => {
                if r.is_err() {
                    break;
                }
                let rec = local_rx.borrow_and_update().clone();
                let msg = Control::Update { record: rec.to_bytes().to_vec() };
                if let Err(e) = wire::write_frame(&mut send, &msg).await {
                    debug!("sending record update failed: {e}");
                    break;
                }
            }
        }
    }
}

/// Receives record updates from the peer.
async fn control_reader(
    inner: Arc<Inner>,
    c: Connection,
    mut recv: RecvStream,
    peer_tx: watch::Sender<VerifiedRecord>,
) {
    let mut next_allowed = tokio::time::Instant::now();
    loop {
        // Every update costs one signature verification per listed identity,
        // also one that turns out stale or a duplicate, so every update is
        // paced, not only accepted ones. QUIC flow control turns the pacing
        // into backpressure on a chatty peer.
        tokio::select! {
            _ = inner.cancel.cancelled() => break,
            _ = tokio::time::sleep_until(next_allowed) => {}
        }
        let msg = tokio::select! {
            _ = inner.cancel.cancelled() => break,
            m = wire::read_frame::<Control>(&mut recv, MAX_CONTROL_FRAME) => m,
        };
        next_allowed = tokio::time::Instant::now() + UPDATE_INTERVAL;
        let record = match msg {
            Ok(Control::Update { record }) => record,
            Ok(_) => {
                c.inner
                    .conn
                    .close(codes::PROTOCOL, b"unexpected control message");
                break;
            }
            Err(e) => {
                debug!("control stream ended: {e}");
                break;
            }
        };
        let current = peer_tx.borrow().clone();
        let new = match VerifiedRecord::verify(&record) {
            Ok(r)
                if r.endpoint_id() == c.inner.conn.remote_id()
                    && r.instance_id() == current.instance_id() =>
            {
                r
            }
            Ok(_) => {
                c.inner
                    .conn
                    .close(codes::IDENTITY_MISMATCH, b"update for another endpoint");
                break;
            }
            Err(e) => {
                // The peer is authenticated, so an invalid record is a
                // protocol violation, not noise.
                debug!("invalid record update, closing connection: {e}");
                c.inner
                    .conn
                    .close(codes::PROTOCOL, b"invalid record update");
                break;
            }
        };
        if new.seq() <= current.seq() {
            continue;
        }
        inner.store.insert(&new);
        peer_tx.send_replace(new.clone());
        debug!(peer = %new.default_id(), ids = ?new.identities(), "peer record updated");
        inner.emit(Event::PeerUpdated {
            previous: current,
            current: new,
        });
    }
}

/// Accepts application streams opened by the peer.
async fn stream_acceptor(inner: Arc<Inner>, c: Connection) {
    loop {
        let res = tokio::select! {
            _ = inner.cancel.cancelled() => break,
            r = c.inner.conn.accept_bi() => r,
        };
        let Ok((send, recv)) = res else { break };
        tokio::spawn(handle_app_stream(inner.clone(), c.clone(), send, recv));
    }
}

async fn handle_app_stream(
    inner: Arc<Inner>,
    c: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
) {
    let header = tokio::time::timeout(inner.config.handshake_timeout, async {
        if wire::read_kind(&mut recv).await? != KIND_APP {
            return Err(Error::Handshake("unexpected stream kind".into()));
        }
        Ok(wire::read_frame::<AppHeader>(&mut recv, MAX_HEADER_FRAME).await?)
    })
    .await;
    let header = match header {
        Ok(Ok(h)) => h,
        _ => {
            let _ = send.reset(codes::PROTOCOL);
            let _ = recv.stop(codes::PROTOCOL);
            return;
        }
    };
    // `to` must be ours *now*, `from` must be bound to this connection *now*.
    let peer = c.peer();
    let code = if !inner.local_record().contains(&header.to) {
        Some(codes::UNKNOWN_IDENTITY)
    } else if !peer.contains(&header.from) {
        Some(codes::SENDER_NOT_BOUND)
    } else {
        None
    };
    if let Some(code) = code {
        debug!(from = %header.from, to = %header.to, "rejecting application stream");
        let _ = send.reset(code);
        let _ = recv.stop(code);
        return;
    }
    let incoming = IncomingStream {
        from: header.from,
        to: header.to,
        peer,
        connection: c,
        send,
        recv,
    };
    if let Err(e) = inner.incoming_tx.try_send(incoming) {
        let mut incoming = e.into_inner();
        let _ = incoming.send.reset(codes::BUSY);
        let _ = incoming.recv.stop(codes::BUSY);
    }
}

async fn close_watcher(inner: Arc<Inner>, c: Connection) {
    let reason = c.inner.conn.closed().await;
    inner.conns.lock().expect("poisoned").remove(&c.stable_id());
    inner.emit(Event::Disconnected {
        peer: c.peer(),
        reason: reason.to_string(),
    });
}

/// A golem-p2p endpoint. Cheap to clone.
///
/// Call [`Endpoint::close`] for a graceful shutdown. If the last clone is
/// dropped without it, background tasks are stopped and the iroh endpoint is
/// closed in the background.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<Inner>,
    _guard: Arc<ShutdownGuard>,
}

/// Stops background tasks when the last [`Endpoint`] handle is dropped.
/// (The tasks themselves hold `Inner`, not the handle.)
struct ShutdownGuard {
    cancel: CancellationToken,
    iroh: iroh::Endpoint,
}

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        if self.cancel.is_cancelled() {
            return;
        }
        self.cancel.cancel();
        let iroh = self.iroh.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move { iroh.close().await });
        }
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("node_id", &self.node_id())
            .field("endpoint_id", &self.endpoint_id().fmt_short().to_string())
            .finish()
    }
}

impl Endpoint {
    /// Starts building an endpoint that signs with `signer` for `identities`.
    pub fn builder(signer: impl Signer, identities: Identities) -> Builder {
        Builder::new(Arc::new(signer), identities)
    }

    /// Returns the default identity.
    pub fn node_id(&self) -> NodeId {
        self.inner.local_record().default_id()
    }

    /// Returns all current identities, default first.
    pub fn identities(&self) -> Vec<NodeId> {
        self.inner.local_record().identities().to_vec()
    }

    /// Returns the iroh endpoint ID (the transport key of this process).
    pub fn endpoint_id(&self) -> EndpointId {
        self.inner.iroh.id()
    }

    /// Returns this process' instance ID, which is new on every start.
    pub fn instance_id(&self) -> [u8; 16] {
        self.inner.instance_id
    }

    /// Returns our current signed record.
    pub fn local_record(&self) -> VerifiedRecord {
        self.inner.local_record()
    }

    /// Returns the underlying iroh endpoint.
    pub fn iroh(&self) -> &iroh::Endpoint {
        &self.inner.iroh
    }

    /// Returns the peer record cache.
    pub fn record_store(&self) -> &Arc<RecordStore> {
        &self.inner.store
    }

    /// Subscribes to endpoint events.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Returns all open connections.
    pub fn connections(&self) -> Vec<Connection> {
        let conns = self.inner.conns.lock().expect("poisoned");
        conns.values().filter(|c| !c.is_closed()).cloned().collect()
    }

    /// Returns a connection to the node serving `to`, dialing if needed.
    pub async fn connect(&self, to: NodeId) -> Result<Connection> {
        self.inner.connect(to).await
    }

    /// Opens a bidirectional stream from our default identity to `to`.
    pub async fn open_bi(&self, to: NodeId) -> Result<(SendStream, RecvStream)> {
        let from = self.node_id();
        self.open_bi_from(from, to).await
    }

    /// Opens a bidirectional stream from one of our identities to `to`.
    pub async fn open_bi_from(&self, from: NodeId, to: NodeId) -> Result<(SendStream, RecvStream)> {
        let (_, s, r) = self.inner.open_bi(from, to).await?;
        Ok((s, r))
    }

    /// Sends `data` from our default identity to `to` and returns the response.
    pub async fn request(&self, to: NodeId, data: &[u8]) -> Result<Vec<u8>> {
        let from = self.node_id();
        self.request_from(from, to, data).await
    }

    /// Sends `data` from `from` (one of our identities) to `to` and returns the
    /// response.
    ///
    /// Not retried once the request may have been delivered. If the request
    /// fails on a connection-level error or timeout, that connection is closed
    /// so the next call dials a fresh one with a freshly looked-up record.
    pub async fn request_from(&self, from: NodeId, to: NodeId, data: &[u8]) -> Result<Vec<u8>> {
        let (conn, mut send, mut recv) = self.inner.open_bi(from, to).await?;
        let max = self.inner.config.max_response_size;
        let exchange = async {
            send.write_all(data).await.map_err(map_write_err)?;
            send.finish()
                .map_err(|e| Error::Connection(e.to_string()))?;
            recv.read_to_end(max).await.map_err(map_read_err)
        };
        match tokio::time::timeout(self.inner.config.request_timeout, exchange).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                if matches!(e, Error::Connection(_)) {
                    conn.close();
                }
                Err(e)
            }
            Err(_) => {
                conn.close();
                Err(Error::Timeout)
            }
        }
    }

    /// Waits for the next incoming application stream. Returns `None` once
    /// the endpoint is closed.
    pub async fn accept(&self) -> Option<IncomingStream> {
        let mut rx = self.inner.incoming_rx.lock().await;
        tokio::select! {
            _ = self.inner.cancel.cancelled() => None,
            s = rx.recv() => s,
        }
    }

    /// Replaces all identities. Signs and publishes a new record and pushes it
    /// to all connected peers. Fails (and changes nothing) if any identity
    /// cannot sign, for example because its key is locked.
    pub async fn set_identities(&self, identities: Identities) -> Result<VerifiedRecord> {
        if identities.all().len() > crate::record::MAX_IDENTITIES {
            return Err(Error::InvalidIdentities("too many identities"));
        }
        self.inner.update_record(Some(identities)).await
    }

    /// Makes `id` the default identity. The previous default stays as an alias.
    pub async fn set_default_identity(&self, id: NodeId) -> Result<VerifiedRecord> {
        let cur = self.current_identities().await;
        let ids = Identities::with_aliases(id, cur.all());
        self.set_identities(ids).await
    }

    /// Adds an alias at runtime; it becomes reachable without a restart.
    pub async fn add_alias(&self, id: NodeId) -> Result<VerifiedRecord> {
        let cur = self.current_identities().await;
        let ids =
            Identities::with_aliases(cur.default_id(), cur.aliases().iter().copied().chain([id]));
        self.set_identities(ids).await
    }

    /// Removes an alias. The default identity cannot be removed.
    pub async fn remove_alias(&self, id: NodeId) -> Result<VerifiedRecord> {
        let cur = self.current_identities().await;
        if cur.default_id() == id {
            return Err(Error::InvalidIdentities(
                "cannot remove the default identity",
            ));
        }
        let ids = Identities::with_aliases(
            cur.default_id(),
            cur.aliases().iter().copied().filter(|a| *a != id),
        );
        self.set_identities(ids).await
    }

    /// Re-signs and republishes our record now.
    pub async fn republish(&self) -> Result<VerifiedRecord> {
        self.inner.update_record(None).await
    }

    async fn current_identities(&self) -> Identities {
        self.inner.local.lock().await.identities.clone()
    }

    /// Closes all connections and the endpoint.
    pub async fn close(&self) {
        self.inner.cancel.cancel();
        let conns: Vec<_> = self.inner.conns.lock().expect("poisoned").drain().collect();
        for (_, c) in conns {
            c.inner.conn.close(codes::SHUTDOWN, b"endpoint closed");
        }
        self.inner.iroh.close().await;
    }
}

fn map_write_err(e: iroh::endpoint::WriteError) -> Error {
    use iroh::endpoint::WriteError;
    match e {
        WriteError::Stopped(code) => Error::StreamRejected(code.into_inner()),
        other => Error::Connection(other.to_string()),
    }
}

fn map_read_err(e: iroh::endpoint::ReadToEndError) -> Error {
    use iroh::endpoint::{ReadError, ReadToEndError};
    match e {
        ReadToEndError::TooLong => Error::TooLarge,
        ReadToEndError::Read(ReadError::Reset(code)) => Error::StreamRejected(code.into_inner()),
        ReadToEndError::Read(other) => Error::Connection(other.to_string()),
    }
}

struct ConnInner {
    conn: IrohConnection,
    peer: watch::Receiver<VerifiedRecord>,
    local: watch::Receiver<VerifiedRecord>,
    direction: Direction,
}

/// An authenticated connection to a peer, with the peer's verified identities.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<ConnInner>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("peer", &self.peer().default_id())
            .field("remote", &self.remote_endpoint_id().fmt_short().to_string())
            .field("direction", &self.inner.direction)
            .finish()
    }
}

impl Connection {
    /// Returns the peer's current verified record (its identities, default
    /// first). It is updated when the peer publishes a newer record.
    pub fn peer(&self) -> VerifiedRecord {
        self.inner.peer.borrow().clone()
    }

    /// Returns the TLS-authenticated iroh endpoint ID of the peer.
    pub fn remote_endpoint_id(&self) -> EndpointId {
        self.inner.conn.remote_id()
    }

    /// Returns who dialed.
    pub fn direction(&self) -> Direction {
        self.inner.direction
    }

    /// Returns a process-local ID of this connection.
    pub fn stable_id(&self) -> usize {
        self.inner.conn.stable_id()
    }

    /// Returns the kind of the currently selected network path.
    pub fn path(&self) -> PathKind {
        let paths = self.inner.conn.paths();
        let selected = paths.iter().find(|p| p.is_selected());
        match selected {
            Some(p) if p.is_relay() => PathKind::Relay,
            Some(p) if p.is_ip() => PathKind::Direct,
            _ => PathKind::Unknown,
        }
    }

    /// Returns the underlying iroh connection.
    pub fn iroh_connection(&self) -> &IrohConnection {
        &self.inner.conn
    }

    /// Returns `true` once the connection is closed.
    pub fn is_closed(&self) -> bool {
        self.inner.conn.close_reason().is_some()
    }

    /// Closes the connection.
    pub fn close(&self) {
        self.inner.conn.close(codes::OK, b"closed");
    }

    /// Waits until the connection is closed.
    pub async fn closed(&self) {
        self.inner.conn.closed().await;
    }

    /// Opens an application stream from local identity `from` to the peer's
    /// identity `to`.
    pub async fn open_bi(&self, from: NodeId, to: NodeId) -> Result<(SendStream, RecvStream)> {
        if !self.inner.local.borrow().contains(&from) {
            return Err(Error::NotLocalIdentity(from));
        }
        if !self.peer().contains(&to) {
            return Err(Error::NotPeerIdentity(to));
        }
        let (mut send, recv) = self
            .inner
            .conn
            .open_bi()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        send.write_all(&[KIND_APP]).await.map_err(map_write_err)?;
        wire::write_frame(&mut send, &AppHeader { from, to }).await?;
        Ok((send, recv))
    }
}

/// An application stream opened by a peer, with verified sender and receiver.
pub struct IncomingStream {
    from: NodeId,
    to: NodeId,
    peer: VerifiedRecord,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl fmt::Debug for IncomingStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingStream")
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

impl IncomingStream {
    /// The sending identity. It is listed in the peer's verified record, which
    /// is bound to the TLS-authenticated connection.
    pub fn from(&self) -> NodeId {
        self.from
    }

    /// Which of our identities the stream is addressed to.
    pub fn to(&self) -> NodeId {
        self.to
    }

    /// The peer's verified record when the stream was accepted.
    pub fn peer(&self) -> &VerifiedRecord {
        &self.peer
    }

    /// The connection the stream arrived on.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Mutable access to the raw streams.
    pub fn streams(&mut self) -> (&mut SendStream, &mut RecvStream) {
        (&mut self.send, &mut self.recv)
    }

    /// Takes the raw streams.
    pub fn into_streams(self) -> (SendStream, RecvStream) {
        (self.send, self.recv)
    }

    /// Reads the request body (until the peer finishes its side).
    pub async fn read_request(&mut self, max: usize) -> Result<Vec<u8>> {
        self.recv.read_to_end(max).await.map_err(map_read_err)
    }

    /// Writes the response and finishes the stream.
    pub async fn respond(mut self, data: &[u8]) -> Result<()> {
        self.send.write_all(data).await.map_err(map_write_err)?;
        self.send
            .finish()
            .map_err(|e| Error::Connection(e.to_string()))?;
        // Wait until the peer has read everything, so dropping does not race
        // with delivery.
        let _ = tokio::time::timeout(Duration::from_secs(10), self.send.stopped()).await;
        Ok(())
    }
}
