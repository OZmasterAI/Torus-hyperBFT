#!/bin/bash
# deploy.sh — Deploy AMM contracts (Factory, Router, WETH, test tokens) to Torus devnet
# Usage: bash deploy.sh [rpc_url]
# Outputs: addresses.json with all deployed contract addresses
set -euo pipefail

RPC="${1:-http://localhost:8545}"
CHAIN_ID=7778
DEPLOYER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
DEPLOYER_ADDR="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
GAS_PRICE="100000000000000"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="$SCRIPT_DIR/out"

echo "=== Torus Devnet — AMM Deployment ==="
echo "RPC:      $RPC"
echo "Deployer: $DEPLOYER_ADDR"
echo ""

# Check RPC
CHAIN=$(cast chain-id --rpc-url "$RPC" 2>/dev/null) || { echo "ERROR: RPC unreachable at $RPC"; exit 1; }
echo "Chain ID: $CHAIN"
BLOCK=$(cast block-number --rpc-url "$RPC" 2>/dev/null)
echo "Block:    $BLOCK"
echo ""

deploy() {
    local NAME="$1"
    local BYTECODE="$2"
    shift 2
    local ARGS=("$@")

    echo -n "Deploying $NAME... "
    local RESULT
    RESULT=$(forge create \
        --rpc-url "$RPC" \
        --private-key "$DEPLOYER_KEY" \
        --gas-price "$GAS_PRICE" \
        --priority-gas-price "$GAS_PRICE" \
        --json \
        --root "$SCRIPT_DIR" \
        "$BYTECODE" \
        "${ARGS[@]}" 2>&1) || { echo "FAILED: $RESULT"; exit 1; }

    local ADDR
    ADDR=$(echo "$RESULT" | jq -r '.deployedTo')
    echo "$ADDR"
    echo "$ADDR"
}

# 1. Deploy WETH9
WETH=$(deploy "WETH9" "src/WETH9.sol:WETH9")

# 2. Deploy Factory
FACTORY=$(deploy "Factory" "src/Factory.sol:Factory")

# 3. Deploy Router
ROUTER=$(deploy "Router" "src/Router.sol:Router" --constructor-args "$FACTORY" "$WETH")

# 4. Deploy test tokens (1B initial supply each)
SUPPLY="1000000000000000000000000000"
TOKEN_A=$(deploy "TokenA (ALPHA)" "src/TestToken.sol:TestToken" --constructor-args "Alpha Token" "ALPHA" "$SUPPLY")
TOKEN_B=$(deploy "TokenB (BETA)" "src/TestToken.sol:TestToken" --constructor-args "Beta Token" "BETA" "$SUPPLY")
TOKEN_C=$(deploy "TokenC (GAMMA)" "src/TestToken.sol:TestToken" --constructor-args "Gamma Token" "GAMMA" "$SUPPLY")

echo ""
echo "=== Creating pairs ==="

# Create pairs: ALPHA/WTRS, BETA/WTRS, ALPHA/BETA
cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    "$FACTORY" "createPair(address,address)" "$TOKEN_A" "$WETH" >/dev/null 2>&1 && echo "Pair ALPHA/WTRS created" || echo "Pair ALPHA/WTRS skipped"
cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    "$FACTORY" "createPair(address,address)" "$TOKEN_B" "$WETH" >/dev/null 2>&1 && echo "Pair BETA/WTRS created" || echo "Pair BETA/WTRS skipped"
cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    "$FACTORY" "createPair(address,address)" "$TOKEN_A" "$TOKEN_B" >/dev/null 2>&1 && echo "Pair ALPHA/BETA created" || echo "Pair ALPHA/BETA skipped"

PAIR_A_WETH=$(cast call --rpc-url "$RPC" "$FACTORY" "getPair(address,address)(address)" "$TOKEN_A" "$WETH")
PAIR_B_WETH=$(cast call --rpc-url "$RPC" "$FACTORY" "getPair(address,address)(address)" "$TOKEN_B" "$WETH")
PAIR_A_B=$(cast call --rpc-url "$RPC" "$FACTORY" "getPair(address,address)(address)" "$TOKEN_A" "$TOKEN_B")

