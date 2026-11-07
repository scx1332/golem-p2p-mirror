#!/usr/bin/env bash
# Benchmark campaign 2: campaign 1's scenarios and run counts again, after the
# bench verdict fixes and "a request timeout no longer closes the whole
# connection", so the results compare with campaign 1 (compare.py).
# `golem-p2p bench --json` in the NAT lab, between lab nodes behind real NATs
# and this machine via the real relay, relay-only through the real relay, and
# startup timings. Run on the build machine (the Hetzner server), which needs
# ssh to the lab VM and the relay VM (see harness/README.md) and a built
# target/release/golem-p2p.
#
# Changes from campaign1.sh: every step that runs golem-p2p in the lab first
# checks that the lab VM has exactly the local binary (bench and listen must
# be the same version: a listener older than the bench cannot answer a
# stopped upload), and deploy records the versions (DIR/versions.txt); the
# lab relay URL carries its QAD port (lab.sh: https://198.18.0.1,quic=7842),
# so lab benches know the port is right, while the startup-*-qad-7842
# scenarios still omit the real relay's port on purpose; summarize also
# writes compare.json/compare.md against campaign 1.
#
#   harness/campaign/campaign2.sh [--out DIR] [--repeat N] <step>...
#
# Steps (in this order for `all`):
#   deploy      build the CLI, copy the lab to the VM (lab/deploy.sh)
#   lab         lab.sh up campaign; selftest; every lab scenario N times
#               (rounds interleaved) with lab.sh benchpair; fetch the runs
#   labextra    NAT "reboot" during --idle: conntrack -F on both routers of a
#               port-restricted pair 20 s into the idle phase (lab still up)
#   realworld   lab.sh up campaign-real; a temporary directory on the relay VM;
#               lab nodes (easy/symmetric/udp-blocked, uplink=real) bench a
#               listener on this machine (random port behind ufw, and UDP
#               8000), and this machine benches listeners in lab nodes; QAD on
#               the real relay's port 56001
#   startup     local-only bench (no --to): bind time and net report with the
#               real relay's QAD port, iroh's default port (unanswered) and QAD
#               off, here and in a lab node via the real uplink
#   relay       relay-only benches between two processes on this machine
#               through the real relay (alone, then 2 and 4 in parallel)
#   summarize   summary.json from the run directories (summarize.py), and
#               compare.json/compare.md against campaign 1 (compare.py)
#   cleanup     lab.sh down, stop the directory on the relay VM, kill local
#               processes started here
#   all         deploy lab labextra realworld startup relay summarize cleanup
#
# Results: DIR/<group>/<scenario>/run<r>/ with bench.jsonl (all events of
# `golem-p2p --json bench`, ready first, bench_report last), bench.err,
# bench.exit, listen.jsonl/listen.err (the peer), meta.json; DIR/scenarios.jsonl
# (one line per scenario: id, group, title, sides, arguments). Default DIR:
# harness/results/<date>-campaign2. Real-relay traffic is read from the relay's
# metrics (DIR/relay-metrics.txt) before and after the steps that use it.
#
# Environment: GPL_SSH_CONFIG, GPL_HOST (lab VM), GPL_RELAY_HOST (relay VM),
# GPL_RELAY_PUBLIC_IP, GPL_REAL_RELAY (URL with ,quic=PORT), GPL_REAL_RELAY_CA_SRC,
# GPL_RW_DIR_PORT (directory port on the relay VM, forwarded publicly),
# GPL_OPEN_UDP_PORT (a UDP port this machine's firewall lets in, default 8000),
# GPL_COMPARE_WITH (the campaign directory to compare with, default campaign 1).
set -euo pipefail
export LC_ALL=C

