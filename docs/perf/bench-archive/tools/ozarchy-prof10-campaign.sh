#!/usr/bin/env bash
# item 6 step 2: 10-market exec-path CPU profile, crab (d52a33f) vs main (92a02ed).
# Both nodes built with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only, RUSTFLAGS mold + frame pointers.
# Same crab bench-throughput for both arms. perf on val0 via prof-sidecar.sh.
set -u
R=/home/oz/bench-results-matched
HARNESS=/home/oz/projects/wt/item6-phase1/tools/matched-bench/run-cell.sh
TOOLS=/home/oz/projects/wt/item6-phase1/tools/matched-bench
SIDE=/home/oz/bench-results-matched/ozarchy-prof10-tools/prof-sidecar.sh
WT_C=/home/oz/projects/wt/item6-phase1
WT_M=/home/oz/projects/wt/main
TD_C=/home/oz/.cargo-target-item6                       # crab node + crab bench
TD_M=/home/oz/bench-results-matched/ozarchy-prof10-stage/main   # main node + crab bench
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

cell() { # label wt targetdir dur oracle(1/0) prof_start prof_dur
    local label=$1 wt=$2 td=$3 dur=$4 orc=$5 ps=$6 pd=$7
    quiet || return 1
    echo "[$(date +%T)] START $label load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    "$SIDE" "$R/$label" "$ps" "$pd" "$BENCH" & local SP=$!
    if [ "$orc" = 1 ]; then
        env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 RETRY_BUSY=1 ORACLE_FEED=1 ORACLE_PRICE=30000 \
            ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 OVERWRITE=1 \
            "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    else
        env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 RETRY_BUSY=1 OVERWRITE=1 \
            "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    fi
    local rc=$?
    wait "$SP"
    echo "[$(date +%T)] END $label rc=$rc sidecar_rc=$? $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-prof10-warm "$WT_C" "$TD_C" 60  1 15 20
cell ozarchy-prof10-crab "$WT_C" "$TD_C" 120 1 35 45
cell ozarchy-prof10-main "$WT_M" "$TD_M" 120 0 35 45
echo "[$(date +%T)] CAMPAIGN END"
