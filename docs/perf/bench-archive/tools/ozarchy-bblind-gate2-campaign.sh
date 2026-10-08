#!/usr/bin/env bash
# Gate 2, B-blind (item 6, section 9.10): candidate <SHA> (origin/perf/item6-phase1, on top of ab12c75) vs main 92a02ed, interleaved,
# both trie maintenance OFF, NO perf on any cell. Two shapes, 300-market block first, then the 10-market block:
#   300 mk = section 14 / 18 shape, 10 mk = section 15 / 16 shape (same flags): cap 400, rate 76,000, RETRY_BUSY=1, 120 s cells,
#   warm-up 60 s; candidate with oracle feed 30000/2000ms/walk 0, main without (TORUS_NATIVE_TRIE_MAINTENANCE=0 via EXTRA_ENV).
#   Per shape: crab warm, crab r1, main r1, crab r2, main r2 (= ozarchy-4acdc59-300m-campaign.sh / ozarchy-5524646-10m-campaign.sh order).
# Why 300 mk first: it is the shape B-blind targets (the ~8% zero-fill sell cuts are a 300-market effect) and the shape that lost
# processes to the outside SIGKILL; if it aborts, the 10 mk block is not worth the hour on its own.
# Every cell runs as its own transient systemd --user service (unit bench-<label>, via the harness's campaign/detach.sh @ ab12c75);
# run-cell.sh refuses anything else. Run THIS script detached too (see ozarchy-bblind-gate2-launch.sh).
# SIGKILL trace: not part of detach.sh. It is the root-only `perf record -e signal:signal_generate --filter 'sig == 9' -a -g`
# that run 4 of section 18 ran under (started by the owner with sudo in a terminal). This script refuses to start a cell unless that
# trace process is alive (SIGTRACE_REQUIRED=0 overrides) and logs its pid per cell.
# Death watcher, envchk, check(), samplers: copied unchanged from ozarchy-4acdc59-300m-campaign.sh; the harness pid it TERMs is the
# run-cell.sh process inside the cell's unit.
# Prerequisite: ozarchy-bblind-gate2-prep.sh <SHA> (build + stage). Run tools / run-cell.sh from the candidate worktree.
set -u
SHA_IN=${1:?usage: ozarchy-bblind-gate2-campaign.sh <CANDIDATE_SHA>}
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
SHA=$(git -C "$REPO" rev-parse --verify -q "$SHA_IN^{commit}") || { echo "unknown commit $SHA_IN"; exit 1; }
S7=${SHA:0:7}
WT_C=/home/oz/projects/wt/bblind-$S7
WT_M=/home/oz/projects/wt/main
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
DETACH=/home/oz/projects/wt/harness/tools/matched-bench/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-bblind-$S7
TD_M=$R/ozarchy-bblind-$S7-stage/main
MAIN_NODE_MD5=31a95c654761f582a5c124b4146d6524
SIGTRACE_RE='perf record -e signal:signal_generate'
MK=0   # set per block
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

death() { # o hp f what comm pid detail: record a mid-cell death, TERM the harness (its trap stops the rest)
    local o=$1 hp=$2 f=$3 what=$4 comm=$5 pid=$6 detail=$7 now
    now=$8
    echo "DEATH $what comm=$comm pid=$pid detected at $now (0.5 s poll; previous check ok <= ~0.6 s earlier); $detail" | tee -a "$f" > "$o/node-death.txt"
    echo "[$(date +%T)] DEATH $what comm=$comm pid=$pid at $now -> aborting (TERM harness $hp)"
    kill -TERM "$hp" 2>/dev/null
}

lastline() { sed -E 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | tail -1 | cut -c1-160; }