SSH_CONFIG=${GPL_SSH_CONFIG:-$HOME/.ssh/p2p/ssh_config}
LAB_HOST=${GPL_HOST:-p2p-natlab}
REMOTE_DIR=${GPL_REMOTE_DIR:-golem-p2p-lab}
RELAY_HOST=${GPL_RELAY_HOST:-p2p-relay1}
PUBLIC_IP=${GPL_RELAY_PUBLIC_IP:-213.163.75.157}
REAL_RELAY=${GPL_REAL_RELAY:-https://$PUBLIC_IP:56000,quic=56001}
REAL_RELAY_NOQ=${REAL_RELAY%%,*}
REAL_CA=${GPL_REAL_RELAY_CA_SRC:-$HOME/.ssh/p2p/p2p-relay1.crt}
DIR_PORT=${GPL_RW_DIR_PORT:-56050}
OPEN_PORT=${GPL_OPEN_UDP_PORT:-8000}
DIRURL=http://$PUBLIC_IP:$DIR_PORT/

here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
BIN=${GOLEM_P2P_BIN:-$repo/target/release/golem-p2p}

out="" repeat=3 steps=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --out) out=$2; shift 2 ;;
        --repeat) repeat=$2; shift 2 ;;
        -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        *) steps+=("$1"); shift ;;
    esac
