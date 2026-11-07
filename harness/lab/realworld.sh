#!/usr/bin/env bash
# Experiment 2: a lab node behind an emulated NAT whose traffic leaves through
# the lab VM's real uplink, talking to a public node on this machine, through
# the real relay. Run on the machine that plays the public node (it needs ssh
# to the lab VM and to the relay VM, and a built target/release/golem-p2p).
#
#   harness/lab/realworld.sh [--repeat N] [--duration S] [--nodes "real1 real2"]
#                            [--local-bind ADDR] [--out DIR] [--natcheck]
#
# Prerequisite on the lab VM: `lab.sh up real` (nodes with uplink=real).
#
# What it does, for every lab node and repeat, in both directions:
#   1. a temporary HTTP directory on the relay VM (systemd transient unit
#      gpl-rw-directory, port GPL_RW_DIR_PORT, stopped at the end and capped
#      with RuntimeMaxSec), reachable by both sides at <public ip>:<port>
#   2. listen here + send from the lab node, then listen in the lab node +
#      send from here
#   3. summary lines like lab.sh pair
# With --natcheck it first measures the NAT behaviour of the real path with
# tools/natcheck.py against reflectors on the relay VM (two forwarded ports,
# reached through the i3D host's hairpin) and here (--natcheck-local-port, a
# UDP port this machine's firewall already lets in).
#
# Everything it starts is stopped on exit (trap), also on Ctrl-C.
set -euo pipefail
export LC_ALL=C

SSH_CONFIG=${GPL_SSH_CONFIG:-$HOME/.ssh/p2p/ssh_config}
LAB_HOST=${GPL_HOST:-p2p-natlab}
REMOTE_DIR=${GPL_REMOTE_DIR:-golem-p2p-lab}
RELAY_HOST=${GPL_RELAY_HOST:-p2p-relay1}
PUBLIC_IP=${GPL_RELAY_PUBLIC_IP:-213.163.75.157}
REAL_RELAY=${GPL_REAL_RELAY:-https://$PUBLIC_IP:56000}
REAL_CA=${GPL_REAL_RELAY_CA_SRC:-$HOME/.ssh/p2p/p2p-relay1.crt}
DIR_PORT=${GPL_RW_DIR_PORT:-56050}
REFL_PORTS=${GPL_RW_REFL_PORTS:-56090,56091}

repeat=3 dur=30 nodes="real1" local_bind="" out="" natcheck=0 natcheck_local_port=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --repeat) repeat=$2; shift 2 ;;
        --duration) dur=$2; shift 2 ;;
        --nodes) nodes=$2; shift 2 ;;
        --local-bind) local_bind=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --natcheck) natcheck=1; shift ;;
        --natcheck-local-port) natcheck_local_port=$2; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
BIN=${GOLEM_P2P_BIN:-$repo/target/release/golem-p2p}
[[ -x $BIN ]] || { echo "build first: cargo build --release -p golem-p2p-cli" >&2; exit 1; }
out=${out:-$HOME/gpl-runs/realworld-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$out/keys"
DIRURL=http://$PUBLIC_IP:$DIR_PORT/

lab() { ssh -F "$SSH_CONFIG" "$LAB_HOST" "$@"; }
rel() { ssh -F "$SSH_CONFIG" "$RELAY_HOST" "$@"; }
say() { echo "realworld: $*" >&2; }

LOCAL_PIDS=()
LAB_TAGS=()
cleanup() {
    set +e
    for p in "${LOCAL_PIDS[@]}"; do kill "$p" 2>/dev/null; done
    for t in "${LAB_TAGS[@]}"; do lab "cd $REMOTE_DIR && ./lab.sh stop $t" 2>/dev/null; done
    rel "sudo systemctl stop gpl-rw-directory gpl-rw-reflector 2>/dev/null; sudo systemctl reset-failed gpl-rw-directory gpl-rw-reflector 2>/dev/null; rm -rf /tmp/gpl-rw" 2>/dev/null
    say "cleaned up (local processes, lab listeners, relay VM units)"
}
trap cleanup EXIT

# 1. Temporary directory (and reflector) on the relay VM.
say "starting directory on $RELAY_HOST:$DIR_PORT"
rel "mkdir -p /tmp/gpl-rw && cat > /tmp/gpl-rw/golem-p2p && chmod 755 /tmp/gpl-rw/golem-p2p" <"$BIN"
rel "sudo systemd-run --quiet --unit=gpl-rw-directory -p User=$(rel 'id -un') -p RuntimeMaxSec=3600 \
    /tmp/gpl-rw/golem-p2p directory --listen 0.0.0.0:$DIR_PORT"
for _ in $(seq 50); do
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "${DIRURL}v1/records/0x0000000000000000000000000000000000000000" || true)
    [[ $code == 404 ]] && break
    sleep 0.2
