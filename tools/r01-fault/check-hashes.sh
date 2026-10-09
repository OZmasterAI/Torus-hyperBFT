#!/usr/bin/env bash
# check-hashes.sh — restart a finished case's 4 validators on their existing data dirs
# (same argv/env as the case), wait until all 4 apply the next checkpoint past the head
# and compare every running-state-hash checkpoint from FROM up to it (torus_getStateHash).
#   CASE=<name> PIPE=0|1 [FROM=100] /home/oz/r01-fault/check-hashes.sh
set -uo pipefail
: "${CASE:?}" "${PIPE:?}"
source /home/oz/r01-fault/lib.sh
[ -d "$FDIR/v3" ] || { echo "no $FDIR/v3" >&2; exit 1; }
export TORUS_EXEC_PIPELINE=$PIPE
declare -a PIDS
trap 'stop_all' EXIT
log "check-hashes: restarting all 4 validators of $CASE on their data dirs"
for i in 0 1 2 3; do start_node "$i"; done
wait_head 0 1 120 || { log "no head"; exit 1; }
sleep 5
h0=$(head_of 0); target=$(( (h0 / 100 + 1) * 100 ))
log "head v0=$h0 v3=$(head_of 3); waiting for checkpoint $target on all 4"
for i in 0 1 2 3; do wait_head "$i" "$target" 300 || log "v$i did not reach $target"; done
sleep 3
rpc "${RPC_PORTS[3]}" torus_getStateHash "[$target]" > "$RES/getStateHash-v3-$target.json"
log "raw v3 torus_getStateHash($target): $(cut -c1-400 "$RES/getStateHash-v3-$target.json")"
if compare_checkpoints "${FROM:-100}" "$target"; then
    log "RESULT: checkpoints ${FROM:-100}..$target MATCH on all 4"
else
    log "RESULT: checkpoint MISMATCH or missing"
fi