done
((${#steps[@]})) || { echo "usage: campaign2.sh [--out DIR] [--repeat N] <step>... (--help)" >&2; exit 2; }
out=${out:-$repo/harness/results/$(date -u +%Y-%m-%d)-campaign2}
mkdir -p "$out"
out=$(cd "$out" && pwd)
CAMP=$(basename "$out")              # run directory name on the lab VM: ~/gpl-runs/$CAMP
KEYS=$out/.keys                      # local keys (not committed)
mkdir -p "$KEYS"

lab() { ssh -F "$SSH_CONFIG" -o ServerAliveInterval=15 "$LAB_HOST" "$@"; }
rel() { ssh -F "$SSH_CONFIG" -o ServerAliveInterval=15 "$RELAY_HOST" "$@"; }
say() { echo "campaign: $(date -u +%H:%M:%S) $*" >&2; }

LOCAL_PIDS=()
cleanup_local() {
    local p
    for p in "${LOCAL_PIDS[@]}"; do kill -TERM "$p" 2>/dev/null || true; done
    LOCAL_PIDS=()
}
trap cleanup_local EXIT

# scenario <id> <group> <title> <a json> <b json> <netem|null> <args>: records a
# scenario definition in scenarios.jsonl (replacing an older line of that id).
scenario() {
    python3 - "$out/scenarios.jsonl" "$@" <<'EOF'
import json, sys
path, sid, group, title, a, b, netem, args = sys.argv[1:9]
rows = []
try:
    rows = [json.loads(l) for l in open(path) if l.strip()]
except FileNotFoundError:
    pass
rows = [r for r in rows if r["id"] != sid]
rows.append({"id": sid, "group": group, "title": title, "a": json.loads(a), "b": json.loads(b),
             "netem": None if netem in ("", "null") else netem, "args": args})
with open(path, "w") as f:
    for r in rows:
        f.write(json.dumps(r) + "\n")
EOF
}

relay_metrics() { # label
    {
        echo "== $1 $(date -u +%FT%TZ)"
        rel "curl -s localhost:9090/metrics | grep -E '^relayserver_(bytes_(sent|recv)_total|accepts_total|send_packets_(sent|recv|dropped)_total|qad_connections_total)'" || echo "metrics unavailable"
    } >>"$out/relay-metrics.txt"
}

# ------------------------------------------------------------------ deploy

step_deploy() {
    (cd "$repo" && cargo build --release -p golem-p2p-cli)
    GPL_SKIP_BUILD=1 "$repo/harness/lab/deploy.sh"
    check_versions
    local dirty=""
    git -C "$repo" status --porcelain | grep -q . && dirty=-dirty
    {
        echo "built from: $(git -C "$repo" rev-parse --short HEAD)$dirty at $(date -u +%FT%TZ)"
        echo "local: $("$BIN" --version), sha256 $(sha256sum "$BIN" | cut -d' ' -f1)"
        echo "lab:   $(lab "cd $REMOTE_DIR && bin/golem-p2p --version && sha256sum bin/golem-p2p && cat DEPLOYED" | tr '\n' ' ')"
        echo "relay VM directory: copied from the local binary whenever it starts (start_directory)"
    } >"$out/versions.txt"
    cat "$out/versions.txt" >&2
}

# Bench and listen must be the same version everywhere: refuse to run with a
# lab binary that differs from the local one (the relay VM's directory is
# always copied from the local binary).
check_versions() {
    local mine theirs
    mine=$(sha256sum "$BIN" | cut -d' ' -f1)
    theirs=$(lab "sha256sum $REMOTE_DIR/bin/golem-p2p | cut -d' ' -f1")
    if [[ $mine != "$theirs" ]]; then
        say "the lab's golem-p2p ($theirs) differs from $BIN ($mine): run the deploy step first"
        exit 1
    fi
}

# ------------------------------------------------------------------ lab

# id | listener | bencher | extra bench args | title
LAB_SCENARIOS=(
    "lab-public-easy|pub1|easy1|--compare-relay|public (listener) <- easy (bench)"
    "lab-easy-easy|easy1|easy2|--compare-relay|easy <- easy"
    "lab-sym-easy|sym1|easy1|--compare-relay|symmetric (listener) <- easy (bench)"
    "lab-easy-sym|easy1|sym1||easy (listener) <- symmetric (bench)"
    "lab-pr-pr|pr1|pr2|--compare-relay|port-restricted <- port-restricted"
    "lab-sym-sym|sym1|sym2||symmetric <- symmetric"
    "lab-cgnat-easy|cgn1|easy1||cgnat (listener) <- easy (bench)"
    "lab-double-easy|dbl1|easy1||double NAT (listener) <- easy (bench)"
    "lab-easy-udpb|easy1|udpb1||easy (listener) <- udp-blocked (bench)"
    "lab-udpb-easy|udpb1|easy1||udp-blocked (listener) <- easy (bench)"
    "lab-pr-sym|pr1|sym1||port-restricted (listener) <- symmetric (bench)"
    "lab-rc-sym|rc1|sym1||restricted (listener) <- symmetric (bench)"
    "lab-netem-pr|pr2|lossy1|--compare-relay|port-restricted <- port-restricted on a bad link (50 ms, 1 % loss each way)"
    "lab-idle30-pr|idle1|idle2|--idle 60s|port-restricted <- port-restricted, NAT mapping timeout 30 s, 60 s idle"
    "lab-idle5-pr|idle3|idle4|--idle 60s|port-restricted <- port-restricted, NAT mapping timeout 5 s, 60 s idle"
)

lab_node_json() { # node profile role [netem] [udp_timeout]
    printf '{"role":"%s","node":"%s","profile":"%s","location":"lab (emulated NAT, local relay)","link":"%s","udp_timeout_s":%s}' \
        "$3" "$1" "$2" "${4:-delay=10ms each way}" "${5:-120}"
}

step_lab() {
    check_versions
    say "lab: up campaign + selftest"
    lab "cd $REMOTE_DIR && ./lab.sh up campaign >/dev/null && ./lab.sh selftest" | tee "$out/lab-selftest.txt"
    lab "cd $REMOTE_DIR && ./lab.sh status && cat DEPLOYED" >"$out/lab-status.txt"
    lab "cat $REMOTE_DIR/topologies/campaign.topo" >"$out/lab-campaign.topo"
    local nodes
    nodes=$(lab "cd $REMOTE_DIR && ./lab.sh nodes")
    local s id l b args title
    for s in "${LAB_SCENARIOS[@]}"; do
        IFS='|' read -r id l b args title <<<"$s"
        local lp bp netem=null ln="" bn="" lt=120 bt=120
        lp=$(awk -v n="$l" '$1 == n {print $2}' <<<"$nodes")
        bp=$(awk -v n="$b" '$1 == n {print $2}' <<<"$nodes")
        case $b in lossy1) netem="delay=50ms loss=1% each way on the bench node's access link"; bn="delay=50ms loss=1% each way" ;; esac
        case $l in idle1 | idle2) lt=30 ;; idle3 | idle4) lt=5 ;; esac
        case $b in idle1 | idle2) bt=30 ;; idle3 | idle4) bt=5 ;; esac
        scenario "$id" lab "$title" "$(lab_node_json "$b" "$bp" bench "$bn" "$bt")" \
            "$(lab_node_json "$l" "$lp" listener "$ln" "$lt")" "$netem" "$args"
    done
    local r
    for r in $(seq "$repeat"); do
        for s in "${LAB_SCENARIOS[@]}"; do
            IFS='|' read -r id l b args title <<<"$s"
            say "lab: round $r $id"
            # shellcheck disable=SC2086
            lab "cd $REMOTE_DIR && ./lab.sh benchpair $l $b --out ~/gpl-runs/$CAMP/lab/$id/run$r --run-id $id-r$r -- $args" \
                >/dev/null 2>>"$out/lab-progress.txt" || say "lab: $id run $r: no report"
            tail -n 1 "$out/lab-progress.txt" | cut -c1-400 >&2
        done
    done
    fetch_lab lab
}

