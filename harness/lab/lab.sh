#!/usr/bin/env bash
# golem-p2p emulated-NAT lab ("layer 1"): network namespaces, veth pairs,
# nftables NAT profiles and tc netem on one Linux machine. Run as root
# (re-executes itself with sudo). See harness/README.md.
#
#   lab.sh up <topology>          build the lab (tears down any previous one first)
#   lab.sh down                   remove everything the lab created (also after a crash)
#   lab.sh status                 show services, nodes and root-namespace changes
#   lab.sh nodes                  machine-readable node table (TSV)
#   lab.sh env [<node>]           shell variables for a node (relay, directory, ...)
#   lab.sh exec [--root] <target> <cmd...>
#                                 run a command in a node (as the invoking user)
#                                 or in inet/svc/<node>-r1/<node>-r2 (with --root)
#   lab.sh spawn [--root] <target> <tag> <cmd...>
#                                 same, in the background; log in $STATE/logs/<tag>.log
#   lab.sh stop <tag>             stop a spawned process
#   lab.sh netem <node> [delay=..] [jitter=..] [loss=..] [rate=..] | clear
#                                 change a node's access link parameters at runtime
#   lab.sh selftest [<topology>]  measure the NAT behaviour of every node
#   lab.sh listen <node> [--tag T] [--relay ..] [--relay-ca ..] [--directory ..] [-- args]
#                                 background `golem-p2p listen`; prints its READY line
#   lab.sh send <node> --to <id> [--duration S] [--out DIR] [...] [-- args]
#                                 `golem-p2p send` for S seconds, summarized
#   lab.sh pair <listener> <sender> [--duration S] [--out DIR] [...]
#                                 golem-p2p listen/send between two nodes, summarized
#   lab.sh matrix [--repeat N] [--duration S] [--out DIR] <listener:sender>...
#                                 run pair for every listed pair, N times
#   lab.sh bench <node> [--to ID] [--out DIR] [--run-id R] [...] [-- bench args]
#                                 `golem-p2p --json bench`; prints its bench_report line
#   lab.sh benchpair <listener> <bencher> [--out DIR] [--run-id R] [--listen-arg A]...
#                    [--bench-arg A]... [--timeout S]
#                                 `listen --json` + `bench --json --to`; prints the bench_report
#   lab.sh benchmatrix [--repeat N] [--out DIR] [--bench-arg A]... <listener:bencher>...
#                                 benchpair for every pair, N rounds; reports.jsonl + results.txt
set -euo pipefail
export LC_ALL=C

LAB_DIR=$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)
STATE=${GPL_STATE:-/run/gpl}
BIN_DIR=${GPL_BIN_DIR:-$LAB_DIR/bin}
GOLEM_P2P=${GOLEM_P2P_BIN:-$BIN_DIR/golem-p2p}
IROH_RELAY=${IROH_RELAY_BIN:-$BIN_DIR/iroh-relay}
TOOLS=$LAB_DIR/tools
TOPOLOGIES=$LAB_DIR/topologies

# Real (internet) relay, used by nodes with uplink=real.
# The relay serves QUIC address discovery on UDP 56001 (not iroh's default 7842).
REAL_RELAY=${GPL_REAL_RELAY:-https://213.163.75.157:56000,quic=56001}
REAL_RELAY_CA=${GPL_REAL_RELAY_CA:-$LAB_DIR/certs/real-relay.crt}
REAL_DIRECTORY=${GPL_REAL_DIRECTORY:-}

PFX=gpl                 # namespaces gpl-*, nft tables gpl/gpl_uplink
INET=$PFX-inet          # the simulated internet (a router)
SVC=$PFX-svc            # relay, directory and reflector, attached to inet
UPLINK_TABLE=${PFX}_uplink
ROOT_LINK=gplu          # root-namespace veth names for uplink=real: gplu<idx>

# Address plan (see README): 198.18.0.0/15 (RFC 2544) plays the public internet.
RELAY_IP=198.18.0.1     # relay https :443, QUIC address discovery :7842, directory :8080
LAB_QAD_PORT=7842
REFL_A=198.18.0.2       # reflector, UDP 3478 + 3479, TCP 3478
REFL_B=198.18.0.3       # reflector, UDP 3478
REFL_C=198.18.0.4       # reflector, UDP 4000 (only sends unsolicited packets)
# The lab relay's QAD port is given explicitly (it is iroh's default too), so
# bench knows the port is right and blames a missing QAD answer on UDP.
LAB_RELAY_HTTPS=https://$RELAY_IP
LAB_RELAY=$LAB_RELAY_HTTPS,quic=$LAB_QAD_PORT
LAB_DIRECTORY=http://$RELAY_IP:8080/

PROFILES="public easy restricted port-restricted symmetric udp-blocked double cgnat"

die() { echo "lab: error: $*" >&2; exit 1; }
log() { echo "lab: $*" >&2; }

if [[ $EUID -ne 0 ]]; then
    # sudo resets the environment; keep the lab's own variables.
    keep=()
    for v in $(compgen -e); do
        [[ $v == GPL_* || $v == GOLEM_P2P_BIN || $v == IROH_RELAY_BIN ]] && keep+=("$v=${!v}")
    done
    exec sudo -- env "${keep[@]}" "$0" "$@"
fi

RUN_USER=${SUDO_USER:-root}
RUN_UID=$(id -u "$RUN_USER")
RUN_GID=$(id -g "$RUN_USER")
RUN_HOME=$(getent passwd "$RUN_USER" | cut -d: -f6)

# ---------------------------------------------------------------- helpers

# nsx <ns> <cmd...>: run in a namespace ("" = root namespace).
nsx() {
    local ns=$1
    shift
    if [[ -z $ns ]]; then "$@"; else ip netns exec "$ns" "$@"; fi
}

# user_cmd: prefix that drops root to the invoking user, with the lab environment.
user_cmd() { # <node or ""> -> prints nothing; sets USER_CMD array
    USER_CMD=(setpriv --reuid="$RUN_UID" --regid="$RUN_GID" --init-groups
        env HOME="$RUN_HOME" USER="$RUN_USER" LOGNAME="$RUN_USER" NO_COLOR=1
        PATH="$BIN_DIR:/usr/local/bin:/usr/bin:/bin")
    if [[ -n ${1:-} ]]; then
        local kv
        while IFS= read -r kv; do USER_CMD+=("$kv"); done < <(node_env "$1")
    fi
}

# mkdir -p as the invoking user, so results stay theirs.
mkdir_user() { setpriv --reuid="$RUN_UID" --regid="$RUN_GID" --init-groups mkdir -p "$1"; }

lab_is_up() { [[ -f $STATE/nodes ]]; }
require_up() { lab_is_up || die "the lab is down (lab.sh up <topology>)"; }

# node_line <name>: sets N_NAME N_IDX N_PROFILE N_UPLINK N_DELAY N_JITTER N_LOSS N_RATE N_UDPT
node_line() {
    local l
    l=$(awk -v n="$1" '$1 == n' "$STATE/nodes")
    [[ -n $l ]] || die "no node '$1' in the current topology"
    read -r N_NAME N_IDX N_PROFILE N_UPLINK N_DELAY N_JITTER N_LOSS N_RATE N_UDPT <<<"$l"
}

# Outermost router of a node (the one with the WAN link).
outer_router() { # profile name
    case $1 in
        double | cgnat) echo "$PFX-$2-r2" ;;
        *) echo "$PFX-$2-r1" ;;
    esac
}

node_ip() { # profile idx
    if [[ $1 == public ]]; then echo "198.19.$2.2"; else echo "192.168.$2.2"; fi
}

