#!/usr/bin/env bash
# One-line launcher for the B-blind Gate 2 campaign (ozarchy):
#   /home/oz/bench-results-matched/ozarchy-bblind-gate2-launch.sh <CANDIDATE_SHA>
# 1. prep in the foreground (worktree, build into its own target dir, stage main; no node starts) unless build.done says exit=0
# 2. requires the SIGKILL trace to be running already (owner, in a terminal, sudo):
#      sudo perf record -e signal:signal_generate --filter 'sig == 9' -a -g -o /tmp/sigkill-bblind.data
# 3. detaches the campaign driver as unit bench-bblind-<s7>-gate2 (harness detach.sh @ ab12c75); each cell then runs as its
#    own unit bench-<label>. Log $R/ozarchy-bblind-<s7>-gate2.campaign.log, marker $R/ozarchy-bblind-<s7>-gate2.campaign.done.
set -u
SHA_IN=${1:?usage: ozarchy-bblind-gate2-launch.sh <CANDIDATE_SHA>}
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
DETACH=/home/oz/projects/wt/harness/tools/matched-bench/campaign/detach.sh
git -C "$REPO" fetch -q origin
SHA=$(git -C "$REPO" rev-parse --verify -q "$SHA_IN^{commit}") || { echo "unknown commit $SHA_IN"; exit 1; }
S7=${SHA:0:7}
if [ "$(cat "$R/ozarchy-bblind-$S7-build/build.done" 2>/dev/null)" != "exit=0" ]; then
    "$R/ozarchy-bblind-gate2-prep.sh" "$SHA" || exit 1
fi
[ -n "$(pgrep -f -- 'perf record -e signal:signal_generate')" ] || [ "${SIGTRACE_REQUIRED:-1}" = 0 ] || {
    echo "SIGKILL trace not running. Start it first in a terminal:"
    echo "  sudo perf record -e signal:signal_generate --filter 'sig == 9' -a -g -o /tmp/sigkill-bblind.data"
    exit 1
}
[ -z "$(pgrep 'cargo|rustc')" ] || { echo "cargo/rustc running: host not quiet"; exit 1; }
[ ! -e "$R/ozarchy-bblind-$S7-gate2.campaign.done" ] || { echo "marker exists: $R/ozarchy-bblind-$S7-gate2.campaign.done (rename it first)"; exit 1; }
DRY_RUN=1 "$R/ozarchy-bblind-gate2-campaign.sh" "$SHA" | grep -q 'PREFLIGHT OK' || { DRY_RUN=1 "$R/ozarchy-bblind-gate2-campaign.sh" "$SHA"; exit 1; }
cd "$R" || exit 1
"$DETACH" "bblind-$S7-gate2" "$R/ozarchy-bblind-$S7-gate2.campaign.log" \
    bash -c "$R/ozarchy-bblind-gate2-campaign.sh $SHA; echo \"exit=\$?\" > $R/ozarchy-bblind-$S7-gate2.campaign.done"
sleep 2
systemctl --user is-active "bench-bblind-$S7-gate2.service"
