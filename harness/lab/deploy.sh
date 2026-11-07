#!/usr/bin/env bash
# Builds the golem-p2p CLI and copies the lab (scripts, CLI, iroh-relay, the
# real relay's certificate) to the lab VM. Run on the build machine.
#
#   harness/lab/deploy.sh            build + copy
#   GPL_SKIP_BUILD=1 deploy.sh       copy the existing target/release/golem-p2p
#
# Environment (defaults in brackets):
#   GPL_SSH_CONFIG     ssh config [~/.ssh/p2p/ssh_config]
#   GPL_HOST           lab VM [p2p-natlab]
#   GPL_REMOTE_DIR     directory on the VM, relative to home [golem-p2p-lab]
#   GPL_RELAY_HOST     host to copy iroh-relay from if the VM lacks it [p2p-relay1]
#   IROH_RELAY_BIN     local iroh-relay binary to copy instead
#   GPL_REAL_RELAY_CA_SRC  certificate of the real relay [~/.ssh/p2p/p2p-relay1.crt]
set -euo pipefail

SSH_CONFIG=${GPL_SSH_CONFIG:-$HOME/.ssh/p2p/ssh_config}
HOST=${GPL_HOST:-p2p-natlab}
REMOTE_DIR=${GPL_REMOTE_DIR:-golem-p2p-lab}
RELAY_HOST=${GPL_RELAY_HOST:-p2p-relay1}
REAL_CA=${GPL_REAL_RELAY_CA_SRC:-$HOME/.ssh/p2p/p2p-relay1.crt}

here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
r() { ssh -F "$SSH_CONFIG" "$HOST" "$@"; }

if [[ -z ${GPL_SKIP_BUILD:-} ]]; then
    (cd "$repo" && cargo build --release -p golem-p2p-cli)
fi
bin=$repo/target/release/golem-p2p
[[ -x $bin ]] || { echo "no $bin" >&2; exit 1; }

echo "deploy: scripts -> $HOST:~/$REMOTE_DIR"
r "mkdir -p $REMOTE_DIR/bin $REMOTE_DIR/certs"
tar -C "$here" --exclude=__pycache__ -cf - lab.sh tools topologies | r "tar -C $REMOTE_DIR -xf -"

local_sum=$(sha256sum "$bin" | cut -d' ' -f1)
remote_sum=$(r "sha256sum $REMOTE_DIR/bin/golem-p2p 2>/dev/null | cut -d' ' -f1" || true)
if [[ $local_sum != "$remote_sum" ]]; then
    echo "deploy: golem-p2p -> $HOST ($local_sum)"
    r "cat > $REMOTE_DIR/bin/golem-p2p.new && chmod 755 $REMOTE_DIR/bin/golem-p2p.new && mv $REMOTE_DIR/bin/golem-p2p.new $REMOTE_DIR/bin/golem-p2p" <"$bin"
fi

if ! r "test -x $REMOTE_DIR/bin/iroh-relay"; then
    if [[ -n ${IROH_RELAY_BIN:-} ]]; then
        echo "deploy: iroh-relay from $IROH_RELAY_BIN"
        r "cat > $REMOTE_DIR/bin/iroh-relay && chmod 755 $REMOTE_DIR/bin/iroh-relay" <"$IROH_RELAY_BIN"
    else
        echo "deploy: iroh-relay from $RELAY_HOST:/usr/local/bin/iroh-relay"
        ssh -F "$SSH_CONFIG" "$RELAY_HOST" "cat /usr/local/bin/iroh-relay" |
            r "cat > $REMOTE_DIR/bin/iroh-relay && chmod 755 $REMOTE_DIR/bin/iroh-relay"
    fi
fi

if [[ -f $REAL_CA ]]; then
    r "cat > $REMOTE_DIR/certs/real-relay.crt" <"$REAL_CA"
fi

if ! r "command -v nft >/dev/null && command -v conntrack >/dev/null"; then
    echo "deploy: installing nftables, conntrack, tcpdump on $HOST"
    r "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nftables conntrack tcpdump >/dev/null"
fi

commit=$(git -C "$repo" rev-parse --short HEAD)
dirty=$(git -C "$repo" status --porcelain | grep -q . && echo "-dirty" || true)
echo "$commit$dirty $(date -u +%Y-%m-%dT%H:%M:%SZ)" | r "cat > $REMOTE_DIR/DEPLOYED"
r "cd $REMOTE_DIR && echo \"deployed: \$(cat DEPLOYED)\" && bin/golem-p2p --version && bin/iroh-relay --version && python3 --version"
echo "deploy: done. On the VM: cd ~/$REMOTE_DIR && ./lab.sh up selftest"
