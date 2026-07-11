#!/usr/bin/env bash
# bench-ofat-ramp-s447.sh — OFAT (one-factor-at-a-time) throughput sweep vs the LIVE chain.
#
# Unlike bench-native-orders-grid.sh (full cartesian, pre-sign only), this walks a strong
# BASELINE and sweeps each knob INDEPENDENTLY, so each axis' effect is isolated without a
# 1600-cell cross-product. Adds three axes the grid never touched: pre-sign-vs-stream mode,
# --concurrency, and --rate. Per cell it also samples the bench process PEAK RSS (the memory
# question: stream mode stays flat; pre-sign balloons ~28MB/sender as ammo).
#
# Records per cell: orders/s (tool), node actions/s + blk/s (Prometheus, peak DUR-window),
# peak RSS (MB). Resumable (cells already in CSV are skipped). Health guard AUTO-ABORTS the
# whole sweep if the live chain's committed block rate drops below FLOOR_BPS.
#
# Env knobs (override any):  RPC METRICS DUR COOLDOWN FLOOR_BPS SUBMIT FORMAT SENDER_OFFSET
#                            FUNDED_CEIL OUTDIR BIN  SIGN_LIST
#   plus per-axis lists:     SENDERS_LIST MARKETS_LIST BATCH_LIST CONC_LIST RATE_LIST
#   baseline:                B_MK B_BATCH B_SEND B_CONC
set -u
cd "$(dirname "$0")/.." || { echo "cannot cd to repo root"; exit 1; }

BIN="${BIN:-./target/release/bench-throughput}"
RPC="${RPC:-http://127.0.0.1:8555}"
METRICS="${METRICS:-http://127.0.0.1:9090}"
SENDER_OFFSET="${SENDER_OFFSET:-0}"
FUNDED_CEIL="${FUNDED_CEIL:-100060}"
FORMAT="${FORMAT:-bin}"
SUBMIT="${SUBMIT:-15}"
DUR="${DUR:-15}"
COOLDOWN="${COOLDOWN:-6}"
FLOOR_BPS="${FLOOR_BPS:-3}"
SIGN_LIST="${SIGN_LIST:-eip712 session}"

# ---- baseline (held constant while sweeping a given axis) ----
B_MK="${B_MK:-1}"; B_BATCH="${B_BATCH:-400}"; B_SEND="${B_SEND:-100}"; B_CONC="${B_CONC:-512}"

# ---- per-axis sweep values ----
SENDERS_LIST="${SENDERS_LIST:-1 10 25 50 100 200 500 1000 2000 5000}"
MARKETS_LIST="${MARKETS_LIST:-1 2 3 4 5 6 7 8 9 10}"
BATCH_LIST="${BATCH_LIST:-10 50 100 200 400 500 800 1000}"
CONC_LIST="${CONC_LIST:-64 128 256 512 1024 2048}"
RATE_LIST="${RATE_LIST:-0 30 100}"                 # pre-sign mode only
PRESIGN_SENDERS="${PRESIGN_SENDERS:-100 500 2000}" # sender points to A/B stream-vs-presign

OUTDIR="${OUTDIR:-testnet/ofat-s447-$(date +%m%d-%H%M)}"
CSV="$OUTDIR/results.csv"
mkdir -p "$OUTDIR"
[ -x "$BIN" ] || { echo "bench binary not found/executable: $BIN"; exit 1; }

# ---------------- helpers (borrowed from the grid script) ----------------
height() {
  local hex
  hex=$(curl -s --max-time 5 -X POST "$RPC" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
        | grep -oE '"result":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' | head -1)
  [ -n "$hex" ] && printf '%d\n' "$hex" 2>/dev/null || echo 0
}
native_balance() {
  curl -s --max-time 5 -X POST "$RPC" -H 'content-type: application/json' \
       -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"torus_getBalances\",\"params\":[\"$1\"]}" 2>/dev/null \
    | grep -oiE '"native_?balance":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' | head -1
}
sender_addr() { "$BIN" gen-accounts --offset "$1" --count 1 2>/dev/null | awk '{print $2; exit}'; }
metric() { curl -s --max-time 5 "$METRICS/metrics" 2>/dev/null | awk -v k="$1" '$1==k{print $2; exit}'; }
blkrate() { local n="${1:-2}" a b; a=$(height); sleep "$n"; b=$(height); awk -v a="$a" -v b="$b" -v n="$n" 'BEGIN{printf "%.2f",(b-a)/n}'; }
healthy() { local br; br=$(blkrate 2); awk -v br="$br" -v f="$FLOOR_BPS" 'BEGIN{exit !(br+0 >= f+0)}'; }
# FILE DUR -> "node_actions_s blk_s peak_rss_mb" over the peak DUR-second action window.
# sample line: "<epoch> <native_actions_counter> <height> <vmrss_kb>"
window_rate() {
  awk -v dur="${2:-15}" '
    $1 ~ /^[0-9]+$/ { n++; T[n]=$1+0; A[n]=$2+0; H[n]=$3+0; R[n]=$4+0; if(R[n]>rmax)rmax=R[n] }
    END {
      if (n < 2) { printf "NA NA %.1f\n", rmax/1024; exit }
      best=-1; bblk=0
      for (i=1;i<=n;i++){ j=i+1; while(j<=n && T[j]-T[i]<dur) j++; if(j>n)j=n; dt=T[j]-T[i]; if(dt<=0)continue;
        r=(A[j]-A[i])/dt; if(r>best){best=r; bblk=(H[j]-H[i])/dt} }
      if(best<0){ printf "NA NA %.1f\n", rmax/1024; exit }
      printf "%.1f %.2f %.1f\n", best, bblk, rmax/1024
    }' "$1"
}

