# NAT lab: first results

Later: benchmark campaign 1 with `bench --json` (lab matrix, real world with
QAD on the real relay, relay-only load, startup timings) is in
[../results/2026-11-07-campaign1/](../results/2026-11-07-campaign1/)
(`summary.json`, `table.md`, `notes.json`).

Date: 2026-11-07. Code: branch `harness-lab` on top of `always-encrypted`
(`1fccda6`). The CLI is the one on that commit (no `--json`, no `probe`, no
relay QAD-port option). iroh 1.4.1; iroh-relay 1.4.1 is the musl release
binary from p2p-relay1 (sha256 `9b9804c7…ac71b3`).

Lab VM: p2p-natlab, Ubuntu 26.04, kernel 7.0.0-43-generic, nftables 1.1.6,
Python 3.14, 8 vCPU. The CLI was built on the Hetzner server (Ubuntu 26.04,
glibc 2.43; the binary needs glibc ≥ 2.34).

Raw data: `results/2026-11-07-*.txt` (one summary line per run, fields in
`tools/summarize.py`). Per-run logs (sender output with timestamps, sender
stderr, listener log) stayed on the VM in `~/gpl-runs/` and on the Hetzner
server in `~/gpl-runs/realworld-*`.

## Method

* Topology `topologies/matrix.topo`: 11 nodes, every access link 10 ms each
  way, the relay/directory link another 10 ms each way. So a direct round trip
  between two nodes is 40 ms and a relayed one 80 ms, and the path can be told
  from the request times alone.
* One run = `lab.sh pair <listener> <sender> --duration 30`: `golem-p2p
  listen` in one node, then `golem-p2p send --count 100000000` in the other,
  stopped with SIGTERM after 30 s. Every request is one QUIC stream; `send`
  prints the selected path after each request. About 25 requests/s direct,
  12/s relayed.
* Each pair ran 3 times, rounds interleaved (all pairs, then all pairs again),
  with fresh processes (new iroh endpoint keys and ports) every run.
* Column meanings: **connected** runs with at least one reply; **final path**
  path after the last request in each run (d = direct, r = relay);
  **t_direct** seconds from the sender's process start to the first reply on a
  direct path (includes ~0.6 s of `bind`); **first request** duration of the
  first request (directory lookup, connect, golem-p2p handshake, one request);
  **RTT** median request time per path.

## Selftest: NAT profiles as measured

`lab.sh selftest`: every profile behaved as designed in every run, 9 runs in
total (topology `selftest` 7 times with 8/8, `matrix` once with 11/11, `real`
once with 1/1 for its lab node).

