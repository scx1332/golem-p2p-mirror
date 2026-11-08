#!/usr/bin/env bash
# Soak tests of golem-p2p: many clients against one listener for hours, with
# the resources of every process sampled over time. See harness/README.md,
# section "Soak tests".
#
#   soak.sh run <steady|relay|churn> --out DIR [options]   all-in-one, locally
#   soak.sh services --out DIR [options]   relay (unless SOAK_RELAY), directory
#                                          (unless SOAK_DIRECTORY), listener
#   soak.sh clients --out DIR [options]    --clients N steady clients
#   soak.sh churn --out DIR [options]      --workers W churn workers
#   soak.sh sample --out DIR               the sampler (sampler.py)
#   soak.sh stop --out DIR [load|all]      stop the clients/churn, or everything
#   soak.sh phase NAME --out DIR           record a phase (idle, load_start,
#                                          load_end, cooldown, end) in
#                                          DIR/phases.jsonl, for split runs
#   soak.sh summarize --out DIR            summarize.py DIR
#   soak.sh status --out DIR               which processes of DIR are alive
#
# Options (defaults in brackets):
#   --out DIR              run directory (required)
#   --duration D           run: how long the load runs [30m]
#   --idle D               run: sample the idle services before the load [60s]
#   --cooldown D           run: keep sampling after the load stopped [120s]
#   --clients N            steady/relay: client processes [60]
#   --interval D           time between echo requests per client [1s]
#   --payload-size B       echo request size [256]
#   --bulk-every D         steady/relay: an upload per client this often [10m]
#   --bulk-size B          size of each upload [1M]
#   --workers W            churn: concurrent churn workers [4]; odd-numbered
#                          workers use a fresh identity per session, even ones
#                          cycle through --reuse-keys keys of their own
#   --reuse-keys K         churn: keys per reusing worker [4]
#   --session D            churn: length of one client session [5s]
#   --session-interval D   churn: echo interval within a session [250ms]
#   --stats-interval D     soak_stats/stats events [60s]
#   --sample-interval S    sampler interval in seconds [10]
#   --relay-only           clients and listener use the relay only
#
# Environment (for remote soaks: services on one host, clients on others):
#   GOLEM_P2P        CLI binary [target/release/golem-p2p of this repo]
#   IROH_RELAY_BIN   iroh-relay 1.4.1 binary [target/soak-tools/bin/iroh-relay,
#                    then iroh-relay on PATH]; only for a local relay
#   SOAK_RELAY       relay as for --relay (https://IP:PORT[,quic=PORT]); unset:
#                    start a local relay on SOAK_HOST_IP
#   SOAK_RELAY_CA    certificate to trust for SOAK_RELAY
#   SOAK_RELAY_METRICS  Prometheus URL of the relay, for the sampler
#   SOAK_RELAY_MATCH    sample a relay process not started here: a substring
#                    of its command line (e.g. iroh-relay)
#   SOAK_DIRECTORY   directory URL; unset: start a local directory
#   SOAK_DIRECTORY_PORT  TCP port of a directory started here [0: any free]
#   SOAK_TO          node ID of a listener elsewhere; unset: `services` starts one
#   SOAK_HOST_IP     address local services bind to [127.0.0.1]; 0.0.0.0 for all
#   SOAK_PUBLIC_IP   address other hosts reach them on: in the relay's
#                    certificate and URL and in the directory URL
#                    [SOAK_HOST_IP, 127.0.0.1 if that is 0.0.0.0]
#   SOAK_BIND        UDP bind address of the nodes [127.0.0.1:0 if SOAK_HOST_IP
#                    is 127.0.0.1, else all interfaces]
#   SOAK_CLIENT_THREADS  tokio worker threads per client process [2]
#   SOAK_SLICE       systemd user slice to run in, with SOAK_CPU_QUOTA [600%]
#                    and SOAK_MEMORY_MAX [6G] for the whole slice
#                    [golem-soak.slice; empty: no slice]
#   SOAK_RUN         run ID copied into every event [basename of DIR]
#   SOAK_TAG         clients and churn workers started by this call are named
#                    client-TAG-<i> and churn-TAG-<w> (files, pids, node names)
#                    instead of client-<i> and churn-<w>, so that several
#                    calls (e.g. one per lab node or NAT type) share one DIR;
#                    the sampler and summarize.py group them by TAG
#
# `services` writes DIR/env with the relay, directory and listener it uses;
# `clients`, `churn` and `sample` read it. Every process gets `--run-id` and a
# `--node-name` (listener, directory, client-<i>, churn-<w>), which is how the
# sampler finds them. Logs and events go to DIR; pids to DIR/pids.

