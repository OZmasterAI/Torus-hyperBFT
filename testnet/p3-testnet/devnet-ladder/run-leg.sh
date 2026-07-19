#!/usr/bin/env bash
# Run ONE devnet ladder leg against a FRESH chain, measuring node counters.
#
# Every leg wipes the devnet first: book state is path-dependent (a leg that
# inherits a saturated book with exhausted trader caps measures something
# different from one starting clean), so wiping is what keeps legs comparable
# across branches and rungs.
#
# Usage: ./run-leg.sh <image> <label> <taker_ratio> [markets] [senders] [duration]
#
# Env:
#   FLOW=match|rest|cross|churn   order-flow shape (default match)
#   BATCH / RATE                  bench shape (default 100 / 60)
#   PRESIGN                       pre-signed actions per sender (default 400)
#   TORUS_NATIVE_TOTAL_BLOCK_CAP / _ORDERS_PER_BLOCK_CAP / _BLOCK_BYTES_CAP
#                                 block caps; unset = devnet compose defaults
#                                 (400/400k/2MB). Shipped defaults, which the
#                                 testnet runs, are 100/50k/6MB.
#   RESULTS_DIR                   output root (default ./results next to this script)
#
# PRESIGN NOTE: the documented default of 60 runs every sender DRY after ~20s
# ("ammo exhausted" in bench.log) while the harness keeps measuring an IDLE
# chain — which silently dilutes "sustained" with idle time. 400 sustains ~120s
# and stays nonce-window safe: the guard at bench main.rs:1400 warns when
# pre_sign*submit_batch ms exceeds NONCE_WINDOW_MS/2 (30000ms); 400*15=6000ms.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
PROJ=${PROJ:-p3ladder}
RESULTS_DIR=${RESULTS_DIR:-$HERE/results}

IMAGE=${1:?image}; LABEL=${2:?label}; TAKER=${3:?taker_ratio}
MARKETS=${4:-10}; SENDERS=${5:-60}; DUR=${6:-120}
PRESIGN=${PRESIGN:-400}
BENCH_BIN=${BENCH_BIN:-$REPO/target/release/bench-throughput}

echo "###### LEG $LABEL (flow=${FLOW:-match} taker=$TAKER markets=$MARKETS senders=$SENDERS dur=${DUR}s) ######"
[ -x "$BENCH_BIN" ] || { echo "FATAL: bench binary missing: $BENCH_BIN (cargo build --release -p bench-throughput)"; exit 1; }

# Block-size caps are compose-interpolated. `sudo` STRIPS the caller env, so any
# override must ride INLINE on `sudo env ...` — exporting would silently no-op
# and the leg would quietly run devnet defaults while claiming other caps.
CAPENV=()
for v in TORUS_NATIVE_TOTAL_BLOCK_CAP TORUS_NATIVE_ORDERS_PER_BLOCK_CAP TORUS_NATIVE_BLOCK_BYTES_CAP; do
    [ -n "${!v:-}" ] && CAPENV+=("$v=${!v}")
done
echo "cap overrides: ${CAPENV[*]:-<none, devnet compose defaults 400/400k/2MB>}"

COMPOSE=(-f "$REPO/devnet/docker-compose.yml" -f "$HERE/compose.10mkt.yml")
cd "$REPO"
sudo -n env DEVNET_IMAGE="$IMAGE" "${CAPENV[@]}" docker compose -p "$PROJ" "${COMPOSE[@]}" \
    down -v --remove-orphans >/dev/null 2>&1 || true
sudo -n env DEVNET_IMAGE="$IMAGE" "${CAPENV[@]}" docker compose -p "$PROJ" "${COMPOSE[@]}" \
    up -d >/dev/null 2>&1

