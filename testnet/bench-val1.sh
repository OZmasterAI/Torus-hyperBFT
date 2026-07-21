#!/usr/bin/env bash
# Session throughput bench — VAL1 box (box 2 of 3, friend1's 4-core server, ssh).
# Senders 20..39. Run AT THE SAME TIME as bench-our.sh (0..19) and
# bench-friend2.sh (40..59). Disjoint ranges so nonces/accounts never collide;
# benches val1's OWN local node. 60 senders funded in testnet/genesis-weighted-base.json
# (era 1781913600, chain_id 7778).
#
# RUN ON VAL1 (we have ssh): scp the bench-throughput binary + this script over, then
#     BIN=/path/to/bench-throughput ./bench-val1.sh
# NOTE: val1 is 4-core — the bench competes with val1's node for CPU. Lower --rate
# if val1's block production stalls (prior val1-localhost runs used rate 30, sb 15).
# PREREQ: val1 on the FIXED (session-key) binary + this genesis.
set -euo pipefail
# Resolve via cargo metadata: a redirected [build] target-dir means a successful
# `cargo build --release` leaves target/release EMPTY. See testnet/lib/cargo-bin.sh.
_R="$(git rev-parse --show-toplevel 2>/dev/null || echo .)"
. "$_R/testnet/lib/cargo-bin.sh"
BIN="${BIN:-$(cargo_bin bench-throughput "$_R")}"
RPC="${RPC:-http://localhost:8545}"
SIGN_MODE="${SIGN_MODE:-session}"
echo "[bench VAL1] senders 20..39 | sign=$SIGN_MODE | rpc=$RPC" >&2
exec "$BIN" consensus --rpc-urls "$RPC" \
  --senders 20 --sender-offset 20 \
  --duration "${DURATION:-20}" --batch-size "${BATCH:-400}" \
  --submit-batch "${SUBMIT_BATCH:-15}" --rate "${RATE:-30}" --pre-sign "${PRESIGN:-60}" \
  --sign-mode "$SIGN_MODE" --format bin
