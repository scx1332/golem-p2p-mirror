# Campaign comparison: 2026-11-07-campaign1 → 2026-11-07-campaign2

Base: commit 9c37664; new: commit 711a316 (harness on top of 3fb1e9c; the golem-p2p crates are those of 3fb1e9c). Both recomputed from the raw runs with `harness/campaign/summarize.py` (`compare.py`); generated, except the highlights, which come from the new campaign's `notes.json` (`comparison`).

## Highlights

* **Same outcomes, no failures.** All 35 scenarios keep their direct/relay outcome; final paths 51 direct / 42 relay / 0 failed (campaign 1: 49 / 42 / 2 failed, the two lossy-link runs whose connection was closed). Lab RTT, throughput, bind and connect times are unchanged within noise; real-world RTT/throughput moved by the two internet route clusters (~34 / ~37.5 ms), not by the code.
* **Verdict accuracy: 88 right / 23 misleading / 0 wrong runs (was 81 / 28 / 2).** Fixed: symmetric NAT named via hard_nat_likely (9 runs), no 'two NATs' for the NAT-less Hetzner side (6), no packet_loss on congestion (6), wrong QAD port named as a possible cause (6), no 'through a relay' for a lost direct connection (2 wrong). New: packet_loss false positives in 21 runs (20 clean lab runs whose hole punching finished during the latency phase, 1 real-world) and relay_fallback_failed on the lossy link's slow but working relay path (2).
* **Lossy link (lab-netem-pr):** connection survives 3/3 (was closed in 2/3), final path direct and QUIC counters present in every run; runs 61-69 s (was 68-81 s); packet_loss hint in 2/3 (was 0/3); but the concurrency phase yields no throughput (all requests cut off at 8 s) and 2/3 runs report relay_fallback_failed because the relayed upload's count arrives later than the 3 s the bench waits.
* **Connection survival after timeouts:** 0 connections lost in 93 peer runs (was 2). The bench no longer times requests out on the lossy link (cut off at 8 s instead); the listener timed out 8 requests in one run without closing the connection.
* **Regressions:** (1) packet_loss false positives from hole-punching probes and reordering counted as latency-phase loss (debug/latency-loss); (2) relay_fallback_failed on a slow relay path; (3) no concurrency throughput on slow links. No measurement regressions; rw-easy-to-open's relay-first start and lower RTT/throughput are routing.

## Totals

| | base | new |
|---|---|---|
| scenarios / runs / runs with a report | 35 / 111 / 111 | 35 / 111 / 111 |
| peer runs: final direct / relay / failed | 49 / 42 / 2 | 51 / 42 / 0 |
| runs whose connection closed during the run | 2 | 0 |
| peer runs with QUIC counters at the end | 91 | 93 |
| verdict runs right (by worst finding) | 81 | 88 |
| verdict runs misleading (by worst finding) | 28 | 23 |
| verdict runs wrong (by worst finding) | 2 | 0 |
| verdict checks right | 229 | 252 |
| verdict checks misleading | 41 | 24 |
| verdict checks wrong | 2 | 0 |

## Per scenario (base → new)

Outcome = final path direct/relay/failed per run (failed includes connections gone at the end). RTT = latency-phase p50 pooled over runs; up/down = median throughput; bind = `ready.bind_ms` p50; run = bench run duration p50; lost conn = runs with a `disconnect` event; verdicts = runs right/misleading/wrong by their worst finding.