public_ip() { # profile idx uplink
    if [[ $3 == real ]]; then
        echo "real"
    elif [[ $1 == public ]]; then
        echo "198.19.$2.2"
    else
        echo "198.18.$2.2"
    fi
}

# Environment for commands run in a node.
node_env() {
    node_line "$1"
    echo "GPL_NODE=$N_NAME"
    echo "GPL_STATE=$STATE"
    echo "GOLEM_P2P=$GOLEM_P2P"
    if [[ $N_UPLINK == real ]]; then
        echo "GPL_RELAY=$REAL_RELAY"
        echo "GPL_RELAY_CA=$REAL_RELAY_CA"
        echo "GPL_DIRECTORY=$(cat "$STATE/real_directory" 2>/dev/null || true)"
    else
        echo "GPL_RELAY=$LAB_RELAY"
        echo "GPL_RELAY_CA=$STATE/relay/relay.crt"
        echo "GPL_DIRECTORY=$LAB_DIRECTORY"
        echo "GPL_REFLECTOR=$REFL_A:3478"
    fi
}

resolve_ns() { # target -> namespace name
    local t=$1
    case $t in
        inet) echo "$INET" ;;
        svc) echo "$SVC" ;;
        *) echo "$PFX-$t" ;;
    esac
}

ns_exists() { ip netns list 2>/dev/null | awk '{print $1}' | grep -qxF "$1"; }

mkns() { # name [fwd]
    ip netns add "$1"
    nsx "$1" ip link set lo up
    nsx "$1" sysctl -qw net.ipv6.conf.all.disable_ipv6=1 net.ipv6.conf.default.disable_ipv6=1
    if [[ ${2:-} == fwd ]]; then
        nsx "$1" sysctl -qw net.ipv4.ip_forward=1
    fi
}

# veth <ns1> <if1> <ns2> <if2>: a veth pair, each end created directly in its
# namespace ("" = root), so names never collide in the root namespace.
veth() {
    local a=(name "$2") b=(name "$4")
    [[ -n $1 ]] && a+=(netns "$1")
    [[ -n $3 ]] && b+=(netns "$3")
    ip link add "${a[@]}" type veth peer "${b[@]}"
    nsx "$1" ip link set "$2" up
    nsx "$3" ip link set "$4" up
}

valid_netem() { # key value
    case $1 in
        delay | jitter) [[ $2 =~ ^[0-9]+(\.[0-9]+)?(us|ms|s)$ ]] ;;
        loss) [[ $2 =~ ^[0-9]+(\.[0-9]+)?%$ ]] ;;
        rate) [[ $2 =~ ^[0-9]+(\.[0-9]+)?([kmgKMG])?(bit|bps)$ ]] ;;
        *) return 1 ;;
    esac
}

# apply_netem <ns> <dev> <delay> <jitter> <loss> <rate> ("-" = unset)
apply_netem() {
    local ns=$1 dev=$2 delay=$3 jitter=$4 loss=$5 rate=$6 args=()
    if [[ $delay != - ]]; then
        args+=(delay "$delay")
        [[ $jitter != - ]] && args+=("$jitter")
    elif [[ $jitter != - ]]; then
        die "jitter needs delay"
    fi
    [[ $loss != - ]] && args+=(loss "$loss")
    [[ $rate != - ]] && args+=(rate "$rate")
    if ((${#args[@]})); then
        nsx "$ns" tc qdisc replace dev "$dev" root netem "${args[@]}"
    else
        nsx "$ns" tc qdisc del dev "$dev" root 2>/dev/null || true
    fi
}

# ---------------------------------------------------------------- NAT profiles

# Router kinds, innermost first. Every NAT kind silently drops unsolicited
# inbound traffic (no ICMP or RST), like typical home routers.
profile_kinds() {
    case $1 in
        public) echo route ;;
        easy) echo eif ;;
        restricted) echo adf ;;
        port-restricted) echo apdf ;;
        symmetric) echo edm ;;
        udp-blocked) echo udpblock ;;
        double) echo "eif eif" ;;
        cgnat) echo "eif apdf" ;; # home router (easy) behind a carrier NAT
        *) die "unknown profile '$1' (known: $PROFILES)" ;;
    esac
}

# router_nft <ns> <kind> <lan host> <mapping timeout s>
#   route    no NAT, no filtering
#   eif      EIM (port-preserving masquerade) + endpoint-independent filtering
#   adf      EIM + address-dependent filtering
#   apdf     EIM + address-and-port-dependent filtering (plain Linux masquerade)
#   edm      endpoint-dependent mapping (masquerade fully-random) + APDF
#   udpblock apdf for TCP, all UDP dropped
# Linux conntrack NAT keeps a mapping per 5-tuple, not per internal endpoint.
# EIM comes from masquerade preserving the source port, which always succeeds
# with one host behind the router. EIF/ADF are emulated with a dynamic set of
# mapped ports (or remote address + port) that inbound packets are DNATed by.
router_nft() {
    local ns=$1 kind=$2 host=$3 t=$4
    [[ $kind == route ]] && return 0
    local masq=masquerade sets="" pre="" fwd=""
    case $kind in
        eif)
            sets="set eif { type inet_service; flags timeout; }"
            fwd="iifname \"lan\" oifname \"wan\" meta l4proto udp update @eif { udp sport timeout ${t}s }"
            pre="iifname \"wan\" udp dport @eif dnat to $host"
            ;;
        adf)
            sets="set adf { type ipv4_addr . inet_service; flags timeout; }"
            fwd="iifname \"lan\" oifname \"wan\" meta l4proto udp update @adf { ip daddr . udp sport timeout ${t}s }"
            pre="iifname \"wan\" ip saddr . udp dport @adf dnat to $host"
            ;;
        apdf) ;;
        edm) masq="masquerade fully-random" ;;
        udpblock) fwd="meta l4proto udp drop" ;;
        *) die "unknown router kind $kind" ;;
    esac
    nsx "$ns" nft -f - <<EOF
table ip $PFX {
    $sets
    chain prerouting {
        type nat hook prerouting priority dstnat; policy accept;
        $pre
    }
    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        oifname "wan" $masq
    }
    chain forward {
        type filter hook forward priority filter; policy accept;
        $fwd
        iifname "wan" ct status dnat accept
        iifname "wan" ct state new drop
    }
    chain input {
        type filter hook input priority filter; policy accept;
        iifname "wan" ct state new drop
    }
}
EOF
    if [[ $t != 120 ]]; then
        nsx "$ns" sysctl -qw net.netfilter.nf_conntrack_udp_timeout="$((t < 30 ? t : 30))" \
            net.netfilter.nf_conntrack_udp_timeout_stream="$t"
    fi
}

# ---------------------------------------------------------------- topology

resolve_topology() {
    local t=$1
    if [[ -f $t ]]; then
        readlink -f "$t"
    elif [[ -f $TOPOLOGIES/$t.topo ]]; then
        echo "$TOPOLOGIES/$t.topo"
    else
        die "no topology '$t' (file, or one of: $(cd "$TOPOLOGIES" && ls *.topo | sed 's/\.topo$//' | tr '\n' ' '))"
    fi
}

