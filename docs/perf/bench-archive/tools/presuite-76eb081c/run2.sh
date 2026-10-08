#!/bin/bash
D=/home/oz/bench-results-matched/presuite-76eb081c
# candidate
cd /home/oz/projects/wt/read-gas-bench
export CARGO_TARGET_DIR=$HOME/.cargo-target-read-gas
t0=$(date +%s); ci/run-local.sh check > $D/4-cand-runlocal.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-cand-runlocal.rc
t0=$(date +%s); cargo clippy --workspace --all-targets > $D/4-cand-clippy.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-cand-clippy.rc
cargo fmt --all --check > $D/4-cand-fmt.log 2>&1; echo "rc=$?" > $D/4-cand-fmt.rc
# baseline
cd /home/oz/projects/wt/main-b8bf3e8a
export CARGO_TARGET_DIR=$HOME/.cargo-target-main
t0=$(date +%s); ci/run-local.sh check > $D/4-base-runlocal.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-base-runlocal.rc
t0=$(date +%s); cargo clippy --workspace --all-targets > $D/4-base-clippy.log 2>&1; echo "rc=$? wall=$(( $(date +%s)-t0 ))s" > $D/4-base-clippy.rc
cargo fmt --all --check > $D/4-base-fmt.log 2>&1; echo "rc=$?" > $D/4-base-fmt.rc
