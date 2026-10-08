#!/usr/bin/env bash
# feat/action-results (v2 per-action execution failures) on 239ff69, 300 markets, ozarchy.
# Copy of ozarchy-239ff69-campaign.sh: same prof-flags build, same cell shape
# (cap 400, rate 76000, RETRY_BUSY=1, oracle feed 30000/2000ms/walk 0), but NO perf:
# one warm 60 s cell + one 120 s cell. Done marker: ozarchy-action-results.campaign.done
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/action-results
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-action-results-prof

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1)"; return 1; }
        sleep 10
    done
}

envchk() {
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
}

cell() { # label markets dur rate
    local label=$1 mk=$2 dur=$3 rate=$4
    quiet || return 1
    echo "[$(date +%T)] START $label markets=$mk rate=$rate node_md5=$(md5sum < "$TD_C/release/torus-node" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    env TARGET_DIR="$TD_C" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 \
        ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 \
        "$HARNESS" "$WT_C" "$label" "$mk" "$dur" "$rate" > "$R/$label.campaign.log" 2>&1
    local rc=$?
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
}

cell ozarchy-action-results-warm 300 60 76000
cell ozarchy-action-results-r1   300 120 76000
echo "[$(date +%T)] CAMPAIGN END"
echo "exit=0" > $R/ozarchy-action-results.campaign.done
