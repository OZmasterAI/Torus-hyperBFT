#!/usr/bin/env bash
# O5 Task 6 — bs400 push-threshold sweep (devnet mechanics proof, S415).
#
# Two legs (the threshold is BINARY at a given body size — a mid leg behaves
# identically to control, see docs/plans/o5-feed-gates-impl.md Task 6):
#   control  : TORUS_PUSH_THRESHOLD=524288  -> every block ships a hash
#              manifest; all validators PULL the bodies (chunked native-DA)
#   fullpush : TORUS_PUSH_THRESHOLD=3145728 -> bodies go as ONE direct push
#              (3 MB < 4 MB fleet floor, so valid against ANY fleet build)
#
# Acceptance (tasks.json #6): fullpush leg has pull_delta < 10 AND wedged=0,
# with block_ms_fit <= control. Block time comes from a least-squares fit over
# sampled heights — never endpoint-to-endpoint (S405 lesson).
#
# Prereqs (BUILT ON THE LAPTOP, rsynced back — do not build on this VPS):
#   lap build   # workspace release
#   lap sh 'true' >/dev/null   # (tunnel check)
#   rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/target/release/torus-node       target/release/
#   rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/target/release/bench-throughput target/release/
#
# Policy: devnet UP only while testing — this script tears it down when done.
set -euo pipefail
cd "$(dirname "$0")"

BIN=../target/release/torus-node
BENCH=../target/release/bench-throughput
[ -x "$BIN" ] || { echo "missing $BIN — lap build on laptop, rsync back (see header)"; exit 1; }
[ -x "$BENCH" ] || { echo "missing $BENCH — lap build on laptop, rsync back (see header)"; exit 1; }

DURATION=${DURATION:-50}   # pre-signed nonces expire 60s after signing (bench
                           # nonce window) — the whole paced burn must fit inside
                           # it, so keep DURATION under ~55s when PRESIGN is on
BS=${BS:-400}              # orders per PlaceOrderBatch (bodies ~28KB/action)
SENDERS=${SENDERS:-20}     # genesis-funded market-maker accounts (bench cap)
RATE=${RATE:-3}            # actions/s/sender, paced (s367: burst under-measures).
                           # 20x3=60 actions/s -> ~20-30 actions/block: bodies
                           # 560-840KB, BETWEEN the legs' thresholds by design.
                           # First run at RATE=6 CPU-wedged the box (8 cores,
                           # load 19.6): 4 validators x 17k-order exec + bench
                           # signing starved push AND pull into timeouts.
SIGN=${SIGN:-session}      # ed25519 session keys
PRESIGN=${PRESIGN:-$((DURATION * RATE))}  # ammo/sender signed BEFORE the window
                           # so the bench is CPU-idle while we measure the chain
MARKETS=${MARKETS:-4}
OUT=${OUT:-sweep-o5-s415.csv}
RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548"
V0_RPC=http://localhost:8645
METRICS_PORTS="9091 9092 9093 9094"

height() {
    curl -s -m 2 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$V0_RPC" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))' \
        2>/dev/null || echo -1
}

pulls() { # sum of torus_native_da_pull_requests across the 4 validators
    local total=0 v p
    for p in $METRICS_PORTS; do
        v=$(curl -s -m 2 "http://localhost:$p/metrics" \
            | awk '$1 ~ /^torus_native_da_pull_requests(_total)?$/ {print int($2); found=1; exit} END {if(!found) print 0}')
        total=$((total + v))
    done
    echo "$total"
}

fit_block_ms() { # least-squares blocks/s over (t,height) samples -> ms/block
    python3 - "$1" <<'EOF'
import sys
pts = []
for line in open(sys.argv[1]):
    f = line.split()
    if len(f) == 2 and int(f[1]) >= 0:
        pts.append((float(f[0]), int(f[1])))
if len(pts) < 3:
    print(0); raise SystemExit
mt = sum(t for t, _ in pts) / len(pts)
mh = sum(h for _, h in pts) / len(pts)
num = sum((t - mt) * (h - mh) for t, h in pts)
den = sum((t - mt) ** 2 for t, _ in pts)
bps = num / den if den else 0.0
print(round(1000.0 / bps, 1) if bps > 0 else 0)
EOF
}

echo "leg,threshold,orders_s,block_ms_fit,avg_native_per_block,pull_delta,wedged" > "$OUT"

LEGS=${LEGS:-"control:524288 fullpush:3145728"}  # override to reverse order (A/B/A confound check)
for leg in $LEGS; do
    name=${leg%%:*}; thr=${leg##*:}
    echo "=== leg $name  TORUS_PUSH_THRESHOLD=$thr ==="
    export TORUS_PUSH_THRESHOLD=$thr
    docker compose down -v >/dev/null 2>&1 || true
    docker compose up -d --build
    # wait for the chain to move before measuring
    up=0
    for _ in $(seq 1 30); do h=$(height); [ "$h" -gt 2 ] && { up=1; break; }; sleep 2; done
    if [ "$up" -ne 1 ]; then
        echo "leg $name: chain never started" >&2
        echo "$name,$thr,0,0,0,0,1" >> "$OUT"
        docker compose down -v >/dev/null 2>&1 || true
        continue
    fi

    p0=$(pulls); h0=$(height)
    samples="sweep-o5-$name-heights.tsv"; : > "$samples"
    (
        end=$((SECONDS + DURATION + 10))
        while [ $SECONDS -lt $end ]; do
            printf '%s\t%s\n' "$(date +%s.%N)" "$(height)" >> "$samples"
            sleep 2
        done
    ) &
    sampler=$!

    # Bench runs INSIDE the devnet image (host network): the binary is built on
    # the ThinkPad (glibc 2.43) and cannot run on this 24.04 host (glibc 2.39).
    docker run --rm --network host --entrypoint /bench \
        -v "$(cd .. && pwd)/target/release/bench-throughput:/bench:ro" \
        torus-devnet-node:local \
        consensus --rpc-urls "$RPCS" --batch-size "$BS" --senders "$SENDERS" \
        --duration "$DURATION" --rate "$RATE" --pre-sign "$PRESIGN" \
        --sign-mode "$SIGN" --markets "$MARKETS" \
        --format bin 2>&1 | tee "bench-o5-$name.txt"

    wait "$sampler" || true
    p1=$(pulls); h1=$(height)

    orders_s=$(grep -oE 'Sustained:\s+[0-9]+' "bench-o5-$name.txt" | grep -oE '[0-9]+' | tail -1)
    avg_native=$(grep -oE '[0-9]+ avg native/block' "bench-o5-$name.txt" | grep -oE '^[0-9]+' | tail -1)
    block_ms=$(fit_block_ms "$samples")
    wedged=0; [ $((h1 - h0)) -lt 5 ] && wedged=1
    echo "$name,$thr,${orders_s:-0},${block_ms:-0},${avg_native:-0},$((p1 - p0)),$wedged" >> "$OUT"
    docker compose down -v >/dev/null 2>&1 || true
done

unset TORUS_PUSH_THRESHOLD
echo; echo "=== $OUT ==="; cat "$OUT"
