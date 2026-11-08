# Campaign series: 2026-11-07-campaign1 → 2026-11-07-campaign2 → 2026-11-08-campaign3

All campaigns recomputed from their raw runs with `harness/campaign/summarize.py` (`compare.py --series`), each judged by the rules of its own bench; generated, except the highlights, which come from the last campaign's `notes.json` (`comparison_series`).

* 2026-11-07-campaign1: commit 9c37664
* 2026-11-07-campaign2: commit 711a316 (harness on top of 3fb1e9c; the golem-p2p crates are those of 3fb1e9c)
* 2026-11-08-campaign3: commit d1bc1cd (harness on top of c270dde; the golem-p2p crates are those of c270dde)

## Highlights

* **Verdict runs right / misleading / wrong: 81 / 28 / 2 -> 88 / 23 / 0 -> 110 / 1 / 0.** Checks 229 / 41 / 2 -> 252 / 24 / 0 -> 254 / 1 / 0. Campaign 1's misleading classes (symmetric NAT not named, 'two NATs' without a NAT, packet_loss on congestion, wrong QAD port blamed on blocked UDP, 'through a relay' for a lost connection) were fixed in campaign 2; campaign 2's new ones (21 packet_loss false positives from hole punching, 2 relay_fallback_failed on a slow relay path) are fixed in campaign 3; the remaining one is a sampling miss of 1 % loss.
* **Outcomes: final direct / relay / failed 49 / 42 / 2 -> 51 / 42 / 0 -> 51 / 42 / 0; lost connections 2 -> 0 -> 0.** The direct/relay outcome of every scenario is the same in all three campaigns; the two failures of campaign 1 were the lossy link's connections closed after request timeouts.
* **Lossy link across the campaigns:** campaign 1: connection closed in 2/3, verdict 'through a relay', loss hint 0/3 (2 % whole-connection rule), concurrency 1/3; campaign 2: connection survives 3/3, relay_fallback_failed 2/3, loss hint 2/3, concurrency 0/3; campaign 3: survives 3/3, relay fallback ok 3/3 (13/13 with extra runs), loss hint 2/3 (11/13), concurrency 3/3 (11/13), exit 0 in 3/3.
* **Measurements stable:** lab RTT 40.4 / 80.6 ms and throughput ~123 / ~35 Mbps direct / relayed in all three campaigns; real-world RTT and throughput vary with the ~34 / ~37.5 ms route clusters; 418.5 / 418.4 / 418.7 MB through the real relay with no drops. Runs are 2 s longer in campaign 3 (12 s latency phase).

## Totals

| | 2026-11-07-campaign1 | 2026-11-07-campaign2 | 2026-11-08-campaign3 |
|---|---|---|---|
| scenarios / runs / runs with a report | 35 / 111 / 111 | 35 / 111 / 111 | 35 / 111 / 111 |
| verdict runs right / misleading / wrong (by worst finding) | 81 / 28 / 2 | 88 / 23 / 0 | 110 / 1 / 0 |
| verdict checks right / misleading / wrong | 229 / 41 / 2 | 252 / 24 / 0 | 254 / 1 / 0 |
| peer runs: final direct / relay / failed | 49 / 42 / 2 | 51 / 42 / 0 | 51 / 42 / 0 |
| runs whose connection closed during the run | 2 | 0 | 0 |
| peer runs with QUIC counters at the end | 91 | 93 | 93 |

## Per scenario

Each cell lists the campaigns in order (2026-11-07-campaign1 → 2026-11-07-campaign2 → 2026-11-08-campaign3). Verdicts = runs right/misleading/wrong by their worst finding; outcome = final path direct/relay/failed per run (failed includes connections gone at the end); RTT = latency-phase p50 pooled over runs; up/down/conc = median throughput (conc: concurrency phase, - = no run measured it); run = bench run duration p50; lost conn = runs with a `disconnect` event.

