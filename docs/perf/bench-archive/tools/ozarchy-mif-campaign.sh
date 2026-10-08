#!/usr/bin/env bash
# max-in-flight sweep (s17 bench/max-in-flight harness @ 0d6f2c3, wt/max-in-flight) on ozarchy. ALL cells are main:
# node 31a95c65 (92a02ed, pinned) + bench 9b32d897 (mif), staged in ozarchy-mif-stage/main/release. Section 19's
# 300-market main cell: cap 400, rate 76,000, RETRY_BUSY=1, 120 s cells (warm 60 s), TORUS_NATIVE_TRIE_MAINTENANCE=0
# via EXTRA_ENV, NO perf, NO oracle feed. Labels ozarchy-mif-300m-<tag>.
# Usage: <this> tag:dur:max_in_flight:budget ...   ('-' = unset). Cells run in argument order.
# Copied from ozarchy-walk-5584880-campaign.sh: quiet/envchk/death watcher/check(main)/md5 checks; liqwatch dropped.
# Stop rule: the FIRST capped cell's bench.log has the in-flight tail WARNING (errors/missed > 0) -> stop.
# Failure policy: a cell with rc != 0 or a death is recorded and the campaign goes on, as long as no torus-node runs.
set -u
R=/home/oz/bench-results-matched
SHA=0d6f2c3
WT_H=/home/oz/projects/wt/max-in-flight
WT_M=/home/oz/projects/wt/main
TOOLS=$WT_H/tools/matched-bench
HARNESS=$TOOLS/run-cell.sh
DETACH=$TOOLS/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
TD=$R/ozarchy-mif-stage/main
NODE_MD5=31a95c65
BENCH_MD5=9b32d897
MK=300
P=ozarchy-mif-300m
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
FIRST_CAPPED=1

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1)"; return 1; }
        sleep 10
    done
}

envchk() { # record node environ TRIE var + exe md5 once the bench runs
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
}

death() { # o hp f what comm pid detail now: record a mid-cell death, TERM the harness (its trap stops the rest)
    local o=$1 hp=$2 f=$3 what=$4 comm=$5 pid=$6 detail=$7 now=$8
    echo "DEATH $what comm=$comm pid=$pid detected at $now (0.5 s poll; previous check ok <= ~0.6 s earlier); $detail" | tee -a "$f" > "$o/node-death.txt"
    echo "[$(date +%T)] DEATH $what comm=$comm pid=$pid at $now -> aborting cell (TERM harness $hp)"
    kill -TERM "$hp" 2>/dev/null
}

lastline() { sed -E 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | tail -1 | cut -c1-160; }

watch_nodes() { # unchanged from ozarchy-feeddrain-5584880-cell.sh
    local o=$1 hp=$2 pids= p i t=0 f=$1/node-life.txt bpid= opid= bdone=0 odone=0 now k L
    until pids=$(grep -m1 '\] node pids: ' "$o/run.log" 2>/dev/null | sed 's/.*node pids: //'); [ -n "$pids" ]; do
        kill -0 "$hp" 2>/dev/null || { echo "harness exited before node pids line" > "$f"; return 0; }
        sleep 1; t=$((t+1)); [ $t -gt 600 ] && { echo "no node pids line after 600 s" > "$f"; return 0; }
    done
    echo "$(date +%T) watching node pids: $pids" > "$f"
    while :; do
        if grep -q '^stopping pid ' "$o/run.log"; then stopwin "$o" "$hp" "$f"; return 0; fi
        if [ -z "$opid" ]; then
            opid=$(grep -m1 -oE '\] oracle feed pid [0-9]+;' "$o/run.log" | grep -oE '[0-9]+')
            [ -n "$opid" ] && echo "$(date '+%T.%3N') watching oracle feed pid $opid" >> "$f"
        fi
        if [ -z "$bpid" ] && [ "$bdone" = 0 ] && grep -q '\] bench: ' "$o/run.log"; then
            for k in $(pgrep -f '^/[^ ]*/bench-throughput consensus '); do [ "$(cat /proc/$k/comm 2>/dev/null)" = bench-throughpu ] && bpid=$k; done
            [ -n "$bpid" ] && echo "$(date '+%T.%3N') watching load generator pid $bpid ($(cat /proc/$bpid/comm 2>/dev/null))" >> "$f"
        fi
        i=0
        for p in $pids; do
            if ! kill -0 "$p" 2>/dev/null; then
                now=$(date '+%F %T.%3N')
                grep -q '^stopping pid ' "$o/run.log" && { stopwin "$o" "$hp" "$f"; return 0; }
                death "$o" "$hp" "$f" "node val$i" torus-node "$p" "last val$i log line: $(lastline /home/oz/torus-wsl-devnet/run/val$i.log)" "$now"
                return 3
            fi
            i=$((i+1))
        done
        if [ -n "$bpid" ] && [ "$bdone" = 0 ] && ! kill -0 "$bpid" 2>/dev/null; then
            now=$(date '+%F %T.%3N')
            for k in 1 2 3 4 5 6 7 8 9 10; do L=$(grep -m1 '\] bench exited rc=' "$o/run.log"); [ -n "$L" ] && break; sleep 0.5; done
            if grep -q 'bench exited rc=0 ' <<< "$L"; then
                bdone=1; echo "$now load generator pid $bpid gone; harness: ${L:-none}" >> "$f"
            else
                death "$o" "$hp" "$f" "load generator" bench-throughput "$bpid" "harness: ${L:-no 'bench exited' line within 5 s}; last bench.log line: $(lastline "$o/bench.log")" "$now"
                return 3
            fi
        fi
        if [ -n "$opid" ] && [ "$odone" = 0 ] && ! kill -0 "$opid" 2>/dev/null; then
            now=$(date '+%F %T.%3N')
            for k in 1 2 3 4 5 6; do L=$(grep -m1 "oracle feed pid $opid stopped rc=" "$o/run.log"); [ -n "$L" ] && break; sleep 0.5; done
            if grep -q 'stopped rc=0$' <<< "$L"; then
                odone=1; echo "$now oracle feed pid $opid gone; harness: $L" >> "$f"
            else
                death "$o" "$hp" "$f" "oracle feed" bench-throughput "$opid" "harness: ${L:-no 'oracle feed pid stopped' line within 3 s}; last oracle-feed.log line: $(lastline "$o/oracle-feed.log")" "$now"
                return 3
            fi
        fi
        kill -0 "$hp" 2>/dev/null || { echo "$(date +%T) harness exited, no stopping pid line; all watched pids alive at last check" >> "$f"; return 0; }
        sleep 0.5
    done
}

