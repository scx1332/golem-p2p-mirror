# golem-p2p test harness

Tools for testing golem-p2p across NATs. Layer 1, in `lab/`, is an emulated-NAT
lab on one Linux VM. Nodes run in network namespaces behind emulated home
routers, carrier NATs and firewalls, with a simulated internet that hosts its
own iroh relay and golem-p2p directory. One switch per node sends its traffic
out through the VM's real uplink instead.

Results of the first experiments: [lab/RESULTS.md](lab/RESULTS.md).

```
lab/lab.sh            the lab: up/down/status/exec/spawn/listen/send/pair/matrix/
                      bench/benchpair/benchmatrix/selftest/netem
lab/deploy.sh         build the CLI and copy the lab to the VM (run on the build machine)
lab/realworld.sh      experiment 2: lab node via the real uplink <-> public node here
lab/topologies/*.topo topologies (selftest, matrix, real, campaign, campaign-real)
lab/tools/reflector.py   UDP/TCP address reflector (runs in the lab's "internet")
lab/tools/natcheck.py    measures mapping and filtering behaviour from behind a NAT
lab/tools/summarize.py   summarizes a send run (path, timings)
lab/tools/aggregate.py   turns matrix/realworld results.txt into a Markdown table
lab/tools/benchsum.py    one key=value line per `bench --json` run; run metadata
campaign/campaign1.sh    benchmark campaign 1 (lab matrix, real world, relay, startup)
campaign/campaign2.sh    campaign 2: the same scenarios after the bench/timeout fixes
campaign/campaign3.sh    campaign 3: the same scenarios after the loss/relay-fallback/sizing fixes
campaign/campaign4.sh    campaign 4: the same scenarios after the upload-count/concurrency fixes
campaign/summarize.py    campaign directory -> summary.json + table.md (all bench formats)
campaign/compare.py      two campaign directories -> compare.json + compare.md;
                         --series: several -> compare<N>.json + compare<N>.md
soak/soak.sh             soak tests: many clients against one listener for hours,
                         resources sampled over time (section "Soak tests")
soak/sampler.py          samples RSS, heap, threads, fds, sockets, CPU of the run's processes
soak/summarize.py        soak run -> summary.json, table.md, plot/*.csv (growth fits)
results/<date>-<name>/   raw data and summaries of campaigns and soak runs
```

Results of benchmark campaign 1: [results/2026-11-07-campaign1/](results/2026-11-07-campaign1/);
campaign 2 and its comparison with campaign 1: [results/2026-11-07-campaign2/](results/2026-11-07-campaign2/)
(`compare.md`); campaign 3, its comparison with campaign 2 (`compare.md`) and
campaigns 1-3 side by side (`compare3.md`):
[results/2026-11-08-campaign3/](results/2026-11-08-campaign3/); campaign 4,
its comparison with campaign 3 (`compare.md`) and campaigns 1-4 side by side
(`compare4.md`): [results/2026-11-08-campaign4/](results/2026-11-08-campaign4/).

## Requirements

* A Linux VM (tested: Ubuntu 26.04, kernel 7.0, 8 vCPU) with passwordless sudo,
  `iproute2`, `nftables`, `conntrack`, `python3`, `curl`, `openssl` and the
  `sch_netem` module. `deploy.sh` installs `nftables conntrack tcpdump` if missing.
* The lab owns the VM's network namespaces named `gpl-*`, the nft tables `gpl`
  (inside namespaces) and `gpl_uplink` (root namespace), interfaces `gplu*`
  and `/run/gpl`. The only other change is `net.ipv4.ip_forward` in the root
  namespace while a node has `uplink=real` (restored by `down`). Kernel modules
  the lab loads (`veth`, `nf_conntrack`, `nf_nat`, `nft_*`, `sch_netem`) stay
  loaded after `down`; with no rules left in the root namespace they do
  nothing.
* Binaries: `golem-p2p` (built on the build machine, glibc ≥ 2.34) and
  `iroh-relay` 1.4.1 (copied from the relay VM or given with `IROH_RELAY_BIN`).

## Deploy

On the build machine (here: the Hetzner server, with `~/.ssh/p2p/ssh_config`):

```sh
harness/lab/deploy.sh                 # cargo build --release -p golem-p2p-cli, then copy
GPL_SKIP_BUILD=1 harness/lab/deploy.sh   # copy only
```

It copies `lab.sh`, `tools/`, `topologies/` to `~/golem-p2p-lab` on
`p2p-natlab`, the CLI to `bin/golem-p2p`, `iroh-relay` from `p2p-relay1` to
`bin/iroh-relay` (once), the real relay's certificate to
`certs/real-relay.crt`, and writes the deployed commit to `DEPLOYED`.
Don't deploy while a lab command runs: bash reads scripts while executing them.

