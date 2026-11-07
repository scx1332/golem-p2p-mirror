# golem-p2p

Peer-to-peer communication between [yagna](https://github.com/golemfactory/yagna)
nodes, built on [iroh](https://iroh.computer). It is meant to replace the
relay-based networking in yagna (`ya-relay`).

* **Transport:** iroh 1.4 (QUIC, TLS 1.3, hole punching, relays). Direct
  connections where possible, relayed otherwise, with any number of relays.
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
crates/golem-p2p-cli   `golem-p2p` command line tool (directory, listen, send, probe)
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
| `connectivity.rs` | direct connection; relay-only connection (no UDP, path checked); relay-assisted connection becoming direct; two relays with each node homed on a different one (via HTTP directory); a relay going down while another stays up |
| `identity_changes.rs` | default switch with and without restart; A to B to A with and without restart; alias added and removed at runtime; locked key blocks updates; restart with a persisted endpoint key |
| `security.rs` | forged record from a directory; stale record replay; record listing an identity that did not sign it; valid record presented over another endpoint's connection (both directions); spoofed stream sender and receiver; new instance superseding a live old connection |
| `encryption.rs` | peers offering another ALPN refused in the TLS handshake; a relay (plain HTTP behind a recording TLS-terminating proxy) sees endpoint IDs but neither a plaintext marker nor the Ethereum identities; non-https relay URLs rejected by the builder; a signed record with an `http://` relay refused and never dialed |
| `crash_restart.rs` (CLI) | real process killed with SIGKILL and restarted with the same identity, homed on an https relay with a self-signed certificate (`--relay-ca`) |
| `relay_url.rs` (CLI) | `http://` and other non-https relay URLs rejected on the command line |
| `probe.rs` (CLI) | `--json` output of `listen`, `probe` and `send` as child processes: relay-only probe with bulk transfer (every line parses, common fields, summary consistent with the replies, listener saw every request and the bulk bytes over the relay); probe that ends on a direct path with `time_to_direct_ms`; unknown node fails with exit code 1; SIGTERM ends a probe with a summary; `keygen`/`directory` with `--json`, error event and exit code |
| `relay_quic.rs` (CLI) | the relay's QUIC address discovery port (`--relay-quic-port`, per relay `,quic=PORT`, `off`) reaches iroh: a local relay with QAD on a random port sees QAD connections; invalid ports rejected |
| unit tests | yagna/ethsign compatibility vectors, record verification (including https-only relay URLs), record store rules and bounds; CLI: argument parsers, timestamps, percentiles |

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
`--alias-key` adds secondary identities, `--relay-only` disables direct UDP.
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

With the global option `--json` every command prints machine-readable events
on stdout, one JSON object per line (logs stay on stderr): `ready`, `conn`,
`path_change`, `home_relay`, `request`, `reply`, `summary`, `error` and more,
each with a timestamp and the values of `--run-id` and `--node-name`. This is
how a test harness controller drives the CLI on many machines. The events,
their fields and the exit codes are described in
[docs/cli-events.md](docs/cli-events.md). `listen --duration` stops a
listener on its own, and SIGTERM ends a probe early with a summary.

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

// Serve requests. `from()` is verified against the authenticated connection.
let server = ep.clone();
tokio::spawn(async move {
    while let Some(mut incoming) = server.accept().await {
        let req = incoming.read_request(64 * 1024).await?;
        incoming.respond(&req).await?;
    }
    Ok::<_, golem_p2p::Error>(())
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
`relay_config`, `relay_tls`, `discovery`, `transport_mode`, `bind_addr`
(`bind` fails for a relay URL that is not `https`); then `connect`,
`open_bi`/`open_bi_from`, `request`/`request_from`, `accept`,
`set_identities`/`set_default_identity`/`add_alias`/`remove_alias`,
`subscribe`, `connections`, `close`. Implement `golem_p2p::Signer` to sign
with keys held elsewhere, and `golem_p2p::Discovery` for other directories.
