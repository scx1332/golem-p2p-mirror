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
golem-p2p --json --run-id r42 --node-name carol bench \
    --directory http://dir:8080/ --relay https://relay:56000,quic=56001 --relay-ca relay.crt \
    --to 0xALICE --compare-relay
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

* `listen`, `send`, `probe`, `bench`: `ready` is always the first line.
* `send`, `probe`: `summary` is always the last line; `bench`: `bench_report`
  is always the last line. Nothing is printed after it, not even events of
  connections closing at shutdown.
* Everything in between comes from concurrent tasks; order the lines by `ts`
  when exact ordering matters. For example a `conn` event may follow the first
  `reply` of the request that opened the connection.
* If a command fails before `ready` (bad key file, bind error, ...) its only
  line is an `error` event, and it exits with code 1.

## Events

### `ready`

The node is bound, has published its record (or tried to: a failed publish
is retried in the background), and serves requests (`listen`)
or is about to send them (`send`, `probe`). `listen` prints it as soon as the
endpoint is bound.

| field | type | meaning |
|---|---|---|
| `node_id` | string | default identity |
| `identities` | [string] | all identities, default first |
| `endpoint_id` | string | iroh endpoint ID (new on every start) |
| `instance_id` | string | 32 hex characters, new on every start |
| `relays` | [string] | home relay(s) in the published record; empty if no relay connection (yet), e.g. an untrusted relay certificate |
| `direct_addrs` | [string] | direct UDP addresses in the published record, best first (public before private; container and VM bridges and `--exclude-interface` interfaces left out) |
| `relay_config` | [{`url`, `quic_port`}] | configured relays and their QUIC address discovery port (null: QAD off) |
| `transport` | string | `direct_and_relay`, `relay_only` or `direct_only` |
| `bind_ms` | number | time to bind, including waiting (at most 5 s together) for the relay and for addresses, and publishing the record (at most 10 s; a failed publish is retried in the background) |

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

`listen` answered a request, or served (or refused) an upload or download.

| field | type | meaning |
|---|---|---|
| `from` | string | sending identity (verified) |
| `to` | string | our identity it was addressed to |
| `path` | string | path of the connection when the request was read |
| `bytes` | number | `echo`: request size; `sink`: bytes received after the header; `source`: bytes sent after the status line |
| `kind` | string | `echo` (normal request), `sink` (upload of `probe --bulk` and `bench`) or `source` (download of `bench`) |
| `ms` | number | `sink`, `source` only: time from stream start to the last byte |
| `requested` | number or null | `source` only: bytes asked for (null if the request was malformed) |
| `refused` | string or null | `sink`, `source` only: the limit that refused or cut the transfer: `too_large`, `busy`, `timeout`, `bad_request`; null if served. A `source` with `bytes` below `requested` and no refusal was stopped early by the downloader |

### `reply` (send, probe)

One request/reply.

| field | type | meaning |
|---|---|---|
| `phase` | string | `bench` only, first after the common fields: `latency`, `relay_latency` (`--compare-relay`) or `idle` |
| `seq` | number | 1, 2, ... (per phase) |
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
| `jitter_ms` | number or null | mean absolute difference between consecutive `rtt_ms` samples; null with fewer than two |
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
| `cause` | string | `bulk` only: the kind of the underlying error (`refused` if a listener limit refused it) |

Where errors appear: a command that fails before `ready` prints only `error`
and exits with 1; `listen` prints `error` for a request it could not read or
answer, and keeps running; `probe --bulk` prints an `error` of kind `bulk`
before the `summary` if the transfer failed or was skipped (no successful
reply, or interrupted). Failed requests are `reply` events with `ok: false`,
not `error` events. `send` stops at the first failed request: it prints that
`reply`, the `summary`, and exits with 1.

Error kinds: `fatal` (setup: arguments, files), `request` (listen: stream
error), `bulk`, `refused` (a listener limit refused an upload or download),
`lagged` (the CLI missed endpoint events; connections are rescanned), and the
library error kinds: `not_found` (no record for the node
ID in any directory), `timeout`, `connect`, `connection`, `handshake`,
`rejected`, `stream_rejected`, `not_peer_identity`, `identity`, `record`,
`discovery`, `bind`, `config`, `protocol`, `too_large`, `closed`.

### `bench_phase` (bench)