## Run

On the VM (`lab.sh` re-executes itself with sudo; processes in nodes run as
the invoking user):

```sh
cd ~/golem-p2p-lab
./lab.sh up matrix            # build the topology in topologies/matrix.topo (or a path)
./lab.sh status               # services, nodes, root-namespace changes
./lab.sh selftest             # measure every node's NAT (table below)
./lab.sh pair easy1 sym1 --duration 30      # listen in easy1, send from sym1 for 30 s
./lab.sh matrix --repeat 3 --duration 30 pub1:easy1 easy1:sym1 ...
./lab.sh exec easy1 sh -c 'golem-p2p listen --key k --relay $GPL_RELAY --relay-ca $GPL_RELAY_CA --directory $GPL_DIRECTORY'
./lab.sh exec --root easy1-r1 conntrack -L  # look at a router's NAT table
./lab.sh netem easy1 delay=50ms loss=2%     # change a link at runtime
./lab.sh down                 # remove everything (also after a crash)
```

| command | what it does |
|---|---|
| `up <topology>` | tears down any previous lab, builds the topology, starts relay, directory and reflector, waits until they answer. Always rebuilds from scratch, so it is idempotent. |
| `down` | kills every process in a `gpl-*` namespace (TERM, then KILL), deletes the namespaces, leftover `gplu*` links and the `gpl_uplink` table, restores `net.ipv4.ip_forward`, removes `/run/gpl`. Works from any state, including a half-built lab or after a crash. |
| `status` | human-readable state. `nodes` prints a TSV table (name, profile, uplink, netns, node IP, public IP) for scripts. |
| `env [<node>]` | the variables a node's commands get: `GPL_RELAY`, `GPL_RELAY_CA`, `GPL_DIRECTORY`, `GPL_REFLECTOR`, `GPL_NODE`, `GPL_STATE`, `GOLEM_P2P` (real relay for `uplink=real` nodes). |
| `exec [--root] <target> <cmd...>` | runs a command in a namespace. Targets: a node name, `<node>-r1`, `<node>-r2` (routers, innermost first), `inet`, `svc`. In a node it runs as the invoking user with `bin/` on `PATH` and the `env` variables set; `--root` runs as root. |
| `spawn [--root] <target> <tag> <cmd...>` / `stop <tag>` | the same in the background, log in `/run/gpl/logs/<tag>.log`, pid in `/run/gpl/pids/<tag>.pid`. |
| `listen <node> [--tag T] [--relay U --relay-ca F --directory D] [-- args]` | background `golem-p2p listen` with the node's relay and directory; prints the `READY` line (with `node_id=`). Stop with `stop <tag>` (default tag `listen-<node>`). |
| `send <node> --to <id> [--duration S] [--out DIR] [...] [-- args]` | `golem-p2p send --count <many>` for S seconds, every output line timestamped, then one summary line. |
| `pair <listener> <sender> [--duration S] [--out DIR] [--listen-arg A] [--send-arg A] [...]` | `listen` + `send` + `stop`, summary includes which paths the listener saw. Raw logs in `~/gpl-runs/<time>-<listener>-<sender>/`. |
| `matrix [--repeat N] [--duration S] [--out DIR] <l:s>...` | `pair` for every pair, N rounds, results in `DIR/results.txt`. |
| `bench <node> [--to ID] [--out DIR] [--run-id R] [--relay U --relay-ca F --directory D] [--timeout S] [-- args]` | `golem-p2p --json bench` in a node (SIGTERM after `--timeout`, default 300 s). Prints the `bench_report` line; a `key=value` summary goes to stderr. Raw output in DIR: `bench.jsonl` (every event, `ready` first, `bench_report` last), `bench.err`, `bench.exit`, `meta.json`. |
| `benchpair <listener> <bencher> [--out DIR] [--run-id R] [--listen-arg A]... [--bench-arg A]... [-- bench args]` | `golem-p2p --json listen` in one node, `bench --json --to <it>` in the other, then stops the listener. Prints the `bench_report` line; adds `listen.jsonl`, `listen.err` and `summary.txt` to DIR (default `~/gpl-runs/<time>-bench-<l>-<b>`). |
| `benchmatrix [--repeat N] [--out DIR] [--bench-arg A]... <l:b>...` | `benchpair` for every pair, N rounds; every `bench_report` line appended to `DIR/reports.jsonl`, summaries to `DIR/results.txt`, raw runs in `DIR/<l>-<b>/run<r>/`. |
| `selftest [<topology>]` | runs `natcheck.py` in every lab node and compares with the profile's expected behaviour; prints a Markdown table, exits non-zero on a mismatch. |
| `netem <node> [delay=] [jitter=] [loss=] [rate=] \| clear` | changes the node's access link (both directions) while the lab runs. |

