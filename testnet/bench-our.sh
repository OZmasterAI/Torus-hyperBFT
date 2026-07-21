#!/usr/bin/env bash
# Session throughput bench — OUR machine (box 1 of 3). Senders 0..19 (hardhat keys).
# Run AT THE SAME TIME as bench-val1.sh (20..39) and bench-friend2.sh (40..59).
# Disjoint sender ranges so nonces / accounts / rate-limits never collide; each box
# benches its OWN local node (spreads ingress across all 3). All 60 senders are
# funded in testnet/genesis.json (era 1781913600, chain_id 7778).
#
# PREREQ: all 3 validators on the FIXED (session-key seconds/ms) binary + this genesis.
# A/B (lever 1): SIGN_MODE=eip712 ./testnet/bench-our.sh   # baseline (ecrecover/action)
#                ./testnet/bench-our.sh                    # session fast path (default)
set -euo pipefail
# Resolve via cargo metadata: a redirected [build] target-dir means a successful
# `cargo build --release` leaves target/release EMPTY. See testnet/lib/cargo-bin.sh.
_R="$(git rev-parse --show-toplevel 2>/dev/null || echo .)"
. "$_R/testnet/lib/cargo-bin.sh"
BIN="${BIN:-$(cargo_bin bench-throughput "$_R")}"
RPC="${RPC:-http://localhost:8545}"
SIGN_MODE="${SIGN_MODE:-session}"
echo "[bench OUR] senders 0..19 | sign=$SIGN_MODE | rpc=$RPC" >&2
exec "$BIN" consensus --rpc-urls "$RPC" \
  --senders 20 --sender-offset 0 \
  --duration "${DURATION:-20}" --batch-size "${BATCH:-400}" \
  --submit-batch "${SUBMIT_BATCH:-15}" --rate "${RATE:-30}" --pre-sign "${PRESIGN:-60}" \
  --sign-mode "$SIGN_MODE" --format bin
