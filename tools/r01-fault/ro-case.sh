#!/usr/bin/env bash
# ro-case.sh — R01 READ-ONLY fault case (needs the user's sudo; this script never uses sudo).
#
#   /home/oz/r01-fault/ro-case.sh <native|none> [label]
#
# Steps and the user's sudo commands: /home/oz/r01-fault/RO-STEPS.md.
# 1. pre-flight: /mnt/r01 mounted rw WITH errors=remount-ro (else exit), no stale fill,
#    no node running on our ports;
# 2. 4-validator devnet, pipeline OFF (TORUS_EXEC_PIPELINE=0), v3's data dir on
#    /mnt/r01/ro-<load>-<label>/v3, the others under /home/oz/r01-fault/run/;
#    LOAD=native: native order bursts (BS/RATE env, default 400 orders x 10 actions/s per
#    sender, which keeps the execution thread busy); STARVE=1 (default) also slows it;
# 3. once v3 is producing and in sync: creates /home/oz/r01-fault/run/ro-TRIGGER (an
#    armed `sudo` watcher fires on it, see RO-STEPS.md) and prints
#    'READY: ask user to force ro now';
# 4. polls up to 15 min: v3 exit -> exit code; records the last applied height v3
#    served over RPC (eth_blockNumber = applied-height marker), the R01 FATAL line
#    (failed height H), the first failure line, a ZOMBIE verdict if a thread panicked
#    and the process stayed up (then kills v3 at the end of the window);
# 5. prints 'RECOVER: ...' and waits up to 30 min for /mnt/r01 to be writable again
#    (user: umount, e2fsck, mount); restarts v3 with the same argv, records its boot
#    replay line (applied_height = marker after the exit), waits until v3 applies the
#    next checkpoint and compares every retained checkpoint hash with v0..v2.
# Results: /home/oz/r01-fault/results/ro-<load>-<label>/ (SUMMARY.txt first).
set -uo pipefail
LOAD=${1:?usage: ro-case.sh <native|none> [label]}
case "$LOAD" in native|none) ;; *) echo "LOAD must be native|none" >&2; exit 2 ;; esac
CASE="ro-$LOAD-${2:-1}"
# native: the heavy shape (400 orders x 10 actions/s per sender) that kept v3's execution
# thread busy enough for the full-disk case 1 HIT. STARVE=1 (default): SCHED_IDLE v3's
# execution thread next to 16 busy loops on CPU 31 from READY until the fs goes ro, so a
# committed block is more likely in flight in execution when the first write fails.
export BS=${BS:-400} RATE=${RATE:-10}
STARVE=${STARVE:-1}
source /home/oz/r01-fault/lib.sh
TRIG=/home/oz/r01-fault/run/ro-TRIGGER
WANT='FATAL: native overlay flush|FATAL: applied-height marker flush|FATAL: EVM block marker flush|FATAL: flush worker'

opts=$(findmnt -n -o OPTIONS /mnt/r01) || { echo "FATAL: /mnt/r01 not mounted" >&2; exit 1; }
case ",$opts," in *,rw,*) ;; *) echo "FATAL: /mnt/r01 is not rw ($opts) - recover it first (RO-STEPS.md)" >&2; exit 1 ;; esac
# DRY_FILL=1: harness self-test without sudo: the fault is a full disk (fallocate
# /mnt/r01/fill at READY, deleted at RECOVER) instead of the ext4 error.
[ "${DRY_FILL:-0}" = 1 ] || case ",$opts," in *,errors=remount-ro,*) ;; *) echo "FATAL: /mnt/r01 lacks errors=remount-ro ($opts) - run step 1 of RO-STEPS.md" >&2; exit 1 ;; esac
[ -e /mnt/r01/fill ] && { echo "FATAL: /mnt/r01/fill exists (full-disk leftover)" >&2; exit 1; }
[ -e "$FDIR" ] && { echo "FATAL: $FDIR exists - pick another label" >&2; exit 1; }
[ -e "$RES" ] && [ -n "$(ls -A "$RES" 2>/dev/null)" ] && { echo "FATAL: $RES not empty" >&2; exit 1; }
for p in "${RPC_PORTS[@]}"; do
    if ss -ltn | grep -q ":$p "; then echo "FATAL: port $p in use (another devnet running?)" >&2; exit 1; fi