watch_nodes() { # o harness_pid: kill -0 every 0.5 s on the 3 node pids, the bench-throughput load generator and the oracle feed,
    # until the harness's stop-3val ("stopping pid") or harness exit. A node gone before "stopping pid", a load generator gone with
    # no "bench exited rc=0" line within 5 s, or an oracle feed gone with no "oracle feed pid N stopped rc=0" line within 3 s
    # = mid-cell death: record it (time to the ms, comm, pid), TERM the harness, exit 3.
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

stopwin() { # o hp f: the harness stop-3val has begun (its "stopping pid" lines); record the window holding its kill -9 loop.
    local o=$1 hp=$2 f=$3 t0 t1 n=0
    t0=$(date '+%F %T.%3N')
    echo "$t0 all 3 node pids alive at every 0.5 s check until the harness stop began (stopping pid seen; previous check <= ~0.6 s earlier)" >> "$f"
    until grep -q '^stopped\.$' "$o/run.log"; do sleep 0.2; n=$((n+1)); [ $n -gt 600 ] && { echo "no 'stopped.' line after 120 s" >> "$f"; return 0; }; done
    t1=$(date '+%F %T.%3N')
    echo "stop-3val window (contains its TERM + kill -9 of the node pids $(sed 's/.*node pids: //' <<< "$(grep -m1 '\] node pids: ' "$o/run.log")")): from <= $t0 to $t1 (local, CEST)" >> "$f"
}

check() { # label arm: stop the campaign if the trie setting is wrong
    local o=$R/$1 arm=$2 n
    if [ "$arm" = c ]; then
        grep -q 'TORUS_NATIVE_TRIE' "$o/node-environ-trie.txt" && { echo "CHECK FAIL $1: crab node has TRIE var"; return 1; }
        [ "$(grep -c 'TRIE_VAR_MISSING' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: environ not recorded for 3 nodes"; return 1; }
        for v in 0 1 2; do
            n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance off (default')
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
        done
    else
        [ "$(grep -c 'TORUS_NATIVE_TRIE_MAINTENANCE=0' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: main nodes lack =0"; return 1; }
        for v in 0 1 2; do
            n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance DISABLED')
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v DISABLED lines=$n"; return 1; }
        done
    fi
    for v in 0 1 2; do
        zcat "$o/val$v.log.gz" | grep -iE 'rebuil' | grep -iE 'trie' | head -2 | sed "s/^/[$1 val$v rebuild?] /"
    done
    echo "CHECK OK $1"
}

sigtrace() { # print the pid of the running SIGKILL trace, or nothing
    pgrep -f -- "$SIGTRACE_RE" | head -1
}

cell() { # label arm(c|m) dur: one cell as its own systemd --user service bench-<label>
    local label=$1 arm=$2 dur=$3 wt td st
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    quiet || return 1
    st=$(sigtrace)
    if [ -z "$st" ] && [ "${SIGTRACE_REQUIRED:-1}" = 1 ]; then echo "[$(date +%T)] NO SIGKILL TRACE running ($SIGTRACE_RE) -> not starting $label"; return 1; fi
    echo "[$(date +%T)] START $label arm=$arm MK=$MK sigtrace_pid=${st:-none} node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    local rcf=$R/$label.cell.rc unit=bench-$label
    rm -f -- "${R:?}/$label.cell.rc"
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1)
    local -a X=()
    if [ "$arm" = c ]; then
        E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    else
        X=("TORUS_NATIVE_TRIE_MAINTENANCE=0")
    fi
    # unit main process = this bash; `env` execs run-cell.sh as its only child; rc lands in $rcf.
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "${X[@]}" || { kill "${EP:?}" "${CP:?}" "${TP:?}" 2>/dev/null; return 1; }
    local mp= HP= t=0
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "[$(date +%T)] ABORT $label: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; kill "${EP:?}" "${CP:?}" "${TP:?}" 2>/dev/null; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & local WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    local rc; rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    echo "[$(date +%T)] $label sigtrace_pid_end=$(sigtrace || true)"
    if [ -e "$R/$label/node-death.txt" ]; then
        kill "${EP:?}" "${CP:?}" "${TP:?}" 2>/dev/null
        for v in 0 1 2; do
            if grep -q "DEATH node val$v " "$R/$label/node-death.txt"; then n=val$v-died; else n=val$v; fi
            gzip -c /home/oz/torus-wsl-devnet/run/val$v.log > "$R/$label/$n.log.gz"
        done
        echo "[$(date +%T)] ABORT $label rc=$rc: $(cat "$R/$label/node-death.txt")"
        return 1
    fi
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    [ "$rc" = 0 ] || { echo "CHECK FAIL $label: run-cell rc=$rc"; return 1; }
    local nm; nm=$(md5sum < "$td/release/torus-node" | cut -c1-8)
    [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
    check "$label" "$arm"
}

preflight() {
    local f
    for f in "$HARNESS" "$DETACH" "$T/cpu-sampler.sh" "$T/task-sampler.py" "$TD_C/release/torus-node" "$TD_C/release/bench-throughput" \
             "$TD_M/release/torus-node" "$TD_M/release/bench-throughput"; do
        [ -e "$f" ] || { echo "PREFLIGHT FAIL: missing $f (run ozarchy-bblind-gate2-prep.sh $S7)"; return 1; }
    done
    [ "$(git -C "$WT_C" rev-parse HEAD)" = "$SHA" ] && [ -z "$(git -C "$WT_C" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_C not clean at $SHA"; return 1; }
    [ "$(md5sum < "$TD_M/release/torus-node" | cut -d' ' -f1)" = "$MAIN_NODE_MD5" ] || { echo "PREFLIGHT FAIL: staged main node md5"; return 1; }
    cmp -s "$TD_M/release/bench-throughput" "$TD_C/release/bench-throughput" || { echo "PREFLIGHT FAIL: arms have different bench-throughput"; return 1; }
    grep -q "$(md5sum < "$TD_C/release/torus-node" | cut -d' ' -f1)" "$R/ozarchy-bblind-$S7-build/md5s.txt" || { echo "PREFLIGHT FAIL: candidate node != prep build"; return 1; }
    grep -qE '/bench-[^/]*\.service$' /proc/self/cgroup || echo "WARNING: campaign driver is not inside a bench-*.service unit"
    [ -z "$(pgrep -x torus-node)" ] || { echo "PREFLIGHT FAIL: torus-node already running"; return 1; }
    echo "[$(date +%T)] PREFLIGHT OK cand=$SHA node_md5=$(md5sum < "$TD_C/release/torus-node" | cut -c1-8) main_md5=${MAIN_NODE_MD5:0:8} bench_md5=$(md5sum < "$TD_C/release/bench-throughput" | cut -c1-8) sigtrace_pid=$(sigtrace)"
}

[ "${DRY_RUN:-0}" = 1 ] && { preflight; r=$?; echo "DRY_RUN: would run labels:"; for m in 300 10; do for c in warm crab-r1 main-r1 crab-r2 main-r2; do echo "  ozarchy-bblind-$S7-${m}m-$c"; done; done; exit $r; }
preflight || exit 1
P=ozarchy-bblind-$S7
MK=300
cell $P-300m-warm    c 60  || exit 1
cell $P-300m-crab-r1 c 120 || exit 1
cell $P-300m-main-r1 m 120 || exit 1
cell $P-300m-crab-r2 c 120 || exit 1
cell $P-300m-main-r2 m 120 || exit 1
echo "[$(date +%T)] BLOCK END 300m"
MK=10
cell $P-10m-warm    c 60  || exit 1
cell $P-10m-crab-r1 c 120 || exit 1
cell $P-10m-main-r1 m 120 || exit 1
cell $P-10m-crab-r2 c 120 || exit 1
cell $P-10m-main-r2 m 120 || exit 1
echo "[$(date +%T)] CAMPAIGN END"
python3 -I "$R/ozarchy-bblind-gate2-counters.py" "$S7" > "$R/$P-gate2-counters.txt" 2>&1 && echo "counters -> $R/$P-gate2-counters.txt"
