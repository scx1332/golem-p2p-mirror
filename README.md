# golem-p2p

Peer-to-peer communication between [yagna](https://github.com/golemfactory/yagna)
nodes, built on [iroh](https://iroh.computer). It is meant to replace the
relay-based networking in yagna (`ya-relay`).

* **Transport:** iroh 1.4 (QUIC, TLS 1.3, hole punching, relays). Direct
  connections where possible, relayed otherwise, with any number of relays.
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
crates/golem-p2p-cli   `golem-p2p` command line tool (directory, listen, send)
docs/design.md         design document
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
| `crash_restart.rs` (CLI) | real process killed with SIGKILL and restarted with the same identity |
| unit tests | yagna/ethsign compatibility vectors, record verification, record store rules and bounds |

## Trying it between two machines

You need a relay and a directory both machines can reach.

```sh
cargo build --release -p golem-p2p-cli
alias golem-p2p=target/release/golem-p2p

# A relay: any iroh 1.4 relay works. For development, plain HTTP on port 3340:
cargo install iroh-relay --version 1.4.1 --features server
iroh-relay --dev

# A directory (plain HTTP; put it behind TLS for anything serious):
golem-p2p directory --listen 0.0.0.0:8080

# Machine A
golem-p2p keygen --out alice.key          # prints Alice's node ID
golem-p2p listen --key alice.key \
    --relay http://RELAY_HOST:3340 --directory http://DIR_HOST:8080/

# Machine B
golem-p2p keygen --out bob.key
golem-p2p send --key bob.key \
    --relay http://RELAY_HOST:3340 --directory http://DIR_HOST:8080/ \
    --to 0xALICE_NODE_ID --count 5 "hello"
```

`send` prints each reply, the round-trip time and whether the path is
`direct` or `relay`. Repeat `--relay` and `--directory` to use several.
`--alias-key` adds secondary identities, `--relay-only` disables direct UDP.

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
`relay_config`, `discovery`, `transport_mode`, `bind_addr`; then `connect`,
`open_bi`/`open_bi_from`, `request`/`request_from`, `accept`,
`set_identities`/`set_default_identity`/`add_alias`/`remove_alias`,
`subscribe`, `connections`, `close`. Implement `golem_p2p::Signer` to sign
with keys held elsewhere, and `golem_p2p::Discovery` for other directories.