# NAT "reboot" during --idle: both routers of a port-restricted pair lose all
# their mappings (conntrack -F) 20 s into the idle phase, so the direct path
# and the relay connection lose their NAT state while no data flows. Needs the
# campaign lab (step lab) still up.
step_labextra() {
    check_versions
    local id=lab-natflush-pr l=pr1 b=pr2 r
    scenario "$id" lab "port-restricted <- port-restricted, NAT state of both routers flushed 20 s into a 60 s idle" \
        "$(lab_node_json "$b" port-restricted bench)" "$(lab_node_json "$l" port-restricted listener)" \
        "conntrack -F on both NAT routers 20 s into the idle phase" "--idle 60s"
    for r in $(seq "$repeat"); do
        say "lab: round $r $id"
        local rd="~/gpl-runs/$CAMP/lab/$id/run$r"
        lab "cd $REMOTE_DIR && rm -rf $rd && ./lab.sh benchpair $l $b --out $rd --run-id $id-r$r -- --idle 60s" \
            >/dev/null 2>>"$out/lab-progress.txt" &
        local bp=$!
        lab "for i in \$(seq 600); do grep -q '\"phase\":\"concurrency\"' $rd/bench.jsonl 2>/dev/null && break; sleep 0.5; done; sleep 20; \
            cd $REMOTE_DIR && ./lab.sh exec --root $l-r1 conntrack -F && ./lab.sh exec --root $b-r1 conntrack -F && date -u +%FT%T.%3NZ" \
            >"$out/.flush-$r.txt" 2>&1 || say "lab: flush failed"
        wait "$bp" || say "lab: $id run $r: no report"
        lab "cat >> $rd/flush.txt" <"$out/.flush-$r.txt"
        rm -f "$out/.flush-$r.txt"
        tail -n 1 "$out/lab-progress.txt" | cut -c1-400 >&2
    done
    fetch_lab lab
}

fetch_lab() { # subdir
    mkdir -p "$out/$1"
    lab "tar -C ~/gpl-runs/$CAMP -cf - $1" | tar -C "$out" -xf -
}

# ------------------------------------------------------------------ realworld