A phase of `bench` finished. Phases run in this order; skipped ones
(`--skip`, no `--to`, no `--compare-relay`, no `--idle`) print nothing:
`local`, `connect`, `latency`, `upload`, `download`, `concurrency`,
`relay_compare`, `idle`.

| field | type | meaning |
|---|---|---|
| `phase` | string | phase name |
| `ok` | bool | the phase succeeded (`local`: every relay and directory check passed; `latency`, `concurrency`: no failed request; requests cut off by the time limit are not failures) |
| `duration_ms` | number | duration of the phase |
| `result` | object | the phase's section of `bench_report`: `local`, `peer.connect`, `peer.latency`, `peer.upload`, `peer.download`, `peer.concurrency`, `relay_compare`, `peer.idle` |

### `bench_report` (bench)

The complete result, always the last line (also after SIGINT/SIGTERM, then
with `interrupted: true`). Units are part of the field names (`_ms`, `_bytes`,
`_mbps`, `_pct`); values that were not measured are null, and so are the
sections of phases that did not run.

| field | type | meaning |
|---|---|---|
| `status` | string | `healthy`, `no_connection`, `relay_only` or `degraded`, see exit codes |
| `exit_code` | number | the process exit code |
| `verdict` | string | one or two sentences in plain language |
| `issues` | [{`code`, `message`}] | problems; any issue makes a run `degraded` (unless it is `no_connection` or `relay_only` already) |
| `hints` | [{`code`, `message`}] | observations with advice that do not change the status |
| `unknowns` | [{`item`, `reason`}] | what could not be determined, and why |
| `node_id`, `endpoint_id` | string | this node |
| `temporary_identity` | bool | no `--key`: a temporary identity was used |
| `transport` | string | as in `ready` |
| `to` | string or null | the peer (`--to`) |
| `duration_ms` | number | the whole run |
| `interrupted` | bool | ended early by SIGINT/SIGTERM |
| `settings` | object | effective parameters: `timeout_ms`, `latency_duration_ms`, `interval_ms`, `payload_bytes`, `transfer_bytes`, `transfer_time_ms`, `relay_transfer_bytes`, `streams`, `concurrency_bytes`, `concurrency_request_bytes`, `compare_relay`, `idle_ms`, `skip` |
| `local` | object or null | part A, below; null with `--skip local` |
| `peer` | object or null | part B, below; null without `--to` |
| `relay_compare` | object or null | below; null without `--compare-relay` |

Issue codes: `relay_unreachable`, `no_home_relay`, `directory_unreachable`,
`identity_in_use`, `record_not_published`, `udp_blocked`, `connect_failed`,
`requests_failed`, `transfer_refused`, `transfer_failed`, `parallel_failed`,
`connection_lost`, `direct_path_lost`, `idle_failed`, `relay_fallback_failed`.
Hint codes: `qad_failed`, `hard_nat`, `hard_nat_likely`, `no_relays`,
`idle_reconnect`, `idle_path_lost`, `packet_loss`, `jitter`,
`latency_under_load`, `relay_fallback`, `relay_fallback_degraded`. Unknown
items: `local_network`, `port_mapping`, `udp`, `nat_mapping`, `peer_network`,
`packet_loss`, `download_loss`. `port_mapping` is always listed with the local check:
golem-p2p deliberately does not use port mapping (UPnP, PCP, NAT-PMP), so
there is nothing to measure.

`local`:

| field | type | meaning |
|---|---|---|
| `duration_ms` | number | duration of the check |
| `bound_sockets` | [string] | local UDP sockets of the endpoint |
| `direct_addrs` | [string] | direct addresses as published in the record, best first: the public address from QAD and local interface addresses, without container and VM bridges |
| `home_relay` | {`url`, `connected`, `error`} or null | null without relays |
| `net_report` | object or null | iroh's net report, below; null without relays, with `--relay-only`, or if none finished within 6 s |
| `relays` | [object] | one check per configured relay, below |
| `directories` | [{`url`, `ok`, `lookup_ms`, `own_record`, `error`}] | lookup of our own node ID; `own_record`: `published` (our endpoint), `missing`, `other_endpoint` (another process published a newer record for this identity) |
| `record_published` | bool or null | our record is in at least one directory; false if in none of those that answered; null if none answered |

`local.net_report` (iroh's net report probes the relays with QUIC address
discovery (QAD) over UDP, and with HTTPS):

