#!/usr/bin/env bash
# trie-off A/B (TORUS_NATIVE_TRIE_MAINTENANCE=0 to the nodes via EXTRA_ENV), crab 14236fa vs main 92a02ed, 14236fa bench. No perf.
# both with the 14236fa bench-throughput (md5 01e5aee3). 300 mk cap400 rate76k RETRY_BUSY 120 s.
# Crab: oracle feed 30000/2000ms/walk 0; main: no oracle. Template: ozarchy-14236fa-campaign.sh / ozarchy-c3pf1-campaign.sh step2.
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/item6-phase1
WT_M=/home/oz/projects/wt/main
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-ipc-tools
TD_C=/home/oz/.cargo-target-14236fa-prof
TD_M=$R/ozarchy-ipc-stage/main

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1)"; return 1; }
        sleep 10
    done
}

envchk() { # record node environ TRIE var once the bench runs
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
}

cell() { # label arm(c|m) dur prof(1/0)
    local label=$1 arm=$2 dur=$3 prof=$4 SP= wt td
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    quiet || return 1
    echo "[$(date +%T)] START $label arm=$arm prof=$prof node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & SP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1)
    [ "$arm" = c ] && E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    env "${E[@]}" "$HARNESS" "$wt" "$label" 300 "$dur" 76000 "TORUS_NATIVE_TRIE_MAINTENANCE=0" > "$R/$label.campaign.log" 2>&1
    local rc=$?
    [ -n "$SP" ] && wait "$SP"
    wait "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-trie0-warm c 60 0
cell ozarchy-trie0-crab-r1 c 120 0
cell ozarchy-trie0-main-r1 m 120 0
cell ozarchy-trie0-crab-r2 c 120 0
cell ozarchy-trie0-main-r2 m 120 0
echo "[$(date +%T)] CAMPAIGN END"