done
[[ $code == 404 ]] || { say "directory not reachable at $DIRURL"; exit 1; }

if ((natcheck)); then
    say "natcheck: reflector on $RELAY_HOST ports $REFL_PORTS"
    rel "cat > /tmp/gpl-rw/reflector.py" <"$here/tools/reflector.py"
    args=""
    for p in ${REFL_PORTS//,/ }; do args+=" --udp 0.0.0.0:$p"; done
    rel "sudo systemd-run --quiet --unit=gpl-rw-reflector -p User=$(rel 'id -un') -p RuntimeMaxSec=600 \
        /usr/bin/python3 -u /tmp/gpl-rw/reflector.py $args"
    maps=""
    for p in ${REFL_PORTS//,/ }; do maps+=" --map $PUBLIC_IP:$p"; done
    if [[ -n $natcheck_local_port ]]; then
        python3 -u "$here/tools/reflector.py" --udp "0.0.0.0:$natcheck_local_port" >"$out/local-reflector.log" 2>&1 &
        LOCAL_PIDS+=($!)
        sleep 0.5
        myip=$(ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')
        maps+=" --map $myip:$natcheck_local_port"
    fi
    sleep 1
    for n in $nodes; do
        for i in 1 2 3; do
            r=$(lab "cd $REMOTE_DIR && ./lab.sh exec $n python3 tools/natcheck.py $maps --filt $PUBLIC_IP:${REFL_PORTS%%,*}")
            echo "natcheck node=$n try=$i $r" | tee -a "$out/natcheck.txt"
        done
    done
    rel "sudo systemctl stop gpl-rw-reflector" || true
    if [[ -n $natcheck_local_port ]]; then kill "${LOCAL_PIDS[-1]}" 2>/dev/null || true; fi
fi

stamp() { while IFS= read -r l; do printf '%s %s\n' "$EPOCHREALTIME" "$l"; done; }
local_key() { [[ -f $out/keys/$1.key ]] || "$BIN" keygen --out "$out/keys/$1.key" >"$out/keys/$1.id"; cat "$out/keys/$1.id"; }
LOCAL_ARGS=(--relay "$REAL_RELAY" --relay-ca "$REAL_CA" --directory "$DIRURL")
[[ -n $local_bind ]] && LOCAL_ARGS+=(--bind "$local_bind")

for r in $(seq "$repeat"); do
    for n in $nodes; do
        # A: listen here, send from the lab node.
        d=$out/run$r-local-$n
        mkdir -p "$d"
        lid=$(local_key local)
        NO_COLOR=1 timeout -k 5 $((dur + 60)) "$BIN" listen --key "$out/keys/local.key" "${LOCAL_ARGS[@]}" \
            >"$d/listen.log" 2>&1 </dev/null &
        lpid=$!
        LOCAL_PIDS+=("$lpid")
        for _ in $(seq 100); do grep -q '^READY' "$d/listen.log" && break; sleep 0.1; done
        grep '^READY' "$d/listen.log" >&2 || { say "local listener not ready"; tail "$d/listen.log" >&2; exit 1; }
        line=$(lab "cd $REMOTE_DIR && ./lab.sh send $n --to $lid --duration $dur --out ~/gpl-runs/$(basename "$out")/run$r-local-$n --directory $DIRURL" | tail -1)
        kill "$lpid" 2>/dev/null || true
        wait "$lpid" 2>/dev/null || true
        echo "run=$r listener=local $line" | tee -a "$out/results.txt"

        # B: listen in the lab node, send from here.
        d=$out/run$r-$n-local
        mkdir -p "$d"
        tag=rw-listen-$n
        LAB_TAGS+=("$tag")
        ready=$(lab "cd $REMOTE_DIR && ./lab.sh listen $n --tag $tag --directory $DIRURL")
        echo "$ready" >"$d/lab-ready.txt"
        nid=$(sed -n 's/^READY node_id=\([^ ]*\).*/\1/p' <<<"$ready")
        [[ -n $nid ]] || { say "lab listener not ready: $ready"; exit 1; }
        local_key localsend >/dev/null
        start=$EPOCHREALTIME
        { NO_COLOR=1 timeout -s TERM -k 5 "$dur" "$BIN" send --key "$out/keys/localsend.key" "${LOCAL_ARGS[@]}" \
            --to "$nid" --count 100000000 ping 2>"$d/send.err" </dev/null || true; } | stamp >"$d/send.out"
        lab "cd $REMOTE_DIR && cat /run/gpl/logs/$tag.log; ./lab.sh stop $tag" >"$d/listen.log" 2>&1 || true
        s=$(python3 "$here/tools/summarize.py" --start "$start" --out "$d/send.out" --err "$d/send.err" --listen "$d/listen.log")
        echo "run=$r listener=$n sender=local $s" | tee -a "$out/results.txt"
    done
done
say "results in $out/results.txt"
