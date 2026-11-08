# Soak tests, local, 2026-11-08

Long-running, multi-client tests of golem-p2p on the build machine (Hetzner,
32 cores, shared: load average 4 to 39 from other tenants during the runs),
looking for leaks, unbounded growth, slowdowns, fd/task leaks and failures
under churn in the library, the CLI's `listen` and `directory`, and
iroh-relay 1.4.1.

**Bottom line.** No leak in golem-p2p's own state: after every run and
library test the connection table, connect slots, dial locks, handshakes,
queued streams, tokio tasks, threads and file descriptors were back at their
idle values; 2.1 million requests and 23 600 churn sessions without a failed
request or session; no slowdown with time or with connections served. What
grows: (1) iroh keeps two hash-map entries per remote endpoint ID forever
(0.1 to 0.2 KiB each, confirmed in a 50 000-session test and as a 3.4 MB step
in the churn listener at the map's resize); (2) resident memory follows the
peak, not the load: glibc keeps freed memory (`MALLOC_ARENA_MAX=2` halves
the creep); (3) bounded caches fill up (peer record cache, relay key cache
of 1 M keys by default). Plus a policy issue found on the way: a full
record store evicts the records that expire soonest, which a flood of
long-lived records exploits.

## Setup

* Code: the soak branch on `main` of 2026-11-08 (`0ad511d` for the runs,
  rebased onto `f820525` afterwards); `golem-p2p` release build;
  `iroh-relay` 1.4.1 from crates.io (`cargo install --features server`).
* Everything local: one relay (self-signed certificate, QUIC address
  discovery on), one directory and one listener per run on `127.0.0.1`,
  clients as separate processes (`golem-p2p soak`, 2 tokio worker threads
  each). All runs in the systemd user slice `golem-soak.slice`
  (`CPUQuota=600%`, `MemoryMax=6G`); they used 0.3 to 2 cores and at most
  2.9 GB together.
* Tooling: `harness/soak/` (`harness/README.md`, "Soak tests"); library
  tests in `crates/golem-p2p/tests/soak.rs` (`--ignored`).
* The runs overlapped (same slice, plus the library tests), so latencies
  include each other's load and the host's. Heap figures are glibc's
  `mallinfo2` (`heap_used_kb`: allocated and not freed; `heap_free_kb`:
  freed, kept by the allocator); the relay is not our binary, so only its
  resident memory is known.

| run | load | duration |
|---|---|---|
| `steady` | 100 clients over direct paths: a 256-byte echo per second each, a 1 MB upload every 10 min each | 150 min, 2 min idle before, 3 min cool-down after |
| `relay` | 50 clients, clients and listener `--relay-only`: everything through the relay | 150 min |
| `churn` | 6 workers starting `soak` sessions back to back (3 s, a request per 200 ms); 3 with a fresh identity per session, 3 cycling through 4 keys each; a new iroh endpoint key every session | 150 min, 16 322 sessions |
| `churn-relay` | the same, relay only, 4 workers | 100 min, 7 299 sessions |
| `steady-arena2` | `steady` with `MALLOC_ARENA_MAX=2` for every process | 70 min |
| `churn-relay-fixed` | `churn-relay` after the soak client's fix (below) | 20 min |

Files: per run `summary.json`, `table.md` (every series: idle, after the
15-minute warm-up, peak, end of load, after cool-down, slope, R^2, flags),
`plot/*.csv` (5-minute bins and per-sample series), gzipped raw data
(`samples.jsonl.gz`, `listener.jsonl.gz`, `directory.jsonl.gz`,
`clients/*.jsonl.gz` or `churn/*.jsonl.gz`, `relay.log.gz`); `lib/` the
library tests' output; `summary.json` here the key facts of all runs and
the library tests' checkpoints.

## Results of the CLI runs

Resident memory in MB (idle / after warm-up / end of load / after
cool-down), CPU as a share of one core during the load:

| run | listener RSS | listener heap in use | directory RSS | relay RSS | client RSS (largest) |
|---|---|---|---|---|---|
| steady | 19.9 / 34.2 / 51.1 / 51.1, 7 % CPU | 4.0 / 17.9 / 21.0 / **6.4** | 9.4 / 11.4 / 12.6 / 12.6 | 10.2 / 25.3 / 30.3 / 30.2, 1.4 % | 21.3 -> 27.4 |
| relay | 19.4 / 28.3 / 43.2 / 43.2, 4 % | 1.0 / 8.7 / 11.2 / **2.7** | 9.4 / 10.5 / 11.0 / 11.0 | 9.6 / 12.4 / 15.9 / 14.9, 2.6 % | 20.9 -> 22.1 |
| churn | 20.0 / 28.2 / 47.6 / 47.6, 3 % | 4.0 / 9.8 / 19.4 / 17.4 | 9.5 / 11.4 / 16.7 / 16.7 | 10.3 / 14.0 / 17.5 / 17.5, 1 % | - |
| churn-relay | 19.5 / 26.3 / 36.5 / 36.6, 2 % | 1.0 / 5.3 / 11.7 / 10.4 | 9.4 / 10.7 / 13.7 / 13.7 | 9.6 / 10.9 / 12.6 / 12.6, 1.5 % | - |
| steady-arena2 | 19.9 / 32.3 / 37.8 / 37.9, 8 % | 4.0 / 17.9 / 20.1 / 6.2 | 9.3 / 11.4 / 12.0 / 12.0 | 10.4 / 23.7 / 26.4 / 26.4, 1.6 % | 21.9 -> 24.9 |

* **Cost of a connection.** Listener heap in use 143 KiB per open
  connection (direct, 100 connections) and 160 KiB (relay only, 50); it
  grows over the connection's life with the uploads (to 174 KiB after 15
  uploads of 1 MB, levelling off) and is freed when the connection closes
  (heap in use 21.0 -> 6.4 MB after the cool-down). The relay holds
  89 KiB per homed client, and 171 KiB per client that also does QUIC
  address discovery (QAD keeps a second QUIC connection per client open on
  the relay, replaced every ~23 s: 39 514 QAD connections in `steady`). A
  client process: 20 to 22 MB resident (mostly the shared binary), heap in
  use 4.2 MB (direct; 1.1 MB relay only).