| scenario | verdicts r/m/w | outcome d/r/f | RTT ms | up Mbps | down Mbps | conc Mbps | run s | lost conn |
|---|---|---|---|---|---|---|---|---|
| lab-public-easy | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 123.5 → 123.5 → 123.0 | 122.4 → 123.6 → 122.9 | 156.0 → 156.0 → 155.8 | 20.3 → 20.3 → 22.3 | 0 → 0 → 0 |
| lab-easy-easy | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 123.0 → 122.7 → 122.5 | 123.7 → 123.6 → 123.7 | 156.4 → 156.1 → 156.0 | 20.3 → 20.3 → 22.3 | 0 → 0 → 0 |
| lab-sym-easy | 3/0/0 → 0/3/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 123.1 → 122.8 → 122.6 | 123.6 → 123.3 → 122.7 | 156.2 → 156.2 → 155.9 | 20.4 → 20.4 → 22.4 | 0 → 0 → 0 |
| lab-easy-sym | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 123.4 → 123.5 → 123.3 | 122.7 → 124.3 → 123.5 | 156.0 → 156.1 → 156.2 | 12.6 → 12.6 → 14.6 | 0 → 0 → 0 |
| lab-pr-pr | 3/0/0 → 0/3/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.5 → 40.4 | 122.2 → 123.0 → 121.8 | 123.6 → 123.4 → 123.6 | 156.1 → 156.1 → 156.0 | 20.4 → 20.4 → 22.4 | 0 → 0 → 0 |
| lab-sym-sym | 0/3/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 80.6 → 80.6 → 80.6 | 35.4 → 35.6 → 35.4 | 35.1 → 35.4 → 34.9 | 78.0 → 78.0 → 78.1 | 13.2 → 13.2 → 15.2 | 0 → 0 → 0 |
| lab-cgnat-easy | 3/0/0 → 0/3/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 123.5 → 123.2 → 123.6 | 123.7 → 123.5 → 123.3 | 156.1 → 156.1 → 156.0 | 12.7 → 12.7 → 14.7 | 0 → 0 → 0 |
| lab-double-easy | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 123.0 → 124.0 → 124.2 | 123.6 → 123.9 → 124.4 | 155.8 → 156.0 → 155.9 | 12.6 → 12.6 → 14.6 | 0 → 0 → 0 |
| lab-easy-udpb | 3/0/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 80.6 → 80.6 → 80.6 | 35.6 → 35.6 → 35.7 | 35.2 → 35.2 → 35.4 | 78.2 → 74.3 → 74.2 | 15.8 → 15.8 → 17.8 | 0 → 0 → 0 |
| lab-udpb-easy | 3/0/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 80.6 → 80.6 → 80.7 | 35.6 → 35.6 → 35.4 | 35.2 → 35.3 → 35.4 | 78.1 → 74.2 → 74.2 | 13.2 → 13.2 → 15.2 | 0 → 0 → 0 |
| lab-pr-sym | 0/3/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 80.7 → 80.6 → 80.6 | 35.6 → 35.3 → 35.4 | 35.4 → 35.1 → 35.2 | 78.2 → 74.3 → 74.2 | 13.2 → 13.2 → 15.2 | 0 → 0 → 0 |
| lab-rc-sym | 3/0/0 → 1/2/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 122.7 → 122.8 → 123.2 | 123.3 → 122.7 → 122.9 | 156.1 → 156.1 → 155.9 | 12.7 → 12.7 → 14.7 | 0 → 0 → 0 |
| lab-netem-pr | 0/1/2 → 1/2/0 → 2/1/0 | 1/0/2 → 3/0/0 → 3/0/0 | 120.4 → 120.4 → 120.4 | 21.8 → 18.8 → 1.5 | 3.9 → 1.2 → 2.6 | 2.4 → - → 1.0 | 71.9 → 63.6 → 69.0 | 2 → 0 → 0 |
| lab-idle30-pr | 3/0/0 → 0/3/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.5 → 40.4 | 122.2 → 123.1 → 122.3 | 123.0 → 122.7 → 123.8 | 156.3 → 156.0 → 156.1 | 72.7 → 72.7 → 74.7 | 0 → 0 → 0 |
| lab-idle5-pr | 3/0/0 → 0/3/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 122.7 → 122.8 → 121.9 | 122.8 → 123.4 → 123.6 | 156.2 → 156.3 → 156.2 | 72.8 → 72.7 → 74.7 | 0 → 0 → 0 |
| lab-natflush-pr | 3/0/0 → 0/3/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 40.4 → 40.4 → 40.4 | 121.8 → 121.4 → 122.7 | 124.3 → 123.4 → 122.9 | 156.2 → 156.1 → 156.0 | 72.8 → 72.8 → 74.8 | 0 → 0 → 0 |
| rw-easy-to-ufw | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 33.9 → 37.1 → 34.5 | 145.4 → 134.2 → 140.4 | 146.9 → 133.3 → 134.5 | 280.4 → 261.3 → 279.6 | 11.6 → 11.8 → 13.8 | 0 → 0 → 0 |
| rw-easy-to-open | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 33.7 → 37.1 → 37.2 | 147.6 → 133.9 → 132.8 | 147.7 → 132.5 → 132.9 | 308.2 → 262.5 → 155.9 | 17.7 → 17.9 → 20.0 | 0 → 0 → 0 |
| rw-symmetric-to-ufw | 0/3/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 37.7 → 37.3 → 35.1 | 77.5 → 78.6 → 82.1 | 76.7 → 76.8 → 80.3 | 155.3 → 155.3 → 154.5 | 11.4 → 11.4 → 13.4 | 0 → 0 → 0 |
| rw-symmetric-to-open | 3/0/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 34.1 → 34.1 → 36.9 | 145.9 → 147.3 → 134.4 | 146.2 → 147.5 → 134.8 | 280.0 → 280.0 → 279.9 | 11.6 → 11.6 → 13.7 | 0 → 0 → 0 |
| rw-udp-blocked-to-ufw | 3/0/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 37.8 → 34.2 → 37.4 | 77.3 → 85.5 → 77.3 | 75.8 → 85.8 → 76.4 | 155.1 → 155.2 → 154.8 | 14.1 → 14.0 → 16.1 | 0 → 0 → 0 |
| rw-udp-blocked-to-open | 3/0/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 37.4 → 37.5 → 37.7 | 77.4 → 77.0 → 76.5 | 77.7 → 77.0 → 76.9 | 155.0 → 155.1 → 131.1 | 14.1 → 14.1 → 16.2 | 0 → 0 → 0 |
| rw-ufw-to-easy | 0/3/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 37.3 → 34.5 → 37.1 | 77.7 → 83.9 → 75.8 | 78.1 → 85.0 → 79.2 | 155.1 → 154.1 → 154.6 | 11.9 → 11.8 → 13.9 | 0 → 0 → 0 |
| rw-open-to-easy | 0/3/0 → 3/0/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 33.9 → 37.2 → 34.0 | 146.1 → 134.9 → 145.6 | 145.8 → 134.5 → 146.5 | 264.3 → 156.2 → 280.0 | 12.2 → 12.5 → 14.3 | 0 → 0 → 0 |
| rw-ufw-to-symmetric | 0/3/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 34.8 → 37.9 → 37.1 | 83.6 → 75.9 → 77.7 | 85.1 → 77.6 → 78.8 | 153.4 → 152.9 → 154.6 | 11.8 → 11.9 → 13.9 | 0 → 0 → 0 |
| rw-open-to-symmetric | 0/3/0 → 2/1/0 → 3/0/0 | 3/0/0 → 3/0/0 → 3/0/0 | 36.9 → 36.9 → 37.0 | 135.4 → 134.9 → 136.5 | 135.2 → 134.2 → 134.8 | 278.5 → 156.3 → 156.2 | 12.3 → 12.4 → 14.4 | 0 → 0 → 0 |
| startup-hetzner-qad-56001 | 3/0/0 → 3/0/0 → 3/0/0 | - → - → - | - → - → - | - → - → - | - → - → - | - → - → - | 0.7 → 0.7 → 0.7 | - → - → - |
| startup-lab-easy-qad-56001 | 3/0/0 → 3/0/0 → 3/0/0 | - → - → - | - → - → - | - → - → - | - → - → - | - → - → - | 0.2 → 0.3 → 0.2 | - → - → - |
| startup-hetzner-qad-7842 | 0/3/0 → 3/0/0 → 3/0/0 | - → - → - | - → - → - | - → - → - | - → - → - | - → - → - | 3.3 → 3.4 → 3.4 | - → - → - |
| startup-lab-easy-qad-7842 | 0/3/0 → 3/0/0 → 3/0/0 | - → - → - | - → - → - | - → - → - | - → - → - | - → - → - | 3.0 → 3.0 → 3.0 | - → - → - |
| startup-hetzner-qad-off | 3/0/0 → 3/0/0 → 3/0/0 | - → - → - | - → - → - | - → - → - | - → - → - | - → - → - | 0.7 → 0.7 → 0.8 | - → - → - |
| startup-lab-easy-qad-off | 3/0/0 → 3/0/0 → 3/0/0 | - → - → - | - → - → - | - → - → - | - → - → - | - → - → - | 0.2 → 0.2 → 0.3 | - → - → - |
| relay-single | 3/0/0 → 3/0/0 → 3/0/0 | 0/3/0 → 0/3/0 → 0/3/0 | 74.5 → 67.6 → 71.1 | 38.7 → 42.4 → 40.5 | 38.2 → 42.1 → 40.3 | 70.5 → 73.9 → 78.0 | 13.0 → 12.8 → 14.8 | 0 → 0 → 0 |
| relay-parallel2 | 4/0/0 → 4/0/0 → 4/0/0 | 0/4/0 → 0/4/0 → 0/4/0 | 71.1 → 67.4 → 72.2 | 40.8 → 42.8 → 39.4 | 40.2 → 41.3 → 38.8 | - → - → - | 12.4 → 12.3 → 14.5 | 0 → 0 → 0 |
| relay-parallel4 | 8/0/0 → 8/0/0 → 8/0/0 | 0/8/0 → 0/8/0 → 0/8/0 | 68.8 → 72.3 → 72.1 | 41.2 → 39.6 → 39.5 | 40.8 → 39.1 → 39.7 | - → - → - | 12.3 → 12.4 → 14.5 | 0 → 0 → 0 |

