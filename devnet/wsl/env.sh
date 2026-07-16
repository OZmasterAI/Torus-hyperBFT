#!/usr/bin/env bash
# env.sh — shared topology for the WSL bare-metal 3-validator devnet.
# Sourced by launch/stop/health scripts. Localhost only. NO docker, NO remote.
#
# Host ports are chosen to avoid ANY collision with a live testnet node
# (which conventionally holds 8545 / 30333 / 9090).
#
# Validator keys are the deterministic devnet keys (INSECURE — devnet only).
# The libp2p PeerId is derived from the same ed25519 key, so the ids below are
# fixed. val0/val1 ids are taken from devnet/docker-compose.yml (same keys);
# val2 dials out to val0+val1 so no node needs val2's id up front.

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BIN="$REPO/target/release/torus-node"
GENESIS="$REPO/devnet/wsl/genesis-3val.json"
DATA_ROOT="${DATA_ROOT:-$HOME/torus-wsl-devnet}"
RUN_DIR="$DATA_ROOT/run"       # pids + logs

# validator hex keys (32-byte ed25519 seed)
KEY0=0100000000000000000000000000000000000000000000000000000000000000
KEY1=0200000000000000000000000000000000000000000000000000000000000000
KEY2=0300000000000000000000000000000000000000000000000000000000000000

# libp2p peer ids (deterministic from KEY0/KEY1)
PID0=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
PID1=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3

# per-node ports
P2P0=30401; RPC0=8645; MET0=9161
P2P1=30402; RPC1=8646; MET1=9162
P2P2=30403; RPC2=8647; MET2=9163

# dial edges (localhost multiaddrs)
PEER_TO_V0=/ip4/127.0.0.1/udp/$P2P0/quic-v1/p2p/$PID0
PEER_TO_V1=/ip4/127.0.0.1/udp/$P2P1/quic-v1/p2p/$PID1

# node runtime env — mirror the S415/S458 devnet body-push tuning so a later
# bench run (A3) executes against the same consensus config as idle bring-up.
export TORUS_HASH_ONLY_PUSH_THRESHOLD="${TORUS_HASH_ONLY_PUSH_THRESHOLD:-6000000}"
export TORUS_NATIVE_TOTAL_BLOCK_CAP="${TORUS_NATIVE_TOTAL_BLOCK_CAP:-100}"

METRICS_PORTS="$MET0 $MET1 $MET2"
RPC_URLS="http://localhost:$RPC0 http://localhost:$RPC1 http://localhost:$RPC2"
