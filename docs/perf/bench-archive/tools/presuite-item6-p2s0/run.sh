#!/bin/bash
# Pre-commit suites for perf/item6-phase2 step 0 (ozarchy s27), modeled on presuite-105a6e28/run.sh.
D=/home/oz/bench-results-matched/presuite-item6-p2s0
export CARGO_TARGET_DIR=$HOME/.cargo-target-read-gas
C=/home/oz/projects/wt/item6-phase2
B=/home/oz/projects/wt/item6-phase2-base
cd $C
git rev-parse HEAD > $D/cand-head.txt; git status --short > $D/cand-status.txt
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
cd $B
git rev-parse HEAD > $D/base-head.txt
touch $(git -C $C diff --name-only d3ba3c0a -- '*.rs' | while read f; do [ -e "$B/$f" ] && echo "$B/$f"; done)
t0=$(date +%s); cargo clippy --workspace --all-targets > $D/4-base-clippy.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-base-clippy.rc
cargo fmt --all --check > $D/4-base-fmt.log 2>&1; echo "rc=$?" > $D/4-base-fmt.rc
cd $C
t0=$(date +%s); python3 -I -m pytest -q tools/matched-bench > $D/6-pytest.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/6-pytest.rc
: > $D/6-scripts.rc
for t in tools/matched-bench/test_*.py; do b=$(basename "$t" .py); python3 "$t" > "$D/6-$b.log" 2>&1; echo "$t rc=$?" >> $D/6-scripts.rc; done
grep -c Checking $D/4-base-clippy.log > $D/4-base-checked.txt
grep -c Checking $D/4-cand-clippy.log > $D/4-cand-checked.txt
touch $D/ALLDONE
