#!/usr/bin/env bash
# Session throughput bench — FRIEND2 box (box 3 of 3). Senders 40..59.
# Run this AT THE SAME TIME as the other two boxes (our: 0..19, val1: 20..39).
#
# HOW TO RUN (friend2 — you don't need the repo, just this script + the binary):
#     BIN=/path/to/bench-throughput ./bench-friend2.sh
#   Defaults assume ./target/release/bench-throughput and your local node's RPC at
#   http://localhost:8545. Override with BIN=... and RPC=... if different.
#
# DO NOT change --sender-offset (40): it must stay disjoint from our(0)/val1(20),
# or nonces/accounts collide across boxes. Your 20 sender accounts (idx 40..59) are
# pre-funded in the shared genesis (era 1781913600, chain_id 7778).
#
# PREREQ: your box must run the FIXED (session-key) binary + the SAME genesis, or
# session-signed orders are rejected ("session key not found").
set -euo pipefail
BIN="${BIN:-./target/release/bench-throughput}"
RPC="${RPC:-http://localhost:8545}"
SIGN_MODE="${SIGN_MODE:-session}"
echo "[bench FRIEND2] senders 40..59 | sign=$SIGN_MODE | rpc=$RPC" >&2
exec "$BIN" consensus --rpc-urls "$RPC" \
  --senders 20 --sender-offset 40 \
  --duration "${DURATION:-20}" --batch-size "${BATCH:-400}" \
  --submit-batch "${SUBMIT_BATCH:-15}" --rate "${RATE:-30}" --pre-sign "${PRESIGN:-60}" \
  --sign-mode "$SIGN_MODE" --format bin