| field | type | meaning |
|---|---|---|
| `udp_v4`, `udp_v6` | bool | a relay answered QAD over IPv4 / IPv6 |
| `public_v4`, `public_v6` | string or null | our address (`ip:port`) as the relays saw it |
| `behind_nat_v4` | bool or null | the public IPv4 address is not the local source address towards it |
| `port_preserved_v4` | bool or null | the public port equals the local UDP port |
| `mapping_varies_by_dest` | bool or null | different relays saw different public addresses (hard NAT); null unless at least two relays answered over the same IP version |
| `qad_relays_v4`, `qad_relays_v6` | number | relays that answered QAD |
| `preferred_relay` | string or null | the relay with the lowest latency |
| `captive_portal` | bool or null | iroh's captive portal check (plain HTTP to port 80 of a relay host). Only meaningful if the relay host serves port 80; not used for the verdict |

`local.relays[]`:

| field | type | meaning |
|---|---|---|
| `url` | string | relay URL |
| `quic_port` | number or null | configured QAD port; null if off |
| `quic_port_explicit` | bool | the QAD setting was given (`,quic=` or `--relay-quic-port`); false for iroh's default port 7842 |
| `ok` | bool | DNS, TCP, TLS (certificate verified with the configured roots) and `GET /ping` all succeeded within 5 s |
| `addr` | string or null | address connected to |
| `dns_ms`, `tcp_ms`, `tls_ms`, `http_ms` | number or null | duration of each step (`dns_ms` is null for an IP address) |
| `http_status` | number or null | status of `GET /ping` |
| `https_latency_ms`, `qad_v4_ms`, `qad_v6_ms` | number or null | from the net report: HTTPS probe and QAD round trips; null without an answer |
| `error_kind` | string or null | `dns`, `tcp`, `tls_untrusted` (unknown issuer, e.g. self-signed without `--relay-ca`), `tls_certificate` (expired, wrong name, ...), `tls`, `http`, `timeout` |
| `error` | string or null | message |

`peer`:

| field | type | meaning |
|---|---|---|
| `connect` | object | `ok`, `lookup_ms` (directory lookup), `connect_ms` (lookup and both handshakes), `handshake_ms` (`connect_ms - lookup_ms`), `path` and `remote_addr` right after connecting, `peer_endpoint`, `error_kind`, `error` |
| `latency` | object or null | `duration_ms`, `sent`, `ok`, `failed`, `failed_pct`, `rtt_ms` {`min`, `p50`, `p95`, `max`, `mean`}, `rtt_samples`, `jitter_ms`, `direct_fraction`, `final_path` (as in `summary`), `quic` (below, during this phase), `loss` (packet loss once the path had settled: `from_ms` (when counting started, from the start of the phase), `sent_packets`, `lost_packets`, `lost_pct`, counted like `quic`; null if the selected path changed until the end of the phase, or the connection changed; see **Packet loss**) |
| `upload`, `download` | object or null | `ok`, `requested_bytes` (after the relay cap), `relay_capped`, `bytes`, `duration_ms`, `throughput_mbps`, `second_half_mbps` (throughput in the second half of the transfer; of an upload: of writing, see **Bench phases**; null if it took less than 1 s), `path` (at the end), `limited_by` (`size` or `time`), `error_kind` (`refused` for a listener limit; `no_count`: an upload whose count did not come back in time, see **Bench phases**), `error`, `quic` (during this phase) |
| `concurrency` | object or null | `ok`, `streams`, `request_bytes` (of the counted requests: 256 KiB, smaller on a slow link or after a restart), `sized_for_mbps` (the requests were made smaller for a slow link: the rate they were sized for, the lowest `throughput_mbps` or `second_half_mbps` of the upload and download; null if not; after a restart it applies to the first requests), `restart` (null, or the phase started over with smaller requests because none had finished by half of `--transfer-time` and they were not about to (see the `concurrency` phase below): `at_ms` (from the start of the phase), `cut_off` (first requests cut off then), `request_bytes` (their size); all other counts, times and the throughput are then of the requests since the restart), `first_reply_ms` (when the first counted reply arrived, from the start of the phase; null if none did; over half of `--transfer-time` without a `restart`: none had finished by then, but they were about to, so the phase went on), `requests` (correct replies), `failed`, `unfinished` (still running at `--transfer-time`, cut off; not failures), `limited_by` (`size` or `time`), `total_bytes` (payload, each way), `duration_ms` (until the last worker ended, from the start of the phase or the restart), `throughput_mbps` (payload, each way), `rtt_ms` (of these requests), `loaded_rtt_ms` and `loaded_samples` (small requests sent meanwhile), `error_kind`, `error`, `quic` (during this phase) |
| `idle` | object or null | `idle_ms`, `ok` (a request succeeded afterwards), `survived` (the same connection was still open), `path_before`, `path_after`, `attempts` (at most 3), `recovery_ms` (first request to first reply), `rtt_ms`, `error` |
| `path` | object | `time_to_direct_ms` (from the start of connecting), `changes`, `final_path` (`unknown` if there was no connection at the end), `final_remote_addr`, `timeline` [{`t_ms`, `path`, `remote_addr`}] (at most 64 entries) |
| `connection_lost` | object or null | the connection made in the `connect` phase closed before the end of the run: `t_ms` (from the start of connecting), `phase` (the phase running then), `cause` (`closed_here`: this node closed it, usually because a request got no answer and nothing at all arrived from the peer during the request timeout (golem-p2p also closes a connection after a connection error, or when the peer restarted); `idle_timeout`: nothing arrived for QUIC's idle timeout; `closed_by_peer`; `reset`: the peer no longer knew the connection, e.g. it restarted; `error`), `reason` (QUIC's close reason). Null if it stayed open. A loss during the `idle` phase is reported by `idle` instead of the `connection_lost` issue |
| `quic` | object or null | QUIC counters of the connection since connecting (of the current connection if it changed; null without one): `sent_packets`, `lost_packets` (sent by us and declared lost), `lost_pct`, `sent_bytes`, `received_bytes` (UDP payload bytes). Path MTU probes are not counted: they are lost by design when larger than the path allows. The `quic` object of a phase has the same fields, counted over that phase; it is null if the connection changed during the phase |

