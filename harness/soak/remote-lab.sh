#!/usr/bin/env bash
# Remote soak across three hosts, with soak.sh's split steps: listener and
# directory on the build machine (public IP, UDP and TCP port 8000 open), soak
# clients and churn workers in NAT lab nodes on p2p-natlab whose traffic goes
# out through the real uplink (topologies/soak-real.topo: easy,
# port-restricted, symmetric, udp-blocked, relay-only, a 20 s NAT timeout),
# and the real relay p2p-relay1. A second iroh-relay on p2p-relay1 with a
# small key_cache_capacity serves only a relay-only churn stream against a
# second, relay-only listener here, to compare the relays' memory per new
# client key. See harness/README.md, "Soak tests".
#
#   harness/soak/remote-lab.sh --out DIR <step>...
#
# Steps:
#   setup     deploy soak.sh/sampler.py to the lab VM, lab.sh up soak-real;
#             on the relay VM: the second relay (systemd-run unit
#             gpl-soak-relay2, ports RELAY2_PORT tcp / +1 udp QAD, metrics on
#             127.0.0.1:9091, the existing certificate copied) and a sampler
#             of both relay processes and their metrics (unit gpl-soak-sampler)
#   services  here: run "main" (directory on TCP 8000, listener on UDP 8000,
#             home relay p2p-relay1) and run "keycache" (relay-only listener on
#             the second relay, same directory), a sampler for each; phase idle
#   load      lab VM: steady clients per node (SOAK_TAG = node), churn workers,
#             a sampler per run; phase load_start
#   stop      phase load_end, stop clients and churn, COOLDOWN seconds, phase
#             end, stop everything (here, lab VM, relay VM samplers)
#   collect   fetch the lab VM's and relay VM's data, merge into the run
#             directories (clients/, churn/, samples.jsonl), summarize.py
#   cleanup   lab.sh down; on the relay VM stop and remove the second relay
#             and the sampler (units and files); stop local processes
#   status    what runs where
#
# Layout of DIR: main/ and keycache/ (soak.sh run directories: listener and
# directory here, clients and churn from the lab VM, samples of all hosts),
# raw/ (the lab VM's and relay VM's samples as recorded, host load).
#
# Environment: GPL_SSH_CONFIG, GPL_HOST (lab VM), GPL_RELAY_HOST,
# SOAK_PUBLIC_IP (this machine, default 65.109.154.126), REAL_RELAY_IP
# (213.163.75.157), RELAY2_PORT (56010), RELAY2_KEY_CACHE (1024), COOLDOWN
# (300), GOLEM_P2P.
set -euo pipefail
export LC_ALL=C

SSH_CONFIG=${GPL_SSH_CONFIG:-$HOME/.ssh/p2p/ssh_config}
LAB_HOST=${GPL_HOST:-p2p-natlab}
RELAY_HOST=${GPL_RELAY_HOST:-p2p-relay1}
LAB_DIR=golem-p2p-lab
PUB=${SOAK_PUBLIC_IP:-65.109.154.126}
RIP=${REAL_RELAY_IP:-213.163.75.157}
RELAY1=https://$RIP:56000,quic=56001
R2P=${RELAY2_PORT:-56010}
RELAY2=https://$RIP:$R2P,quic=$((R2P + 1))
R2CACHE=${RELAY2_KEY_CACHE:-1024}
CA=$HOME/.ssh/p2p/p2p-relay1.crt
COOLDOWN=${COOLDOWN:-300}
RTMP=/tmp/gpl-soak                   # on the relay VM
LSOAK=gpl-soak                       # on the lab VM, under ~
LAB_CA=$LAB_DIR/certs/real-relay.crt # on the lab VM, under ~

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export GOLEM_P2P=${GOLEM_P2P:-$repo/target/release/golem-p2p}

out="" steps=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --out) out=$2; shift 2 ;;
        -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        *) steps+=("$1"); shift ;;
    esac
