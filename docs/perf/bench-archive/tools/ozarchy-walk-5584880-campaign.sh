#!/usr/bin/env bash
# Moving-price merge-gate pair (item 6, 5584880): crab node 721e48d7 (bench/oracle-feed-drain harness @ ffc245e, wt/feed-drain)
# with ORACLE_FEED_DRAIN=1 at walk 10 bp (W10) and walk 0 (W0), vs main 92a02ed (node 31a95c65, TORUS_NATIVE_TRIE_MAINTENANCE=0 via
# EXTRA_ENV, NO oracle feed; = section 19's main arm). Section 19 300-market shape: cap 400, rate 76,000, RETRY_BUSY=1, 120 s cells,
# warm 60 s (W0 settings), trie off, NO perf, no SIGKILL trace.
# Order: warm, w10-r1, w0-r1, m-r1, w10-r2, w0-r2, m-r2. Labels ozarchy-walk-5584880-300m-<arm>-<r>.
# Both arms run the candidate's bench-throughput cc12451c (main stage dir ozarchy-walk-5584880-stage/main/release).
# quiet/envchk/death watcher/check copied from ozarchy-feeddrain-5584880-cell.sh (= ozarchy-bblind-gate2-campaign.sh).
# Added: liqwatch (from "bench exited" to "stopping pid", every 2 s: liquidation + order counters on all 3 nodes -> liq-drain.tsv).
# Failure policy: a cell with rc != 0 (e.g. a feed-live drain timeout) or a death is recorded and the campaign goes on, as long as
# no torus-node is left running; a wrong binary / trie setting aborts.
# Run detached:  detach.sh walk-5584880-driver <log> bash -c '<this>; echo "exit=$?" > <done>'
set -u
R=/home/oz/bench-results-matched
SHA=ffc245e4d52d21cf5c43cab21b696edfcabc78d6
WT_C=/home/oz/projects/wt/feed-drain
WT_M=/home/oz/projects/wt/main
TOOLS=$WT_C/tools/matched-bench
HARNESS=$TOOLS/run-cell.sh
DETACH=$TOOLS/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
TD_C=/home/oz/.cargo-target-feeddrain-5584880
TD_M=$R/ozarchy-walk-5584880-stage/main
NODE_MD5=721e48d7
MAIN_NODE_MD5=31a95c65
BENCH_MD5=cc12451c
MK=300
P=ozarchy-walk-5584880-300m
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1)"; return 1; }
        sleep 10
    done
}

envchk() { # record node environ TRIE var + exe md5 once the bench runs
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
}

liqwatch() { # o hp: from "bench exited" until "stopping pid" (or harness exit), every 2 s, per node: liquidation + order counters
    local o=$1 hp=$2 f=$1/liq-drain.tsv i m
    until grep -q '\] bench exited rc=' "$o/run.log" 2>/dev/null; do kill -0 "$hp" 2>/dev/null || return 0; sleep 1; done
    echo -e "epoch\tnode\tliquidations\tplaced\tmatched\tresting\trej_margin\tcommitted\texec_lag\tmempool" > "$f"
    while kill -0 "$hp" 2>/dev/null && ! grep -q '^stopping pid ' "$o/run.log"; do
        for i in 0 1 2; do
            m=$(curl -s -m 2 "http://127.0.0.1:916$((i+1))/metrics") || continue
            awk -v ts="$(date +%s.%N | cut -c1-14)" -v n="val$i" '
                $1=="torus_liquidations_triggered_total"{l=$2} $1=="torus_orders_placed_accepted_total"{p=$2}
                $1=="torus_orders_matched_total"{x=$2} $1=="torus_orders_resting_total"{r=$2}
                $1=="torus_orders_rejected_margin_total"{rm=$2} $1=="torus_blocks_committed_total"{c=$2}
                $1=="torus_exec_queue_depth"{q=$2} $1=="torus_mempool_native_size"{mp=$2}
                END{print ts"\t"n"\t"l"\t"p"\t"x"\t"r"\t"rm"\t"c"\t"q"\t"mp}' <<< "$m" >> "$f"
        done
        sleep 2
    done
}