`relay_compare`: `skipped` (string or null: why it did not run), `ok`
(everything worked: connected, every request answered, upload and download
succeeded), `reachable` (connected, and at least one request got a correct
reply or a transfer succeeded), `relay` (the relay URL used), and
`connect`, `latency`, `upload`, `download`, `quic` as in `peer`.

**Relay fallback.** Whether the peer can be reached through a relay is
judged by what got through the relay-only connection (`reachable`), not by
its transfers: `relay_fallback_failed` (issue) only if it could not connect
or nothing got through. If it is reachable but a transfer or some requests
failed (e.g. a relayed upload whose count did not come back in time on a
slow path), the hint `relay_fallback_degraded` says so and names what
failed; if everything worked and a direct path was found, the hint
`relay_fallback` compares the two.

**UDP and the NAT.** `udp_blocked` (issue): QAD was configured and a relay
answered over HTTPS, but no relay answered QAD over UDP, and no direct path
to the peer showed that UDP works. That is what blocked outgoing UDP looks
like, and also what a relay looks like that does not serve QAD on the probed
port. If every QAD port was given explicitly (`quic_port_explicit`), the
message calls UDP probably blocked and mentions the port as the other
possibility; on iroh's default port it names both as equally likely (the
code stays `udp_blocked`). `qad_failed` (hint): no QAD answer either, but a
direct path to the peer worked, so UDP is not blocked and the QAD port is
probably wrong. `hard_nat` (hint): two relays saw different public ports
(`mapping_varies_by_dest`). `hard_nat_likely` (hint, also named in the
verdict): the connection stayed relayed, this side is behind a NAT that did
not keep the local UDP port (`behind_nat_v4` true, `port_preserved_v4`
false), and only one relay answered, so the per-destination behaviour could
not be measured; NATs that change the port usually are symmetric (hard)
NATs. Without a NAT on this side (`behind_nat_v4` false) a relay-only verdict
points at the peer's network or a firewall in front of this machine, not at
"two NATs".