done
[[ -n $out && ${#steps[@]} -gt 0 ]] || { echo "usage: remote-lab.sh --out DIR <step>... (--help)" >&2; exit 2; }
mkdir -p "$out"
out=$(cd "$out" && pwd)
MAIN=$out/main KC=$out/keycache

lab() { ssh -F "$SSH_CONFIG" -o ServerAliveInterval=15 "$LAB_HOST" "$@"; }
rel() { ssh -F "$SSH_CONFIG" -o ServerAliveInterval=15 "$RELAY_HOST" "$@"; }
say() { echo "remote-lab: $(date -u +%H:%M:%S) $*" >&2; }
soak() { "$here/soak.sh" "$@"; }

# Steady clients: node | clients | soak.sh options. Churn: node | run | workers.
STEADY=(
    "se1|15|--interval 1s --payload-size 256 --bulk-every 20m --bulk-size 1M"
    "spr1|10|--interval 1s --payload-size 256 --bulk-every 20m --bulk-size 1M"
    "ssym1|10|--interval 1s --payload-size 256 --bulk-every 20m --bulk-size 1M"
    "sudp1|10|--interval 1s --payload-size 256 --bulk-every 30m --bulk-size 1M"
    "srly1|10|--interval 1s --payload-size 256 --bulk-every 30m --bulk-size 1M --relay-only"
    "sidle1|5|--interval 30s --payload-size 256 --bulk-every 0"
)
CHURN=(
    "sc1|main|3"
    "sc2|main|3"
    "sc3|kc|5"
)
CHURN_OPTS="--session 3s --session-interval 500ms"

step_setup() {
    say "setup: lab VM"
    lab "mkdir -p $LAB_DIR/soak && rm -rf $LSOAK"
    tar -C "$here" -cf - soak.sh sampler.py | lab "tar -C $LAB_DIR/soak -xf -"
    tar -C "$repo/harness/lab" -cf - topologies/soak-real.topo | lab "tar -C $LAB_DIR -xf -"
    local mine theirs
    mine=$(sha256sum "$GOLEM_P2P" | cut -d' ' -f1)
    theirs=$(lab "sha256sum $LAB_DIR/bin/golem-p2p | cut -d' ' -f1")
    [[ $mine == "$theirs" ]] || { say "the lab's golem-p2p differs from $GOLEM_P2P: deploy first"; exit 1; }
    lab "cd $LAB_DIR && ./lab.sh up soak-real >/dev/null && ./lab.sh status" >"$out/lab-status.txt"
    say "setup: relay VM (second relay on $R2P, key_cache_capacity $R2CACHE; sampler)"
    tar -C "$here" -cf - sampler.py | rel "sudo rm -rf $RTMP && mkdir -p $RTMP && tar -C $RTMP -xf -"
    rel "sudo install -m 644 -o \$(id -un) /etc/iroh-relay/relay.crt $RTMP/relay.crt && \
         sudo install -m 600 -o \$(id -un) /etc/iroh-relay/relay.key $RTMP/relay.key && cat > $RTMP/relay2.toml" <<EOF
# second relay for the remote soak (remote-lab.sh): small key cache
enable_relay = true
enable_quic_addr_discovery = true
http_bind_addr = "127.0.0.1:8081"
enable_metrics = true
metrics_bind_addr = "127.0.0.1:9091"
key_cache_capacity = $R2CACHE

[tls]
cert_mode = "Manual"
manual_cert_path = "$RTMP/relay.crt"
manual_key_path = "$RTMP/relay.key"
https_bind_addr = "0.0.0.0:$R2P"
quic_bind_addr = "0.0.0.0:$((R2P + 1))"
EOF
    rel "sudo systemctl stop gpl-soak-relay2 gpl-soak-sampler 2>/dev/null; sudo systemctl reset-failed gpl-soak-relay2 gpl-soak-sampler 2>/dev/null; \
         sudo systemd-run --quiet --unit=gpl-soak-relay2 -p User=\$(id -un) -p RuntimeMaxSec=6h -p LimitNOFILE=65536 \
           -E RUST_LOG=info -E NO_COLOR=1 /usr/local/bin/iroh-relay --config-path $RTMP/relay2.toml"
    local i
    for i in $(seq 50); do
        curl -sk --max-time 2 -o /dev/null "https://$RIP:$R2P/ping" && break
        sleep 0.2
    done
    curl -sk --max-time 3 -o /dev/null -w "relay2 /ping: %{http_code}\n" "https://$RIP:$R2P/ping" >&2
    rel "sudo systemd-run --quiet --unit=gpl-soak-sampler -p RuntimeMaxSec=6h \
           python3 $RTMP/sampler.py --out $RTMP/samples-relay.jsonl --interval 10 --prom-every 6 \
           --match relay=/etc/iroh-relay/config.toml --match relay2=$RTMP/relay2.toml \
           --prom relay=http://127.0.0.1:9090/metrics --prom relay2=http://127.0.0.1:9091/metrics"
    rel "systemctl is-active gpl-soak-relay2 gpl-soak-sampler; pgrep -a iroh-relay" >&2
}

step_services() {
    say "services: main (directory TCP 8000, listener UDP 8000, $RELAY1)"
    SOAK_RUN=soakr-main SOAK_HOST_IP=0.0.0.0 SOAK_PUBLIC_IP=$PUB SOAK_DIRECTORY_PORT=8000 SOAK_BIND=0.0.0.0:8000 \
        SOAK_RELAY=$RELAY1 SOAK_RELAY_CA=$CA soak services --out "$MAIN"
    SOAK_RUN=soakr-main soak sample --out "$MAIN"
    local dir
    dir=$(sed -n 's/^DIRECTORY_URL=//p' "$MAIN/env")
    say "services: keycache (relay-only listener on $RELAY2, directory $dir)"
    SOAK_RUN=soakr-kc SOAK_DIRECTORY=$dir SOAK_BIND= SOAK_RELAY=$RELAY2 SOAK_RELAY_CA=$CA \
        soak services --out "$KC" --relay-only
    SOAK_RUN=soakr-kc soak sample --out "$KC"
    write_meta
    soak phase idle --out "$MAIN"
    soak phase idle --out "$KC"
}

write_meta() {
    local d
    for d in "$MAIN" "$KC"; do
        printf '%s\n' "${STEADY[@]}" >"$d/.steady"
        printf '%s\n' "${CHURN[@]}" >"$d/.churn"
        python3 - "$d" "$PUB" "$LAB_HOST" "$RELAY_HOST" "$RELAY1" "$RELAY2" "$R2CACHE" "$CHURN_OPTS" "$GOLEM_P2P" <<'EOF'
import json, os, sys, platform
d, pub, lab, relay_host, relay1, relay2, cache, copts, binary = sys.argv[1:10]
name = os.path.basename(d)
steady = [l.split("|") for l in open(os.path.join(d, ".steady")).read().split("\n") if l]
churn = [l.split("|") for l in open(os.path.join(d, ".churn")).read().split("\n") if l]
os.remove(os.path.join(d, ".steady"))
os.remove(os.path.join(d, ".churn"))
kc = name != "main"
json.dump({
    "scenario": "remote-" + name, "run": "soakr-kc" if kc else "soakr-main",
    "hosts": {"services": f"{platform.node()} ({pub})", "clients": f"{lab} (lab nodes, uplink=real)",
              "relay": relay_host},
    "relay": f"{relay2} (second iroh-relay, key_cache_capacity {cache})" if kc else relay1,
    "steady_clients": [] if kc else [{"node": n, "clients": int(c), "options": o} for n, c, o in steady],
    "clients": 0 if kc else sum(int(c) for _, c, _ in steady),
    "churn_workers": [{"node": n, "workers": int(w)} for n, r, w in churn if (r == "kc") == kc],
    "workers": sum(int(w) for n, r, w in churn if (r == "kc") == kc),
    "churn_options": copts, "relay_only": kc, "binary": binary,
}, open(os.path.join(d, "meta.json"), "w"), indent=1)
EOF
    done
    "$GOLEM_P2P" --version >"$MAIN/version.txt"
    cp "$MAIN/version.txt" "$KC/version.txt"
}

# lab_soak <node> <run dir name> <soak.sh args...>: soak.sh in a lab node.
lab_soak() {
    local node=$1 run=$2
    shift 2
    local rid=soakr-$run
    [[ $run == kc ]] || rid=soakr-main
    # setsid: the clients and churn workers outlive soak.sh, sudo and ssh.
    lab "cd $LAB_DIR && ./lab.sh exec $node setsid --wait env SOAK_TAG=$node SOAK_RUN=$rid SOAK_SLICE= SOAK_BIND= \
        SOAK_RELAY_CA=\$HOME/$LAB_CA bash \$HOME/$LAB_DIR/soak/soak.sh $* --out \$HOME/$LSOAK/$run \
        >>\$HOME/$LSOAK/$run/soak-$node.log 2>&1 </dev/null"
}

step_load() {
    say "load: env files to the lab VM"
    lab "mkdir -p $LSOAK/main $LSOAK/kc"
    lab "cat > $LSOAK/main/env" <"$MAIN/env"
    lab "cat > $LSOAK/kc/env" <"$KC/env"
    local s n c opts
    soak phase load_start --out "$MAIN"
    soak phase load_start --out "$KC"
    for s in "${STEADY[@]}"; do
        IFS='|' read -r n c opts <<<"$s"
        say "load: $c clients in $n ($opts)"
        lab_soak "$n" main clients --clients "$c" $opts --stats-interval 60s
    done
    for s in "${CHURN[@]}"; do
        IFS='|' read -r n r c <<<"$s"
        say "load: $c churn workers in $n (run $r)"
        lab_soak "$n" "$r" churn --workers "$c" $CHURN_OPTS
    done
    say "load: samplers on the lab VM"
    lab "cd $LSOAK && for r in main kc; do rid=soakr-\$r; [ \$r = main ] || rid=soakr-kc; \
        SOAK_RUN=\$rid SOAK_SLICE= setsid bash \$HOME/$LAB_DIR/soak/soak.sh sample --out \$HOME/$LSOAK/\$r \
        >>\$HOME/$LSOAK/\$r/soak-sample.log 2>&1 </dev/null; done"
}

step_stop() {
    soak phase load_end --out "$MAIN"
    soak phase load_end --out "$KC"
    say "stop: clients and churn on the lab VM"
    lab "for r in main kc; do bash \$HOME/$LAB_DIR/soak/soak.sh stop --out \$HOME/$LSOAK/\$r load >>\$HOME/$LSOAK/\$r/soak-stop.log 2>&1; done"
    soak phase cooldown --out "$MAIN"
    soak phase cooldown --out "$KC"
    say "stop: cool-down ${COOLDOWN}s"
    sleep "$COOLDOWN"
    soak phase end --out "$MAIN"
    soak phase end --out "$KC"
    say "stop: everything"
    lab "for r in main kc; do bash \$HOME/$LAB_DIR/soak/soak.sh stop --out \$HOME/$LSOAK/\$r all >>\$HOME/$LSOAK/\$r/soak-stop.log 2>&1; done"
    soak stop --out "$KC"
    soak stop --out "$MAIN"
    rel "sudo systemctl stop gpl-soak-sampler" || true
}

step_collect() {
    mkdir -p "$out/raw"
    say "collect: lab VM"
    lab "tar -C $LSOAK --exclude=keys --exclude=pids -cf - main kc" | tar -C "$out/raw" --transform 's,^,natlab-,' -xf -
    say "collect: relay VM"
    rel "cat $RTMP/samples-relay.jsonl" >"$out/raw/samples-relay1.jsonl"
    rel "journalctl -u gpl-soak-relay2 --no-pager -o short-iso" >"$out/raw/relay2.log" 2>/dev/null || true
    local r src
    for r in main keycache; do
        src=$out/raw/natlab-main
        [[ $r == keycache ]] && src=$out/raw/natlab-kc
        rm -rf "${out:?}/$r/clients" "${out:?}/$r/churn"
        [[ -d $src/clients ]] && cp -r "$src/clients" "$out/$r/"
        [[ -d $src/churn ]] && cp -r "$src/churn" "$out/$r/"
    done
    python3 - "$out" <<'EOF'
import json, os, sys
out = sys.argv[1]
def lines(path):
    with open(path) as f:
        for l in f:
            if l.startswith("{"):
                yield json.loads(l)
# The lab VM's processes (clients, churn) and the relays' (relay1 into main,
# relay2 into keycache as "relay"), appended to each run's samples.jsonl;
# their host/cgroup lines stay in raw/.
for run, natlab in (("main", "natlab-main"), ("keycache", "natlab-kc")):
    dst = os.path.join(out, run, "samples.jsonl")
    keep = [json.loads(l) for l in open(dst) if l.startswith("{") and '"origin"' not in l]
    add = []
    p = os.path.join(out, "raw", natlab, "samples.jsonl")
    if os.path.exists(p):
        add += [dict(e, origin="natlab") for e in lines(p) if e.get("kind") in ("proc", "group")]
    want = "relay" if run == "main" else "relay2"
    for e in lines(os.path.join(out, "raw", "samples-relay1.jsonl")):
        if e.get("kind") == "proc" and e.get("role") == want:
            add.append(dict(e, role="relay", origin="relay1"))
        elif e.get("kind") == "prom" and e.get("name") == want:
            add.append(dict(e, name="relay", origin="relay1"))
    rows = keep + add
    rows.sort(key=lambda e: e["ts"])
    with open(dst, "w") as f:
        for e in rows:
            f.write(json.dumps(e, separators=(",", ":")) + "\n")
    print(f"{run}: {len(keep)} local + {len(add)} remote samples")
EOF
    python3 "$here/summarize.py" "$MAIN" "$KC"
}

step_cleanup() {
    say "cleanup: lab down"
    lab "cd $LAB_DIR && for r in main kc; do [ -d \$HOME/$LSOAK/\$r ] && bash soak/soak.sh stop --out \$HOME/$LSOAK/\$r all >/dev/null 2>&1; done; ./lab.sh down" || true
    lab "rm -rf $LSOAK $LAB_DIR/soak $LAB_DIR/topologies/soak-real.topo" || true
    say "cleanup: relay VM"
    rel "sudo systemctl stop gpl-soak-relay2 gpl-soak-sampler 2>/dev/null; sudo systemctl reset-failed gpl-soak-relay2 gpl-soak-sampler 2>/dev/null; sudo rm -rf $RTMP" || true
    say "cleanup: local"
    [[ -d $MAIN ]] && soak stop --out "$MAIN" || true
    [[ -d $KC ]] && soak stop --out "$KC" || true
}

step_status() {
    soak status --out "$MAIN" 2>/dev/null | grep -c alive | sed 's/^/main alive here: /' || true
    soak status --out "$KC" 2>/dev/null | grep -c alive | sed 's/^/kc alive here: /' || true
    lab "pgrep -c -f 'golem-p2p.*--run-id soakr' | sed 's/^/lab golem-p2p processes: /'; uptime" || true
    rel "systemctl is-active gpl-soak-relay2 gpl-soak-sampler; pgrep -a iroh-relay; uptime" || true
}

for s in "${steps[@]}"; do
    case $s in
        setup | services | load | stop | collect | cleanup | status) "step_$s" ;;
        *) echo "unknown step $s" >&2; exit 2 ;;
    esac
done
