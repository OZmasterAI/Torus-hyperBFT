#!/bin/bash
# e2e-local.sh: the Uniswap-V2 end-to-end check (D6, S392) run locally. It was
# the `uniswap-e2e` job of the removed GitHub CI (s80).
#
# What it does:
# - builds torus-node (debug) and starts a throwaway 1-validator devnet;
# - deploys Factory/Router/tokens + Multicall3, adds liquidity, swaps, and
#   asserts balances and receipts (deploy.sh + ci-swap-check.sh);
# - stops the node on exit, pass or fail.
#
# The node uses non-default ports so it cannot clash with a running node. The
# contract scripts run from a temporary copy of this directory, so their
# out/ and addresses.json never land in the repo.
#
# Needs: foundry (forge, cast), jq, python3.
#
# Usage: devnet/uniswap/e2e-local.sh [rpc_port] [p2p_port]   (default 18545 30933)
# Set CARGO_TARGET_DIR to build into a per-worktree target.
set -euo pipefail

RPC_PORT="${1:-18545}"
P2P_PORT="${2:-30933}"
RPC="http://127.0.0.1:$RPC_PORT"

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$REPO/target}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/torus-uniswap-e2e.XXXXXX")"
NODE_PID=""

cleanup() {
    local rc=$?
    if [ -n "$NODE_PID" ] && kill -0 "$NODE_PID" 2>/dev/null; then
        kill "$NODE_PID" 2>/dev/null || true
        wait "$NODE_PID" 2>/dev/null || true
    fi
    if [ "$rc" -ne 0 ]; then
        echo "=== FAILED (rc=$rc); last node log lines:"
        tail -150 "${WORK:?}/node.log" 2>/dev/null || true
        echo "=== work dir kept: $WORK"
    else
        rm -r -- "${WORK:?}"
    fi
    exit "$rc"
}
trap cleanup EXIT

for tool in forge cast jq python3; do
    command -v "$tool" >/dev/null || { echo "missing: $tool"; exit 1; }
done
if ss -ltn 2>/dev/null | grep -q ":$RPC_PORT\b"; then
    echo "port $RPC_PORT is in use; pass another rpc_port"
    exit 1
fi

echo "=== build torus-node (debug)"
(cd "$REPO" && cargo build -p torus-node)

echo "=== start 1-validator devnet on $RPC (p2p $P2P_PORT)"
python3 "$REPO/devnet/uniswap/make-ci-genesis.py" "$REPO/devnet/genesis.json" "$WORK/genesis.json"
TORUS_EVM_BLOCK_GAS_BUDGET=15000000 "$TARGET/debug/torus-node" \
    --genesis "$WORK/genesis.json" \
    --data-dir "$WORK/data" \
    --validator-key 0100000000000000000000000000000000000000000000000000000000000000 \
    --p2p-listen "/ip4/127.0.0.1/udp/$P2P_PORT/quic-v1" \
    --rpc-addr "127.0.0.1:$RPC_PORT" \
    --log-level info > "$WORK/node.log" 2>&1 &
NODE_PID=$!
for _ in $(seq 1 60); do
    if cast block-number --rpc-url "$RPC" >/dev/null 2>&1; then break; fi
    kill -0 "$NODE_PID" 2>/dev/null || { echo "node exited early"; exit 1; }
    sleep 2
done
cast chain-id --rpc-url "$RPC"

echo "=== deploy Uniswap V2 + Multicall3"
cp -r "$REPO/devnet/uniswap" "$WORK/uniswap"
bash "$WORK/uniswap/deploy.sh" "$RPC"

echo "=== swap and assert"
bash "$WORK/uniswap/ci-swap-check.sh" "$RPC"

echo "=== PASS"
