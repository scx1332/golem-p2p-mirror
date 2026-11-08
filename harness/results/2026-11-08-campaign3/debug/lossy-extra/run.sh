# Runs on the lab VM (piped to bash over ssh): 10 extra lab-netem-pr runs
# (pr2 listener <- lossy1 bench, --compare-relay), same as the campaign's
# scenario, for the packet_loss detection rate, the settle window and the
# relay fallback on the lossy link.
set -u
cd ~/golem-p2p-lab
./lab.sh up campaign >/dev/null
base=~/gpl-runs/2026-11-08-campaign3/debug/lossy-extra
for r in $(seq 10); do
    echo "lossy-extra: $(date -u +%T) run $r" >&2
    ./lab.sh benchpair pr2 lossy1 --out $base/run$r --run-id lossy-extra-r$r -- --compare-relay >/dev/null || echo "run $r: no report" >&2
done
echo "lossy-extra: done $(date -u +%T)" >&2
