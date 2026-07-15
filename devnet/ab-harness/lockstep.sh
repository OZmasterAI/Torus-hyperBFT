#!/usr/bin/env bash
# Task A/8: induce a lock-step timeout and show geometric backoff + reconvergence
# without a multi-view park. SIGSTOP/SIGCONT (never kill) f+1=2 of 4 validators to
# drop quorum, then restore. Usage: lockstep.sh <tag> <binary>
set -uo pipefail
source /home/18c/torus-ab/harness.sh
MV=/home/18c/torus-ab/metval.sh
TAG=$1; BIN=$2
TRACE=$RUN/$TAG/trace.tsv

launch "$TAG" "$BIN" 0
wait_chain "$TAG" || { teardown "$TAG"; exit 1; }
sleep 8
mapfile -t P < "$RUN/$TAG/pids"
echo "pids: ${P[*]}"

sample(){
  local v t h
  v=$(bash "$MV" $(metport 0) torus_consensus_view)
  t=$(bash "$MV" $(metport 0) torus_consensus_timeout_total)
  h=$(height $(rpcport 0))
  printf '%s\t%s\t%s\t%s\t%s\n' "$(date +%s.%N)" "$1" "$v" "$t" "$h" >> "$TRACE"
}
printf 'ts\tphase\tview\ttimeouts\theight\n' > "$TRACE"

echo "--- HEALTHY baseline 15s ---"
for _ in $(seq 1 15); do sample healthy; sleep 1; done

echo "--- STALL: SIGSTOP nodes 2 and 3 ---"
kill -STOP "${P[2]}" "${P[3]}"
STALL_START=$(date +%s.%N)
for _ in $(seq 1 ${STALL_S:-45}); do sample stall; sleep 1; done

echo "--- RESTORE: SIGCONT nodes 2 and 3 ---"
kill -CONT "${P[2]}" "${P[3]}"
RESUME_TS=$(date +%s.%N)
for _ in $(seq 1 ${RECOVER_S:-30}); do sample recover; sleep 1; done

teardown "$TAG"
echo "STALL_START=$STALL_START RESUME_TS=$RESUME_TS"
echo "=== TRACE ==="
cat "$TRACE"