A summary line looks like

```
listener=sym1 sender=easy1 ok=1 replies=721 t_first=0.92 first_ms=298 first_path=relay
  t_direct=0.98 last_path=direct switches=1 direct_pct=100 relay_ms=- direct_ms=40
  listener_saw=direct:721,relay:1 error=-
```

`t_first`/`t_direct` count from the start of the `send` process (bind waits
for the relay, up to 5 s), `first_ms` is the first request as printed by
`send` (directory lookup, connect, handshake, one request). Field meanings are
in `tools/summarize.py`.

The `bench` commands use the CLI's `--json` events instead
([docs/cli-events.md](../docs/cli-events.md)); `--run-id` and the node name
(`--node-name`) are set on both sides. Their summary line (`tools/benchsum.py`)
looks like

```
listener=sym1 listener_profile=symmetric bencher=easy1 bencher_profile=easy run=trial1
  exit=0 bind_ms=608 status=healthy connect_ms=217 first_path=relay ttd_ms=304
  final_path=direct changes=1 direct_frac=0.98 rtt_p50=40.4 rtt_p95=40.6 jitter=0.9
  lost_pct=0.04 up=122.7[direct] down=122.1[direct] conc=155.9 rc=80.6/35.4/35.6
  listener_saw=direct:83,relay:28 issues=- hints=relay_fallback verdict="Good: ..."
```

`lab.sh listen <node> -- --json ...` prints the listener's `ready` event and
keeps its stderr apart (`$STATE/logs/<tag>.err`). Lab nodes use the lab relay
with its QAD port given explicitly (`https://198.18.0.1,quic=7842`: 7842 is
iroh's default, but only an explicit port lets `bench` treat a missing QAD
answer as blocked UDP rather than as a possibly wrong port). Nodes with
`uplink=real` use the real relay with its QAD port by default
(`https://213.163.75.157:56000,quic=56001`, override with `GPL_REAL_RELAY`).

### Topology files

```
# comment
set relay_delay=10ms          # optional: netem on the relay/services link, each direction
# name    profile          options
a         easy             delay=20ms jitter=5ms loss=0.5% rate=20mbit
b         symmetric        udp_timeout=30
c         easy             uplink=real
```

Node names: `[a-z][a-z0-9]{0,11}`. Options:

* `delay`, `jitter`, `loss`, `rate`: `tc netem` on the node's access link (the
  outermost router's WAN link), applied in both directions. `delay=10ms` adds
  10 ms each way, so 20 ms to the node's round trip. Jitter reorders packets
  (netem's default).
* `udp_timeout=N`: NAT mapping lifetime in seconds on the node's routers (the
  EIF/ADF set timeout and the conntrack UDP timeouts). Default 120 s, Linux's
  `nf_conntrack_udp_timeout_stream`; unanswered flows expire after 30 s.
* `uplink=lab|real`: where the outermost router's WAN goes (default `lab`).

Built-in: `selftest` (one node per profile), `matrix` (experiment nodes, 10 ms
links, 10 ms relay link), `real` (two `uplink=real` nodes and one lab node).

## Topology

```
                                  VM root namespace (untouched unless a node has uplink=real)
                                  enp5s0 10.167.138.10 ── i3D host masquerade (213.163.75.157) ── internet
                                     │  table ip gpl_uplink: masquerade, forward only gplu* <-> enp5s0
                                  gplu<i> 172.30.<i>.1
                                     │                                 (uplink=real nodes only)
 gpl-svc ─────────────────┐     gpl-inet ("the internet", plain router) ┐
 │ 198.18.0.1  iroh-relay │ svc │ 198.18.0.254                         │
 │   https :443           ├─────┤ n1 198.18.1.1    n2 198.18.2.1   ... │
 │   QAD   :7842/udp      │     └──┬──────────────────┬────────────────┘
 │   directory :8080      │        │ netem            │ netem   (delay/jitter/loss/rate)
 │ 198.18.0.2-4 reflector │        │                  │
 └────────────────────────┘     wan 198.18.1.2     wan 198.18.2.2
                                gpl-a-r1           gpl-b-r2   outer NAT (double: easy, cgnat: CGN)
                                (NAT profile)      lan 10.2.0.1 (double) / 100.64.2.1 (cgnat)
                                lan 192.168.1.1        │
                                   │               wan 10.2.0.2 / 100.64.2.2
                                   │               gpl-b-r1   home NAT (easy)
                                   │               lan 192.168.2.1
                                eth0 192.168.1.2       │
                                gpl-a (node)       eth0 192.168.2.2
                                                   gpl-b (node)
```

