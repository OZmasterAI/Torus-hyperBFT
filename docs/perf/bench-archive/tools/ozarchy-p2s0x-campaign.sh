#!/usr/bin/env bash
# p2s0x (ozarchy): bisect of the 35e69b3 -> d3ba3c0a regression (results doc section 26: B/A 0.937, not the SSTs,
# not the build style; end_resident positions per 1k fills 1.9x, settle pass B +6.9%, match +6%). Copy of
# ozarchy-p2s0r-campaign.sh (cell(), checks, death watcher, fd sampler, quiet-host check, stop rule, failure policy).
# Every arm: node-only build in a fresh unseeded target dir (ozarchy-p2s0x-build.sh; B reused from p2s0r), the same
# bench fff899ca, the 707f132f harness (TOOLS_DIR pinned), oracle feed 30000 / 2000 ms walk 0, trie off by default,
# standard shape (N=4 + budget 900, 300 mk, cap 400, rate 76,000, RETRY_BUSY=1, 120 s), no perf, no extra env.
#   a  = 35e69b3   p0 = 8582e827 (a746c408^1)   p1 = a746c408 (ADL budget, C2 holder lists)
#   p2 = 2ebe1a14 (9e695364^1)   p3 = 9e695364 (exact cost basis, Position v2)   b = d3ba3c0a (193ae781)
# Layout: b-warm 60 s, then a p0 p1 p2 p3 b b p3 p2 p1 p0 a (2 cells per arm, mirrored). Labels ozarchy-p2s0x-300m-<tag>.
# Usage: <this> N (4). DRY_RUN=1: preflight only.
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N   (MAX_IN_FLIGHT, e.g. 4)"; exit 2; }
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
SHA=707f132f07ec31d1e5a3413e98c673a448d5ec84
WT_H=/home/oz/projects/wt/p2s0b-707f132f
TOOLS=$WT_H/tools/matched-bench
HARNESS=$TOOLS/run-cell.sh
DETACH=$TOOLS/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
declare -A WT SHA_OF TD NM
WT[a]=/home/oz/projects/wt/main-35e69b3;     SHA_OF[a]=35e69b3d85c6ba56f24086604b703fcbbf1a15c4
WT[p0]=/home/oz/projects/wt/bisect-8582e827; SHA_OF[p0]=8582e827feed09222e14a889a31c7280e75194a0
WT[p1]=/home/oz/projects/wt/bisect-a746c408; SHA_OF[p1]=a746c408624b0c84729adab7f2f817bf7b6b3827
WT[p2]=/home/oz/projects/wt/bisect-2ebe1a14; SHA_OF[p2]=2ebe1a145d744ef94fdb27a9f3d419b5c5be7b2a
WT[p3]=/home/oz/projects/wt/bisect-9e695364; SHA_OF[p3]=9e695364972ce759e81cbf2dbb7b88989b6ca73d
WT[b]=/home/oz/projects/wt/main-d3ba3c0a;    SHA_OF[b]=d3ba3c0a47dcd0a262bf26dc8fbfa9687842ae4b
ARMS="a p0 p1 p2 p3 b"
for x in a p0 p1 p2 p3; do TD[$x]=$R/ozarchy-p2s0x-stage/$x; done
TD[b]=$R/ozarchy-p2s0r-stage/b
for x in $ARMS; do NM[$x]=$(md5sum < "${TD[$x]}/release/torus-node" | cut -c1-8); done
BENCH=${TD[b]}/release/bench-throughput
MK=300
P=ozarchy-p2s0x-300m
TAGS="b-warm a-r1 p0-r1 p1-r1 p2-r1 p3-r1 b-r1 b-r2 p3-r2 p2-r2 p1-r2 p0-r2 a-r2"
SIGTRACE_RE='perf record -e signal:signal_generate'
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

sigtrace() { pgrep -f -- "$SIGTRACE_RE" | head -1; }

