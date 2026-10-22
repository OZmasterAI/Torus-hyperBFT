#!/usr/bin/env bash
# env.sh — shared topology for the WSL bare-metal 3-validator devnet (4 with
# N_VALIDATORS=4, see below). Sourced by launch/stop/health scripts.
# Localhost only. NO docker, NO remote.
#
# Host ports are chosen to avoid ANY collision with a live testnet node
# (which conventionally holds 8545 / 30333 / 9090).
#
# Validator keys are the deterministic devnet keys (INSECURE — devnet only).
# The libp2p PeerId is derived from the same ed25519 key, so the ids below are
# fixed. val0/val1 ids are taken from devnet/docker-compose.yml (same keys);
# val2 dials out to val0+val1 so no node needs val2's id up front.
#
# N_VALIDATORS=3 (default) | 4: how many validators launch-3val.sh starts
# (tools/matched-bench/run-cell.sh N_VALIDATORS; the 4-validator devnet of the
# dead-leader bench 9p, owner s138). val3 = KEY3 (devnet/genesis.json's 4th
# validator pubkey), ports 30404 / 8648 / 9164, and it dials val0+val1+val2
# (PID2, so the 4 nodes form a full mesh). Everything N=3 reads is unchanged.

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BIN="$REPO/target/release/torus-node"
GENESIS="$REPO/devnet/wsl/genesis-3val.json"
DATA_ROOT="${DATA_ROOT:-$HOME/torus-wsl-devnet}"
RUN_DIR="$DATA_ROOT/run"       # pids + logs

# validator hex keys (32-byte ed25519 seed)
KEY0=0100000000000000000000000000000000000000000000000000000000000000
KEY1=0200000000000000000000000000000000000000000000000000000000000000
KEY2=0300000000000000000000000000000000000000000000000000000000000000
KEY3=0400000000000000000000000000000000000000000000000000000000000000

# libp2p peer ids (deterministic from KEY0/KEY1)
PID0=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
PID1=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3
# (KEY2/KEY3: base58(identity multihash of the protobuf ed25519 public key);
# only val3 dials val2, PID3 is for reference)
PID2=12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo
PID3=12D3KooWLJtG8fd2hkQzTn96MrLvThmnNQjTUFZwGEsLRz5EmSzc

# per-node ports
P2P0=30401; RPC0=8645; MET0=9161
P2P1=30402; RPC1=8646; MET1=9162
P2P2=30403; RPC2=8647; MET2=9163
P2P3=30404; RPC3=8648; MET3=9164

# dial edges (localhost multiaddrs)
PEER_TO_V0=/ip4/127.0.0.1/udp/$P2P0/quic-v1/p2p/$PID0
PEER_TO_V1=/ip4/127.0.0.1/udp/$P2P1/quic-v1/p2p/$PID1
PEER_TO_V2=/ip4/127.0.0.1/udp/$P2P2/quic-v1/p2p/$PID2

N_VALIDATORS="${N_VALIDATORS:-3}"
case "$N_VALIDATORS" in
    3|4) ;;
    *) echo "env.sh: N_VALIDATORS must be 3 or 4 (got '$N_VALIDATORS')" >&2; return 1 2>/dev/null || exit 1 ;;
esac