**Packet loss.** The `packet_loss` hint looks at the latency phase only,
and only from when the selected path had settled (`peer.latency.loss`):
from the start of the phase, or, if the path changed shortly before or
during it, from 1 s (or three of the shortest round trips, if longer) after
the last change (`from_ms`). Setting up a path loses packets by design: iroh's hole punching
sends probes that the peer's NAT drops until the hole is open, and they take
packet numbers from the relayed path, so QUIC also declares relayed packets
lost when a probe's acknowledgement overtakes them (campaign 2 counted 1-6
such losses of about 120 packets on clean lab links, all within 75 ms of the
switch to the direct path). `peer.latency.quic` still counts the whole phase.
The hint is given when at least 0.5% of the packets this node sent in that
window were lost, out of at least 50 (with the defaults the window holds
about 140, so one lost packet is enough). If the path did not settle before
the end of the phase, or fewer than 50 packets were counted, packet loss is
listed as unknown (`packet_loss`). The phase sends a few small
requests per second, far below what any link carries, so its losses come
from the network (Wi-Fi, mobile, overloaded links), not from the test. In
the transfers (`upload`, `download`, `concurrency`) QUIC's loss-based
congestion control increases its rate until packets are lost, so loss there
is expected and is reported for information only (it was 2-7% on a clean
150 Mbps path). Random loss matters because congestion control takes every
loss as congestion: throughput falls roughly with the square root of the
loss rate (at 0.5% and 100 ms round trip a connection gets a few Mbps), and
in the lab 1% loss each way cut 123 Mbps to 2-26 Mbps. With about 140
packets the measurement is coarse: 1% random loss is flagged in about three
of four runs, 2% in nineteen of twenty. (The default `--latency-duration`
is 12 s for that; with 10 s and the second after hole punching left out it
would be two of three. Beyond 200 packets one lost packet is less than 0.5%,
so a longer phase only helps much from about 400 packets, about 30 s, on.)
Loss of packets the peer sends is not measured (`download_loss`).

### `keygen`, `directory`

`keygen --json` prints `{"event":"keygen", "node_id", "key_file"}`;
`directory --json` prints `{"event":"directory", "url"}` once it listens.

### `stats` (listen, directory)

With `--stats-interval D`, `listen` and `directory` print one `stats` event
every `D`, for soak tests (`harness/soak/`): the resources and internal state
of a long-running process, so that growth over time shows.

| field | type | meaning |
|---|---|---|
| `uptime_s` | number | seconds since the stats task started |
| `proc` | object | this process (Linux, from `/proc/self`; fields null elsewhere): `rss_kb` resident memory, `hwm_kb` its peak, `anon_kb` anonymous resident memory (mostly heap), `threads`, `fds` open file descriptors, `sockets` how many of them are sockets, `cpu_s` user plus system CPU time |
| `tokio` | object | the async runtime: `workers` threads, `alive_tasks` tasks spawned and not finished, `global_queue` tasks waiting in the global queue |
| `endpoint` | object | `listen` only: the endpoint's state, see below |
| `store` | object | `directory` only: `identities` and `endpoints` in its record store (at most 100 000 each; expired records are pruned every minute) |

`endpoint` has the library's `EndpointStats` and a few of iroh's counters.
Sizes, which return to their idle values when the load ends:
`open_connections` (not closed), `connections` (the connection table, also
closed connections not processed yet), `connect_slots` and `dial_locks` (connects and
dials in progress; the last one is removed lazily), `handshakes` (incoming
handshakes in progress), `queued_streams` (streams waiting for the
application), `cached_identities` and `cached_endpoints` (the peer record
cache, at most 4096 each). Totals since start: `connections_outgoing`,
`connections_incoming` (completed handshakes), `connections_closed`,
`handshakes_failed` (incoming connections refused or failed in the
handshake), `streams_accepted` (requests handed to the application),
`streams_refused` (reset before: bad header, unknown identity, sender not
bound, queue full). `iroh`: iroh's socket counters `conns_opened`,
`conns_closed`, `conns_direct`, `paths_direct`, `paths_relay`,
`holepunch_attempts`, `ip_paths_added`, `ip_paths_removed`,
`relay_paths_added`, `relay_paths_removed`, `relay_conns_success`,
`relay_conns_failed`, `relay_conns_closed`.

### `soak_stats` (soak)

`soak` sends one echo request per `--interval` to a `listen` node (one at a
time), optionally an upload of `--bulk-size` every `--bulk-every`, and prints
a `soak_stats` event every `--stats-interval` and a last one when it ends
(after `--duration`, or on SIGINT/SIGTERM). Counts are per window (since the
previous `soak_stats`) unless they are in `totals`.

