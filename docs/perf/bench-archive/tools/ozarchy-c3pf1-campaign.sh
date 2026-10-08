#!/usr/bin/env bash
# item 6 C3+PF1 merge measurement (d9ef4f7 vs main 92a02ed), template = ozarchy-pf1-campaign.sh.
# Both nodes built with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only, RUSTFLAGS mold + frame pointers.
# Same C3PF1 bench-throughput for both arms. Crab arm: oracle feed 30000/2000ms/walk 0; main arm: no oracle.
# usage: ozarchy-c3pf1-campaign.sh step1 | step2 | step1-low <rate>
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/item6-c3-pf1
WT_M=/home/oz/projects/wt/main
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-c3pf1-tools
TD_C=/home/oz/.cargo-target-c3pf1-prof                        # C3PF1 node + C3PF1 bench
TD_M=$R/ozarchy-c3pf1-stage/main                               # main node + C3PF1 bench
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

cell() { # label arm(c|m) markets dur rate retry(1/0) prof(1/0) prof_start prof_dur
    local label=$1 arm=$2 mk=$3 dur=$4 rate=$5 retry=$6 prof=$7 ps=$8 pd=$9 SP= wt td
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    quiet || return 1
    echo "[$(date +%T)] START $label arm=$arm markets=$mk rate=$rate retry=$retry prof=$prof load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    [ "$prof" = 1 ] && { "$T/prof-sidecar.sh" "$R/$label" "$ps" "$pd" "$BENCH" "$mk" & SP=$!; }
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1)
    [ "$retry" = 1 ] && E+=(RETRY_BUSY=1)
    [ "$arm" = c ] && E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    env -u RETRY_BUSY "${E[@]}" "$HARNESS" "$wt" "$label" "$mk" "$dur" "$rate" > "$R/$label.campaign.log" 2>&1
    local rc=$?
    [ -n "$SP" ] && wait "$SP"
    wait "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

case "${1:-}" in
step1)
    cell ozarchy-c3pf1-10m-warm c 10 60 76000 1 0 0 0
    for rt in 1 0; do
        cell ozarchy-c3pf1-10m-retry$rt-c3pf1-r1 c 10 120 76000 $rt 0 0 0
        cell ozarchy-c3pf1-10m-retry$rt-main-r1  m 10 120 76000 $rt 0 0 0
        cell ozarchy-c3pf1-10m-retry$rt-c3pf1-r2 c 10 120 76000 $rt 0 0 0
        cell ozarchy-c3pf1-10m-retry$rt-main-r2  m 10 120 76000 $rt 0 0 0
    done ;;
step1-low)
    rate=$2
    for r in 1 2; do
        cell ozarchy-c3pf1-10m-retry1-rate$rate-c3pf1-r$r c 10 120 "$rate" 1 0 0 0
        cell ozarchy-c3pf1-10m-retry1-rate$rate-main-r$r  m 10 120 "$rate" 1 0 0 0
    done ;;
step2)
    cell ozarchy-c3pf1-300m-warm     c 300 60 76000 1 0 0 0
    cell ozarchy-c3pf1-300m-c3pf1-r1 c 300 120 76000 1 1 35 45
    cell ozarchy-c3pf1-300m-main-r1  m 300 120 76000 1 1 35 45
    cell ozarchy-c3pf1-300m-c3pf1-r2 c 300 120 76000 1 0 0 0
    cell ozarchy-c3pf1-300m-main-r2  m 300 120 76000 1 0 0 0 ;;
*) echo "usage: $0 step1 | step2 | step1-low <rate>"; exit 2 ;;
esac
echo "[$(date +%T)] CAMPAIGN END"
