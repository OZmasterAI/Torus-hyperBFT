#!/usr/bin/env bash
# bench-native-orders-grid.sh — full-cartesian NATIVE orders/s sweep vs the LIVE seed (10-market genesis).
#
# Walks the cartesian product of {markets} x {batch} x {senders} x {sign-mode}, one
# `bench-throughput consensus` leg per cell, records orders/s (tool) + node-side
# actions/s (Prometheus) to a CSV, with a between-cell cooldown and a block-rate
# HEALTH GUARD that AUTO-ABORTS the whole sweep if the live chain wedges/drops below
# a floor. Resumable: cells already in the CSV are skipped.
#
# Axes (env-overridable lists):   MARKETS_LIST  BATCH_LIST  SENDERS_LIST  SIGN_LIST
# Knobs:  RPC METRICS SENDER_OFFSET DUR RATE SUBMIT COOLDOWN FLOOR_BPS OUTDIR BIN
#
#   full grid:      ./testnet/bench-native-orders-grid.sh
#   quick subset:   MARKETS_LIST="1 10" BATCH_LIST="400 1000" SENDERS_LIST="20 40" SIGN_LIST="session" \
#                   SENDER_OFFSET=20 RPC=http://127.0.0.1:8555 METRICS=http://127.0.0.1:9090 \
#                   ./testnet/bench-native-orders-grid.sh   # ~4 min, proves node_actions_s/blk_s non-zero
#
# SENDER_OFFSET (default 20): bench senders occupy indices SENDER_OFFSET..SENDER_OFFSET+senders-1.
#   The live genesis funds native bench senders indices 0..59 (index 0 is the market-maker).
#   Default 20 is the conservative methodology default (avoids draining the market-maker and
#   matches prior 3-box runs where our=0..19 / val1=20..39 / friend2=40..59). With offset 20 the
#   funded window caps senders at 40 (20+40=60, last index 59) — hence the default SENDERS_LIST
#   tops out at 40. To sweep 60 senders per cell set SENDER_OFFSET=0. A funded-range GUARD below
#   aborts loudly if SENDER_OFFSET + max(SENDERS_LIST) > 60.
#
# PRECHECKS (fail loud, never silent-zero): before the sweep this script verifies the RPC and
#   METRICS endpoints are reachable and serving the expected data (a wrong METRICS silently
#   zeroes the node_actions_s/blk_s columns), and that the first + last sender in the range are
#   actually funded (via torus_getBalances, with a 5s smoke-cell fallback).
#
# SAFETY: this offers real load to the running validator. Block time WILL rise under
# load (baseline ~12.7 blk/s idle). The guard aborts if it drops below FLOOR_BPS.
set -u
cd "$(dirname "$0")/.." || { echo "cannot cd to repo root"; exit 1; }

BIN="${BIN:-./target/release/bench-throughput}"
RPC="${RPC:-http://localhost:8545}"
METRICS="${METRICS:-http://127.0.0.1:9090}"
SENDER_OFFSET="${SENDER_OFFSET:-20}"   # first sender index; funded genesis window is 0..59
MARKETS_LIST="${MARKETS_LIST:-1 2 3 5 10}"
BATCH_LIST="${BATCH_LIST:-100 200 400 500 600 1000}"
SENDERS_LIST="${SENDERS_LIST:-2 10 20 40}"   # max 40 under default offset 20 (funded window ends at idx 59)
SIGN_LIST="${SIGN_LIST:-session eip712}"
DUR="${DUR:-20}"                # timed window per cell (s)
RATE="${RATE:-30}"              # actions/s PER sender (paced sustained ceiling); 0=burst under-measures — avoid
SUBMIT="${SUBMIT:-15}"          # actions per submitNativeActions RPC call (server cap 100)
COOLDOWN="${COOLDOWN:-8}"       # seconds between cells (mempool drain)
FLOOR_BPS="${FLOOR_BPS:-4}"     # ABORT if committed block rate < this (idle baseline ~12.7)
OUTDIR="${OUTDIR:-testnet/grid-native-$(date +s%m%d-%H%M)}"
CSV="$OUTDIR/results.csv"
mkdir -p "$OUTDIR"

[ -x "$BIN" ] || { echo "bench binary not found/executable: $BIN"; exit 1; }

