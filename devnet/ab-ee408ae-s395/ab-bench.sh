#!/bin/bash
# One loaded A/B leg: fresh 4-val devnet -> settle -> pre-metrics -> paced
# pre-signed bench (+height sampling + one docker-stats snapshot) -> drain ->
# post-metrics -> exec phase table -> teardown.
# Usage: ab-bench.sh <override-yml> <label> [duration] [rate] [batch_size]
set -euo pipefail

AB_DIR="$(cd "$(dirname "$0")" && pwd)"
OVERRIDE="$1"
LABEL="$2"
DURATION="${3:-60}"
RATE="${4:-20}"
BS="${5:-100}"
REPO=/home/crab/projects/Torus-hyperBFT
RPC=http://localhost:8645
PRESIGN=$(( (RATE * DURATION) / 100 + 4 ))

COMPOSE=(docker compose -p ab
  -f "$REPO/devnet/docker-compose.yml"
  -f "$AB_DIR/$OVERRIDE")

height() {
  curl -s -X POST -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
    "$RPC" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))'
}

cleanup() { "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

"${COMPOSE[@]}" up -d validator-0 validator-1 validator-2 validator-3 >/dev/null 2>&1

# settle: height >= 20 and rising, max 180s
LAST=-1
for _ in $(seq 60); do
  H=$(height 2>/dev/null || echo -1)
  if [ "$H" -ge 20 ] && [ "$H" -gt "$LAST" ] && [ "$LAST" -ge 0 ]; then break; fi
  LAST=$H
  sleep 3
done
echo "$LABEL: settled at height $(height)"

curl -s localhost:9091/metrics > "$AB_DIR/$LABEL.pre.metrics"

# background height sampler (10s cadence) + one stats snapshot mid-bench
( while true; do echo "$(date +%s),$(height 2>/dev/null || echo -1)"; sleep 10; done \
    > "$AB_DIR/$LABEL.heights.csv" ) & SAMPLER=$!
( sleep 35; docker stats --no-stream --format '{{.Name}} {{.CPUPerc}}' \
    > "$AB_DIR/$LABEL.stats" ) & STATS=$!

H1=$(height)
T1=$(date +%s)
"$REPO/target/release/bench-throughput" consensus \
  --rpc-urls http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548 \
  --duration "$DURATION" --batch-size "$BS" --submit-batch 100 \
  --pre-sign "$PRESIGN" --rate "$RATE" --markets 1 --senders 20 \
  2>&1 | tee "$AB_DIR/$LABEL.bench.out"
T2=$(date +%s)
H2=$(height)

kill $SAMPLER $STATS 2>/dev/null || true
sleep 10  # drain
H3=$(height)
curl -s localhost:9091/metrics > "$AB_DIR/$LABEL.post.metrics"

WALL=$((T2 - T1))
BLOCKS=$((H2 - H1))
echo "$LABEL SUMMARY: bench wall=${WALL}s blocks=$BLOCKS ($H1->$H2, drain->$H3)"
if [ "$BLOCKS" -gt 0 ]; then
  echo "$LABEL ms/blk under load: $(( WALL * 1000 / BLOCKS ))"
fi
python3 "$REPO/devnet/scripts/exec_phase_table.py" \
  "$AB_DIR/$LABEL.pre.metrics" "$AB_DIR/$LABEL.post.metrics" \
  | tee "$AB_DIR/$LABEL.phases.txt" || true
