#!/usr/bin/env bash
# item 6 IPC / cache-miss measurement: crab 14236fa (node md5 7da38063) vs main 92a02ed (node md5 31a95c65),
# both with the 14236fa bench-throughput (md5 01e5aee3). 300 mk cap400 rate76k RETRY_BUSY 120 s.
# Crab: oracle feed 30000/2000ms/walk 0; main: no oracle. Template: ozarchy-14236fa-campaign.sh / ozarchy-c3pf1-campaign.sh step2.
# val0 perf: ipc-sidecar.sh W1 35..80 s (cyc,ins,L1d miss,DRAM fill,br miss), W2 82..102 s (cyc,ins,L3-hit fill,dTLB miss,IC miss).
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

cell() { # label arm(c|m) dur prof(1/0)
    local label=$1 arm=$2 dur=$3 prof=$4 SP= wt td
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    quiet || return 1
    echo "[$(date +%T)] START $label arm=$arm prof=$prof node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    [ "$prof" = 1 ] && { "$T/ipc-sidecar.sh" "$R/$label" 35 45 2 20 & SP=$!; }
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1)
    [ "$arm" = c ] && E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    env "${E[@]}" "$HARNESS" "$wt" "$label" 300 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    local rc=$?
    [ -n "$SP" ] && wait "$SP"
    wait "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-ipc-warm c 60 0
cell ozarchy-ipc-crab c 120 1
cell ozarchy-ipc-main m 120 1
echo "[$(date +%T)] CAMPAIGN END"