done
mkdir -p "$RES" "$RUN" "$FDIR"
rm -f "$TRIG"
BUSY=()
unstarve() { [ "${#BUSY[@]}" -gt 0 ] && kill "${BUSY[@]}" 2>/dev/null; BUSY=(); }
trap 'rm -f "$TRIG"; unstarve; stop_all; echo done > "$RES/DONE"' EXIT
export TORUS_EXEC_PIPELINE=0
declare -a PIDS
S="$RES/SUMMARY.txt"; : > "$S"
sum() { echo "$*" | tee -a "$S" >&2; }
strip() { sed -E 's/\x1b\[[0-9;]*m//g'; }

log "RO CASE $CASE LOAD=$LOAD BS=$BS RATE=$RATE STARVE=$STARVE pipeline=0 node md5 $(md5sum "$NODE_BIN" | cut -c1-12) commit $(git -C "$WT" rev-parse --short HEAD)"
log "mount: $opts ; ext4 errors_count=$(cat /sys/fs/ext4/loop0/errors_count 2>/dev/null)"
make_genesis 100
for i in 0 1 2 3; do start_node "$i"; done
wait_head 3 3 120 || { sum "RESULT: devnet never produced blocks"; exit 1; }
[ "$LOAD" = native ] && { load_loop & LOAD_PID=$!; }
sleep 45
for _ in $(seq 1 60); do p=$(peer_head); [ "$(head_of 3)" -ge $((p - 3)) ] && break; sleep 2; done
off=$(stat -c %s "$(nodelog 3)")
if [ "$STARVE" = 1 ]; then
    tid=$(grep -lx 'torus-execution' /proc/"${PIDS[3]}"/task/*/comm 2>/dev/null | head -1 | cut -d/ -f5)
    for _ in $(seq 1 16); do taskset -c 31 bash -c 'while :; do :; done' & BUSY+=($!); done
    taskset -p -c 31 "$tid" > /dev/null; chrt --idle -p 0 "$tid"
    log "exec thread tid $tid starved ($(chrt -p "$tid" | head -1 | sed 's/.*: //'), CPU 31 + 16 busy loops)"
    sleep 2
fi
ready_h=$(head_of 3)
touch "$TRIG"
[ "${DRY_FILL:-0}" = 1 ] && fallocate -l "$(df -B1 --output=avail /mnt/r01 | tail -1 | tr -d ' ')" /mnt/r01/fill 2>/dev/null
log "v3 applied=$ready_h peers=$(peer_head)"
echo
echo "READY: ask user to force ro now   (echo r01 | sudo tee /sys/fs/ext4/loop0/trigger_fs_error)"
echo "       trigger file $TRIG created (an armed watcher fires on it)"
echo

# ---- wait for the node to exit (15 min) ----
code=alive; last=$ready_h; t=0; ro_seen=""
while [ "$t" -lt $(( 2 * ${WAIT_S:-900} )) ]; do   # 0.5 s steps, WAIT_S (default 900 s = 15 min)
    if ! kill -0 "${PIDS[3]}" 2>/dev/null; then wait "${PIDS[3]}"; code=$?; break; fi
    [ "$t" -eq 120 ] && unstarve   # never starve longer than 60 s
    if [ -z "$ro_seen" ] && case ",$(findmnt -n -o OPTIONS /mnt/r01)," in *,ro,*) true ;; *) false ;; esac; then
        ro_seen=$(date +%T); log "/mnt/r01 is now read-only (v3 applied $(head_of 3))"; unstarve
    fi
    h=$(head_of 3); [ "$h" -ge 0 ] && last=$h
    sleep 0.5; t=$((t + 1))
done
unstarve
seg=$(tail -c +"$((off + 1))" "$(nodelog 3)" | strip)
first=$(grep -aE 'ERROR|panicked|FATAL' <<< "$seg" | head -1 | cut -c1-400)
fatal=$(grep -aE "$WANT" <<< "$seg" | head -1 | cut -c1-400)
H=$(grep -o 'height=[0-9]*' <<< "$fatal" | head -1 | cut -d= -f2)
if [ "$code" = alive ]; then
    if grep -aq panicked <<< "$seg"; then verdict=ZOMBIE; else verdict=NO_FAILURE; fi
    kill -9 "${PIDS[3]}" 2>/dev/null; wait "${PIDS[3]}" 2>/dev/null
else
    verdict="EXIT_$code"
fi
grep -aE 'panicked|FATAL|ERROR' <<< "$seg" | head -40 > "$RES/fault-lines.txt"
sum "verdict:            $verdict (exit code $code; 70 = R01 fail-stop)"
sum "fs went ro at:      ${ro_seen:-not seen}"
sum "first failure:      $first"
sum "R01 line:           ${fatal:-none}"
sum "failed height H:    ${H:-none}"
sum "last applied (RPC): $last  (H-1 expected: $([ -n "${H:-}" ] && echo $((H - 1)) || echo n/a))"
[ -n "${H:-}" ] && rpc "${RPC_PORTS[0]}" torus_getBlockBody "[$H]" > "$RES/block-H-body-from-v0.json"

# ---- recovery (user) ----
echo
[ "${DRY_FILL:-0}" = 1 ] && rm -f /mnt/r01/fill
echo "RECOVER: v3 is stopped. Run the recovery commands (RO-STEPS.md step 4): umount, e2fsck, mount."
echo "         waiting up to 30 min for /mnt/r01 to be mounted rw again..."
echo
ok=0
for _ in $(seq 1 900); do
    o=$(findmnt -n -o OPTIONS /mnt/r01 2>/dev/null) || { sleep 2; continue; }
    case ",$o," in *,rw,*) [ -w "$FDIR/v3" ] && touch "$FDIR/.rw-probe" 2>/dev/null && { rm -f "$FDIR/.rw-probe"; ok=1; break; } ;; esac
    sleep 2
done
[ "$ok" = 1 ] || { sum "RESULT: /mnt/r01 not writable again within 30 min - no replay check"; exit 1; }
sum "remounted:          $(findmnt -n -o SOURCE,OPTIONS /mnt/r01); owner $(stat -c %U:%G /mnt/r01) / $(stat -c %U:%G "$FDIR/v3")"
roff=$(stat -c %s "$(nodelog 3)")
start_node 3
sleep 15
if ! kill -0 "${PIDS[3]}" 2>/dev/null; then wait "${PIDS[3]}"; sum "RESULT: v3 exited $? on restart"; fi
boot=$(tail -c +"$((roff + 1))" "$(nodelog 3)" | strip | grep -aE 'execution gap|crash recovery|FATAL|panicked' | head -6 | cut -c1-400)
sum "restart boot lines:"; sum "$boot"
ph=$(peer_head); target=$(( (ph / 100 + 1) * 100 ))
if wait_head 3 "$target" 600 && wait_head 0 "$target" 120; then
    sleep 3
    from=$(( ${H:-$ready_h} / 100 * 100 - 200 )); [ "$from" -lt 100 ] && from=100
    if compare_checkpoints "$from" "$target"; then sum "RESULT: checkpoints $from..$target MATCH on all 4 (hash-compare.txt)"
    else sum "RESULT: checkpoint MISMATCH/missing (hash-compare.txt)"; fi
else
    sum "RESULT: v3 did not reach checkpoint $target (head $(head_of 3))"
fi
log "ro case $CASE finished"
