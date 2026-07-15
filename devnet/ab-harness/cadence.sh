#!/usr/bin/env bash
# Measure empty + under-load block cadence / commit latency for one arm.
# Usage: cadence.sh <tag> <binary> <sync 0|1>
set -uo pipefail
source /home/18c/torus-ab/harness.sh
TAG=$1; BIN=$2; SYNC=$3
EMPTY_WINDOW=${EMPTY_WINDOW:-40}
LOAD_DUR=${LOAD_DUR:-40}
BS=${BS:-50}; SENDERS=${SENDERS:-8}; RATE=${RATE:-20}
BENCH=/home/18c/torus-ab/bin/bench-throughput
RPCS="http://127.0.0.1:18555,http://127.0.0.1:18556,http://127.0.0.1:18557,http://127.0.0.1:18558"

echo "### ARM $TAG bin=$(basename "$BIN") sync=$SYNC"
launch "$TAG" "$BIN" "$SYNC"
wait_chain "$TAG" || { teardown "$TAG"; exit 1; }
sleep 5   # settle

echo "--- EMPTY phase ${EMPTY_WINDOW}s ---"
snap "$TAG" empty_before
h0=$(height $(rpcport 0)); t0=$(date +%s.%N)
sleep "$EMPTY_WINDOW"
h1=$(height $(rpcport 0)); t1=$(date +%s.%N)
snap "$TAG" empty_after
ewin=$(python3 -c "print($t1-$t0)")
echo "empty height $h0 -> $h1 over ${ewin}s"
echo "EMPTY_ANALYSIS_$TAG:"
python3 /home/18c/torus-ab/analyze.py "$RUN/$TAG" empty_before empty_after "$ewin" | tee "$RUN/$TAG/empty.json"

echo "--- LOAD phase dur=$LOAD_DUR bs=$BS senders=$SENDERS rate=$RATE ---"
snap "$TAG" load_before
lh0=$(height $(rpcport 0)); lt0=$(date +%s.%N)
$PIN "$BENCH" consensus --rpc-urls "$RPCS" --batch-size "$BS" --senders "$SENDERS" \
   --duration "$LOAD_DUR" --rate "$RATE" --pre-sign "$((LOAD_DUR*RATE))" \
   --sign-mode session --markets 4 --format bin > "$RUN/$TAG/bench.txt" 2>&1 || echo "bench returned $?"
lt1=$(date +%s.%N); lh1=$(height $(rpcport 0))
snap "$TAG" load_after
lwin=$(python3 -c "print($lt1-$lt0)")
python3 /home/18c/torus-ab/parse_bench.py "$RUN/$TAG/bench.txt" | tee "$RUN/$TAG/bench_summary.txt"; orders=$(grep -oE "orders_s=[0-9.NA]+" "$RUN/$TAG/bench_summary.txt" | cut -d= -f2)
echo "load height $lh0 -> $lh1 over ${lwin}s ; orders/s=$orders"
echo "LOAD_ANALYSIS_$TAG:"
python3 /home/18c/torus-ab/analyze.py "$RUN/$TAG" load_before load_after "$lwin" | tee "$RUN/$TAG/load.json"
echo "ORDERS_PER_S_$TAG=$orders"
tail -20 "$RUN/$TAG/bench.txt"

teardown "$TAG"
echo "### DONE $TAG"
