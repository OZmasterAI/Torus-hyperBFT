#!/bin/bash
# tx-loop.sh — 20 hardhat accounts send random TRS to each other in parallel.
# Usage: bash tx-loop.sh [rpc_url] [batch_size]
# Requires: cast (foundry)
set -o pipefail

EVM_URL="${1:-http://localhost:8545}"
CHAIN_ID=7778
GAS_PRICE="100000000000000"

# 20 hardhat well-known dev keys (NEVER use in production)
KEYS=(
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
    "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
    "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a"
    "0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba"
    "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e"
    "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356"
    "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97"
    "0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6"
    "0xf214f2b2cd398c806f84e317254e0f0b801d0643303237d97a22a48e01628897"
    "0x701b615bbdfb9de65240bc28bd21bbc0d996645a3dd57e7b12bc2bdf6f192c82"
    "0xa267530f49f8280200edf313ee7af6b827f2a8bce2897751d06a843f644967b1"
    "0x47c99abed3324a2707c28affff1267e45918ec8c3f20b8aa892e8b065d2942dd"
    "0xc526ee95bf44d8fc405a158bb884d9d1238d99f0612e9f33d006bb0789009aaa"
    "0x8166f546bab6da521a8369cab06c5d2b9e46670292d85c875ee9ec20e84ffb61"
    "0xea6c44ac03bff858b476bba40716402b03e41b8e97e276d1baec7c37d42484a0"
    "0x689af8efa8c651a91ad287602527f3af2fe9f6501a7ac4b061667b5a93e037fd"
    "0xde9be858da4a475276426320d5e9262ecfc3ba460bfac56360bfa6c4c28b4ee0"
    "0xdf57089febbacf7ba0bc227dafbffa9fc08a93fdc68e1e42411a14efcf23656e"
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
echo "Sending steady stream across $NUM_KEYS accounts (1 tx every ~30ms)"
echo "Press Ctrl+C to stop"
echo ""

COUNT=0
ERRORS=0
BG_COUNT=0
MAX_BG=20
trap 'wait 2>/dev/null; echo ""; echo "Stopped after $COUNT tx ($ERRORS errors)."; exit 0' INT

while true; do
    SENDER_IDX=$(( COUNT % NUM_KEYS ))
    RECV_IDX=$(( (SENDER_IDX + 1 + RANDOM % (NUM_KEYS - 1)) % NUM_KEYS ))

    TRS_AMOUNT=$(( RANDOM % 91 + 10 ))
    WEI_AMOUNT="${TRS_AMOUNT}000000000000000000"

    NONCE="${NONCES[$SENDER_IDX]}"
    NONCES[$SENDER_IDX]=$(( NONCE + 1 ))

    S_SHORT="${ADDRS[$SENDER_IDX]:0:8}"
    R_SHORT="${ADDRS[$RECV_IDX]:0:8}"
    COUNT=$((COUNT + 1))

    (
        TX_HASH=$(cast send \
            --private-key "${KEYS[$SENDER_IDX]}" \
            --rpc-url "$EVM_URL" \
            --chain "$CHAIN_ID" \
            --gas-price "$GAS_PRICE" \
            --priority-gas-price "$GAS_PRICE" \
            --gas-limit 21000 \
            --nonce "$NONCE" \
            --async \
            "${ADDRS[$RECV_IDX]}" \
            --value "$WEI_AMOUNT" \
            2>&1) || TX_HASH="FAILED"

        TS=$(date +"%H:%M:%S")
        if [[ "$TX_HASH" == "FAILED" ]] || [[ "$TX_HASH" == *"rror"* ]]; then
            echo "[$TS] #$COUNT | ${TRS_AMOUNT} TRS | ${S_SHORT}→${R_SHORT} | FAILED"
        else
            echo "[$TS] #$COUNT | ${TRS_AMOUNT} TRS | ${S_SHORT}→${R_SHORT} | ${TX_HASH:0:18}..."
        fi
    ) &

    BG_COUNT=$((BG_COUNT + 1))
    if (( BG_COUNT >= MAX_BG )); then
        wait
        BG_COUNT=0
    fi

    # Resync nonces every full rotation through all accounts.
    # Only advance — never rewind, since the mempool may be ahead of confirmed state.
    if (( COUNT % (NUM_KEYS * 2) == 0 )); then
        wait
        BG_COUNT=0
        for i in "${!ADDRS[@]}"; do
            N=$(cast nonce --rpc-url "$EVM_URL" "${ADDRS[$i]}" 2>/dev/null || echo "${NONCES[$i]}")
            if (( N > NONCES[$i] )); then
                NONCES[$i]="$N"
            fi
        done
    fi

    sleep 0.03
done