| field | type | meaning |
|---|---|---|
| `to` | string | the listener's node ID |
| `window_ms` | number | length of the window |
| `sent`, `ok`, `failed` | number | echo requests finished in the window (a request still running when `soak` ends is not counted) |
| `new_conns` | number | requests that needed a new connection (lookup, dial, handshake) |
| `direct_fraction` | number or null | fraction of the successful requests over a direct path |
| `errors` | object | error kind (as in `error` events, plus `mismatch` and `bulk`) to count |
| `rtt_ms` | object | `p50`, `p90`, `p99`, `max`, `mean` of the round-trip times of successful requests on an existing connection (null without any) |
| `rtt_hist` | object | the same times as a histogram: bucket upper bound in ms (`0.1`, `0.2`, `0.3`, `0.5`, `0.75`, `1`, `1.5`, `2`, `3`, `5`, `7.5`, `10`, `15`, `20`, `30`, `50`, `75`, `100`, `150`, `200`, `300`, `500`, `750`, `1000`, `2000`, `5000`, `10000`, `inf`) to count, only buckets with samples; sums of histograms of many clients give percentiles over all of them |
| `bulk` | object | uploads finished in the window: `ok`, `failed`, `bytes` (of the successful ones), `mbps` (their throughput, null without any) |
| `totals` | object | since start: `sent`, `ok`, `failed`, `new_conns`, `bulk_ok`, `bulk_failed`, `bulk_bytes` |
| `path` | string | path of the connection to `to` now |
| `session` | object | `uptime_s`; `bind_ms` (start to `ready`); `first_ok_ms` (start to the first successful reply, null before) |
| `proc`, `tokio`, `endpoint` | object | as in `stats` |
| `final` | bool | the last event of the run |
| `interrupted` | bool | ended by a signal (not by `--duration`) |

A failed echo request or upload also prints an `error` event (`kind` as in
`errors`, `elapsed_ms`).

## Exit codes

| code | `probe` | `bench` | other commands |
|---|---|---|---|
| 0 | at least one correct reply, and the bulk transfer (if requested) succeeded | `healthy`: no issue found (with `--to`: connected, and a direct path was used) | success |
| 1 | no correct reply, or setup failed | setup failed (before `ready`) | failure |
| 2 | invalid arguments (from the argument parser, before any event) | same | same |
| 3 | correct replies, but the bulk transfer failed | `no_connection`: could not connect to `--to` | - |
| 4 | - | `relay_only`: connected, but never over a direct path | - |
| 5 | - | `degraded`: connected over a direct path (or no `--to`), but with issues | - |

For `bench`, 3 wins over 4, and 4 over 5: a relay-only connection with
failures exits with 4, and its issues are in the report.

`soak` exits with 0 if every request and upload succeeded, 1 if no request
succeeded (or setup failed), 2 for invalid arguments and 3 if some failed.

SIGINT and SIGTERM stop `listen` and `directory` cleanly (exit 0), end a
`probe` early with a `summary` and a `bench` early with a `bench_report`
(`interrupted: true`; the request in flight then is not counted as an issue). `listen --duration D`
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

## Probe and the listener's wire usage

`probe` sends one request per `--interval` (default 1 s) for `--duration`
(default 30 s), each `--payload-size` bytes (default 64, at most 1 MiB): the
ASCII marker `probe seq=N ` padded with `x`. The listener echoes it; the
reply is correct if it ends with the payload. Requests are sequential: a slow
request delays the next one (missed intervals are skipped, not bursted).
`--timeout` (default 10 s) applies to connecting and to each request; a
request still running at the end of `--duration` gets at most one more
`--timeout`. `bench` measures latency the same way.

Every request is one bidirectional stream. The listener tells three kinds
apart by their first bytes:

* **Echo**: anything else, at most 1 MiB. The reply is
  `endpoint=... instance=... to=... from=... echo=<request>`. `send` and the
  latency requests use it; it works with every listener version.
* **Sink (upload)**: the 21-byte header `golem-p2p-cli/sink/1\n`, then any
  number of bytes, then the client finishes the stream. The listener counts
  and discards the bytes and replies
  `endpoint=... instance=... to=... from=... sink_bytes=N`. Used by
  `probe --bulk N` (after at least one correct reply, bounded by
  `--bulk-timeout`, default 60 s) and by `bench`. A client that stops
  uploading at its time limit (`bench --transfer-time`) resets its sending
  side with code 257 (0x101) instead of finishing; the listener then replies
  with the count of what arrived before the reset. (A listener older than
  this answers nothing; `bench` then reports that the listener did not
  report what it received.)