| scenario | outcome d/r/f | RTT ms | up Mbps | down Mbps | bind ms | run s | lost conn | verdicts r/m/w |
|---|---|---|---|---|---|---|---|---|
| lab-public-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.5 → 123.5 | 122.4 → 123.6 | 609 → 608 | 20.3 → 20.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.0 → 122.7 | 123.7 → 123.6 | 608 → 610 | 20.3 → 20.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-sym-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.1 → 122.8 | 123.6 → 123.3 | 609 → 609 | 20.4 → 20.4 | 0 → 0 | 3/0/0 → 0/3/0 |
| lab-easy-sym | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.4 → 123.5 | 122.7 → 124.3 | 609 → 609 | 12.6 → 12.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-pr-pr | 3/0/0 → 3/0/0 | 40.4 → 40.5 | 122.2 → 123.0 | 123.6 → 123.4 | 609 → 608 | 20.4 → 20.4 | 0 → 0 | 3/0/0 → 0/3/0 |
| lab-sym-sym | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.4 → 35.6 | 35.1 → 35.4 | 608 → 609 | 13.2 → 13.2 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-cgnat-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.5 → 123.2 | 123.7 → 123.5 | 609 → 609 | 12.7 → 12.7 | 0 → 0 | 3/0/0 → 0/3/0 |
| lab-double-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.0 → 124.0 | 123.6 → 123.9 | 609 → 609 | 12.6 → 12.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-udpb | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.6 → 35.6 | 35.2 → 35.2 | 3207 → 3207 | 15.8 → 15.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-udpb-easy | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.6 → 35.6 | 35.2 → 35.3 | 608 → 609 | 13.2 → 13.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-pr-sym | 0/3/0 → 0/3/0 | 80.7 → 80.6 | 35.6 → 35.3 | 35.4 → 35.1 | 610 → 609 | 13.2 → 13.2 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-rc-sym | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.7 → 122.8 | 123.3 → 122.7 | 610 → 608 | 12.7 → 12.7 | 0 → 0 | 3/0/0 → 1/2/0 |
| lab-netem-pr | 1/0/2 → 3/0/0 | 120.4 → 120.4 | 21.8 → 18.8 | 3.9 → 1.2 | 1329 → 1329 | 71.9 → 63.6 | 2 → 0 | 0/1/2 → 1/2/0 |
| lab-idle30-pr | 3/0/0 → 3/0/0 | 40.4 → 40.5 | 122.2 → 123.1 | 123.0 → 122.7 | 609 → 608 | 72.7 → 72.7 | 0 → 0 | 3/0/0 → 0/3/0 |
| lab-idle5-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.7 → 122.8 | 122.8 → 123.4 | 608 → 608 | 72.8 → 72.7 | 0 → 0 | 3/0/0 → 0/3/0 |
| lab-natflush-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 121.8 → 121.4 | 124.3 → 123.4 | 609 → 608 | 72.8 → 72.8 | 0 → 0 | 3/0/0 → 0/3/0 |
| rw-easy-to-ufw | 3/0/0 → 3/0/0 | 33.9 → 37.1 | 145.4 → 134.2 | 146.9 → 133.3 | 250 → 249 | 11.6 → 11.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-easy-to-open | 3/0/0 → 3/0/0 | 33.7 → 37.1 | 147.6 → 133.9 | 147.7 → 132.5 | 249 → 248 | 17.7 → 17.9 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-symmetric-to-ufw | 0/3/0 → 0/3/0 | 37.7 → 37.3 | 77.5 → 78.6 | 76.7 → 76.8 | 248 → 248 | 11.4 → 11.4 | 0 → 0 | 0/3/0 → 3/0/0 |
| rw-symmetric-to-open | 3/0/0 → 3/0/0 | 34.1 → 34.1 | 145.9 → 147.3 | 146.2 → 147.5 | 249 → 250 | 11.6 → 11.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-udp-blocked-to-ufw | 0/3/0 → 0/3/0 | 37.8 → 34.2 | 77.3 → 85.5 | 75.8 → 85.8 | 3007 → 3008 | 14.1 → 14.0 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-udp-blocked-to-open | 0/3/0 → 0/3/0 | 37.4 → 37.5 | 77.4 → 77.0 | 77.7 → 77.0 | 3008 → 3007 | 14.1 → 14.1 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-ufw-to-easy | 0/3/0 → 0/3/0 | 37.3 → 34.5 | 77.7 → 83.9 | 78.1 → 85.0 | 607 → 620 | 11.9 → 11.8 | 0 → 0 | 0/3/0 → 3/0/0 |
| rw-open-to-easy | 3/0/0 → 3/0/0 | 33.9 → 37.2 | 146.1 → 134.9 | 145.8 → 134.5 | 590 → 601 | 12.2 → 12.5 | 0 → 0 | 0/3/0 → 3/0/0 |
| rw-ufw-to-symmetric | 0/3/0 → 0/3/0 | 34.8 → 37.9 | 83.6 → 75.9 | 85.1 → 77.6 | 595 → 610 | 11.8 → 11.9 | 0 → 0 | 0/3/0 → 3/0/0 |
| rw-open-to-symmetric | 3/0/0 → 3/0/0 | 36.9 → 36.9 | 135.4 → 134.9 | 135.2 → 134.2 | 602 → 593 | 12.3 → 12.4 | 0 → 0 | 0/3/0 → 2/1/0 |
| startup-hetzner-qad-56001 | - → - | - → - | - → - | - → - | 586 → 609 | 0.7 → 0.7 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-56001 | - → - | - → - | - → - | - → - | 249 → 249 | 0.2 → 0.3 | - → - | 3/0/0 → 3/0/0 |
| startup-hetzner-qad-7842 | - → - | - → - | - → - | - → - | 3215 → 3225 | 3.3 → 3.4 | - → - | 0/3/0 → 3/0/0 |
| startup-lab-easy-qad-7842 | - → - | - → - | - → - | - → - | 3007 → 3007 | 3.0 → 3.0 | - → - | 0/3/0 → 3/0/0 |
| startup-hetzner-qad-off | - → - | - → - | - → - | - → - | 593 → 583 | 0.7 → 0.7 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-off | - → - | - → - | - → - | - → - | 249 → 249 | 0.2 → 0.2 | - → - | 3/0/0 → 3/0/0 |
| relay-single | 0/3/0 → 0/3/0 | 74.5 → 67.6 | 38.7 → 42.4 | 38.2 → 42.1 | 519 → 513 | 13.0 → 12.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| relay-parallel2 | 0/4/0 → 0/4/0 | 71.1 → 67.4 | 40.8 → 42.8 | 40.2 → 41.3 | 501 → 504 | 12.4 → 12.3 | 0 → 0 | 4/0/0 → 4/0/0 |
| relay-parallel4 | 0/8/0 → 0/8/0 | 68.8 → 72.3 | 41.2 → 39.6 | 40.8 → 39.1 | 500 → 510 | 12.3 → 12.4 | 0 → 0 | 8/0/0 → 8/0/0 |

