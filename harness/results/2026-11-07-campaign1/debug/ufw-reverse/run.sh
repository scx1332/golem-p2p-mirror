#!/bin/bash
# Debug: Hetzner bench (random port, ufw) -> lab real1 listener. Verbose iroh logs + captures.
set -u
D=$(pwd)
BIN=/home/ubuntu/golem/golem-p2p-bench/target/release/golem-p2p
S="ssh -F $HOME/.ssh/p2p/ssh_config p2p-natlab"
RELAY="https://213.163.75.157:56000,quic=56001"
DIR=http://213.163.75.157:56050/
LOGF="warn,golem_p2p=info,iroh::_events=trace,iroh::socket::remote_map=trace,noq_proto::n0_nat_traversal=trace"
tag=dbg-ufw-$1
$S "cd golem-p2p-lab && ./lab.sh spawn real1 $tag env RUST_LOG='$LOGF' golem-p2p --json --run-id $tag --node-name real1 listen --key /run/gpl/keys/real1.key --relay '$RELAY' --relay-ca /home/ubuntu/golem-p2p-lab/certs/real-relay.crt --directory $DIR" >/dev/null
for i in $(seq 100); do lid=$($S "grep -m1 -o '\"node_id\":\"0x[0-9a-f]*\"' /run/gpl/logs/$tag.log" 2>/dev/null | cut -d'"' -f4); [[ -n $lid ]] && break; sleep 0.2; done
echo "listener $lid"
$S "sudo timeout 30 tcpdump -ni enp5s0 -c 300 'udp and host 65.109.154.126' 2>&1" > vm-tcpdump-$1.txt &
TP=$!
sudo -n timeout 30 tcpdump -ni any -c 300 '(udp and host 213.163.75.157 and not port 56001) or (icmp and host 213.163.75.157)' > local-tcpdump-$1.txt 2>&1 &
LP=$!
sleep 1.5
NO_COLOR=1 RUST_LOG="$LOGF" timeout 60 $BIN --json --run-id $tag --node-name hetzner bench --relay "$RELAY" --relay-ca ~/.ssh/p2p/p2p-relay1.crt --directory $DIR --to $lid --skip upload,download,concurrency --latency-duration 15s ${2:-} > bench-$1.jsonl 2> bench-$1.err
echo "bench exit $?"
$S "cd golem-p2p-lab && ./lab.sh stop $tag; cp /run/gpl/logs/$tag.log /tmp/$tag.log; cat /tmp/$tag.log; rm -f /run/gpl/logs/$tag.log /tmp/$tag.log" > listen-$1.log 2>&1
wait $TP $LP
tail -1 bench-$1.jsonl | python3 -c 'import json,sys; r=json.loads(sys.stdin.read()); print(r["status"], r["peer"]["path"]["final_path"], r["peer"]["path"]["time_to_direct_ms"])'