# ---------- helpers ----------
height() {  # committed height as decimal (0 on failure)
  local hex
  hex=$(curl -s --max-time 5 -X POST "$RPC" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
        | grep -oE '"result":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' | head -1)
  [ -n "$hex" ] && printf '%d\n' "$hex" 2>/dev/null || echo 0
}
native_balance() {  # hex native balance for an EVM address ($1); empty on failure/absent
  # RPC serializes RpcBalances in camelCase -> "nativeBalance"; accept snake_case too.
  curl -s --max-time 5 -X POST "$RPC" -H 'content-type: application/json' \
       -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"torus_getBalances\",\"params\":[\"$1\"]}" 2>/dev/null \
    | grep -oiE '"native_?balance":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' | head -1
}
sender_addr() {  # derived EVM address for sender index $1, SAME key path the bench uses
  "$BIN" gen-accounts --offset "$1" --count 1 2>/dev/null | awk '{print $2; exit}'
}
metric() {  # value of a Prometheus counter/gauge by exact name ($1); empty if absent
  curl -s --max-time 5 "$METRICS/metrics" 2>/dev/null | awk -v k="$1" '$1==k{print $2; exit}'
}
blkrate() {  # committed blocks/sec over $1 seconds (default 2)
  local n="${1:-2}" a b; a=$(height); sleep "$n"; b=$(height)
  awk -v a="$a" -v b="$b" -v n="$n" 'BEGIN{printf "%.2f", (b-a)/n}'
}
healthy() {  # 0 if block rate >= floor, else 1
  local br; br=$(blkrate 2)
  awk -v br="$br" -v f="$FLOOR_BPS" 'BEGIN{exit !(br+0 >= f+0)}'
}
window_rate() {  # FILE DUR -> "node_actions_s blk_s" over the peak ~DUR-second window
  # Each bench leg spends 27-44s in pre-sign and up to ~120s draining the mempool, with
  # the action counter/height barely moving in those phases. Dividing the whole-leg delta
  # by full tool wall-time therefore under-reports the true load rate ~4-5x (this faked an
  # 18k-vs-90k orders/s "regression", S434). Instead scan the per-second samples for the
  # single DUR-wide sliding window with the largest counter delta (pre-sign/drain windows
  # carry ~0 delta; the real load window carries the peak) and report that rate, plus the
  # committed-block rate measured over that same best window.
  awk -v dur="${2:-20}" '
    $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]/ { n++; T[n]=$1+0; A[n]=$2+0; H[n]=$3+0 }
    END {
      if (n < 2) { print "NA NA"; exit }
      best=-1; bblk=0
      for (i=1; i<=n; i++) {
        j=i+1
        while (j<=n && T[j]-T[i] < dur) j++
        if (j>n) j=n                       # window shorter than DUR near the tail: best-effort
        dt=T[j]-T[i]
        if (dt<=0) continue
        r=(A[j]-A[i])/dt
        if (r>best) { best=r; bblk=(H[j]-H[i])/dt }
      }
      if (best<0) { print "NA NA"; exit }
      printf "%.1f %.2f\n", best, bblk
    }
  ' "$1"
}

# ---------- funded-range guard (genesis funds native bench senders idx 0..59) ----------
max_senders=0
for s in $SENDERS_LIST; do [ "$s" -gt "$max_senders" ] && max_senders=$s; done
last_idx=$(( SENDER_OFFSET + max_senders - 1 ))
if [ "$(( SENDER_OFFSET + max_senders ))" -gt 60 ]; then
  echo "!! ABORT: sender range exceeds the funded genesis window."
  echo "   SENDER_OFFSET=$SENDER_OFFSET + max(SENDERS_LIST)=$max_senders  ->  last index $last_idx > 59"
  echo "   genesis funds native bench senders indices 0..59 (index 0 = market-maker)."
  echo "   lower SENDERS_LIST, or set SENDER_OFFSET=0 to use the full 0..59 window (e.g. 60 senders)."
  exit 1
fi

# ---------- reachability + funding prechecks (fail loud, not silent-zero) ----------
echo "=== prechecks ==="
# RPC: must answer eth_blockNumber with a climbing height
h_pre=$(height)
if [ "$h_pre" -le 0 ]; then
  echo "!! ABORT: RPC unreachable / not returning eth_blockNumber at: $RPC"
  echo "   the bench senders + health guard need this endpoint (override with RPC=...)."
  exit 1
fi
echo "  RPC ok: $RPC (height $h_pre)"
# METRICS: must be reachable AND actually serve torus_ metrics (wrong endpoint => silent-zero columns)
m_body=$(curl -s --max-time 5 "$METRICS/metrics" 2>/dev/null)
if [ -z "$m_body" ]; then
  echo "!! ABORT: metrics endpoint unreachable/empty at: $METRICS/metrics"
  echo "   node_actions_s/blk_s columns come from here; a wrong METRICS silently zeroes them (override METRICS=...)."
  exit 1
fi
if ! printf '%s\n' "$m_body" | grep -q '^torus_'; then
  echo "!! ABORT: metrics body at $METRICS/metrics has no torus_ metrics — wrong endpoint?"
  echo "   first 3 lines received:"; printf '%s\n' "$m_body" | head -3 | sed 's/^/     /'
  exit 1
