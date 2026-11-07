# golem-p2p design

Status: seed implementation, not used by yagna yet. This document describes
what is implemented in this repository, why, and what is still open.

## 1. Goals

golem-p2p is meant to replace the relay-based networking of yagna (`ya-relay`)
with peer-to-peer connections built on [iroh](https://iroh.computer).

Requirements:

1. iroh (1.4.1) is the transport: QUIC, hole punching, relays.
2. Node identities are the same as in yagna. An identity is a secp256k1 key,
   and the node ID is its 20-byte Ethereum address. A node has one default
   identity and may have aliases (secondary identities).
3. Several relays. A node homed on one relay must be reachable from a node
   homed on another.
4. Direct connections with hole punching where possible, relayed otherwise.

Non-goals for this seed: GSB integration, a production directory deployment,
NAT-specific tuning.

## 2. Architecture

```
            yagna (later)                                   other node
  ┌─────────────────────────────────┐
  │ identity service ── Signer ─────┼─┐
  │ net service ────── Endpoint API │ │
  └─────────────────────────────────┘ │
               │                      │ signs record digests
  ┌────────────▼───────────────────────▼──────────────┐
  │ golem_p2p::Endpoint                                │
  │  - local signed NodeRecord (re-signed on change)   │
  │  - RecordStore (verified peer records, bounded)    │
  │  - Discovery (directory / memory / multi)          │
  │  - connection pool, control + application streams  │
  └────────────┬───────────────────────────────────────┘
               │ ALPN "golem-p2p/1"
  ┌────────────▼──────────────┐      ┌────────┐      ┌──────────────┐
  │ iroh::Endpoint            │◄────►│ relay 1│◄────►│ iroh endpoint│
  │  QUIC + TLS (ed25519 key) │◄────►│ relay 2│      └──────────────┘
  │  hole punching, multipath │◄─────────direct UDP──────────►
  └───────────────────────────┘
```

Crate layout (`crates/golem-p2p/src`):

| module | purpose |
|---|---|
| `identity` | `NodeId` (Ethereum address), 65-byte yagna signature format, recovery |
| `signer` | `Signer` trait (identity service signs on request), `InMemorySigner` |
| `record` | `NodeRecord`, signing and verification, limits |
| `addrs` | which direct addresses go into our record: filter, rank, cap (section 5) |
| `store` | `RecordStore`: bounded store of verified records with sequence rules |
| `discovery` | `Discovery` trait, `MemoryDiscovery`, `MultiDiscovery`, `HttpDirectory` client |
| `directory` | HTTP directory server (feature `directory-server`) |
| `lookup` | `RecordLookup`, an iroh `AddressLookup` backed by verified records |
| `wire` | wire protocol: ALPN, frames, control messages, error codes |
| `endpoint` | `Endpoint`, `Builder`, `Connection`, `IncomingStream`, `Event` |

`crates/golem-p2p-cli` is a small CLI (`golem-p2p`) for trying it between two
machines. It is also the per-machine client of the network test harness:
`probe` measures reachability, path (direct or relay), round-trip times and
throughput, and `--json` prints machine-readable events
([cli-events.md](cli-events.md)). `bench` is the diagnostic for users: it
checks the local network (relays, directories, and iroh's net report: UDP,
public address, NAT) and benchmarks the connection to a `listen` node, then
explains the result in plain words. It uses only the public library API (and
iroh's `unstable-net-report` API, with iroh pinned to an exact version); there
is no test-only mode in the library.

## 3. Identities

* **Node ID.** `keccak256(uncompressed_public_key[1..])[12..]`, the Ethereum
  address. Same as `ya_client_model::NodeId` and `ethsign::PublicKey::address`.
  A unit test checks this against the ethsign test vector.
* **Signature format.** 65 bytes `v || r || s` with `v` in {0, 1}, over a
  32-byte prehash, without the Ethereum message prefix. This is what yagna's
  identity service returns for `identity::Sign` (`IdentityKey::sign`) and what
  ya-relay uses in its challenge responses. A unit test reproduces an ethsign
  signature byte for byte (both use RFC 6979 nonces). `v` = 27/28 is accepted
  on verification, high-`s` signatures are rejected.
* **Signing goes through a trait.** In yagna, keys live in the identity
  service, can be locked, and are used by RPC. `Signer::sign(node_id, digest)`
  mirrors `identity::Sign { node_id, payload }`. The library never sees a
  secret key. `InMemorySigner` is provided for tests and the CLI, and can lock
  and unlock keys to simulate the identity service.
* **Locked keys.** A record must be signed by every identity it lists. If any
  of them cannot sign (locked, unknown, RPC error), the update fails and the
  previously published record stays in force. `bind` fails if the initial
  identities cannot sign.

## 4. Encryption and identity binding

### Policy

All traffic between golem-p2p nodes is encrypted, always, in one way: iroh's
QUIC with TLS 1.3. There is no option to turn it off, no plaintext mode, and
no second encryption layer on top. This holds for direct and relayed
connections alike.

What this gives (iroh 1.4 with the `tls-ring` crypto provider):

* **Key exchange.** Every connection runs a TLS 1.3 handshake with ephemeral
  ECDHE (X25519). Traffic keys come from that exchange; the long-term endpoint
  key only signs the handshake. Recorded traffic therefore stays confidential
  if the endpoint key leaks later (forward secrecy). golem-p2p does not send
  0-RTT data.
* **Encryption.** QUIC packet protection with a TLS 1.3 AEAD cipher suite
  (AES-GCM or ChaCha20-Poly1305), plus header protection.
* **Authentication.** Both sides authenticate with their ed25519 endpoint key
  as a TLS raw public key (RFC 7250). The public key is the `EndpointId`; no
  certificates or CAs are involved. A dialer connects to a specific
  `EndpointId`, and the handshake fails if the other side does not hold that
  key.
* **ALPN.** An endpoint offers and accepts only `golem-p2p/1`. A peer that
  offers anything else fails the TLS handshake (`no_application_protocol`).
* **What relays see.** Relays forward QUIC packets they cannot decrypt. They
  see source and destination endpoint IDs, IP addresses, packet sizes and
  timing; never payloads, records or Ethereum identities. The connection to a
  relay is itself TLS (https only, section 9), so an observer on the network
  path to a relay sees less than the relay does.

`tests/encryption.rs` checks that other ALPNs are refused, and that a relay
sees the endpoint IDs it routes by, but neither a plaintext marker sent between
two nodes nor their Ethereum identities. The relay in that test runs plain
HTTP behind a TLS-terminating proxy that records the decrypted relay traffic,
i.e. exactly what the relay server handles.

### Why one mode, and no opt-out

* **One test matrix.** Every feature (handshake, identity changes, relays,
  failover, restarts) is implemented and tested once. A second mode would
  double the matrix or leave one variant untested.
* **No downgrade.** There is nothing to negotiate, so neither an attacker nor
  a configuration mistake can push a connection to a weaker mode.
* **Adding is cheap, removing is not.** If an unencrypted transport is ever
  needed, it can be added later as a separate ALPN without touching this one.
  Removing a mode once something depends on it breaks those dependents.
* **QUIC requires TLS.** QUIC (RFC 9001) has no unencrypted variant. A
  plaintext mode would mean forking iroh and its QUIC stack (noq), not
  changing a setting.
* **Debugging does not need plaintext.** iroh can write the TLS session
  secrets to the file named by the `SSLKEYLOGFILE` environment variable
  (`iroh::endpoint::Builder::keylog`), and Wireshark can decrypt captured
  traffic with that file. golem-p2p's `Builder` does not expose this switch
  yet.

### What ya-relay does today

ya-relay creates a fresh secp256k1 session key per session, each Ethereum
identity signs a statement about that session key, and traffic is encrypted
with a key derived from the session keys (AES-GCM-SIV). The Ethereum keys never
encrypt traffic directly; they *authorize* a session key, and the session key
does the cryptography.

### How Ethereum identities are bound

iroh authenticates every connection with the endpoint's own ed25519 key. That
key cannot be replaced by a secp256k1 key without forking iroh and its QUIC
stack. So the iroh endpoint key plays the role of ya-relay's session key:

* Transport encryption and authentication are iroh's QUIC/TLS. The remote
  `EndpointId` of a connection is authenticated by TLS.
* The node's Ethereum identities sign a **node record** that binds them to the
  `EndpointId` (section 5). The endpoint key signs the same record.
* A peer's Ethereum identities are taken only from a verified record whose
  endpoint ID equals the TLS-authenticated remote endpoint ID.

As in ya-relay, the Ethereum keys authorize the key that encrypts; they do not
encrypt themselves. Only the holder of the listed Ethereum keys can make a node
reachable under those identities, and all traffic is end-to-end encrypted.

There is no second encryption layer. All payloads already travel inside an
end-to-end QUIC/TLS session between the two endpoints, also when relayed. A
second layer would add code and failure modes without protecting against
anything TLS does not already cover.

### Alternatives considered

* **Literal "Ethereum encryption"** (ECDH between secp256k1 identity keys and
  an AEAD layer, like ya-relay). Redundant on top of TLS, and it would require
  every identity key to be usable for ECDH at runtime, which the identity
  service does not offer today (it only signs).
* **Deriving the iroh key from the Ethereum key.** Would tie the transport key
  to one identity, which does not fit several identities per node, default
  switching, or locked keys (the identity service would have to export
  secrets). Rejected.
* **Replacing iroh's TLS with secp256k1.** Not possible without forking iroh
  and noq.
* **An opt-out or plaintext mode.** Rejected for the reasons above. Key
  logging covers debugging.

## 5. The node record

```rust
struct NodeRecord {
    version: u8,              // 1
    seq: u64,                 // higher replaces lower
    expires_at: u64,          // unix seconds
    instance_id: [u8; 16],    // random, new on every process start
    endpoint_id: [u8; 32],    // iroh ed25519 public key
    default_id: NodeId,       // also identities[0]
    identities: Vec<NodeId>,  // full list, default first
    relay_urls: Vec<String>,
    direct_addrs: Vec<SocketAddr>,
}
```

Encoding: `payload = postcard(NodeRecord)`, `digest = sha256("golem-p2p/node-record/v1\0" || payload)`.
The signed record is `postcard { payload, endpoint_sig, identity_sigs }` where

* `endpoint_sig` is the ed25519 signature of the endpoint key over `digest`;
* `identity_sigs[i]` is the yagna-format signature of `identities[i]` over `digest`.

Signatures are over the exact transmitted bytes, so there is no
canonicalization problem. The domain tag separates these digests from any
other 32-byte payload a yagna identity might sign.

**Verification** (`VerifiedRecord::verify`): size limit; strict decoding (no
trailing bytes); version; 1 to 16 unique identities with the default first;
at most 8 relay URLs (https only, at most 256 bytes each) and 16 direct
addresses; not expired and not valid for more than 24 h (with 5 min clock
skew); `seq` not in the future; valid endpoint signature; exactly one signature
per identity, each recovering to that identity. `VerifiedRecord` can only be
constructed by verification (or by signing, which verifies the result), so the
type system guarantees that unverified records never reach the connection
logic.

Why each piece is there:

* **Every listed identity signs the whole record.** Nobody but the key holders
  can add an identity to a node, group identities together, or point an
  identity at an endpoint. Relays and directories need no trust.
* **The endpoint key signs too.** Without it, a node could publish a record
  that claims somebody else's endpoint for its own identities, and a peer
  might attribute that endpoint's connections to the wrong identities.
* **`seq`.** Among valid records, a higher sequence number wins. This also
  prevents rollback to an old endpoint key or address. The sequence number is
  `max(previous + 1, now in microseconds)`. It is monotonic across restarts
  without persistence as long as the clock does not jump back, and
  `Builder::min_seq` lets an application persist and restore it if it wants
  to cover that case. Records with a sequence number in the future (beyond
  clock skew) are rejected, so a wrong clock cannot make a record permanent.
* **`instance_id`.** Distinguishes a restarted process from the previous one,
  also when the endpoint key is kept.
* **`expires_at`.** Default TTL is 1 h; the record is re-signed after a third
  of that, and whenever identities or the published addresses change. A
  re-sign that fails (a signer that cannot sign at the moment) or a publish
  that fails (a directory that is down) is retried after 1 s, then with
  doubling waits up to once a minute, until it succeeds.

### Which direct addresses are published

iroh reports every address the endpoint may be reachable on: the public
address the relays saw (QUIC address discovery, QAD) and the addresses of all
local interfaces that are up. On a host with Docker, libvirt or Kubernetes,
that includes the bridges to its own containers and VMs. A test host with 14
Docker networks published its public IPv4, 14 addresses `172.17.0.1` to
`172.30.0.1` and its IPv6 address: exactly the 16 a record holds, and since
the record took the first 16 of a sorted list, a few more Docker networks
would have pushed out the IPv6 address and then the public IPv4 address.
Those bridge addresses are useless to peers anyway: their networks end at
this host, and on a peer `172.17.0.1` is the peer's own Docker bridge (TLS
makes dialing it harmless, but it wastes attempts). Many Golem providers run
Docker.

So `Endpoint` (module `addrs`) filters, ranks and caps the addresses before
signing. One code path, no option needed for the default:

1. **Filter by interface, not by address range.** `172.16/12` and `10/8` are
   real LANs too, so an address is left out only if every local interface
   that carries it is *host-internal*, by name: `docker*` (`docker0`,
   `docker_gwbridge`), Docker's user-defined networks `br-` plus 12 hex
   digits (not other `br-` names such as OpenWrt's `br-lan`, nor `br0` or
   Proxmox's `vmbr0`, which carry the host's LAN address), `veth*`, `virbr*`
   (libvirt), `lxc*` (LXC, Cilium), `lxdbr*`, `incusbr*`, `podman*`,
   `nerdctl*`, `cni*`, `flannel*`, `cali*`, `vxlan.calico`,
   `vxlan-v6.calico`, `cilium_*`, `kube-*` (`kube-ipvs0`, `kube-bridge`),
   `weave*`, `vmnet*` (VMware), `vboxnet*` (VirtualBox).
   `Builder::exclude_interface` (CLI: `--exclude-interface`) adds names or
   prefixes (`wg*`). Inside a container the interface is `eth0`, so
   containers still publish their container address.
2. **Rank.** By scope: global addresses first, then private (RFC 1918,
   CGNAT `100.64/10`, link-local, IPv6 ULA), then loopback (iroh lists
   loopback only when there is nothing else). Within a scope, addresses that
   are on no local interface come first: they were learned from relays (QAD),
   i.e. the address the internet sees behind a NAT. Within each of these
   groups IPv4 and IPv6 alternate, each sorted by address, so many addresses
   of one family cannot starve the other. The order is deterministic.
3. **Cap** at 16 (`MAX_DIRECT_ADDRS`). The cap now drops the least useful
   addresses; a public or QAD address is never dropped while a private one
   stays.

The interface list comes from `netwatch`, the crate iroh itself uses to find
its local addresses, so both see the same interfaces and no dependency is
added. iroh does not expose where an address came from: its `DirectAddr`
carries a `DirectAddrType` (local, QAD, port mapped, configured), but
`Endpoint::addr`/`watch_addr` drop it, and the QAD addresses are only
available through the unstable net report API. "On no local interface"
identifies the QAD addresses without that API.

**VPN interfaces** (`wg*`, `tailscale0`, `zt*`, `tun*`, `utun*`) are
published, ranked by their addresses, which are private, CGNAT or ULA in
practice: after all public addresses, so they can never push one out. The
line drawn is "can any other machine ever reach this address": a container
bridge cannot, a VPN can (two nodes in the same tailnet, or a private
deployment over WireGuard without relays, where the record is the only way
to find a peer). Dropping VPN addresses could break such setups, keeping them
costs peers outside the VPN a failed attempt, as with LAN addresses. VPN
paths can make a test report a "direct" connection that is really a VPN
tunnel; test harnesses on VPN-connected machines should pass
`--exclude-interface` for the VPN interfaces (iroh may still find the VPN
path during hole punching, see below; the path's remote address shows it).

**What golem-p2p does not control.** The record is what peers dial first and
what the directory stores. After a connection is up (also over a relay),
iroh runs QUIC NAT traversal: it sends all its local direct addresses
(including Docker bridges and VPN addresses, up to 32) to the peer inside the
encrypted connection, and both sides probe them while hole punching.
golem-p2p cannot filter these without patching iroh (iroh 1.4.1 has no hook;
`iroh::endpoint::Builder::addr_filter` only applies to iroh's own address
lookup publishing, which golem-p2p does not use). They go only to
authenticated peers, never to a directory, and TLS makes a wrong target
harmless.

**Older nodes.** This is publishing policy, not a verification rule. A record
that lists Docker addresses (within the bounds) still verifies, and peers
dial every address it lists.

## 6. Record store

`RecordStore` is used as the peer cache in each endpoint, by
`MemoryDiscovery`, and by the directory server. Rules:

* Only verified records are stored. Each record is indexed under every
  identity it lists and under its endpoint ID. Per key, the highest `seq`
  wins; equal `seq` with different content keeps the first one seen.
* An identity is **withdrawn** when the endpoint that last listed it publishes
  a newer record without it (default switch, alias removal). Withdrawn
  identities do not resolve, although their older record is still validly
  signed.
* Nothing pins an identity to an endpoint. A newer record for the same
  identity always replaces the previous binding, which is how restarts with a
  new endpoint key and default switches propagate.
* Bounded: at most N identities and N endpoints (default 4096 in a node).
  When full, expired entries go first, then those that expire soonest.

## 7. Connections and wire protocol

ALPN `golem-p2p/1`; an endpoint accepts no other ALPN (section 4). All
bidirectional streams start with a kind byte. Frames are a 4-byte big-endian
length plus postcard.

**Handshake (control stream).** The dialer opens the first stream and sends
`Hello { record, target }`. The acceptor verifies the record, checks
`record.endpoint_id == TLS remote endpoint ID`, checks that `target` is one of
its current identities, and answers `Welcome { record }` or
`Reject { reason }`. The dialer runs the same checks on the answer and also
that the answer lists the identity it dialed. A record from an older instance
of an endpoint than one already known is refused on both sides.

**Record updates.** The control stream stays open. When a node's record
changes (identities or addresses), it sends `Update { record }` on every
connection. The receiver verifies it, requires the same endpoint and instance,
and accepts only a higher `seq`. An invalid update from an authenticated peer
closes the connection. Updates are paced (100 ms) so a peer cannot make us do
unbounded signature recovery.

**Application streams.** Each further stream starts with
`AppHeader { from, to }`. The receiver accepts it only if `to` is one of its
current identities and `from` is listed in the peer's *current verified*
record. The header is a claim that is checked against the authenticated
binding, never a source of identity. Rejections reset the stream with an
error code (`UNKNOWN_IDENTITY`, `SENDER_NOT_BOUND`, `BUSY`).

**Connection reuse.** One connection per peer node serves all its identities
in both directions. `connect(id)` reuses a live connection whose current peer
record lists `id`, unless the store knows that `id` moved to another endpoint
or that the endpoint restarted. Otherwise it looks the record up in discovery
(cache as fallback) and dials. Concurrent `connect` calls for the same target
make one lookup and one dial: the others wait for it and use its connection,
or get its error if it failed while they waited, so a burst of requests to an
unreachable peer fails after one connect timeout, not one per request.

**Request/response.** `Endpoint::request` opens a stream, writes the request,
finishes, and reads the response. It is not retried once the request may have
reached the peer (requests are not assumed idempotent). On a connection error
or timeout the connection is closed, so the next call dials fresh with a newly
looked-up record. That is how a peer recovers after the other side crashed.
The request timeout starts when the connection is ready and also covers
waiting for a stream when the peer has reached its limit of 256 concurrent
streams; that wait timing out leaves the connection open (the peer is busy,
not gone).

## 8. Discovery

`Discovery` maps a node ID to a verified record:

```rust
trait Discovery {
    fn publish(&self, record: &VerifiedRecord) -> BoxFuture<Result<(), DiscoveryError>>;
    fn lookup(&self, id: NodeId) -> BoxFuture<Result<Option<VerifiedRecord>, DiscoveryError>>;
}
```

The record then gives the iroh `EndpointAddr` (endpoint ID, relay URLs, direct
addresses). Implementations:

* `MemoryDiscovery`, in-process, for tests.
* `HttpDirectory`, a client for the HTTP directory in `directory`. `PUT
  /v1/records` stores a signed record (409 if a newer one is stored), `GET
  /v1/records/0x<id>` returns it. The server verifies before storing and uses
  the same `RecordStore` rules and bounds; the client verifies again and
  checks that the record lists the requested ID. Response bodies are size
  limited.
* `MultiDiscovery` publishes to several services and returns the newest valid
  record from all of them, so one stale or withholding directory cannot hide a
  newer record held by another.

**Why an HTTP directory.** It is the smallest thing that works across machines
and maps directly onto what yagna does today ("ask the relay where node X
is"). Because records are self-signed, a directory needs no trust: the worst it
can do is withhold records or serve old ones, and old ones lose against newer
ones. Alternatives considered:

* iroh's pkarr/DNS address lookup is keyed by the iroh endpoint ID and signed
  only by the ed25519 key. It cannot answer "where is Ethereum address X", and
  its DNS-sized packets do not fit a record with up to 16 identity signatures.
  It could later carry the record of an already known endpoint.
* A DHT or gossip-based directory removes the central service but is far more
  complex; it can be added later behind the same trait.

**Directory transport.** For now the directory server speaks plain HTTP, and
the client accepts `http` and `https` URLs, which keeps local testing simple.
A directory carries only public, signed records, never node-to-node traffic,
so plain HTTP cannot forge or alter a record. It does let anyone on the path
see which node IDs are published and looked up, and withhold records or serve
old ones. Before production the directory gets TLS (in the server or a
terminating proxy) and nodes use `https` directory URLs (section 14). This is
separate from relays, which are https only already (section 9).

**iroh integration.** `RecordLookup` implements iroh's `AddressLookup` from
the endpoint's store of verified records, keyed by endpoint ID. Whenever iroh
needs addresses for a peer (dialing, re-resolution), it gets exactly what the
peer signed. Once connected, iroh also learns the peer's local addresses from
QUIC NAT traversal (section 5, "What golem-p2p does not control").

## 9. Relays

* The endpoint is configured with any number of relays (iroh `RelayMap`).
  iroh's net report picks the home relay with the best latency. The record
  lists the current home relay.
* **Address discovery.** iroh asks relays for the node's public address over
  QUIC (QAD), on UDP port 7842 of the relay host unless the relay's
  `RelayConfig` names another port (`Builder::relay_config`; CLI:
  `--relay URL,quic=PORT` or `--relay-quic-port`).
* **Cross-relay reachability.** A dialer connects to the relay listed in the
  peer's record on demand, also if that relay is not in its own relay map.
  Tested: node A homed on relay 1 only, node B on relay 2 only, traffic in both
  directions.
* **Failover.** If the home relay goes down, iroh moves to another relay on
  its next net report (every 20 to 26 s). A watchdog speeds this up: when no
  home relay has been connected for 3 s, it re-inserts the current relay
  configuration unchanged, which makes iroh run a net report at once. The
  address change triggers a re-signed record, which is pushed to connected
  peers and published to discovery. Tested by shutting down one of two relays.
* **https only.** Relay URLs must use `https`. `Builder::bind` fails with
  `Error::InsecureRelayUrl` for any other scheme, `VerifiedRecord::verify`
  rejects a record that lists one, and the CLI rejects one on the command
  line. So a node neither publishes nor dials a plain-http relay, also not one
  named in a peer's record. Payload confidentiality does not depend on relay
  TLS (packets are end-to-end QUIC/TLS, section 4), but without it everyone on
  the path to the relay would see the relay handshake, the endpoint IDs and who
  talks to whom, and a node could not authenticate its relay. There is no
  exception for loopback or development: iroh-relay's `--dev` mode (plain
  HTTP) cannot be used. A relay server may run plain HTTP behind a
  TLS-terminating proxy, as long as nodes use the https URL.
* **Trust.** A relay sees endpoint IDs, IP addresses, packet sizes and timing,
  never payloads, and it is never asked about identities. Relay certificates
  are verified against the web PKI by default; `Builder::relay_tls` (CLI:
  `--relay-ca`) trusts additional roots, for relays with a self-signed or
  private-CA certificate. Tests run self-signed local relays.

### Decision: no port mapping

golem-p2p does not ask the user's router to open or forward ports: no UPnP
IGD, no PCP, no NAT-PMP. Reachability comes from QAD, hole punching and
relays only. Reasons:

* **It changes a device golem-p2p does not own.** A mapping opens an inbound
  port on the user's router, usually without the user knowing, and it can
  outlive the process (UPnP leases are long or permanent, and a crash leaves
  them behind). Golem providers often run on networks where an admin decides
  what is open; a library should not quietly change that.
* **Exposure.** A mapped port accepts unsolicited packets from the whole
  internet. TLS protects the traffic, but it is still attack surface a
  hole-punched path does not have, and UPnP's SSDP discovery multicasts on
  the LAN and can trigger firewall prompts.
* **One code path.** Router implementations of these protocols vary widely;
  not using them leaves nothing per router to test or support.

How it is enforced: the workspace builds iroh without its `portmapper`
feature (see the comment in `Cargo.toml`), so the `portmapper` crate is not
compiled in, and the endpoint sets `PortmapperConfig::Disabled` on iroh
anyway, so that a larger build that enables the feature (Cargo unifies
features across a dependency graph such as yagna's) still never maps a port.
There is no builder option and no CLI flag to turn it on (the former
`Builder::portmapper` and `--no-portmapper` had no effect and are removed).
`bench` lists port mapping under "Not measured" with this reason.

If port mapping is ever wanted, it should come as a separate, explicit
command (for example `golem-p2p port-map`) that a user runs knowingly, that
says which port it opens on which router and for how long, and how to remove
it again; not as something the library does by default.

## 10. Restarts and the iroh key

**Decision: by default a node generates a new iroh endpoint key on every
start and never writes it to disk.** Reasons:

* The endpoint key plus a valid record is enough to impersonate the node's
  identities at the transport level until the record expires. Keeping it only
  in memory limits that exposure to the process lifetime, like ya-relay's
  per-session key. There is no new secret to protect on disk.
* A new key gives a new endpoint ID, so a restarted process can never be
  confused with the previous one at the transport level.
* Nothing depends on a stable endpoint ID: peers find nodes by Ethereum
  identity, and the newer record replaces the old binding everywhere.

`Builder::secret_key` allows a fixed key for operators who want a stable
endpoint ID. Restarts are still told apart by `instance_id` and `seq`: a peer
that sees a new instance of an endpoint closes connections of the old
instance, and refuses connections that present an older instance.

After a crash, a peer may still hold a connection to the dead process. Its next
request fails after the request timeout, the connection is closed, and the
following request dials the new process using the newer record from
discovery. The crash test kills a real process with SIGKILL and checks that
every answer afterwards comes from the new process.

## 11. Lessons from ya-relay, and how this design applies them

* **A relay or directory must never decide which identities belong together,
  or which key belongs to an identity.** Here every identity signs the full
  record (identity list, endpoint key, addresses, sequence number, expiry), and
  the endpoint key signs it too. Relays and directories only store and forward
  signed records.
* **The identity of a connection comes from the authenticated connection,
  never from data inside it.** The remote endpoint ID comes from TLS. The
  peer's identities come only from a verified record whose endpoint ID matches.
  Sender fields in stream headers are checked against that binding.
* **Identity changes are normal events.** Default switches (with and without
  restart), runtime aliases, switching back (A to B to A), and crash restarts
  are all just newer records. Caches are keyed so that a newer record replaces
  an older one for every identity and endpoint it touches; nothing is cached
  under an identity in a way that outlives a newer record. Records are pushed
  to connected peers and published to discovery on every change. An alias
  added at runtime is reachable immediately.
* **A restart is never confused with the previous process.** New endpoint key
  by default, `instance_id` always, monotonic `seq`.
* **All per-peer state is bounded.** See section 12.
* **Never fall back to plaintext.** There is no unencrypted mode and no
  option for one (section 4), and relays are https only (section 9).

## 12. Bounds

| state | bound |
|---|---|
| record size / identities / relays / addresses | 4 KiB / 16 / 8 / 16 (direct addresses ranked before the cap, section 5) |
| peer record cache (`RecordStore`) | 4096 identities + 4096 endpoints, configurable |
| directory store | configurable, CLI uses 100 000; request body 4 KiB |
| connections | `max_connections`, default 1024 |
| handshakes in progress | 128, each with a 10 s timeout |
| streams per connection | 256 bidirectional, header read timeout 10 s |
| queued incoming streams | 256, then new streams are reset with `BUSY` |
| control frames | 16 KiB, record updates paced at 100 ms |
| events | broadcast channel of 256, slow subscribers lag |
| responses (`request`) | 16 MiB, `Builder::max_response_size` |
| HTTP directory responses (client) | 4 KiB |

## 13. How yagna would use it

* **Signer.** An adapter implementing `Signer` by sending `identity::Sign`
  over GSB. The GSB client is not `Send`, so the adapter forwards requests to
  the actix arbiter through a channel.
* **Identities.** On start, `identity::List` gives the default and aliases.
  Subscribing to identity events and calling `set_identities` keeps the
  record current, so default switches and new aliases need no restart.
* **Net service.** Today `/net/{to}/{service}` calls are routed through
  ya-relay. With golem-p2p, the net service would call
  `request_from(from, to, msg)` (or open a stream per call), and serve
  `Endpoint::accept()` by dispatching to local GSB services, with
  `IncomingStream::from()` as the authenticated caller.
* **Configuration.** A list of relay URLs (https) and directory URLs instead
  of the single relay address.
* **Coexistence.** A node could run both stacks during migration and prefer
  golem-p2p when the peer has a record.

GSB integration itself is out of scope for this seed.

## 14. Not implemented or not tested yet

* Real NAT traversal across different networks (tests use loopback only).
* TLS for the HTTP directory. The server speaks plain HTTP and the client
  accepts `http` URLs, for easier testing. Before production: serve the
  directory over TLS and use `https` directory URLs only (section 8).
* Directory rate limiting per client, and authentication for publishing.
* Relay authentication tokens: can be passed with
  `Builder::relay_config(RelayConfig::new(..).with_auth_token(..))`, not tested.
* Persisting `seq` (the API exists via `min_seq` and `local_record().seq()`).
* Datagram and unidirectional streams (disabled: 0 uni streams).
* Metrics beyond what iroh exports.
* Explicit "withdraw" records for identities removed while offline.
* Address filtering beyond host-internal interfaces: private LAN and VPN
  addresses are published (section 5, question 3 in section 15). Interface
  names are only matched as Linux, macOS and BSD name them; Windows adapter
  names (Hyper-V's `vEthernet`, WSL) are not recognized.
* A loader for yagna's encrypted key files in the CLI (it reads raw hex keys).

## 15. Open questions for the owner

Resolved: encryption (section 4). Traffic is always encrypted with iroh's
QUIC/TLS 1.3, with no opt-out and no second layer; relays are https only.
Port mapping (section 9): not used; maybe later as a separate, explicit
command.

1. Who operates directories and relays, and how many? Should records also be
   published somewhere decentralized?
2. Should a node only use relays from a configured allowlist? Today any
   `https` relay URL in a valid record is dialed, as in iroh.
3. Should direct LAN (and VPN) addresses be published to a public directory,
   or only relay URLs plus public addresses (iroh's pkarr publishes relay
   only by default)? Container and VM bridge addresses are no longer
   published (section 5).
4. Is a network/chain identifier needed in the record (mainnet vs testnet
   directories)?
5. Default record TTL (1 h) and request timeout (30 s): fine for yagna?
6. Should `request` offer an opt-in retry for idempotent calls?
7. License for the repository.
