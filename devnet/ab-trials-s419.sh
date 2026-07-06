#!/usr/bin/env bash
# S419 interleaved A/B trials: O2-head vs bisect2 binaries, bs=400 @512KB,
# single-leg sweep protocol, alternating to balance box drift.
set -euo pipefail
cd "$(dirname "$0")/.."

SP=/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/b487b463-3064-4bde-bdb9-4fb28565f781/scratchpad
OUT=devnet/ab-trials-s419.csv
echo "trial,binary,orders_s,block_ms_fit,avg_native_per_block,pull_delta,wedged" > "$OUT"

run_leg() { # $1=trial# $2=binary-label $3=node-bin $4=bench-bin
    cp "$3" target/release/torus-node
    cp "$4" target/release/bench-throughput
    local csv="devnet/ab-trial-$1-$2.csv"
    LEGS="400:3" OUT="$(basename "$csv")" bash devnet/sweep-o2-batchsize-s416.sh || true
    # sweep writes into devnet/ (its own cwd); append its data row to ours
    local row
    row=$(tail -1 "devnet/$(basename "$csv")" 2>/dev/null || echo "0,0,0,0,0,0,1")
    echo "$1,$2,$(echo "$row" | cut -d, -f3-)" >> "$OUT"
    echo "=== trial $1 ($2): $row ==="
}

run_leg 1 o2head  "$SP/torus-node.o2head"  "$SP/bench-throughput.o2head"
run_leg 2 bisect2 "$SP/torus-node.bisect2" "$SP/bench-throughput.bisect2"
run_leg 3 o2head  "$SP/torus-node.o2head"  "$SP/bench-throughput.o2head"
run_leg 4 bisect2 "$SP/torus-node.bisect2" "$SP/bench-throughput.bisect2"

echo "=== FINAL $OUT ==="
cat "$OUT"
