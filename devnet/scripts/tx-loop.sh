#!/bin/bash
# tx-loop.sh — N funded accounts send random TRS to each other in parallel.
# Usage: bash tx-loop.sh [rpc_url]
#   env: NUM_ACCOUNTS (default 100)  ACCT_OFFSET (default 60)  BIN (bench binary)
# Requires: cast (foundry), a built bench-throughput binary.
#
# Senders are derived from the bench's FUNDED bulk-account range via
# `bench-throughput gen-accounts --secret-keys` (same derivation the genesis
# funded), so every sender provably has an EVM balance. This replaces the old
# hardcoded hardhat keys, most of which are unfunded on the live weighted
# genesis (that caused the FAILED-on-insufficient-funds txs).
set -o pipefail

EVM_URL="${1:-http://localhost:8545}"
CHAIN_ID=7778
GAS_PRICE="100000000000000"

# Resolve the bench binary relative to the repo root (this script lives in
# devnet/scripts/), so it works regardless of the caller's cwd.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${BIN:-$REPO_ROOT/target/release/bench-throughput}"
NUM_ACCOUNTS="${NUM_ACCOUNTS:-100}"   # how many funded senders to load
ACCT_OFFSET="${ACCT_OFFSET:-60}"      # first bench sender index; bulk-funded range starts at 60

if [ ! -x "$BIN" ]; then
    echo "ERROR: bench binary not found/executable: $BIN"
    echo "       build it: cargo build --release -p bench-throughput"
    exit 1
fi

echo "Loading $NUM_ACCOUNTS funded senders from bench gen-accounts (offset $ACCT_OFFSET)..."
KEYS=()
ADDRS=()
while read -r _idx addr key; do
    [ -n "$addr" ] && [ -n "$key" ] || continue
    ADDRS+=("$addr")
    KEYS+=("$key")
done < <("$BIN" gen-accounts --offset "$ACCT_OFFSET" --count "$NUM_ACCOUNTS" --secret-keys 2>/dev/null)
NUM_KEYS=${#KEYS[@]}
if [ "$NUM_KEYS" -lt 2 ]; then
    echo "ERROR: gen-accounts produced $NUM_KEYS keys (need >=2)."
    echo "       Is $BIN built with the --secret-keys flag? (rebuild if not)"
    exit 1
fi
echo "Loaded $NUM_KEYS funded senders (bench idx $ACCT_OFFSET..$((ACCT_OFFSET + NUM_KEYS - 1)))"

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
echo "Balances (first 5 of $NUM_KEYS senders):"
for i in "${!ADDRS[@]}"; do
    [ "$i" -ge 5 ] && break
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
