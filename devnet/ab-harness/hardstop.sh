#!/usr/bin/env bash
# Task B/5: hard-stop (kill -9) a DEVNET node mid-load with TORUS_SYNC_WAL_ON_COMMIT=1,
# restart it, confirm crash-recovery finds NO committed-height-with-missing-body hole
# (no wedge, no exit(70)); node rejoins and continues.
# Usage: hardstop.sh <tag> <binaryB>
set -uo pipefail
source /home/18c/torus-ab/harness.sh
TAG=$1; BIN=$2
BENCH=/home/18c/torus-ab/bin/bench-throughput
RPCS="http://127.0.0.1:18555,http://127.0.0.1:18556,http://127.0.0.1:18557,http://127.0.0.1:18558"
VICTIM=3

launch "$TAG" "$BIN" 1     # sync_wal ON
wait_chain "$TAG" || { teardown "$TAG"; exit 1; }
sleep 6
mapfile -t P < "$RUN/$TAG/pids"
echo "pids: ${P[*]}  victim=node-$VICTIM pid=${P[$VICTIM]}"

echo "--- start background load ---"
$PIN "$BENCH" consensus --rpc-urls "$RPCS" --batch-size 50 --senders 8 \
   --duration 60 --rate 20 --pre-sign 1200 --sign-mode session --markets 4 \
   --format bin > "$RUN/$TAG/bench.txt" 2>&1 &
BENCHPID=$!
sleep 12

hv0=$(height $(rpcport $VICTIM))
echo "--- KILL -9 node-$VICTIM (height=$hv0) mid-load ---"
kill -9 "${P[$VICTIM]}"
sleep 6
echo "victim killed; other nodes height=$(height $(rpcport 0))"

echo "--- RESTART node-$VICTIM (crash recovery into existing data-dir) ---"
launch_one "$TAG" "$BIN" 1 "$VICTIM"
# watch recovery
for i in $(seq 1 30); do
  hv=$(height $(rpcport $VICTIM)); h0=$(height $(rpcport 0))
  echo "  t=${i}s victim=$hv leader=$h0"
  sleep 2
done

wait "$BENCHPID" 2>/dev/null || true
echo "=== victim node log tail (last 40) ==="
tail -40 "$RUN/$TAG/node-$VICTIM.log"
echo "=== check for wedge/exit70/hole markers in victim log ==="
grep -niE 'exit\(70\)|wedge|missing body|committed.*hole|no body|panic|FATAL|corrupt' "$RUN/$TAG/node-$VICTIM.log" | tail -20 || echo "NONE FOUND"
hv=$(height $(rpcport $VICTIM)); h0=$(height $(rpcport 0))
echo "FINAL victim=$hv leader=$h0"
teardown "$TAG"