set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)

die() {
    echo "soak.sh: $*" >&2
    exit 1
}
log() { echo "[$(date -u +%H:%M:%S)] $*" >&2; }

# Durations like 30s, 10m, 2h, 250ms, or plain seconds -> seconds (float).
secs() {
    local v=$1
    case $v in
    *ms) awk "BEGIN{print ${v%ms}/1000}" ;;
    *s) echo "${v%s}" ;;
    *m) awk "BEGIN{print ${v%m}*60}" ;;
    *h) awk "BEGIN{print ${v%h}*3600}" ;;
    *) echo "$v" ;;
    esac
}

CMD=${1:-}
[ -n "$CMD" ] || die "usage: soak.sh <run|services|clients|churn|sample|stop|phase|summarize|status> --out DIR [options]"
shift
SCENARIO=
if [ "$CMD" = run ]; then
    SCENARIO=${1:-}
    shift || true
    case $SCENARIO in steady | relay | churn) ;; *) die "run needs a scenario: steady, relay or churn" ;; esac
fi
PHASE=
if [ "$CMD" = phase ]; then
    PHASE=${1:-}
    shift || true
    case $PHASE in idle | load_start | load_end | cooldown | end) ;; *) die "phase needs idle, load_start, load_end, cooldown or end" ;; esac
fi
ORIG_ARGS=("$@")

DIR=
DURATION=30m
IDLE=60s
COOLDOWN=120s
CLIENTS=60
INTERVAL=1s
PAYLOAD=256
BULK_EVERY=10m
BULK_SIZE=1M
WORKERS=4
REUSE_KEYS=4
SESSION=5s
SESSION_INTERVAL=250ms
STATS_INTERVAL=60s
SAMPLE_INTERVAL=10
RELAY_ONLY=0
STOP_WHAT=all
while [ $# -gt 0 ]; do
    case $1 in
    --out) DIR=$2; shift 2 ;;
    --duration) DURATION=$2; shift 2 ;;
    --idle) IDLE=$2; shift 2 ;;
    --cooldown) COOLDOWN=$2; shift 2 ;;
    --clients) CLIENTS=$2; shift 2 ;;
    --interval) INTERVAL=$2; shift 2 ;;
    --payload-size) PAYLOAD=$2; shift 2 ;;
    --bulk-every) BULK_EVERY=$2; shift 2 ;;
    --bulk-size) BULK_SIZE=$2; shift 2 ;;
    --workers) WORKERS=$2; shift 2 ;;
    --reuse-keys) REUSE_KEYS=$2; shift 2 ;;
    --session) SESSION=$2; shift 2 ;;
    --session-interval) SESSION_INTERVAL=$2; shift 2 ;;
    --stats-interval) STATS_INTERVAL=$2; shift 2 ;;
    --sample-interval) SAMPLE_INTERVAL=$2; shift 2 ;;
    --relay-only) RELAY_ONLY=1; shift ;;
    load | all) STOP_WHAT=$1; shift ;;
    *) die "unknown option $1" ;;
    esac
