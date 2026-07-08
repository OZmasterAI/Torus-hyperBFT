#!/usr/bin/env bash
# bench-evm-tx-grid.sh — cartesian EVM tx/s sweep vs the LIVE seed via torus-tx-flood.
#
# Pre-signed EIP-1559 TRS transfers (chain_id 7778, compiled-in) over eth_sendRawTransaction.
# Walks {accounts} x {txs-per-acct} x {concurrency} x {batch-per-sender}, one tx-flood run
# per cell. Records BOTH the tool's accepted/s (mempool admission — overstates) AND the
# authoritative COMMITTED tx/s measured node-side by scanning eth_getBlockByNumber over the
# run window. (torus_evm_txs_processed_total is registered-but-never-incremented → do NOT use it.)
# Between-cell cooldown; block-rate HEALTH GUARD auto-aborts on wedge/floor; resumable CSV.
#
#   IMPORTANT: only hardhat #0..#9 have EVM balances in genesis → ACCTS capped at 10.
#   Structural ceiling ≈ DRAIN_PER_SENDER_PER_BLOCK(4) x accounts tx/block (=40 at -a 10).
#
# Axes (env lists): ACCTS_LIST  NPER_LIST  CONC_LIST  BPS_LIST
# Knobs: RPC METRICS COOLDOWN FLOOR_BPS OUTDIR BIN
#
#   full grid:    ./testnet/bench-evm-tx-grid.sh
#   quick:        ACCTS_LIST="10" NPER_LIST="500" CONC_LIST="256" ./testnet/bench-evm-tx-grid.sh
#
# SAFETY: real EVM load against the live seed. Guard aborts below FLOOR_BPS block/s.
set -u
cd "$(dirname "$0")/.." || { echo "cannot cd to repo root"; exit 1; }

BIN="${BIN:-./target/release/torus-tx-flood}"
RPC="${RPC:-http://localhost:8545}"
METRICS="${METRICS:-http://127.0.0.1:9090}"
ACCTS_LIST="${ACCTS_LIST:-2 5 10}"     # <=10 (only hardhat #0..#9 are EVM-funded); tool clamps to 20 but #10+ have no EVM balance
NPER_LIST="${NPER_LIST:-200 500}"      # txs per account
CONC_LIST="${CONC_LIST:-64 128 256}"   # in-flight concurrency
BPS_LIST="${BPS_LIST:-4}"              # --batch-per-sender (node drains 4/sender/block; 8 tests over-drain)
COOLDOWN="${COOLDOWN:-8}"
FLOOR_BPS="${FLOOR_BPS:-4}"
OUTDIR="${OUTDIR:-testnet/grid-evm-$(date +s%m%d-%H%M)}"
CSV="$OUTDIR/results.csv"
mkdir -p "$OUTDIR"

[ -x "$BIN" ] || { echo "tx-flood binary not found/executable: $BIN"; exit 1; }

height() {
  local hex
  hex=$(curl -s -X POST "$RPC" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
        | grep -oE '"result":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' | head -1)
  [ -n "$hex" ] && printf '%d\n' "$hex" 2>/dev/null || echo 0
}
blkrate() { local n="${1:-2}" a b; a=$(height); sleep "$n"; b=$(height); awk -v a="$a" -v b="$b" -v n="$n" 'BEGIN{printf "%.2f",(b-a)/n}'; }
healthy() { local br; br=$(blkrate 2); awk -v br="$br" -v f="$FLOOR_BPS" 'BEGIN{exit !(br+0>=f+0)}'; }
# committed EVM txs in (h0,h1]: sum tx-hash count per block via eth_getBlockByNumber(n,false)
evm_committed() {
  local n hexn cnt tot=0
  for ((n=$1+1; n<=$2 && n>0; n++)); do
    hexn=$(printf '0x%x' "$n")
    cnt=$(curl -s -X POST "$RPC" -H 'content-type: application/json' \
          -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$hexn\",false]}" \
          | grep -oE '"transactions":\[[^]]*\]' | grep -oE '0x[0-9a-fA-F]{64}' | wc -l)
    tot=$((tot+cnt))
  done
  echo "$tot"
}

if [ ! -f "$CSV" ]; then
  echo "accounts,nper,concurrency,batch_per_sender,accepted_s,committed_txs,committed_tx_s,blocks,elapsed_s,est_ceiling_tx_blk,status" > "$CSV"
fi
cell_done() { grep -q "^$1,$2,$3,$4," "$CSV"; }

total=0
for a in $ACCTS_LIST; do for n in $NPER_LIST; do for c in $CONC_LIST; do for bp in $BPS_LIST; do total=$((total+1)); done;done;done;done
echo "=== EVM tx/s grid: $total cells ==="
echo "accounts={$ACCTS_LIST} nper={$NPER_LIST} conc={$CONC_LIST} batch-per-sender={$BPS_LIST}"
echo "cooldown=${COOLDOWN}s floor=${FLOOR_BPS}blk/s out=$OUTDIR ; baseline $(blkrate 2) blk/s"

i=0; ran=0
for a in $ACCTS_LIST; do
 if [ "$a" -gt 10 ]; then echo "clamping accounts $a -> 10 (EVM funding)"; a=10; fi
 for n in $NPER_LIST; do
  for c in $CONC_LIST; do
   for bp in $BPS_LIST; do
    i=$((i+1))
    if cell_done "$a" "$n" "$c" "$bp"; then echo "[$i/$total] skip done a=$a n=$n c=$c bp=$bp"; continue; fi
    if ! healthy; then
      br=$(blkrate 2)
      echo "!! ABORT: block-rate ${br}/s < floor ${FLOOR_BPS}/s before cell $i (a=$a n=$n c=$c bp=$bp)" | tee "$OUTDIR/ABORTED"
      break 4
    fi
    out="$OUTDIR/a${a}_n${n}_c${c}_bp${bp}.txt"
    h0=$(height); t0=$(date +%s)
    echo "[$i/$total] RUN a=$a n=$n c=$c bp=$bp"
    "$BIN" -r "$RPC" -a "$a" -n "$n" -c "$c" --batch-per-sender "$bp" --monitor-interval 2 > "$out" 2>&1
    t1=$(date +%s); h1=$(height)
    elapsed=$((t1-t0)); [ "$elapsed" -lt 1 ] && elapsed=1
    accepted_s=$(grep -oE 'Rate:[[:space:]]*[0-9]+(\.[0-9]+)?[[:space:]]*accepted/s' "$out" | grep -oE '[0-9]+(\.[0-9]+)?' | tail -1)
    committed=$(evm_committed "$h0" "$h1")
    committed_s=$(awk -v c="$committed" -v e="$elapsed" 'BEGIN{printf "%.1f", c/e}')
    ceiling=$((4*a))
    echo "$a,$n,$c,$bp,${accepted_s:-NA},$committed,$committed_s,$((h1-h0)),$elapsed,$ceiling,ok" >> "$CSV"
    echo "     -> accepted ${accepted_s:-NA}/s | COMMITTED $committed txs = ${committed_s} tx/s over ${elapsed}s (ceiling ~${ceiling}/blk)"
    ran=$((ran+1))
    sleep "$COOLDOWN"
   done
  done
 done
done

echo; echo "=== SUMMARY ($ran cells; CSV=$CSV) ==="
{ head -1 "$CSV"; tail -n +2 "$CSV" | awk -F, '$7!="NA" && $7!=""' | sort -t, -k7 -rn | head -12; } | column -t -s,
[ -f "$OUTDIR/ABORTED" ] && echo "!! sweep ABORTED early — see $OUTDIR/ABORTED"
echo "done."
