#!/bin/bash
# ci-swap-check.sh — D6 (S392) e2e assertion: swap on the freshly deployed AMM
# and verify balances + receipts. Run after deploy.sh against the same node.
# Usage: bash ci-swap-check.sh [rpc_url]
set -euo pipefail

RPC="${1:-http://localhost:8545}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ADDR_FILE="$SCRIPT_DIR/addresses.json"

# hardhat account 1 — funded with tokens by deploy.sh's distribution step.
TRADER_KEY="0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
TRADER="0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
GAS_PRICE="100000000000000"

ROUTER=$(jq -r '.router' "$ADDR_FILE")
TOKEN_A=$(jq -r '.tokenA' "$ADDR_FILE")
TOKEN_B=$(jq -r '.tokenB' "$ADDR_FILE")
MULTICALL3=$(jq -r '.multicall3 // empty' "$ADDR_FILE")

echo "=== D6 swap check ==="
BAL_B_BEFORE=$(cast call --rpc-url "$RPC" "$TOKEN_B" "balanceOf(address)(uint256)" "$TRADER" | awk '{print $1}')
echo "BETA before: $BAL_B_BEFORE"

# Approve + swap 1000 ALPHA -> BETA through the ALPHA/BETA pool.
cast send --private-key "$TRADER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    "$TOKEN_A" "approve(address,uint256)" "$ROUTER" "1000000000000000000000" >/dev/null

TX_JSON=$(cast send --private-key "$TRADER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --json \
    "$ROUTER" "swapExactTokensForTokens(uint256,uint256,address[],address,uint256)" \
    "1000000000000000000000" "0" "[$TOKEN_A,$TOKEN_B]" "$TRADER" "99999999999")
STATUS=$(echo "$TX_JSON" | jq -r '.status')
TX_HASH=$(echo "$TX_JSON" | jq -r '.transactionHash')
echo "swap tx: $TX_HASH status: $STATUS"
[ "$STATUS" = "0x1" ] || { echo "FAIL: swap receipt status != 1"; exit 1; }

BAL_B_AFTER=$(cast call --rpc-url "$RPC" "$TOKEN_B" "balanceOf(address)(uint256)" "$TRADER" | awk '{print $1}')
echo "BETA after:  $BAL_B_AFTER"
python3 -c "import sys; sys.exit(0 if int('$BAL_B_AFTER') > int('$BAL_B_BEFORE') else 1)" \
    || { echo "FAIL: BETA balance did not increase"; exit 1; }

# Receipt round-trip: eth_getTransactionReceipt resolves the swap by hash (D5).
RCPT_HASH=$(cast receipt --rpc-url "$RPC" "$TX_HASH" --json | jq -r '.transactionHash')
[ "$RCPT_HASH" = "$TX_HASH" ] || { echo "FAIL: receipt hash mismatch"; exit 1; }

# Multicall3, if deployed: exercise a call through the canonical address.
if [ -n "$MULTICALL3" ] && [ "$(cast code --rpc-url "$RPC" "$MULTICALL3" 2>/dev/null)" != "0x" ]; then
    BLK=$(cast call --rpc-url "$RPC" "$MULTICALL3" "getBlockNumber()(uint256)")
    echo "Multicall3 live at $MULTICALL3, getBlockNumber: $BLK"
else
    echo "WARN: Multicall3 not deployed — skipping its check"
fi

echo "=== D6 swap check PASSED ==="