envchk() { # record node environ TRIE var + exe md5 once the bench runs
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
    local v f
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do
        echo "pid=$p $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_BOOK_CF_TARGET_FILE_MB=' || echo BOOKCF_VAR_UNSET)"
    done > "$o/node-bookcf.txt" 2>&1
    for v in 0 1 2; do
        f=$(ls /home/oz/torus-wsl-devnet/data/val$v/OPTIONS-* 2>/dev/null | sort -V | tail -1)
        echo "val$v $(basename "${f:-none}") cf_native_order_books $(awk '/^\[CFOptions "cf_native_order_books"\]/{x=1} x && /target_file_size_base=/{print; exit}' "$f" 2>/dev/null | tr -d ' ')"
    done >> "$o/node-bookcf.txt" 2>&1
}

fdsampler() { # o: node limits once + open fds of the 3 node pids every 5 s until the harness stop (or all pids gone)
    local o=$1 pids= t=0 p line alive
    until pids=$(grep -m1 '\] node pids: ' "$o/run.log" 2>/dev/null | sed 's/.*node pids: //'); [ -n "$pids" ]; do
        sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 0
    done
    for p in $pids; do echo "pid=$p $(grep 'Max open files' /proc/$p/limits 2>/dev/null)"; done > "$o/node-nofile.txt"
    until grep -q '^stopping pid ' "$o/run.log" 2>/dev/null; do
        line=$(date +%s); alive=0
        for p in $pids; do
            if [ -d /proc/$p/fd ]; then line="$line $(ls /proc/$p/fd 2>/dev/null | wc -l)"; alive=1; else line="$line -"; fi
        done
        [ $alive = 0 ] && break
        echo "$line" >> "$o/fds.txt"
        sleep 5
    done
}

fdsum() { # label -> "fds max/end val0 a/b val1 c/d val2 e/f (limit soft hard)"; rc 1 if any max > FD_STOP
    local o=$R/$1
    [ -s "$o/fds.txt" ] || { echo "fds: none recorded"; return 0; }
    awk -v lim="$(awk '{print $5"/"$6; exit}' "$o/node-nofile.txt" 2>/dev/null)" -v stop=$FD_STOP '
        { for (i = 2; i <= 4; i++) if ($i != "-") { if ($i > m[i]) m[i] = $i; e[i] = $i } }
        END { printf "fds max/end val0 %d/%d val1 %d/%d val2 %d/%d (limit soft/hard %s)\n", m[2], e[2], m[3], e[3], m[4], e[4], lim
              exit (m[2] > stop || m[3] > stop || m[4] > stop) }' "$o/fds.txt"
}

death() { # o hp f what comm pid detail now: record a mid-cell death, TERM the harness (its trap stops the rest)
    local o=$1 hp=$2 f=$3 what=$4 comm=$5 pid=$6 detail=$7 now=$8
    echo "DEATH $what comm=$comm pid=$pid detected at $now (0.5 s poll; previous check ok <= ~0.6 s earlier); $detail" | tee -a "$f" > "$o/node-death.txt"
    echo "[$(date +%T)] DEATH $what comm=$comm pid=$pid at $now -> aborting cell (TERM harness $hp)"
    kill -TERM "$hp" 2>/dev/null
}

lastline() { sed -E 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | tail -1 | cut -c1-160; }

