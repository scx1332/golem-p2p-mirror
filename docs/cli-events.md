# golem-p2p CLI events (`--json`)

With `--json`, every `golem-p2p` command prints machine-readable events on
stdout: one JSON object per line, flushed after each line. Logs and human
messages go to stderr only, so stdout can be parsed line by line (JSONL).
This is the interface between the CLI, the per-machine test client of the
network test harness, and the harness controller.

`--json`, `--run-id ID` and `--node-name NAME` are global options: they can be
given before or after the subcommand.

```sh
golem-p2p --json --run-id r42 --node-name alice listen --key a.key \
    --directory http://dir:8080/ --relay https://relay:56000,quic=56001 --relay-ca relay.crt
golem-p2p --json --run-id r42 --node-name bob probe --key b.key \
    --directory http://dir:8080/ --relay https://relay:56000,quic=56001 --relay-ca relay.crt \
    --to 0xALICE --duration 30s --interval 1s --bulk 4M
```

Event names and field names are stable. New fields may be added; consumers
should ignore fields they do not know.

## Common fields

Every event has these fields, first and in this order:

| field | type | meaning |
|---|---|---|
| `ts` | string | time the event was created, RFC 3339 UTC with milliseconds, e.g. `2026-11-07T15:09:12.345Z` |
| `event` | string | event name (below) |
| `run` | string or null | value of `--run-id`, null if not given |
| `node` | string or null | value of `--node-name`, null if not given |

Conventions: node IDs are `0x`-prefixed Ethereum addresses (lower case),
endpoint IDs are 64 hex characters, relay URLs are normalized with a trailing
slash (`https://1.2.3.4:56000/`), socket addresses are `ip:port` (`[v6]:port`).
Durations are milliseconds as floats with microsecond precision (`*_ms`).
Paths are `"direct"`, `"relay"` or `"unknown"` (no path selected, or no
connection).

## Ordering

* `listen`, `send`, `probe`: `ready` is always the first line.
* `send`, `probe`: `summary` is always the last line. Nothing is printed after
  it, not even events of connections closing at shutdown.
* Everything in between comes from concurrent tasks; order the lines by `ts`
  when exact ordering matters. For example a `conn` event may follow the first
  `reply` of the request that opened the connection.
* If a command fails before `ready` (bad key file, bind error, ...) its only
  line is an `error` event, and it exits with code 1.

## Events

### `ready`

The node is bound, has published its record, and serves requests (`listen`)
or is about to send them (`send`, `probe`). `listen` prints it as soon as the
endpoint is bound.

| field | type | meaning |
|---|---|---|
| `node_id` | string | default identity |
| `identities` | [string] | all identities, default first |
| `endpoint_id` | string | iroh endpoint ID (new on every start) |
| `instance_id` | string | 32 hex characters, new on every start |
| `relays` | [string] | home relay(s) in the published record; empty if no relay connection (yet), e.g. an untrusted relay certificate |
| `direct_addrs` | [string] | direct UDP addresses in the published record |
| `relay_config` | [{`url`, `quic_port`}] | configured relays and their QUIC address discovery port (null: QAD off) |
| `transport` | string | `direct_and_relay`, `relay_only` or `direct_only` |
| `bind_ms` | number | time to bind, including waiting (at most 5 s) for the relay and for addresses |

### `home_relay`

The home relay, or the connection to it, changed. Not printed before a home
relay is known (and never without relays).

| field | type | meaning |
|---|---|---|
| `url` | string or null | home relay URL; null when there is none any more |
| `connected` | bool | connected to it |
| `error` | string or null | the last connection error, if not connected |

### `record`

Our own record was re-signed and published: addresses or home relay changed,
or the periodic refresh (every 20 min).

| field | type | meaning |
|---|---|---|
| `seq` | number | record sequence number |
| `identities` | [string] | |
| `relays` | [string] | |
| `direct_addrs` | [string] | |

### `conn`

A connection to a peer completed the golem-p2p handshake (either direction).

| field | type | meaning |
|---|---|---|
| `peer` | string | the peer's default node ID |
| `peer_endpoint` | string | the peer's iroh endpoint ID |
| `direction` | string | `out` (we dialed) or `in` |
| `path` | string | selected path at that moment |
| `remote_addr` | string or null | remote address of the selected path: `ip:port` (direct) or the relay URL |

### `path_change`

The selected network path of a connection changed: its kind (relay to direct
and back) or its remote address (for example another direct address).

| field | type | meaning |
|---|---|---|
| `peer`, `peer_endpoint` | string | as in `conn` |
| `from`, `to` | string | path kind before and after |
| `from_addr` | string or null | remote address before |
| `remote_addr` | string or null | remote address after |

### `disconnect`

A connection closed.

| field | type | meaning |
|---|---|---|
| `peer`, `peer_endpoint` | string | as in `conn` |
| `reason` | string | QUIC close reason, e.g. `closed by peer: endpoint closed (code 8)` |

### `request` (listen)

`listen` answered a request.

| field | type | meaning |
|---|---|---|
| `from` | string | sending identity (verified) |
| `to` | string | our identity it was addressed to |
| `path` | string | path of the connection when the request was read |
| `bytes` | number | request size (for `sink`: bytes received after the header) |
| `kind` | string | `echo` (normal request) or `sink` (bulk transfer of `probe --bulk`) |
| `ms` | number | `sink` only: time from stream start to the last byte |

### `reply` (send, probe)

One request/reply.

