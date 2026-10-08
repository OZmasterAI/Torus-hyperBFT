#!/usr/bin/env bash
# item 6 PF1 gate: full-node A/B + profile, PF1 (0ebfd71) vs main (92a02ed). Reproduces ozarchy-prof10.
# Both nodes built with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only, RUSTFLAGS mold + frame pointers.
# Same PF1 bench-throughput for both arms. perf on val0 via prof10 prof-sidecar.sh (r1 cells only).
set -u
R=/home/oz/bench-results-matched
HARNESS=/home/oz/projects/wt/item6-phase1/tools/matched-bench/run-cell.sh
TOOLS=/home/oz/projects/wt/item6-phase1/tools/matched-bench
SIDE=/home/oz/bench-results-matched/ozarchy-prof10-tools/prof-sidecar.sh
CPUS=/home/oz/bench-results-matched/ozarchy-pf1-tools/cpu-sampler.sh
WT_C=/home/oz/projects/wt/item6-pf1
WT_M=/home/oz/projects/wt/main
TD_C=/home/oz/.cargo-target-pf1-prof                          # PF1 node + PF1 bench
TD_M=/home/oz/bench-results-matched/ozarchy-pf1-stage/main     # main node + PF1 bench
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

cell() { # label wt targetdir dur oracle(1/0) prof(1/0) prof_start prof_dur
    local label=$1 wt=$2 td=$3 dur=$4 orc=$5 prof=$6 ps=$7 pd=$8 SP=
    quiet || return 1
    echo "[$(date +%T)] START $label load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    [ "$prof" = 1 ] && { "$SIDE" "$R/$label" "$ps" "$pd" "$BENCH" & SP=$!; }
    "$CPUS" "$R/$label" & local CP=$!
    if [ "$orc" = 1 ]; then
        env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 RETRY_BUSY=1 ORACLE_FEED=1 ORACLE_PRICE=30000 \
            ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 OVERWRITE=1 \
            "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    else
        env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 RETRY_BUSY=1 OVERWRITE=1 \
            "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    fi
    local rc=$?
    [ -n "$SP" ] && wait "$SP"
    wait "$CP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-pf1-warm     "$WT_C" "$TD_C" 60  1 0 0 0
cell ozarchy-pf1-r1       "$WT_C" "$TD_C" 120 1 1 35 45
cell ozarchy-pf1main-r1   "$WT_M" "$TD_M" 120 0 1 35 45
cell ozarchy-pf1-r2       "$WT_C" "$TD_C" 120 1 0 0 0
cell ozarchy-pf1main-r2   "$WT_M" "$TD_M" 120 0 0 0 0
echo "[$(date +%T)] CAMPAIGN END"
