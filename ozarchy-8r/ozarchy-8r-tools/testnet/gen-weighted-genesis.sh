#!/usr/bin/env bash
# gen-weighted-genesis.sh (S446) — reproduce the weighted-relaunch genesis from the lean base.
# Adds BULK bench-reproducible funded accounts (native + EVM) that are too large to keep in git.
# NOTE (A4): the default NATIVE_AVAIL was raised 1M -> 100M TRS for sustained matched-flow
# benching. To reproduce the S434 live relaunch genesis (state root 0x0395a572...), run with
# NATIVE_AVAIL=1000000.0 — the live genesis was generated with the old 1M default.
#   ./testnet/gen-weighted-genesis.sh              # -> testnet/genesis-weighted-full.json
# Env: BASE BIN OUT BULK_OFFSET BULK_COUNT NATIVE_AVAIL EVM_WEI
# Also writes ${OUT%.json}.params.json: {bulk_offset, bulk_count, native_avail,
# evm_wei, bytes} (bytes = OUT's size), so a reuser (devnet/wsl/gen-3val-genesis.sh)
# can tell which BULK_COUNT built OUT without parsing it (~290 MB at 1M accounts).
# Cost at BULK_COUNT=1M (archy, s68): gen-accounts ~1M key derivations, then
# three jq passes (two -Rn builds + the merge); a 1M-row genesis parse is ~3 s
# and a rewrite ~7 s / ~1.4 GB RSS with jq 1.8.
set -euo pipefail
cd "$(dirname "$0")/.."
# Resolve bench-throughput via cargo metadata — a redirected [build] target-dir
# means a successful `cargo build --release` leaves ./target/release EMPTY.
. "$(dirname "$0")/lib/cargo-bin.sh"
BASE="${BASE:-testnet/genesis-weighted-base.json}"
BIN="${BIN:-$(cargo_bin bench-throughput "$PWD")}"
OUT="${OUT:-testnet/genesis-weighted-full.json}"
BULK_OFFSET="${BULK_OFFSET:-60}"        # start index (0..59 already in base)
BULK_COUNT="${BULK_COUNT:-100000}"      # how many bulk test senders
NATIVE_AVAIL="${NATIVE_AVAIL:-100000000.0}"                  # native TRS each (A4: 100M — headroom for sustained matched flow; maker fills strand ~margin/order in order_margin under current node semantics, so the bench econ shape is the real fix and this is belt-and-braces)
EVM_WEI="${EVM_WEI:-1000000000000000000000}"                # EVM wei each (1000 TRS)
[[ "$BULK_COUNT" =~ ^[1-9][0-9]*$ ]] || { echo "FATAL: BULK_COUNT must be a positive integer (got '$BULK_COUNT')" >&2; exit 1; }
[[ "$BULK_OFFSET" =~ ^(0|[1-9][0-9]*)$ ]] || { echo "FATAL: BULK_OFFSET must be a non-negative integer (got '$BULK_OFFSET')" >&2; exit 1; }
PARAMS="${OUT%.json}.params.json"
# A sidecar left from an earlier build must never describe a half-written OUT.
rm -f -- "${PARAMS:?}"
tmp="$(mktemp -d)"; trap 'rm -r "$tmp"' EXIT
"$BIN" gen-accounts --offset "$BULK_OFFSET" --count "$BULK_COUNT" > "$tmp/addrs.txt"
jq -Rn --arg av "$NATIVE_AVAIL" '[inputs|split(" ")|{address:.[1],available:$av,note:("bulk-test "+.[0])}]' "$tmp/addrs.txt" > "$tmp/nat.json"
jq -Rn --arg bal "$EVM_WEI"    '[inputs|split(" ")|{address:.[1],balance:$bal,note:("bulk-test "+.[0])}]' "$tmp/addrs.txt" > "$tmp/evm.json"
jq --slurpfile nat "$tmp/nat.json" --slurpfile evm "$tmp/evm.json" \
   '.native_balances += $nat[0] | .accounts += $evm[0]' "$BASE" > "$OUT"
jq -n --argjson off "$BULK_OFFSET" --argjson n "$BULK_COUNT" --arg av "$NATIVE_AVAIL" --arg wei "$EVM_WEI" \
   --argjson bytes "$(wc -c < "$OUT")" \
   '{bulk_offset: $off, bulk_count: $n, native_avail: $av, evm_wei: $wei, bytes: $bytes}' > "$tmp/params.json"
mv -f "$tmp/params.json" "$PARAMS"
echo "wrote $OUT ($(wc -c < "$OUT") bytes, $(jq -r '"native=\(.native_balances|length) accts=\(.accounts|length)"' "$OUT"), bulk_offset=$BULK_OFFSET bulk_count=$BULK_COUNT) + $PARAMS"