watch_nodes() { # unchanged from ozarchy-p2s0-campaign.sh
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

bookcf() { # label arm: no arm sets the var; b (d3ba3c0a) must show the 4 MiB book CF, older arms are recorded
    local o=$R/$1 arm=$2
    { [ "$(grep -c 'BOOKCF_VAR_UNSET' "$o/node-bookcf.txt")" = 3 ] && { [ "$arm" != b ] || [ "$(grep -c 'target_file_size_base=4194304' "$o/node-bookcf.txt")" = 3 ]; }; } \
        || { echo "CHECK FAIL $1: book CF setting for arm $arm: $(tr '\n' ';' < "$o/node-bookcf.txt")"; return 1; }
    echo "CHECK OK $1 book CF: $(grep -o 'target_file_size_base=[0-9]*' "$o/node-bookcf.txt" | sort | uniq -c | tr -s ' ' | tr '\n' ';')"
}

check() { # label: trie off by default on every arm (no TRIE var, one off-default line per validator)
    local o=$R/$1 n
    grep -q 'TORUS_NATIVE_TRIE' "$o/node-environ-trie.txt" && { echo "CHECK FAIL $1: node has TRIE var"; return 1; }
    [ "$(grep -c 'TRIE_VAR_MISSING' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: environ not recorded for 3 nodes"; return 1; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance off (default')
        [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
    done
    echo "CHECK OK $1"; return 0
}

profchk() { # label: perf artifacts present (recorded, never stops the campaign)
    local o=$R/$1 sz
    sz=$(stat -c %s "$o/perf.data" 2>/dev/null || echo 0)
    if [ "$sz" -gt 1000000 ]; then echo "[$1] PERF OK perf.data $((sz / 1048576)) MB"; else echo "[$1] PERF FAIL perf.data size=$sz (see prof-sidecar.log, perf-record.log)"; fi
    [ -f "$o/prof-sidecar.log" ] && sed "s/^/[$1 sidecar] /" "$o/prof-sidecar.log" | grep -E 'perf |bench exit|no bench'
}

cell() { # arm(a|p0|p1|p2|p3|b) tag dur. return 0 = go on, 1 = abort
    local arm=$1 label=$P-$2 dur=$3 prof=0 walk=0 drain=0 byid=0 mif=$N bud=$BUDGET wt td nm xe=
    wt=${WT[$arm]} td=${TD[$arm]} nm=${NM[$arm]}
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    local rcf=$R/$label.cell.rc unit=bench-$label st
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    st=$(sigtrace)
    echo "[$(date +%T)] START $label arm=$arm dur=$dur max_in_flight=$mif budget=$bud prof=$prof walk_bp=$walk feed_drain=$drain extra_env=${xe:-none} sigtrace_pid=${st:-none} node_md5=$(md5sum < "$td/release/torus-node" | cut -c1-8) bench_md5=$(md5sum < "$td/release/bench-throughput" | cut -c1-8) load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    local SP=
    if [ "$prof" = 1 ]; then "$T/prof-sidecar.sh" "$R/$label" 35 45 "$BENCH" "$MK" & SP=$!; fi
    envchk "$R/$label" & local EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & local CP=$!
    python3 "$T/task-sampler.py" "$R/$label" & local TP=$!
    fdsampler "$R/$label" & local FP=$!
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
                ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP="$walk")
    [ "$drain" = 1 ] && E+=(ORACLE_FEED_DRAIN=1)
    [ "$byid" = 1 ] && E+=(CANCEL_BY_ID_FRACTION=0.1 MODIFY_FRACTION=0.05)
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$wt" "$label" "$MK" "$dur" 76000 "$xe" || { kill "$EP" "$CP" "$TP" "$FP" $SP 2>/dev/null; return 1; }
    local mp= HP= t=0
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; kill "$EP" "$CP" "$TP" "$FP" $SP 2>/dev/null; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & local WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    local rc; rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    if [ -e "$R/$label/node-death.txt" ]; then
        kill "$EP" "$CP" "$TP" "$FP" $SP 2>/dev/null
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") sigtrace_pid_end=$(sigtrace || true) -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    [ -n "$SP" ] && wait "$SP"
    wait "$EP" "$CP" "$TP" "$FP"
    echo "[$(date +%T)] END $label rc=$rc sigtrace_pid_end=$(sigtrace || true) $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|Cancel-by-id|WARNING: in-flight' "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /"
    local fdok=0; echo "[$label] $(fdsum "$label")" || true; fdsum "$label" > /dev/null || fdok=1
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" || return 1
        bookcf "$label" "$arm" || return 1
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
    python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); h=s["headline"]; print("[rates] matched/s", h["matched_s_avg"], "placed/s", h["placed_s_avg"], "submit/s", s["ingest"].get("bench_submit_rate"), "native blk/s", h["native_blk_s"])' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    python3 -c 'import json,sys; o=json.load(open(sys.argv[1])).get("oracle_feed") or {}; print("[oracle] stale", o.get("stale_marks_at_bench_end"), "fresh", (o.get("marks_at_bench_end") or {}).get("fresh"), "acc", o.get("accepted"), "/", o.get("sent"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    [ "$prof" = 1 ] && profchk "$label"
    [ "$walk" = 10 ] && { grep -q 'walk_bp=10 ' "$R/$label/run.log" || echo "CHECK WARN $label: run.log does not show walk_bp=10"; }
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    if [ "$fdok" = 1 ]; then echo "[$(date +%T)] STOP: a node held > $FD_STOP open fds in $label (set LimitNOFILE for both arms first)"; return 1; fi
    return 0
}

[ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_H not clean at $SHA"; exit 1; }
for x in $ARMS; do
    [ "$(git -C "${WT[$x]}" rev-parse HEAD)" = "${SHA_OF[$x]}" ] && [ -z "$(git -C "${WT[$x]}" status --porcelain)" ] || { echo "PREFLIGHT FAIL: ${WT[$x]} not clean at ${SHA_OF[$x]}"; exit 1; }
    cmp -s "${TD[$x]}/release/bench-throughput" "$BENCH" || { echo "PREFLIGHT FAIL: arm $x bench differs"; exit 1; }
    [ "$x" = b ] || grep -q "^$x ${SHA_OF[$x]} ${NM[$x]}" "$R/ozarchy-p2s0x-build/md5s.txt" || { echo "PREFLIGHT FAIL: arm $x node md5 ${NM[$x]} not in build md5s"; exit 1; }
done
grep -q 'exit=0' "$R/ozarchy-p2s0x-build/build.done" 2>/dev/null || { echo "PREFLIGHT FAIL: p2s0x build not done"; exit 1; }
[ "${NM[b]}" = 193ae781 ] && [ "$(md5sum < "$BENCH" | cut -c1-8)" = fff899ca ] || { echo "PREFLIGHT FAIL: b node / bench md5"; exit 1; }
[ "$(printf '%s\n' "${NM[@]}" | sort -u | wc -l)" = 6 ] || { echo "PREFLIGHT FAIL: arm nodes not distinct"; exit 1; }
[ -z "$(pgrep -x torus-node)" ] || { echo "PREFLIGHT FAIL: torus-node running"; exit 1; }
[ -z "$(pgrep 'cargo|rustc')" ] || { echo "PREFLIGHT FAIL: cargo/rustc running"; exit 1; }
for f in "$T/cpu-sampler.sh" "$HARNESS" "$DETACH"; do [ -x "$f" ] || { echo "PREFLIGHT FAIL: $f not executable"; exit 1; }; done
[ -f "$T/task-sampler.py" ] && [ -f "$T/positions.py" ] || { echo "PREFLIGHT FAIL: task-sampler.py / positions.py missing"; exit 1; }
grep -q 'CANCEL_BY_ID_FRACTION' "$HARNESS" && grep -q 'proc_cpu_by_node' "$TOOLS/summarize.py" || { echo "PREFLIGHT FAIL: harness lacks the step 0.2 columns"; exit 1; }
for g in $TAGS; do [ ! -e "$R/$P-$g" ] && [ ! -e "$R/$P-$g.cell.rc" ] || { echo "PREFLIGHT FAIL: $R/$P-$g(.cell.rc) exists (old attempt: move it away first)"; exit 1; }; done
[ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge 5000 ] || { echo "PREFLIGHT FAIL: < 5 GB free under $R"; exit 1; }
SELF_UNIT=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
OTHER=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "${SELF_UNIT:-none}")
[ -z "$OTHER" ] || { echo "PREFLIGHT FAIL: other bench units active: $(echo $OTHER)"; exit 1; }
[ -n "$SELF_UNIT" ] || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=${SHA:0:8} nodes=$(for x in $ARMS; do printf "%s=%s," $x ${NM[$x]}; done) bench=$(md5sum < "$BENCH" | cut -c1-8) N=$N budget=$BUDGET nofile_soft=$(ulimit -Sn) sigtrace_pid=$(sigtrace || true) cells: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
cell b b-warm 60 || exit 1
for t in a-r1 p0-r1 p1-r1 p2-r1 p3-r1 b-r1 b-r2 p3-r2 p2-r2 p1-r2 p0-r2 a-r2; do cell "${t%-r*}" "$t" 120 || exit 1; done
echo "[$(date +%T)] CAMPAIGN END"