| field | type | meaning |
|---|---|---|
| `seq` | number | 1, 2, ... |
| `to` | string | target node ID |
| `bytes` | number | request size |
| `path` | string | path of the connection to `to` after the request |
| `new_conn` | bool | there was no live connection before the request, so it included lookup and handshake |
| `ok` | bool | a correct reply arrived (`probe`: the echo matched; `send`: any reply) |
| `rtt_ms` | number or null | request to reply time; null if `ok` is false |
| `elapsed_ms` | number | only if `ok` is false: time until the failure |
| `error_kind` | string | only if `ok` is false, see error kinds below; `mismatch` for a wrong echo |
| `error` | string | only if `ok` is false: message |
| `reply` | string | `send` only: the reply text |

### `summary` (send, probe)

| field | type | meaning |
|---|---|---|
| `to` | string | target node ID |
| `sent`, `ok`, `failed` | number | requests sent, correct replies, failures (`sent = ok + failed`) |
| `duration_ms` | number | from the first request to the end of the requests (and bulk transfer) |
| `connect_ms` | number or null | from the first request to the first correct reply, including lookup, handshake and failed attempts; null if none |
| `time_to_direct_ms` | number or null | from the first request until a direct path to the peer was first seen; null if never |
| `direct_fraction` | number or null | fraction of correct replies whose `path` was `direct`; null if `ok` is 0 |
| `rtt_ms` | object | `min`, `p50`, `p95`, `max`, `mean` of `rtt_ms` over correct replies with `new_conn` false (nearest-rank percentiles); each null without samples |
| `rtt_samples` | number | number of replies in `rtt_ms` |
| `final_path` | string | path of the connection to the peer at the end; `unknown` without a connection |
| `payload_size` | number | request size |
| `interrupted` | bool | ended early by SIGINT/SIGTERM |
| `bulk_bytes` | number or null | `--bulk` size; null without `--bulk` |
| `bulk_ok` | bool or null | the listener confirmed receiving all bytes |
| `bulk_ms` | number or null | from opening the stream to the listener's confirmation |
| `throughput_mbps` | number or null | `bulk_bytes * 8 / bulk_ms / 1000`, megabits (10^6) per second |
| `bulk_path` | string or null | path at the end of the transfer |

### `error`

Something failed that is not a `reply`.

| field | type | meaning |
|---|---|---|
| `kind` | string | error kind, see below |
| `message` | string | human-readable message with causes |
| `peer` | string | `listen` request errors only: the sender |
| `cause` | string | `bulk` only: the kind of the underlying error |

Where errors appear: a command that fails before `ready` prints only `error`
and exits with 1; `listen` prints `error` for a request it could not read or
answer, and keeps running; `probe --bulk` prints an `error` of kind `bulk`
before the `summary` if the transfer failed or was skipped (no successful
reply, or interrupted). Failed requests are `reply` events with `ok: false`,
not `error` events. `send` stops at the first failed request: it prints that
`reply`, the `summary`, and exits with 1.

Error kinds: `fatal` (setup: arguments, files), `request` (listen: stream
error), `bulk`, `lagged` (the CLI missed endpoint events; connections are
rescanned), and the library error kinds: `not_found` (no record for the node
ID in any directory), `timeout`, `connect`, `connection`, `handshake`,
`rejected`, `stream_rejected`, `not_peer_identity`, `identity`, `record`,
`discovery`, `bind`, `config`, `protocol`, `too_large`, `closed`.

### `keygen`, `directory`

`keygen --json` prints `{"event":"keygen", "node_id", "key_file"}`;
`directory --json` prints `{"event":"directory", "url"}` once it listens.

## Exit codes

| code | `probe` | other commands |
|---|---|---|
| 0 | at least one correct reply, and the bulk transfer (if requested) succeeded | success |
| 1 | no correct reply, or setup failed | failure |
| 2 | invalid arguments (from the argument parser, before any event) | same |
| 3 | correct replies, but the bulk transfer failed | - |

SIGINT and SIGTERM stop `listen` and `directory` cleanly (exit 0), and end a
`probe` early with a `summary` (`interrupted: true`). `listen --duration D`
exits on its own after `D`.

## How paths are observed

iroh 1.4 reports every change of a connection's open paths and of its
selected path on `Connection::path_events`. For every connection (found via
the endpoint's `Connected` events), the CLI subscribes to that stream, and on
each event re-reads the path snapshot (`Connection::paths`) and compares the
selected path (kind and remote address) with the previous one. This is event
driven, not polled. A `path_change` is printed for every difference, including
transitions to `unknown` while iroh replaces an abandoned path. `reply.path`
and `summary.final_path` read the snapshot of the connection to the target
without dialing.

Limitations: on a single host (loopback or two nodes on one machine) iroh
selects a direct path during the handshake, so `path_change` does not occur
there; it needs nodes behind NATs or firewalls where a connection starts on
the relay. The path of an `ok` reply is read right after the reply, so a path
switch during the request is attributed to the new path.

## Probe and the listener's bulk sink

`probe` sends one request per `--interval` (default 1 s) for `--duration`
(default 30 s), each `--payload-size` bytes (default 64, at most 1 MiB): the
ASCII marker `probe seq=N ` padded with `x`. The listener echoes it; the
reply is correct if it ends with the payload. Requests are sequential: a slow
request delays the next one (missed intervals are skipped, not bursted).
`--timeout` (default 10 s) applies to connecting and to each request; a
request still running at the end of `--duration` gets at most one more
`--timeout`.

With `--bulk N`, after the requests the probe opens one stream to the
listener, writes the 21-byte header `golem-p2p-cli/sink/1\n` and then `N`
bytes, and finishes the stream. The listener counts and discards the bytes
and replies `endpoint=... instance=... to=... from=... sink_bytes=N`. The
transfer is only attempted after at least one correct reply, and is bounded
by `--bulk-timeout` (default 60 s). Requests that do not start with the header
are echo requests, so `send` with any normal message works as before.
