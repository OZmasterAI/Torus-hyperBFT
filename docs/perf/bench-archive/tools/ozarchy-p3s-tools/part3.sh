
walksweep() { # o: hard-link every val0 WAL file (*.log) that is not linked yet into o/wal-val0-keep. A hard link keeps the inode
    # (every byte the node wrote) after RocksDB unlinks the name. A file that vanished before its link is logged as a MISS.
    local o=$1 d=$DEVNET/data/val0 k=$1/wal-val0-keep f b
    mkdir -p "$k"
    for f in "$d"/*.log; do
        [ -e "$f" ] || continue
        b=$(basename "$f")
        [ -e "$k/$b" ] && continue
        if ln "$f" "$k/$b" 2>/dev/null; then echo "$(date +%s.%N) LINK $b $(stat -c %s "$k/$b")" >> "$k/link-log.txt"
        else echo "$(date +%s.%N) MISS $b" >> "$k/miss.txt"; fi
    done
}

walkeep() { # o: keep the val0 WAL files. Starts only after the harness prints 'node pids' (launch-3val has wiped the data dir by then).
    local o=$1 t=0
    until grep -q '\] node pids: ' "$o/run.log" 2>/dev/null; do sleep 0.5; t=$((t+1)); [ $t -gt 1200 ] && return 0; done
    while :; do walksweep "$o"; sleep 0.2; done
}

walkcheck() { # label: after the cell: linked WAL files vs. the files still in the data dir (summary line; WARN on any miss)
    local o=$R/$1 k=$R/$1/wal-val0-keep n inlink inlist ms
    n=$(ls "$k" 2>/dev/null | grep -c '\.log$'); ms=$(wc -l < "$k/miss.txt" 2>/dev/null || echo 0)
    inlist=$(ls "$DEVNET/data/val0" 2>/dev/null | grep '\.log$' | sort | tr '\n' ' ')
    inlink=$(ls "$k" 2>/dev/null | grep '\.log$' | sort | tr '\n' ' ')
    local left=0 f; for f in $inlist; do case " $inlink " in *" $f "*) ;; *) left=$((left+1)) ;; esac; done
    echo "[$1] WAL keep val0: $n WAL files linked, $(du -cb "$k"/*.log 2>/dev/null | tail -1 | cut -f1) bytes, misses=$ms, not-linked-but-still-present=$left"
    [ "$ms" = 0 ] || echo "CHECK WARN $1: $ms WAL file(s) missed by the hard-link sampler (see $k/miss.txt): the WAL decode of this cell is incomplete"
}

profchk() { # label: perf artifacts present (recorded, never stops the campaign)
    local o=$R/$1 sz
    sz=$(stat -c %s "$o/perf.data" 2>/dev/null || echo 0)
    if [ "$sz" -gt 1000000 ]; then echo "[$1] PERF OK perf.data $((sz / 1048576)) MB"; else echo "[$1] PERF FAIL perf.data size=$sz (see prof-sidecar.log, perf-record.log)"; fi
    [ -f "$o/prof-sidecar.log" ] && sed "s/^/[$1 sidecar] /" "$o/prof-sidecar.log" | grep -E 'perf |bench exit|no bench'
}

cell() { # tag dur crash(0|1) killat keep(0|1). return 0 = go on, 1 = abort
    local tg=$1 label=$P-$1 dur=$2 crash=$3 killat=${4:-60} keep=${5:-0} prof=0 mif=$N bud=$BUDGET xe=${XE[tw]}
    local td=$TD nm=$NM bench=$BENCH bmd5=$BENCH_MD5
    local rcf=$R/$label.cell.rc unit=bench-$label st
    local EP= CP= TP= FP= IP= RP= WSP= WP= KP= SP= mp= HP= t=0 rc fdok=0 nt=0
    case $tg in *-p*) prof=1 ;; esac
    case $tg in *-old*) td=$OLD_TD nm=${OLD_MD5:0:8} bench=$OLD_TD/release/bench-throughput bmd5=$OLD_BENCH_MD5 ;; esac
    CRASHCELL=$crash
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    st=$(sigtrace)
    echo "[$(date +%T)] START $label arm=tw dur=$dur crash_kill_at=$([ "$crash" = 1 ] && echo $killat || echo none) keep_wal=$keep prof=$prof max_in_flight=$mif budget=$bud extra_env=${xe:-none} sigtrace_pid=${st:-none} node_md5=$nm bench_md5=${bmd5:0:8} load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & CP=$!
    "$T3/io-sampler.sh" "$R/$label" 2>/dev/null & IP=$!
    python3 "$T/task-sampler.py" "$R/$label" & TP=$!
    fdsampler "$R/$label" & FP=$!
    if [ "$crash" = 1 ]; then restenv "$R/$label" & RP=$!; walsamp "$R/$label" & WSP=$!; fi
    if [ "$keep" = 1 ]; then walkeep "$R/$label" & KP=$!; fi
    if [ "$prof" = 1 ]; then "$T/prof-sidecar.sh" "$R/$label" 35 45 "$bench" "$MK" & SP=$!; fi
    local -a E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
                ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    [ "$crash" = 1 ] && E+=(CRASH_KILL_AT_S=$killat)
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MK" "$dur" 76000 "$xe" || { stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP" "$KP" "$SP"; return 1; }
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP" "$KP" "$SP"; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    while [ -n "$(pgrep -x torus-node)" ] && [ $nt -lt 60 ]; do sleep 1; nt=$((nt+1)); done
    [ -z "$(pgrep -x torus-node)" ] || echo "[$(date +%T)] WARN $label: torus-node still running at LOG copy"
    stopbg "$WSP" "$KP"; wait "$WSP" "$KP" 2>/dev/null
    [ "$keep" = 1 ] && walksweep "$R/$label"
    savelog "$label"
    if [ -e "$R/$label/node-death.txt" ]; then
        stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$SP"
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") sigtrace_pid_end=$(sigtrace || true) -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    wait "$EP" "$CP" "$TP" "$FP" "$IP"
    if [ -n "$RP" ]; then [ -s "$R/$label/crash-kill.json" ] || stopbg "$RP"; wait "$RP" 2>/dev/null; fi
    [ -n "$SP" ] && wait "$SP"
    echo "[$(date +%T)] END $label rc=$rc sigtrace_pid_end=$(sigtrace || true) $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|Cancel-by-id|WARNING: in-flight|crash gate: kill' "$R/$label/run.log" "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /" | head -20
    echo "[$label] $(fdsum "$label")" || true; fdsum "$label" > /dev/null || fdok=1
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" "$crash" || return 1
        envok "$label" tw || return 1
        bookcf "$label" tw || return 1
        knobok "$label" tw "$R/$label/node-env-all.txt" 3 || return 1
        codeccheck "$label" tw || return 1
        lsmcheck "$label" tw "$crash" || return 1
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
    if [ "$crash" = 1 ]; then
        restpost "$label"
        python3 -I "$TOOLS/crash-freeze.py" "$R/$label" --json > "$R/$label/crash-freeze.json" 2> "$R/$label/crash-freeze.stderr" \
            && echo "[$label] crash-freeze.json written ($(wc -c < "$R/$label/crash-freeze.json") bytes)" || echo "[$label] crash-freeze.py failed (see crash-freeze.stderr)"
        [ -s "$R/$label/node-restart-env.txt" ] || { echo "CHECK FAIL $label: no restarted-node environ (node-restart-env.txt)"; return 1; }
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-restart-env.txt")" = 1 ] || { echo "CHECK FAIL $label: restarted val1 exe md5 != $nm"; return 1; }
        knobok "$label" tw "$R/$label/node-restart-env.txt" 1 || return 1
    fi
    [ "$keep" = 1 ] && walkcheck "$label"
    python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); h=s["headline"]; print("[rates] matched/s", h["matched_s_avg"], "placed/s", h["placed_s_avg"], "submit/s", s["ingest"].get("bench_submit_rate"), "native blk/s", h["native_blk_s"])' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    python3 -c 'import json,sys; o=json.load(open(sys.argv[1])).get("oracle_feed") or {}; print("[oracle] stale", o.get("stale_marks_at_bench_end"), "fresh", (o.get("marks_at_bench_end") or {}).get("fresh"), "acc", o.get("accepted"), "/", o.get("sent"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    [ "$prof" = 1 ] && profchk "$label"
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    if [ "$fdok" = 1 ]; then echo "[$(date +%T)] STOP: a node held > $FD_STOP open fds in $label (set LimitNOFILE for both arms first)"; return 1; fi
    return 0
}

# ---- preflight ----
[ "$(md5sum < "$TD/release/torus-node" | cut -d' ' -f1)" = "$MD5_NEW" ] || { echo "PREFLIGHT FAIL: node md5 is not $MD5_NEW"; exit 1; }
grep -q "^n $SHA_NEW $MD5_NEW" "$MD5_BUILD" || { echo "PREFLIGHT FAIL: node $MD5_NEW not in $MD5_BUILD"; exit 1; }
grep -q "^bench $SHA_NEW $BENCH_MD5" "$MD5_BUILD" || { echo "PREFLIGHT FAIL: bench md5 $BENCH_MD5 not in $MD5_BUILD"; exit 1; }
[ "$(md5sum < "$OLD_TD/release/torus-node" | cut -d' ' -f1)" = "$OLD_MD5" ] || { echo "PREFLIGHT FAIL: control node md5 is not $OLD_MD5"; exit 1; }
[ "$(md5sum < "$OLD_TD/release/bench-throughput" | cut -d' ' -f1 | cut -c1-8)" = "$OLD_BENCH_MD5" ] || { echo "PREFLIGHT FAIL: control bench md5 is not $OLD_BENCH_MD5"; exit 1; }
[ "$(git -C "$WT_NEW" rev-parse HEAD)" = "$SHA_NEW" ] && [ -z "$(git -C "$WT_NEW" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_NEW not clean at $SHA_NEW"; exit 1; }
[ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ] || { echo "PREFLIGHT FAIL: $WT_H not clean at $SHA_HARNESS"; exit 1; }
grep -q 'exit=0' "$R/ozarchy-p3s-build/build.done" 2>/dev/null || { echo "PREFLIGHT FAIL: p3s build not done"; exit 1; }
[ "$(stat -c %d "$DEVNET")" = "$(stat -c %d "$R")" ] || { echo "PREFLIGHT FAIL: $DEVNET and $R on different filesystems (hard links)"; exit 1; }
[ -x "$T3/io-sampler.sh" ] || { echo "PREFLIGHT FAIL: io-sampler missing"; exit 1; }
for f in "$PYCHK" "$LSM" "$RSTPY" "$TOOLS/crash-freeze.py" "$TOOLS/crash-kill.sh"; do [ -f "$f" ] || { echo "PREFLIGHT FAIL: $f missing"; exit 1; }; done
[ -z "$(pgrep -x torus-node)" ] || { echo "PREFLIGHT FAIL: torus-node running"; exit 1; }
[ -z "$(pgrep 'cargo|rustc')" ] || { echo "PREFLIGHT FAIL: cargo/rustc running"; exit 1; }
for f in "$T/cpu-sampler.sh" "$HARNESS" "$DETACH" "$T/prof-sidecar.sh"; do [ -x "$f" ] || { echo "PREFLIGHT FAIL: $f not executable"; exit 1; }; done
[ -f "$T/task-sampler.py" ] && [ -f "$T/positions.py" ] || { echo "PREFLIGHT FAIL: task-sampler.py / positions.py missing"; exit 1; }
grep -q 'CANCEL_BY_ID_FRACTION' "$HARNESS" && grep -q 'proc_cpu_by_node' "$TOOLS/summarize.py" && grep -q 'index_entries_end' "$TOOLS/summarize.py" && grep -q 'torus_exec_cancel_all_index_entries' "$HARNESS" || { echo "PREFLIGHT FAIL: harness lacks the step 0.2 columns"; exit 1; }
grep -q 'torus_exec_cache_flush_seconds_sum' "$HARNESS" || { echo "PREFLIGHT FAIL: harness does not record torus_exec_cache_flush_seconds_sum"; exit 1; }
grep -q 'CRASH_KILL_AT_S' "$HARNESS" || { echo "PREFLIGHT FAIL: harness has no CRASH_KILL_AT_S"; exit 1; }
TAGS=$(for c in $CELLS; do c=${c#*:}; printf "%s " "${c%%:*}"; done)
for g in $TAGS; do [ ! -e "$R/$P-$g" ] && [ ! -e "$R/$P-$g.cell.rc" ] || { echo "PREFLIGHT FAIL: $R/$P-$g(.cell.rc) exists (old attempt: move it away first)"; exit 1; }; done
[ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge 20000 ] || { echo "PREFLIGHT FAIL: < 20 GB free under $R"; exit 1; }
SELF_UNIT=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
OTHER=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "${SELF_UNIT:-none}")
[ -z "$OTHER" ] || { echo "PREFLIGHT FAIL: other bench units active: $(echo $OTHER)"; exit 1; }
[ -n "$SELF_UNIT" ] || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=${SHA_HARNESS:0:8} node=$NM ($SHA_NEW) control=$OLD_MD5 bench=${BENCH_MD5:0:8} N=$N budget=$BUDGET nofile_soft=$(ulimit -Sn) cells: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
for c in $CELLS; do IFS=: read -r a t d x k <<< "$c"; cell "$t" "$d" "$([ "$x" = crash ] && echo 1 || echo 0)" "${k:-60}" "$([ "$x" = keep ] && echo 1 || echo 0)" || exit 1; done
echo "[$(date +%T)] CAMPAIGN END"