done
[ -n "$DIR" ] || die "--out DIR is required"
[ "$SCENARIO" = relay ] && RELAY_ONLY=1
mkdir -p "$DIR/pids" "$DIR/keys"
DIR=$(cd "$DIR" && pwd)
RUN=${SOAK_RUN:-$(basename "$DIR")}

GOLEM_P2P=${GOLEM_P2P:-$REPO/target/release/golem-p2p}
HOST_IP=${SOAK_HOST_IP:-127.0.0.1}
PUBLIC_IP=${SOAK_PUBLIC_IP:-$HOST_IP}
[ "$PUBLIC_IP" = 0.0.0.0 ] && PUBLIC_IP=127.0.0.1
if [ -z "${SOAK_BIND+x}" ]; then
    if [ "$HOST_IP" = 127.0.0.1 ]; then SOAK_BIND=127.0.0.1:0; else SOAK_BIND=; fi
fi
CLIENT_THREADS=${SOAK_CLIENT_THREADS:-2}
TAG=${SOAK_TAG:-}
[ -z "$TAG" ] || [[ $TAG =~ ^[a-z][a-z0-9]*$ ]] || die "SOAK_TAG must be lowercase letters and digits"
CLIENT=client${TAG:+-$TAG} CHURN=churn${TAG:+-$TAG}
SLICE=${SOAK_SLICE-golem-soak.slice}

# Run in a systemd user scope inside $SLICE, so that the whole soak (all
# runs at once) is capped in CPU and memory, and `systemctl --user stop
# $SLICE` ends everything.
in_scope() {
    case $CMD in run | services | clients | churn | sample) ;; *) return 0 ;; esac
    [ -n "$SLICE" ] && [ -z "${SOAK_IN_SCOPE:-}" ] && command -v systemd-run >/dev/null || return 0
    systemd-run --user --scope --quiet true 2>/dev/null || return 0
    local unit
    unit="golem-soak-$(basename "$DIR" | tr -c 'a-zA-Z0-9_\n' '_')-$CMD-$$"
    echo "$unit.scope" >>"$DIR/scopes"
    local args=("$CMD")
    [ -n "$SCENARIO" ] && args+=("$SCENARIO")
    SOAK_IN_SCOPE=1 exec systemd-run --user --scope --quiet --collect --slice="$SLICE" \
        --unit="$unit" "$0" "${args[@]}" "${ORIG_ARGS[@]}"
}
in_scope
if [ -n "${SOAK_IN_SCOPE:-}" ] && [ -n "$SLICE" ]; then
    systemctl --user set-property --runtime "$SLICE" \
        "CPUQuota=${SOAK_CPU_QUOTA:-600%}" "MemoryMax=${SOAK_MEMORY_MAX:-6G}" 2>/dev/null ||
        log "could not set limits on $SLICE"
fi

free_port() {
    python3 -c 'import socket
while True:
    t = socket.socket(); t.bind(("'"$HOST_IP"'", 0)); p = t.getsockname()[1]
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        u.bind(("'"$HOST_IP"'", p)); break
    except OSError:
        pass
print(p)'
}

# Waits until `cmd...` succeeds, at most $1 seconds.
wait_until() {
    local t=$1
    shift
    local end=$((SECONDS + t))
    until "$@"; do
        [ $SECONDS -lt $end ] || return 1
        sleep 0.2
    done
}

alive() { [ -f "$DIR/pids/$1.pid" ] && kill -0 "$(cat "$DIR/pids/$1.pid")" 2>/dev/null; }

# Saves a variable to DIR/env.
setenv() { echo "$1=$(printf %q "$2")" >>"$DIR/env"; }