stopwin() {
    local o=$1 hp=$2 f=$3 t0 t1 n=0
    t0=$(date '+%F %T.%3N')
    echo "$t0 all 3 node pids alive at every 0.5 s check until the harness stop began (stopping pid seen; previous check <= ~0.6 s earlier)" >> "$f"
    until grep -q '^stopped\.$' "$o/run.log"; do sleep 0.2; n=$((n+1)); [ $n -gt 600 ] && { echo "no 'stopped.' line after 120 s" >> "$f"; return 0; }; done
    t1=$(date '+%F %T.%3N')
    echo "stop-3val window (contains its TERM + kill -9 of the node pids $(sed 's/.*node pids: //' <<< "$(grep -m1 '\] node pids: ' "$o/run.log")")): from <= $t0 to $t1 (local, CEST)" >> "$f"
}

check() { # main arm trie setting (unchanged from section 19)
    local o=$R/$1 n
    [ "$(grep -c 'TORUS_NATIVE_TRIE_MAINTENANCE=0' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: main nodes lack =0"; return 1; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance DISABLED')
        [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v DISABLED lines=$n"; return 1; }
    done
    echo "CHECK OK $1"
}

cell() { # tag dur mif budget. return 0 = go on, 1 = abort the campaign
    local label=$P-$1 dur=$2 mif=$3 bud=$4
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    local rcf=$R/$label.cell.rc unit=bench-$label
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label dur=$dur max_in_flight=$mif budget=$bud node_md5=$(md5sum < "$TD/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$TD/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$TD" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1)
    [ "$mif" != - ] && E+=(MAX_IN_FLIGHT="$mif")
    [ "$bud" != - ] && E+=(OPEN_ORDER_BUDGET="$bud")
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_M" "$label" "$MK" "$dur" 76000 "TORUS_NATIVE_TRIE_MAINTENANCE=0" || { kill "$EP" "$CP" "$TP" 2>/dev/null; return 1; }
    local mp= HP= t=0
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; kill "$EP" "$CP" "$TP" 2>/dev/null; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & local WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    local rc; rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    if [ -e "$R/$label/node-death.txt" ]; then
        kill "$EP" "$CP" "$TP" 2>/dev/null
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|WARNING: in-flight' "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /"
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$NODE_MD5" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $NODE_MD5"; return 1; }
        check "$label" || return 1
    fi
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if [ "$mif" != - ]; then
        if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
            if [ "$FIRST_CAPPED" = 1 ]; then echo "[$(date +%T)] STOP: tail WARNING in the first capped cell $label"; return 1; fi
            echo "[$(date +%T)] NOTE: tail WARNING in $label (not the first capped cell; continuing)"
        fi
        FIRST_CAPPED=0
    fi
    return 0
}

[ "$(git -C "$WT_H" rev-parse --short=7 HEAD)" = "$SHA" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_H not clean at $SHA"; exit 1; }
[ -z "$(git -C "$WT_M" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_M dirty"; exit 1; }
[ "$(md5sum < "$TD/release/torus-node" | cut -c1-8)" = $NODE_MD5 ] || { echo "PREFLIGHT FAIL: staged node md5"; exit 1; }
[ "$(md5sum < "$TD/release/bench-throughput" | cut -c1-8)" = $BENCH_MD5 ] || { echo "PREFLIGHT FAIL: staged bench md5"; exit 1; }
grep -qE '/bench-[^/]*\.service$' /proc/self/cgroup || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=$SHA node=$NODE_MD5 bench=$BENCH_MD5 cells: $*"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
[ $# -gt 0 ] || { echo "no cells"; exit 1; }
for spec in "$@"; do
    IFS=: read -r tag dur mif bud <<< "$spec"
    cell "$tag" "$dur" "$mif" "$bud" || exit 1
done
echo "[$(date +%T)] CAMPAIGN END"