start_directory() {
    say "directory on $RELAY_HOST:$DIR_PORT"
    # Stop a directory from an earlier step first: its binary is replaced.
    rel "sudo systemctl stop gpl-rw-directory 2>/dev/null; sudo systemctl reset-failed gpl-rw-directory 2>/dev/null; \
        mkdir -p /tmp/gpl-rw && cat > /tmp/gpl-rw/golem-p2p.new && chmod 755 /tmp/gpl-rw/golem-p2p.new && mv /tmp/gpl-rw/golem-p2p.new /tmp/gpl-rw/golem-p2p" <"$BIN"
    rel "sudo systemd-run --quiet --unit=gpl-rw-directory -p User=\$(id -un) -p RuntimeMaxSec=7200 \
        /tmp/gpl-rw/golem-p2p directory --listen 0.0.0.0:$DIR_PORT"
    local code=""
    for _ in $(seq 50); do
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "${DIRURL}v1/records/0x0000000000000000000000000000000000000000" || true)
        [[ $code == 404 ]] && return 0
        sleep 0.2
    done
    say "directory not reachable at $DIRURL"
    return 1
}

stop_directory() {
    rel "sudo systemctl stop gpl-rw-directory 2>/dev/null; sudo systemctl reset-failed gpl-rw-directory 2>/dev/null; rm -rf /tmp/gpl-rw" || true
}

local_key() { # name -> node id
    [[ -f $KEYS/$1.key ]] || "$BIN" keygen --out "$KEYS/$1.key" >"$KEYS/$1.id"
    cat "$KEYS/$1.id"
}

# start_local_listener <dir> <run id> <key name> <args...>: sets LPID, LID.
start_local_listener() {
    local d=$1 run=$2 key=$3
    shift 3
    mkdir -p "$d"
    local_key "$key" >/dev/null
    NO_COLOR=1 timeout -s TERM -k 5 900 "$BIN" --json --run-id "$run" --node-name hetzner listen \
        --key "$KEYS/$key.key" "$@" >"$d/listen.jsonl" 2>"$d/listen.err" </dev/null &
    LPID=$!
    LOCAL_PIDS+=("$LPID")
    LID=""
    for _ in $(seq 150); do
        LID=$(sed -n 's/^{.*"event":"ready".*"node_id":"\(0x[0-9a-f]*\)".*/\1/p' "$d/listen.jsonl" | head -1)
        [[ -n $LID ]] && return 0
        kill -0 "$LPID" 2>/dev/null || break
        sleep 0.1
    done
    say "local listener not ready ($d)"
    return 1
}

stop_local_listener() {
    kill -TERM "$LPID" 2>/dev/null || true
    wait "$LPID" 2>/dev/null || true
}

# run_local_bench <dir> <run id> <node name> <args...>: bench on this machine.
run_local_bench() {
    local d=$1 run=$2 name=$3
    shift 3
    mkdir -p "$d"
    local rc=0
    NO_COLOR=1 timeout -s TERM -k 15 300 "$BIN" --json --run-id "$run" --node-name "$name" bench "$@" \
        >"$d/bench.jsonl" 2>"$d/bench.err" </dev/null || rc=$?
    echo "$rc" >"$d/bench.exit"
}

local_meta() { # dir key=value...
    local d=$1
    shift
    printf '%s\n' "$@" "deployed=$(git -C "$repo" rev-parse --short HEAD) (local build)" |
        python3 "$repo/harness/lab/tools/benchsum.py" meta "$d/meta.json"
}

REAL_NODES="real1:easy real2:symmetric real3:udp-blocked"
HETZNER_A='{"role":"listener","node":"hetzner","profile":"public IP, ufw default-deny inbound (random UDP port)","location":"Hetzner Helsinki 65.109.154.126"}'
HETZNER_B="{\"role\":\"listener\",\"node\":\"hetzner\",\"profile\":\"public IP, UDP $OPEN_PORT allowed by ufw\",\"location\":\"Hetzner Helsinki 65.109.154.126\"}"
real_json() { # node profile role
    printf '{"role":"%s","node":"%s","profile":"%s","location":"lab VM via real uplink (emulated NAT + VM masquerade + i3D host masquerade, 213.163.75.157)"}' "$3" "$1" "$2"
}

