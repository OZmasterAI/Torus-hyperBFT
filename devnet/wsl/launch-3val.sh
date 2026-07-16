#!/usr/bin/env bash
# launch-3val.sh — start 3 bare torus-node validators on localhost (NO docker).
# Idempotent-ish: refuses to start if a devnet is already running (stale pids).
# Data dirs persist across restarts; pass CLEAN=1 to wipe them first (fresh
# genesis init).
set -euo pipefail
cd "$(dirname "$0")"
source ./env.sh

[ -x "$BIN" ]     || { echo "FATAL: missing $BIN — cargo build --release -p torus-node" >&2; exit 1; }
[ -f "$GENESIS" ] || { echo "FATAL: missing $GENESIS — run devnet/wsl/gen-3val-genesis.sh" >&2; exit 1; }

if [ -f "$RUN_DIR/pids" ] && xargs -a "$RUN_DIR/pids" -r -I{} kill -0 {} 2>/dev/null; then
    echo "FATAL: a devnet appears to be running (pids in $RUN_DIR/pids). Run stop-3val.sh first." >&2
    exit 1
fi

if [ "${CLEAN:-0}" = 1 ]; then
    echo "CLEAN=1 — wiping data dirs for fresh genesis init"
    rm -rf "$DATA_ROOT/data"
fi
mkdir -p "$DATA_ROOT/data" "$RUN_DIR"
: > "$RUN_DIR/pids"

start_node() { # $1=idx $2=key $3=p2pport $4=rpcport $5=metport $6=peers_csv
    local idx=$1 key=$2 p2p=$3 rpc=$4 met=$5 peers=$6
    local dd="$DATA_ROOT/data/val$idx"
    local log="$RUN_DIR/val$idx.log"
    mkdir -p "$dd"
    echo "starting val$idx  rpc=$rpc p2p=$p2p metrics=$met"
    nohup "$BIN" \
        --genesis="$GENESIS" \
        --data-dir="$dd" \
        --validator-key="$key" \
        --p2p-listen="/ip4/0.0.0.0/udp/$p2p/quic-v1" \
        --p2p-private-addrs \
        --p2p-peers="$peers" \
        --rpc-addr="0.0.0.0:$rpc" \
        --metrics-addr="0.0.0.0:$met" \
        --log-level=info \
        --native-gossip=true \
        > "$log" 2>&1 &
    echo $! >> "$RUN_DIR/pids"
}

# val0 dials val1; val1 dials val0; val2 dials both. All edges use known ids.
start_node 0 "$KEY0" "$P2P0" "$RPC0" "$MET0" "$PEER_TO_V1"
start_node 1 "$KEY1" "$P2P1" "$RPC1" "$MET1" "$PEER_TO_V0"
start_node 2 "$KEY2" "$P2P2" "$RPC2" "$MET2" "$PEER_TO_V0,$PEER_TO_V1"

echo
echo "launched pids: $(tr '\n' ' ' < "$RUN_DIR/pids")"
echo "logs:          $RUN_DIR/val{0,1,2}.log"
echo "rpc:           $RPC_URLS"
echo "metrics:       $METRICS_PORTS"
echo "run health-3val.sh in ~40s to confirm idle block production."