## Flags

Changes beyond the noise thresholds (relative and absolute: RTT 10 % and 3 ms, throughput 25 % and 5 Mbps, bind 25 % and 100 ms, run duration 25 % and 3 s), outcome and verdict changes.

* `lab-sym-easy`: regression: verdicts r/m/w 3/0/0 -> 0/3/0
* `lab-pr-pr`: regression: verdicts r/m/w 3/0/0 -> 0/3/0
* `lab-sym-sym`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-cgnat-easy`: regression: verdicts r/m/w 3/0/0 -> 0/3/0
* `lab-pr-sym`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-rc-sym`: regression: verdicts r/m/w 3/0/0 -> 1/2/0
* `lab-netem-pr`: improvement: outcome d/r/f 1/0/2 -> 3/0/0; improvement: verdicts r/m/w 0/1/2 -> 1/2/0; improvement: runs with a lost connection 2 -> 0
* `lab-idle30-pr`: regression: verdicts r/m/w 3/0/0 -> 0/3/0
* `lab-idle5-pr`: regression: verdicts r/m/w 3/0/0 -> 0/3/0
* `lab-natflush-pr`: regression: verdicts r/m/w 3/0/0 -> 0/3/0
* `rw-easy-to-open`: regression: rtt_ms_p50 33.718 -> 37.11 ms
* `rw-symmetric-to-ufw`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `rw-ufw-to-easy`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `rw-open-to-easy`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `rw-ufw-to-symmetric`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `rw-open-to-symmetric`: improvement: verdicts r/m/w 0/3/0 -> 2/1/0
* `startup-hetzner-qad-7842`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `startup-lab-easy-qad-7842`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0

## Misleading and wrong verdict checks (base → new)

* `lab-sym-easy`: none → misleading:packet_loss_unshaped x3
* `lab-pr-pr`: none → misleading:packet_loss_unshaped x3
* `lab-sym-sym`: misleading:hard_nat_missed x3, misleading:relay_reason_vague x3 → none
* `lab-cgnat-easy`: none → misleading:packet_loss_unshaped x3
* `lab-pr-sym`: misleading:hard_nat_missed x3, misleading:relay_reason_vague x3 → none
* `lab-rc-sym`: none → misleading:packet_loss_unshaped x2
* `lab-netem-pr`: misleading:connection_closed_unexplained x2, misleading:packet_loss_missed x3, wrong:says_relay x2 → misleading:packet_loss_missed, misleading:relay_fallback_failed_but_works x2
* `lab-idle30-pr`: none → misleading:packet_loss_unshaped x3
* `lab-idle5-pr`: none → misleading:packet_loss_unshaped x3
* `lab-natflush-pr`: none → misleading:packet_loss_unshaped x3
* `rw-symmetric-to-ufw`: misleading:hard_nat_missed x3, misleading:relay_reason_vague x3 → none
* `rw-ufw-to-easy`: misleading:two_nats_without_nat x3 → none
* `rw-open-to-easy`: misleading:packet_loss_congestion x3 → none
* `rw-ufw-to-symmetric`: misleading:two_nats_without_nat x3 → none
* `rw-open-to-symmetric`: misleading:packet_loss_congestion x3 → misleading:packet_loss_unshaped
* `startup-hetzner-qad-7842`: misleading:udp_blocked_wrong_qad_port x3 → none
* `startup-lab-easy-qad-7842`: misleading:udp_blocked_wrong_qad_port x3 → none