step_realworld() {
    check_versions
    say "realworld: up campaign-real"
    lab "cd $REMOTE_DIR && ./lab.sh up campaign-real >/dev/null && ./lab.sh status" >"$out/realworld-lab-status.txt"
    start_directory
    relay_metrics "realworld-start"
    local np n p v r id d lbind extra
    # Forward: bench in the lab node -> listener here.
    for np in $REAL_NODES; do
        n=${np%%:*} p=${np#*:}
        scenario "rw-$p-to-ufw" realworld "lab $p (real uplink) -> Hetzner listener on a random UDP port behind ufw" \
            "$(real_json "$n" "$p" bench)" "$HETZNER_A" null ""
        extra=""
        [[ $p == easy ]] && extra="--compare-relay"
        scenario "rw-$p-to-open" realworld "lab $p (real uplink) -> Hetzner listener on UDP $OPEN_PORT (allowed by ufw)" \
            "$(real_json "$n" "$p" bench)" "$HETZNER_B" null "$extra"
    done
    for r in $(seq "$repeat"); do
        for v in ufw open; do
            for np in $REAL_NODES; do
                n=${np%%:*} p=${np#*:}
                id=rw-$p-to-$v d=$out/realworld/$id/run$r
                lbind=()
                [[ $v == open ]] && lbind=(--bind "0.0.0.0:$OPEN_PORT")
                extra=""
                [[ $v == open && $p == easy ]] && extra="--compare-relay"
                say "realworld: round $r $id"
                start_local_listener "$d" "$id-r$r" "hetzner-$v" --relay "$REAL_RELAY" --relay-ca "$REAL_CA" \
                    --directory "$DIRURL" "${lbind[@]}" || continue
                lab "cd $REMOTE_DIR && ./lab.sh bench $n --to $LID --out ~/gpl-runs/$CAMP/realworld/$id/run$r --run-id $id-r$r --directory $DIRURL -- $extra" \
                    >/dev/null 2>>"$out/realworld-progress.txt" || say "realworld: $id run $r: no report"
                stop_local_listener
                tail -n 1 "$out/realworld-progress.txt" | cut -c1-400 >&2
            done
        done
    done
    # Reverse: bench here -> listener in the lab node (easy, symmetric).
    for np in real1:easy real2:symmetric; do
        n=${np%%:*} p=${np#*:}
        scenario "rw-ufw-to-$p" realworld "Hetzner bench (random UDP port behind ufw) -> lab $p listener (real uplink)" \
            "${HETZNER_A/listener/bench}" "$(real_json "$n" "$p" listener)" null ""
        scenario "rw-open-to-$p" realworld "Hetzner bench (UDP $OPEN_PORT, allowed by ufw) -> lab $p listener (real uplink)" \
            "${HETZNER_B/listener/bench}" "$(real_json "$n" "$p" listener)" null ""
    done
    for r in $(seq "$repeat"); do
        for v in ufw open; do
            for np in real1:easy real2:symmetric; do
                n=${np%%:*} p=${np#*:}
                id=rw-$v-to-$p d=$out/realworld/$id/run$r
                lbind=()
                [[ $v == open ]] && lbind=(--bind "0.0.0.0:$OPEN_PORT")
                say "realworld: round $r $id"
                mkdir -p "$d"
                local tag=cl-$id-r$r ready lid
                ready=$(lab "cd $REMOTE_DIR && ./lab.sh listen $n --tag $tag --directory $DIRURL -- --json --run-id $id-r$r --node-name $n") || {
                    say "lab listener not ready"; continue; }
                lid=$(sed -n 's/^{.*"event":"ready".*"node_id":"\(0x[0-9a-f]*\)".*/\1/p' <<<"$ready")
                run_local_bench "$d" "$id-r$r" hetzner --relay "$REAL_RELAY" --relay-ca "$REAL_CA" \
                    --directory "$DIRURL" --to "$lid" "${lbind[@]}"
                lab "cd $REMOTE_DIR && ./lab.sh stop $tag; cat /run/gpl/logs/$tag.log" >"$d/listen.jsonl" 2>/dev/null || true
                lab "cat /run/gpl/logs/$tag.err; rm -f /run/gpl/logs/$tag.*" >"$d/listen.err" 2>/dev/null || true
                local_meta "$d" "bencher=hetzner" "listener=$n" "listener_profile=$p" "run=$id-r$r" "to=$lid" \
                    "bench_args=--relay $REAL_RELAY --directory $DIRURL --to $lid ${lbind[*]}"
                python3 "$repo/harness/lab/tools/benchsum.py" line "$d" | tee -a "$out/realworld-progress.txt" | cut -c1-400 >&2
            done
        done
    done
    relay_metrics "realworld-end"
    fetch_lab realworld
}

# ------------------------------------------------------------------ startup

step_startup() {
    check_versions
    lab "cd $REMOTE_DIR && ./lab.sh status | grep -q real1 || ./lab.sh up campaign-real >/dev/null"
    start_directory
    relay_metrics "startup-start"
    local q r id d
    for q in "$REAL_RELAY|56001" "$REAL_RELAY_NOQ|7842 (default, unanswered)" "$REAL_RELAY_NOQ,quic=off|off"; do
        local url=${q%%|*} label=${q#*|} tag=${q#*|}
        tag=${tag%% *}
        scenario "startup-hetzner-qad-$tag" startup "local check only on Hetzner, relay QAD port $label" \
            "${HETZNER_A/listener/bench}" null null "--relay $url"
        scenario "startup-lab-easy-qad-$tag" startup "local check only in lab easy node (real uplink), relay QAD port $label" \
            "$(real_json real1 easy bench)" null null "--relay $url"
    done
    for r in $(seq "$repeat"); do
        for q in "$REAL_RELAY|56001" "$REAL_RELAY_NOQ|7842" "$REAL_RELAY_NOQ,quic=off|off"; do
            local url=${q%%|*} tag=${q#*|}
            id=startup-hetzner-qad-$tag d=$out/startup/$id/run$r
            say "startup: round $r $id"
            run_local_bench "$d" "$id-r$r" hetzner --relay "$url" --relay-ca "$REAL_CA" --directory "$DIRURL"
            local_meta "$d" "bencher=hetzner" "run=$id-r$r" "bench_args=--relay $url --directory $DIRURL"
            python3 "$repo/harness/lab/tools/benchsum.py" line "$d" | cut -c1-300 >&2
            id=startup-lab-easy-qad-$tag
            say "startup: round $r $id"
            lab "cd $REMOTE_DIR && ./lab.sh bench real1 --out ~/gpl-runs/$CAMP/startup/$id/run$r --run-id $id-r$r --relay '$url' --relay-ca ~/$REMOTE_DIR/certs/real-relay.crt --directory $DIRURL" \
                >/dev/null 2>>"$out/startup-progress.txt" || say "startup: $id run $r: no report"
            tail -n 1 "$out/startup-progress.txt" | cut -c1-300 >&2
        done
    done
    relay_metrics "startup-end"
    fetch_lab startup
}

# ------------------------------------------------------------------ relay

step_relay() {
    local dport
    dport=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
    say "relay: local directory on 127.0.0.1:$dport"
    "$BIN" directory --listen "127.0.0.1:$dport" >"$out/.relay-directory.log" 2>&1 &
    local dpid=$!
    LOCAL_PIDS+=("$dpid")
    sleep 0.5
    local ldir=http://127.0.0.1:$dport/
    local common=(--relay "$REAL_RELAY" --relay-ca "$REAL_CA" --directory "$ldir" --relay-only)
    local hz='{"role":"%s","node":"hetzner-%s","profile":"relay only (--relay-only)","location":"Hetzner Helsinki 65.109.154.126"}'
    scenario relay-single relay "Hetzner -> Hetzner, relay only through p2p-relay1 (one bench at a time)" \
        "$(printf "$hz" bench b)" "$(printf "$hz" listener l)" null "--relay-only (both sides)"
    scenario relay-parallel2 relay "Hetzner -> Hetzner, relay only through p2p-relay1, 2 benches in parallel (separate listeners), no concurrency phase" \
        "$(printf "$hz" bench b)" "$(printf "$hz" listener l)" null "--relay-only (both sides) --skip concurrency; 2 at once"
    scenario relay-parallel4 relay "Hetzner -> Hetzner, relay only through p2p-relay1, 4 benches in parallel (separate listeners), no concurrency phase" \
        "$(printf "$hz" bench b)" "$(printf "$hz" listener l)" null "--relay-only (both sides) --skip concurrency; 4 at once"
    relay_metrics "relay-start"
    local r id d
    for r in $(seq "$repeat"); do
        id=relay-single d=$out/relay/$id/run$r
        say "relay: round $r $id"
        start_local_listener "$d" "$id-r$r" relay-l1 "${common[@]}" || continue
        run_local_bench "$d" "$id-r$r" hetzner "${common[@]}" --to "$LID"
        stop_local_listener
        local_meta "$d" "bencher=hetzner" "listener=hetzner" "run=$id-r$r" "to=$LID" "bench_args=${common[*]} --to $LID"
        python3 "$repo/harness/lab/tools/benchsum.py" line "$d" | cut -c1-400 >&2
    done
    local par
    for par in 2 4; do
        for r in 1 2; do
            id=relay-parallel$par
            say "relay: round $r $id"
            local i lpids=() lids=() bpids=()
            for i in $(seq "$par"); do
                d=$out/relay/$id/run$r-$i
                start_local_listener "$d" "$id-r$r-$i" "relay-l$i" "${common[@]}" || continue
                lpids+=("$LPID") lids+=("$LID")
            done
            for i in $(seq "${#lids[@]}"); do
                d=$out/relay/$id/run$r-$i
                local_meta "$d" "bencher=hetzner" "listener=hetzner" "run=$id-r$r-$i" "to=${lids[$((i - 1))]}" \
                    "bench_args=${common[*]} --skip concurrency --to ${lids[$((i - 1))]}" "parallel=$par"
                run_local_bench "$d" "$id-r$r-$i" "hetzner-$i" "${common[@]}" --skip concurrency --to "${lids[$((i - 1))]}" &
                bpids+=($!)
            done
            wait "${bpids[@]}" || true
            for i in "${lpids[@]}"; do kill -TERM "$i" 2>/dev/null || true; wait "$i" 2>/dev/null || true; done
            for i in $(seq "$par"); do
                python3 "$repo/harness/lab/tools/benchsum.py" line "$out/relay/$id/run$r-$i" | cut -c1-300 >&2
            done
        done
    done
    relay_metrics "relay-end"
    kill -TERM "$dpid" 2>/dev/null || true
}

# ------------------------------------------------------------------ summarize / cleanup

step_summarize() {
    python3 "$here/summarize.py" "$out"
    local base=${GPL_COMPARE_WITH:-$repo/harness/results/2026-11-07-campaign1}
    if [[ -f $base/scenarios.jsonl && $base != "$out" ]]; then
        python3 "$here/compare.py" "$base" "$out"
    fi
}

step_cleanup() {
    cleanup_local
    stop_directory
    lab "cd $REMOTE_DIR && ./lab.sh down" || true
    say "cleanup: lab down, directory on $RELAY_HOST stopped, local processes stopped"
}

for s in "${steps[@]}"; do
    case $s in
        all) for t in deploy lab labextra realworld startup relay summarize cleanup; do "step_$t"; done ;;
        deploy | lab | labextra | realworld | startup | relay | summarize | cleanup) "step_$s" ;;
        *) echo "unknown step $s" >&2; exit 2 ;;
    esac
done