# Writes $STATE/nodes: name idx profile uplink delay jitter loss rate udp_timeout
# and $STATE/globals.
parse_topology() {
    local file=$1 line idx=0 kv k v
    local relay_delay=-
    : >"$STATE/nodes"
    while IFS= read -r line || [[ -n $line ]]; do
        line=${line%%#*}
        local f=()
        read -ra f <<<"$line"
        ((${#f[@]})) || continue
        if [[ ${f[0]} == set ]]; then
            for kv in "${f[@]:1}"; do
                k=${kv%%=*} v=${kv#*=}
                case $k in
                    relay_delay) valid_netem delay "$v" || die "bad $kv"; relay_delay=$v ;;
                    *) die "unknown global option '$k'" ;;
                esac
            done
            continue
        fi
        local name=${f[0]} profile=${f[1]:-}
        [[ $name =~ ^[a-z][a-z0-9]{0,11}$ ]] || die "bad node name '$name' (a-z0-9, max 12, letter first)"
        [[ $name != inet && $name != svc ]] || die "node name '$name' is reserved"
        [[ " $PROFILES " == *" $profile "* ]] || die "node $name: unknown profile '$profile' (known: $PROFILES)"
        awk -v n="$name" '$1 == n {exit 1}' "$STATE/nodes" || die "duplicate node '$name'"
        idx=$((idx + 1))
        ((idx <= 250)) || die "at most 250 nodes"
        local uplink=lab delay=- jitter=- loss=- rate=- udpt=120
        for kv in "${f[@]:2}"; do
            [[ $kv == *=* ]] || die "node $name: bad option '$kv'"
            k=${kv%%=*} v=${kv#*=}
            case $k in
                uplink) [[ $v == lab || $v == real ]] || die "uplink must be lab or real"; uplink=$v ;;
                delay | jitter | loss | rate) valid_netem "$k" "$v" || die "node $name: bad $kv"; printf -v "$k" %s "$v" ;;
                udp_timeout) [[ $v =~ ^[0-9]+$ ]] && ((v >= 5)) || die "node $name: bad $kv"; udpt=$v ;;
                *) die "node $name: unknown option '$k'" ;;
            esac
        done
        echo "$name $idx $profile $uplink $delay $jitter $loss $rate $udpt" >>"$STATE/nodes"
    done <"$file"
    [[ -s $STATE/nodes ]] || die "topology has no nodes"
    echo "relay_delay=$relay_delay" >"$STATE/globals"
}

create_inet_and_svc() {
    local relay_delay
    relay_delay=$(sed -n 's/^relay_delay=//p' "$STATE/globals")
    mkns "$INET" fwd
    mkns "$SVC"
    veth "$INET" svc "$SVC" eth0
    nsx "$INET" ip addr add 198.18.0.254/24 dev svc
    local a
    for a in "$RELAY_IP" "$REFL_A" "$REFL_B" "$REFL_C"; do
        nsx "$SVC" ip addr add "$a/24" dev eth0
    done
    nsx "$SVC" ip route add default via 198.18.0.254
    # Services run unprivileged; let them bind 80/443 inside this namespace only.
    nsx "$SVC" sysctl -qw net.ipv4.ip_unprivileged_port_start=0
    apply_netem "$INET" svc "$relay_delay" - - -
    apply_netem "$SVC" eth0 "$relay_delay" - - -
}

