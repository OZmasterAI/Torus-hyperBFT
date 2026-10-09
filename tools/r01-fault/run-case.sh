#!/usr/bin/env bash
# run-case.sh — R01 FULL-DISK fault case on a 4-validator localhost devnet.
#
#   CASE=<name> PIPE=0|1 LOAD=native|none [EPOCH=100] [WARM_S=60] [ALIGN=0] \
#   [GAP_KB=0] [MAX_ATTEMPTS=20] [MODE=live|catchup|starve DOWN_S=40 QMIN=1] [WANT='FATAL: native overlay|FATAL: applied-height'] \
#       /home/oz/r01-fault/run-case.sh
#
# One devnet (fresh genesis: epoch_length=EPOCH, running state hash from height 1),
# 4 validators, v3's data dir on /mnt/r01/<case>/v3, TORUS_EXEC_PIPELINE=$PIPE on all.
# Attempt loop (v3 in sync first; MODE=catchup: stop v3 DOWN_S s, restart it and fill
# once its execution queue holds >= QMIN committed blocks; MODE=starve: SCHED_IDLE the
# execution thread next to busy loops on CPU 31 until its queue holds >= QMIN, fill, stop the loops):
#   fill /mnt/r01 with /mnt/r01/fill leaving GAP_KB free (ALIGN=1: fill when v3's
#   applied height is EPOCH-1 mod EPOCH) -> classify v3's reaction within 20 s:
#     HIT     v3 exited AND its log has a line matching WANT (the R01 serial flush);
#     EXIT_n  exited with another failure line; ZOMBIE: a thread panicked, process up;
#     NOFAIL  nothing failed.
#   Not a HIT: SIGKILL v3 if alive, delete the fill, restart v3, wait until it rejoins
#   (its replay lines go to attempts.txt), next attempt.
#   HIT: delete the fill, restart v3 (same argv), record its boot replay lines, wait
#   until v3 and v0 apply the next checkpoint past the live head and compare every
#   retained checkpoint hash on all 4 validators.
set -uo pipefail
: "${CASE:?}" "${PIPE:?}" "${LOAD:?}"
EPOCH=${EPOCH:-100}; WARM_S=${WARM_S:-60}; ALIGN=${ALIGN:-0}; GAP_KB=${GAP_KB:-0}
MAX_ATTEMPTS=${MAX_ATTEMPTS:-20}; RECOVER_TIMEOUT=${RECOVER_TIMEOUT:-600}
MODE=${MODE:-live}; DOWN_S=${DOWN_S:-40}; QMIN=${QMIN:-1}
WANT=${WANT:-'FATAL: native overlay flush|FATAL: applied-height marker flush'}
source /home/oz/r01-fault/lib.sh

if [ -e "$RES" ] && [ -n "$(ls -A "$RES" 2>/dev/null)" ]; then echo "FATAL: $RES not empty" >&2; exit 1; fi
if [ -e "$FDIR" ]; then echo "FATAL: $FDIR exists (stale data dir)" >&2; exit 1; fi
if [ -e /mnt/r01/fill ]; then echo "FATAL: /mnt/r01/fill exists" >&2; exit 1; fi
mkdir -p "$RES" "$RUN" "$FDIR"
trap '[ -n "${BUSY:-}" ] && kill "${BUSY[@]}" 2>/dev/null; stop_all; rm -f /mnt/r01/fill; echo done > "$RES/DONE"' EXIT
export TORUS_EXEC_PIPELINE=$PIPE
declare -a PIDS
ATT="$RES/attempts.txt"; : > "$ATT"
strip() { sed -E 's/\x1b\[[0-9;]*m//g'; }
since() { tail -c +"$(( $1 + 1 ))" "$(nodelog 3)" | strip; }   # v3 log after byte offset

log "CASE=$CASE PIPE=$PIPE LOAD=$LOAD BS=${BS:-100} RATE=${RATE:-3} MODE=$MODE EPOCH=$EPOCH WARM_S=$WARM_S ALIGN=$ALIGN GAP_KB=$GAP_KB WANT='$WANT'"
log "node md5 $(md5sum "$NODE_BIN" | cut -c1-12) worktree commit $(git -C "$WT" rev-parse --short HEAD)"
make_genesis "$EPOCH"
for i in 0 1 2 3; do start_node "$i"; done
wait_head 3 3 120 || { log "RESULT: devnet never produced blocks"; exit 1; }
log "chain up: v3 head $(head_of 3)"
if [ "$LOAD" = native ]; then load_loop & LOAD_PID=$!; log "native load loop pid $LOAD_PID"; fi
sleep "$WARM_S"
log "warm: v3 applied $(head_of 3) peers $(peer_head)"

in_sync() { local p; p=$(peer_head); [ "$(head_of 3)" -ge $(( p - 3 )) ] && [ "$p" -ge 0 ]; }

