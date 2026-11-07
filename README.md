# golem-p2p

Peer-to-peer communication between [yagna](https://github.com/golemfactory/yagna)
nodes, built on [iroh](https://iroh.computer). It is meant to replace the
relay-based networking in yagna (`ya-relay`).

* **Transport:** iroh 1.4 (QUIC, TLS 1.3, hole punching, relays). Direct
  connections where possible, relayed otherwise, with any number of relays.
  No port mapping: golem-p2p never asks the router to open ports (UPnP, PCP,
  NAT-PMP).
* **Encryption:** always on, in one mode. Every connection is end-to-end
  QUIC/TLS 1.3 between the two endpoints, also through relays, which see only
  ciphertext and endpoint IDs. There is no plaintext mode, no opt-out and no
  second layer. Relay URLs must be `https`.
* **Identities:** the same as yagna. A node identity is a secp256k1 key and the
  node ID is its 20-byte Ethereum address. A node has a default identity and
  optional aliases, and can change them at runtime.
* **Binding:** a node record lists the node's identities, its iroh endpoint ID,
  relay URLs, direct addresses, a sequence number and an expiry. Every listed
  identity and the endpoint key sign it. Peers accept only fully verified
  records, so relays and directories need no trust.
* **Addresses:** a node publishes its public address (as relays see it) and
  its interface addresses, public before private, at most 16. Addresses of
  container and VM bridges (`docker0`, Docker's `br-<id>`, `veth*`,
  `virbr*`, ...) are left out: no peer can reach them.
* **Discovery:** a `Discovery` trait from node ID to verified record, with an
  in-memory implementation and an HTTP directory (client and server).

## Status

Early seed. The core library and tests work locally against local iroh relays.
It is **not used by yagna yet**, the wire format may change, and nothing has
been tested across real NATs. See [docs/design.md](docs/design.md) for the
design, the decisions taken, and the open questions.

**License:** not chosen yet. Until a license is added, no rights are granted.

## Layout

```
crates/golem-p2p       core library
crates/golem-p2p-cli   `golem-p2p` command line tool (directory, listen, send, probe, bench)
docs/design.md         design document
docs/cli-events.md     JSON events of the CLI (`--json`), for test harnesses
```

## Build and test

Requires Rust 1.91 or newer (iroh's minimum).

```sh
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

The tests start local iroh relay servers (with self-signed certificates), HTTP
directories and nodes on `127.0.0.1` with OS-assigned ports. They need no
network access. The crash test runs the CLI as a child process and kills it
with SIGKILL. Set `RUST_LOG=golem_p2p=debug` to see what happens.

What the tests cover:

| test file | scenarios |
|---|---|
| `connectivity.rs` | direct connection; relay-only connection (no UDP, path checked); relay-assisted connection becoming direct; two relays with each node homed on a different one (via HTTP directory); a relay going down while another stays up; concurrent requests to a new peer share one lookup and one dial, and to a dead peer fail together after one connect timeout; the request timeout covers waiting for a stream when the peer keeps all its streams busy; a request whose handler is too slow times out alone (its stream is reset with `TIMED_OUT`, concurrent requests succeed, the connection stays open); a peer that hangs is noticed within one request timeout (connection closed, redialed once it recovers); a request cancelled while it is being sent reaches the handler as a reset (`CANCELLED`), never truncated; an address change while `bind` publishes the first record is not lost; `max_response_size`; a request the handler drops unanswered fails with `NOT_ANSWERED` |
| `identity_changes.rs` | default switch with and without restart; A to B to A with and without restart; alias added and removed at runtime; locked key blocks updates; restart with a persisted endpoint key; a failed re-sign (locked key) and a failed publish (directory down) are retried |
| `security.rs` | forged record from a directory; stale record replay; record listing an identity that did not sign it; valid record presented over another endpoint's connection (both directions); spoofed stream sender and receiver; new instance superseding a live old connection; duplicate record updates paced |
| `encryption.rs` | peers offering another ALPN refused in the TLS handshake; a relay (plain HTTP behind a recording TLS-terminating proxy) sees endpoint IDs but neither a plaintext marker nor the Ethereum identities; non-https relay URLs rejected by the builder; a signed record with an `http://` relay refused and never dialed |
| `crash_restart.rs` (CLI) | real process killed with SIGKILL and restarted with the same identity, homed on an https relay with a self-signed certificate (`--relay-ca`) |
| `relay_url.rs` (CLI) | `http://` and other non-https relay URLs rejected on the command line |
| `probe.rs` (CLI) | `--json` output of `listen`, `probe` and `send` as child processes: relay-only probe with bulk transfer (every line parses, common fields, summary consistent with the replies, listener saw every request and the bulk bytes over the relay); probe that ends on a direct path with `time_to_direct_ms`; unknown node fails with exit code 1; SIGTERM ends a probe with a summary; a bulk upload above the listener's `--max-transfer-size` is refused (exit code 3, cause `refused`), one within it works; an echo request that is never finished is refused after 10 s; `keygen`/`directory` with `--json`, error event and exit code |
| `relay_quic.rs` (CLI) | the relay's QUIC address discovery port (`--relay-quic-port`, per relay `,quic=PORT`, `off`) reaches iroh: a local relay with QAD on a random port sees QAD connections; invalid ports rejected |
| `bench.rs` (CLI) | `bench --json` against a local listener: relay-only (exit code 4, every phase over the relay, transfers capped for the relay, the listener saw the upload and download) and normal (exit code 0, QAD answers, direct path, `--compare-relay` over the relay, `--idle`); the listener's limits (too large and busy refused cleanly with exit code 5, exactly the limit works); uploads, downloads and parallel requests end at `--transfer-time` (a stopped upload is still counted by the listener, cut-off requests are not failures); local check only, healthy and with an untrusted relay certificate; unknown node (exit code 3); SIGTERM ends a bench with a report; invalid arguments |
| unit tests | yagna/ethsign compatibility vectors, record verification (including https-only relay URLs, and records of older nodes that list Docker addresses), record store rules and bounds, published addresses (container and VM bridges left out, LAN and VPN kept, ranking before the cap, IPv6); CLI: argument parsers, timestamps, percentiles, jitter, refusals, the bench verdict rules |

## Trying it between two machines

You need a relay and a directory both machines can reach. Relays must use
`https`, so iroh-relay's plain-HTTP `--dev` mode does not work. Any iroh 1.4
relay with a TLS certificate does. With a publicly trusted certificate (for
example iroh-relay's `cert_mode = "LetsEncrypt"`), leave out `--relay-ca`.
With a self-signed certificate for the relay's IP address:

```sh
cargo build --release -p golem-p2p-cli
alias golem-p2p=target/release/golem-p2p

# A relay with a self-signed certificate, https on port 3341:
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -days 30 -subj /CN=golem-p2p-relay -addext subjectAltName=IP:RELAY_IP \
    -addext basicConstraints=critical,CA:FALSE -keyout relay.key -out relay.crt
cat > relay.toml <<'END'
http_bind_addr = "[::]:3340"
enable_metrics = false
[tls]
https_bind_addr = "[::]:3341"
cert_mode = "Manual"
manual_cert_path = "relay.crt"
manual_key_path = "relay.key"
END
cargo install iroh-relay --version 1.4.1 --features server
iroh-relay --config-path relay.toml

# A directory (plain HTTP for now; TLS comes before production):
golem-p2p directory --listen 0.0.0.0:8080

# Machine A (both machines need a copy of relay.crt)
golem-p2p keygen --out alice.key          # prints Alice's node ID
golem-p2p listen --key alice.key \
    --relay https://RELAY_IP:3341 --relay-ca relay.crt \
    --directory http://DIR_HOST:8080/

# Machine B
golem-p2p keygen --out bob.key
golem-p2p send --key bob.key \
    --relay https://RELAY_IP:3341 --relay-ca relay.crt \
    --directory http://DIR_HOST:8080/ \
    --to 0xALICE_NODE_ID --count 5 "hello"
```

`send` prints each reply, the round-trip time and whether the path is
`direct` or `relay`. Repeat `--relay` and `--directory` to use several.
`send`, `probe` and `bench` work without `--key`: they then use a temporary
identity that is not saved. `listen` needs `--key`.
`--alias-key` adds secondary identities, `--relay-only` disables direct UDP,
`--exclude-interface wg0` keeps an interface's addresses out of the record
(container and VM bridges are always left out).
`listen` prints the relays it is homed on in its `READY` line. An empty list
usually means the relay's certificate is not trusted.

**QUIC address discovery (QAD).** iroh learns its public address from
relays over QUIC, by default on UDP port 7842 of the relay host. iroh-relay
serves QAD with `enable_quic_addr_discovery = true`, on `quic_bind_addr` in
its `[tls]` section. If that is not port 7842, set the port per relay with
`--relay https://RELAY_IP:3341,quic=PORT`, or for all relays with
`--relay-quic-port PORT`; `off` disables QAD. A per-relay setting wins.

## Measuring connectivity: `probe` and `--json`

`probe` connects to a node running `listen` and measures for `--duration`
(default 30 s): one request per `--interval` (default 1 s, size
`--payload-size`, default 64 bytes), the time to the first reply, whether and
when the path became direct, and round-trip time percentiles. `--bulk 4M`
then sends 4 MB in one stream to the listener and reports the throughput.
The exit code is 0 if at least one request got a correct reply (1 if none, 3
if only the bulk transfer failed).

```sh
golem-p2p probe --key bob.key \
    --relay https://RELAY_IP:3341 --relay-ca relay.crt \
    --directory http://DIR_HOST:8080/ \
    --to 0xALICE_NODE_ID --duration 30s --interval 1s --bulk 4M
```

`--bulk` uploads are bounded by the listener's limits (below).

With the global option `--json` every command prints machine-readable events
on stdout, one JSON object per line (logs stay on stderr): `ready`, `conn`,
`path_change`, `home_relay`, `request`, `reply`, `summary`, `error` and more,
each with a timestamp and the values of `--run-id` and `--node-name`. This is
how a test harness controller drives the CLI on many machines. The events,
their fields and the exit codes are described in
[docs/cli-events.md](docs/cli-events.md). `listen --duration` stops a
listener on its own, and SIGTERM ends a probe early with a summary.

## Diagnosing connectivity: `bench`

`bench` is one command for users who are not networking experts: it measures
as much as it can and explains the result in plain words. It needs no key
file (it uses a temporary identity).

```sh
# Only the local network: relays, directories, UDP, public address, NAT.
golem-p2p bench --relay https://RELAY_IP:3341 --relay-ca relay.crt \
    --directory http://DIR_HOST:8080/

# Also the connection to a node that runs `listen`:
golem-p2p bench --relay https://RELAY_IP:3341 --relay-ca relay.crt \
    --directory http://DIR_HOST:8080/ --to 0xALICE_NODE_ID --compare-relay
```

**Local network check** (no peer needed; `--skip local` leaves it out):

* every relay: DNS, TCP, TLS with the configured trust roots, and
  `GET /ping`, each timed; an untrusted (self-signed) certificate is named as
  such;
* every directory: whether it answers, and whether this node's record was
  published to it;
* from iroh's net report: whether UDP works (a relay answered QUIC address
  discovery over IPv4/IPv6), the public address the relays saw, whether it is
  behind NAT and the NAT kept the port, whether the public address differs
  per relay (hard NAT; needs answers from two relays), latency per relay, the
  preferred relay; and the home relay and the node's direct addresses.

**Peer benchmark** (`--to NODE_ID`), each phase bounded by size and time:
connect (directory lookup and handshake times, first path); latency (one
request per 200 ms for 12 s: round-trip min/median/p95/max, jitter, failures,
path over time, time until direct, fraction direct, path changes); upload and
download (10 MB each or 8 s); concurrency (8 streams of 256 KiB requests, 8 MB,
plus the round trip of small requests meanwhile); with `--compare-relay` the
same over a relay-only connection, i.e. what you get if hole punching fails;
with `--idle 60s` whether the connection survives a minute without traffic.
QUIC packet loss is reported per phase; only loss while measuring latency
(when the connection is nearly idle), counted once the path has settled after
hole punching, counts as a problem of the network, as the transfers lose
packets by design while congestion control probes for the link's capacity,
and so does setting up a path. Transfers through a relay
are capped at `--relay-transfer-size` (4 MB) to spare shared relays. With the
defaults a run moves at most about 36 MB (44 MB with `--compare-relay`) and
takes about 17 s on a fast link and about 40 s on a slow one, where every
transfer phase stops at its time limit (`--compare-relay` adds about 8 to 30
s; details in [docs/cli-events.md](docs/cli-events.md)). See
`golem-p2p bench --help` for all options.

The report ends with a verdict, for example:

```
Verdict: RELAY ONLY (exit code 4)
  Connected to 0x1234...abcd, but only through a relay
    (https://relay.example:3341/): 75 ms round trip, 22.0 Mbps up, 20.1 Mbps
    down. No relay answered QUIC address discovery (UDP port 3342, as
    configured), although they answer over HTTPS, and no direct path to the
    peer was found: outgoing UDP is probably blocked on your network, so all
    traffic goes through a relay; ask your network admin to allow outgoing
    UDP. (Or the relay does not serve address discovery on port 3342: check
    ,quic=PORT.)
  Not measured: the peer's NAT and firewall cannot be seen from this side
```

When no relay answers QUIC address discovery on iroh's default port 7842
(no `,quic=PORT` given), the verdict names both possibilities: blocked UDP,
or a relay that serves address discovery on another port. A direct path to
the peer proves that UDP works (hint `qad_failed`). With one relay the bench
cannot measure whether the NAT changes the port per destination, but when
the connection stays relayed and the NAT did not keep the local port, the
verdict names a likely symmetric (hard) NAT on this side.

Every statement in the verdict is based on something measured in this run;
`Problem:` lines are what made the result not healthy, `Hint:` lines are
observations with advice, and `Not measured:` lines say what is unknown.

Exit codes: 0 healthy, 1 setup failed, 2 invalid arguments, 3 could not
connect to the node, 4 connected only through a relay, 5 degraded (problems
found). With `--json` the last line is a `bench_report` event with every
measurement, the verdict, the issues, hints and what could not be measured
([docs/cli-events.md](docs/cli-events.md)).

What `bench` cannot tell: whether the router supports UPnP, PCP or NAT-PMP
(golem-p2p deliberately does not use port mapping, see the design document),
the NAT behaviour with only one relay, packet loss towards this node, and
anything about the peer's NAT or firewall.

**Running a bench node.** A public `listen` node bounds what it serves:
uploads and downloads of at most `--max-transfer-size` (default 100 MB) each,
`--max-transfers` (8) at once over all peers, `--max-transfer-time` (120 s)
each; beyond that they are refused with a reason the client reports. Echo
requests are limited to 1 MiB and 10 s. There is no per-peer rate limit yet.
Use a key for the bench node that no other process uses, so its record stays the one in
the directory (the same applies to `probe --key` and `bench --key`: another
process with the same key publishes a newer record for that identity).

## Library example

```rust
use golem_p2p::{Endpoint, HttpDirectory, Identities, InMemorySigner};

let signer = InMemorySigner::new(); // in yagna: an adapter to the identity service
let me = signer.generate();
let ep = Endpoint::builder(signer, Identities::new(me))
    .relay("https://relay-1.example.org".parse()?)
    .relay("https://relay-2.example.org".parse()?)
    .discovery(HttpDirectory::new("https://directory.example.org/".parse()?)?)
    .bind()
    .await?;

// Serve requests, each in its own task: `respond` waits until the peer has
// read the response, and one failed request must not stop the server.
// `from()` is verified against the authenticated connection.
let server = ep.clone();
tokio::spawn(async move {
    while let Some(mut incoming) = server.accept().await {
        tokio::spawn(async move {
            match incoming.read_request(64 * 1024).await {
                Ok(req) => {
                    let _ = incoming.respond(&req).await;
                }
                Err(e) => eprintln!("bad request from {}: {e}", incoming.from()),
            }
        });
    }
});

// Send a request to another node by its Ethereum address.
let reply = ep.request(other_node_id, b"ping").await?;

// Identity changes at runtime are signed, published and pushed to peers.
ep.add_alias(second_id).await?;
ep.set_default_identity(second_id).await?;

// Observe connections and peer record changes.
let mut events = ep.subscribe();
```

Main API: `Endpoint::builder(signer, identities)` with `relay`,
`relay_config`, `relay_tls`, `discovery`, `transport_mode`, `bind_addr`,
`exclude_interface`, `connect_timeout`, `request_timeout`,
`max_response_size` (`bind` fails for a relay URL that is not `https`); then `connect`,
`open_bi`/`open_bi_from`, `request`/`request_from`, `accept`,
`set_identities`/`set_default_identity`/`add_alias`/`remove_alias`,
`subscribe`, `connections`, `close`. Implement `golem_p2p::Signer` to sign
with keys held elsewhere, and `golem_p2p::Discovery` for other directories.