* **Source (download)**: the 23-byte header `golem-p2p-cli/source/1\n`, then
  the byte count N in decimal ASCII (optionally followed by `\n`), then the
  client finishes the stream. The listener replies with the status line
  `ok bytes=N\n` followed by N bytes, and finishes. The client may stop
  reading early (`bench --transfer-time`). A listener older than this
  protocol treats the request as an echo; `bench` then reports that the
  listener does not support downloads.

**Listener limits.** Uploads and downloads are bounded on the listener, so a
public bench node cannot be abused:

| option | default | effect |
|---|---|---|
| `--max-transfer-size` | `100M` | largest upload or download per stream. A larger upload is stopped as soon as it passes the limit; a larger download is refused before any data is sent |
| `--max-transfers` | 8 | uploads and downloads served at once, over all peers; more are refused as busy. 0 serves echo requests only |
| `--max-transfer-time` | `120s` | longest time per upload or download; then the upload is stopped, or the download stream is reset with code 256 |

Echo requests stay limited to 1 MiB each and must arrive (be finished by the
client) within 10 s, as must the header of an upload or download request;
otherwise the listener resets the stream with code 256 and prints an `error`
of kind `timeout`. The library limits streams per connection (256) and
connections (1024). There is no per-peer rate limit.

A refusal is the reply (for a download: the status line)
`refused reason=too_large max_bytes=M`, `refused reason=busy
max_transfers=K`, `refused reason=timeout max_seconds=T` or
`refused reason=bad_request`. For an upload the listener also stops the
stream's receive side with code 256 (0x100), so the uploader's writes fail at
once and it then reads the reason. The listener prints a `request` event with
`refused` set.

## Bench phases

`bench` runs these phases, each bounded in size and time (defaults in
brackets):

1. `local`: the relay, directory and net report checks, in parallel (at most
   about 6 s).
2. `connect`: directory lookup, QUIC/TLS and golem-p2p handshakes
   (`--timeout`, 10 s).
3. `latency`: echo requests of `--payload-size` (64 B) every `--interval`
   (200 ms) for `--latency-duration` (12 s). Packet loss is counted from
   when the selected path has settled (see **Packet loss**).
