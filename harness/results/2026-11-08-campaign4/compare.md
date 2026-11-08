# Campaign comparison: 2026-11-08-campaign3 → 2026-11-08-campaign4

Base: commit d1bc1cd (harness on top of c270dde; the golem-p2p crates are those of c270dde); new: commit 1484e3a (harness on top of 5c5396a; the golem-p2p crates are those of 5c5396a). Both recomputed from the raw runs with `harness/campaign/summarize.py` (`compare.py`); generated, except the highlights, which come from the new campaign's `notes.json` (`comparison`).

## Highlights

* **Verdict accuracy unchanged: 110 right / 1 misleading / 0 wrong runs (checks 254 / 1 / 0).** The one misleading run is again a sampling miss of 1 % loss on the lossy link (lab-netem-pr run 1: 0 of 118 settled packets lost). Issue, hint, status and exit counts per scenario are identical to campaign 3; no false positives (0 of 11,335 settled packets lost on clean links).
* **Lossy link (lab-netem-pr, 3 campaign + 10 extra runs):** healthy and exit 0 in 13/13 (campaign 3: 12/13); no 'closed stream' or transfer_failed (campaign 3: 1; one upload now waited past the 8 s limit for its count, 11.0 s); concurrency throughput in 13/13 (was 11/13), 0.46-3.64 Mbps, 2 restarts; relay_compare ok 13/13 (as before); packet_loss 9/13 (was 11/13; misses at the expected sampling rate).
* **Fast-path concurrency is measured instead of quantized:** lab direct 185-252 Mbps (was 156 in every run), relayed 74-81, real-world direct 213-330, relay-only 65-87 Mbps; the 'improvement' flags on 12 scenarios are this measurement fix.
* **Regression flag on rw-easy-to-ufw is the environment:** download 134.5 -> 15.6 Mbps, concurrency 279.6 -> 15.2 Mbps, run 13.8 -> 22.3 s: runs 1-2 (and rw-symmetric-to-open run 1) got 13.7-15.6 Mbps Hetzner -> lab at 01:34-01:36 UTC, with this machine's load at 36-45 from other sessions; 9 re-runs at 01:57-02:00 ran at 131-148 Mbps (debug/rw-recheck). Same symptom in campaigns 1 and 3 with older code.
* **Same outcomes, RTT and throughput:** final paths 51 direct / 42 relay / 0 failed, 0 connections lost, QUIC counters in 93/93; lab RTT/throughput unchanged; real-world numbers move with the route clusters; benches on this machine bound ~50-60 ms slower under load; 418.3 MB through the real relay, no drops.
* **New bug:** concurrency requests cut off by a restart or the time limit are not cancelled on the wire (Endpoint::request_from resets only before the request is fully buffered), so their data keeps using the link: after run8's restart the cut-off requests took most of the uplink (0.46 Mbps counted of ~1.2 Mbps sent), and in 7 of 13 lossy runs cut-off requests reached the listener up to 3.9 s after the phase had ended.

## Totals

| | base | new |
|---|---|---|
| scenarios / runs / runs with a report | 35 / 111 / 111 | 35 / 111 / 111 |
| peer runs: final direct / relay / failed | 51 / 42 / 0 | 51 / 42 / 0 |
| runs whose connection closed during the run | 0 | 0 |
| peer runs with QUIC counters at the end | 93 | 93 |
| verdict runs right (by worst finding) | 110 | 110 |
| verdict runs misleading (by worst finding) | 1 | 1 |
| verdict runs wrong (by worst finding) | 0 | 0 |
| verdict checks right | 254 | 254 |
| verdict checks misleading | 1 | 1 |
| verdict checks wrong | 0 | 0 |

## Per scenario (base → new)

Outcome = final path direct/relay/failed per run (failed includes connections gone at the end). RTT = latency-phase p50 pooled over runs; up/down/conc = median throughput (conc: concurrency phase); bind = `ready.bind_ms` p50; run = bench run duration p50; lost conn = runs with a `disconnect` event; verdicts = runs right/misleading/wrong by their worst finding.