load_env() {
    # --relay-only on the command line wins over DIR/env (e.g. relay-only
    # clients against a listener that is not).
    local relay_only=$RELAY_ONLY
    # shellcheck disable=SC1091
    [ -f "$DIR/env" ] && . "$DIR/env"
    [ "$relay_only" = 1 ] && RELAY_ONLY=1
    RELAY_SPEC=${SOAK_RELAY:-${RELAY_SPEC:-}}
    RELAY_CA=${SOAK_RELAY_CA:-${RELAY_CA:-}}
    RELAY_METRICS=${SOAK_RELAY_METRICS:-${RELAY_METRICS:-}}
    DIRECTORY_URL=${SOAK_DIRECTORY:-${DIRECTORY_URL:-}}
    TO=${SOAK_TO:-${TO:-}}
}

node_args() {
    NODE=()
    [ -n "$RELAY_SPEC" ] && NODE+=(--relay "$RELAY_SPEC")
    [ -n "$RELAY_CA" ] && NODE+=(--relay-ca "$RELAY_CA")
    NODE+=(--directory "$DIRECTORY_URL")
    [ -n "$SOAK_BIND" ] && NODE+=(--bind "$SOAK_BIND")
    [ "$RELAY_ONLY" = 1 ] && NODE+=(--relay-only)
    return 0
}

start_relay() {
    local bin=${IROH_RELAY_BIN:-$REPO/target/soak-tools/bin/iroh-relay}
    [ -x "$bin" ] || bin=$(command -v iroh-relay) ||
        die "no iroh-relay: cargo install iroh-relay --version 1.4.1 --features server --root $REPO/target/soak-tools"
    local d=$DIR/relay https quic metrics http
    mkdir -p "$d"
    if [ ! -f "$d/relay.crt" ]; then
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
            -days 30 -subj /CN=golem-soak-relay -addext "subjectAltName=IP:$PUBLIC_IP" \
            -addext basicConstraints=critical,CA:FALSE -addext extendedKeyUsage=serverAuth \
            -keyout "$d/relay.key" -out "$d/relay.crt" 2>/dev/null
    fi
    https=$(free_port) quic=$(free_port) metrics=$(free_port) http=$(free_port)
    cat >"$d/relay.toml" <<EOF
# generated by soak.sh
enable_relay = true
enable_quic_addr_discovery = true
http_bind_addr = "$HOST_IP:$http"
enable_metrics = true
metrics_bind_addr = "$HOST_IP:$metrics"

[tls]
cert_mode = "Manual"
manual_cert_path = "$d/relay.crt"
manual_key_path = "$d/relay.key"
https_bind_addr = "$HOST_IP:$https"
quic_bind_addr = "$HOST_IP:$quic"
EOF
    "$bin" --config-path "$d/relay.toml" </dev/null >"$d/relay.log" 2>&1 &
    echo $! >"$DIR/pids/relay.pid"
    wait_until 20 curl -skf -o /dev/null "https://$PUBLIC_IP:$https/ping" || die "relay did not start, see $d/relay.log"
    RELAY_SPEC="https://$PUBLIC_IP:$https,quic=$quic"
    RELAY_CA=$d/relay.crt
    RELAY_METRICS="http://$PUBLIC_IP:$metrics/metrics"
    setenv RELAY_SPEC "$RELAY_SPEC"
    setenv RELAY_CA "$RELAY_CA"
    setenv RELAY_METRICS "$RELAY_METRICS"
    log "relay $RELAY_SPEC (metrics $RELAY_METRICS)"
}

