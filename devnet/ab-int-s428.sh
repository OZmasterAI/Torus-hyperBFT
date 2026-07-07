#!/usr/bin/env bash
# S426 BS-4a/4b devnet A/B: old (fleet-pin 6e03294 code) vs new
# (perf/bs4a-da-recovery e05b65c), O2 bs=400 single-first-leg protocol (S419),
# interleaved old/new to balance box drift. Two phases:
#   clean  @512KB pinned  — no-regression gate (orders/s, block_ms_fit)
#   stress @6MB compose-mirror (node clamps to 4MB) — fullpush body-miss
#          injection, the S419-reproduced collapse config (448 orders/s on
#          pre-BS4a code); BS-4a success = p99 view-duration down + survival
# One bench binary (e05b65c) for ALL trials — bench code is identical
# 6e03294..e05b65c (diff touches only torus-consensus + torus-telemetry).
# Per-trial artifacts (FULL metrics dumps incl. histogram buckets, node logs,
# heights) under devnet/ab-int-s428/trial-*/; devnet torn down after each
# trial and on any exit.
# Syntax gate: bash -n devnet/ab-int-s428.sh
set -euo pipefail
cd "$(dirname "$0")"

SP=${SP:?path to stashed binaries (torus-node.old/.new, bench-throughput.new)}
CSV=ab-int-s428.csv
OUTROOT=ab-int-s428
PY=scripts/ab-metrics-delta.py
NA_FRAG="na,na,na,na,na,na,na"

BS=400
RATE=3
DURATION=${DURATION:-50} # bench pre-signed nonce window: keep under ~55s
SENDERS=20
MARKETS=4 # genesis seeds 4 markets — bench --markets must not exceed (S419)
SIGN=session
RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548"
V0_RPC=http://localhost:8645
METRICS_PORTS="9091 9092 9093 9094"

for f in torus-node.old torus-node.new bench-throughput.new; do
    [ -x "$SP/$f" ] || { echo "missing $SP/$f" >&2; exit 1; }
done
cp "$SP/bench-throughput.new" ../target/release/bench-throughput

trap 'docker compose down -v >/dev/null 2>&1 || true' EXIT

height() {
    curl -s -m 2 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$V0_RPC" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))' \
        2>/dev/null || echo -1
}

snap_metrics() { # $1=dir $2=label — FULL dumps (histogram buckets needed for p99)
    local p
    for p in $METRICS_PORTS; do
        { curl -s -m 2 "http://localhost:$p/metrics" || true; } \
            > "$1/metrics-$p-$2.txt" || true
    done
}

mkdir -p "$OUTROOT"
# APPEND=1: extra-trial rerun — keep the existing CSV and its header
if [ "${APPEND:-0}" != 1 ]; then
    echo "trial,binary,phase,threshold,orders_s,block_ms_fit,avg_native_per_block,wedged,p50_view_ms,p99_view_ms,views,pulls,missing_rej,handoffs,timeouts" > "$CSV"
fi

run_trial() { # $1=trial# $2=old|new $3=threshold $4=clean|stress
    local n=$1 label=$2 thr=$3 phase=$4
    local dir="$OUTROOT/trial-$n-$label-$phase"
    mkdir -p "$dir"
    echo "=== trial $n: binary=$label phase=$phase threshold=$thr ==="
    cp "$SP/torus-node.$label" ../target/release/torus-node
    docker compose down -v >/dev/null 2>&1 || true
    TORUS_PUSH_THRESHOLD=$thr docker compose up -d --build \
        > "$dir/compose-up.log" 2>&1
    local up=0 h
    for _ in $(seq 1 30); do h=$(height); [ "$h" -gt 2 ] && { up=1; break; }; sleep 2; done
    if [ "$up" -ne 1 ]; then
        echo "trial $n ($label/$phase): chain never started" >&2
        docker compose logs --no-color > "$dir/compose-all.log" 2>&1 || true
        echo "$n,$label,$phase,$thr,0,0,0,1,$NA_FRAG" >> "$CSV"
        docker compose down -v >/dev/null 2>&1 || true
        return 0
    fi

    local h0 h1
    h0=$(height)
    snap_metrics "$dir" before
    : > "$dir/heights.tsv"
    (
        end=$((SECONDS + DURATION + 10))
        while [ $SECONDS -lt $end ]; do
            printf '%s\t%s\n' "$(date +%s.%N)" "$(height)" >> "$dir/heights.tsv"
            sleep 2
        done
    ) &
    local sampler=$!

    # Bench runs INSIDE the devnet image (host network), same as the sweep.
    docker run --rm --network host --entrypoint /bench \
        -v "$(cd .. && pwd)/target/release/bench-throughput:/bench:ro" \
        torus-devnet-node:local \
        consensus --rpc-urls "$RPCS" --batch-size "$BS" --senders "$SENDERS" \
        --duration "$DURATION" --rate "$RATE" --pre-sign "$((DURATION * RATE))" \
        --sign-mode "$SIGN" --markets "$MARKETS" \
        --format bin 2>&1 | tee "$dir/bench.txt" || true

    wait "$sampler" || true
    snap_metrics "$dir" after
    h1=$(height)

    # node logs BEFORE teardown (S419 repro lesson — teardown destroys them)
    local svc
    for svc in validator-0 validator-1 validator-2 validator-3 rpc-node; do
        docker compose logs --no-color "$svc" > "$dir/log-$svc.txt" 2>&1 || true
    done

    local orders_s avg_native block_ms wedged frag
    orders_s=$(grep -oE 'Sustained:\s+[0-9]+' "$dir/bench.txt" | grep -oE '[0-9]+' | tail -1 || true)
    avg_native=$(grep -oE '[0-9]+ avg native/block' "$dir/bench.txt" | grep -oE '^[0-9]+' | tail -1 || true)
    block_ms=$(python3 "$PY" fit "$dir/heights.tsv" || echo 0)
    wedged=0
    [ $((h1 - h0)) -lt 5 ] && wedged=1
    frag=$(python3 "$PY" deltas "$dir" || echo "$NA_FRAG")
    echo "$n,$label,$phase,$thr,${orders_s:-0},${block_ms:-0},${avg_native:-0},$wedged,$frag" >> "$CSV"
    tail -1 "$CSV"
    docker compose down -v >/dev/null 2>&1 || true
}

# Trial list: n:binary:threshold:phase — overridable for extra-trial reruns
# (e.g. APPEND=1 TRIALS="9:old:524288:clean 10:new:524288:clean").
# Default: clean phase 512KB pinned (S415-proven control; compose default is
# 6MB!) then stress phase 6MB testnet-mirror (clamped to 4MB direct-msg floor)
# — forces fullpush body misses at bs400 (~2.8MB bodies), S419 collapse repro.
TRIALS=${TRIALS:-"1:old:524288:clean 2:new:524288:clean 3:old:524288:clean 4:new:524288:clean 5:old:6000000:stress 6:new:6000000:stress 7:old:6000000:stress 8:new:6000000:stress"}
for t in $TRIALS; do
    n=${t%%:*}; rest=${t#*:}
    label=${rest%%:*}; rest=${rest#*:}
    thr=${rest%%:*}; phase=${rest##*:}
    run_trial "$n" "$label" "$thr" "$phase"
done

echo
echo "=== FINAL $CSV ==="
cat "$CSV"