4. `upload`, `download`: `--transfer-size` (10 MB) each, stopped at
   `--transfer-time` (8 s, from opening the stream; `limited_by: "time"`). An
   upload stopped there drops what is not delivered yet (it resets the
   stream) and the listener reports what arrived. The stop reaches the
   listener only after what is still queued on the path, which on a slow
   path takes seconds, so the bench waits for that count at least 3 s, as
   long as the upload ran (8 s), or three round trips (QUIC's smoothed RTT,
   which includes queueing), whichever is longest, at most 30 s; without it
   the upload fails with `error_kind` `no_count`. An upload written
   completely before the time limit is not stopped (`limited_by: "size"`):
   what the stream buffered (up to QUIC's stream window, 1.25 MB) is still
   on its way, and the listener counts it once it has arrived, which on a
   slow path can be after the time limit; the bench waits for that count
   until the time limit, or, if that ends later, as long as it would after a
   stop, counted from the end of writing (`no_count` if it does not come
   either). A download stops reading (one whose bytes had all arrived counts
   as complete, also if the end of the stream had not). Throughput is the
   bytes over the time from opening the stream to the listener's count (upload) or
   to the last byte (download). `second_half_mbps` is the throughput in the
   second half of that time (of a download: of what arrived; of an upload:
   of what the stream took while writing, which flow control keeps at most
   one stream window ahead of what has arrived, so after the first 1.25 MB
   it follows the arrival rate, give or take a window update of about
   160 kB). Null for transfers under 1 s.
5. `concurrency`: `--streams` (8) workers send echo requests of 256 KiB until
   `--concurrency-size` (8 MB of requests) or `--transfer-time`; requests
   still running at the time limit are cut off (`unfinished`) and not
   counted. On a slow link the requests are smaller (`sized_for_mbps`), so
   that each stream can finish about four within `--transfer-time` at what
   the transfers just measured sustained: the slower of upload and download,
   each the lower of its `throughput_mbps` and `second_half_mbps` (whole KiB,
   at least 4 KiB; below about 8 Mbps with the defaults). With 256 KiB a
   1-4 Mbps link finished none and gave no throughput (campaign 2); sized by
   the averages alone, 2 of 13 runs on a link with 1 % loss each way still
   finished none, because a transfer there can start fast (no loss early in
   slow start) and settle at 1-2 Mbps, and its average (4.7 and 6.7 Mbps)
   overstated the link (campaign 3). Transfers under 1 s (10 MB at over
   80 Mbps) are judged by their average, so fast paths keep 256 KiB.
   If none of the requests has finished by half of `--transfer-time` (they
   may still be too large: the transfers overstated the link, or it slowed
   down), the phase starts over (`restart`), unless the requests in flight
   are about to finish: at the pace the connection carried bytes (both
   ways, QUIC's UDP counters) from a quarter to half of `--transfer-time`,
   the bytes they still need (each request and its echo, less what the
   connection carried since the start of the phase) would be through by
   three quarters of it (6 s with the defaults). The recent pace, not the
   average: an upload that went through at once and echoes that come back
   slowly move at the echoes' pace. When it starts over, the requests in
   flight are cut off (and cancelled on the wire, see below), and for the
   rest of the time the workers send requests a sixteenth of the size
   (whole KiB, at least 4 KiB; from 256 KiB that is 16 KiB, of which 8
   streams finish a round trip in the remaining 4 s down to about
   0.5 Mbps). Only those are counted, from the restart on. The budget
   (`--concurrency-size`) includes the cut-off requests. On fast paths the
   first request finishes in milliseconds, so this does not happen there.
   A phase that went on instead has no `restart` and a `first_reply_ms`
   over half of `--transfer-time`. (Up to campaign 4 every phase without a
   reply by half the time started over; in one lossy run that cut off
   requests sized only ~2x too large, 80 % of whose echoes had arrived.)
   Requests cut off by a restart or the time limit (and the last small
   request) are cancelled on the wire: golem-p2p resets their streams
   (`CANCELLED`), so QUIC stops sending them and the listener reads an
   error instead of the request, or, if it had received the whole request
   already, fails to send the echo. (Up to campaign 4 only requests not yet
   written completely were reset, and the rest kept using the link after
   the cut: after a restart they took most of a lossy uplink, and they
   reached the listener up to 3.9 s after the phase.)
   Meanwhile small requests every `--interval` measure the round trip under
   load (over the whole phase). The phase ends when the last worker does (a
   small request still running then is dropped); its duration is taken
   then. (Up to campaign 3 it ended only when the small requests next
   noticed, so on fast paths its duration snapped to their 200 ms ticks:
   every lab direct run reported 7.5 MiB in about 403 ms, 156 Mbps.)
6. `relay_compare` (`--compare-relay`): a second endpoint with a temporary
   identity, relay only (no UDP sockets) and not published to the
   directories, connects to the same peer and repeats latency (at most 5 s),
   upload and download. It needs its own endpoint because golem-p2p keeps one
   connection per peer, and iroh would add a direct path to it.
7. `idle` (`--idle D`): no traffic for D, then up to 3 requests.

Every phase that runs over a relay (the main connection while it is relayed,
and `--compare-relay`) caps its size at `--relay-transfer-size` (4 MB), to
spare shared relays. With the defaults a run moves at most about 36 MB (10 MB
up, 10 MB down, 8 MB of parallel requests each way) plus 8 MB with
`--compare-relay`.

How long a run takes, with the defaults: `local` up to about 6 s (usually
under 1.5 s); `connect` up to 10 s (usually under 1 s); `latency` 12 s, plus
at most one `--timeout` for a request still running at its end (usually one
round trip); `upload` at most 8 s plus the wait for the listener's count
(usually one round trip; up to 8 s on a slow path, at most 30 s);
`download` and `concurrency` at most 8 s each. On a fast link the transfers
finish early and a run takes about 17 s; on a slow or lossy link each
transfer phase uses its 8 s and a run takes about 45 s. `--compare-relay`
adds a second endpoint and connection (about 1 to 2 s, at most 15 s), 5 s of
latency, and a relayed upload and download bounded the same way: about 8 s on
a fast relay path, up to about 35 s on a slow one. `--idle D` adds D and up to
3 requests.
