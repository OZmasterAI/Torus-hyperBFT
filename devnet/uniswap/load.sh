#!/bin/bash
# load.sh — DeFi load generator: random swaps, LP adds/removes on Torus devnet
# Usage: bash load.sh [addresses.json] [rpc_url]
# Reads addresses from deploy.sh output. Runs until Ctrl+C.
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ADDR_FILE="${1:-$SCRIPT_DIR/addresses.json}"
RPC="${2:-http://localhost:8545}"
CHAIN_ID=7778
GAS_PRICE="100000000000000"
MAX_BG=10

if [ ! -f "$ADDR_FILE" ]; then
    echo "ERROR: addresses.json not found at $ADDR_FILE"
    echo "Run deploy.sh first."
    exit 1
fi

ROUTER=$(jq -r '.router' "$ADDR_FILE")
WETH=$(jq -r '.weth' "$ADDR_FILE")
TOKEN_A=$(jq -r '.tokenA' "$ADDR_FILE")
TOKEN_B=$(jq -r '.tokenB' "$ADDR_FILE")
TOKEN_C=$(jq -r '.tokenC' "$ADDR_FILE")
FACTORY=$(jq -r '.factory' "$ADDR_FILE")

# Test accounts 1-9 (account 0 is deployer)
KEYS=(
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
    "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
    "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a"
    "0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba"
    "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e"
    "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356"
    "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97"
    "0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6"
)
ADDRS=(
    "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
    "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
    "0x90F79bf6EB2c4f870365E785982E1f101E93b906"
    "0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65"
    "0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc"
    "0x976EA74026E726554dB657fA54763abd0C3a0aa9"
    "0x14dC79964da2C08dda4c5d09d47B49dc8de5F3e2"
    "0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f"
    "0xa0Ee7A142d267C1f36714E4a8F75612F20a79720"
)
NUM_ACCOUNTS=${#KEYS[@]}
TOKENS=("$TOKEN_A" "$TOKEN_B" "$TOKEN_C")
DEADLINE="99999999999"

echo "=== Torus DeFi Load Generator ==="
echo "RPC:     $RPC"
echo "Router:  $ROUTER"
echo "WETH:    $WETH"
echo "Tokens:  ALPHA=$TOKEN_A"
echo "         BETA=$TOKEN_B"
echo "         GAMMA=$TOKEN_C"
echo ""

# Approve all tokens for all accounts
echo "Setting up approvals..."
MAX_UINT="115792089237316195423570985008687907853269984665640564039457584007913129639935"
for i in "${!KEYS[@]}"; do
    for TOKEN in "${TOKENS[@]}"; do
        cast send --private-key "${KEYS[$i]}" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
            "$TOKEN" "approve(address,uint256)" "$ROUTER" "$MAX_UINT" >/dev/null 2>&1 &
    done
done
wait
echo "All approvals set"
echo ""
echo "Running DeFi load (swaps, LP adds/removes). Press Ctrl+C to stop."
echo ""

COUNT=0
SWAPS=0
LPS=0
ERRORS=0
BG_COUNT=0
trap 'wait 2>/dev/null; echo ""; echo "Stopped: $COUNT ops ($SWAPS swaps, $LPS LP ops, $ERRORS errors)"; exit 0' INT

while true; do
    ACCT_IDX=$(( RANDOM % NUM_ACCOUNTS ))
    KEY="${KEYS[$ACCT_IDX]}"
    ADDR="${ADDRS[$ACCT_IDX]}"
    ACCT_SHORT="${ADDR:0:8}"
    TS=$(date +"%H:%M:%S")

    # Pick random operation: 70% swap, 15% LP add, 15% LP remove
    OP=$(( RANDOM % 100 ))

    if (( OP < 70 )); then
        # === SWAP ===
        # Pick random token pair and direction
        PAIR_TYPE=$(( RANDOM % 4 ))
        SWAP_AMT=$(( (RANDOM % 900 + 100) ))
        SWAP_WEI="${SWAP_AMT}000000000000000000"

        case $PAIR_TYPE in
            0) # ALPHA -> WTRS (swap token for ETH)
                FROM_TOKEN="$TOKEN_A"
                (
                    cast send --private-key "$KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 300000 \
                        "$ROUTER" "swapExactTokensForETH(uint256,uint256,address[],address,uint256)" \
                        "$SWAP_WEI" "0" "[$TOKEN_A,$WETH]" "$ADDR" "$DEADLINE" >/dev/null 2>&1 \
                    && echo "[$TS] SWAP  | ${SWAP_AMT} ALPHA→TRS | $ACCT_SHORT" \
                    || echo "[$TS] SWAP  | FAILED ALPHA→TRS | $ACCT_SHORT"
                ) &
                ;;
            1) # WTRS -> BETA (swap ETH for token)
                ETH_AMT=$(( (RANDOM % 5 + 1) ))
                ETH_WEI="${ETH_AMT}000000000000000000"
                (
                    cast send --private-key "$KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 300000 \
                        --value "$ETH_WEI" \
                        "$ROUTER" "swapExactETHForTokens(uint256,address[],address,uint256)" \
                        "0" "[$WETH,$TOKEN_B]" "$ADDR" "$DEADLINE" >/dev/null 2>&1 \
                    && echo "[$TS] SWAP  | ${ETH_AMT} TRS→BETA | $ACCT_SHORT" \
                    || echo "[$TS] SWAP  | FAILED TRS→BETA | $ACCT_SHORT"
                ) &
                ;;
            2) # ALPHA -> BETA (token-to-token)
                (
                    cast send --private-key "$KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 300000 \
                        "$ROUTER" "swapExactTokensForTokens(uint256,uint256,address[],address,uint256)" \
                        "$SWAP_WEI" "0" "[$TOKEN_A,$TOKEN_B]" "$ADDR" "$DEADLINE" >/dev/null 2>&1 \
                    && echo "[$TS] SWAP  | ${SWAP_AMT} ALPHA→BETA | $ACCT_SHORT" \
                    || echo "[$TS] SWAP  | FAILED ALPHA→BETA | $ACCT_SHORT"
                ) &
                ;;
            3) # BETA -> ALPHA
                (
                    cast send --private-key "$KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 300000 \
                        "$ROUTER" "swapExactTokensForTokens(uint256,uint256,address[],address,uint256)" \
                        "$SWAP_WEI" "0" "[$TOKEN_B,$TOKEN_A]" "$ADDR" "$DEADLINE" >/dev/null 2>&1 \
                    && echo "[$TS] SWAP  | ${SWAP_AMT} BETA→ALPHA | $ACCT_SHORT" \
                    || echo "[$TS] SWAP  | FAILED BETA→ALPHA | $ACCT_SHORT"
                ) &
                ;;
        esac
        SWAPS=$((SWAPS + 1))

    elif (( OP < 85 )); then
        # === ADD LIQUIDITY (ETH pair) ===
        LP_TOKEN_IDX=$(( RANDOM % 2 ))
        LP_TOKEN="${TOKENS[$LP_TOKEN_IDX]}"
        LP_NAME=$([ "$LP_TOKEN_IDX" -eq 0 ] && echo "ALPHA" || echo "BETA")
        TOKEN_AMT="10000000000000000000000"
        ETH_AMT="1000000000000000000"
        (
            cast send --private-key "$KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 500000 \
                --value "$ETH_AMT" \
                "$ROUTER" "addLiquidityETH(address,uint256,uint256,uint256,address,uint256)" \
                "$LP_TOKEN" "$TOKEN_AMT" "0" "0" "$ADDR" "$DEADLINE" >/dev/null 2>&1 \
            && echo "[$TS] LP+   | 10K $LP_NAME + 1 TRS | $ACCT_SHORT" \
            || echo "[$TS] LP+   | FAILED $LP_NAME/TRS | $ACCT_SHORT"
        ) &
        LPS=$((LPS + 1))

    else
        # === ADD LIQUIDITY (token/token pair) ===
        TOKEN_AMT="5000000000000000000000"
        (
            cast send --private-key "$KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 500000 \
                "$ROUTER" "addLiquidity(address,address,uint256,uint256,uint256,uint256,address,uint256)" \
                "$TOKEN_A" "$TOKEN_B" "$TOKEN_AMT" "$TOKEN_AMT" "0" "0" "$ADDR" "$DEADLINE" >/dev/null 2>&1 \
            && echo "[$TS] LP+   | 5K ALPHA + 5K BETA | $ACCT_SHORT" \
            || echo "[$TS] LP+   | FAILED ALPHA/BETA | $ACCT_SHORT"
        ) &
        LPS=$((LPS + 1))
    fi

    COUNT=$((COUNT + 1))
    BG_COUNT=$((BG_COUNT + 1))
    if (( BG_COUNT >= MAX_BG )); then
        wait
        BG_COUNT=0
    fi

    sleep 0.1
done
