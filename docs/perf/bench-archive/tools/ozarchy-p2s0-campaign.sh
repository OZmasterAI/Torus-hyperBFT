#!/usr/bin/env bash
# p2s0 (ozarchy): item 6 Phase 2 step 0 profile at 300 markets, crab 59fa407 vs main, MAX_IN_FLIGHT=N + OPEN_ORDER_BUDGET=900
# on every cell. Copy of ozarchy-mif3-campaign.sh (arms, checks, death watcher unchanged) + perf on val0 as in
# ozarchy-4acdc59-300m-campaign.sh:144 (ozarchy-14236fa-tools/prof-sidecar.sh <cell> 35 45 <bench> <MK>).
# Harness bench/max-in-flight @ 0d6f2c3 (wt/max-in-flight).
#   arm m (main): node 31a95c65 (92a02ed) + bench 9b32d897 staged in ozarchy-mif-stage/main/release, worktree wt/main,
#                 TORUS_NATIVE_TRIE_MAINTENANCE=0 via EXTRA_ENV, no oracle feed.
#   arm c (crab): node main@59fa407 (item 6 Phase 1) + the same bench 9b32d897 staged in ozarchy-mif2-stage/crab/release,
#                 worktree wt/main-59fa407, oracle feed 30000 / 2000 ms / walk 0 (SIGSTOPped for the drain), no trie variable.
# Shape: 300 mk uniform, cap 400, rate 76,000, RETRY_BUSY=1, MAX_IN_FLIGHT=N, OPEN_ORDER_BUDGET=900. Labels ozarchy-p2s0-300m-<tag>.
# Layout (fixed, in this order):
#   crab-warm 60 s (no perf); crab-r1, main-r1 120 s (no perf); crab-r2, main-r2 120 s (perf, original sidecar 35/45);
#   crab-w10 120 s: ORACLE_WALK_BP=10 + ORACLE_FEED_DRAIN=1, perf on val0 in two windows (ozarchy-p2s0-tools/prof-sidecar-2win.sh:
#   load 35/45 -> perf.data, drain = DRAIN_PERF_S (25) s at 1999 Hz from the harness "bench exited rc=" line -> perf.drain.data).
# Usage: <this> N        (N = max in flight per sender, e.g. 2, 4 or 8). DRY_RUN=1: preflight only.
# Stop rule: ANY cell whose bench.log has the in-flight tail WARNING -> stop.
# Failure policy: a cell with rc != 0 or a death is recorded and the campaign goes on, as long as no torus-node runs.
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N   (MAX_IN_FLIGHT, e.g. 2, 4 or 8)"; exit 2; }
BUDGET=900
DRAIN_PERF_S=25  # ~6 s of exec backlog after bench exit in the MIF cells (mif2 drains: 16-18 s incl. the 10 s quiet window), then oracle-only blocks every ~2 s (feed round): 25 s gives ~9 of them for row 78
DRAIN_PERF_HZ=1999
R=/home/oz/bench-results-matched
SHA=0d6f2c3
WT_H=/home/oz/projects/wt/max-in-flight
WT_M=/home/oz/projects/wt/main
TOOLS=$WT_H/tools/matched-bench
HARNESS=$TOOLS/run-cell.sh
DETACH=$TOOLS/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
T2=$R/ozarchy-p2s0-tools
WT_C=/home/oz/projects/wt/main-59fa407
SHA_C=59fa407bd4e5c61fc35c784f8e74f24a144d984c
TD=$R/ozarchy-mif-stage/main
TD_C=$R/ozarchy-mif2-stage/crab
NODE_MD5=31a95c65
NODE_MD5_C=$(cut -c1-8 < "$R/ozarchy-mif2-build/md5s.txt" | head -1)
BENCH_MD5=9b32d897
BENCH=$TD_C/release/bench-throughput
MK=300
P=ozarchy-p2s0-300m
TAGS="crab-warm crab-r1 main-r1 crab-r2 main-r2 crab-w10"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}

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

