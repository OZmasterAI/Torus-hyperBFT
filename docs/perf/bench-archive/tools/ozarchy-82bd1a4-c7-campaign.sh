#!/usr/bin/env bash
# item 6 C7 on perf/item6-phase1 @ 82bd1a4 (C6a/b/c + liquidation rule B + C7), 300 markets, crab only, trie OFF.
# Copy of ozarchy-14236fa-campaign.sh: prof flags build (line-tables-only, mold + frame pointers),
# oracle feed 30000/2000ms/walk 0; val0 profiled in both measured cells.
# Plus TORUS_NATIVE_TRIE_MAINTENANCE=0 via run-cell.sh EXTRA_ENV (6th arg), env check as ozarchy-trie0-campaign.sh.
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/item6-phase1
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-82bd1a4-prof
BENCH=$TD_C/release/bench-throughput

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

cell() { # label markets dur rate prof(1/0) prof_start prof_dur
    local label=$1 mk=$2 dur=$3 rate=$4 prof=$5 ps=$6 pd=$7 SP=
    quiet || return 1
    echo "[$(date +%T)] START $label markets=$mk rate=$rate prof=$prof node_md5=$(md5sum < "$TD_C/release/torus-node" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    [ "$prof" = 1 ] && { "$T/prof-sidecar.sh" "$R/$label" "$ps" "$pd" "$BENCH" "$mk" & SP=$!; }
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    env TARGET_DIR="$TD_C" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 \
        ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 \
        "$HARNESS" "$WT_C" "$label" "$mk" "$dur" "$rate" "TORUS_NATIVE_TRIE_MAINTENANCE=0" > "$R/$label.campaign.log" 2>&1
    local rc=$?
    [ -n "$SP" ] && wait "$SP"
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-82bd1a4-c7-warm 300 60 76000 0 0 0
cell ozarchy-82bd1a4-c7-r1   300 120 76000 1 35 45
cell ozarchy-82bd1a4-c7-r2   300 120 76000 1 35 45
echo "[$(date +%T)] CAMPAIGN END"
