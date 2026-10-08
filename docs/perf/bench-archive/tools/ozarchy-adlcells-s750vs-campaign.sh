#!/usr/bin/env bash
# adlcells-s750vs (ozarchy, s25): rerun ONLY the s750vs cell (S=750, TORUS_LIQ_VALUE_SUM=1) of the s22 adlcells campaign
# (copy of ozarchy-adlcells-campaign.sh) with the drain fix: harness wt/liq-stress @ 95f50cd adds ORACLE_FEED_DRAIN_MAX_LAG;
# value-sum cells pass ORACLE_FEED_DRAIN_MAX_LAG=1000 (exec ~0.34 s/block keeps the lag ~60 while the feed runs, so the
# feed-live drain's default lag bound 2 never held; s400vs hit the 780 s timeout). Same node/bench binaries (18bf0759 / ec4e14a9).
# adlcells (ozarchy): ADL-budget proof cells for perf/adl-budget 6a25e20 (A1-A8 + review fixes + drain caches). Copy of
# ozarchy-liq-campaign.sh with: node built from the detached worktree wt/adl-cells-6a25e20 (passed to run-cell.sh as the
# cell worktree), harness/tools from wt/liq-stress @ b49e40b, EXPECT_NODE_MD5/EXPECT_BENCH_MD5 set, the node environ also
# records TORUS_LIQ_VALUE_SUM, two value-sum cells (EXTRA_ENV TORUS_LIQ_VALUE_SUM=1; untimed = same shape, their timings
# are not read because the value-sum walk runs in the exec thread every native block), 'liquidation: value sum' lines
# extracted, and a tail WARNING recorded instead of stopping the campaign.
# Layout: warm 60 s; s400 120 s; s750 120 s (value sum off, timed); s400vs 120 s; s750vs 120 s (value sum on, untimed).
# --- original header (ozarchy-liq-campaign.sh):
# liq (ozarchy, campaign B): liquidation stress (row 76) at 300 markets, crab arm only, bench/liq-stress @ af8529e
# (= main 35e69b3 + tools-only changes; harness + worktree wt/liq-stress). Copy of ozarchy-bd-campaign.sh (checks, death
# watcher, genesis check unchanged) with: oracle feed 30000 / 2000 ms / walk 10 + ORACLE_FEED_DRAIN=1 on every cell;
# stress cells add LIQ_THIN=200 ORACLE_SHOCK_BP=S ORACLE_SHOCK_ROUND=45, the thin-snap.py sidecar (val0 balances of the
# 200 thin senders every 5 s), and after the cell liq_stress.py + the nodes' 'liquidation: ADL' / 'liquidation step' lines.
# Shape: 300 mk uniform, cap 400, rate 76,000, RETRY_BUSY=1, MAX_IN_FLIGHT=N, OPEN_ORDER_BUDGET=900. Labels ozarchy-liq-300m-<tag>.
# Layout: warm 60 s (no shock/thin); s400 120 s; s750 120 s.  Usage: <this> N   DRY_RUN=1: preflight only.
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N"; exit 2; }
BUDGET=900
R=/home/oz/bench-results-matched
SHA=95f50cd
WT_H=/home/oz/projects/wt/liq-stress
TOOLS=$WT_H/tools/matched-bench
HARNESS=$TOOLS/run-cell.sh
DETACH=/home/oz/projects/torus-economy/Torus-hyperBFT/tools/matched-bench/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
LT=$R/ozarchy-liq-tools
WT_C=/home/oz/projects/wt/adl-cells-6a25e20
NODE_SHA=6a25e20
TD_C=$R/ozarchy-adlcells-stage
NODE_MD5_FULL=$(head -1 "$R/ozarchy-adlcells-build/md5s.txt" | cut -c1-32)
BENCH_MD5_FULL=$(sed -n 3p "$R/ozarchy-adlcells-build/md5s.txt" | cut -c1-32)
NODE_MD5_C=${NODE_MD5_FULL:0:8}
BENCH_MD5=${BENCH_MD5_FULL:0:8}
MK=300
P=ozarchy-adlcells-300m
TAGS="s750vs"
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
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING) LVS=$(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_LIQ_VALUE_SUM=' || echo none)"
    done > "$o/node-environ-trie.txt" 2>&1
}