death() { # o hp f what comm pid detail now: record a mid-cell death, TERM the harness (its trap stops the rest)
    local o=$1 hp=$2 f=$3 what=$4 comm=$5 pid=$6 detail=$7 now=$8
    echo "DEATH $what comm=$comm pid=$pid detected at $now (0.5 s poll; previous check ok <= ~0.6 s earlier); $detail" | tee -a "$f" > "$o/node-death.txt"
    echo "[$(date +%T)] DEATH $what comm=$comm pid=$pid at $now -> aborting cell (TERM harness $hp)"
    kill -TERM "$hp" 2>/dev/null
}

lastline() { sed -E 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | tail -1 | cut -c1-160; }

watch_nodes() { # unchanged from ozarchy-feeddrain-5584880-cell.sh
    local o=$1 hp=$2 pids= p i t=0 f=$1/node-life.txt bpid= opid= bdone=0 odone=0 now k L
    until pids=$(grep -m1 '\] node pids: ' "$o/run.log" 2>/dev/null | sed 's/.*node pids: //'); [ -n "$pids" ]; do
        kill -0 "$hp" 2>/dev/null || { echo "harness exited before node pids line" > "$f"; return 0; }
        sleep 1; t=$((t+1)); [ $t -gt 600 ] && { echo "no node pids line after 600 s" > "$f"; return 0; }
    done
    echo "$(date +%T) watching node pids: $pids" > "$f"
    while :; do
        if grep -q '^stopping pid ' "$o/run.log"; then stopwin "$o" "$hp" "$f"; return 0; fi
        if [ -z "$opid" ]; then
            opid=$(grep -m1 -oE '\] oracle feed pid [0-9]+;' "$o/run.log" | grep -oE '[0-9]+')
            [ -n "$opid" ] && echo "$(date '+%T.%3N') watching oracle feed pid $opid" >> "$f"
        fi
        if [ -z "$bpid" ] && [ "$bdone" = 0 ] && grep -q '\] bench: ' "$o/run.log"; then
            for k in $(pgrep -f '^/[^ ]*/bench-throughput consensus '); do [ "$(cat /proc/$k/comm 2>/dev/null)" = bench-throughpu ] && bpid=$k; done
            [ -n "$bpid" ] && echo "$(date '+%T.%3N') watching load generator pid $bpid ($(cat /proc/$bpid/comm 2>/dev/null))" >> "$f"
        fi
        i=0
        for p in $pids; do
            if ! kill -0 "$p" 2>/dev/null; then
                now=$(date '+%F %T.%3N')
                grep -q '^stopping pid ' "$o/run.log" && { stopwin "$o" "$hp" "$f"; return 0; }
                death "$o" "$hp" "$f" "node val$i" torus-node "$p" "last val$i log line: $(lastline /home/oz/torus-wsl-devnet/run/val$i.log)" "$now"
                return 3
            fi
            i=$((i+1))
        done
        if [ -n "$bpid" ] && [ "$bdone" = 0 ] && ! kill -0 "$bpid" 2>/dev/null; then
            now=$(date '+%F %T.%3N')
            for k in 1 2 3 4 5 6 7 8 9 10; do L=$(grep -m1 '\] bench exited rc=' "$o/run.log"); [ -n "$L" ] && break; sleep 0.5; done
            if grep -q 'bench exited rc=0 ' <<< "$L"; then
                bdone=1; echo "$now load generator pid $bpid gone; harness: ${L:-none}" >> "$f"
            else
                death "$o" "$hp" "$f" "load generator" bench-throughput "$bpid" "harness: ${L:-no 'bench exited' line within 5 s}; last bench.log line: $(lastline "$o/bench.log")" "$now"
                return 3
            fi
        fi
        if [ -n "$opid" ] && [ "$odone" = 0 ] && ! kill -0 "$opid" 2>/dev/null; then
            now=$(date '+%F %T.%3N')
            for k in 1 2 3 4 5 6; do L=$(grep -m1 "oracle feed pid $opid stopped rc=" "$o/run.log"); [ -n "$L" ] && break; sleep 0.5; done
            if grep -q 'stopped rc=0$' <<< "$L"; then
                odone=1; echo "$now oracle feed pid $opid gone; harness: $L" >> "$f"
            else
                death "$o" "$hp" "$f" "oracle feed" bench-throughput "$opid" "harness: ${L:-no 'oracle feed pid stopped' line within 3 s}; last oracle-feed.log line: $(lastline "$o/oracle-feed.log")" "$now"
                return 3
            fi
        fi
        kill -0 "$hp" 2>/dev/null || { echo "$(date +%T) harness exited, no stopping pid line; all watched pids alive at last check" >> "$f"; return 0; }
        sleep 0.5
    done
}

