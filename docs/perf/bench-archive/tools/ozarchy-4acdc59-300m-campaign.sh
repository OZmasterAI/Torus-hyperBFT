#!/usr/bin/env bash
# Gate 2, 300-market shape (section 14): crab perf/item6-phase1 @ 4acdc59 (5524646 + 2333ba4 step 2: end_resident on a
# torus-end-resident worker, begin_resident(N+1) just before new_env + e65411d run-cell.sh samples the end_resident timers)
# vs main 92a02ed, interleaved, both trie maintenance OFF. Copy of ozarchy-c58775f-10m-campaign.sh (section 15 shape, perf via
# prof-sidecar 35/45) at MK=300, plus the per-pid exe md5 check of ozarchy-5524646-10m-campaign.sh. Shape = section 14: 300 markets,
# cap 400, rate 76,000, RETRY_BUSY=1, 120 s; crab with oracle feed 30000/2000ms/walk 0, main without.
# Perf on val0 in the r2 pair only (both arms); r1 pair has no perf. Harness/tools from the 4acdc59 worktree.
# Crab: NO EXTRA_ENV (trie off by default). Main: TORUS_NATIVE_TRIE_MAINTENANCE=0 via EXTRA_ENV.
# Death watcher (not in the repo): kill -0 every 0.5 s on the 3 node pids, the load generator and the oracle feed; a mid-cell death aborts the campaign.
# node-life.txt records each cell's stop-3val window ("stopping pid" first seen .. "stopped." seen): its kill -9 loop runs inside it.
# Both arms use the 4acdc59 bench-throughput. Both arms built with the same flags (mold, frame pointers, line-tables-only).
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/item6-4acdc59
WT_M=/home/oz/projects/wt/main
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-4acdc59
TD_M=$R/ozarchy-4acdc59-stage/main
BENCH=$TD_C/release/bench-throughput
MK=300

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

cell() { # label arm(c|m) dur prof(1/0)
    local label=$1 arm=$2 dur=$3 prof=$4 SP= wt td
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    quiet || return 1
    echo "[$(date +%T)] START $label arm=$arm prof=$prof node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    [ "$prof" = 1 ] && { "$T/prof-sidecar.sh" "$R/$label" 35 45 "$BENCH" "$MK" & SP=$!; }
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
    env "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "${X[@]}" > "$R/$label.campaign.log" 2>&1 &
    local HP=$!
    watch_nodes "$R/$label" "$HP" & local WP=$!
    wait "$HP"; local rc=$?
    wait "$WP"
    if [ -e "$R/$label/node-death.txt" ]; then
        [ -n "$SP" ] && kill "${SP:?}" 2>/dev/null
        kill "${EP:?}" "${CP:?}" "${TP:?}" 2>/dev/null
        for v in 0 1 2; do
            if grep -q "DEATH node val$v " "$R/$label/node-death.txt"; then n=val$v-died; else n=val$v; fi
            gzip -c /home/oz/torus-wsl-devnet/run/val$v.log > "$R/$label/$n.log.gz"
        done
        echo "[$(date +%T)] ABORT $label rc=$rc: $(cat "$R/$label/node-death.txt")"
        return 1
    fi
    [ -n "$SP" ] && wait "$SP"
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    local nm; nm=$(md5sum < "$td/release/torus-node" | cut -c1-8)
    [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
    check "$label" "$arm"
}

cell ozarchy-4acdc59-300m-warm    c 60  0 || exit 1
cell ozarchy-4acdc59-300m-crab-r1 c 120 0 || exit 1
cell ozarchy-4acdc59-300m-main-r1 m 120 0 || exit 1
cell ozarchy-4acdc59-300m-crab-r2 c 120 1 || exit 1
cell ozarchy-4acdc59-300m-main-r2 m 120 1 || exit 1
echo "[$(date +%T)] CAMPAIGN END"