Address plan (`<i>` = node index in the topology file, 1..250):

| range | use |
|---|---|
| `198.18.0.0/24` | services: `.1` relay + directory, `.2`/`.3`/`.4` reflector, `.254` inet |
| `198.18.<i>.0/24` | WAN link of node i: `.1` inet, `.2` outermost router (the node's public IP) |
| `198.19.<i>.0/24` | LAN of a `public` node (routed, no NAT): `.1` router, `.2` node |
| `192.168.<i>.0/24` | LAN of a NAT'd node: `.1` router, `.2` node |
| `10.<i>.0.0/24` | between the two routers of `double` |
| `100.64.<i>.0/24` | between home router and carrier NAT of `cgnat` (RFC 6598 shared space) |
| `172.30.<i>.0/24` | `uplink=real`: root namespace `.1`, outermost router `.2` |

198.18.0.0/15 (RFC 2544 benchmarking) plays the public internet: nothing on
the real internet uses it, and iroh treats it as a normal global address.
IPv6 is disabled in all lab namespaces, and there is no DNS (use IPs).

## NAT profiles

Mapping and filtering in RFC 4787 terms: EIM = endpoint-independent mapping
(same external ip:port toward every destination), ADM/APDM = address
(-and-port) dependent mapping; EIF/ADF/APDF = endpoint-independent /
address-dependent / address-and-port-dependent filtering.

| profile | routers (inner first) | intended behaviour | how |
|---|---|---|---|
| `public` | router, no NAT | public IP, no firewall | plain routing |
| `easy` | home NAT | EIM + EIF ("full cone"), port-preserving | `masquerade` + dynamic set of mapped UDP ports; inbound UDP to a mapped port is DNATed to the node |
| `restricted` | home NAT | EIM + ADF ("restricted cone") | `masquerade` + set of (remote IP, port); inbound from a remote IP the node sent to is DNATed |
| `port-restricted` | home NAT | EIM + APDF ("port-restricted cone") | plain Linux `masquerade` (conntrack filtering) |
| `symmetric` | home NAT | APDM + APDF | `masquerade fully-random`: a random external port per 5-tuple |
| `udp-blocked` | home NAT | TCP only | `port-restricted` plus all UDP dropped in both directions |
| `double` | easy behind easy | EIM + EIF through two NATs | two `easy` routers, 10.x between them |
| `cgnat` | easy home NAT behind a carrier NAT | EIM + APDF | `easy` router, 100.64.x, `port-restricted` CGN |

All NAT routers drop unsolicited inbound traffic silently (no ICMP
unreachable, no TCP RST), like typical home routers. The rules are in
`router_nft` in `lab.sh`.

### Selftest

`lab.sh selftest` runs `tools/natcheck.py` in every node against the
reflector in `gpl-svc`, which listens on `198.18.0.2:3478`, `198.18.0.2:3479`,
`198.18.0.3:3478` and `198.18.0.4:4000`:

* mapping: one socket asks three reflector endpoints (two ports on one IP, one
  other IP) for its external address;
* filtering: a fresh socket contacts `198.18.0.2:3478` only, the reflector then
  sends from its other three endpoints; which of them arrive gives EIF, ADF or
  APDF;
* unmapped port: the reflector also sends to an external port the node never
  used (a bound but silent socket); only a node without NAT should get it;
* TCP: connect to `198.18.0.2:3478/tcp` and read the observed address.

Observed on p2p-natlab (kernel 7.0.0-43, nftables 1.1.6), identical in every
run so far (see RESULTS.md):

| profile | mapping | filtering | port preserved | inbound to unmapped port | UDP | TCP |
|---|---|---|---|---|---|---|
| `public` | none (reflectors see the node's own ip:port) | open | yes | yes | yes | yes |
| `easy` | EIM | EIF | yes | no | yes | yes |
| `restricted` | EIM | ADF | yes | no | yes | yes |
| `port-restricted` | EIM | APDF | yes | no | yes | yes |
| `symmetric` | APDM (new random port per destination, also per port on one IP) | APDF | no | no | yes | yes |
| `udp-blocked` | - | - | - | - | no | yes |
| `double` | EIM | EIF | yes | no | yes | yes |
| `cgnat` | EIM | APDF | yes | no | yes | yes |

For `uplink=real` nodes the selftest prints "skipped": the lab reflector is
not reachable from them. `realworld.sh --natcheck` measures them against
reflectors on the relay VM and on the build machine instead.

## uplink=real

A node with `uplink=real` gets its outermost router's WAN on a veth to the VM's
root namespace instead of `gpl-inet`. This is the one deliberate change to the
root namespace, made only when such a node exists and undone by `down`:

* `net.ipv4.ip_forward=1` (the old value is saved in `/run/gpl/ip_forward.orig`
  and restored);
* table `ip gpl_uplink`: masquerade of `172.30.0.0/16` and `198.19.0.0/16` out
  of the default-route interface; a forward chain with policy drop that only
  passes `gplu*` -> uplink (not to private, loopback, link-local or multicast
  destinations) and established replies back; and an input chain that drops
  everything from `gplu*`, so lab nodes cannot reach the VM itself.

The default route, `enp5s0`, sshd and the rest of the VM are not touched.

Such a node's packets cross **three NATs** on the way out: its emulated
profile, the VM's masquerade, and the i3D host's masquerade to
`213.163.75.157`. Measured (RESULTS.md, `realworld.sh --natcheck`): both
outer layers keep the port, so the emulated profile's mapping shows through
unchanged (`easy` stays EIM with its own port, `symmetric` stays APDM with its
random ports), but they add APDF filtering. So `easy` with `uplink=real` is a
port-restricted cone from the internet's point of view. The real relay (`https://213.163.75.157:56000`) is reached through the i3D host's
hairpin NAT, and sees the node as `213.163.75.157:<port>`. There is no
default directory for these nodes: pass `--directory` to `listen`/`send`/`pair`,
or set `GPL_REAL_DIRECTORY=<url>` for `lab.sh up` (`realworld.sh` runs one on
the relay VM). Lab-internal
services (relay, directory, reflector at 198.18.0.x) are not reachable from
`uplink=real` nodes, and vice versa.

## Known limitations

* **Linux conntrack is not a real NAT box.** Mappings are per 5-tuple. EIM
  comes from masquerade keeping the source port, which works because each
  router has exactly one host behind it; two hosts behind one router could
  collide and get different ports. `easy`/`restricted` filtering is emulated
  with nft sets keyed by the internal port, which relies on that same port
  preservation (verified by the selftest's "port preserved" column).
* **Mapping lifetimes.** One timeout for all mappings (default 120 s after
  the last outbound packet for the EIF/ADF sets; conntrack's 30 s / 120 s for
  the rest). Real routers vary (30 s to many minutes) and may refresh on
  inbound traffic. Use `udp_timeout=` to test short timeouts.
* **No hairpinning between lab nodes behind the same NAT**, no shared CGN
  between several subscribers, no port-block allocation, no UPnP/PCP/NAT-PMP
  (golem-p2p deliberately does not use port mapping, see design.md §9).
* **Fixed LAN addressing.** Each node has its own LAN subnet; real homes
  often share `192.168.1.0/24`, which matters when peers publish private
  addresses (they do, see design.md §14).
* **IPv4 only**, no DNS inside the lab.
* **One local relay.** Multi-relay and relay-failover scenarios need a second
  relay in `gpl-svc` (not done yet).
* **netem jitter reorders packets**; `rate` limits per direction.
* **VM behind the i3D host's NAT.** `uplink=real` adds two NAT layers (above);
  nothing on the internet can open a connection to a lab node, and the host's
  NAT behaviour is outside our control.
* The lab VM runs everything on one kernel: CPU contention between nodes is
  possible with many busy nodes (the experiments here use about 25 requests/s).

## Benchmark campaigns

`campaign/campaign1.sh` re-runs benchmark campaign 1 from the build machine
(it drives the lab VM and the relay VM over ssh, and runs `golem-p2p` on the
build machine itself). Steps, in this order for `all`:

```sh
harness/campaign/campaign1.sh --out harness/results/$(date -u +%F)-campaign1 all
# or step by step:
harness/campaign/campaign1.sh --out DIR deploy lab labextra # lab matrix (~30 min)
harness/campaign/campaign1.sh --out DIR realworld startup    # via the real relay (~30 min)
harness/campaign/campaign1.sh --out DIR relay summarize cleanup
```

| step | what |
|---|---|
| `deploy` | build the CLI, `lab/deploy.sh` |
| `lab` | `lab.sh up campaign`, selftest, then every lab scenario (`LAB_SCENARIOS` in the script: listener, bencher, extra bench arguments) `--repeat` times (default 3), rounds interleaved, with `lab.sh benchpair`; fetches the runs |
| `labextra` | NAT "reboot" during `--idle 60s`: `conntrack -F` on both routers of a port-restricted pair 20 s into the idle phase (needs the lab from `lab`) |
| `realworld` | `lab.sh up campaign-real` (easy, symmetric and udp-blocked nodes with `uplink=real`), a temporary directory on the relay VM (`systemd-run` unit `gpl-rw-directory`, port 56050, `RuntimeMaxSec`); lab nodes bench a `listen` node on this machine on a random UDP port (behind ufw) and on UDP 8000 (allowed by ufw), and this machine benches `listen` nodes in the lab; relay with `,quic=56001` |
| `startup` | local check only (`bench` without `--to`) here and in the easy lab node, with the relay's QAD port, iroh's default port (unanswered) and QAD off: bind time, net report |
| `relay` | relay only (`--relay-only` on both sides) between two processes on this machine through the real relay: alone, then 2 and 4 benches in parallel (separate listeners, no concurrency phase) |
| `summarize` | `campaign/summarize.py DIR`: `summary.json` and `table.md` |
| `cleanup` | `lab.sh down`, stop the directory on the relay VM and local processes |

Layout of a campaign directory: `scenarios.jsonl` (one line per scenario:
`id`, `group`, `title`, `a` = the bench side, `b` = the listener side,
`netem`, extra `args`), `<group>/<scenario>/run<r>/` with `bench.jsonl`,
`bench.err`, `bench.exit`, `listen.jsonl`, `listen.err`, `meta.json`
(parallel relay runs: `run<r>-<i>`), `relay-metrics.txt` (the real relay's
byte counters before and after each step that uses it), `notes.json`
(hand-written observations merged into the summary), `summary.json`,
`table.md`, `debug/` (extra captures and runs, not summarized), and from
campaign 2 on `versions.txt` (the binary on each machine) and
`compare.json`/`compare.md` (against the previous campaign; campaign 3 also
`compare3.json`/`compare3.md` over campaigns 1-3, campaign 4
`compare4.json`/`compare4.md` over campaigns 1-4, and `hetzner-load.txt`, the
build machine's load average during the steps that run there). Keys of the local test nodes go to
`DIR/.keys/` (git-ignored). `summary.json` has per scenario the run counts,
final paths, medians/percentiles of the bench measurements, `bind_ms`, and
`verdict_checks`: the bench's verdict and net report compared with the known
NAT profile of the bench side (right / misleading / wrong). The real relay is shared: every bench caps relayed transfers at
4 MB (`--relay-transfer-size`), and a full run of campaign 1 moves a few
hundred MB through it.

`campaign/campaign2.sh` runs campaign 1's scenarios with the same run counts
and steps (`all`, or step by step as above, default output
`results/<date>-campaign2`). It differs in three ways: every step that runs
`golem-p2p` refuses to start unless the lab VM has the same binary as the
build machine (bench and listen must be the same version: a listener older
than the bench cannot answer an upload stopped at `--transfer-time`); `deploy`
writes `versions.txt`; `summarize` also runs `compare.py` against campaign 1
(`GPL_COMPARE_WITH=DIR` for another base). `summarize.py` reads the bench JSON
of both campaigns and judges each by its own bench's rules (see its
docstring); re-run on campaign 1's raw data it reproduces campaign 1's
`summary.json`, with a few added fields (`run_duration_s`, `phase_ms_p50`,
`disconnect_runs`, `connection_lost`, `quic_counters_runs`, `latency_loss`,
`concurrency`, `transfer_limited_by`), and on campaign 2's raw data campaign
2's `summary.json` exactly. Benches after campaign 2 report
`peer.latency.loss` (latency-phase loss counted once the path had settled,
which the packet_loss hint judges; `latency_loss` then pools it instead of
`peer.latency.quic`), `relay_compare.reachable` (an issue
`relay_fallback_failed` only if nothing got through; otherwise the hint
`relay_fallback_degraded`, judged right) and `concurrency.sized_for_mbps`
(smaller parallel requests on a slow link; `concurrency.sized_runs` and
`request_bytes` in the summary). Benches after campaign 3 add
`upload/download.second_half_mbps` (the requests are sized for the lowest of
the transfers' throughput and second-half throughput;
`transfer_second_half_mbps` in the summary) and `concurrency.restart` (the
phase started over with smaller requests because none had finished by half
the time limit; `concurrency.restarts` and `first_request_bytes`); these keys
appear only for runs that report the fields, so campaigns 1-3 summarize as
before. The benchsum line shows `up=AVG/HALF` when the second half was under
half the average, and `conc=...:restart`.

`campaign/campaign3.sh` is `campaign2.sh` with campaign 2's scenarios, run
counts and checks (default output `results/<date>-campaign3`); only its
`summarize` step differs: `compare.py` against campaign 2
(`GPL_COMPARE_WITH`), and `compare.py --series` over campaigns 1, 2 and 3
(`GPL_COMPARE_SERIES`, the earlier directories oldest first), which writes
`compare3.json`/`compare3.md`: totals and per-scenario outcomes, verdicts,
misleading/wrong checks, RTT, throughputs (with the concurrency phase), run
durations and lost connections of every campaign side by side, highlights
from the last campaign's `notes.json` key `comparison_series`.

`campaign/campaign4.sh` is `campaign3.sh` with the same scenarios, run counts
and checks (default output `results/<date>-campaign4`), compared with
campaign 3 (`compare.md`) and over campaigns 1-4 (`compare4.md`). It also
samples this machine's load average every 10 s while the `realworld`,
`startup` and `relay` steps run (`DIR/hetzner-load.txt`), since other work on
the build machine can shift the numbers measured there.

## Soak tests

`soak/soak.sh` runs many clients against one listener for hours and samples
the resources of every process, to find leaks, unbounded growth, slowdowns
and failures under churn in the library, the CLI's listener and directory,
and the iroh relay. It needs only the CLI binary, `iroh-relay` 1.4.1 (for a
local relay), `python3`, `openssl` and `curl`; no root.

```sh
cargo build --release -p golem-p2p-cli
cargo install iroh-relay --version 1.4.1 --features server --root target/soak-tools
harness/soak/soak.sh run steady --out /tmp/soak-steady --duration 2h --clients 100
harness/soak/soak.sh run relay  --out /tmp/soak-relay  --duration 2h --clients 50
harness/soak/soak.sh run churn  --out /tmp/soak-churn  --duration 2h --workers 6 --session 3s
harness/soak/soak.sh stop --out /tmp/soak-steady      # early stop (the run ends itself)
```

Scenarios of `run` (a local relay with a self-signed certificate, a local
directory and a listener on `127.0.0.1`, then the load, then a cool-down):

| scenario | load |
|---|---|
| `steady` | `--clients` processes of `golem-p2p soak`, each one echo request (`--payload-size`) per `--interval` over a direct path, and an upload of `--bulk-size` every `--bulk-every` |
| `relay` | the same with `--relay-only` on the clients and the listener: everything goes through the relay |
| `churn` | `--workers` loops of client sessions: start `golem-p2p soak` for `--session` (one request per `--session-interval`), exit, start again. Odd workers use a fresh identity every session, even ones cycle through `--reuse-keys` keys of their own; every session has a new iroh endpoint key, as on every process start |

Phases (in `phases.jsonl`): `idle` (services only, `--idle`), `load_start`,
`load_end`, `cooldown` (clients gone, `--cooldown`), `end`. The whole run
goes into a systemd user scope in the slice `golem-soak.slice`, capped at
`SOAK_CPU_QUOTA` (600 %) and `SOAK_MEMORY_MAX` (6G) for all concurrent runs
together (`SOAK_SLICE=` disables this); `systemctl --user stop
golem-soak.slice` ends everything. Clients run with 2 tokio worker threads
(`SOAK_CLIENT_THREADS`).

What is recorded in the run directory:

* `samples.jsonl`, from `soak/sampler.py` every `--sample-interval` seconds:
  per process (relay, directory, listener) RSS, peak RSS, anonymous RSS
  (mostly heap), threads, open fds, sockets, CPU time; the clients and churn
  workers aggregated (count, sums, maxima); the cgroup's memory; the host's
  load; and the relay's Prometheus metrics every minute.
* `listener.jsonl` and `directory.jsonl`: their `stats` events every 30 s
  (`--stats-interval`, see [docs/cli-events.md](../docs/cli-events.md)): tokio
  tasks, the endpoint's connection tables and counters, the directory's store
  size. The listener's per-request events are dropped.
* `clients/client-<i>.jsonl`: each client's `soak_stats` every
  `--stats-interval` (counts, errors, RTT percentiles and histogram, uploads,
  its own memory and tasks); `churn/churn-<w>.jsonl`: one `soak_stats` per
  session plus `session_exit` lines with the exit code.
* `relay/relay.log`, `*.err`, `meta.json`, `phases.jsonl`.

`soak/summarize.py DIR` (run at the end of `run`) writes `summary.json`,
`table.md` and `plot/*.csv` (`procs.csv`, `listener_stats.csv`,
`directory_stats.csv`, `latency.csv` and `churn.csv` per 5-minute bin). For
every series it reports idle, after warm-up (a quarter of the load, at most
15 min), peak, end of load and after cool-down, and a least-squares slope
over the load after warm-up; `flags` lists what grows beyond a threshold
(memory 5 % and 2 MiB, fds 4, threads 2, tasks 10 % and 20, with R^2 >= 0.6),
gauges that do not return to idle after the cool-down, latency trends and
churn slowdowns. It also fits memory against the connections the listener
served (KiB per connection) and against the directory's stored records.
`summarize.py` reads `.jsonl.gz` as well, so raw data can be compressed.

**Remote soaks.** The pieces also run separately, with the services on one
host and clients on others: `services` starts what is not given by
`SOAK_RELAY` (+ `SOAK_RELAY_CA`, `SOAK_RELAY_METRICS`), `SOAK_DIRECTORY` and
`SOAK_TO`, and writes `DIR/env`; `clients`, `churn` and `sample` read it (or
the same variables); `stop DIR [load|all]` ends them. `SOAK_HOST_IP` is the
address local services bind to (`0.0.0.0` for all), `SOAK_PUBLIC_IP` the
address other hosts reach them on (it goes into the local relay's
certificate and URL and into the directory URL), `SOAK_BIND` the nodes' UDP
address (empty: all interfaces), `GOLEM_P2P` and `IROH_RELAY_BIN` the
binaries. Every process gets `--run-id <basename of DIR>` (`SOAK_RUN`), so
use the same run directory name on every host, run `sample` on each, and
copy the client hosts' `clients/`, `churn/` and `samples.jsonl` (renamed,
e.g. `samples-clients.jsonl`, and appended) into one directory for
`summarize`. A relay started by `services` needs its certificate on the
client hosts at the same path (`DIR/relay/relay.crt`). For example, with
the services on the relay VM and clients on the build machine:

```sh
# relay VM: directory and listener, using the VM's own iroh-relay
SOAK_HOST_IP=0.0.0.0 SOAK_PUBLIC_IP=VM_IP SOAK_BIND= SOAK_SLICE= \
  SOAK_RELAY=https://VM_IP:443,quic=7842 SOAK_RELAY_CA=relay.crt \
  SOAK_RELAY_METRICS=http://127.0.0.1:9090/metrics SOAK_RELAY_MATCH=iroh-relay \
  soak.sh services --out ~/soak/r1 && soak.sh sample --out ~/soak/r1
# client host: DIR/env (and the relay certificate) copied from the VM
SOAK_BIND= soak.sh clients --out ~/soak/r1 --clients 100 && soak.sh sample --out ~/soak/r1
soak.sh churn --out ~/soak/r1 --workers 6 --session 3s     # or churn
# at the end, on every host
soak.sh stop --out ~/soak/r1
```

## Driving it from a controller

The intended next layer is a Python controller (on the build machine or on the
VM) that drives `lab.sh` over ssh:

1. `deploy.sh`, then `ssh p2p-natlab golem-p2p-lab/lab.sh up <topology>`
   with a topology file it generated (or one from `topologies/`); `lab.sh
   nodes` for the node table, `lab.sh env <node>` for relay/directory.
2. `lab.sh selftest` as a precondition (exit code).
3. Scenarios with `lab.sh listen` / `send` / `pair` (summary lines are
   `key=value`), or arbitrary commands with `exec`/`spawn`/`stop`, e.g.
   the CLI's `probe` and `--json` output once they land.
4. Fault injection while traffic runs: `lab.sh netem <node> loss=100%`
   (link down), `delay=`, `rate=`; `exec --root <node>-r1 conntrack -F`
   plus, for `easy`/`restricted`, `exec --root <node>-r1 nft flush set ip gpl eif`
   (or `adf`) to simulate a NAT reboot (all mappings lost); `stop` a process
   (crash).
5. `lab.sh down` at the end, and in a `finally`/trap.

Stop CLI processes with SIGTERM, not SIGINT: a non-interactive shell starts
background jobs with SIGINT ignored, the children inherit that, and
`golem-p2p send` then keeps running (`lab.sh` itself uses `timeout -s TERM`).

All state lives in `/run/gpl` (lost on reboot, like the namespaces), results
in `~/gpl-runs/`. Commands print one result per line and exit non-zero on
failure, so a controller can parse them without screen-scraping.
