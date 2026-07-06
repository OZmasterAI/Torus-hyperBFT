#!/usr/bin/env bash
# O2 Task 9 — PlaceOrderBatch batch-size sweep (devnet mechanics proof, S416).
#
# Four legs vary batch-size (orders per PlaceOrderBatch) with rate scaled per
# leg so submitted orders/s (SENDERS*rate*bs) meets/exceeds the exec ceiling for
# bs>=100 (chain-bound legs), while bs=1 shows the single-action ceiling — the
# action-cap-bound O2 baseline the batch win is measured against:
#   bs=1    rate=100  ->  2,000 orders/s  (single-action cap ceiling, ~250/s@cap100)
#   bs=100  rate=10   -> 20,000 orders/s  (exec-bound)
#   bs=400  rate=3    -> 24,000 orders/s  (exec-bound; S415 measured 11.4k sustained)
#   bs=1024 rate=2    -> 40,960 orders/s  (exec-bound, at-cap batch)
#
# THRESHOLD (S419 fix): docker-compose.yml defaults TORUS_PUSH_THRESHOLD to
# 6000000 (testnet mirror, clamped to 4MB) when unset — "no export" does NOT
# mean the compiled 512KB default. S419 run under that 4MB fallback reproduced
# the S415 fullpush collapse (bs400: 448 orders/s, failed body pulls, empty
# blocks). Export the S415-proven 512KB control config explicitly:
export TORUS_PUSH_THRESHOLD=${TORUS_PUSH_THRESHOLD:-524288}
#
# Bench signs with --sign-mode session, registering SessionScope::Full sessions
# (main.rs:603) — the sweep does NOT depend on Task 5 (Trading-scope extension);
# devnet rebuilds the whole fleet from this branch, so no lockstep concern inside
# the sweep.
#
# Acceptance: all four legs wedged=0; orders/s strictly increasing bs=1 -> bs=400;
# report the orders/s + block_ms_fit table in the roadmap entry. Block time comes
# from a least-squares fit over sampled heights — never endpoint-to-endpoint
# (S405 lesson).
#
# Prereqs (BUILT ON THE LAPTOP, rsynced back — do not build on this VPS):
#   lap build   # workspace release
#   lap sh 'true' >/dev/null   # (tunnel check)
#   rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/target/release/torus-node       target/release/
#   rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/target/release/bench-throughput target/release/
#
# Policy: devnet UP only while testing — this script tears it down when done.
# Syntax gate (execution deferred by policy):  bash -n devnet/sweep-o2-batchsize-s416.sh
set -euo pipefail
cd "$(dirname "$0")"

# Policy guard: tear the devnet down on ANY exit (success, leg failure, or
# signal) — same compose invocation as the per-leg teardown below.
trap 'docker compose down -v >/dev/null 2>&1 || true' EXIT

BIN=../target/release/torus-node
BENCH=../target/release/bench-throughput
[ -x "$BIN" ] || { echo "missing $BIN — lap build on laptop, rsync back (see header)"; exit 1; }
[ -x "$BENCH" ] || { echo "missing $BENCH — lap build on laptop, rsync back (see header)"; exit 1; }

DURATION=${DURATION:-50}   # pre-signed nonces expire 60s after signing (bench
                           # nonce window) — the whole paced burn must fit inside
                           # it, so keep DURATION under ~55s when pre-signing
SENDERS=${SENDERS:-20}     # genesis-funded market-maker accounts (bench cap)
SIGN=${SIGN:-session}      # ed25519 session keys, SessionScope::Full
MARKETS=${MARKETS:-4}
OUT=${OUT:-sweep-o2-batchsize-s416.csv}
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
        # curl guarded like height(): a down port must not trip pipefail —
        # awk's END block turns the empty input into 0.
        v=$({ curl -s -m 2 "http://localhost:$p/metrics" || true; } \
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

echo "bs,rate,orders_s,block_ms_fit,avg_native_per_block,pull_delta,wedged" > "$OUT"

# bs:rate — submitted orders/s = 20*rate*bs: 2,000 / 20,000 / 24,000 / 40,960.
# bs=1@100 exposes the single-action cap ceiling (~250 actions/s at cap 100);
# bs>=100 legs are exec-bound (S415 measured 11.4k sustained at bs400).
LEGS=${LEGS:-"1:100 100:10 400:3 1024:2"}
for leg in $LEGS; do
    bs=${leg%%:*}; rate=${leg##*:}
    echo "=== leg bs=$bs rate=$rate ==="
    docker compose down -v >/dev/null 2>&1 || true
    docker compose up -d --build
    # wait for the chain to move before measuring
    up=0
    for _ in $(seq 1 30); do h=$(height); [ "$h" -gt 2 ] && { up=1; break; }; sleep 2; done
    if [ "$up" -ne 1 ]; then
        echo "leg bs=$bs: chain never started" >&2
        echo "$bs,$rate,0,0,0,0,1" >> "$OUT"
        docker compose down -v >/dev/null 2>&1 || true
        continue
    fi

    p0=$(pulls); h0=$(height)
    samples="sweep-o2-bs$bs-heights.tsv"; : > "$samples"
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
        consensus --rpc-urls "$RPCS" --batch-size "$bs" --senders "$SENDERS" \
        --duration "$DURATION" --rate "$rate" --pre-sign "$((DURATION * rate))" \
        --sign-mode "$SIGN" --markets "$MARKETS" \
        --format bin 2>&1 | tee "bench-o2-bs$bs.txt" || true
    # ^ || true: a crashed bench must not abort the sweep (errexit+pipefail) —
    #   the leg still records a fallback row below and the loop continues.

    wait "$sampler" || true
    p1=$(pulls); h1=$(height)

    # || true: absent 'Sustained:'/metric lines leave the var empty and the
    # ${var:-0} fallbacks below record the sentinel row instead of aborting.
    orders_s=$(grep -oE 'Sustained:\s+[0-9]+' "bench-o2-bs$bs.txt" | grep -oE '[0-9]+' | tail -1 || true)
    avg_native=$(grep -oE '[0-9]+ avg native/block' "bench-o2-bs$bs.txt" | grep -oE '^[0-9]+' | tail -1 || true)
    block_ms=$(fit_block_ms "$samples")
    wedged=0; [ $((h1 - h0)) -lt 5 ] && wedged=1
    echo "$bs,$rate,${orders_s:-0},${block_ms:-0},${avg_native:-0},$((p1 - p0)),$wedged" >> "$OUT"
    docker compose down -v >/dev/null 2>&1 || true
done

echo; echo "=== $OUT ==="; cat "$OUT"