start_directory() {
    "$GOLEM_P2P" --json --run-id "$RUN" --node-name directory directory \
        --listen "$HOST_IP:${SOAK_DIRECTORY_PORT:-0}" --stats-interval 30s </dev/null >"$DIR/directory.jsonl" 2>"$DIR/directory.err" &
    echo $! >"$DIR/pids/directory.pid"
    wait_until 10 grep -q '"event":"directory"' "$DIR/directory.jsonl" || die "directory did not start"
    # The URL with the address other hosts reach the directory on.
    DIRECTORY_URL=$(python3 -c 'import json, sys, urllib.parse as u
url = u.urlsplit(json.loads(open(sys.argv[1]).readline())["url"])
print(url._replace(netloc=f"{sys.argv[2]}:{url.port}").geturl())' "$DIR/directory.jsonl" "$PUBLIC_IP")
    setenv DIRECTORY_URL "$DIRECTORY_URL"
    log "directory $DIRECTORY_URL"
}

start_listener() {
    local key=$DIR/keys/listener.key
    [ -f "$key" ] || "$GOLEM_P2P" keygen --out "$key" >/dev/null
    node_args
    # Request events (one per request) are dropped; the listener's `stats`
    # events count the requests.
    "$GOLEM_P2P" --json --run-id "$RUN" --node-name listener listen --key "$key" "${NODE[@]}" \
        --stats-interval 30s --max-transfers 32 </dev/null \
        > >(grep --line-buffered -v '"event":"request"' >"$DIR/listener.jsonl") 2>"$DIR/listener.err" &
    echo $! >"$DIR/pids/listener.pid"
    wait_until 30 grep -q '"event":"ready"' "$DIR/listener.jsonl" || die "listener did not start, see $DIR/listener.err"
    TO=$(python3 -c 'import json,sys
for l in open(sys.argv[1]):
    e = json.loads(l)
    if e["event"] == "ready": print(e["node_id"]); break' "$DIR/listener.jsonl")
    setenv TO "$TO"
    log "listener $TO"
}

cmd_services() {
    : >"$DIR/env"
    load_env
    if [ -z "$RELAY_SPEC" ]; then start_relay; else setenv RELAY_SPEC "$RELAY_SPEC"; setenv RELAY_CA "$RELAY_CA"; setenv RELAY_METRICS "$RELAY_METRICS"; fi
    if [ -z "$DIRECTORY_URL" ]; then start_directory; else setenv DIRECTORY_URL "$DIRECTORY_URL"; fi
    if [ -z "$TO" ]; then start_listener; else setenv TO "$TO"; fi
    setenv RELAY_ONLY "$RELAY_ONLY"
}

cmd_clients() {
    load_env
    [ -n "$TO" ] || die "no listener: run 'services' first or set SOAK_TO"
    node_args
    mkdir -p "$DIR/clients"
    local i bulk=()
    [ -n "$BULK_EVERY" ] && [ "$BULK_EVERY" != 0 ] && bulk=(--bulk-every "$BULK_EVERY" --bulk-size "$BULK_SIZE")
    for i in $(seq 1 "$CLIENTS"); do
        TOKIO_WORKER_THREADS=$CLIENT_THREADS "$GOLEM_P2P" --json --run-id "$RUN" --node-name "$CLIENT-$i" \
            soak --to "$TO" "${NODE[@]}" --interval "$INTERVAL" --payload-size "$PAYLOAD" \
            "${bulk[@]}" --stats-interval "$STATS_INTERVAL" </dev/null \
            >"$DIR/clients/$CLIENT-$i.jsonl" 2>"$DIR/clients/$CLIENT-$i.err" &
        echo $! >"$DIR/pids/$CLIENT-$i.pid"
        sleep 0.1
    done
    log "started $CLIENTS clients ($CLIENT-*)"
}

# One churn worker: sessions of --session each, back to back, until
# DIR/stop-churn exists. Odd workers use a fresh identity per session, even
# workers cycle through keys of their own (a new iroh endpoint key per
# session either way, as on every process start).
churn_worker() {
    local w=$1 n=0 key code
    local out=$DIR/churn/$CHURN-$w.jsonl
    while [ ! -e "$DIR/stop-churn" ]; do
        n=$((n + 1))
        local keyarg=()
        if [ $((w % 2)) = 0 ]; then
            key=$DIR/keys/$CHURN-$w-$((n % REUSE_KEYS)).key
            [ -f "$key" ] || "$GOLEM_P2P" keygen --out "$key" >/dev/null
            keyarg=(--key "$key")
        fi
        set +e
        TOKIO_WORKER_THREADS=$CLIENT_THREADS "$GOLEM_P2P" --json --run-id "$RUN" --node-name "$CHURN-$w" \
            soak --to "$TO" "${NODE[@]}" "${keyarg[@]}" --duration "$SESSION" \
            --interval "$SESSION_INTERVAL" --payload-size "$PAYLOAD" --stats-interval 1h </dev/null \
            2>>"$DIR/churn/$CHURN-$w.err" | grep --line-buffered -E '"event":"(soak_stats|error)"' >>"$out"
        code=${PIPESTATUS[0]}
        set -e
        echo "{\"event\":\"session_exit\",\"ts\":$(date +%s.%N),\"worker\":$w,\"session\":$n,\"reuse\":$((1 - w % 2)),\"code\":$code}" >>"$out"
    done
}

cmd_churn() {
    load_env
    [ -n "$TO" ] || die "no listener: run 'services' first or set SOAK_TO"
    node_args
    mkdir -p "$DIR/churn"
    rm -f "$DIR/stop-churn"
    local w
    for w in $(seq 1 "$WORKERS"); do
        churn_worker "$w" &
        echo $! >"$DIR/pids/churnworker-${TAG:+$TAG-}$w.pid"
    done
    log "started $WORKERS churn workers ($CHURN-*)"
}

cmd_sample() {
    load_env
    local args=(--out "$DIR/samples.jsonl" --interval "$SAMPLE_INTERVAL" --run "$RUN")
    [ -f "$DIR/relay/relay.toml" ] && args+=(--match "relay=$DIR/relay/relay.toml")
    [ -n "${SOAK_RELAY_MATCH:-}" ] && args+=(--match "relay=$SOAK_RELAY_MATCH")
    [ -n "$RELAY_METRICS" ] && args+=(--prom "relay=$RELAY_METRICS")
    python3 "$HERE/sampler.py" "${args[@]}" </dev/null 2>"$DIR/sampler.err" &
    echo $! >"$DIR/pids/sampler.pid"
    log "sampler every $SAMPLE_INTERVAL s"
}

# Stops the processes whose pid files match the glob, SIGTERM then SIGKILL.
stop_pids() {
    local f pids=()
    for f in "$DIR"/pids/$1.pid; do
        [ -f "$f" ] || continue
        pids+=("$(cat "$f")")
    done
    [ ${#pids[@]} -gt 0 ] || return 0
    kill -TERM "${pids[@]}" 2>/dev/null || true
    local end=$((SECONDS + ${2:-20}))
    while [ $SECONDS -lt $end ]; do
        local any=0 p
        for p in "${pids[@]}"; do kill -0 "$p" 2>/dev/null && any=1; done
        [ $any = 0 ] && break
        sleep 0.5
    done
    kill -KILL "${pids[@]}" 2>/dev/null || true
    for f in "$DIR"/pids/$1.pid; do rm -f "$f"; done
}

cmd_stop_load() {
    touch "$DIR/stop-churn"
    stop_pids 'client-*'
    # Churn workers finish their current session (at most --session plus
    # startup), then exit.
    local end=$((SECONDS + 60)) f
    for f in "$DIR"/pids/churnworker-*.pid; do
        [ -f "$f" ] || continue
        while kill -0 "$(cat "$f")" 2>/dev/null && [ $SECONDS -lt $end ]; do sleep 0.5; done
    done
    stop_pids 'churnworker-*' 5
}

cmd_stop() {
    cmd_stop_load
    [ "$STOP_WHAT" = load ] && return 0
    stop_pids sampler 5
    stop_pids listener
    stop_pids directory
    stop_pids relay
    local s
    if [ -f "$DIR/scopes" ]; then
        while read -r s; do systemctl --user stop "$s" 2>/dev/null || true; done <"$DIR/scopes"
    fi
}

cmd_status() {
    local f
    for f in "$DIR"/pids/*.pid; do
        [ -f "$f" ] || continue
        local n p
        n=$(basename "$f" .pid) p=$(cat "$f")
        if kill -0 "$p" 2>/dev/null; then echo "$n $p alive"; else echo "$n $p gone"; fi
    done
}

phase() {
    echo "{\"phase\":\"$1\",\"ts\":$(date +%s.%N)}" >>"$DIR/phases.jsonl"
    log "phase $1"
}

cmd_run() {
    log "run $SCENARIO in $DIR (run id $RUN)"
    rm -f "$DIR/phases.jsonl" "$DIR/stop-churn"
    python3 - "$DIR/meta.json" <<EOF
import json, sys, platform, os
json.dump({
    "scenario": "$SCENARIO", "run": "$RUN", "duration": "$DURATION", "idle": "$IDLE",
    "cooldown": "$COOLDOWN", "clients": $CLIENTS, "interval": "$INTERVAL",
    "payload_size": "$PAYLOAD", "bulk_every": "$BULK_EVERY", "bulk_size": "$BULK_SIZE",
    "workers": $WORKERS, "reuse_keys": $REUSE_KEYS, "session": "$SESSION",
    "session_interval": "$SESSION_INTERVAL", "stats_interval": "$STATS_INTERVAL",
    "sample_interval": $SAMPLE_INTERVAL, "relay_only": bool($RELAY_ONLY),
    "client_threads": $CLIENT_THREADS, "slice": "$SLICE",
    "cpu_quota": "${SOAK_CPU_QUOTA:-600%}", "memory_max": "${SOAK_MEMORY_MAX:-6G}",
    "host": platform.node(), "nproc": os.cpu_count(),
    "binary": "$GOLEM_P2P",
    "env": {k: v for k, v in os.environ.items()
            if k.startswith(("SOAK_", "MALLOC_", "GLIBC_TUNABLES", "RUST_LOG", "TOKIO_"))},
}, open(sys.argv[1], "w"), indent=1)
EOF
    "$GOLEM_P2P" --version >"$DIR/version.txt" 2>&1 || true
    trap 'log "interrupted, stopping"; cmd_stop; exit 130' INT TERM
    cmd_services
    cmd_sample
    phase idle
    sleep "$(secs "$IDLE")"
    phase load_start
    case $SCENARIO in
    steady | relay) cmd_clients ;;
    churn) cmd_churn ;;
    esac
    local end
    end=$(awk "BEGIN{print int($(date +%s) + $(secs "$DURATION"))}")
    while [ "$(date +%s)" -lt "$end" ]; do
        sleep 30
        local s
        for s in listener directory relay sampler; do
            if [ -f "$DIR/pids/$s.pid" ] && ! alive "$s"; then
                log "$s died"
                echo "{\"event\":\"died\",\"process\":\"$s\",\"ts\":$(date +%s)}" >>"$DIR/phases.jsonl"
                rm -f "$DIR/pids/$s.pid"
            fi
        done
    done
    phase load_end
    cmd_stop_load
    phase cooldown
    sleep "$(secs "$COOLDOWN")"
    phase end
    trap - INT TERM
    STOP_WHAT=all
    stop_pids sampler 5
    stop_pids listener
    stop_pids directory
    stop_pids relay
    python3 "$HERE/summarize.py" "$DIR" || log "summarize failed"
    log "done: $DIR"
}

case $CMD in
run) cmd_run ;;
services) cmd_services ;;
clients) cmd_clients ;;
churn) cmd_churn ;;
sample) cmd_sample ;;
stop) cmd_stop ;;
status) cmd_status ;;
phase) phase "$PHASE" ;;
summarize) python3 "$HERE/summarize.py" "$DIR" ;;
*) die "unknown command $CMD" ;;
esac
