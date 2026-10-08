#!/usr/bin/env bash
# Gate 2, 10-market shape: crab perf/item6-phase1 @ 5524646 (c58775f + b2bcfaa end_resident timers + 1242d80 sums carry reuses
# decoded positions) vs main 92a02ed, interleaved, both trie maintenance OFF. Copy of ozarchy-c58775f-10m-campaign.sh with NO perf on
# either arm (prof-sidecar removed). Shape = section 5: 10 markets, cap 400, rate 76,000, RETRY_BUSY=1, 120 s; crab with oracle feed
# 30000/2000ms/walk 0, main without. Samplers from ozarchy-14236fa-tools. Harness/tools from the 5524646 worktree (summarize.py knows end_resident).
# Crab: NO EXTRA_ENV (trie off by default). Main: TORUS_NATIVE_TRIE_MAINTENANCE=0 via EXTRA_ENV.
# Both arms use the 5524646 bench-throughput (section 9.1 rule). Both arms built with the same flags (mold, frame pointers, line-tables-only).
set -u
R=/home/oz/bench-results-matched
WT_C=/home/oz/projects/wt/item6-5524646
WT_M=/home/oz/projects/wt/main
HARNESS=$WT_C/tools/matched-bench/run-cell.sh
TOOLS=$WT_C/tools/matched-bench
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-5524646
TD_M=$R/ozarchy-5524646-stage/main
MK=10

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

check() { # label arm: stop the campaign if the trie setting is wrong
    local o=$R/$1 arm=$2 n
    if [ "$arm" = c ]; then
        grep -q 'TORUS_NATIVE_TRIE' "$o/node-environ-trie.txt" && { echo "CHECK FAIL $1: crab node has TRIE var"; return 1; }
        [ "$(grep -c 'TRIE_VAR_MISSING' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: environ not recorded for 3 nodes"; return 1; }
        for v in 0 1 2; do
            n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance off (default')
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
        done
    else
        [ "$(grep -c 'TORUS_NATIVE_TRIE_MAINTENANCE=0' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: main nodes lack =0"; return 1; }
        for v in 0 1 2; do
            n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance DISABLED')
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v DISABLED lines=$n"; return 1; }
        done
    fi
    for v in 0 1 2; do
        zcat "$o/val$v.log.gz" | grep -iE 'rebuil' | grep -iE 'trie' | head -2 | sed "s/^/[$1 val$v rebuild?] /"
    done
    echo "CHECK OK $1"
}

cell() { # label arm(c|m) dur
    local label=$1 arm=$2 dur=$3 wt td
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    quiet || return 1
    echo "[$(date +%T)] START $label arm=$arm node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1)
    local -a X=()
    if [ "$arm" = c ]; then
        E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    else
        X=("TORUS_NATIVE_TRIE_MAINTENANCE=0")
    fi
    env "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "${X[@]}" > "$R/$label.campaign.log" 2>&1
    local rc=$?
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    local nm; nm=$(md5sum < "$td/release/torus-node" | cut -c1-8)
    [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
    check "$label" "$arm"
}

cell ozarchy-5524646-10m-warm    c 60  || exit 1
cell ozarchy-5524646-10m-crab-r1 c 120 || exit 1
cell ozarchy-5524646-10m-main-r1 m 120 || exit 1
cell ozarchy-5524646-10m-crab-r2 c 120 || exit 1
cell ozarchy-5524646-10m-main-r2 m 120 || exit 1
echo "[$(date +%T)] CAMPAIGN END"
