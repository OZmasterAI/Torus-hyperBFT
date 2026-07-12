#!/usr/bin/env bash
# S458 direct-to-leader devnet A/B: gossip pre-spread (--native-gossip=true, today's
# fleet mode) vs unicast-to-leader (--native-gossip=false, activates the RPC
# forward_bodies gate in main.rs — Step 2 of docs/plans/direct-to-leader-impl.md).
# ONE binary (current tree) for both arms; only TORUS_NATIVE_GOSSIP differs.
# Two phases, arms interleaved to balance box drift:
#   clean @rate3  — no-regression gate (orders/s, block_ms_fit, wedged)
#   flood @rate15 — drop-repro attempt: offered = SENDERS*RATE*BS = 120k orders/s.
#     Arm A success-signal: torus_native_gossip_dropped_full > 0 (send-queue overflow,
#     the S456-confirmed testnet bottleneck). Arm B bar: dropped_full identically 0
#     (no gossip publisher), da/forward path healthy, committed >= arm A, no wedge.
# Per-trial artifacts (FULL metric dumps, node logs, heights) under devnet/ab-d2l-s458/.
# Designed to run ON 18c from the repo checkout. Syntax gate: bash -n devnet/ab-d2l-s458.sh
set -euo pipefail
cd "$(dirname "$0")"
REPO=$(cd .. && pwd)

CSV=ab-d2l-s458.csv
OUTROOT=ab-d2l-s458
PY=scripts/ab-metrics-delta.py
NA_FRAG="na,na,na,na,na,na,na,na,na,na,na,na,na,na"

BS=${BS:-400}
SENDERS=${SENDERS:-20}
MARKETS=4 # genesis seeds 4 markets — bench --markets must not exceed (S419)
DURATION=${DURATION:-50} # bench pre-signed nonce window: keep under ~55s
SIGN=session
THRESHOLD=${TORUS_PUSH_THRESHOLD:-6000000}
RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548"
V0_RPC=http://localhost:8645
METRICS_PORTS="9091 9092 9093 9094"

# --- build current tree unless SKIP_BUILD=1; stage binaries where the
# Dockerfile COPY (target/release/torus-node) and the bench mount expect them.
if [ "${SKIP_BUILD:-0}" != 1 ]; then
    (cd "$REPO" && nice -n 10 cargo build --release -p torus-node -p bench-throughput)
fi
CT=${CARGO_TARGET_DIR:-$REPO/target}
if [ "$CT" != "$REPO/target" ]; then
    mkdir -p "$REPO/target/release"
    cp "$CT/release/torus-node" "$CT/release/bench-throughput" "$REPO/target/release/"
fi
for f in torus-node bench-throughput; do
    [ -x "$REPO/target/release/$f" ] || { echo "missing $REPO/target/release/$f" >&2; exit 1; }
done

trap 'docker compose down -v >/dev/null 2>&1 || true' EXIT

height() {
    curl -s -m 2 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$V0_RPC" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))' \
        2>/dev/null || echo -1
}

snap_metrics() { # $1=dir $2=label — FULL dumps
    local p
    for p in $METRICS_PORTS; do
        { curl -s -m 2 "http://localhost:$p/metrics" || true; } \
            > "$1/metrics-$p-$2.txt" || true
    done
}

d2l_deltas() { # $1=dir — cross-node counter deltas + v0 committed + leftover mempool
    python3 - "$1" <<'PYEOF'
import sys, os
d = sys.argv[1]
COUNTERS = [
    "torus_native_gossip_published_actions",
    "torus_native_gossip_dropped_full",
    "torus_native_gossip_dropped_oversized",
    "torus_native_gossip_received_actions",
    "torus_rpc_submit_admit_forward_seconds_count",
    "torus_direct_send_failures_untracked",
    "torus_native_da_pull_requests",
    "torus_native_da_pull_recovered",
    "torus_native_da_pull_failures",
    "torus_native_da_recovery_handoffs",
    "torus_native_da_recovery_timeouts",
]
def read(path):
    vals = {}
    try:
        for line in open(path):
            if line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) == 2:
                name = parts[0].split("{")[0]
                try:
                    vals[name] = vals.get(name, 0.0) + float(parts[1])
                except ValueError:
                    pass
    except FileNotFoundError:
        pass
    return vals
tot = {c: 0.0 for c in COUNTERS}
v0_proc = blocks = 0.0
mempool_after = 0.0
for port in ["9091", "9092", "9093", "9094"]:
    b = read(os.path.join(d, f"metrics-{port}-before.txt"))
    a = read(os.path.join(d, f"metrics-{port}-after.txt"))
    for c in COUNTERS:
        tot[c] += a.get(c, 0.0) - b.get(c, 0.0)
    mempool_after += a.get("torus_mempool_native_size", 0.0)
    if port == "9091":
        v0_proc = a.get("torus_native_actions_processed", 0.0) - b.get("torus_native_actions_processed", 0.0)
        blocks = a.get("torus_blocks_committed", 0.0) - b.get("torus_blocks_committed", 0.0)