stopwin() {
    local o=$1 hp=$2 f=$3 t0 t1 n=0
    t0=$(date '+%F %T.%3N')
    echo "$t0 all 3 node pids alive at every 0.5 s check until the harness stop began (stopping pid seen; previous check <= ~0.6 s earlier)" >> "$f"
    until grep -q '^stopped\.$' "$o/run.log"; do sleep 0.2; n=$((n+1)); [ $n -gt 600 ] && { echo "no 'stopped.' line after 120 s" >> "$f"; return 0; }; done
    t1=$(date '+%F %T.%3N')
    echo "stop-3val window (contains its TERM + kill -9 of the node pids $(sed 's/.*node pids: //' <<< "$(grep -m1 '\] node pids: ' "$o/run.log")")): from <= $t0 to $t1 (local, CEST)" >> "$f"
}

check() { # label arm(c|m): trie setting per arm (unchanged from section 19)
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
    echo "CHECK OK $1"
}

cell() { # label arm(c|m) dur walk_bp: one cell as its own unit bench-<label>. return 0 = go on, 1 = abort the campaign
    local label=$1 arm=$2 dur=$3 walk=$4 wt td
    if [ "$arm" = c ]; then wt=$WT_C td=$TD_C; else wt=$WT_M td=$TD_M; fi
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    local rcf=$R/$label.cell.rc unit=bench-$label
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm walk=$walk node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1)
    local -a X=()
    if [ "$arm" = c ]; then
        E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP="$walk" ORACLE_FEED_DRAIN=1)
    else
        X=("TORUS_NATIVE_TRIE_MAINTENANCE=0")
    fi
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "${X[@]}" || { kill "$EP" "$CP" "$TP" 2>/dev/null; return 1; }
    local mp= HP= t=0
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; kill "$EP" "$CP" "$TP" 2>/dev/null; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & local WP=$!
    liqwatch "$R/$label" "$HP" & local LP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    local rc; rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"; wait "$LP"
    if [ -e "$R/$label/node-death.txt" ]; then
        kill "$EP" "$CP" "$TP" 2>/dev/null
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; return 0
    fi
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    grep -E 'drained=|settle drained|feed-live drain: max' "$R/$label/run.log" | sed "s/^/[$label] /"
    local nm; nm=$(md5sum < "$td/release/torus-node" | cut -c1-8)
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" "$arm" || return 1
    fi
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    return 0
}

[ "$(git -C "$WT_C" rev-parse HEAD)" = "$SHA" ] && [ -z "$(git -C "$WT_C" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_C not clean at $SHA"; exit 1; }
[ -z "$(git -C "$WT_M" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_M dirty"; exit 1; }
[ "$(md5sum < "$TD_C/release/torus-node" | cut -c1-8)" = $NODE_MD5 ] || { echo "PREFLIGHT FAIL: crab node md5"; exit 1; }
[ "$(md5sum < "$TD_C/release/bench-throughput" | cut -c1-8)" = $BENCH_MD5 ] || { echo "PREFLIGHT FAIL: crab bench md5"; exit 1; }
[ "$(md5sum < "$TD_M/release/torus-node" | cut -c1-8)" = $MAIN_NODE_MD5 ] || { echo "PREFLIGHT FAIL: staged main node md5"; exit 1; }
cmp -s "$TD_M/release/bench-throughput" "$TD_C/release/bench-throughput" || { echo "PREFLIGHT FAIL: arms have different bench-throughput"; exit 1; }
grep -qE '/bench-[^/]*\.service$' /proc/self/cgroup || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=$SHA crab=$NODE_MD5 main=$MAIN_NODE_MD5 bench=$BENCH_MD5"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
cell $P-warm  c 60  0  || exit 1
cell $P-w10-r1 c 120 10 || exit 1
cell $P-w0-r1  c 120 0  || exit 1
cell $P-m-r1   m 120 0  || exit 1
cell $P-w10-r2 c 120 10 || exit 1
cell $P-w0-r2  c 120 0  || exit 1
cell $P-m-r2   m 120 0  || exit 1
echo "[$(date +%T)] CAMPAIGN END"
