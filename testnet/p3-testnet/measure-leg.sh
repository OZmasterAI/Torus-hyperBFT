#!/usr/bin/env bash
# ===========================================================================
# p3 TESTNET throughput leg — counter-scrape measurement (read-only)
# ===========================================================================
# Port of the devnet mission harness (scratchpad/mission/harness/sustained-leg.sh)
# to a LIVE testnet validator. The devnet harness spun up a 4-val docker-compose
# devnet and tore it down; on testnet the chain is already running on separate
# machines, so this script does NONE of that. It is strictly read-only:
#
#   * it NEVER starts/stops/wipes a node or touches systemd,
#   * it only curls the local node's RPC + /metrics and (optionally) fires a bench.
#
# HEADLINE METRIC is node-counter-sourced, exactly like the mission:
#   Δ torus_orders_placed, Δ torus_orders_matched, Δ torus_native_actions_processed
# The bench tool's own "orders/s" is DELIBERATELY NOT used (that was the
# mis-measurement the p3 mission corrected). placed/matched/executed are
# consensus-deterministic, so measuring THIS node measures the whole chain.
#
# ---------------------------------------------------------------------------
# Usage:
#   ./measure-leg.sh <label> <results-dir> [flow]
#     <label>        name for this leg's output subdir (e.g. p3-c18-cross-1)
#     <results-dir>  parent dir for results (created if missing)
#     [flow]         rest|cross|churn   (default cross — the matching-proof shape)
#
# Two run modes (RUN_BENCH):
#   RUN_BENCH=1 (default) — this box BOTH fires a local bench AND measures.
#                           Good for a single-box smoke test on one validator.
#   RUN_BENCH=0           — measure only (no local load). Use on the COORDINATOR
#                           while the other operators fire bench-our/val1/friend2.
#                           Because the counters are chain-global, ONE measuring
#                           node captures the aggregate — do NOT sum across boxes.
#
# Env knobs (defaults tuned for c18's testnet unit; override per node):
#   RPC          local node RPC        (default http://127.0.0.1:8555)   # seed: :8545
#   METRICS      local node /metrics   (default http://127.0.0.1:9090)
#   BENCH_BIN    bench-throughput path (default <repo>/target/release/bench-throughput)
#   DURATION     bench seconds         (default 120)
#   SUBWIN       peak sliding-window s (default 60)  # peak placed/s over any SUBWIN
#   MARKETS      spread orders 1..=N   (default 10)  # anchored genesis registers 10
#   SENDERS      distinct senders      (default 20)  # must be funded in genesis
#   SENDER_OFFSET first sender index   (default 0)   # disjoint ranges per box: 0/20/40
#   BATCH        orders per PlaceOrderBatch (default 100)  # b100 = the clean-books shape
#   RATE         actions/s per sender  (default 60)
#   SUBMIT_BATCH actions per RPC call  (default 15)  # server cap 100
#   PRESIGN      pre-signed batches/sender (default 60)
#   SIGN_MODE    eip712|session        (default session)  # ed25519 fast path
#   FORMAT       json|bin              (default bin)
#
# Requires: bash, curl, python3, jq(optional). No sudo, no docker.
# ===========================================================================
set -euo pipefail

# --------------------------- args ------------------------------------------
if [ $# -lt 2 ]; then
    grep -E '^# (Usage|  )' "$0" | sed 's/^# //'
    exit 2
fi
LABEL=$1
RESULTS_DIR=$2
FLOW=${3:-cross}
# `match` accepted since the p3 devnet fill ladder: the bench has supported
# --flow match / --taker-ratio since a616073, but this wrapper never plumbed it
# through, which is why the testnet match legs had to bypass the harness and
# fire bench-throughput by hand (losing summary.json / sampler.csv).
case "$FLOW" in rest|cross|churn|match) ;; *) echo "FATAL: flow must be rest|cross|churn|match (got '$FLOW')" >&2; exit 2 ;; esac

HERE=$(cd "$(dirname "$0")" && pwd)
# Overridable so a copy of this harness can drive legs from outside the repo
# (BENCH_BIN and the git provenance in leg-config.json resolve against it).
REPO=${REPO:-$(cd "$HERE/../.." && pwd)}

