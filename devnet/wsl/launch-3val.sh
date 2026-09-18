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

if [ "${FRESH_ONLY:-0}" = 1 ]; then
    # Atomically claim a new data directory; never enter the cleanup path,
    # even if a caller also sets CLEAN=1. A preflight check alone has a race.
    mkdir -p "$DATA_ROOT"
    mkdir "$DATA_ROOT/data" || {
        echo "FATAL: FRESH_ONLY requires a new DATA_ROOT/data path; existing data was not touched" >&2
        exit 1
    }
elif [ "${CLEAN:-0}" = 1 ]; then
    echo "CLEAN=1 — wiping data dirs for fresh genesis init"
    rm -rf "$DATA_ROOT/data"
fi
mkdir -p "$DATA_ROOT/data" "$RUN_DIR"
: > "$RUN_DIR/pids"

# The node argv lives in start-node.sh so a crash-gate RESTART
# (tools/matched-bench/crash-kill.sh) can never drift from a fresh launch —
# in particular every node keeps its EXPLICIT --p2p-peers, without which
# main.rs falls back to the compiled TESTNET bootstrap peers and the devnet
# dial-storms the live seed (S395 cross-contamination, mem e9e757f8).
source ./start-node.sh

for idx in 0 1 2; do
    start_node "$idx"
    echo "$STARTED_PID" >> "$RUN_DIR/pids"
done

echo
echo "launched pids: $(tr '\n' ' ' < "$RUN_DIR/pids")"
echo "logs:          $RUN_DIR/val{0,1,2}.log"
echo "rpc:           $RPC_URLS"
echo "metrics:       $METRICS_PORTS"
echo "run health-3val.sh in ~40s to confirm idle block production."
