#!/usr/bin/env bash
# item 6 C1 full-node A/B (ozarchy). PRE 9c4be2c vs C1 81a9567, same SYNC2 bench + harness.
# Copy of ozarchy-s2-campaign.sh with arms swapped.
set -u
R=/home/oz/bench-results-matched
HARNESS=/home/oz/projects/wt/item6-sync2/tools/matched-bench/run-cell.sh
TOOLS=/home/oz/projects/wt/item6-sync2/tools/matched-bench
POLL=/tmp/claude-1000/-home-oz-projects-torus-economy-Torus-hyperBFT/22e8587a-f8ab-4692-a7ea-56c5e1b05190/scratchpad/s2poll.py
WT_P=/home/oz/projects/wt/crab-9c4be2c
WT_I=/home/oz/projects/wt/item6-phase1
TD_P=/home/oz/bench-results-matched/ozarchy-s2-stage/pre     # PRE node + SYNC2 bench
TD_I=/home/oz/bench-results-matched/ozarchy-s2-stage/item6   # item6 node + SYNC2 bench

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1)"; return 1; }
        sleep 10
    done
}

cell() { # label wt targetdir dur blockcap
    local label=$1 wt=$2 td=$3 dur=$4 cap=$5
    quiet || return 1
    echo "[$(date +%T)] START $label load=$(cut -d' ' -f1 /proc/loadavg)"
    python3 "$POLL" "$R/$label.poll.jsonl" 10 "$R/$label.stopPoll" & local PP=$!
    env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP="$cap" RETRY_BUSY=1 ORACLE_FEED=1 \
        "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    local rc=$?
    touch "$R/$label.stopPoll"; wait "$PP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-c1ab-warm    "$WT_I" "$TD_I" 60  400
cell ozarchy-c1ab-pre-r1  "$WT_P" "$TD_P" 120 400
cell ozarchy-c1ab-c1-r1   "$WT_I" "$TD_I" 120 400
cell ozarchy-c1ab-pre-r2  "$WT_P" "$TD_P" 120 400
cell ozarchy-c1ab-c1-r2   "$WT_I" "$TD_I" 120 400
echo "[$(date +%T)] CAMPAIGN END"