| profile | mapping | filtering | port preserved | unmapped port open | UDP | TCP |
|---|---|---|---|---|---|---|
| public | none (reflector sees the node's own ip:port) | open (all 3 unsolicited sources get in) | yes | yes | yes | yes |
| easy | EIM | EIF | yes | no | yes | yes |
| restricted | EIM | ADF (same IP, other port gets in; other IPs don't) | yes | no | yes | yes |
| port-restricted | EIM | APDF | yes | no | yes | yes |
| symmetric | APDM (a new random port toward every destination, also another port on the same IP) | APDF | no | no | yes | yes |
| udp-blocked | - | - | - | - | no | yes |
| double | EIM | EIF | yes | no | yes | yes |
| cgnat | EIM | APDF | yes | no | yes | yes |

Example raw `natcheck` output (symmetric): local `192.168.5.2:59701`, mapped
`198.18.5.2:64329`, `198.18.5.2:44034`, `198.18.5.2:12646` toward
`.2:3478`, `.2:3479`, `.3:3478`; conntrack on the router shows one entry per
destination with a different reply port.

## Experiment 1: pairs inside the lab (local relay + directory)

54 runs, 3 per pair, all connected; no errors.

| listener | sender | connected | final path | t_direct s | first request ms | direct % | RTT direct/relay ms |
|---|---|---|---|---|---|---|---|
| pub1 (public) | easy1 (easy) | 3/3 | ddd | 0.79 | 171 | 100 | 40/- |
| easy1 (easy) | pub1 (public) | 3/3 | ddd | 0.79 | 170 | 100 | 40/- |
| easy1 (easy) | easy2 (easy) | 3/3 | ddd | 0.79 | 170 | 100 | 40/- |
| easy1 (easy) | sym1 (symmetric) | 3/3 | ddd | 0.79 | 170 | 100 | 40/- |
| sym1 (symmetric) | easy1 (easy) | 3/3 | ddd | 0.97 | 298 | 100 | 40/- |
| pr1 (port-restricted) | pr2 (port-restricted) | 3/3 | ddd | 0.91 | 297 | 100 | 40/- |
| sym1 (symmetric) | sym2 (symmetric) | 3/3 | rrr | - | 322 | 0 | -/80 |
| udpb1 (udp-blocked) | easy1 (easy) | 3/3 | rrr | - | 297 | 0 | -/80 |
| easy1 (easy) | udpb1 (udp-blocked) | 3/3 | rrr | - | 298 | 0 | -/80 |
| cgn1 (cgnat) | easy1 (easy) | 3/3 | ddd | 0.91 | 297 | 100 | 40/- |
| easy1 (easy) | cgn1 (cgnat) | 3/3 | ddd | 0.79 | 170 | 100 | 40/- |
| dbl1 (double) | easy1 (easy) | 3/3 | ddd | 0.79 | 171 | 100 | 40/- |
| easy1 (easy) | dbl1 (double) | 3/3 | ddd | 0.79 | 170 | 100 | 40/- |
| pr1 (port-restricted) | sym1 (symmetric) | 3/3 | rrr | - | 300 | 0 | -/80 |
| sym1 (symmetric) | pr1 (port-restricted) | 3/3 | rrr | - | 297 | 0 | -/80 |
| rc1 (restricted) | sym1 (symmetric) | 3/3 | ddd | 0.94 | 323 | 100 | 40/- |
| pub1 (public) | sym1 (symmetric) | 3/3 | ddd | 0.79 | 170 | 100 | 40/- |
| sym1 (symmetric) | pub1 (public) | 3/3 | ddd | 0.97 | 297 | 100 | 40/- |

Expected vs observed: every row matches the textbook prediction.

* **Direct from the first request** (first request ≈ 170 ms, no relay seen
  by the listener) whenever the *listener* accepts unsolicited packets on its
  published address: public, easy (EIF), double (EIF twice). The sender
  dials the listener's published direct address and the relay in parallel, and
  the direct path wins at once.
* **Relay first, direct within ~0.05-0.2 s** when the listener filters (APDF or
  symmetric) but hole punching can work: sym1 listening for easy1/pub1, pr1↔pr2,
  cgn1 listening for easy1, rc1 (ADF) listening for sym1. The first request
  takes ≈ 300 ms (one relay round trip more), the listener sees exactly one
  request via the relay, everything after that is direct.
* **Relay only** (80 ms RTT for 30 s): symmetric↔symmetric,
  port-restricted↔symmetric (both directions), and udp-blocked↔easy (both
  directions). These are the cases where a peer-to-peer UDP path needs port
  prediction (no sign of it in iroh 1.4.1: no direct path within 30 s) or is
  impossible at all.
* **easy↔symmetric works** in both directions: the easy (EIF) side accepts
  the symmetric side's new random port, and iroh adopts that peer-reflexive
  address. **restricted↔symmetric works** too (ADF only checks the IP).
* Once direct, no run fell back to the relay (switches ≤ 1, direct 100 %).
* Extra spot check (single run): cgn1 listening for dbl1, direct from the first
  request.

## Experiment 1b: with packet loss and jitter

`lab.sh netem <node> delay=10ms jitter=5ms loss=5%` on easy1, easy2, sym1,
pr1, pr2 (while the lab was up), so 5 % loss per direction on each access link,
about 10 % per direction end to end, and reordering. 2 runs per pair.

| listener | sender | connected | final path | t_direct s | first request ms | direct % | RTT direct/relay ms |
|---|---|---|---|---|---|---|---|
| easy1 (easy) | easy2 (easy) | 2/2 | dd | 1.17 | 449 | 100 | 42/- |
| sym1 (symmetric) | easy1 (easy) | 2/2 | dd | 1.23 | 592 | 100 | 42/- |
| pr1 (port-restricted) | pr2 (port-restricted) | 2/2 | dd | 1.73 | 548 | 100 | 42/- |
| easy1 (easy) | sym1 (symmetric) | 2/2 | dd | 1.03 | 322 | 100 | 42/- |

Still direct in every run; the first request takes 0.3-0.7 s instead of
0.17-0.3 s, and the request rate halves (QUIC loss recovery), but the path
does not flap.

## Experiment 2: real world (lab node via the real uplink <-> public node in Helsinki)

Setup (`realworld.sh`, topology `real`):

* Lab nodes `real1` (easy) and `real2` (symmetric) with `uplink=real`: their
  packets leave through the emulated NAT, the VM's masquerade and the i3D
  host's masquerade, and appear as `213.163.75.157`.
* Public node: `golem-p2p` on the Hetzner server `65.109.154.126` (Helsinki).
  It has a public IP on its interface, but the server runs ufw with
  default-deny incoming, so unsolicited UDP to a random port is dropped:
  "public IP behind a stateful firewall". Variant B binds the node to UDP 8000,
  which an existing ufw rule already allows (nothing else used it at the time;
  the firewall was not changed), so it behaves like an open public node.
* Real relay `https://213.163.75.157:56000` with `--relay-ca p2p-relay1.crt`.
  The lab nodes reach it through the i3D host's hairpin NAT.
* Directory: a temporary `golem-p2p directory` on p2p-relay1, port 56050
  (forwarded), as a transient systemd unit with `RuntimeMaxSec`, stopped at
  the end.
* **Limitation of this CLI:** it cannot set the relay's QUIC address discovery
  port, so iroh probes `213.163.75.157:7842`, which does not answer (the relay
  serves QAD on 56001). Nodes therefore never learn their public address from
  the relay. Relaying works regardless. The lab nodes published only their LAN
  address (`addrs=[192.168.1.2:…]`), the Hetzner node its interface addresses.

### NAT behaviour of the real path

`realworld.sh --natcheck`: reflectors on p2p-relay1 (`213.163.75.157:56090`
and `:56091`, through the hairpin) and on the Hetzner server (`:8000`):

```
natcheck node=real1 try=1 filtering=APDF local=192.168.1.2:48653 mapped=213.163.75.157:48653,213.163.75.157:48653,213.163.75.157:48653 mapping=EIM port_preserved=yes unmapped_inbound=no unsolicited=-
natcheck node=real1 try=2 filtering=APDF local=192.168.1.2:50997 mapped=213.163.75.157:50997,213.163.75.157:50997,213.163.75.157:50997 mapping=EIM port_preserved=yes unmapped_inbound=no unsolicited=-
natcheck node=real1 try=3 filtering=APDF local=192.168.1.2:42792 mapped=213.163.75.157:42792,213.163.75.157:42792,213.163.75.157:42792 mapping=EIM port_preserved=yes unmapped_inbound=no unsolicited=-
natcheck node=real2 try=1 filtering=APDF local=192.168.2.2:41796 mapped=213.163.75.157:24221,213.163.75.157:25703,213.163.75.157:35729 mapping=APDM port_preserved=no unmapped_inbound=no unsolicited=-
natcheck node=real2 try=2 filtering=APDF local=192.168.2.2:36010 mapped=213.163.75.157:1985,213.163.75.157:4973,213.163.75.157:5164 mapping=APDM port_preserved=no unmapped_inbound=no unsolicited=-
natcheck node=real2 try=3 filtering=APDF local=192.168.2.2:51461 mapped=213.163.75.157:19764,213.163.75.157:8238,213.163.75.157:61592 mapping=APDM port_preserved=no unmapped_inbound=no unsolicited=-
```

* `easy` + VM + host: **EIM, port preserved through all three NATs** (the
  same `213.163.75.157:<node's own port>` toward the relay VM and toward
  Helsinki); filtering APDF (on the hairpin path).
* `symmetric` + VM + host: APDM; the random port picked by the emulated
  router survives both outer NATs unchanged. So the VM's and the host's
  masquerades are port-preserving and add APDF filtering only.

### Results

Variant A, public node behind ufw (random UDP port), 3 runs per direction
and node, 30 s each:

| listener | sender | connected | final path | t_direct s | first request ms | direct % | RTT direct/relay ms |
|---|---|---|---|---|---|---|---|
| local | real1 | 3/3 | rrr | - | 121 | 0 | -/37 |
| real1 | local | 3/3 | rrr | - | 170 | 0 | -/34 |
| local | real2 | 3/3 | rrr | - | 123 | 0 | -/37 |
| real2 | local | 3/3 | rrr | - | 165 | 0 | -/34 |

Variant B, public node on the ufw-allowed UDP port 8000:

| listener | sender | connected | final path | t_direct s | first request ms | direct % | RTT direct/relay ms |
|---|---|---|---|---|---|---|---|
| local | real1 | 3/3 | ddd | 3.13 | 110 | 100 | 33/34 |
| real1 | local | 3/3 | ddd | 3.43 | 168 | 100 | 36/34 |
| local | real2 | 3/3 | ddd | 3.14 | 112 | 100 | 36/34 |
| real2 | local | 3/3 | ddd | 3.42 | 175 | 100 | 36/36 |

* **A: relay only, in all 12 runs.** Neither side has a usable direct
  candidate. The lab node's packets to the Hetzner node's random port are
  dropped by ufw, which has no state for them yet, and the Hetzner node cannot
  create that state: the lab node publishes only `192.168.x.2` (no QAD), so it
  has no public address of the lab node to send to. With
  QAD on the right port the lab node would publish `213.163.75.157:<port>`
  (EIM, so it would be the right port), the Hetzner node's outgoing packet to
  it would open ufw's state, and hole punching should succeed; to be
  re-run once the CLI can set the QAD port.
* **B: direct in all 12 runs, about 0.07 s after the first reply**, for the
  easy and the symmetric lab node, in both directions: the lab node sends to
  the Hetzner node's published `65.109.154.126:8000`, that gets in, and iroh
  uses the observed (peer-reflexive) `213.163.75.157:<port>` for the way back.
* Bind takes ~3.1-3.4 s here vs ~0.8 s in the lab (`t_first`). That matches
  iroh's 3 s net-report probe timeout (`PROBES_TIMEOUT`) waiting for the QAD
  probes that never get an answer; the udp-blocked lab node shows the same
  (3.5 s).
* Relay and direct RTTs are about equal (33-37 ms) because the relay runs on
  the same i3D host as the lab VM, so the relay adds almost no detour.
* The Hetzner node published 16 direct addresses: its public IPv4, 14 Docker
  bridge addresses (172.17-30.0.1) and one IPv6. 16 is the record limit, and
  `build_and_sign` truncates iroh's (sorted) address list with
  `take(MAX_DIRECT_ADDRS)`. Here the public address sorts first and survived;
  on a host with more than 16 addresses whose public or QAD-discovered address
  sorts after its private ones, it would be the one dropped (see open issues).

## Open issues found

1. **Address truncation can drop the public address.** `direct_addrs` takes
   the first 16 of iroh's sorted addresses, so a Docker host with many bridges
   can lose its public or QAD-discovered address from the record. Filtering
   private/bridge addresses before publishing (design.md §14, open question 3)
   or ranking public/QAD addresses first would avoid it.
2. **QAD port.** With the real relay's QAD on 56001 and no way to configure it
   in the CLI, nodes never learn their public address; variant A stays on the
   relay. Re-run once the CLI option lands.
3. **Startup cost of failed QAD.** 3 s extra in `bind` whenever QAD probes go
   unanswered (UDP blocked, wrong port). `Builder::online_timeout` (5 s) caps
   it, but every CLI invocation pays it.
4. **symmetric↔symmetric and port-restricted↔symmetric stay relayed**, as
   expected without port prediction. Whether that matters depends on how
   common symmetric NATs are among yagna providers.

## Caveats

* The emulated NATs are Linux conntrack + nftables, see the README's
  limitations: EIM relies on port preservation with one host per router,
  EIF/ADF are emulated with nft sets, all NATs drop unsolicited packets
  silently, one mapping timeout. Real CPE behaves in more ways than this.
* 30 s runs with continuous traffic say nothing about idle connections, NAT
  mapping expiry, or keepalive intervals. They also don't show whether iroh
  would get symmetric↔symmetric direct after much longer than 30 s.
* Each run uses fresh endpoints; iroh had no previous state about the peer.
* The lab's relay is reached with 10 ms one-way delay and the nodes with 10 ms
  each; real relays are often much farther than the peer, which makes the
  relay path more expensive than here.
* During the first matrix attempt a leftover listener from an aborted run kept
  running in node `pub1` with the same identity (an operator error, not a lab
  bug). All 54 runs were checked afterwards: every reply came from the
  listener started for that run.
