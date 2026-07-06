#!/usr/bin/env bash
# S419 instrumented repro of the O2 bs=400 inclusion collapse.
# Single leg, devnet kept up until docker logs + metrics are captured
# (the sweep script's teardown destroys node logs — this one saves them first).
set -euo pipefail
cd "$(dirname "$0")"

OUTDIR=o2-collapse-repro-s419
mkdir -p "$OUTDIR"
trap 'docker compose down -v >/dev/null 2>&1 || true' EXIT

BIN=../target/release/torus-node
BENCH=../target/release/bench-throughput
[ -x "$BIN" ] && [ -x "$BENCH" ] || { echo "missing binaries"; exit 1; }

BS=${BS:-400}
RATE=${RATE:-3}
DURATION=${DURATION:-50}
SENDERS=20
MARKETS=4
RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548"
V0_RPC=http://localhost:8645
METRICS_PORTS="9091 9092 9093 9094"

height() {
    curl -s -m 2 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$V0_RPC" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))' \
        2>/dev/null || echo -1
}

snap_metrics() { # $1 = label; grab native/mempool/da/pull-ish counters from all validators
    local p
    for p in $METRICS_PORTS; do
        { curl -s -m 2 "http://localhost:$p/metrics" || true; } \
            | grep -Ei 'native|pull|mempool|defer|batch|body|reconstruct' \
            > "$OUTDIR/metrics-$p-$1.txt" || true
    done
}

docker compose down -v >/dev/null 2>&1 || true
docker compose up -d --build

up=0
for _ in $(seq 1 30); do h=$(height); [ "$h" -gt 2 ] && { up=1; break; }; sleep 2; done
[ "$up" -eq 1 ] || { echo "chain never started"; exit 1; }

h0=$(height)
echo "burn starts at height $h0"
snap_metrics before

# per-2s height+timestamp samples (same fit input as the sweep)
: > "$OUTDIR/heights.tsv"
(
    end=$((SECONDS + DURATION + 15))
    while [ $SECONDS -lt $end ]; do
        printf '%s\t%s\n' "$(date +%s.%N)" "$(height)" >> "$OUTDIR/heights.tsv"
        sleep 2
    done
) &
sampler=$!

docker run --rm --network host --entrypoint /bench \
    -v "$(cd .. && pwd)/target/release/bench-throughput:/bench:ro" \
    torus-devnet-node:local \
    consensus --rpc-urls "$RPCS" --batch-size "$BS" --senders "$SENDERS" \
    --duration "$DURATION" --rate "$RATE" --pre-sign "$((DURATION * RATE))" \
    --sign-mode session --markets "$MARKETS" \
    --format bin 2>&1 | tee "$OUTDIR/bench.txt" || true

wait "$sampler" || true
snap_metrics after
h1=$(height)
echo "burn ended at height $h1"

# capture node logs BEFORE teardown — the whole point of this script
for svc in validator-0 validator-1 validator-2 validator-3 rpc-node; do
    docker compose logs --no-color "$svc" > "$OUTDIR/log-$svc.txt" 2>&1 || true
done

# per-block native-action counts over the burn range via RPC (body presence map)
python3 - "$V0_RPC" "$h0" "$h1" > "$OUTDIR/blocks.tsv" 2>/dev/null <<'EOF' || true
import sys, json, urllib.request
rpc, h0, h1 = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
for h in range(max(0, h0 - 5), h1 + 1):
    req = urllib.request.Request(rpc, data=json.dumps({
        "jsonrpc": "2.0", "method": "eth_getBlockByNumber",
        "params": [hex(h), False], "id": 1}).encode(),
        headers={"Content-Type": "application/json"})
    try:
        b = json.load(urllib.request.urlopen(req, timeout=3))["result"]
        txs = len(b.get("transactions", [])) if b else -1
        print(f"{h}\t{txs}")
    except Exception as e:
        print(f"{h}\tERR")
EOF

echo "REPRO-DONE h0=$h0 h1=$h1 outdir=$OUTDIR"
