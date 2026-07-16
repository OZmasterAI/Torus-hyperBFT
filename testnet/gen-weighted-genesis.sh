#!/usr/bin/env bash
# gen-weighted-genesis.sh (S446) — reproduce the weighted-relaunch genesis from the lean base.
# Adds BULK bench-reproducible funded accounts (native + EVM) that are too large to keep in git.
# NOTE (A4): the default NATIVE_AVAIL was raised 1M -> 100M TRS for sustained matched-flow
# benching. To reproduce the S434 live relaunch genesis (state root 0x0395a572...), run with
# NATIVE_AVAIL=1000000.0 — the live genesis was generated with the old 1M default.
#   ./testnet/gen-weighted-genesis.sh              # -> testnet/genesis-weighted-full.json
# Env: BASE BIN OUT BULK_OFFSET BULK_COUNT NATIVE_AVAIL EVM_WEI
set -euo pipefail
cd "$(dirname "$0")/.."
BASE="${BASE:-testnet/genesis-weighted-base.json}"
BIN="${BIN:-./target/release/bench-throughput}"
OUT="${OUT:-testnet/genesis-weighted-full.json}"
BULK_OFFSET="${BULK_OFFSET:-60}"        # start index (0..59 already in base)
BULK_COUNT="${BULK_COUNT:-100000}"      # how many bulk test senders
NATIVE_AVAIL="${NATIVE_AVAIL:-100000000.0}"                  # native TRS each (A4: 100M — headroom for sustained matched flow; maker fills strand ~margin/order in order_margin under current node semantics, so the bench econ shape is the real fix and this is belt-and-braces)
EVM_WEI="${EVM_WEI:-1000000000000000000000}"                # EVM wei each (1000 TRS)
tmp="$(mktemp -d)"; trap 'rm -r "$tmp"' EXIT
"$BIN" gen-accounts --offset "$BULK_OFFSET" --count "$BULK_COUNT" > "$tmp/addrs.txt"
jq -Rn --arg av "$NATIVE_AVAIL" '[inputs|split(" ")|{address:.[1],available:$av,note:("bulk-test "+.[0])}]' "$tmp/addrs.txt" > "$tmp/nat.json"
jq -Rn --arg bal "$EVM_WEI"    '[inputs|split(" ")|{address:.[1],balance:$bal,note:("bulk-test "+.[0])}]' "$tmp/addrs.txt" > "$tmp/evm.json"
jq --slurpfile nat "$tmp/nat.json" --slurpfile evm "$tmp/evm.json" \
   '.native_balances += $nat[0] | .accounts += $evm[0]' "$BASE" > "$OUT"
echo "wrote $OUT ($(wc -c < "$OUT") bytes, native=$(jq '.native_balances|length' "$OUT") accts=$(jq '.accounts|length' "$OUT"))"
