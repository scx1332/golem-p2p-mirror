#!/usr/bin/env bash
# Runs on the build machine after the campaign (directory on the relay VM
# still running): a fresh `lab.sh up campaign-real`, then the direct
# real-world scenarios with the Hetzner listener (lab bench -> Hetzner), as
# campaign4.sh's realworld step runs them, 3 rounds. Checks whether the slow
# Hetzner -> lab downloads (13.7-15.6 Mbps in 3 direct runs at 01:34-01:36
# UTC, the first minutes of the realworld step) come back, e.g. again right
# after the lab is rebuilt. Writes run<r>-<scenario>/ next to this script
# (bench files fetched from the lab VM, listen.jsonl/err from here) and
# load.txt (this machine's load average per run).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
BIN=$repo/target/release/golem-p2p
SSH_CONFIG=$HOME/.ssh/p2p/ssh_config
REAL_RELAY=https://213.163.75.157:56000,quic=56001
REAL_CA=$HOME/.ssh/p2p/p2p-relay1.crt
DIRURL=http://213.163.75.157:56050/
KEYS=$here/.keys
mkdir -p "$KEYS"
lab() { ssh -F "$SSH_CONFIG" -o ServerAliveInterval=15 p2p-natlab "$@"; }

[[ $(sha256sum "$BIN" | cut -d' ' -f1) == $(lab "sha256sum golem-p2p-lab/bin/golem-p2p | cut -d' ' -f1") ]] ||
    { echo "binary mismatch" >&2; exit 1; }
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "${DIRURL}v1/records/0x0000000000000000000000000000000000000000" || true)
[[ $code == 404 ]] || { echo "directory not running ($code)" >&2; exit 1; }
echo "rw-recheck: $(date -u +%T) lab up campaign-real" >&2
lab "cd golem-p2p-lab && ./lab.sh up campaign-real >/dev/null"
for r in 1 2 3; do
    for s in "rw-easy-to-ufw real1 ufw" "rw-symmetric-to-open real2 open" "rw-easy-to-open real1 open"; do
        read -r id n v <<<"$s"
        d=$here/run$r-$id
        mkdir -p "$d"
        bind=()
        [[ $v == open ]] && bind=(--bind 0.0.0.0:8000)
        [[ -f $KEYS/hetzner-$v.key ]] || "$BIN" keygen --out "$KEYS/hetzner-$v.key" >/dev/null
        echo "$(date -u +%FT%TZ) $id run$r $(cut -d' ' -f1-4 /proc/loadavg)" >>"$here/load.txt"
        NO_COLOR=1 timeout -s TERM -k 5 300 "$BIN" --json --run-id "recheck-$id-r$r" --node-name hetzner listen \
            --key "$KEYS/hetzner-$v.key" --relay "$REAL_RELAY" --relay-ca "$REAL_CA" --directory "$DIRURL" "${bind[@]}" \
            >"$d/listen.jsonl" 2>"$d/listen.err" </dev/null &
        lpid=$!
        lid=""
        for _ in $(seq 150); do
            lid=$(sed -n 's/^{.*"event":"ready".*"node_id":"\(0x[0-9a-f]*\)".*/\1/p' "$d/listen.jsonl" | head -1)
            [[ -n $lid ]] && break
            sleep 0.1
        done
        [[ -n $lid ]] || { echo "listener not ready" >&2; kill -TERM $lpid; continue; }
        lab "cd golem-p2p-lab && ./lab.sh bench $n --to $lid --out ~/gpl-runs/rw-recheck/run$r-$id --run-id recheck-$id-r$r --directory $DIRURL" \
            2>&1 >/dev/null | tail -n 1 | cut -c1-400 >&2 || true
        kill -TERM $lpid 2>/dev/null || true
        wait $lpid 2>/dev/null || true
    done
done
lab "tar -C ~/gpl-runs/rw-recheck -cf - ." | tar -C "$here" -xf -
lab "rm -rf ~/gpl-runs/rw-recheck"
echo "rw-recheck: $(date -u +%T) done" >&2
