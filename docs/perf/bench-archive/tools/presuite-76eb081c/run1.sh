#!/bin/bash
cd /home/oz/projects/wt/read-gas-bench
export CARGO_TARGET_DIR=$HOME/.cargo-target-read-gas
D=/home/oz/bench-results-matched/presuite-76eb081c
t0=$(date +%s)
cargo nextest run --workspace --cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar > $D/1-nextest.log 2>&1
echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/1-nextest.rc
t0=$(date +%s)
cargo test --workspace --doc -q > $D/2-doc.log 2>&1
echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/2-doc.rc
t0=$(date +%s)
cargo test --workspace --no-fail-fast -q > $D/3-fulltest.log 2>&1
echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/3-fulltest.rc