| scenario | outcome d/r/f | RTT ms | up Mbps | down Mbps | conc Mbps | bind ms | run s | lost conn | verdicts r/m/w |
|---|---|---|---|---|---|---|---|---|---|
| lab-public-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.0 → 123.0 | 122.9 → 123.9 | 155.8 → 209.4 | 609 → 609 | 22.3 → 22.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.5 → 123.3 | 123.7 → 122.4 | 156.0 → 226.8 | 609 → 609 | 22.3 → 22.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-sym-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.6 → 121.9 | 122.7 → 123.0 | 155.9 → 219.3 | 609 → 609 | 22.4 → 22.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-sym | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.3 → 123.5 | 123.5 → 123.1 | 156.2 → 207.4 | 609 → 609 | 14.6 → 14.5 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-pr-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 121.8 → 122.4 | 123.6 → 123.7 | 156.0 → 189.8 | 608 → 608 | 22.4 → 22.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-sym-sym | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.4 → 35.4 | 34.9 → 35.1 | 78.1 → 78.1 | 609 → 609 | 15.2 → 15.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-cgnat-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.6 → 122.8 | 123.3 → 123.7 | 156.0 → 227.5 | 609 → 609 | 14.7 → 14.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-double-easy | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 124.2 → 122.2 | 124.4 → 123.9 | 155.9 → 223.2 | 609 → 609 | 14.6 → 14.5 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-easy-udpb | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.7 → 35.6 | 35.4 → 35.4 | 74.2 → 75.9 | 3207 → 3207 | 17.8 → 17.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-udpb-easy | 0/3/0 → 0/3/0 | 80.7 → 80.6 | 35.4 → 35.7 | 35.4 → 35.3 | 74.2 → 76.2 | 608 → 608 | 15.2 → 15.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-pr-sym | 0/3/0 → 0/3/0 | 80.6 → 80.6 | 35.4 → 35.7 | 35.2 → 35.3 | 74.2 → 80.6 | 609 → 608 | 15.2 → 15.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-rc-sym | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 123.2 → 122.6 | 122.9 → 123.3 | 155.9 → 201.6 | 608 → 608 | 14.7 → 14.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-netem-pr | 3/0/0 → 3/0/0 | 120.4 → 120.4 | 1.5 → 2.1 | 2.6 → 4.3 | 1.0 → 0.9 | 1329 → 1329 | 69.0 → 61.6 | 0 → 0 | 2/1/0 → 2/1/0 |
| lab-idle30-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.3 → 123.3 | 123.8 → 123.9 | 156.1 → 223.3 | 608 → 608 | 74.7 → 74.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-idle5-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 121.9 → 122.9 | 123.6 → 123.4 | 156.2 → 219.1 | 609 → 609 | 74.7 → 74.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| lab-natflush-pr | 3/0/0 → 3/0/0 | 40.4 → 40.4 | 122.7 → 122.0 | 122.9 → 123.6 | 156.0 → 240.8 | 609 → 609 | 74.8 → 74.6 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-easy-to-ufw | 3/0/0 → 3/0/0 | 34.5 → 34.3 | 140.4 → 144.1 | 134.5 → 15.6 | 279.6 → 15.2 | 248 → 249 | 13.8 → 22.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-easy-to-open | 3/0/0 → 3/0/0 | 37.2 → 37.2 | 132.8 → 134.1 | 132.9 → 133.2 | 155.9 → 295.2 | 248 → 248 | 20.0 → 20.0 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-symmetric-to-ufw | 0/3/0 → 0/3/0 | 35.1 → 37.2 | 82.1 → 77.3 | 80.3 → 76.8 | 154.5 → 177.0 | 248 → 250 | 13.4 → 13.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-symmetric-to-open | 3/0/0 → 3/0/0 | 36.9 → 34.2 | 134.4 → 141.1 | 134.8 → 131.9 | 279.9 → 276.6 | 250 → 249 | 13.7 → 13.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-udp-blocked-to-ufw | 0/3/0 → 0/3/0 | 37.4 → 34.7 | 77.3 → 83.2 | 76.4 → 82.4 | 154.8 → 168.9 | 3008 → 3007 | 16.1 → 16.1 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-udp-blocked-to-open | 0/3/0 → 0/3/0 | 37.7 → 34.2 | 76.5 → 84.3 | 76.9 → 84.1 | 131.1 → 155.1 | 3008 → 3007 | 16.2 → 16.0 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-ufw-to-easy | 0/3/0 → 0/3/0 | 37.1 → 34.3 | 75.8 → 82.4 | 79.2 → 83.9 | 154.6 → 188.5 | 615 → 602 | 13.9 → 13.8 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-open-to-easy | 3/0/0 → 3/0/0 | 34.0 → 33.8 | 145.6 → 144.2 | 146.5 → 147.1 | 280.0 → 300.4 | 622 → 611 | 14.3 → 14.2 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-ufw-to-symmetric | 0/3/0 → 0/3/0 | 37.1 → 37.5 | 77.7 → 76.8 | 78.8 → 77.0 | 154.6 → 160.1 | 616 → 624 | 13.9 → 13.9 | 0 → 0 | 3/0/0 → 3/0/0 |
| rw-open-to-symmetric | 3/0/0 → 3/0/0 | 37.0 → 34.5 | 136.5 → 142.8 | 134.8 → 145.2 | 156.2 → 295.4 | 621 → 611 | 14.4 → 14.3 | 0 → 0 | 3/0/0 → 3/0/0 |
| startup-hetzner-qad-56001 | - → - | - → - | - → - | - → - | - → - | 608 → 667 | 0.7 → 0.8 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-56001 | - → - | - → - | - → - | - → - | - → - | 248 → 248 | 0.2 → 0.2 | - → - | 3/0/0 → 3/0/0 |
| startup-hetzner-qad-7842 | - → - | - → - | - → - | - → - | - → - | 3234 → 3248 | 3.4 → 3.4 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-7842 | - → - | - → - | - → - | - → - | - → - | 3008 → 3008 | 3.0 → 3.0 | - → - | 3/0/0 → 3/0/0 |
| startup-hetzner-qad-off | - → - | - → - | - → - | - → - | - → - | 619 → 666 | 0.8 → 0.8 | - → - | 3/0/0 → 3/0/0 |
| startup-lab-easy-qad-off | - → - | - → - | - → - | - → - | - → - | 249 → 248 | 0.3 → 0.2 | - → - | 3/0/0 → 3/0/0 |
| relay-single | 0/3/0 → 0/3/0 | 71.1 → 74.1 | 40.5 → 38.2 | 40.3 → 38.3 | 78.0 → 70.5 | 523 → 584 | 14.8 → 15.0 | 0 → 0 | 3/0/0 → 3/0/0 |
| relay-parallel2 | 0/4/0 → 0/4/0 | 72.2 → 71.6 | 39.4 → 40.5 | 38.8 → 40.0 | - → - | 516 → 532 | 14.5 → 14.4 | 0 → 0 | 4/0/0 → 4/0/0 |
| relay-parallel4 | 0/8/0 → 0/8/0 | 72.1 → 71.5 | 39.5 → 40.5 | 39.7 → 39.7 | - → - | 522 → 525 | 14.5 → 14.4 | 0 → 0 | 8/0/0 → 8/0/0 |

