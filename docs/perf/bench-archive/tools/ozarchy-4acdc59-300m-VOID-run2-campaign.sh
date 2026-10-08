#!/usr/bin/env bash
# Gate 2, 300-market shape (section 14): crab perf/item6-phase1 @ 4acdc59 (5524646 + 2333ba4 step 2: end_resident on a
# torus-end-resident worker, begin_resident(N+1) just before new_env + e65411d run-cell.sh samples the end_resident timers)
# vs main 92a02ed, interleaved, both trie maintenance OFF. Copy of ozarchy-c58775f-10m-campaign.sh (section 15 shape, perf via
# prof-sidecar 35/45) at MK=300, plus the per-pid exe md5 check of ozarchy-5524646-10m-campaign.sh. Shape = section 14: 300 markets,
# cap 400, rate 76,000, RETRY_BUSY=1, 120 s; crab with oracle feed 30000/2000ms/walk 0, main without.
# Perf on val0 in the r2 pair only (both arms); r1 pair has no perf. Harness/tools from the 4acdc59 worktree.
# Crab: NO EXTRA_ENV (trie off by default). Main: TORUS_NATIVE_TRIE_MAINTENANCE=0 via EXTRA_ENV.
# Node watcher (not in the repo): kill -0 on the 3 node pids every 2 s; a mid-cell death aborts the campaign.
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

watch_nodes() { # o harness_pid: kill -0 the 3 node pids every 2 s until the harness's stop-3val ("stopping pid") or harness exit.
    # A node gone before "stopping pid" appears = mid-cell death: record it, TERM the harness (its trap stops the rest), exit 3.
    local o=$1 hp=$2 pids= p i t=0 f=$1/node-life.txt
    until pids=$(grep -m1 '\] node pids: ' "$o/run.log" 2>/dev/null | sed 's/.*node pids: //'); [ -n "$pids" ]; do
        kill -0 "$hp" 2>/dev/null || { echo "harness exited before node pids line" > "$f"; return 0; }
        sleep 1; t=$((t+1)); [ $t -gt 600 ] && { echo "no node pids line after 600 s" > "$f"; return 0; }
    done
    echo "$(date +%T) watching pids: $pids" > "$f"
    while :; do
        if grep -q '^stopping pid ' "$o/run.log"; then echo "$(date +%T) all 3 alive until the harness stop (stopping pid seen)" >> "$f"; return 0; fi
        i=0
        for p in $pids; do
            if ! kill -0 "$p" 2>/dev/null; then
                grep -q '^stopping pid ' "$o/run.log" && { echo "$(date +%T) all 3 alive until the harness stop (stopping pid seen)" >> "$f"; return 0; }
                local now; now=$(date '+%F %T.%N')
                echo "NODE DEATH val$i pid=$p detected at $now (2 s poll; previous check ok ~2 s earlier); last val$i log line: $(sed -E 's/\x1b\[[0-9;]*m//g' /home/oz/torus-wsl-devnet/run/val$i.log | tail -1 | cut -c1-160)" | tee -a "$f" > "$o/node-death.txt"
                echo "[$(date +%T)] NODE DEATH val$i pid=$p at $now -> aborting (TERM harness $hp)"
                kill -TERM "$hp" 2>/dev/null
                return 3
            fi
            i=$((i+1))
        done
        kill -0 "$hp" 2>/dev/null || { echo "$(date +%T) harness exited, no stopping pid line; all 3 alive at last check" >> "$f"; return 0; }
        sleep 2
    done
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
            if grep -q "val$v " "$R/$label/node-death.txt"; then n=val$v-died; else n=val$v; fi
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
