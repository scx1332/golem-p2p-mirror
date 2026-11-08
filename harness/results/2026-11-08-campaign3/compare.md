# Campaign comparison: 2026-11-07-campaign2 → 2026-11-08-campaign3

Base: commit 711a316 (harness on top of 3fb1e9c; the golem-p2p crates are those of 3fb1e9c); new: commit d1bc1cd (harness on top of c270dde; the golem-p2p crates are those of c270dde). Both recomputed from the raw runs with `harness/campaign/summarize.py` (`compare.py`); generated, except the highlights, which come from the new campaign's `notes.json` (`comparison`).

## Highlights

* **Verdict accuracy: 110 right / 1 misleading / 0 wrong runs (was 88 / 23 / 0).** Gone: the 21 packet_loss false positives from hole punching (now 0 of 90 clean-link runs; the whole-phase counter still shows the artefact in 20 of them, the settled count in none) and the 2 relay_fallback_failed on the lossy link's working relay path. Left: 1 packet_loss_missed on the lossy link (0 of 115 packets lost in the window, sampling).
* **Lossy link (lab-netem-pr):** healthy and exit 0 in 3/3 (was degraded/exit 5 in 2/3); relay_compare ok 3/3 (was 1/3) and 10/10 extra runs; packet_loss 2/3 as before (11/13 with the extra runs), the count settled in every run; concurrency throughput 0.6-1.4 Mbps in 3/3 (was none) and 8/10 extra; runs 58-70.5 s (was 61-69 s).
* **Same outcomes, no failures:** final paths 51 direct / 42 relay / 0 failed, 0 connections lost, QUIC counters in 93/93, as in campaign 2. Lab RTT/throughput/bind/connect unchanged; real-world numbers move with the internet route clusters.
* **Run durations:** +2.0 s on every peer run from the 12 s latency phase (e.g. 12.6 -> 14.6 s, 20.3 -> 22.3 s); the lossy link's runs did not get longer.
* **Regressions and new findings:** No outcome, RTT or throughput regression. Flagged changes are noise or artefacts: the lossy upload median 18.8 -> 1.5 Mbps (per run 1.3 / 27.8 / 1.5, bimodal on that link in every campaign) and concurrency 262 -> 156 (rw-easy-to-open) / 156 -> 280 (rw-open-to-easy) Mbps, which is the 200 ms tick quantization of the concurrency phase (an old measurement bug, now documented). New bug found in the extra lossy runs: an upload finishing just before the 8 s limit failed with 'closed stream' (1 of 13), and concurrency sized from a lucky fast transfer still gave no number in 2 of 13.

## Totals

| | base | new |
|---|---|---|
| scenarios / runs / runs with a report | 35 / 111 / 111 | 35 / 111 / 111 |
| peer runs: final direct / relay / failed | 51 / 42 / 0 | 51 / 42 / 0 |
| runs whose connection closed during the run | 0 | 0 |
| peer runs with QUIC counters at the end | 93 | 93 |
| verdict runs right (by worst finding) | 88 | 110 |
| verdict runs misleading (by worst finding) | 23 | 1 |
| verdict runs wrong (by worst finding) | 0 | 0 |
| verdict checks right | 252 | 254 |
| verdict checks misleading | 24 | 1 |
| verdict checks wrong | 0 | 0 |

## Per scenario (base → new)

Outcome = final path direct/relay/failed per run (failed includes connections gone at the end). RTT = latency-phase p50 pooled over runs; up/down/conc = median throughput (conc: concurrency phase); bind = `ready.bind_ms` p50; run = bench run duration p50; lost conn = runs with a `disconnect` event; verdicts = runs right/misleading/wrong by their worst finding.