# Wait for liveness instead of sleeping a fixed guess.
h=0
for i in $(seq 1 60); do
    h=$(curl -s -m 3 -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' http://127.0.0.1:8645 \
        2>/dev/null | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo 0)
    [ "${h:-0}" -gt 3 ] && { echo "chain live at height $h after ${i}s"; break; }
    sleep 1
done
[ "${h:-0}" -gt 3 ] || { echo "FATAL: chain never went live"; exit 1; }

# All requested markets must be registered or the RPC rejects the order flow (S432).
nm=$(curl -s -m 5 -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"torus_getMarkets","params":[]}' http://127.0.0.1:8645 \
     | python3 -c "import sys,json;print(len(json.load(sys.stdin)['result']))")
[ "$nm" -ge "$MARKETS" ] || { echo "FATAL: need $MARKETS markets, chain has $nm"; exit 1; }
echo "markets registered: $nm"

# CPU sampler. A throughput plateau is uninterpretable without knowing WHO is pegged:
#   validators pegged + bench idle -> chain-bound (a real ceiling)
#   bench pegged + validators idle -> load-generator-bound (rig artifact)
#   everything pegged              -> host-bound (rig too small for the question)
mkdir -p "$RESULTS_DIR/$LABEL"
CPUSTOP="$RESULTS_DIR/$LABEL/.cpu-stop"; rm -f "$CPUSTOP"
{
  echo "ts,load1,val0,val1,val2,val3,rpcnode,bench"
  while [ ! -f "$CPUSTOP" ]; do
    stats=$(sudo -n docker stats --no-stream --format '{{.Name}} {{.CPUPerc}}' 2>/dev/null | tr -d '%')
    g() { awk -v n="$1" '$1==n{print $2}' <<<"$stats" | head -1; }
    # `ps -C` exits 1 before the bench launches; under `set -o pipefail` that
    # would kill this subshell on iteration 1 and leave a header-only csv.
    bench=$(ps -C bench-throughput -o %cpu= 2>/dev/null | awk '{s+=$1} END{print s+0}' || true)
    echo "$(date +%s),$(awk '{print $1}' /proc/loadavg),$(g "${PROJ}-validator-0-1"),$(g "${PROJ}-validator-1-1"),$(g "${PROJ}-validator-2-1"),$(g "${PROJ}-validator-3-1"),$(g "${PROJ}-rpc-node-1"),$bench"
  done
} > "$RESULTS_DIR/$LABEL/cpu.csv" 2>/dev/null &
CPUPID=$!

REPO="$REPO" BENCH_BIN="$BENCH_BIN" LEG_IMAGE="$IMAGE" \
RPC=http://127.0.0.1:8645 METRICS=http://127.0.0.1:9091/metrics \
DURATION="$DUR" SUBWIN=60 MARKETS="$MARKETS" SENDERS="$SENDERS" \
TAKER_RATIO="$TAKER" PRESIGN="$PRESIGN" BATCH="${BATCH:-100}" RATE="${RATE:-60}" \
  "$HERE/../measure-leg.sh" "$LABEL" "$RESULTS_DIR" "${FLOW:-match}"

touch "$CPUSTOP"; wait "$CPUPID" 2>/dev/null || true

# Saturation + health context, captured WITH the numbers so a rate can be explained.
grep -E 'Drop rate|Submitted:|Included:|Block time:' "$RESULTS_DIR/$LABEL/bench.log" 2>/dev/null \
  > "$RESULTS_DIR/$LABEL/offered-vs-included.txt" || true
{
  curl -s -m5 http://127.0.0.1:9091/metrics | grep -E '^torus_orders_rejected_total|^torus_native_resting_depth|^torus_consensus_view|^torus_block_height' | sort
} > "$RESULTS_DIR/$LABEL/post-metrics.txt" 2>&1
# DA/wedge signals: absence of evidence only counts if you looked.
{
  for c in $(sudo -n docker compose -p "$PROJ" ps --format '{{.Name}}' 2>/dev/null); do
    log=$(sudo -n docker logs "$c" 2>&1 || true)
    printf '%s: body_fetch_exhausted=%s hash_only=%s send_queue_full=%s compact_fallback=%s\n' "$c" \
      "$(grep -ci 'body fetch exhausted' <<<"$log")" "$(grep -ci 'hash-only' <<<"$log")" \
      "$(grep -ci 'Send Queue full' <<<"$log")" "$(grep -ci 'CompactBlock' <<<"$log")"
  done
} > "$RESULTS_DIR/$LABEL/da-signals.txt" 2>&1

echo "done -> $RESULTS_DIR/$LABEL/"
