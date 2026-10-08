#!/usr/bin/env bash
# item 6 sync point 2 A/B campaign (ozarchy). SYNC2 vs ITEM6, same bench binary.
set -u
R=/home/oz/bench-results-matched
HARNESS=/home/oz/projects/wt/item6-sync2/tools/matched-bench/run-cell.sh
TOOLS=/home/oz/projects/wt/item6-sync2/tools/matched-bench
POLL=/tmp/claude-1000/-home-oz-projects-torus-economy-Torus-hyperBFT/22e8587a-f8ab-4692-a7ea-56c5e1b05190/scratchpad/s2poll.py
WT_S=/home/oz/projects/wt/item6-sync2
WT_I=/home/oz/projects/wt/item6-phase1
TD_S=/home/oz/.cargo-target-sync2
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

cell() { # label wt targetdir dur blockcap [spam]
    local label=$1 wt=$2 td=$3 dur=$4 cap=$5 spam=${6:-}
    quiet || return 1
    echo "[$(date +%T)] START $label load=$(cut -d' ' -f1 /proc/loadavg)"
    python3 "$POLL" "$R/$label.poll.jsonl" 10 "$R/$label.stopPoll" & local PP=$!
    local SP=""
    if [ -n "$spam" ]; then
        mkdir -p "$R/$label.sys"
        sar -u -r -d 1 > "$R/$label.sys/sar.txt" 2>&1 & SP=$!
        env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP="$cap" RETRY_BUSY=1 ORACLE_FEED=1 \
            SPAM_CANCEL_KEYS=256 SPAM_CANCEL_RATE=2000 SPAM_CANCEL_FUNDED=1 \
            "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    else
        env TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP="$cap" RETRY_BUSY=1 ORACLE_FEED=1 \
            "$HARNESS" "$wt" "$label" 10 "$dur" 76000 > "$R/$label.campaign.log" 2>&1
    fi
    local rc=$?
    touch "$R/$label.stopPoll"; [ -n "$SP" ] && kill "$SP"; wait "$PP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-s2-warm                  "$WT_S" "$TD_S" 60  400
cell ozarchy-s2-item6-r1              "$WT_I" "$TD_I" 120 400
cell ozarchy-s2-sync2-r1              "$WT_S" "$TD_S" 120 400
cell ozarchy-s2-item6-r2              "$WT_I" "$TD_I" 120 400
cell ozarchy-s2-sync2-r2              "$WT_S" "$TD_S" 120 400
cell ozarchy-s2-sync2-spam256-cap20   "$WT_S" "$TD_S" 120 20 spam
echo "[$(date +%T)] CAMPAIGN END"
