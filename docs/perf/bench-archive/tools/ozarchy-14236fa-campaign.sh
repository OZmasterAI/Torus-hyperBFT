#!/usr/bin/env bash
# item 6 baseline of perf/item6-phase1 @ 14236fa (C3+C4+PF1+cooldown fix), 300 markets, crab only.
# Template: ozarchy-c3pf1-campaign.sh step2. Prof flags build; oracle feed 30000/2000ms/walk 0; val0 profiled in both measured cells.
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/item6-phase1
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-14236fa-prof
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

cell() { # label markets dur rate prof(1/0) prof_start prof_dur
    local label=$1 mk=$2 dur=$3 rate=$4 prof=$5 ps=$6 pd=$7 SP=
    quiet || return 1
    echo "[$(date +%T)] START $label markets=$mk rate=$rate prof=$prof load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    [ "$prof" = 1 ] && { "$T/prof-sidecar.sh" "$R/$label" "$ps" "$pd" "$BENCH" "$mk" & SP=$!; }
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    env TARGET_DIR="$TD_C" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 \
        ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 \
        "$HARNESS" "$WT_C" "$label" "$mk" "$dur" "$rate" > "$R/$label.campaign.log" 2>&1
    local rc=$?
    [ -n "$SP" ] && wait "$SP"
    wait "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-14236fa-300m-warm 300 60 76000 0 0 0
cell ozarchy-14236fa-300m-r1   300 120 76000 1 35 45
cell ozarchy-14236fa-300m-r2   300 120 76000 1 35 45
echo "[$(date +%T)] CAMPAIGN END"