## Flags

Changes beyond the noise thresholds (relative and absolute: RTT 10 % and 3 ms, throughput (also concurrency) 25 % and 5 Mbps, bind 25 % and 100 ms, run duration 25 % and 3 s), outcome and verdict changes.

* `lab-public-easy`: improvement: concurrency_mbps 155.839 -> 209.429 Mbps
* `lab-easy-easy`: improvement: concurrency_mbps 155.979 -> 226.822 Mbps
* `lab-sym-easy`: improvement: concurrency_mbps 155.945 -> 219.282 Mbps
* `lab-easy-sym`: improvement: concurrency_mbps 156.176 -> 207.356 Mbps
* `lab-cgnat-easy`: improvement: concurrency_mbps 156 -> 227.526 Mbps
* `lab-double-easy`: improvement: concurrency_mbps 155.939 -> 223.185 Mbps
* `lab-rc-sym`: improvement: concurrency_mbps 155.895 -> 201.58 Mbps
* `lab-idle30-pr`: improvement: concurrency_mbps 156.113 -> 223.284 Mbps
* `lab-idle5-pr`: improvement: concurrency_mbps 156.21 -> 219.07 Mbps
* `lab-natflush-pr`: improvement: concurrency_mbps 156.037 -> 240.787 Mbps
* `rw-easy-to-ufw`: regression: download_mbps 134.546 -> 15.611 Mbps; regression: concurrency_mbps 279.606 -> 15.237 Mbps; regression: run_duration_s_p50 13.8 -> 22.3 s
* `rw-easy-to-open`: improvement: concurrency_mbps 155.913 -> 295.167 Mbps
* `rw-open-to-symmetric`: improvement: concurrency_mbps 156.222 -> 295.438 Mbps

## Misleading and wrong verdict checks (base → new)

* `lab-netem-pr`: misleading:packet_loss_missed → misleading:packet_loss_missed