| scenario | outcome d/r/f | RTT ms | up Mbps | down Mbps | conc Mbps | bind ms | run s | lost conn | verdicts r/m/w |
|---|---|---|---|---|---|---|---|---|---|
| lab-public-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.5 → 123.0 | 123.6 → 122.9 | 156.0 → 155.8 | 608 → 609 | 20.3 → 22.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.7 → 122.5 | 123.6 → 123.7 | 156.1 → 156.0 | 610 → 609 | 20.3 → 22.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-sym-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.8 → 122.6 | 123.3 → 122.7 | 156.2 → 155.9 | 609 → 609 | 20.4 → 22.4 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-easy-sym | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.5 → 123.3 | 124.3 → 123.5 | 156.1 → 156.2 | 609 → 609 | 12.6 → 14.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-pr-pr | 3/0/0 → 3/0/0 | 40.5 → 40.4 | 123.0 → 121.8 | 123.4 → 123.6 | 156.1 → 156.0 | 608 → 608 | 20.4 → 22.4 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-sym-sym | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.6 → 35.4 | 35.4 → 34.9 | 78.0 → 78.1 | 609 → 609 | 13.2 → 15.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-cgnat-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.2 → 123.6 | 123.5 → 123.3 | 156.1 → 156.0 | 609 → 609 | 12.7 → 14.7 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-double-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 124.0 → 124.2 | 123.9 → 124.4 | 156.0 → 155.9 | 609 → 609 | 12.6 → 14.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-udpb | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.6 → 35.7 | 35.2 → 35.4 | 74.3 → 74.2 | 3207 → 3207 | 15.8 → 17.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-udpb-easy | 0/3/0 → 0/3/0 | 80.6 → 80.7 | 35.6 → 35.4 | 35.3 → 35.4 | 74.2 → 74.2 | 609 → 608 | 13.2 → 15.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-pr-sym | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.3 → 35.4 | 35.1 → 35.2 | 74.3 → 74.2 | 609 → 609 | 13.2 → 15.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-rc-sym | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.8 → 123.2 | 122.7 → 122.9 | 156.1 → 155.9 | 608 → 608 | 12.7 → 14.7 | 0 → 0 | 1/2/0 → 3/0/0 |
| lab-netem-pr | 3/0/0 → 3/0/0 | 120.4 → 120.4 | 18.8 → 1.5 | 1.2 → 2.6 | - → 1.0 | 1329 → 1329 | 63.6 → 69.0 | 0 → 0 | 1/2/0 → 2/1/0 |
| lab-idle30-pr | 3/0/0 → 3/0/0 | 40.5 → 40.4 | 123.1 → 122.3 | 122.7 → 123.8 | 156.0 → 156.1 | 608 → 608 | 72.7 → 74.7 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-idle5-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.8 → 121.9 | 123.4 → 123.6 | 156.3 → 156.2 | 608 → 609 | 72.7 → 74.7 | 0 → 0 | 0/3/0 → 3/0/0 |
| lab-natflush-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 121.4 → 122.7 | 123.4 → 122.9 | 156.1 → 156.0 | 608 → 609 | 72.8 → 74.8 | 0 → 0 | 0/3/0 → 3/0/0 |
| rw-easy-to-ufw | 3/0/0 → 3/0/0 | 37.1 → 34.5 | 134.2 → 140.4 | 133.3 → 134.5 | 261.3 → 279.6 | 249 → 248 | 11.8 → 13.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-easy-to-open | 3/0/0 → 3/0/0 | 37.1 → 37.2 | 133.9 → 132.8 | 132.5 → 132.9 | 262.5 → 155.9 | 248 → 248 | 17.9 → 20.0 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-symmetric-to-ufw | 0/3/0 → 0/3/0 | 37.3 → 35.1 | 78.6 → 82.1 | 76.8 → 80.3 | 155.3 → 154.5 | 248 → 248 | 11.4 → 13.4 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-symmetric-to-open | 3/0/0 → 3/0/0 | 34.1 → 36.9 | 147.3 → 134.4 | 147.5 → 134.8 | 280.0 → 279.9 | 250 → 250 | 11.6 → 13.7 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-udp-blocked-to-ufw | 0/3/0 → 0/3/0 | 34.2 → 37.4 | 85.5 → 77.3 | 85.8 → 76.4 | 155.2 → 154.8 | 3008 → 3008 | 14.0 → 16.1 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-udp-blocked-to-open | 0/3/0 → 0/3/0 | 37.5 → 37.7 | 77.0 → 76.5 | 77.0 → 76.9 | 155.1 → 131.1 | 3007 → 3008 | 14.1 → 16.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-ufw-to-easy | 0/3/0 → 0/3/0 | 34.5 → 37.1 | 83.9 → 75.8 | 85.0 → 79.2 | 154.1 → 154.6 | 620 → 615 | 11.8 → 13.9 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-open-to-easy | 3/0/0 → 3/0/0 | 37.2 → 34.0 | 134.9 → 145.6 | 134.5 → 146.5 | 156.2 → 280.0 | 601 → 622 | 12.5 → 14.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-ufw-to-symmetric | 0/3/0 → 0/3/0 | 37.9 → 37.1 | 75.9 → 77.7 | 77.6 → 78.8 | 152.9 → 154.6 | 610 → 616 | 11.9 → 13.9 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-open-to-symmetric | 3/0/0 → 3/0/0 | 36.9 → 37.0 | 134.9 → 136.5 | 134.2 → 134.8 | 156.3 → 156.2 | 593 → 621 | 12.4 → 14.4 | 0 → 0 | 2/1/0 → 3/0/0 |
| startup-hetzner-qad-56001 | - → - | - → - | - → - | - → - | - → - | 609 → 608 | 0.7 → 0.7 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-56001 | - → - | - → - | - → - | - → - | - → - | 249 → 248 | 0.3 → 0.2 | - → - | 3/0/0 → 3/0/0 |
| startup-hetzner-qad-7842 | - → - | - → - | - → - | - → - | - → - | 3225 → 3234 | 3.4 → 3.4 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-7842 | - → - | - → - | - → - | - → - | - → - | 3007 → 3008 | 3.0 → 3.0 | - → - | 3/0/0 → 3/0/0 |
| startup-hetzner-qad-off | - → - | - → - | - → - | - → - | - → - | 583 → 619 | 0.7 → 0.8 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-off | - → - | - → - | - → - | - → - | - → - | 249 → 249 | 0.2 → 0.3 | - → - | 3/0/0 → 3/0/0 |
| relay-single | 0/3/0 → 0/3/0 | 67.6 → 71.1 | 42.4 → 40.5 | 42.1 → 40.3 | 73.9 → 78.0 | 513 → 523 | 12.8 → 14.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| relay-parallel2 | 0/4/0 → 0/4/0 | 67.4 → 72.2 | 42.8 → 39.4 | 41.3 → 38.8 | - → - | 504 → 516 | 12.3 → 14.5 | 0 → 0 | 4/0/0 → 4/0/0 |
| relay-parallel4 | 0/8/0 → 0/8/0 | 72.3 → 72.1 | 39.6 → 39.5 | 39.1 → 39.7 | - → - | 510 → 522 | 12.4 → 14.5 | 0 → 0 | 8/0/0 → 8/0/0 |