fi
if ! printf '%s\n' "$m_body" | grep -q '^torus_native_actions_processed_total'; then
  echo "  WARN: torus_native_actions_processed_total absent from $METRICS/metrics — node_actions_s may read NA"
else
  echo "  METRICS ok: $METRICS/metrics (torus_native_actions_processed_total present)"
fi
# FUNDING: first + last sender in the range must be funded. Prefer torus_getBalances (no load);
# fall back to a tiny 5s smoke cell asserting orders_s>0 if balances are unreadable.
funding_ok=1
for idx in "$SENDER_OFFSET" "$last_idx"; do
  addr=$(sender_addr "$idx")
  if [ -z "$addr" ]; then
    echo "  WARN: could not derive sender addr for idx $idx (BIN gen-accounts failed) — will smoke-test"
    funding_ok=0; break
  fi
  bal=$(native_balance "$addr")
  if [ -z "$bal" ]; then
    echo "  WARN: torus_getBalances unreadable for idx $idx ($addr) — will smoke-test"
    funding_ok=0; break
  fi
  if [ "$bal" = "0x0" ] || [ "$bal" = "0x00" ]; then
    echo "!! ABORT: sender idx $idx ($addr) has ZERO native balance."
    echo "   genesis must fund senders $SENDER_OFFSET..$last_idx; check SENDER_OFFSET / genesis funding."
    exit 1
  fi
  echo "  funded: idx $idx $addr native_balance=$bal"
done
if [ "$funding_ok" -ne 1 ]; then
  s_smoke=1; for s in $SENDERS_LIST; do s_smoke=$s; break; done
  echo "  smoke: 5s cell m=1 b=400 senders=$s_smoke offset=$SENDER_OFFSET to prove funded senders..."
  smoke_out=$("$BIN" consensus --rpc-urls "$RPC" --markets 1 --senders "$s_smoke" \
      --sender-offset "$SENDER_OFFSET" --duration 5 --batch-size 400 --submit-batch "$SUBMIT" \
      --rate "$RATE" --pre-sign 60 --sign-mode session --format bin 2>&1)
  smoke_os=$(printf '%s\n' "$smoke_out" | grep -oE 'Sustained:[[:space:]]*[0-9]+ orders/s' | grep -oE '[0-9]+' | head -1)
  if [ -z "$smoke_os" ] || [ "$smoke_os" -le 0 ]; then
    echo "!! ABORT: smoke cell produced 0 orders/s — senders at offset $SENDER_OFFSET likely unfunded or RPC rejecting."
    printf '%s\n' "$smoke_out" | tail -15 | sed 's/^/     /'
    exit 1
  fi
  echo "  smoke ok: $smoke_os orders/s at offset $SENDER_OFFSET"
fi
echo "=== prechecks passed ==="

# ---------- CSV (resumable) ----------
if [ ! -f "$CSV" ]; then
  echo "markets,batch,senders,sign,submit,rate,dur,orders_s,actions_s,included_s,node_actions_s,blocks,blk_s,h0,h1,status" > "$CSV"
fi
cell_done() { grep -q "^$1,$2,$3,$4," "$CSV"; }

# ---------- plan ----------
total=0
for m in $MARKETS_LIST; do for b in $BATCH_LIST; do for s in $SENDERS_LIST; do for sg in $SIGN_LIST; do total=$((total+1)); done; done; done; done
echo "=== native orders/s grid: $total cells ==="
echo "markets={$MARKETS_LIST} batch={$BATCH_LIST} senders={$SENDERS_LIST} sign={$SIGN_LIST} offset=$SENDER_OFFSET"
echo "dur=${DUR}s rate=${RATE}/sndr submit=${SUBMIT} cooldown=${COOLDOWN}s floor=${FLOOR_BPS}blk/s  out=$OUTDIR"
echo "baseline block rate: $(blkrate 2) blk/s"