## Misleading and wrong verdict checks per campaign

* `lab-sym-easy`: none → misleading:packet_loss_unshaped x3 → none
* `lab-pr-pr`: none → misleading:packet_loss_unshaped x3 → none
* `lab-sym-sym`: misleading:hard_nat_missed x3, misleading:relay_reason_vague x3 → none → none
* `lab-cgnat-easy`: none → misleading:packet_loss_unshaped x3 → none
* `lab-pr-sym`: misleading:hard_nat_missed x3, misleading:relay_reason_vague x3 → none → none
* `lab-rc-sym`: none → misleading:packet_loss_unshaped x2 → none
* `lab-netem-pr`: misleading:connection_closed_unexplained x2, misleading:packet_loss_missed x3, wrong:says_relay x2 → misleading:packet_loss_missed, misleading:relay_fallback_failed_but_works x2 → misleading:packet_loss_missed
* `lab-idle30-pr`: none → misleading:packet_loss_unshaped x3 → none
* `lab-idle5-pr`: none → misleading:packet_loss_unshaped x3 → none
* `lab-natflush-pr`: none → misleading:packet_loss_unshaped x3 → none
* `rw-symmetric-to-ufw`: misleading:hard_nat_missed x3, misleading:relay_reason_vague x3 → none → none
* `rw-ufw-to-easy`: misleading:two_nats_without_nat x3 → none → none
* `rw-open-to-easy`: misleading:packet_loss_congestion x3 → none → none
* `rw-ufw-to-symmetric`: misleading:two_nats_without_nat x3 → none → none
* `rw-open-to-symmetric`: misleading:packet_loss_congestion x3 → misleading:packet_loss_unshaped → none
* `startup-hetzner-qad-7842`: misleading:udp_blocked_wrong_qad_port x3 → none → none
* `startup-lab-easy-qad-7842`: misleading:udp_blocked_wrong_qad_port x3 → none → none
