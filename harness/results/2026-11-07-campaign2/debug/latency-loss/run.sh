#!/usr/bin/env bash
# How debug/latency-loss was captured (2026-11-07 22:12 UTC, after the campaign,
# same binary): why the packet_loss hint fires on clean lab links. A
# port-restricted pair (relay first, then hole punching) in the campaign lab,
# latency phase only, with noq's packet-level trace on the bench side.
# Result: 3 of 127 latency-phase packets "lost", all within 75 ms after the
# connect phase (trace-excerpt.txt; the full 2579-line trace was not kept).
set -euo pipefail
ssh -F ~/.ssh/p2p/ssh_config p2p-natlab '
cd golem-p2p-lab && ./lab.sh up campaign >/dev/null
mkdir -p ~/gpl-runs/dbg-loss
id=$(./lab.sh listen pr1 --tag dbg -- --json --run-id dbg-loss | sed -n "s/.*\"node_id\":\"\(0x[0-9a-f]*\)\".*/\1/p")
./lab.sh exec pr2 sh -c "RUST_LOG=noq_proto=trace,iroh=debug,info golem-p2p --json bench --to $id \
    --relay \$GPL_RELAY --relay-ca \$GPL_RELAY_CA --directory \$GPL_DIRECTORY --skip upload,download,concurrency \
    > ~/gpl-runs/dbg-loss/bench.jsonl 2> ~/gpl-runs/dbg-loss/bench.err"
./lab.sh stop dbg
grep "packets lost" ~/gpl-runs/dbg-loss/bench.err
'
