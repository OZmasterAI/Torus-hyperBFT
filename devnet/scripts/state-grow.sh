#!/usr/bin/env bash
# state-grow.sh — grow REAL EVM state on the Torus devnet by sending tiny value
# transfers to FRESH, never-before-seen recipient addresses. Every fresh
# recipient is a brand-new account in CF_ACCOUNTS + the incremental trie, so the
# total account count (and data-dir size / state-root full-scan cost) grows
# MONOTONICALLY — exactly the "large pre-grown state" the A1.6 flatness proof
# needs (not churn on a fixed sender set).
#
# Senders are the genesis-funded hardhat accounts (0..99, from
# /tmp/hardhat_accounts.txt). Recipients are synthesized from a high, collision-
# free keyspace (0xcafe... + a global counter) so they can never be precompiles
# (addrs 0x01..0x09) and never repeat across stages (pass a rising OFFSET).
#
# Uses host `cast` (foundry) exactly like tx-loop.sh — cast is a plain RPC client
# here, no devnet-image glibc constraint. Targets ONLY the devnet RPCs; 8545
# (the live testnet on this host) is never touched.
#
# Usage / env:
#   DURATION=120 OFFSET=0 SENDERS=40 \
#   RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548" \
#     bash state-grow.sh
# Prints periodic progress (submitted / accepted / height) and, at the end, the
# global counter reached (feed OFFSET = that value into the next stage).
#
# Syntax gate: bash -n devnet/scripts/state-grow.sh
set -uo pipefail

DURATION=${DURATION:-120}
OFFSET=${OFFSET:-0}
SENDERS=${SENDERS:-40}
VALUE=${VALUE:-1000000000}          # 1 gwei — trivial, senders are richly funded
CHAIN_ID=${CHAIN_ID:-7778}
GAS_PRICE=${GAS_PRICE:-100000000000000}
MAX_BG=${MAX_BG:-64}    # cast-per-tx is spawn-bound; pipeline this many in flight
RECIP_PREFIX=${RECIP_PREFIX:-cafe}  # 4 hex; + 36 hex counter = 20-byte address
ACCOUNTS_FILE=${ACCOUNTS_FILE:-/tmp/hardhat_accounts.txt}
RPCS=${RPCS:-"http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548"}

command -v cast >/dev/null 2>&1 || { echo "state-grow: cast not found on PATH" >&2; exit 1; }
[ -r "$ACCOUNTS_FILE" ] || { echo "state-grow: $ACCOUNTS_FILE missing" >&2; exit 1; }

IFS=',' read -r -a EP <<<"$RPCS"
# Guard: refuse to ever touch the live testnet RPC (8545).
for e in "${EP[@]}"; do
    case "$e" in
        *:8545|*:8545/*) echo "state-grow: REFUSING to target testnet RPC $e" >&2; exit 1 ;;
    esac
done

# Load funded sender keys + addresses (idx|privkey|address).
KEYS=(); ADDRS=()
while IFS='|' read -r idx key addr; do
    [ -n "${key:-}" ] || continue
    KEYS+=("$key"); ADDRS+=("$addr")
    [ "${#KEYS[@]}" -ge "$SENDERS" ] && break
done <"$ACCOUNTS_FILE"
NK=${#KEYS[@]}
[ "$NK" -gt 0 ] || { echo "state-grow: no sender keys loaded" >&2; exit 1; }

recip() { # $1=global counter -> 0x + prefix + zero-padded counter (20 bytes)
    printf '0x%s%036x' "$RECIP_PREFIX" "$1"
}
height() {
    cast block-number --rpc-url "${EP[0]}" 2>/dev/null || echo 0
}

echo "=== state-grow: senders=$NK duration=${DURATION}s offset=$OFFSET endpoints=${#EP[@]} ==="
echo "    recipient base $(recip "$OFFSET")  value=$VALUE wei each"

# Seed local per-sender nonces from chain (never rewind — mempool may lead).
NONCES=()
for a in "${ADDRS[@]}"; do
    NONCES+=("$(cast nonce --rpc-url "${EP[0]}" "$a" 2>/dev/null || echo 0)")
done

start=$SECONDS
submitted=0
counter=$OFFSET
bg=0
h0=$(height)
last_report=$SECONDS

while [ $((SECONDS - start)) -lt "$DURATION" ]; do
    sidx=$(( submitted % NK ))
    ep=${EP[$(( submitted % ${#EP[@]} ))]}
    to=$(recip "$counter")
    nonce=${NONCES[$sidx]}
    NONCES[$sidx]=$(( nonce + 1 ))
    counter=$(( counter + 1 ))
    submitted=$(( submitted + 1 ))

    cast send --private-key "${KEYS[$sidx]}" --rpc-url "$ep" --chain "$CHAIN_ID" \
        --gas-price "$GAS_PRICE" --priority-gas-price "$GAS_PRICE" --gas-limit 21000 \
        --nonce "$nonce" --async "$to" --value "$VALUE" >/dev/null 2>&1 &

    # Pipeline: keep ~MAX_BG casts in flight, reaping ONE finished job at a time
    # (`wait -n`) instead of draining the whole batch — cast process-spawn is the
    # bottleneck, so continuous pipelining lifts the submit rate well above a
    # batch-drain loop (falls back to `wait` on pre-4.3 bash without `wait -n`).
    bg=$(( bg + 1 ))
    if [ "$bg" -ge "$MAX_BG" ]; then
        if wait -n 2>/dev/null; then bg=$(( bg - 1 )); else wait; bg=0; fi
    fi

    # Periodic resync (advance-only) + progress line.
    if [ $((SECONDS - last_report)) -ge 5 ]; then
        for i in "${!ADDRS[@]}"; do
            n=$(cast nonce --rpc-url "${EP[0]}" "${ADDRS[$i]}" 2>/dev/null || echo "${NONCES[$i]}")
            [ "$n" -gt "${NONCES[$i]}" ] && NONCES[$i]="$n"
        done
        printf 'state-grow %4ds | submitted=%-7d counter=%-9d height=%s\n' \
            "$((SECONDS - start))" "$submitted" "$counter" "$(height)"
        last_report=$SECONDS
    fi
done
wait
h1=$(height)
echo "=== state-grow done: submitted=$submitted new-recipients=$((counter - OFFSET)) height ${h0}->${h1} ==="
echo "NEXT_OFFSET=$counter"
