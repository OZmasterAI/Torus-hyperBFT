#!/bin/bash
# tx-loop.sh — 8 hardhat accounts send random TRS to each other (~10 tx/sec).
# Usage: bash tx-loop.sh [rpc_url]
# Requires: cast (foundry)
set -o pipefail

EVM_URL="${1:-http://localhost:8545}"
CHAIN_ID=7778
GAS_PRICE="100000000000000"

# 8 hardhat well-known dev keys (NEVER use in production)
KEYS=(
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
    "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
    "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a"
    "0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba"
    "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e"
    "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356"
)
NUM_KEYS=${#KEYS[@]}

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

# Fetch initial nonces.
NONCES=()
for i in "${!ADDRS[@]}"; do
    N=$(cast nonce --rpc-url "$EVM_URL" "${ADDRS[$i]}" 2>/dev/null || echo "0")
    NONCES+=("$N")
done

echo ""
echo "Sending ~10 tx/sec (round-robin across $NUM_KEYS accounts)"
echo "Press Ctrl+C to stop"
echo ""

COUNT=0
ERRORS=0
trap 'echo ""; echo "Stopped after $COUNT tx ($ERRORS errors)."; exit 0' INT

while true; do
    SENDER_IDX=$(( COUNT % NUM_KEYS ))
    RECV_IDX=$(( (SENDER_IDX + 1 + RANDOM % (NUM_KEYS - 1)) % NUM_KEYS ))

    COUNT=$((COUNT + 1))
    TIMESTAMP=$(date +"%H:%M:%S")

    TRS_AMOUNT=$(( RANDOM % 9901 + 100 ))
    WEI_AMOUNT="${TRS_AMOUNT}000000000000000000"

    NONCE="${NONCES[$SENDER_IDX]}"

    S_SHORT="${ADDRS[$SENDER_IDX]:0:8}"
    R_SHORT="${ADDRS[$RECV_IDX]:0:8}"

    TX_HASH=$(cast send \
        --private-key "${KEYS[$SENDER_IDX]}" \
        --rpc-url "$EVM_URL" \
        --chain "$CHAIN_ID" \
        --gas-price "$GAS_PRICE" \
        --nonce "$NONCE" \
        --async \
        "${ADDRS[$RECV_IDX]}" \
        --value "$WEI_AMOUNT" \
        2>&1) || TX_HASH="FAILED"

    if [[ "$TX_HASH" == "FAILED" ]] || [[ "$TX_HASH" == *"rror"* ]]; then
        ERRORS=$((ERRORS + 1))
        FRESH=$(cast nonce --rpc-url "$EVM_URL" "${ADDRS[$SENDER_IDX]}" 2>/dev/null)
        if [ -n "$FRESH" ]; then
            NONCES[$SENDER_IDX]="$FRESH"
        fi
        echo "[$TIMESTAMP] #$COUNT | ${TRS_AMOUNT} TRS | ${S_SHORT}→${R_SHORT} | FAILED (resync nonce→${FRESH})"
    else
        NONCES[$SENDER_IDX]=$(( NONCE + 1 ))
        echo "[$TIMESTAMP] #$COUNT | ${TRS_AMOUNT} TRS | ${S_SHORT}→${R_SHORT} | ${TX_HASH:0:18}..."
    fi

    sleep 0.1
done
