#!/usr/bin/env bash
# start-node.sh — the ONE place a bare-metal devnet validator process is started.
#
# Sourced by:
#   * devnet/wsl/launch-3val.sh          (fresh 3-validator launch)
#   * tools/matched-bench/crash-kill.sh  (restart after the kill -9 crash gate)
#
# Both MUST produce byte-identical argv: a node restarted with different flags
# is not a crash test of the node the cell was running. That is asserted in
# tools/matched-bench/test_harness.py (test_restart_uses_the_same_argv_as_launch).
#
# Requires env.sh to have been sourced first (BIN, GENESIS, DATA_ROOT, RUN_DIR,
# KEY0..3, P2P0..3, RPC0..3, MET0..3, PEER_TO_V0/V1/V2; val3 only with
# N_VALIDATORS=4).
#
#   start_node <idx> [truncate|append]   ->  sets $STARTED_PID
#
# `append` keeps the existing val<idx>.log so a crash cell holds the pre-crash
# log and the post-restart replay lines in one file (run-cell.sh remembers the
# byte offset of the kill and scans only the tail). The default truncates,
# which is what a fresh launch wants. The pids file is the CALLER's business:
# launch-3val.sh appends, crash-kill.sh replaces the one line.

# shellcheck disable=SC2154
node_var() { local v="$1$2"; printf '%s' "${!v-}"; }

# val0 dials val1; val1 dials val0; val2 dials both; val3 (N_VALIDATORS=4) dials
# all three, so the 4 nodes form a full mesh. All edges use known ids.
node_peers() {
    case "$1" in
        0) printf '%s' "$PEER_TO_V1" ;;
        1) printf '%s' "$PEER_TO_V0" ;;
        2) printf '%s' "$PEER_TO_V0,$PEER_TO_V1" ;;
        3) printf '%s' "$PEER_TO_V0,$PEER_TO_V1,$PEER_TO_V2" ;;
        *) return 1 ;;
    esac
}

start_node() {
    local idx=${1-} mode=${2:-truncate}
    local key p2p rpc met peers dd log
    local retention=()
    peers=$(node_peers "$idx") || { echo "start_node: bad validator index '$idx'" >&2; return 1; }
    # Optional history retention (Q9 data-dir growth soak): TORUS_RETENTION_BLOCKS=N
    # passes --retention-blocks=N (state pruner + block-tree pruner; the DA body
    # GC window derives from it, see torus_consensus::da_gc). Unset or empty =
    # no flag: the node default (since s131 prune by default, N = 100,000);
    # TORUS_RETENTION_BLOCKS=0 = archive. Appended last so the --data-dir match in
    # crash-kill.sh is unchanged; launch and restart read the same variable.
    if [ -n "${TORUS_RETENTION_BLOCKS:-}" ]; then
        case "$TORUS_RETENTION_BLOCKS" in
            *[!0-9]*) echo "start_node: TORUS_RETENTION_BLOCKS='$TORUS_RETENTION_BLOCKS' is not a block count" >&2; return 1 ;;
        esac
        retention=("--retention-blocks=$TORUS_RETENTION_BLOCKS")
    fi
    # Optional running state hash attestation (run-cell.sh STATE_HASH_ATTEST=1):
    # STATE_HASH_ATTEST_KEY_DIR=D passes --state-hash-attest-key=D/val<idx>.key,
    # the validator's secp256k1 ACCOUNT key (owner-only file; the node refuses
    # group/other access), so the node signs AttestStateHash for every durable
    # checkpoint. Unset or empty = no flag, the argv of every earlier cell. Last
    # on the line (after the retention flag); launch and restart read the same
    # variable, so a restarted node attests too.
    local attest=()
    if [ -n "${STATE_HASH_ATTEST_KEY_DIR:-}" ]; then
        [ -r "$STATE_HASH_ATTEST_KEY_DIR/val$idx.key" ] || {
            echo "start_node: STATE_HASH_ATTEST_KEY_DIR=$STATE_HASH_ATTEST_KEY_DIR has no readable val$idx.key" >&2; return 1; }
        attest=("--state-hash-attest-key=$STATE_HASH_ATTEST_KEY_DIR/val$idx.key")
    fi
    key=$(node_var KEY "$idx"); p2p=$(node_var P2P "$idx")
    rpc=$(node_var RPC "$idx"); met=$(node_var MET "$idx")
    [ -n "$key" ] && [ -n "$p2p" ] && [ -n "$rpc" ] && [ -n "$met" ] || {
        echo "start_node: env.sh not sourced (no KEY$idx/P2P$idx/RPC$idx/MET$idx)" >&2; return 1; }
    dd="$DATA_ROOT/data/val$idx"
    log="$RUN_DIR/val$idx.log"
    mkdir -p "$dd" "$RUN_DIR"
    [ "$mode" = append ] || : > "$log"
    echo "starting val$idx  rpc=$rpc p2p=$p2p metrics=$met (log: $mode)"
    nohup "$BIN" \
        --genesis="$GENESIS" \
        --data-dir="$dd" \
        --validator-key="$key" \
        --p2p-listen="/ip4/127.0.0.1/udp/$p2p/quic-v1" \
        --p2p-private-addrs \
        --p2p-peers="$peers" \
        --rpc-addr="127.0.0.1:$rpc" \
        --metrics-addr="127.0.0.1:$met" \
        --log-level=info \
        --native-gossip=true \
        ${retention[@]+"${retention[@]}"} \
        ${attest[@]+"${attest[@]}"} \
        >> "$log" 2>&1 &
    STARTED_PID=$!
}