# --------------------------- config ----------------------------------------
RPC=${RPC:-http://127.0.0.1:8555}
METRICS=${METRICS:-http://127.0.0.1:9090}
BENCH_BIN=${BENCH_BIN:-$REPO/target/release/bench-throughput}
RUN_BENCH=${RUN_BENCH:-1}

DURATION=${DURATION:-120}
SUBWIN=${SUBWIN:-60}
MARKETS=${MARKETS:-10}
SENDERS=${SENDERS:-20}
SENDER_OFFSET=${SENDER_OFFSET:-0}
BATCH=${BATCH:-100}
RATE=${RATE:-60}
SUBMIT_BATCH=${SUBMIT_BATCH:-15}
PRESIGN=${PRESIGN:-60}
SIGN_MODE=${SIGN_MODE:-session}
FORMAT=${FORMAT:-bin}
# `--flow match` only: fraction of senders acting as TAKERS. The bench ignores
# it for every other flow, so it is always safe to pass.
TAKER_RATIO=${TAKER_RATIO:-0.5}

LEG_DIR="$RESULTS_DIR/$LABEL"
mkdir -p "$LEG_DIR"
STOP_FLAG="$LEG_DIR/.sampler-stop"
rm -f "$STOP_FLAG"

log() { printf '[%s] [%s] %s\n' "$(date -u +%H:%M:%SZ)" "$LABEL" "$*"; }
die() { log "FATAL: $*"; exit 1; }

# --------------------------- helpers ---------------------------------------
rpc_call() {
    curl -m 5 -s -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" "$RPC" 2>/dev/null
}
block_height() {
    local r; r=$(rpc_call eth_blockNumber "[]") || return 1
    python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))" <<<"$r" 2>/dev/null
}
snap_counters() {  # $1 = tag (before|after) -> writes raw + parsed json, echoes json path
    local raw="$LEG_DIR/counters-$1-raw.txt" jsn="$LEG_DIR/counters-$1.json"
    curl -m 5 -s "$METRICS" > "$raw" || die "cannot scrape $METRICS"
    python3 "$HERE/scrape.py" "$raw" > "$jsn" || die "scrape.py failed on $raw"
    echo "$jsn"
}

# --------------------------- preflight -------------------------------------
log "preflight: RPC=$RPC METRICS=$METRICS flow=$FLOW run_bench=$RUN_BENCH"
[ "$RUN_BENCH" -eq 0 ] || [ -x "$BENCH_BIN" ] || die "bench binary missing/not executable: $BENCH_BIN"

H0=$(block_height) || die "RPC not answering eth_blockNumber at $RPC (is the node up?)"
curl -m 5 -s "$METRICS" | grep -q "torus_orders_placed" \
    || die "$METRICS has no torus_orders_placed (wrong port, or old binary without p3 counters?)"
sleep 4
H1=$(block_height) || die "RPC stopped answering"
if [ "$H1" -le "$H0" ]; then
    log "WARN: block height did not advance in 4s ($H0 -> $H1) — chain may be idle/stalled; measuring anyway"
else
    log "chain live: height $H0 -> $H1 (+$((H1 - H0)) in ~4s)"
fi

# --------------------------- 1s sampler (bg) --------------------------------
# Reuses scrape.py's exact folding so the time series matches before/after.
SAMPLER_CSV="$LEG_DIR/sampler.csv"
METRICS="$METRICS" STOP_FLAG="$STOP_FLAG" OUT="$SAMPLER_CSV" HERE="$HERE" \
python3 - <<'PY' &
import os, sys, time, urllib.request, tempfile
sys.path.insert(0, os.environ["HERE"])
from scrape import parse
url, stop, out = os.environ["METRICS"], os.environ["STOP_FLAG"], os.environ["OUT"]
cols = ["ts","orders_placed","orders_matched","native_actions_processed",
        "blocks_committed","mempool_native_size"]
with open(out, "w") as f:
    f.write(",".join(cols) + "\n")
    while not os.path.exists(stop):
        t = int(time.time())
        try:
            body = urllib.request.urlopen(url, timeout=3).read().decode("utf-8", "replace")
            tf = tempfile.NamedTemporaryFile("w", delete=False, suffix=".txt")
            tf.write(body); tf.close()
            p = parse(tf.name); os.unlink(tf.name)
            c, g = p["counters"], p["gauges"]
            row = [t, c["torus_orders_placed"], c["torus_orders_matched"],
                   c["torus_native_actions_processed"], c["torus_blocks_committed"],
                   g.get("torus_mempool_native_size")]
        except Exception:
            row = [t, "", "", "", "", ""]
        f.write(",".join(str(x) for x in row) + "\n"); f.flush()
        time.sleep(1)
PY
SAMPLER_PID=$!
cleanup() { touch "$STOP_FLAG" 2>/dev/null || true; wait "$SAMPLER_PID" 2>/dev/null || true; }
trap cleanup EXIT

# --------------------------- measure window --------------------------------
BEFORE=$(snap_counters before)
T0=$(date +%s)
log "window START t=$T0 height=$H0"

if [ "$RUN_BENCH" -eq 1 ]; then
    log "firing bench: senders=$SENDERS offset=$SENDER_OFFSET markets=$MARKETS flow=$FLOW taker=$TAKER_RATIO b=$BATCH r=$RATE dur=$DURATION"
    timeout $((DURATION + 180)) "$BENCH_BIN" consensus \
        --rpc-urls "$RPC" \
        --senders "$SENDERS" --sender-offset "$SENDER_OFFSET" \
        --duration "$DURATION" --batch-size "$BATCH" \
        --submit-batch "$SUBMIT_BATCH" --rate "$RATE" --pre-sign "$PRESIGN" \
        --sign-mode "$SIGN_MODE" --format "$FORMAT" \
        --markets "$MARKETS" --flow "$FLOW" --taker-ratio "$TAKER_RATIO" \
        > "$LEG_DIR/bench.log" 2>&1 \
        || log "WARN: bench exited non-zero (see bench.log) — measuring window regardless"