watch_nodes() { # unchanged from ozarchy-mif3-campaign.sh (= ozarchy-feeddrain-5584880-cell.sh)
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

check() { # label arm: trie setting (section 19's check())
    local o=$R/$1 arm=$2 n
    if [ "$arm" = c ]; then
        grep -q 'TORUS_NATIVE_TRIE' "$o/node-environ-trie.txt" && { echo "CHECK FAIL $1: crab node has TRIE var"; return 1; }
        [ "$(grep -c 'TRIE_VAR_MISSING' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: environ not recorded for 3 nodes"; return 1; }
        for v in 0 1 2; do
            n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance off (default')
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
        done
        echo "CHECK OK $1 (crab)"; return 0
    fi
    [ "$(grep -c 'TORUS_NATIVE_TRIE_MAINTENANCE=0' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: main nodes lack =0"; return 1; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance DISABLED')
        [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v DISABLED lines=$n"; return 1; }
    done
    echo "CHECK OK $1"
}

profchk() { # label prof: perf artifacts present (recorded, never stops the campaign)
    local o=$R/$1 f sz
    for f in perf.data $([ "$2" = 2 ] && echo perf.drain.data); do
        sz=$(stat -c %s "$o/$f" 2>/dev/null || echo 0)
        if [ "$sz" -gt 1000000 ]; then echo "[$1] PERF OK $f $((sz / 1048576)) MB"; else echo "[$1] PERF FAIL $f size=$sz (see prof-sidecar.log, perf-record*.log)"; fi
    done
    [ -f "$o/prof-sidecar.log" ] && sed "s/^/[$1 sidecar] /" "$o/prof-sidecar.log" | grep -E 'perf |clock|bench exit|drain window|no bench'
    if [ "$2" = 2 ]; then
        grep -E 'oracle feed ON|feed-live drain|drained=' "$o/run.log" | sed "s/^/[$1] /"
        grep -q 'walk_bp=10 ' "$o/run.log" || echo "CHECK WARN $1: run.log does not show walk_bp=10"
    fi
}

cell() { # arm tag dur prof(0 none | 1 original sidecar | 2 walk 10 + feed drain + 2-window sidecar). return 0 = go on, 1 = abort
    local arm=$1 label=$P-$2 dur=$3 prof=$4 mif=$N bud=$BUDGET wt td nm walk=0
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C nm=$NODE_MD5_C; else wt=$WT_M td=$TD nm=$NODE_MD5; fi
    [ "$prof" = 2 ] && walk=10
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    local rcf=$R/$label.cell.rc unit=bench-$label
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm dur=$dur max_in_flight=$mif budget=$bud prof=$prof walk_bp=$([ "$arm" = c ] && echo $walk || echo -) feed_drain=$([ "$prof" = 2 ] && echo 1 || echo 0) node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    local SP=
    if [ "$prof" = 1 ]; then "$T/prof-sidecar.sh" "$R/$label" 35 45 "$BENCH" "$MK" & SP=$!; fi
    if [ "$prof" = 2 ]; then "$T2/prof-sidecar-2win.sh" "$R/$label" 35 45 "$DRAIN_PERF_S" "$DRAIN_PERF_HZ" & SP=$!; fi
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud")
    local -a X=()
    if [ "$arm" = c ]; then
        E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP="$walk")
        [ "$prof" = 2 ] && E+=(ORACLE_FEED_DRAIN=1)
    else
        X=("TORUS_NATIVE_TRIE_MAINTENANCE=0")
    fi
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "${X[@]}" || { kill "$EP" "$CP" "$TP" $SP 2>/dev/null; return 1; }
    local mp= HP= t=0
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; kill "$EP" "$CP" "$TP" $SP 2>/dev/null; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & local WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    local rc; rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    if [ -e "$R/$label/node-death.txt" ]; then
        kill "$EP" "$CP" "$TP" $SP 2>/dev/null
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    [ -n "$SP" ] && wait "$SP"
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|WARNING: in-flight' "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /"
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" "$arm" || return 1
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
    if [ "$arm" = c ]; then
        python3 -c 'import json,sys; o=json.load(open(sys.argv[1])).get("oracle_feed") or {}; print("[oracle] stale", o.get("stale_marks_at_bench_end"), "fresh", (o.get("marks_at_bench_end") or {}).get("fresh"), "acc", o.get("accepted"), "/", o.get("sent"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    fi
    [ "$prof" != 0 ] && profchk "$label" "$prof"
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    return 0
}

[ "$(git -C "$WT_H" rev-parse --short=7 HEAD)" = "$SHA" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_H not clean at $SHA"; exit 1; }
[ "$(git -C "$WT_M" rev-parse --short=7 HEAD)" = 1cd786d ] && [ -z "$(git -C "$WT_M" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_M not clean at 1cd786d"; exit 1; }
[ "$(git -C "$WT_C" rev-parse HEAD)" = "$SHA_C" ] && [ -z "$(git -C "$WT_C" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_C not clean at $SHA_C"; exit 1; }
grep -q 'exit=0' "$R/ozarchy-mif2-build/build.done" 2>/dev/null || { echo "PREFLIGHT FAIL: mif2 build not done"; exit 1; }
[ "$(md5sum < "$TD_C/release/torus-node" | cut -c1-8)" = "$NODE_MD5_C" ] && [ "$NODE_MD5_C" != "$NODE_MD5" ] || { echo "PREFLIGHT FAIL: staged crab node md5"; exit 1; }
cmp -s "$TD_C/release/bench-throughput" "$TD/release/bench-throughput" || { echo "PREFLIGHT FAIL: arms have different bench"; exit 1; }
[ -z "$(pgrep -x torus-node)" ] || { echo "PREFLIGHT FAIL: torus-node running"; exit 1; }
[ "$(md5sum < "$TD/release/torus-node" | cut -c1-8)" = $NODE_MD5 ] || { echo "PREFLIGHT FAIL: staged node md5"; exit 1; }
[ "$(md5sum < "$TD/release/bench-throughput" | cut -c1-8)" = $BENCH_MD5 ] || { echo "PREFLIGHT FAIL: staged bench md5"; exit 1; }
command -v perf >/dev/null || { echo "PREFLIGHT FAIL: no perf"; exit 1; }
[ "$(cat /proc/sys/kernel/perf_event_paranoid)" -le 2 ] || { echo "PREFLIGHT FAIL: perf_event_paranoid > 2 (cycles:u -p needs <= 2)"; exit 1; }
for f in "$T/prof-sidecar.sh" "$T2/prof-sidecar-2win.sh" "$T/cpu-sampler.sh"; do [ -x "$f" ] || { echo "PREFLIGHT FAIL: $f not executable"; exit 1; }; done
[ -f "$T/task-sampler.py" ] && [ -f "$T/positions.py" ] || { echo "PREFLIGHT FAIL: task-sampler.py / positions.py missing"; exit 1; }
for g in $TAGS; do [ ! -e "$R/$P-$g" ] && [ ! -e "$R/$P-$g.cell.rc" ] || { echo "PREFLIGHT FAIL: $R/$P-$g(.cell.rc) exists (old attempt: move it away first)"; exit 1; }; done
[ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge 3000 ] || { echo "PREFLIGHT FAIL: < 3 GB free under $R"; exit 1; }
SELF_UNIT=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
OTHER=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "${SELF_UNIT:-none}")
[ -z "$OTHER" ] || { echo "PREFLIGHT FAIL: other bench units active: $(echo $OTHER)"; exit 1; }
[ -n "$SELF_UNIT" ] || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=$SHA main_node=$NODE_MD5 crab_node=$NODE_MD5_C bench=$BENCH_MD5 N=$N budget=$BUDGET drain_perf_s=$DRAIN_PERF_S drain_perf_hz=$DRAIN_PERF_HZ cells: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
cell c crab-warm 60  0 || exit 1
cell c crab-r1   120 0 || exit 1
cell m main-r1   120 0 || exit 1
cell c crab-r2   120 1 || exit 1
cell m main-r2   120 1 || exit 1
cell c crab-w10  120 2 || exit 1
echo "[$(date +%T)] CAMPAIGN END"
