#!/bin/bash
D=/home/oz/bench-results-matched/presuite-105a6e28
export CARGO_TARGET_DIR=$HOME/.cargo-target-read-gas
cd /home/oz/projects/wt/read-gas-followup
t0=$(date +%s)
cargo nextest run --workspace --cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar > $D/1-nextest.log 2>&1
echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/1-nextest.rc
t0=$(date +%s)
cargo test --workspace --doc -q > $D/2-doc.log 2>&1
echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/2-doc.rc
t0=$(date +%s)
cargo test --workspace --no-fail-fast -q > $D/3-fulltest.log 2>&1
echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/3-fulltest.rc
t0=$(date +%s); cargo clippy --workspace --all-targets > $D/4-cand-clippy.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-cand-clippy.rc
cargo fmt --all --check > $D/4-cand-fmt.log 2>&1; echo "rc=$?" > $D/4-cand-fmt.rc
git -C /home/oz/projects/torus-economy/Torus-hyperBFT worktree add -q --detach /home/oz/projects/wt/read-gas-base origin/main
cd /home/oz/projects/wt/read-gas-base
touch $(git -C /home/oz/projects/wt/read-gas-followup diff --name-only origin/main HEAD -- '*.rs')
t0=$(date +%s); cargo clippy --workspace --all-targets > $D/4-base-clippy.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-base-clippy.rc
cargo fmt --all --check > $D/4-base-fmt.log 2>&1; echo "rc=$?" > $D/4-base-fmt.rc
cd /home/oz/projects/wt/read-gas-followup
t0=$(date +%s); python3 -I -m pytest -q tools/matched-bench > $D/6-pytest.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/6-pytest.rc
for t in tools/matched-bench/test_*.py; do b=$(basename "$t" .py); python3 "$t" > "$D/6-$b.log" 2>&1; echo "$t rc=$?" >> $D/6-scripts.rc; done
grep -c Checking $D/4-base-clippy.log > $D/4-base-checked.txt
grep -c Checking $D/4-cand-clippy.log > $D/4-cand-checked.txt
touch $D/ALLDONE