restart_v3() { # $1 = label, $2 = settle seconds (8); records the boot replay lines
    local off; off=$(stat -c %s "$(nodelog 3)")
    start_node 3
    sleep "${2:-8}"
    { echo "--- restart ($1) boot lines:"; since "$off" | grep -aE 'execution gap|replay|crash recovery|FATAL|panicked' | grep -av 'replayed native action' | cut -c1-400 | head -12; } >> "$ATT"
    if ! kill -0 "${PIDS[3]}" 2>/dev/null; then wait "${PIDS[3]}"; echo "    v3 EXITED $? on restart" >> "$ATT"; return 1; fi
    return 0
}

qdepth() { curl -s -m1 "127.0.0.1:${MET_PORTS[3]}/metrics" | awk '/^torus_exec_queue_depth /{print int($2)}'; }

hit=0
for a in $(seq 1 "$MAX_ATTEMPTS"); do
    for _ in $(seq 1 60); do in_sync && break; sleep 2; done
    if [ "$MODE" = catchup ]; then
        # Stop v3, let the chain run on, restart it: while it catches up its execution
        # queue holds committed blocks, so its NEXT serial flush runs after the fill.
        kill "${PIDS[3]}"; wait "${PIDS[3]}"; sc=$?
        log "attempt $a: v3 stopped (SIGTERM, exit $sc), down ${DOWN_S}s"
        sleep "$DOWN_S"
        restart_v3 "catch-up before attempt $a" 1 || { log "v3 failed to start for attempt $a"; break; }
        q=0
        for _ in $(seq 1 1500); do
            q=$(qdepth); q=${q:-0}
            if [ "$q" -ge "$QMIN" ]; then
                [ "$ALIGN" != 1 ] && break
                h=$(head_of 3); [ $(( (h + 1) % EPOCH )) -eq 0 ] && break
            fi
            sleep 0.02
        done
        log "attempt $a: exec queue depth $q at fill (applied $(head_of 3), peers $(peer_head))"
    elif [ "$MODE" = starve ]; then
        # Starve v3's execution thread ("torus-execution"): pin it to CPU 31 next to
        # 4 busy loops, SCHED_IDLE. Its queue fills to the sync_channel bound (64) and
        # the HotStuff thread parks on the blocking send, so consensus stops writing;
        # the next write v3 makes is the in-flight block's serial flush.
        tid=$(grep -lx 'torus-execution' /proc/"${PIDS[3]}"/task/*/comm 2>/dev/null | head -1 | cut -d/ -f5)
        BUSY=()
        for _ in $(seq 1 "${STARVE_LOOPS:-4}"); do taskset -c 31 bash -c 'while :; do :; done' & BUSY+=($!); done
        taskset -p -c 31 "$tid" > /dev/null; chrt --idle -p 0 "$tid"
        log "attempt $a: starving exec thread tid $tid ($(chrt -p "$tid" | head -1 | sed 's/.*: //'))"
        q=0
        for _ in $(seq 1 3000); do
            q=$(qdepth); q=${q:-0}
            if [ "$q" -ge "$QMIN" ]; then
                [ "$ALIGN" != 1 ] && break
                h=$(head_of 3); [ $(( (h + 1) % EPOCH )) -eq 0 ] && break
            fi
            sleep 0.02
        done
        log "attempt $a: exec queue depth $q at fill (applied $(head_of 3), peers $(peer_head))"
    elif [ "$ALIGN" = 1 ]; then
        for _ in $(seq 1 600); do h=$(head_of 3); [ $(( (h + 1) % EPOCH )) -eq 0 ] && break; sleep 0.05; done
    fi
    off=$(stat -c %s "$(nodelog 3)")
    avail=$(df -B1 --output=avail /mnt/r01 | tail -1 | tr -d ' ')
    size=$(( avail - GAP_KB * 1024 ))
    fill_h=$(head_of 3)
    fallocate -l "$size" /mnt/r01/fill 2>/dev/null
    after=$(df -B1 --output=avail /mnt/r01 | tail -1 | tr -d ' ')
    if [ "$MODE" = starve ] && [ "${DA_TRIGGER:-0}" = 1 ]; then
        # Consensus is parked and the exec thread is held: no write is in flight. A short
        # native burst at v3's RPC makes the DA store write (>= 40 KB) that hits ENOSPC
        # and puts RocksDB into its stop-writes state BEFORE the exec thread flushes.
        timeout 20 "$BENCH_BIN" consensus --rpc-urls "http://127.0.0.1:${RPC_PORTS[3]}" --batch-size 400 \
            --senders 4 --duration 3 --rate 5 --pre-sign 15 --sign-mode session --markets 4 \
            >> "$RES/da-trigger.log" 2>&1 &
        trig=$!
        for _ in $(seq 1 300); do since "$off" | grep -aq 'No space left' && break; sleep 0.05; done
        log "attempt $a: DA trigger: first ENOSPC line: $(since "$off" | grep -a 'No space left' | head -1 | cut -c1-140)"
        kill "$trig" 2>/dev/null
    fi
    if [ "$MODE" = starve ]; then kill "${BUSY[@]}" 2>/dev/null; wait "${BUSY[@]}" 2>/dev/null; fi
    log "attempt $a: v3 applied=$fill_h peers=$(peer_head) fill=$size free-after=$after"
    code=alive; last=$fill_h
    for _ in $(seq 1 80); do
        if ! kill -0 "${PIDS[3]}" 2>/dev/null; then wait "${PIDS[3]}"; code=$?; break; fi
        h=$(head_of 3); [ "$h" -ge 0 ] && last=$h
        sleep 0.25
    done
    seg=$(since "$off")
    first=$(grep -aE 'ERROR|panicked|FATAL' <<< "$seg" | head -1 | cut -c1-400)
    fatal=$(grep -aE "$WANT" <<< "$seg" | head -1 | cut -c1-400)
    fh=$(grep -o 'height=[0-9]*' <<< "$fatal" | head -1 | cut -d= -f2)
    if [ "$code" != alive ] && [ -n "$fatal" ] && [ "${REQUIRE_BOUNDARY:-0}" = 1 ] \
        && [ $(( ${fh:-1} % EPOCH )) -ne 0 ]; then cls="EXIT_${code}_NOT_BOUNDARY_H$fh"
    elif [ "$code" != alive ] && [ -n "$fatal" ]; then cls=HIT
    elif [ "$code" != alive ]; then cls="EXIT_$code"
    elif grep -aq panicked <<< "$seg"; then cls=ZOMBIE
    elif [ -n "$first" ]; then cls=ERROR_ALIVE
    else cls=NOFAIL; fi
    {
        echo "=== attempt $a: $cls  exit=$code  v3 applied at fill=$fill_h  last applied seen=$last  free after fill=$after"
        echo "    first failure: $first"
        [ -n "$fatal" ] && echo "    R01 line:      $fatal"
        grep -aE 'panicked|FATAL' <<< "$seg" | head -6 | cut -c1-300 | sed 's/^/    /'
    } >> "$ATT"
    log "attempt $a: $cls exit=$code last-applied=$last first: ${first:0:160}"
    if [ "$cls" = HIT ]; then
        hit=1
        H=$(grep -o 'height=[0-9]*' <<< "$fatal" | head -1 | cut -d= -f2)
        echo "$code" > "$RES/exit-code.txt"
        echo "$H" > "$RES/failed-height.txt"
        echo "$last" > "$RES/applied-last-seen-via-rpc.txt"
        grep -aE 'panicked|FATAL|ERROR' <<< "$seg" | head -40 > "$RES/fault-lines.txt"
        rpc "${RPC_PORTS[0]}" torus_getBlockBody "[$H]" > "$RES/block-H-body-from-v0.json"
        break
    fi
    # not the R01 path: clear and bring v3 back for the next attempt
    p=${PIDS[3]}
    if kill -0 "$p" 2>/dev/null; then kill -9 "$p"; wait "$p" 2>/dev/null; fi
    rm -f /mnt/r01/fill
    restart_v3 "after attempt $a" || { log "v3 failed to restart after attempt $a"; break; }
done

rm -f /mnt/r01/fill
df -B1 /mnt/r01 > "$RES/df-after-clear.txt"
if [ "$hit" = 1 ]; then
    log "HIT at height $H (exit $code); clearing the fault and restarting v3"
    restart_v3 "after HIT" || log "RESULT: v3 died on restart after the HIT"
    grep -A12 'after HIT' "$ATT" > "$RES/restart-lines.txt"
    cat "$RES/restart-lines.txt" >&2
    ph=$(peer_head); target=$(( (ph / 100 + 1) * 100 ))
    log "waiting for v3 and v0 to apply checkpoint $target (peers $ph)"
    if wait_head 3 "$target" "$RECOVER_TIMEOUT" && wait_head 0 "$target" 120; then
        sleep 3
        first=$(( (H / 100) * 100 - 200 )); [ "$first" -lt 100 ] && first=100
        if compare_checkpoints "$first" "$target"; then
            log "RESULT: PASS-candidate H=$H exit=$code; checkpoints $first..$target match on all 4"
        else
            log "RESULT: checkpoint MISMATCH or missing (hash-compare.txt)"
        fi
    else
        log "RESULT: v3 did not reach checkpoint $target within ${RECOVER_TIMEOUT}s (head $(head_of 3))"
    fi
else
    log "RESULT: no HIT in $MAX_ATTEMPTS attempts"
    ph=$(peer_head); target=$(( (ph / 100 + 1) * 100 ))
    if wait_head 3 "$target" 300 && wait_head 0 "$target" 60; then
        sleep 3; compare_checkpoints 100 "$target" && log "post-attempts checkpoints match on all 4"
    fi
fi
du -sb "$FDIR/v3" > "$RES/v3-datadir-size.txt"
log "case $CASE finished"