# node runtime env — mirror the S415/S458 devnet body-push tuning so a later
# bench run (A3) executes against the same consensus config as idle bring-up.
# r4: the direct-push body floor is 8 MB (torus_network::caps
# DIRECT_PUSH_BODY_BYTES, env TORUS_DIRECT_PUSH_BODY_BYTES; was the pinned 4 MB
# pre-O5 fleet floor that clamped the old 6000000 here to 4 MB), so the
# threshold is set AT the floor: every pre-proposal body set the codec can
# carry (cap 200 ~5.6 MB, cap 300 ~7-8 MB at bs400) is pushed directly; only
# larger sets fall back to the HASH manifest + chunked pull.
# s128 (section 86, bench queue 7b): moved to the 512 KB code default
# (HASH_ONLY_PUSH_THRESHOLD, torus-network bridge.rs): +4.3% matched/s, -5.9% CPU
# per fill vs the 8 MB pin on one host; the WAN threshold is still open.
export TORUS_HASH_ONLY_PUSH_THRESHOLD="${TORUS_HASH_ONLY_PUSH_THRESHOLD:-524288}"
# r4: the compiled default IS the r3 cap-200 bundle (NATIVE_TOTAL_BLOCK_CAP 200,
# ORDERS_PER_BLOCK 100k, BLOCK_BYTES 12 MB, trust-cache 32k) — leave the cap
# UNSET here so the node runs its compiled default; export
# TORUS_NATIVE_TOTAL_BLOCK_CAP=100 (or run-cell.sh BLOCK_CAP=100) for the
# cap-100 control. All of these are proposer-local selection policy /
# node-local sizing (validate_block rejects on none) — mixed values cannot fork.
# tools/matched-bench/run-cell.sh BLOCK_CAP=N still exports the coherent bundle
# for sweep cells (BLOCK_CAP=200 == the compiled defaults, byte-for-byte).
if [ -n "${TORUS_NATIVE_TOTAL_BLOCK_CAP:-}" ]; then export TORUS_NATIVE_TOTAL_BLOCK_CAP; fi
# perf A1 (landed 8fa6ccd): skip shard custody in the execute_batch funnel.
export TORUS_SHARD_CUSTODY="${TORUS_SHARD_CUSTODY:-0}"
# mode-2 save-books parallel drain (node-local, byte-identical): default ON at
# host parallelism; set TORUS_SAVE_BOOKS_WORKERS=1 for the serial loop, N>=2
# to cap the drain threads (TORUS_SAVE_BOOKS_MIN_OPS = work gate, default 32).
# Left unset here on purpose — the binary default is the measured config.
# Node anti-spam limits (items A, B, D: funded-account check, per-address
# request limit, per-IP weight limit) are ON in the node binary but OFF on
# the bench devnet, whose senders would otherwise be throttled; turn them on
# for a cell with tools/matched-bench/run-cell.sh ANTISPAM=1. Item C (cancel
# share of each block, TORUS_CANCEL_BLOCK_SHARE_PCT) is a fairness fix and
# stays at the node default here too.
export TORUS_INGRESS_MIN_COLLATERAL="${TORUS_INGRESS_MIN_COLLATERAL:-0}"
export TORUS_ADDR_RATE_LIMIT="${TORUS_ADDR_RATE_LIMIT:-0}"
export TORUS_RPC_IP_WEIGHT_PER_MIN="${TORUS_RPC_IP_WEIGHT_PER_MIN:-0}"
# s58 (exp/archy-s58-bundle-dmt): the stuck-leader header re-send (s75 fix D,
# TORUS_HEADER_RESEND_MS) at 250 ms on the devnet and every matched-bench cell;
# since s128 the node's compiled default is 250 ms too (hotstuff
# implementation.rs, was 3000), so this export only pins it. A caller or a
# bench arm overrides it (EXTRA_ENV / arm env 'TORUS_HEADER_RESEND_MS=3000'
# for the old 3 s; '=0' = off). To run with the name ABSENT (an arm listing it
# in absent_env), set the harness knob HEADER_RESEND_UNSET=1 (campaign
# harness_env, or the caller's env): nothing is exported then, and the node
# uses its compiled default (250 ms, no longer 3 s).
if [ "${HEADER_RESEND_UNSET:-0}" = 1 ]; then
    unset TORUS_HEADER_RESEND_MS
else
    export TORUS_HEADER_RESEND_MS="${TORUS_HEADER_RESEND_MS:-250}"
fi
# History retention, unset here on purpose = the node default (since s131:
# prune by default, N = 100,000; cells before that ran archive).
# TORUS_RETENTION_BLOCKS=N (caller's env) makes start-node.sh pass
# --retention-blocks=N: the state pruner, the block-tree pruner and the DA body
# GC (window = max(N, 1000) + 128 dispatched blocks, RH4) follow N;
# TORUS_RETENTION_BLOCKS=0 = archive (the earlier cells' argv). TORUS_DA_BODY_RETENTION=N (node env) sets the DA body GC
# window alone (0 = off; below the block-tree window a peer lagging further
# cannot get bodies from that node).

METRICS_PORTS="$MET0 $MET1 $MET2"
RPC_URLS="http://localhost:$RPC0 http://localhost:$RPC1 http://localhost:$RPC2"
if [ "$N_VALIDATORS" = 4 ]; then
    METRICS_PORTS="$METRICS_PORTS $MET3"
    RPC_URLS="$RPC_URLS http://localhost:$RPC3"
fi