# ---------------- prechecks (fail loud) ----------------
echo "=== prechecks ==="
h_pre=$(height); [ "$h_pre" -le 0 ] && { echo "!! ABORT: RPC unreachable at $RPC"; exit 1; }
echo "  RPC ok: $RPC (height $h_pre)"
m_body=$(curl -s --max-time 5 "$METRICS/metrics" 2>/dev/null)
[ -z "$m_body" ] && { echo "!! ABORT: metrics unreachable at $METRICS/metrics"; exit 1; }
printf '%s\n' "$m_body" | grep -q '^torus_native_actions_processed_total' \
  && echo "  METRICS ok: torus_native_actions_processed_total present" \
  || echo "  WARN: torus_native_actions_processed_total absent — node_actions_s may read NA"
max_s=0; for s in $SENDERS_LIST; do [ "$s" -gt "$max_s" ] && max_s=$s; done
last_idx=$(( SENDER_OFFSET + max_s - 1 ))
[ "$(( SENDER_OFFSET + max_s ))" -gt "$FUNDED_CEIL" ] && { echo "!! ABORT: senders exceed funded window ($last_idx > $((FUNDED_CEIL-1)))"; exit 1; }
for idx in "$SENDER_OFFSET" "$last_idx"; do
  addr=$(sender_addr "$idx"); bal=$(native_balance "$addr")
  [ -z "$bal" ] && { echo "  WARN: balance unreadable for idx $idx ($addr)"; continue; }
  { [ "$bal" = "0x0" ] || [ "$bal" = "0x00" ]; } && { echo "!! ABORT: idx $idx ($addr) ZERO balance"; exit 1; }
  echo "  funded: idx $idx $addr native_balance=$bal"
done
echo "=== prechecks passed ==="

# ---------------- CSV (resumable) ----------------
[ -f "$CSV" ] || echo "axis,mode,sign,senders,markets,batch,submit,conc,rate,dur,orders_s,actions_s,node_actions_s,blk_s,peak_rss_mb,base_rss_mb,h0,h1,status" > "$CSV"
# key = mode,sign,senders,markets,batch,conc,rate  (axis excluded so dedup is by real config)
cell_done() { cut -d, -f2-9 "$CSV" | grep -qx "$1"; }

# ---------------- build cell list (OFAT) ----------------
# spec: axis|mode|sign|senders|markets|batch|conc|rate
CELLS=()
add() { CELLS+=("$1"); }
for sg in $SIGN_LIST; do
  add "baseline|stream|$sg|$B_SEND|$B_MK|$B_BATCH|$B_CONC|0"
  for s in $SENDERS_LIST; do add "senders|stream|$sg|$s|$B_MK|$B_BATCH|$B_CONC|0"; done
  for m in $MARKETS_LIST; do add "markets|stream|$sg|$B_SEND|$m|$B_BATCH|$B_CONC|0"; done
  for b in $BATCH_LIST;  do add "batch|stream|$sg|$B_SEND|$B_MK|$b|$B_CONC|0"; done
  for c in $CONC_LIST;   do add "conc|stream|$sg|$B_SEND|$B_MK|$B_BATCH|$c|0"; done
  # pre-sign builds ALL orders up front (O(all orders) signing). At batch=400 that is tens of
  # millions of eip712 sigs = hours, and nonces (stamped at presign-start, ~60s horizon) go
  # stale -> ALL ammo rejected -> 0 orders/s (the S387/S434 "presign trap"). The S434 auto-
  # fallback (choose_ammo_plan) is NOT in this binary, so do the stream-vs-presign A/B at
  # batch=1 where the build is cheap and nonces stay fresh.
  for s in $PRESIGN_SENDERS; do
    add "mode|stream|$sg|$s|$B_MK|1|$B_CONC|0"
    add "mode|presign|$sg|$s|$B_MK|1|$B_CONC|30"
  done
  for r in $RATE_LIST;   do add "rate|presign|$sg|$B_SEND|$B_MK|1|$B_CONC|$r"; done
done