## Flags

Changes beyond the noise thresholds (relative and absolute: RTT 10 % and 3 ms, throughput (also concurrency) 25 % and 5 Mbps, bind 25 % and 100 ms, run duration 25 % and 3 s), outcome and verdict changes.

* `lab-sym-easy`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-pr-pr`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-cgnat-easy`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-rc-sym`: improvement: verdicts r/m/w 1/2/0 -> 3/0/0
* `lab-netem-pr`: regression: upload_mbps 18.797 -> 1.544 Mbps; change: concurrency_mbps None -> 0.981; improvement: verdicts r/m/w 1/2/0 -> 2/1/0
* `lab-idle30-pr`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-idle5-pr`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `lab-natflush-pr`: improvement: verdicts r/m/w 0/3/0 -> 3/0/0
* `rw-easy-to-open`: regression: concurrency_mbps 262.531 -> 155.913 Mbps
* `rw-open-to-easy`: improvement: concurrency_mbps 156.195 -> 280.007 Mbps
* `rw-open-to-symmetric`: improvement: verdicts r/m/w 2/1/0 -> 3/0/0

## Misleading and wrong verdict checks (base → new)

* `lab-sym-easy`: misleading:packet_loss_unshaped x3 → none
* `lab-pr-pr`: misleading:packet_loss_unshaped x3 → none
* `lab-cgnat-easy`: misleading:packet_loss_unshaped x3 → none
* `lab-rc-sym`: misleading:packet_loss_unshaped x2 → none
* `lab-netem-pr`: misleading:packet_loss_missed, misleading:relay_fallback_failed_but_works x2 → misleading:packet_loss_missed
* `lab-idle30-pr`: misleading:packet_loss_unshaped x3 → none
* `lab-idle5-pr`: misleading:packet_loss_unshaped x3 → none
* `lab-natflush-pr`: misleading:packet_loss_unshaped x3 → none
* `rw-open-to-symmetric`: misleading:packet_loss_unshaped → none
