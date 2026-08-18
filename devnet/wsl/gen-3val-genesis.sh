#!/usr/bin/env bash
# gen-3val-genesis.sh — build a bare-metal 3-validator devnet genesis for the
# WSL local devnet (perf/funnel-truth). NO docker.
#
# The genesis = the WEIGHTED 100k-account testnet genesis (bench-reproducible
# funded senders, pre-seeded markets, hardhat governance stakers) but with the
# consensus validator set REPLACED by the 3 deterministic devnet validator keys
# (0x01.. / 0x02.. / 0x03..) whose private keys we actually hold. permanent_stakes
# are governance-only (CF_STAKING_PERMANENT) and carry NO consensus voting power
# (torus-genesis to_hotstuff_genesis builds the validator set solely from
# .validators), so they are kept untouched.
#
# Output (git-ignored, regenerable — the full genesis is too large for git):
#   devnet/wsl/genesis-3val.json            (override with OUT=/path)
#
# Env:
#   FORCE=1     rebuild the weighted-full genesis even if present.
#   MARKETS=N   emit exactly N markets (default: whatever the weighted base
#               carries — 100 since ad3b8d8). N <= base: keep the first N
#               market rows verbatim (ids 1..N, the bench spreads orders over
#               ids 1..=--markets). N > base: append synthetic S<k>-USD perps
#               following the existing row schema (lot/tick 1.0, im 5.0).
#               The 100k funded accounts are untouched either way.
#   OUT=/path   where to write the 3-val genesis.
#   BENCH_BIN=  bench-throughput binary (default: cargo-bin resolution, which
#               honours CARGO_TARGET_DIR, then <repo>/target/release).
set -euo pipefail
cd "$(dirname "$0")/../.."
REPO=$(pwd)

. "$REPO/testnet/lib/cargo-bin.sh"
BIN="${BENCH_BIN:-$(cargo_bin bench-throughput "$REPO")}" || exit 1
FULL="$REPO/testnet/genesis-weighted-full.json"
OUT="${OUT:-$REPO/devnet/wsl/genesis-3val.json}"

[ -x "$BIN" ] || { echo "FATAL: missing $BIN — build first: cargo build --release -p bench-throughput" >&2; exit 1; }
command -v jq >/dev/null || { echo "FATAL: jq not installed" >&2; exit 1; }

# 1. Materialise the weighted 100k-account genesis (adds ~100k bulk senders).
if [ "${FORCE:-0}" = 1 ] || [ ! -f "$FULL" ]; then
    echo "building weighted-full genesis (100k bulk senders)..."
    BIN="$BIN" "$REPO/testnet/gen-weighted-genesis.sh"
else
    echo "reusing existing $FULL"
fi

# 2. The 3 devnet validators (pubkeys == ed25519 pubkeys of keys 01/02/03),
#    lifted verbatim from devnet/genesis.json so stake/commission match.
VALS=$(jq '[.validators[0:3][]]' "$REPO/devnet/genesis.json")

# 3. Weighted accounts + 3-val consensus set (+ optional market count).
if [ -n "${MARKETS:-}" ]; then
    [[ "$MARKETS" =~ ^[0-9]+$ ]] && [ "$MARKETS" -ge 1 ] \
        || { echo "FATAL: MARKETS must be a positive integer (got '$MARKETS')" >&2; exit 1; }
    # Keep the first N rows verbatim; synthesise S<k>-USD rows past the base.
    jq --argjson vals "$VALS" --argjson n "$MARKETS" '
        .validators = $vals
        | .markets = (.markets | sort_by(.market_id))
        | .markets = (
            if ($n <= (.markets|length)) then .markets[0:$n]
            else .markets + [ range((.markets|length)+1; $n+1) | {
                market_id: .,
                base_asset: ("S" + tostring),
                quote_asset: "USD",
                lot_size: "1.0",
                tick_size: "1.0",
                initial_margin: "5.0",
                note: ("synthetic bench market S" + tostring + "-USD (added for multi-market throughput sweeps)")
            } ] end )' "$FULL" > "$OUT"
    # Sanity: exactly N rows, ids 1..N contiguous, all rows have the schema.
    got=$(jq '.markets|length' "$OUT")
    ids_ok=$(jq '[.markets[].market_id] == [range(1; (.markets|length)+1)]' "$OUT")
    [ "$got" = "$MARKETS" ] && [ "$ids_ok" = true ] \
        || { echo "FATAL: market synthesis failed (got=$got ids_contiguous=$ids_ok)" >&2; exit 1; }
else
    jq --argjson vals "$VALS" '.validators = $vals' "$FULL" > "$OUT"
fi

echo "wrote $OUT"
echo "  chain_id   = $(jq '.chain_id' "$OUT")"
echo "  validators = $(jq '.validators|length' "$OUT")  ($(jq -r '.validators[].address' "$OUT" | tr '\n' ' '))"
echo "  native_bal = $(jq '.native_balances|length' "$OUT")"
echo "  accounts   = $(jq '.accounts|length' "$OUT")"
echo "  markets    = $(jq '.markets|length' "$OUT")"
echo "  perm_stakes= $(jq '.permanent_stakes|length' "$OUT")"