echo "  ALPHA/WTRS: $PAIR_A_WETH"
echo "  BETA/WTRS:  $PAIR_B_WETH"
echo "  ALPHA/BETA: $PAIR_A_B"

echo ""
echo "=== Adding initial liquidity ==="

# Approve tokens for router (max uint256)
MAX_UINT="115792089237316195423570985008687907853269984665640564039457584007913129639935"
for TOKEN in "$TOKEN_A" "$TOKEN_B" "$TOKEN_C"; do
    cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
        "$TOKEN" "approve(address,uint256)" "$ROUTER" "$MAX_UINT" >/dev/null 2>&1
done
echo "Tokens approved for Router"

# Add liquidity: 10M ALPHA + 100 WTRS
AMOUNT_TOKEN="10000000000000000000000000"
AMOUNT_ETH="100000000000000000000"
cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    --value "$AMOUNT_ETH" \
    "$ROUTER" "addLiquidityETH(address,uint256,uint256,uint256,address,uint256)" \
    "$TOKEN_A" "$AMOUNT_TOKEN" "0" "0" "$DEPLOYER_ADDR" "99999999999" >/dev/null 2>&1 && echo "Liquidity added: 10M ALPHA + 100 WTRS" || echo "FAILED: ALPHA/WTRS liquidity"

# Add liquidity: 10M BETA + 100 WTRS
cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    --value "$AMOUNT_ETH" \
    "$ROUTER" "addLiquidityETH(address,uint256,uint256,uint256,address,uint256)" \
    "$TOKEN_B" "$AMOUNT_TOKEN" "0" "0" "$DEPLOYER_ADDR" "99999999999" >/dev/null 2>&1 && echo "Liquidity added: 10M BETA + 100 WTRS" || echo "FAILED: BETA/WTRS liquidity"

# Add liquidity: 5M ALPHA + 5M BETA
AMOUNT_HALF="5000000000000000000000000"
cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
    "$ROUTER" "addLiquidity(address,address,uint256,uint256,uint256,uint256,address,uint256)" \
    "$TOKEN_A" "$TOKEN_B" "$AMOUNT_HALF" "$AMOUNT_HALF" "0" "0" "$DEPLOYER_ADDR" "99999999999" >/dev/null 2>&1 && echo "Liquidity added: 5M ALPHA + 5M BETA" || echo "FAILED: ALPHA/BETA liquidity"

echo ""
echo "=== Distributing tokens to test accounts ==="

# Distribute tokens to accounts 1-9 for load testing
DIST_AMOUNT="100000000000000000000000000"
TEST_KEYS=(
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
TEST_ADDRS=(
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

for ADDR in "${TEST_ADDRS[@]}"; do
    for TOKEN in "$TOKEN_A" "$TOKEN_B" "$TOKEN_C"; do
        cast send --private-key "$DEPLOYER_KEY" --rpc-url "$RPC" --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" \
            "$TOKEN" "mint(address,uint256)" "$ADDR" "$DIST_AMOUNT" >/dev/null 2>&1
    done
done
echo "Distributed 100M of each token to 9 test accounts"

echo ""
echo "=== Deployment Complete ==="

# Save addresses
cat > "$SCRIPT_DIR/addresses.json" << EOF
{
    "rpc": "$RPC",
    "chainId": $CHAIN_ID,
    "deployer": "$DEPLOYER_ADDR",
    "weth": "$WETH",
    "factory": "$FACTORY",
    "router": "$ROUTER",
    "tokenA": "$TOKEN_A",
    "tokenB": "$TOKEN_B",
    "tokenC": "$TOKEN_C",
    "pairs": {
        "ALPHA_WTRS": "$PAIR_A_WETH",
        "BETA_WTRS": "$PAIR_B_WETH",
        "ALPHA_BETA": "$PAIR_A_B"
    }
}
EOF
echo "Addresses saved to $SCRIPT_DIR/addresses.json"
echo ""
cat "$SCRIPT_DIR/addresses.json"