death() { # o hp f what comm pid detail now: record a mid-cell death, TERM the harness (its trap stops the rest)
    local o=$1 hp=$2 f=$3 what=$4 comm=$5 pid=$6 detail=$7 now=$8
    echo "DEATH $what comm=$comm pid=$pid detected at $now (0.5 s poll; previous check ok <= ~0.6 s earlier); $detail" | tee -a "$f" > "$o/node-death.txt"
    echo "[$(date +%T)] DEATH $what comm=$comm pid=$pid at $now -> aborting cell (TERM harness $hp)"
    kill -TERM "$hp" 2>/dev/null
}

lastline() { sed -E 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | tail -1 | cut -c1-160; }

watch_nodes() { # unchanged from ozarchy-mif3-campaign.sh (= ozarchy-feeddrain-5584880-cell.sh)
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

check() { # label arm: trie setting (section 19's check())
    local o=$R/$1 arm=$2 n
    if [ "$arm" = c ]; then
        grep -q 'TORUS_NATIVE_TRIE' "$o/node-environ-trie.txt" && { echo "CHECK FAIL $1: crab node has TRIE var"; return 1; }
        [ "$(grep -c 'TRIE_VAR_MISSING' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: environ not recorded for 3 nodes"; return 1; }
        for v in 0 1 2; do
            n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance off (default')
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
        done
        echo "CHECK OK $1 (crab)"; return 0
    fi
    [ "$(grep -c 'TORUS_NATIVE_TRIE_MAINTENANCE=0' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: main nodes lack =0"; return 1; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance DISABLED')
        [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v DISABLED lines=$n"; return 1; }
    done
    echo "CHECK OK $1"
}

genchk() { # label wt t_start: genesis regenerated in this cell's worktree during this cell; file md5 == run.log md5
    local o=$R/$1 wt=$2 t0=$3 g=$2/devnet/wsl/genesis-3val.json lm fm mt
    grep -q "genesis -> $g\$" "$o/run.log" || { echo "CHECK FAIL $1: run.log genesis path is not $g"; return 1; }
    lm=$(grep -m1 -oE 'genesis markets=[0-9]+ native_balances=[0-9]+ md5=[0-9a-f]+' "$o/run.log" | sed 's/.*md5=//')
    fm=$(md5sum < "$g" | cut -d' ' -f1); mt=$(stat -c %Y "$g")
    [ -n "$lm" ] && [ "${fm:0:${#lm}}" = "$lm" ] || [ "$fm" = "$lm" ] || { echo "CHECK FAIL $1: genesis md5 file=$fm run.log=$lm"; return 1; }
    [ "$mt" -ge "$t0" ] || { echo "CHECK FAIL $1: genesis mtime $mt < cell start $t0 (not regenerated)"; return 1; }
    grep -q 'genesis markets=300 native_balances=100060 ' "$o/run.log" || { echo "CHECK FAIL $1: genesis native_balances != 100060"; return 1; }
    cp -p "$g" "$o/genesis-3val.json"
    echo "GENESIS OK $1 md5=$fm chain_id=$(jq -c '.chain_id // .chainId // .config.chain_id // "absent"' "$g") $(grep -m1 -oE 'genesis markets=[0-9]+ native_balances=[0-9]+' "$o/run.log")"
}

profchk() { # label prof: perf artifacts present (recorded, never stops the campaign)
    local o=$R/$1 f sz
    for f in perf.data $([ "$2" = 2 ] && echo perf.drain.data); do
        sz=$(stat -c %s "$o/$f" 2>/dev/null || echo 0)
        if [ "$sz" -gt 1000000 ]; then echo "[$1] PERF OK $f $((sz / 1048576)) MB"; else echo "[$1] PERF FAIL $f size=$sz (see prof-sidecar.log, perf-record*.log)"; fi
    done
    [ -f "$o/prof-sidecar.log" ] && sed "s/^/[$1 sidecar] /" "$o/prof-sidecar.log" | grep -E 'perf |clock|bench exit|drain window|no bench'
    if [ "$2" = 2 ]; then
        grep -E 'oracle feed ON|feed-live drain|drained=' "$o/run.log" | sed "s/^/[$1] /"
        grep -q 'walk_bp=10 ' "$o/run.log" || echo "CHECK WARN $1: run.log does not show walk_bp=10"
    fi
}

cell() { # arm tag dur shock_bp (0 = warm: no shock, no thin) vs (1 = TORUS_LIQ_VALUE_SUM=1). return 0 = go on, 1 = abort
    local arm=$1 label=$P-$2 dur=$3 shock=$4 vs=${5:-0} prof=0 mif=$N bud=$BUDGET wt=$WT_C td=$TD_C nm=$NODE_MD5_C walk=10
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    local rcf=$R/$label.cell.rc unit=bench-$label
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm dur=$dur max_in_flight=$mif budget=$bud prof=$prof walk_bp=$([ "$arm" = c ] && echo $walk || echo -) feed_drain=1 shock_bp=$shock value_sum=$vs liq_thin=$([ "$shock" != 0 ] && echo 200 || echo 0) node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    local t_start; t_start=$(date +%s)
    local SP=
    [ "$shock" != 0 ] && { python3 "$LT/thin-snap.py" "$R/$label" "$wt/devnet/wsl/genesis-3val.json" 200 5 2> "$R/$label/thin-snap.err" & SP=$!; }
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    local -a E=(TARGET_DIR="$td" EXPECT_NODE_MD5="$NODE_MD5_FULL" EXPECT_BENCH_MD5="$BENCH_MD5_FULL" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud")
    local -a X=()
    [ "$vs" = 1 ] && X=("TORUS_LIQ_VALUE_SUM=1")
    [ "$vs" = 1 ] && E+=(ORACLE_FEED_DRAIN_MAX_LAG=1000)
    E+=(ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP="$walk" ORACLE_FEED_DRAIN=1)
    [ "$shock" != 0 ] && E+=(LIQ_THIN=200 ORACLE_SHOCK_BP="$shock" ORACLE_SHOCK_ROUND=45)
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "${X[@]}" || { kill "$EP" "$CP" "$TP" $SP 2>/dev/null; return 1; }
    local mp= HP= t=0
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; kill "$EP" "$CP" "$TP" $SP 2>/dev/null; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & local WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    local rc; rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    if [ -e "$R/$label/node-death.txt" ]; then
        kill "$EP" "$CP" "$TP" $SP 2>/dev/null
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    [ -n "$SP" ] && { kill "$SP" 2>/dev/null; wait "$SP"; }
    wait "$EP" "$CP" "$TP"
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|WARNING: in-flight' "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /"
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" "$arm" || return 1
        if [ "$vs" = 1 ]; then
            [ "$(grep -c 'LVS=TORUS_LIQ_VALUE_SUM=1$' "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: TORUS_LIQ_VALUE_SUM=1 not in all 3 node environs"; return 1; }
        else
            [ "$(grep -c 'LVS=none$' "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: TORUS_LIQ_VALUE_SUM set on a value-sum-off cell"; return 1; }
        fi
        echo "CHECK OK $label value_sum=$vs (node environ)"
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
    genchk "$label" "$wt" "$t_start" || return 1
    if [ "$arm" = c ]; then
        python3 -c 'import json,sys; o=json.load(open(sys.argv[1])).get("oracle_feed") or {}; print("[oracle] stale", o.get("stale_marks_at_bench_end"), "fresh", (o.get("marks_at_bench_end") or {}).get("fresh"), "acc", o.get("accepted"), "/", o.get("sent"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    fi
    grep -E 'feed-live drain|drained=|liquidation stress ON|LIQ_THIN=|liquidator vault:' "$R/$label/run.log" | sed "s/^/[$label] /"
    for v in 0 1 2; do
        zcat "$R/$label/val$v.log.gz" 2>/dev/null | grep -E 'liquidation: ADL|liquidation step' | sed -E 's/\x1b\[[0-9;]*m//g' > "$R/$label/liq-lines-val$v.txt"
        zcat "$R/$label/val$v.log.gz" 2>/dev/null | grep -E 'liquidation: value sum' | sed -E 's/\x1b\[[0-9;]*m//g' > "$R/$label/value-sum-val$v.txt"
        zcat "$R/$label/val$v.log.gz" 2>/dev/null | sed -E 's/\x1b\[[0-9;]*m//g' | grep -E ' (ERROR|WARN) ' | grep -iE 'liquidat|adl|escrow|D9|fatal' > "$R/$label/liq-errors-val$v.txt"
        echo "[$label] val$v: 'liquidation step' lines=$(grep -c 'liquidation step' "$R/$label/liq-lines-val$v.txt") 'ADL to escrow'=$(grep -c 'liquidation: ADL to escrow' "$R/$label/liq-lines-val$v.txt") 'ADL escrow pairing'=$(grep -c 'ADL escrow pairing' "$R/$label/liq-lines-val$v.txt") 'ADL escrow dust'=$(grep -c 'ADL escrow dust' "$R/$label/liq-lines-val$v.txt") 'value sum'=$(grep -c . "$R/$label/value-sum-val$v.txt") liq ERROR/WARN=$(grep -c . "$R/$label/liq-errors-val$v.txt")"
    done
    if [ "$shock" != 0 ]; then
        python3 "$TOOLS/liq_stress.py" "$R/$label" > "$R/$label/liq-stress.out" 2>&1; echo "[$label] liq_stress.py rc=$? -> liq-stress.json"
    fi
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] WARN: tail WARNING in capped cell $label (recorded, continuing)"
    fi
    return 0
}

[ "$(git -C "$WT_H" rev-parse --short=7 HEAD)" = "$SHA" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_H not clean at $SHA"; exit 1; }
[ "$(git -C "$WT_C" rev-parse --short=7 HEAD)" = "$NODE_SHA" ] && [ -z "$(git -C "$WT_C" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_C not clean at $NODE_SHA"; exit 1; }
grep -q EXPECT_NODE_MD5 "$HARNESS" || { echo "PREFLIGHT FAIL: harness lacks EXPECT_NODE_MD5"; exit 1; }
for k in ORACLE_FEED_DRAIN_MAX_LAG ORACLE_FEED_DRAIN MAX_IN_FLIGHT LIQ_THIN ORACLE_SHOCK_BP; do grep -q "$k" "$HARNESS" || { echo "PREFLIGHT FAIL: harness lacks $k"; exit 1; }; done
grep -q 'exit=0' "$R/ozarchy-adlcells-build/build.done" 2>/dev/null || { echo "PREFLIGHT FAIL: adlcells build not done"; exit 1; }
[ "$(md5sum < "$TD_C/release/torus-node" | cut -c1-8)" = "$NODE_MD5_C" ] || { echo "PREFLIGHT FAIL: staged node md5"; exit 1; }
[ "$(md5sum < "$TD_C/release/bench-throughput" | cut -c1-8)" = "$BENCH_MD5" ] || { echo "PREFLIGHT FAIL: staged bench md5"; exit 1; }
[ -z "$(pgrep -x torus-node)" ] || { echo "PREFLIGHT FAIL: torus-node running"; exit 1; }
[ -z "$(pgrep 'cargo|rustc')" ] || { echo "PREFLIGHT FAIL: cargo/rustc running"; exit 1; }
[ -x "$T/cpu-sampler.sh" ] && [ -f "$T/task-sampler.py" ] && [ -f "$LT/thin-snap.py" ] && [ -f "$TOOLS/liq_stress.py" ] || { echo "PREFLIGHT FAIL: tools missing"; exit 1; }
for g in $TAGS; do [ ! -e "$R/$P-$g" ] && [ ! -e "$R/$P-$g.cell.rc" ] || { echo "PREFLIGHT FAIL: $R/$P-$g(.cell.rc) exists (old attempt: move it away first)"; exit 1; }; done
[ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge 60000 ] || { echo "PREFLIGHT FAIL: < 60 GB free under $R"; exit 1; }
SELF_UNIT=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
OTHER=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "${SELF_UNIT:-none}")
[ -z "$OTHER" ] || { echo "PREFLIGHT FAIL: other bench units active: $(echo $OTHER)"; exit 1; }
[ -n "$SELF_UNIT" ] || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=$SHA node_src=$NODE_SHA node=$NODE_MD5_C bench=$BENCH_MD5 N=$N budget=$BUDGET cells: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
cell c s750vs 120 750 1 || exit 1
echo "[$(date +%T)] CAMPAIGN END"
