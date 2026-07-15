#!/usr/bin/env bash
set -euo pipefail
cd /home/18c/torus-ab
source ~/.cargo/env 2>/dev/null || true
export CARGO_TARGET_DIR=/home/18c/torus-ab/target
mkdir -p /home/18c/torus-ab/bin
echo "=== [$(date +%T)] ARM A build: feat/pacemaker-backoff ==="
git checkout -q feat/pacemaker-backoff
git rev-parse HEAD
nice -n 19 cargo build --release -p torus-node -p bench-throughput
cp target/release/torus-node bin/torus-node-A
cp target/release/bench-throughput bin/bench-throughput
echo "=== [$(date +%T)] ARM B build: feat/commit-fsync-durability ==="
git checkout -q feat/commit-fsync-durability
git rev-parse HEAD
nice -n 19 cargo build --release -p torus-node
cp target/release/torus-node bin/torus-node-B
echo "=== [$(date +%T)] BUILD DONE ==="
ls -la bin/
sha256sum bin/torus-node-A bin/torus-node-B bin/bench-throughput