i=0; ran=0
for m in $MARKETS_LIST; do
 for b in $BATCH_LIST; do
  for s in $SENDERS_LIST; do
   for sg in $SIGN_LIST; do
    i=$((i+1))
    if cell_done "$m" "$b" "$s" "$sg"; then echo "[$i/$total] skip done  m=$m b=$b s=$s sign=$sg"; continue; fi

    # HEALTH GUARD before load
    if ! healthy; then
      br=$(blkrate 2)
      echo "!! ABORT: chain block-rate ${br}/s < floor ${FLOOR_BPS}/s before cell $i (m=$m b=$b s=$s sign=$sg)" | tee "$OUTDIR/ABORTED"
      echo "   partial results in $CSV"
      break 4
    fi

    # pre-sign ammo: >= dur*rate/submit, but nonce span (presign*submit ms) must stay < 30000 (NONCE_WINDOW/2)
    presign=$(( DUR*RATE/SUBMIT + 15 )); [ "$presign" -gt 1900 ] && presign=1900
    out="$OUTDIR/m${m}_b${b}_s${s}_${sg}.txt"

    samples="$OUTDIR/.samples_m${m}_b${b}_s${s}_${sg}"; : > "$samples"
    h0=$(height); t0=$(date +%s); na0=$(metric torus_native_actions_processed_total)
    echo "[$i/$total] RUN  m=$m b=$b s=$s sign=$sg offset=$SENDER_OFFSET presign=$presign"
    "$BIN" consensus --rpc-urls "$RPC" --markets "$m" --senders "$s" --sender-offset "$SENDER_OFFSET" \
      --duration "$DUR" --batch-size "$b" --submit-batch "$SUBMIT" --rate "$RATE" \
      --pre-sign "$presign" --sign-mode "$sg" --format bin > "$out" 2>&1 &
    bench_pid=$!
    # WINDOWED node-side sampler: append "<epoch> <native_actions_counter> <height>" once
    # per second WHILE the bench runs, so node_actions_s/blk_s can be measured over the real
    # DUR-second LOAD window instead of full tool wall-time. The leg also spends 27-44s in
    # pre-sign + up to ~120s draining (counters ~flat there); whole-wall-time division
    # dilutes the rate ~4-5x and faked an 18k-vs-90k orders/s regression (S434). The sampler
    # self-exits when the bench PID goes away; window_rate() locates the peak window below.
    ( while kill -0 "$bench_pid" 2>/dev/null; do
        printf '%s %s %s\n' "$(date +%s)" "$(metric torus_native_actions_processed_total)" "$(height)" >> "$samples"
        sleep 1
      done ) &
    sampler_pid=$!
    wait "$bench_pid"
    kill "$sampler_pid" 2>/dev/null; wait "$sampler_pid" 2>/dev/null
    t1=$(date +%s); h1=$(height); na1=$(metric torus_native_actions_processed_total)

    orders_s=$(grep -oE 'Sustained:[[:space:]]*[0-9]+ orders/s' "$out" | grep -oE '[0-9]+' | head -1)
    actions_s=$(grep -oE '\([0-9]+ actions/s\)' "$out" | grep -oE '[0-9]+' | head -1)
    included_s=$(grep -iE 'Included' "$out" | grep -oE '[0-9]+(\.[0-9]+)?[[:space:]]*/?s' | grep -oE '[0-9.]+' | head -1)
    dt=$((t1-t0)); [ "$dt" -lt 1 ] && dt=1
    # node_actions_s / blk_s over the peak DUR-second window (see window_rate); fall back to
    # the old whole-window delta only when the sampler captured <2 usable points.
    read -r node_actions_s blk_s < <(window_rate "$samples" "$DUR")
    if [ -z "$node_actions_s" ] || [ "$node_actions_s" = "NA" ]; then
      node_actions_s=$(awk -v a="${na0:-0}" -v b="${na1:-0}" -v d="$dt" 'BEGIN{printf "%.1f",(b-a)/d}')
    fi
    if [ -z "$blk_s" ] || [ "$blk_s" = "NA" ]; then
      blk_s=$(awk -v a="$h0" -v b="$h1" -v d="$dt" 'BEGIN{printf "%.2f",(b-a)/d}')
    fi
    rm -f "$samples"
    echo "$m,$b,$s,$sg,$SUBMIT,$RATE,$DUR,${orders_s:-NA},${actions_s:-NA},${included_s:-NA},$node_actions_s,$((h1-h0)),$blk_s,$h0,$h1,ok" >> "$CSV"
    echo "     -> ${orders_s:-NA} orders/s (${actions_s:-NA} act/s) | node ${node_actions_s} act/s (windowed) | chain ${blk_s} blk/s under load"
    ran=$((ran+1))
    sleep "$COOLDOWN"
   done
  done
 done
done

# ---------- report ----------
echo; echo "=== SUMMARY  ($ran cells run this pass; CSV=$CSV) ==="
echo "--- top 12 by orders/s ---"
{ head -1 "$CSV"; tail -n +2 "$CSV" | awk -F, '$8!="NA" && $8!=""' | sort -t, -k8 -rn | head -12; } | column -t -s,
echo "--- markets scaling @ batch=1000 senders=40 sign=session (parallel-matching payoff) ---"
{ echo "markets orders_s blk_s"; tail -n +2 "$CSV" | awk -F, '$2==1000 && $3==40 && $4=="session"{print $1, $8, $13}' | sort -n; } | column -t
[ -f "$OUTDIR/ABORTED" ] && echo "!! sweep ABORTED early — see $OUTDIR/ABORTED"
echo "done."
