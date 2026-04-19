#!/bin/bash
# tx-loop.sh — 4 hardhat accounts send random TRS to each other (~2 tx/sec).
# Usage: bash tx-loop.sh [rpc_url]
# Requires: cast (foundry)
set -o pipefail

EVM_URL="${1:-http://localhost:8545}"
CHAIN_ID=7778
# ~0.0001 TRS per gas unit → ~2.1 TRS fee per 21000-gas transfer
GAS_PRICE="100000000000000"

# Hardhat well-known dev keys (NEVER use in production)
KEYS=(
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
    "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
)

echo "Deriving addresses..."
ADDRS=()
for i in "${!KEYS[@]}"; do
    ADDR=$(cast wallet address --private-key "${KEYS[$i]}" 2>/dev/null)
    if [ -z "$ADDR" ]; then
        echo "ERROR: cast wallet address failed for key #$i"
        exit 1
    fi
    ADDRS+=("$ADDR")
    echo "  account #$i: $ADDR"
done

echo ""
echo "Checking RPC at $EVM_URL ..."
RPC_CHAIN=$(cast chain-id --rpc-url "$EVM_URL" 2>/dev/null)
if [ -z "$RPC_CHAIN" ]; then
    echo "ERROR: cannot reach RPC at $EVM_URL"
    exit 1
fi
if [ "$RPC_CHAIN" != "$CHAIN_ID" ]; then
    echo "WARNING: expected chain $CHAIN_ID, got $RPC_CHAIN"
fi

BLOCK=$(cast block-number --rpc-url "$EVM_URL" 2>/dev/null || echo "?")
echo "Connected — chain $RPC_CHAIN, block #$BLOCK"

echo ""
echo "Balances:"
for i in "${!ADDRS[@]}"; do
    BAL=$(cast balance --ether --rpc-url "$EVM_URL" "${ADDRS[$i]}" 2>/dev/null || echo "?")
    echo "  #$i ${ADDRS[$i]:0:10}...: $BAL TRS"
done

echo ""
echo "Sending 100-10000 TRS between accounts (1 tx/block)"
echo "Press Ctrl+C to stop"
echo ""

COUNT=0
ERRORS=0
trap 'echo ""; echo "Stopped after $COUNT tx ($ERRORS errors)."; exit 0' INT

while true; do
    COUNT=$((COUNT + 1))
    TIMESTAMP=$(date +"%H:%M:%S")

    SENDER_IDX=$(( RANDOM % 4 ))
    RECV_IDX=$(( (SENDER_IDX + 1 + RANDOM % 3) % 4 ))

    TRS_AMOUNT=$(( RANDOM % 9901 + 100 ))
    WEI_AMOUNT="${TRS_AMOUNT}000000000000000000"

    S_SHORT="${ADDRS[$SENDER_IDX]:0:8}"
    R_SHORT="${ADDRS[$RECV_IDX]:0:8}"

    RESULT=$(cast send \
        --private-key "${KEYS[$SENDER_IDX]}" \
        --rpc-url "$EVM_URL" \
        --chain "$CHAIN_ID" \
        --gas-price "$GAS_PRICE" \
        "${ADDRS[$RECV_IDX]}" \
        --value "$WEI_AMOUNT" \
        --json 2>&1) || RESULT="FAILED"

    if [[ "$RESULT" == "FAILED" ]] || [[ "$RESULT" == *"error"* ]] || [[ "$RESULT" == *"Error"* ]]; then
        ERRORS=$((ERRORS + 1))
        echo "[$TIMESTAMP] #$COUNT | ${TRS_AMOUNT} TRS | ${S_SHORT}→${R_SHORT} | FAILED"
    else
        TX_HASH=$(echo "$RESULT" | grep -o '"transactionHash":"0x[a-f0-9]*"' | head -1 | cut -d'"' -f4)
        echo "[$TIMESTAMP] #$COUNT | ${TRS_AMOUNT} TRS | ${S_SHORT}→${R_SHORT} | ${TX_HASH:0:18}..."
    fi
done