* **Tables and tasks** (listener `stats` events): connections 100 -> 0,
  tasks 716 -> 15 (idle 16), connect slots, dial locks, handshakes, queued
  streams 0 throughout, fds and threads flat in every process (relay sockets
  108 -> 8 after the cool-down). 7 tasks per open connection on the
  listener (4 golem-p2p, 1 CLI path watcher, iroh's).
* **Requests.** `steady` 901 525, `relay` 450 580, `churn` 244 744,
  `churn-relay` 109 415 requests: **0 failed**. Uploads: 1 500 + 747 ok, 3
  refused as busy (relay run, see the stall below: the listener's
  `--max-transfers 32` working as designed). Churn: 16 322 + 7 299 sessions,
  every one exit code 0.
* **Latency** (histograms merged over all clients per 5 minutes): steady
  p50 0.3 to 1 ms, p99 1 to 5 ms; relay only p50 0.5 to 1.5 ms, p99 1.5 to
  15 ms; no trend over 150 min (fitted p50 +0.03 ms/h, p99 -0.4 ms/h). One
  stall at 02:32 UTC in every run at once: the host's load average jumped
  from 27 to 39 (other tenants) during a round of uploads; relay-only p99
  was 200 ms in that bin, one client's p99 5.4 s, the uploads ran at 3 Mbps
  instead of ~450, and three were refused as busy. Not repeated.
* **Churn timings.** Bind p50 297 ms (p99 358), first reply p50 404 ms (p99
  520), flat over 16 322 sessions (trend -2 ms/h, R^2 0.02); relay only 276
  / 383 ms. No slowdown with the number of connections served.
* **Directory.** Store size follows the records of the last hour (pruned
  every minute): 7 100 endpoints and 3 550 identities in `churn` from minute
  60 on, heap in use flat at 6.1 MB (about 0.9 KiB per stored record), RSS flat
  after an hour. Steady load: 0.4 MB heap.
* **Listener errors.** None in `steady` and `relay`. Churn: 109 requests
  cancelled and 16 "connection lost" (churn-relay: 24 and 41): clients
  exiting with a request in flight. That was the soak client aborting its
  last request (fixed, see below); `churn-relay-fixed` (the fixed client, 20 min, 1 468 sessions, 22 026
  requests) had none, where the same rate before gave 65 in 7 299 sessions.

## Findings

### 1. iroh keeps two map entries per remote endpoint forever (upstream; slow, unbounded)

iroh 1.4.1, `socket/remote_map.rs`: `MappedAddrs::endpoint_addrs`
(`AddrMap<EndpointId, EndpointIdMappedAddr>`) gets an entry when the first
`RemoteStateActor` of a remote endpoint starts, `relay_addrs`
(`AddrMap<(RelayUrl, EndpointId), RelayMappedAddr>`) one per relay path to
it (`to_mapped_tuple`). Each `AddrMap` is two `FxHashMap`s (forward and
reverse) without removal: the actor stops after 60 s idle and its sender is
removed, but the mapped addresses stay for the endpoint's lifetime. With
golem-p2p's fresh endpoint key on every start (design section 10), every
restart of every peer adds entries on every node it talked to.

Measured: in the library test, from 10 000 to 50 000 client sessions (record
cache full in both) the heap in use grew by 5.9 MB, 0.15 KiB per session,
never released; by the types' sizes, the two `endpoint_addrs` maps going
from 16 384 to 65 536 buckets account for 4.8 MB of it. In the `churn`
listener the heap in use stepped up by 3.4 MB at 03:13 UTC, within the one
minute in which the number of remote endpoints crossed 14 336 (7/8 of 16 384
buckets: hashbrown's resize), and was flat for 50 minutes before. Amortized
0.1 to 0.2 KiB per remote endpoint (twice that with relay paths): a node
that sees 10 000 peer restarts a day grows 1 to 4 MB a day. Not fixable in
golem-p2p without patching iroh (iroh would have to drop the mapped
addresses once no connection or path to the endpoint is left). Worth an
upstream issue; peers that keep their endpoint key (`Builder::secret_key`)
add no entries when they restart.

### 2. iroh keeps 17 KiB per remote endpoint for 60 s after its last connection (transient)

Every remote endpoint keeps a `RemoteStateActor` (one task) for 60 s after
its last connection (`ACTOR_MAX_IDLE_TIMEOUT`). Library test: 16.8 KiB of
heap per session while inside that window, all released after it (tasks
back to idle). A node accepting N new remote endpoints per minute holds
N x 17 KiB (1 000 per minute: 17 MB). Bounded by the churn rate.

### 3. Resident memory follows the peak, not the load (glibc)

Freed memory stays with the process: after 2 000 sessions and the idle
minute the library test's heap in use was within 4.3 MB of the start, but
resident memory stayed 56 MB higher. In the `steady` listener resident
memory rose 34 -> 51 MB during the load while the heap in use rose only
18 -> 21 MB; the free heap kept by glibc went 5 -> 21 MB (35 MB after the
connections closed, resident memory unchanged). The clients' resident memory
rose by about 1 MB per upload round (the largest client 21 -> 27 MB, all
100 together 612 -> 1191 MB anonymous), with their heap in use flat
(median +0.12 MB/h). With `MALLOC_ARENA_MAX=2` (`steady-arena2`, same
times into the load, 15 -> 70 min): listener +5.6 MB instead of +11.6 MB,
relay +3.0 instead of +4.4 MB, clients +284 instead of +462 MB, and the
listener's free heap stayed at 1.4 to 4.6 MB instead of 5 to 21 MB. Not a
leak, but operators will see resident memory that only goes up after bursts.
Options for yagna: `MALLOC_ARENA_MAX=2`, a periodic `malloc_trim`, or
jemalloc/mimalloc.

### 4. Bounded caches fill up and stay full

* The endpoint's peer record cache (4 096 identities and 4 096 endpoints)
  drops expired records only when it is full. The `churn` listener reached
  4 096 endpoints after 40 minutes and then oscillated between 3 540 and
  4 096 identities as expired records were dropped in bulk; about 2 KiB per
  record in the library test, so up to 8 to 9 MB per endpoint. Bounded;
  pruning expired records on the 20-minute re-sign tick would make it follow
  the peers of the last hour instead.
* iroh-relay's key cache holds up to 1 048 576 client keys by default
  (`key_cache_capacity`). Each new endpoint key (every golem-p2p start) adds
  one: the `churn` relay saw 16 304 keys and grew 6.6 -> 10.0 MB anonymous
  (about 0.2 KiB per key, still growing at the end). At the default cap that
  is up to ~200 MB; set `key_cache_capacity` (e.g. 100 000) on public relays.
* The relay itself levels off: `steady` relay 25.3 -> 30.3 MB in the first
  hour, then flat (22.6 -> 23.5 MB anonymous in the last 90 minutes);
  `relay` (1.2 GB relayed) flat after the first hour.

### 5. A full record store evicts what expires soonest, which publishers choose (policy)

When a `RecordStore` is full it evicts the records that expire soonest. The
expiry is chosen by the publisher (up to 24 h); honest nodes use 1 h and
republish every 20 minutes. Found when the library soak test's shared
discovery store (4 096 records) evicted the server's own record after 4 096
client sessions, because it had been published first: every later client
failed with "no valid record found". In a directory (100 000 records), a
publisher with many identities (free to generate) and 24-hour records fills
the store once; from then on every honest insert evicts other honest
records (they all expire sooner): honest nodes cannot be looked up for a
day, without a sustained flood. Evicting the least recently inserted or
updated records would let a one-time flood age out as honest nodes
republish; no eviction policy replaces per-client rate limits (design
section 14 lists them as not done). A decision for the owner, not changed.

### 6. Smaller things

* iroh logs `ERROR iroh::socket::transports::relay: relay_recv_channel closed`
  on every normal close of an endpoint with a relay (2 000 lines in the
  relay library test): noise that looks like a failure in production logs.
* The soak client aborted the request in flight when it ended, which the
  listener reported as errors (fixed in the soak client, below).

## Library soak tests (one process, `crates/golem-p2p/tests/soak.rs`)

"Retained" is the heap in use 65 s after the last session (iroh's
per-remote state expired) minus before the first, per session.

| test | load | result |
|---|---|---|
| `soak_server_direct` | 2 000 client sessions (new identity and endpoint key each, 5 requests) against one server, 8 at a time | 16.8 KiB heap per session while inside iroh's 60 s window; after it tasks 19 -> 8, fds 12 -> 12, tables 0, heap in use 4.7 -> 9.0 MB: 2.2 KiB retained per session (mostly the record cache); resident memory 80 MB (56 MB free heap kept by glibc) |
| same | 10 000 sessions | heap in use 4.7 -> 13.6 MB after idle (0.9 KiB per session; record cache full at 4 063) |
| same | 50 000 sessions (41 min) | 4.7 -> 19.5 MB (0.3 KiB per session); 10 000 -> 50 000: +5.9 MB, **0.15 KiB per session that stays** (finding 1). Session rate 35/s at first and 18 to 20/s later; a fresh process at the same host load also did 20/s, so that was the host, not the process |
| `soak_server_relay` | 2 000 sessions relay only, in-process relay | 10.4 KiB per session in the window, 3.0 KiB retained (includes the relay server) |
| `soak_client_many_peers` | one client, 3 000 short-lived servers | 12.3 KiB per peer in the window, 1.3 KiB retained (3 008 cached records); connect slots and dial locks 1 (the last one, removed lazily) |
| `soak_requests_one_connection` | 500 000 echo requests, 16 at a time, one connection | heap 7.69 -> 7.79 MB: **0.2 B per request**, tasks 27 -> 28 |
| `soak_large_requests_one_connection` | 2 000 x (1 MB up, 1 MB back), one connection | direct -0.25, with a relay path +0.24, relay only +0.08 KiB per transfer (noise of +-0.3 MB) |
| `soak_idle_nodes_qad` / `_no_qad` | 20 nodes + hub + relay, a request per node every 5 s, 20 min | +12 to 14 KiB per minute for the whole process after a 5-minute warm-up, with or without QAD; at minute 20 all nodes re-signed their records and resident memory jumped by 15 MB of free heap |
| `soak_idle_nodes_qad`, glibc tcache off (exact heap in use) | 40 min | heap in use 87.07 MB at minute 8, 87.31 at 13, 87.16 at 38: **flat** |

`connections_are_released` and `failed_connects_are_released` (in the normal
test suite) check the same tables after 20 clients, 10 servers, 20 unknown
and one dead peer.

## Fixes made

* `soak` finished its run by aborting the request in flight; the listener
  then logged a `stream_rejected` (code 0) or `connection lost` error for
  it. The series now stops between requests and the last one is counted
  (commit "soak: finish the request in flight before exiting; CLI tests").
* The library soak test's server republishes every 2 000 sessions
  (finding 5 made the long runs fail otherwise).
* `summarize.py` counts requests the requester gave up on (CANCELLED,
  TIMED_OUT, and the old code 0) as `cancelled`, not as listener failures.

No library bug was found that needed a fix: everything golem-p2p keeps per
connection, per stream and per connect attempt was released.

## Running it elsewhere

`harness/soak/soak.sh run steady|relay|churn --out DIR` does everything on
one host. For the remote soak: `soak.sh services` on the relay VM (with
`SOAK_HOST_IP=0.0.0.0 SOAK_PUBLIC_IP=<VM address>`, `SOAK_RELAY*` for the
VM's own relay, `SOAK_RELAY_MATCH=iroh-relay` so the sampler follows it),
`soak.sh clients` / `churn` and `sample` on each client host with the same
run directory name, `soak.sh stop` everywhere, then `summarize.py` over the
merged directory; details in `harness/README.md`. Suggested additions for a
longer remote soak: a relay with `key_cache_capacity` set, one run with
`MALLOC_ARENA_MAX=2`, and a day-long `steady` run to see whether the
per-connection heap growth during uploads (finding 3, 143 -> 174 KiB)
levels off.