else
    log "measure-only: sleeping DURATION=$DURATION while other operators generate load"
    sleep "$DURATION"
fi

T1=$(date +%s)
H2=$(block_height || echo "$H1")
AFTER=$(snap_counters after)
log "window END   t=$T1 height=$H2  (wall=$((T1 - T0))s)"
cleanup; trap - EXIT

# --------------------------- summary ---------------------------------------
python3 - "$BEFORE" "$AFTER" "$SAMPLER_CSV" "$T0" "$T1" "$H0" "$H2" "$SUBWIN" "$LEG_DIR/summary.json" <<'PY'
import sys, json
before, after, csvp, t0, t1, h0, h2, subwin, outp = sys.argv[1:10]
t0, t1, subwin = int(t0), int(t1), int(subwin)
h0, h2 = int(h0), int(h2)
b = json.load(open(before))["counters"]; a = json.load(open(after))["counters"]
win = max(1, t1 - t0)

def d(k): return a[k] - b[k]
placed, matched, execd, blocks = (d("torus_orders_placed"), d("torus_orders_matched"),
                                  d("torus_native_actions_processed"), d("torus_blocks_committed"))

# peak over any SUBWIN-second sliding window, from the 1s sampler
def peak(col_idx):
    rows = []
    for line in open(csvp).read().splitlines()[1:]:
        p = line.split(",")
        try: rows.append((int(p[0]), float(p[col_idx])))
        except (ValueError, IndexError): pass
    best = 0.0
    for i, (ts_i, v_i) in enumerate(rows):
        for ts_j, v_j in rows[i+1:]:
            span = ts_j - ts_i
            if span <= 0: continue
            if span > subwin: break
            if span >= subwin * 0.8:            # ~full window
                best = max(best, (v_j - v_i) / span)
    return best

peak_placed = peak(1); peak_matched = peak(2)
summary = {
    "window_s": win, "height_delta": h2 - h0, "blocks_committed_delta": blocks,
    "blk_per_s": round(blocks / win, 2),
    "sustained": {
        "orders_placed_per_s": round(placed / win, 1),
        "orders_matched_per_s": round(matched / win, 1),
        "executed_actions_per_s": round(execd / win, 1),
        "matched_per_executed_order_pct": round(100 * matched / placed, 3) if placed else None,
    },
    "peak_over_%ds_window" % subwin: {
        "orders_placed_per_s": round(peak_placed, 1),
        "orders_matched_per_s": round(peak_matched, 1),
    },
    "totals": {"placed": placed, "matched": matched, "executed_actions": execd},
}
json.dump(summary, open(outp, "w"), indent=2)
print("\n================= LEG SUMMARY (node-counter-sourced) =================")
print(json.dumps(summary, indent=2))
print("=====================================================================")
print("NOTE: placed/matched/executed are CHAIN-GLOBAL (consensus-deterministic).")
print("      In a multi-box run, ONE measuring node = the aggregate. Do NOT sum.")
PY

# Provenance: a ladder cell is only comparable to another if the offered load
# shape matches, so record it NEXT TO the numbers rather than trusting the label.
# LEG_IMAGE (set by run-leg.sh) is authoritative for which BRANCH ran: the git
# checkout is NOT, because legs run pre-built per-branch images and the working
# tree may sit on a different branch entirely.
cat > "$LEG_DIR/leg-config.json" <<EOF
{
  "label": "$LABEL", "flow": "$FLOW", "taker_ratio": $TAKER_RATIO,
  "duration_s": $DURATION, "subwin_s": $SUBWIN,
  "markets": $MARKETS, "senders": $SENDERS, "sender_offset": $SENDER_OFFSET,
  "batch": $BATCH, "rate": $RATE, "submit_batch": $SUBMIT_BATCH,
  "pre_sign": $PRESIGN, "sign_mode": "$SIGN_MODE", "format": "$FORMAT",
  "rpc": "$RPC", "metrics": "$METRICS", "run_bench": $RUN_BENCH,
  "leg_image": "${LEG_IMAGE:-unknown}",
  "caps": {
    "total_block": "${TORUS_NATIVE_TOTAL_BLOCK_CAP:-compose-default}",
    "orders_per_block": "${TORUS_NATIVE_ORDERS_PER_BLOCK_CAP:-compose-default}",
    "block_bytes": "${TORUS_NATIVE_BLOCK_BYTES_CAP:-compose-default}"
  },
  "git_branch_at_runtime": "$(git -C "$REPO" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)",
  "git_commit_at_runtime": "$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"
}
EOF

log "done -> $LEG_DIR/ (summary.json, leg-config.json, sampler.csv, counters-{before,after}.json, bench.log)"
