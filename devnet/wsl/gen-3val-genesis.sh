#!/usr/bin/env bash
# gen-3val-genesis.sh — build a bare-metal 3-validator devnet genesis for the
# WSL local devnet (perf/funnel-truth). NO docker.
#
# The genesis = the WEIGHTED 100k-account testnet genesis (bench-reproducible
# funded senders, 10 markets, hardhat governance stakers) but with the consensus
# validator set REPLACED by the 3 deterministic devnet validator keys
# (0x01.. / 0x02.. / 0x03..) whose private keys we actually hold. permanent_stakes
# are governance-only (CF_STAKING_PERMANENT) and carry NO consensus voting power
# (torus-genesis to_hotstuff_genesis builds the validator set solely from
# .validators), so they are kept untouched.
#
# Output (git-ignored, regenerable — the full genesis is too large for git):
#   devnet/wsl/genesis-3val.json
#
# Env: FORCE=1 to rebuild the weighted-full genesis even if present.
set -euo pipefail
cd "$(dirname "$0")/../.."
REPO=$(pwd)

BIN="$REPO/target/release/bench-throughput"
FULL="$REPO/testnet/genesis-weighted-full.json"
OUT="$REPO/devnet/wsl/genesis-3val.json"

[ -x "$BIN" ] || { echo "FATAL: missing $BIN — build first: cargo build --release -p bench-throughput" >&2; exit 1; }
command -v jq >/dev/null || { echo "FATAL: jq not installed" >&2; exit 1; }

# 1. Materialise the weighted 100k-account genesis (adds ~100k bulk senders).
if [ "${FORCE:-0}" = 1 ] || [ ! -f "$FULL" ]; then
    echo "building weighted-full genesis (100k bulk senders)..."
    "$REPO/testnet/gen-weighted-genesis.sh"
else
    echo "reusing existing $FULL"
fi

# 2. The 3 devnet validators (pubkeys == ed25519 pubkeys of keys 01/02/03),
#    lifted verbatim from devnet/genesis.json so stake/commission match.
VALS=$(jq '[.validators[0:3][]]' "$REPO/devnet/genesis.json")

# 3. Weighted accounts + 3-val consensus set.
jq --argjson vals "$VALS" '.validators = $vals' "$FULL" > "$OUT"

echo "wrote $OUT"
echo "  chain_id   = $(jq '.chain_id' "$OUT")"
echo "  validators = $(jq '.validators|length' "$OUT")  ($(jq -r '.validators[].address' "$OUT" | tr '\n' ' '))"
echo "  native_bal = $(jq '.native_balances|length' "$OUT")"
echo "  accounts   = $(jq '.accounts|length' "$OUT")"
echo "  markets    = $(jq '.markets|length' "$OUT")"
echo "  perm_stakes= $(jq '.permanent_stakes|length' "$OUT")"