total=${#CELLS[@]}
echo "=== OFAT sweep: $total cells (pre-dedup) ==="
echo "baseline: mode=stream m=$B_MK b=$B_BATCH s=$B_SEND conc=$B_CONC submit=$SUBMIT fmt=$FORMAT dur=${DUR}s"
echo "out=$OUTDIR  floor=${FLOOR_BPS}blk/s  baseline block rate: $(blkrate 2) blk/s"

# ---------------- run ----------------
i=0; ran=0
for spec in "${CELLS[@]}"; do
  i=$((i+1))
  IFS='|' read -r axis mode sg s m b c r <<< "$spec"
  key="$mode,$sg,$s,$m,$b,$SUBMIT,$c,$r"
  if cell_done "$key"; then echo "[$i/$total] skip done  ($axis) $key"; continue; fi
  if ! healthy; then
    br=$(blkrate 2)
    echo "!! ABORT: block-rate ${br}/s < floor ${FLOOR_BPS} before cell $i" | tee "$OUTDIR/ABORTED"
    break
  fi
  # pre-sign ammo sizing: paced -> dur*rate/submit+15; burst -> fixed cap
  presign_arg=0
  if [ "$mode" = "presign" ]; then
    if [ "$r" -gt 0 ]; then presign_arg=$(( DUR*r/SUBMIT + 15 )); else presign_arg=$(( DUR*80/SUBMIT )); fi
    [ "$presign_arg" -gt 1900 ] && presign_arg=1900
    # HARD GUARD (S387/S434 presign trap): total sigs to build = presign_arg*SUBMIT*b*s. Cap so
    # the eip712 build stays ~<30s and nonces (stamped at presign-start) don't age past ~60s.
    max_ammo=$(( 8000000 / (SUBMIT * b * s) )); [ "$max_ammo" -lt 1 ] && max_ammo=1
    [ "$presign_arg" -gt "$max_ammo" ] && { echo "     (presign ammo capped $presign_arg->$max_ammo to bound sig-build)"; presign_arg=$max_ammo; }
  fi
  out="$OUTDIR/${axis}_${mode}_${sg}_s${s}_m${m}_b${b}_c${c}_r${r}.txt"
  samples="$OUTDIR/.samples"; : > "$samples"
  h0=$(height)
  echo "[$i/$total] RUN ($axis) mode=$mode sign=$sg s=$s m=$m b=$b conc=$c rate=$r presign=$presign_arg"
  "$BIN" consensus --rpc-urls "$RPC" --senders "$s" --sender-offset "$SENDER_OFFSET" \
    --markets "$m" --batch-size "$b" --submit-batch "$SUBMIT" --concurrency "$c" \
    --duration "$DUR" --rate "$r" --pre-sign "$presign_arg" --sign-mode "$sg" --format "$FORMAT" \
    > "$out" 2>&1 &
  bench_pid=$!
  base_rss=$(awk '/VmRSS/{print int($2/1024)}' /proc/$bench_pid/status 2>/dev/null)
  # sampler: epoch, native_actions_counter, height, VmRSS_kB  (peak RSS answers the memory Q)
  ( while kill -0 "$bench_pid" 2>/dev/null; do
      rss=$(awk '/VmRSS/{print $2}' /proc/$bench_pid/status 2>/dev/null)
      printf '%s %s %s %s\n' "$(date +%s)" "$(metric torus_native_actions_processed_total)" "$(height)" "${rss:-0}" >> "$samples"
      sleep 1
    done ) &
  sampler_pid=$!
  wait "$bench_pid"; kill "$sampler_pid" 2>/dev/null; wait "$sampler_pid" 2>/dev/null
  h1=$(height)
  orders_s=$(grep -oE 'Sustained:[[:space:]]*[0-9]+ orders/s' "$out" | grep -oE '[0-9]+' | head -1)
  actions_s=$(grep -oE '\([0-9]+ actions/s\)' "$out" | grep -oE '[0-9]+' | head -1)
  read -r node_actions_s blk_s peak_rss < <(window_rate "$samples" "$DUR")
  echo "$axis,$mode,$sg,$s,$m,$b,$SUBMIT,$c,$r,$DUR,${orders_s:-NA},${actions_s:-NA},${node_actions_s},${blk_s},${peak_rss},${base_rss:-NA},$h0,$h1,ok" >> "$CSV"
  echo "     -> ${orders_s:-NA} orders/s | node ${node_actions_s} act/s | ${blk_s} blk/s | peakRSS ${peak_rss}MB (base ${base_rss:-?}MB)"
  ran=$((ran+1)); rm -f "$samples"; sleep "$COOLDOWN"
done

echo; echo "=== SUMMARY ($ran cells this pass; CSV=$CSV) ==="
echo "--- top 15 by orders/s ---"
{ head -1 "$CSV"; tail -n +2 "$CSV" | awk -F, '$11!="NA"&&$11!=""' | sort -t, -k11 -rn | head -15; } | column -t -s,
echo "--- peak RSS by senders (stream vs presign — memory question) ---"
{ echo "mode senders peak_rss_mb orders_s"; tail -n +2 "$CSV" | awk -F, '{print $2, $4, $15, $11}' | sort -k1,1 -k2,2n -u; } | column -t
[ -f "$OUTDIR/ABORTED" ] && echo "!! sweep ABORTED early — see $OUTDIR/ABORTED"
echo "done."