create_node() {
    local name=$1 i=$2 profile=$3 uplink=$4 delay=$5 jitter=$6 loss=$7 rate=$8 udpt=$9
    local node=$PFX-$name r1=$PFX-$name-r1 r2=$PFX-$name-r2 kinds lan
    read -ra kinds <<<"$(profile_kinds "$profile")"
    if [[ $profile == public ]]; then lan=198.19.$i; else lan=192.168.$i; fi

    mkns "$node"
    mkns "$r1" fwd
    veth "$node" eth0 "$r1" lan
    nsx "$node" ip addr add "$lan.2/24" dev eth0
    nsx "$node" ip route add default via "$lan.1"
    nsx "$r1" ip addr add "$lan.1/24" dev lan
    router_nft "$r1" "${kinds[0]}" "$lan.2" "$udpt"
    local outer=$r1
    if ((${#kinds[@]} > 1)); then
        local mid
        if [[ $profile == cgnat ]]; then mid=100.64.$i; else mid=10.$i.0; fi
        mkns "$r2" fwd
        veth "$r1" wan "$r2" lan
        nsx "$r1" ip addr add "$mid.2/24" dev wan
        nsx "$r1" ip route add default via "$mid.1"
        nsx "$r2" ip addr add "$mid.1/24" dev lan
        router_nft "$r2" "${kinds[1]}" "$mid.2" "$udpt"
        outer=$r2
    fi
    if [[ $uplink == lab ]]; then
        veth "$outer" wan "$INET" "n$i"
        nsx "$outer" ip addr add "198.18.$i.2/24" dev wan
        nsx "$outer" ip route add default via "198.18.$i.1"
        nsx "$INET" ip addr add "198.18.$i.1/24" dev "n$i"
        [[ $profile == public ]] && nsx "$INET" ip route add "$lan.0/24" via "198.18.$i.2"
        apply_netem "$INET" "n$i" "$delay" "$jitter" "$loss" "$rate"
    else
        veth "$outer" wan "" "$ROOT_LINK$i"
        nsx "$outer" ip addr add "172.30.$i.2/24" dev wan
        nsx "$outer" ip route add default via "172.30.$i.1"
        ip addr add "172.30.$i.1/24" dev "$ROOT_LINK$i"
        [[ $profile == public ]] && ip route add "$lan.0/24" via "172.30.$i.2"
        apply_netem "" "$ROOT_LINK$i" "$delay" "$jitter" "$loss" "$rate"
    fi
    apply_netem "$outer" wan "$delay" "$jitter" "$loss" "$rate"
}

# The one deliberate change to the root namespace, only for uplink=real nodes:
# forwarding on, and masquerade from the gplu* links out of the default-route
# interface. Everything else the root namespace forwards is dropped, and lab
# nodes cannot reach the root namespace or private/local destinations.
setup_root_uplink() {
    local upif
    upif=${GPL_UPLINK_IF:-$(ip -4 route show default | awk '{print $5; exit}')}
    [[ -n $upif ]] || die "no default route in the root namespace"
    [[ -f $STATE/ip_forward.orig ]] || sysctl -n net.ipv4.ip_forward >"$STATE/ip_forward.orig"
    echo "$upif" >"$STATE/uplink_if"
    nft -f - <<EOF
table ip $UPLINK_TABLE {
    chain forward {
        type filter hook forward priority filter; policy drop;
        iifname "$ROOT_LINK*" ip daddr { 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/3 } drop
        iifname "$ROOT_LINK*" oifname "$upif" accept
        iifname "$upif" oifname "$ROOT_LINK*" ct state established,related accept
    }
    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        oifname "$upif" ip saddr { 172.30.0.0/16, 198.19.0.0/16 } masquerade
    }
    chain input {
        type filter hook input priority filter; policy accept;
        iifname "$ROOT_LINK*" drop
    }
}
EOF
    sysctl -qw net.ipv4.ip_forward=1
}

# ---------------------------------------------------------------- processes

# spawn [--root] <ns> <tag> <cmd...>: background process, detached from the
# terminal; pid in $STATE/pids/<tag>.pid, output in $STATE/logs/<tag>.log.
spawn_in() {
    local asroot=0
    if [[ $1 == --root ]]; then asroot=1; shift; fi
    local ns=$1 tag=$2 node=${3:-}
    shift 3
    [[ $tag =~ ^[A-Za-z0-9._-]+$ ]] || die "bad tag '$tag'"
    local logf=$STATE/logs/$tag.log pidf=$STATE/pids/$tag.pid
    if [[ -f $pidf ]] && kill -0 "$(cat "$pidf")" 2>/dev/null; then
        die "a process with tag '$tag' is already running"
    fi
    # SPAWN_ERR=<file>: stderr goes there instead of into the log (for
    # commands that print JSON on stdout).
    local errf=${SPAWN_ERR:-}
    local cmd=("$@")
    if ((!asroot)); then
        user_cmd "$node"
        cmd=("${USER_CMD[@]}" "$@")
    fi
    if [[ -n $errf ]]; then
        setsid ip netns exec "$ns" "${cmd[@]}" >"$logf" 2>"$errf" </dev/null &
        echo $! >"$pidf"
        chown "$RUN_UID:$RUN_GID" "$errf"
    else
        setsid ip netns exec "$ns" "${cmd[@]}" >"$logf" 2>&1 </dev/null &
        echo $! >"$pidf"
    fi
    chown "$RUN_UID:$RUN_GID" "$logf"
    cat "$pidf"
}

stop_tag() {
    local pidf=$STATE/pids/$1.pid pid
    [[ -f $pidf ]] || return 0
    pid=$(cat "$pidf")
    if kill -TERM "$pid" 2>/dev/null; then
        local n
        for n in $(seq 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
        kill -KILL "$pid" 2>/dev/null || true
    fi
    rm -f "$pidf"
}

start_services() {
    local d=$STATE/relay
    mkdir -p "$d"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
        -days 30 -subj /CN=gpl-lab-relay -addext "subjectAltName=IP:$RELAY_IP" \
        -addext basicConstraints=critical,CA:FALSE -addext extendedKeyUsage=serverAuth \
        -keyout "$d/relay.key" -out "$d/relay.crt" 2>/dev/null
    cat >"$d/relay.toml" <<EOF
# generated by lab.sh
enable_relay = true
enable_quic_addr_discovery = true
http_bind_addr = "$RELAY_IP:80"
enable_metrics = true
metrics_bind_addr = "$RELAY_IP:9090"

[tls]
cert_mode = "Manual"
manual_cert_path = "$d/relay.crt"
manual_key_path = "$d/relay.key"
https_bind_addr = "$RELAY_IP:443"
quic_bind_addr = "$RELAY_IP:$LAB_QAD_PORT"
EOF
    chown -R "$RUN_UID:$RUN_GID" "$d"
    chmod 600 "$d/relay.key"
    [[ -x $IROH_RELAY ]] || die "iroh-relay not found at $IROH_RELAY (run deploy.sh or set IROH_RELAY_BIN)"
    [[ -x $GOLEM_P2P ]] || die "golem-p2p not found at $GOLEM_P2P (run deploy.sh or set GOLEM_P2P_BIN)"
    spawn_in "$SVC" svc.relay "" "$IROH_RELAY" --config-path "$d/relay.toml" >/dev/null
    spawn_in "$SVC" svc.directory "" "$GOLEM_P2P" directory --listen "$RELAY_IP:8080" >/dev/null
    spawn_in "$SVC" svc.reflector "" python3 -u "$TOOLS/reflector.py" \
        --udp "$REFL_A:3478" --udp "$REFL_A:3479" --udp "$REFL_B:3478" --udp "$REFL_C:4000" \
        --tcp "$REFL_A:3478" >/dev/null
    local n code
    for n in $(seq 100); do
        code=$(nsx "$SVC" curl -s -o /dev/null -w '%{http_code}' --max-time 1 --cacert "$d/relay.crt" "$LAB_RELAY_HTTPS/" || true)
        [[ $code == 200 ]] && break
        sleep 0.2
    done
    [[ $code == 200 ]] || die "relay did not come up (see $STATE/logs/svc.relay.log)"
    for n in $(seq 50); do
        code=$(nsx "$SVC" curl -s -o /dev/null -w '%{http_code}' --max-time 1 "${LAB_DIRECTORY}v1/records/0x0000000000000000000000000000000000000000" || true)
        [[ $code == 404 ]] && break
        sleep 0.2
    done
    [[ $code == 404 ]] || die "directory did not come up (see $STATE/logs/svc.directory.log)"
}

# ---------------------------------------------------------------- commands

cmd_up() {
    [[ $# -eq 1 ]] || die "usage: lab.sh up <topology>"
    local topo
    topo=$(resolve_topology "$1")
    if ns_exists "$INET" || lab_is_up; then
        log "tearing down the previous lab first"
    fi
    cmd_down >/dev/null
    # A failed up must not leave half a lab (or root-namespace changes) behind.
    UP_DONE=0
    trap up_failed EXIT
    mkdir -p "$STATE"/{logs,pids,keys}
    chown "$RUN_UID:$RUN_GID" "$STATE/logs" "$STATE/keys"
    cp "$topo" "$STATE/topology.src"
    echo "$topo" >"$STATE/topology.path"
    parse_topology "$topo"
    [[ -n $REAL_DIRECTORY ]] && echo "$REAL_DIRECTORY" >"$STATE/real_directory"
    create_inet_and_svc
    local name i profile uplink delay jitter loss rate udpt real=0
    while read -r name i profile uplink delay jitter loss rate udpt; do
        create_node "$name" "$i" "$profile" "$uplink" "$delay" "$jitter" "$loss" "$rate" "$udpt"
        [[ $uplink == real ]] && real=1
    done <"$STATE/nodes"
    if ((real)); then
        setup_root_uplink
    fi
    start_services
    UP_DONE=1
    trap - EXIT
    log "lab is up ($(wc -l <"$STATE/nodes") nodes, topology $topo)"
    cmd_status
}

up_failed() {
    [[ ${UP_DONE:-1} == 1 ]] && return
    local keep
    keep=$RUN_HOME/gpl-runs/up-failed-$(date +%Y%m%d-%H%M%S)
    mkdir_user "$keep"
    cp -r "$STATE/logs/." "$keep/" 2>/dev/null || true
    chown -R "$RUN_UID:$RUN_GID" "$keep"
    log "up failed; service logs saved in $keep; tearing down"
    cmd_down
}

cmd_down() {
    local nss ns pids n
    nss=$(ip netns list 2>/dev/null | awk '{print $1}' | grep "^$PFX-" || true)
    for ns in $nss; do
        pids=$(ip netns pids "$ns" 2>/dev/null || true)
        [[ -n $pids ]] && kill -TERM $pids 2>/dev/null || true
    done
    for n in $(seq 30); do
        pids=""
        for ns in $nss; do pids+=$(ip netns pids "$ns" 2>/dev/null || true); done
        [[ -z $pids ]] && break
        sleep 0.1
    done
    for ns in $nss; do
        pids=$(ip netns pids "$ns" 2>/dev/null || true)
        [[ -n $pids ]] && kill -KILL $pids 2>/dev/null || true
    done
    for ns in $nss; do ip netns del "$ns" 2>/dev/null || true; done
    # Root-side veth ends normally vanish with their peer namespace.
    local l
    for l in $(ip -o link show 2>/dev/null | awk -F': ' '{print $2}' | sed 's/@.*//' | grep "^$ROOT_LINK[0-9]" || true); do
        ip link del "$l" 2>/dev/null || true
    done
    local had_uplink=0
    if nft list table ip "$UPLINK_TABLE" >/dev/null 2>&1; then
        nft delete table ip "$UPLINK_TABLE"
        had_uplink=1
    fi
    if [[ -f $STATE/ip_forward.orig ]]; then
        sysctl -qw net.ipv4.ip_forward="$(cat "$STATE/ip_forward.orig")"
    elif ((had_uplink)); then
        log "warning: $STATE/ip_forward.orig is gone; net.ipv4.ip_forward left at $(sysctl -n net.ipv4.ip_forward)"
    fi
    rm -rf "$STATE"
    log "lab is down"
}

cmd_status() {
    if ! lab_is_up; then
        echo "lab is down"
        local stray
        stray=$(ip netns list 2>/dev/null | awk '{print $1}' | grep "^$PFX-" || true)
        [[ -n $stray ]] && echo "stray namespaces (run lab.sh down): $stray"
        return 0
    fi
    echo "topology: $(cat "$STATE/topology.path")"
    echo "services (namespace $SVC):"
    local t pid st
    for t in svc.relay svc.directory svc.reflector; do
        pid=$(cat "$STATE/pids/$t.pid" 2>/dev/null || echo -)
        if [[ $pid != - ]] && kill -0 "$pid" 2>/dev/null; then st=running; else st=DEAD; fi
        printf '  %-14s %-8s pid %s\n' "${t#svc.}" "$st" "$pid"
    done
    echo "  relay $LAB_RELAY (QAD $RELAY_IP:$LAB_QAD_PORT/udp, CA $STATE/relay/relay.crt), directory $LAB_DIRECTORY"
    echo "  reflector udp $REFL_A:3478,$REFL_A:3479,$REFL_B:3478,$REFL_C:4000 tcp $REFL_A:3478"
    echo "nodes:"
    printf '  %-12s %-16s %-6s %-14s %-14s %s\n' NAME PROFILE UPLINK NODE_IP PUBLIC_IP "LINK (procs)"
    local name i profile uplink delay jitter loss rate udpt link procs
    while read -r name i profile uplink delay jitter loss rate udpt; do
        link=""
        [[ $delay != - ]] && link+="delay=$delay "
        [[ $jitter != - ]] && link+="jitter=$jitter "
        [[ $loss != - ]] && link+="loss=$loss "
        [[ $rate != - ]] && link+="rate=$rate "
        [[ $udpt != 120 ]] && link+="udp_timeout=$udpt "
        procs=$(ip netns pids "$PFX-$name" 2>/dev/null | wc -l)
        printf '  %-12s %-16s %-6s %-14s %-14s %s(%s)\n' "$name" "$profile" "$uplink" \
            "$(node_ip "$profile" "$i")" "$(public_ip "$profile" "$i" "$uplink")" "$link" "$procs"
    done <"$STATE/nodes"
    if nft list table ip "$UPLINK_TABLE" >/dev/null 2>&1; then
        echo "root namespace: table ip $UPLINK_TABLE present, ip_forward=$(sysctl -n net.ipv4.ip_forward)" \
            "(was $(cat "$STATE/ip_forward.orig")), masquerade out of $(cat "$STATE/uplink_if")"
    else
        echo "root namespace: untouched (no uplink=real nodes)"
    fi
}

cmd_nodes() {
    require_up
    printf 'name\tprofile\tuplink\tnetns\tnode_ip\tpublic_ip\n'
    local name i profile uplink rest
    while read -r name i profile uplink rest; do
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$profile" "$uplink" "$PFX-$name" \
            "$(node_ip "$profile" "$i")" "$(public_ip "$profile" "$i" "$uplink")"
    done <"$STATE/nodes"
}

cmd_env() {
    require_up
    if [[ $# -ge 1 ]]; then
        node_env "$1"
    else
        echo "GPL_STATE=$STATE"
        echo "GOLEM_P2P=$GOLEM_P2P"
        echo "GPL_RELAY=$LAB_RELAY"
        echo "GPL_RELAY_CA=$STATE/relay/relay.crt"
        echo "GPL_DIRECTORY=$LAB_DIRECTORY"
        echo "GPL_REFLECTOR=$REFL_A:3478"
    fi
}

cmd_exec() {
    require_up
    local asroot=0
    if [[ ${1:-} == --root ]]; then asroot=1; shift; fi
    [[ $# -ge 2 ]] || die "usage: lab.sh exec [--root] <target> <cmd...>"
    local target=$1 ns
    shift
    ns=$(resolve_ns "$target")
    ns_exists "$ns" || die "no namespace $ns"
    if ((asroot)); then
        exec ip netns exec "$ns" "$@"
    fi
    local node=""
    awk -v n="$target" '$1 == n {f=1} END {exit !f}' "$STATE/nodes" && node=$target
    user_cmd "$node"
    exec ip netns exec "$ns" "${USER_CMD[@]}" "$@"
}

cmd_spawn() {
    require_up
    local root=()
    if [[ ${1:-} == --root ]]; then root=(--root); shift; fi
    [[ $# -ge 3 ]] || die "usage: lab.sh spawn [--root] <target> <tag> <cmd...>"
    local target=$1 tag=$2 ns node=""
    shift 2
    ns=$(resolve_ns "$target")
    ns_exists "$ns" || die "no namespace $ns"
    awk -v n="$target" '$1 == n {f=1} END {exit !f}' "$STATE/nodes" && node=$target
    spawn_in "${root[@]}" "$ns" "$tag" "$node" "$@"
}

cmd_stop() {
    require_up
    [[ $# -eq 1 ]] || die "usage: lab.sh stop <tag>"
    stop_tag "$1"
}

cmd_netem() {
    require_up
    [[ $# -ge 1 ]] || die "usage: lab.sh netem <node> [delay=..] [jitter=..] [loss=..] [rate=..] | clear"
    node_line "$1"
    shift
    local delay=$N_DELAY jitter=$N_JITTER loss=$N_LOSS rate=$N_RATE kv k v
    for kv in "$@"; do
        if [[ $kv == clear ]]; then delay=- jitter=- loss=- rate=-; continue; fi
        k=${kv%%=*} v=${kv#*=}
        [[ $k == delay || $k == jitter || $k == loss || $k == rate ]] || die "bad $kv"
        if [[ $v == - || $v == off ]]; then
            v=-
        else
            valid_netem "$k" "$v" || die "bad $kv"
        fi
        printf -v "$k" %s "$v"
    done
    local outer
    outer=$(outer_router "$N_PROFILE" "$N_NAME")
    apply_netem "$outer" wan "$delay" "$jitter" "$loss" "$rate"
    if [[ $N_UPLINK == real ]]; then
        apply_netem "" "$ROOT_LINK$N_IDX" "$delay" "$jitter" "$loss" "$rate"
    else
        apply_netem "$INET" "n$N_IDX" "$delay" "$jitter" "$loss" "$rate"
    fi
    awk -v n="$N_NAME" -v d="$delay" -v j="$jitter" -v l="$loss" -v r="$rate" \
        '$1 == n {$5=d; $6=j; $7=l; $8=r} {print}' "$STATE/nodes" >"$STATE/nodes.new"
    mv "$STATE/nodes.new" "$STATE/nodes"
    nsx "$outer" tc qdisc show dev wan
}

# Expected selftest result per profile: mapping filtering unmapped_inbound tcp
expected_nat() {
    case $1 in
        public) echo "no-nat EIF yes yes" ;;
        easy | double) echo "EIM EIF no yes" ;;
        restricted) echo "EIM ADF no yes" ;;
        port-restricted | cgnat) echo "EIM APDF no yes" ;;
        symmetric) echo "APDM APDF no yes" ;;
        udp-blocked) echo "no-udp n/a no yes" ;;
    esac
}

cmd_selftest() {
    if [[ $# -ge 1 ]]; then cmd_up "$1" >/dev/null; fi
    require_up
    local name i profile uplink rest out fail=0 ok=0
    echo "| node | profile | mapping | filtering | port preserved | unmapped port open | TCP | expected | result |"
    echo "|---|---|---|---|---|---|---|---|---|"
    while read -r name i profile uplink rest; do
        if [[ $uplink == real ]]; then
            echo "| $name | $profile | - | - | - | - | - | - | skipped (uplink=real) |"
            continue
        fi
        user_cmd "$name"
        out=$(ip netns exec "$PFX-$name" "${USER_CMD[@]}" python3 "$TOOLS/natcheck.py" \
            --map "$REFL_A:3478" --map "$REFL_A:3479" --map "$REFL_B:3478" \
            --filt "$REFL_A:3478" --tcp "$REFL_A:3478" </dev/null)
        local -A r=()
        local kv
        for kv in $out; do r[${kv%%=*}]=${kv#*=}; done
        local exp got verdict
        exp=$(expected_nat "$profile")
        got="${r[mapping]} ${r[filtering]} ${r[unmapped_inbound]} ${r[tcp]}"
        if [[ $got == "$exp" ]]; then verdict=PASS; ok=$((ok + 1)); else verdict=FAIL; fail=$((fail + 1)); fi
        echo "| $name | $profile | ${r[mapping]} | ${r[filtering]} | ${r[port_preserved]} | ${r[unmapped_inbound]} | ${r[tcp]} | $(cut -d' ' -f1,2 <<<"$exp") | $verdict |"
        [[ -n ${GPL_VERBOSE:-} ]] && echo "    $out"
        unset r
    done <"$STATE/nodes"
    echo
    echo "selftest: $ok passed, $fail failed"
    ((fail == 0))
}

ensure_key() { # node -> prints node id; key in $STATE/keys/<node>.key
    local k=$STATE/keys/$1.key
    if [[ ! -f $k ]]; then
        setpriv --reuid="$RUN_UID" --regid="$RUN_GID" --init-groups \
            "$GOLEM_P2P" keygen --out "$k" >"$STATE/keys/$1.id"
    fi
    cat "$STATE/keys/$1.id"
}

# conn_args <node> <relay> <ca> <directory>: sets CONN to the relay/directory
# options for a node. Empty values mean the default for the node's uplink:
# the lab relay and directory, or the real relay and GPL_REAL_DIRECTORY.
conn_args() {
    local relay=$2 ca=$3 dir=$4
    node_line "$1"
    if [[ -z $relay ]]; then
        if [[ $N_UPLINK == real ]]; then
            relay=$REAL_RELAY ca=${ca:-$REAL_RELAY_CA}
        else
            relay=$LAB_RELAY ca=${ca:-$STATE/relay/relay.crt}
        fi
    fi
    if [[ -z $dir ]]; then
        if [[ $N_UPLINK == real ]]; then
            dir=$(cat "$STATE/real_directory" 2>/dev/null || true)
        else
            dir=$LAB_DIRECTORY
        fi
    fi
    [[ -n $dir ]] || die "no directory for node $1 (pass --directory, or set GPL_REAL_DIRECTORY at up time)"
    CONN=(--relay "$relay" --directory "$dir")
    [[ -n $ca ]] && CONN+=(--relay-ca "$ca")
    return 0
}

# Parses the options shared by listen/send/pair into O_* variables; the rest
# goes to O_REST.
parse_run_opts() {
    O_DUR=30 O_OUT="" O_RELAY="" O_CA="" O_DIR="" O_TAG="" O_TO="" O_EXTRA=() O_LARGS=() O_SARGS=() O_REST=()
    while [[ $# -gt 0 ]]; do
        case $1 in
            --duration) O_DUR=$2; shift 2 ;;
            --out) O_OUT=$2; shift 2 ;;
            --relay) O_RELAY=$2; shift 2 ;;
            --relay-ca) O_CA=$2; shift 2 ;;
            --directory) O_DIR=$2; shift 2 ;;
            --tag) O_TAG=$2; shift 2 ;;
            --to) O_TO=$2; shift 2 ;;
            --listen-arg) O_LARGS+=("$2"); shift 2 ;;
            --send-arg) O_SARGS+=("$2"); shift 2 ;;
            --) shift; O_EXTRA=("$@"); break ;;
            *) O_REST+=("$1"); shift ;;
        esac
    done
    [[ $O_DUR =~ ^[0-9]+$ ]] || die "bad --duration $O_DUR"
}

# start_listener <node> <tag> <args...>: spawns `golem-p2p listen`, waits for
# its READY line and sets READY to it.
start_listener() {
    local node=$1 tag=$2
    shift 2
    ensure_key "$node" >/dev/null
    spawn_in "$PFX-$node" "$tag" "$node" "$GOLEM_P2P" listen --key "$STATE/keys/$node.key" "$@" >/dev/null
    local n
    READY=""
    for n in $(seq 200); do
        # `READY ...` (human output) or the `ready` event (with --json).
        READY=$(grep -m1 -E '^READY|^\{.*"event":"ready"' "$STATE/logs/$tag.log" 2>/dev/null || true)
        [[ -n $READY ]] && return 0
        kill -0 "$(cat "$STATE/pids/$tag.pid")" 2>/dev/null || break
        sleep 0.1
    done
    tail -5 "$STATE/logs/$tag.log" >&2 || true
    stop_tag "$tag"
    die "listener in $node did not become ready (log: $STATE/logs/$tag.log)"
}

# ready_node_id <READY line or ready event>: the listener's node ID.
ready_node_id() {
    sed -n -e 's/^READY node_id=\([^ ]*\).*/\1/p' -e 's/^{.*"event":"ready".*"node_id":"\(0x[0-9a-f]*\)".*/\1/p' <<<"$1"
}

# run_send <node> <to> <duration> <out dir> <args...>: runs `golem-p2p send
# --count <many>` for <duration> seconds (SIGTERM ends it; SIGINT would be
# ignored when the lab runs in the background). Every output line gets a unix
# timestamp. Sets SEND_START.
run_send() {
    local node=$1 to=$2 dur=$3 out=$4
    shift 4
    ensure_key "$node" >/dev/null
    user_cmd "$node"
    SEND_START=$EPOCHREALTIME
    {
        ip netns exec "$PFX-$node" "${USER_CMD[@]}" timeout -s TERM -k 5 "$dur" \
            "$GOLEM_P2P" send --key "$STATE/keys/$node.key" "$@" \
            --to "$to" --count 100000000 ping 2>"$out/send.err" </dev/null || true
    } | while IFS= read -r l; do printf '%s %s\n' "$EPOCHREALTIME" "$l"; done >"$out/send.out"
}

# listen <node> [--tag T] [--relay URL] [--relay-ca F] [--directory URL] [-- extra listen args]
# Starts a background listener (stop it with `lab.sh stop <tag>`, default
# listen-<node>) and prints its READY line. With `-- --json ...` it prints the
# `ready` event; the events are in $STATE/logs/<tag>.log, stderr in <tag>.err.
cmd_listen() {
    require_up
    [[ $# -ge 1 ]] || die "usage: lab.sh listen <node> [--tag T] [--relay URL] [--relay-ca F] [--directory URL] [-- args]"
    local node=$1
    shift
    parse_run_opts "$@"
    ((${#O_REST[@]} == 0)) || die "unknown listen option ${O_REST[0]}"
    conn_args "$node" "$O_RELAY" "$O_CA" "$O_DIR"
    local tag=${O_TAG:-listen-$node} a
    # With --json, stdout (the log) carries only events; stderr goes to <tag>.err.
    for a in "${O_EXTRA[@]}"; do
        [[ $a == --json ]] && local SPAWN_ERR=$STATE/logs/$tag.err
    done
    start_listener "$node" "$tag" "${CONN[@]}" "${O_EXTRA[@]}"
    echo "$READY"
}

# send <node> --to <node id> [--duration S] [--out DIR] [--relay ..] [--relay-ca ..]
#      [--directory ..] [-- extra send args]
# Sends requests for S seconds and prints a summary line (see tools/summarize.py).
cmd_send() {
    require_up
    [[ $# -ge 1 ]] || die "usage: lab.sh send <node> --to <node id> [options]"
    local node=$1
    shift
    parse_run_opts "$@"
    ((${#O_REST[@]} == 0)) || die "unknown send option ${O_REST[0]}"
    [[ -n $O_TO ]] || die "send needs --to <node id>"
    conn_args "$node" "$O_RELAY" "$O_CA" "$O_DIR"
    local out=${O_OUT:-$RUN_HOME/gpl-runs/$(date +%Y%m%d-%H%M%S)-send-$node}
    mkdir_user "$out"
    run_send "$node" "$O_TO" "$O_DUR" "$out" "${CONN[@]}" "${O_EXTRA[@]}"
    chown -R "$RUN_UID:$RUN_GID" "$out"
    local summary
    summary=$(python3 "$TOOLS/summarize.py" --start "$SEND_START" --out "$out/send.out" --err "$out/send.err")
    echo "sender=$node $summary" | tee "$out/summary.txt"
}

# pair <listener> <sender> [--duration S] [--out DIR] [--relay URL] [--relay-ca F]
#      [--directory URL] [--listen-arg A]... [--send-arg A]...
# Starts `golem-p2p listen` in the listener node, runs `golem-p2p send` in the
# sender node for S seconds, stops the listener and prints one summary line.
# Raw logs go to DIR (default ~/gpl-runs/<time>-<listener>-<sender>).
cmd_pair() {
    require_up
    [[ $# -ge 2 ]] || die "usage: lab.sh pair <listener> <sender> [options]"
    local lnode=$1 snode=$2
    shift 2
    parse_run_opts "$@"
    ((${#O_REST[@]} == 0)) || die "unknown pair option ${O_REST[0]}"
    node_line "$lnode"
    local lup=$N_UPLINK
    node_line "$snode"
    [[ -n $O_RELAY || $lup == "$N_UPLINK" ]] ||
        die "$lnode and $snode use different uplinks; pass --relay/--relay-ca/--directory"
    conn_args "$lnode" "$O_RELAY" "$O_CA" "$O_DIR"
    local out=${O_OUT:-$RUN_HOME/gpl-runs/$(date +%Y%m%d-%H%M%S)-$lnode-$snode}
    mkdir_user "$out"
    local ltag=pair-listen-$lnode-$$ lid
    start_listener "$lnode" "$ltag" "${CONN[@]}" "${O_LARGS[@]}"
    lid=$(ready_node_id "$READY")
    run_send "$snode" "$lid" "$O_DUR" "$out" "${CONN[@]}" "${O_SARGS[@]}"
    stop_tag "$ltag"
    cp "$STATE/logs/$ltag.log" "$out/listen.log"
    rm -f "$STATE/logs/$ltag.log"
    chown -R "$RUN_UID:$RUN_GID" "$out"
    local summary
    summary=$(python3 "$TOOLS/summarize.py" --start "$SEND_START" --out "$out/send.out" \
        --err "$out/send.err" --listen "$out/listen.log")
    echo "listener=$lnode sender=$snode $summary" | tee "$out/summary.txt"
}

cmd_matrix() {
    require_up
    local repeat=3 dur=30 out="" pairs=()
    while [[ $# -gt 0 ]]; do
        case $1 in
            --repeat) repeat=$2; shift 2 ;;
            --duration) dur=$2; shift 2 ;;
            --out) out=$2; shift 2 ;;
            *:*) pairs+=("$1"); shift ;;
            *) die "unknown matrix argument $1 (pairs are listener:sender)" ;;
        esac
    done
    ((${#pairs[@]})) || die "no pairs given"
    out=${out:-$RUN_HOME/gpl-runs/matrix-$(date +%Y%m%d-%H%M%S)}
    mkdir_user "$out"
    cp "$STATE/topology.src" "$out/topology.topo"
    local r p
    for r in $(seq "$repeat"); do
        for p in "${pairs[@]}"; do
            local line
            line=$(cmd_pair "${p%%:*}" "${p#*:}" --duration "$dur" --out "$out/run$r-${p%%:*}-${p#*:}" 2>&1 | tail -1) || true
            echo "run=$r $line" | tee -a "$out/results.txt"
        done
    done
    chown -R "$RUN_UID:$RUN_GID" "$out"
    log "results in $out/results.txt"
}

# ---------------------------------------------------------------- bench --json

# Parses the options of bench/benchpair into B_* variables.
parse_bench_opts() {
    B_OUT="" B_RELAY="" B_CA="" B_DIR="" B_TO="" B_RUN="" B_TIMEOUT=300 B_LARGS=() B_BARGS=()
    while [[ $# -gt 0 ]]; do
        case $1 in
            --out) B_OUT=$2; shift 2 ;;
            --relay) B_RELAY=$2; shift 2 ;;
            --relay-ca) B_CA=$2; shift 2 ;;
            --directory) B_DIR=$2; shift 2 ;;
            --to) B_TO=$2; shift 2 ;;
            --run-id) B_RUN=$2; shift 2 ;;
            --timeout) B_TIMEOUT=$2; shift 2 ;;
            --listen-arg) B_LARGS+=("$2"); shift 2 ;;
            --bench-arg) B_BARGS+=("$2"); shift 2 ;;
            --) shift; B_BARGS+=("$@"); break ;;
            *) die "unknown bench option $1" ;;
        esac
    done
    [[ $B_TIMEOUT =~ ^[0-9]+$ ]] || die "bad --timeout $B_TIMEOUT"
}

# node_meta <role> <node>: key=value lines describing a lab node, for meta.json.
node_meta() {
    node_line "$2"
    echo "$1=$N_NAME"
    echo "$1_profile=$N_PROFILE"
    echo "$1_uplink=$N_UPLINK"
    echo "$1_delay=$N_DELAY"
    echo "$1_jitter=$N_JITTER"
    echo "$1_loss=$N_LOSS"
    echo "$1_rate=$N_RATE"
    echo "$1_udp_timeout=$N_UDPT"
}

# run_bench <node> <out dir> <run id> <timeout s> <args...>: `golem-p2p --json
# bench` in a node; stdout to bench.jsonl, stderr to bench.err, exit code to
# bench.exit. SIGTERM after <timeout> s (bench then still prints its report).
run_bench() {
    local node=$1 out=$2 run=$3 tmo=$4
    shift 4
    user_cmd "$node"
    local rc=0
    ip netns exec "$PFX-$node" "${USER_CMD[@]}" timeout -s TERM -k 15 "$tmo" \
        "$GOLEM_P2P" --json --run-id "$run" --node-name "$node" bench "$@" \
        >"$out/bench.jsonl" 2>"$out/bench.err" </dev/null || rc=$?
    echo "$rc" >"$out/bench.exit"
    chown "$RUN_UID:$RUN_GID" "$out/bench.jsonl" "$out/bench.err" "$out/bench.exit"
}

# Prints the bench_report line of a run directory; fails if there is none.
bench_report_line() {
    local l
    l=$(tail -n 1 "$1/bench.jsonl" 2>/dev/null || true)
    [[ $l == *'"event":"bench_report"'* ]] || { echo "lab: no bench_report in $1/bench.jsonl (exit $(cat "$1/bench.exit" 2>/dev/null))" >&2; return 1; }
    echo "$l"
}

# bench <node> [--to ID] [--out DIR] [--run-id R] [--relay ..] [--relay-ca ..]
#       [--directory ..] [--timeout S] [-- bench args]
# Runs `golem-p2p --json bench` in a node and prints its bench_report line.
# Raw output in DIR (default ~/gpl-runs/<time>-bench-<node>): bench.jsonl,
# bench.err, bench.exit, meta.json.
cmd_bench() {
    require_up
    [[ $# -ge 1 ]] || die "usage: lab.sh bench <node> [--to ID] [options] [-- bench args]"
    local node=$1
    shift
    parse_bench_opts "$@"
    conn_args "$node" "$B_RELAY" "$B_CA" "$B_DIR"
    local out=${B_OUT:-$RUN_HOME/gpl-runs/$(date +%Y%m%d-%H%M%S)-bench-$node}
    local run=${B_RUN:-$(basename "$out")}
    mkdir_user "$out"
    local to=()
    [[ -n $B_TO ]] && to=(--to "$B_TO")
    { node_meta bencher "$node"; echo "run=$run"; echo "to=$B_TO";
      echo "bench_args=${CONN[*]} ${to[*]} ${B_BARGS[*]}"; echo "deployed=$(cat "$LAB_DIR/DEPLOYED" 2>/dev/null)"; } |
        python3 "$TOOLS/benchsum.py" meta "$out/meta.json"
    run_bench "$node" "$out" "$run" "$B_TIMEOUT" "${CONN[@]}" "${to[@]}" "${B_BARGS[@]}"
    chown -R "$RUN_UID:$RUN_GID" "$out"
    python3 "$TOOLS/benchsum.py" line "$out" >&2 || true
    bench_report_line "$out"
}

# benchpair <listener> <bencher> [--out DIR] [--run-id R] [--relay ..] [--relay-ca ..]
#           [--directory ..] [--listen-arg A]... [--bench-arg A]... [--timeout S]
# Starts `golem-p2p --json listen` in the listener node, runs `golem-p2p --json
# bench --to <listener>` in the bencher node, stops the listener and prints the
# bench_report line (a key=value summary goes to stderr). Raw output in DIR
# (default ~/gpl-runs/<time>-bench-<listener>-<bencher>): bench.jsonl,
# bench.err, bench.exit, listen.jsonl, listen.err, meta.json, summary.txt.
cmd_benchpair() {
    require_up
    [[ $# -ge 2 ]] || die "usage: lab.sh benchpair <listener> <bencher> [options]"
    local lnode=$1 bnode=$2
    shift 2
    parse_bench_opts "$@"
    node_line "$lnode"
    local lup=$N_UPLINK
    node_line "$bnode"
    [[ -n $B_RELAY || $lup == "$N_UPLINK" ]] ||
        die "$lnode and $bnode use different uplinks; pass --relay/--relay-ca/--directory"
    conn_args "$lnode" "$B_RELAY" "$B_CA" "$B_DIR"
    local out=${B_OUT:-$RUN_HOME/gpl-runs/$(date +%Y%m%d-%H%M%S)-bench-$lnode-$bnode}
    local run=${B_RUN:-$(basename "$out")}
    mkdir_user "$out"
    local ltag=bench-listen-$lnode-$$ lid
    SPAWN_ERR=$STATE/logs/$ltag.err start_listener "$lnode" "$ltag" \
        --json --run-id "$run" --node-name "$lnode" "${CONN[@]}" "${B_LARGS[@]}"
    lid=$(ready_node_id "$READY")
    [[ -n $lid ]] || { stop_tag "$ltag"; die "no node_id in the listener's ready event"; }
    { node_meta listener "$lnode"; node_meta bencher "$bnode"; echo "run=$run"; echo "to=$lid";
      echo "listen_args=${CONN[*]} ${B_LARGS[*]}"; echo "bench_args=${CONN[*]} --to $lid ${B_BARGS[*]}";
      echo "deployed=$(cat "$LAB_DIR/DEPLOYED" 2>/dev/null)"; } |
        python3 "$TOOLS/benchsum.py" meta "$out/meta.json"
    run_bench "$bnode" "$out" "$run" "$B_TIMEOUT" "${CONN[@]}" --to "$lid" "${B_BARGS[@]}"
    stop_tag "$ltag"
    cp "$STATE/logs/$ltag.log" "$out/listen.jsonl"
    cp "$STATE/logs/$ltag.err" "$out/listen.err"
    rm -f "$STATE/logs/$ltag.log" "$STATE/logs/$ltag.err"
    python3 "$TOOLS/benchsum.py" line "$out" | tee "$out/summary.txt" >&2 || true
    chown -R "$RUN_UID:$RUN_GID" "$out"
    bench_report_line "$out"
}

# benchmatrix [--repeat N] [--out DIR] [--listen-arg A]... [--bench-arg A]...
#             [--timeout S] <listener:bencher>...
# benchpair for every pair, N rounds (all pairs, then all pairs again). Every
# run's bench_report line is appended to DIR/reports.jsonl, its key=value
# summary to DIR/results.txt; raw output in DIR/<listener>-<bencher>/run<r>/.
cmd_benchmatrix() {
    require_up
    local repeat=3 out="" pairs=() pass=()
    while [[ $# -gt 0 ]]; do
        case $1 in
            --repeat) repeat=$2; shift 2 ;;
            --out) out=$2; shift 2 ;;
            --listen-arg | --bench-arg | --timeout) pass+=("$1" "$2"); shift 2 ;;
            *:*) pairs+=("$1"); shift ;;
            *) die "unknown benchmatrix argument $1 (pairs are listener:bencher)" ;;
        esac
    done
    ((${#pairs[@]})) || die "no pairs given"
    out=${out:-$RUN_HOME/gpl-runs/benchmatrix-$(date +%Y%m%d-%H%M%S)}
    mkdir_user "$out"
    cp "$STATE/topology.src" "$out/topology.topo"
    local r p l b d line
    for r in $(seq "$repeat"); do
        for p in "${pairs[@]}"; do
            l=${p%%:*} b=${p#*:} d=$out/$l-$b/run$r
            mkdir_user "$out/$l-$b"
            if line=$(cmd_benchpair "$l" "$b" --out "$d" --run-id "$l-$b-r$r" "${pass[@]}"); then
                echo "$line" >>"$out/reports.jsonl"
            fi
            echo "round=$r $(cat "$d/summary.txt" 2>/dev/null || echo "listener=$l bencher=$b error=no-report")" |
                tee -a "$out/results.txt"
        done
    done
    chown -R "$RUN_UID:$RUN_GID" "$out"
    log "results in $out/reports.jsonl and $out/results.txt"
}

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'; }

main() {
    local cmd=${1:-help}
    shift || true
    case $cmd in
        up) cmd_up "$@" ;;
        down) cmd_down "$@" ;;
        status) cmd_status "$@" ;;
        nodes) cmd_nodes "$@" ;;
        env) cmd_env "$@" ;;
        exec) cmd_exec "$@" ;;
        spawn) cmd_spawn "$@" ;;
        stop) cmd_stop "$@" ;;
        netem) cmd_netem "$@" ;;
        selftest) cmd_selftest "$@" ;;
        listen) cmd_listen "$@" ;;
        send) cmd_send "$@" ;;
        pair) cmd_pair "$@" ;;
        matrix) cmd_matrix "$@" ;;
        bench) cmd_bench "$@" ;;
        benchpair) cmd_benchpair "$@" ;;
        benchmatrix) cmd_benchmatrix "$@" ;;
        help | -h | --help) usage ;;
        *) die "unknown command '$cmd' (lab.sh help)" ;;
    esac
}

main "$@"
