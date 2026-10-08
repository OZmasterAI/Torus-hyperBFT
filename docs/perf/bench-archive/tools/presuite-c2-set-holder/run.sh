#!/bin/bash
D=/home/oz/bench-results-matched/presuite-c2-set-holder
export CARGO_TARGET_DIR=/home/oz/.cargo-target-c2-set-holder
cd /home/oz/projects/wt/c2-set-holder
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
cd /home/oz/projects/wt/c2-set-holder-base
touch crates/torus-bridge/src/trader_positions.rs crates/torus-bridge/src/trader_positions_tests.rs
t0=$(date +%s); cargo clippy --workspace --all-targets > $D/4-base-clippy.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-base-clippy.rc
cargo fmt --all --check > $D/4-base-fmt.log 2>&1; echo "rc=$?" > $D/4-base-fmt.rc
echo done > $D/ALLDONE