print(",".join(str(int(tot[c])) for c in COUNTERS)
      + f",{int(v0_proc)},{int(blocks)},{int(mempool_after)}")
PYEOF
}

mkdir -p "$OUTROOT"
if [ "${APPEND:-0}" != 1 ]; then
    echo "trial,gossip,phase,rate,bs,orders_s,block_ms_fit,wedged,gsp_published,gsp_drop_full,gsp_drop_over,gsp_received,fwd_admits,direct_send_fail,da_pulls,da_recovered,da_pull_fail,da_handoffs,da_timeouts,v0_actions_processed,v0_blocks,mempool_native_after" > "$CSV"
fi

run_trial() { # $1=trial# $2=true|false(gossip) $3=rate $4=clean|flood
    local n=$1 gossip=$2 rate=$3 phase=$4
    local dir="$OUTROOT/trial-$n-gossip$gossip-$phase"
    mkdir -p "$dir"
    echo "=== trial $n: native-gossip=$gossip phase=$phase rate=$rate bs=$BS ==="
    docker compose down -v >/dev/null 2>&1 || true
    TORUS_NATIVE_GOSSIP=$gossip TORUS_PUSH_THRESHOLD=$THRESHOLD \
        docker compose up -d --build > "$dir/compose-up.log" 2>&1

    # Preflight (first trial pays it, cached after): image binary must accept
    # --native-gossip=false — pre-fix binaries silently run the WRONG ARM.
    docker run --rm --entrypoint torus-node torus-devnet-node:local \
        --native-gossip=false --help >/dev/null 2>&1 \
        || { echo "FATAL: image torus-node rejects --native-gossip=false (pre-S458 binary?)" >&2; exit 1; }

    local up=0 h
    for _ in $(seq 1 30); do h=$(height); [ "$h" -gt 2 ] && { up=1; break; }; sleep 2; done
    if [ "$up" -ne 1 ]; then
        echo "trial $n (gossip=$gossip/$phase): chain never started" >&2
        docker compose logs --no-color > "$dir/compose-all.log" 2>&1 || true
        echo "$n,$gossip,$phase,$rate,$BS,0,0,1,$NA_FRAG" >> "$CSV"
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

    docker run --rm --network host --entrypoint /bench \
        -v "$REPO/target/release/bench-throughput:/bench:ro" \
        torus-devnet-node:local \
        consensus --rpc-urls "$RPCS" --batch-size "$BS" --senders "$SENDERS" \
        --duration "$DURATION" --rate "$rate" --pre-sign "$((DURATION * rate))" \
        --sign-mode "$SIGN" --markets "$MARKETS" \
        --format bin 2>&1 | tee "$dir/bench.txt" || true

    wait "$sampler" || true
    snap_metrics "$dir" after
    h1=$(height)

    # node logs BEFORE teardown (S419 lesson — teardown destroys them)
    local svc
    for svc in validator-0 validator-1 validator-2 validator-3 rpc-node; do
        docker compose logs --no-color "$svc" > "$dir/log-$svc.txt" 2>&1 || true
    done

    local orders_s block_ms wedged frag
    orders_s=$(grep -oE 'Sustained:\s+[0-9]+' "$dir/bench.txt" | grep -oE '[0-9]+' | tail -1 || true)
    block_ms=$(python3 "$PY" fit "$dir/heights.tsv" || echo 0)
    wedged=0
    [ $((h1 - h0)) -lt 5 ] && wedged=1
    frag=$(d2l_deltas "$dir" || echo "$NA_FRAG")
    echo "$n,$gossip,$phase,$rate,$BS,${orders_s:-0},${block_ms:-0},$wedged,$frag" >> "$CSV"
    tail -1 "$CSV"
    docker compose down -v >/dev/null 2>&1 || true
}

# n:gossip:rate:phase — overridable (e.g. APPEND=1 TRIALS="9:false:15:flood")
TRIALS=${TRIALS:-"1:true:3:clean 2:false:3:clean 3:true:3:clean 4:false:3:clean 5:true:15:flood 6:false:15:flood 7:true:15:flood 8:false:15:flood"}
for t in $TRIALS; do
    n=${t%%:*}; rest=${t#*:}
    gossip=${rest%%:*}; rest=${rest#*:}
    rate=${rest%%:*}; phase=${rest##*:}
    run_trial "$n" "$gossip" "$rate" "$phase"
done

echo
echo "=== FINAL $CSV ==="
cat "$CSV"
