#!/usr/bin/env bash
# drill.sh — running-state-hash devnet drill (docs/plans/running-state-hash-impl.md,
# Task 9) on the WSL 3-validator topology (devnet/wsl/env.sh ports + keys), with
# a drill-specific genesis, data root, binary and per-validator attest keys.
#
#   DRILL_ROOT=/path GENESIS_FILE=/path NODE_BIN=/path drill.sh <cmd>
#     start <idx>   start one validator (log appended)    stop <idx>  SIGTERM + wait
#     launch        start val0..2 (fresh logs)            stopall     stop every validator
#     status        per node: height + state-hash metrics
#     hash <h>      torus_getStateHash(h) on every node
#
# Keys: $DRILL_ROOT/keys/attest<N>.key (0600, secp256k1 hex) whose addresses are
# the genesis .validators[N].address. Env TORUS_STATE_HASH_FAILSTOP passes through.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
: "${DRILL_ROOT:?}" "${GENESIS_FILE:?}" "${NODE_BIN:?}"
export DATA_ROOT="$DRILL_ROOT" DRILL_KEYS="$DRILL_ROOT/keys" NODE_BIN
# shellcheck source=../../wsl/env.sh
. "$HERE/../../wsl/env.sh"
# shellcheck source=../../wsl/start-node.sh
. "$HERE/../../wsl/start-node.sh"
BIN="$HERE/node-wrapper.sh"
GENESIS="$GENESIS_FILE"
mkdir -p "$RUN_DIR"

pidfile() { printf '%s/val%s.pid' "$RUN_DIR" "$1"; }
alive() { [ -f "$(pidfile "$1")" ] && kill -0 "$(cat "$(pidfile "$1")")" 2>/dev/null; }

start() {
    alive "$1" && { echo "val$1 already running" >&2; return 1; }
    start_node "$1" "${2:-append}"
    echo "$STARTED_PID" > "$(pidfile "$1")"
}

stop() {
    alive "$1" || { echo "val$1 not running"; return 0; }
    local pid; pid=$(cat "$(pidfile "$1")")
    kill "$pid"
    for _ in $(seq 60); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    if kill -0 "$pid" 2>/dev/null; then echo "val$1 did not exit in 60s" >&2; return 1; fi
    echo "val$1 (pid $pid) stopped"
}

metric() { curl -s -m 3 "http://127.0.0.1:$1/metrics" | grep -E "^$2" || true; }

case "${1:-}" in
    start) start "$2" ;;
    stop) stop "$2" ;;
    launch) for i in 0 1 2; do start "$i" truncate; done ;;
    stopall) for i in 0 1 2; do stop "$i"; done ;;
    status)
        for i in 0 1 2; do
            m=$(node_var MET "$i")
            echo "== val$i $(metric "$m" 'torus_block_height ')"
            metric "$m" 'torus_state_hash_(mismatch|no_quorum|unverified|height|attestations)' | sed 's/^/   /'
        done ;;
    hash)
        for i in 0 1 2; do
            r=$(node_var RPC "$i")
            printf 'val%s ' "$i"
            curl -s -m 3 -H 'content-type: application/json' "http://127.0.0.1:$r" \
                -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"torus_getStateHash\",\"params\":[$2]}"
            echo
        done ;;
    *) sed -n '2,13p' "$0"; exit 2 ;;
esac
