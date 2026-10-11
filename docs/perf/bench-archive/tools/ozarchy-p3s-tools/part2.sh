export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}

stopbg() { local p; for p in "$@"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; return 0; }

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1)"; return 1; }
        sleep 10
    done
}

dumeas() { # label: du -sb of each validator data dir, plus SST and WAL bytes and SST count (before the next CLEAN start)
    local label=$1 v d o=$R/$1/data-du.txt
    : > "$o"
    for v in 0 1 2; do
        d=$DEVNET/data/val$v
        echo "val$v total_bytes $(du -sb "$d" | cut -f1) sst_bytes $(find "$d" -maxdepth 1 -name '*.sst' -printf '%s\n' | awk '{s+=$1} END{print s+0}') sst_files $(find "$d" -maxdepth 1 -name '*.sst' | wc -l) wal_bytes $(find "$d" -maxdepth 1 -name '*.log' -printf '%s\n' | awk '{s+=$1} END{print s+0}')" >> "$o"
    done
    echo "[$(date +%T)] data sizes for $label: $(tr '\n' ';' < "$o")"
}

savelog() { # label: copy each validator's RocksDB LOG (+ LOG.old.* rotated during the cell) into the cell dir.
    # run-cell.sh runs launch-3val.sh with CLEAN=1 (data dirs wiped before every cell), so LOG is this cell's only.
    # A restart cell: LOG.old.* = val1's pre-kill open, LOG = the restarted process's open.
    local label=$1 v f b
    for v in 0 1 2; do
        if [ -s $DEVNET/data/val$v/LOG ]; then
            cp -p $DEVNET/data/val$v/LOG "$R/$label/rocksdb-LOG-val$v.txt"
        else
            echo "[$(date +%T)] LOG MISSING $label val$v"
        fi
        for f in $DEVNET/data/val$v/LOG.old.*; do
            [ -e "$f" ] || continue
            b=$(basename "$f"); cp -p "$f" "$R/$label/rocksdb-${b}-val$v.txt"
            echo "[$(date +%T)] LOG rotated during $label: $b (val$v)"
        done
    done
    echo "[$(date +%T)] saved RocksDB LOG for $label: $(ls "$R/$label"/rocksdb-LOG* 2>/dev/null | wc -l) files"
    dumeas "$label"
}

sigtrace() { pgrep -f -- "$SIGTRACE_RE" | head -1; }

envchk() { # record node environ TRIE var + exe md5 once the bench runs
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_' | sort | tr '\n' ' ')"
    done > "$o/node-env-all.txt" 2>&1
    local v f
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_BOOK_CF_TARGET_FILE_MB=' || echo BOOKCF_VAR_UNSET)"
    done > "$o/node-bookcf.txt" 2>&1
    for v in 0 1 2; do
        f=$(ls $DEVNET/data/val$v/OPTIONS-* 2>/dev/null | sort -V | tail -1)
        echo "val$v $(basename "${f:-none}") cf_native_order_books $(awk '/^\[CFOptions "cf_native_order_books"\]/{x=1} x && /target_file_size_base=/{print; exit}' "$f" 2>/dev/null | tr -d ' ')"
    done >> "$o/node-bookcf.txt" 2>&1
}

restenv() { # o: restart cells only. The restarted val1's pid is line 2 of the devnet pids file (crash-kill.sh rewrites it before it writes crash-kill.json)
    local o=$1 t=0 p
    until [ -s "$o/crash-kill.json" ]; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    p=$(sed -n 2p $DEVNET/run/pids)
    { echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe 2>/dev/null | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -E '^TORUS_' | sort | tr '\n' ' ')"; } > "$o/node-restart-env.txt"
}

walsamp() { # o: restart cells only. val1's WAL files (name, bytes) every 1 s until the cell ends: the WAL state just before the SIGKILL
    local o=$1
    while :; do
        echo "T $(date +%s.%N)"
        find $DEVNET/data/val1 -maxdepth 1 -name '*.log' -printf 'F %f %s\n' 2>/dev/null
        sleep 1
    done >> "$o/val1-wal-samples.txt"
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

watch_nodes() { # from ozarchy-acc-campaign.sh; restart cells: val1 may be dead between its SIGKILL and its restart, so its pid is read
    # from the devnet pids file (line 2) and a dead val1 is expected only while the kill is in progress (no crash-kill.json yet)
    local o=$1 hp=$2 pids= p i t=0 f=$1/node-life.txt bpid= opid= bdone=0 odone=0 now k L cur
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
            if [ "$CRASHCELL" = 1 ] && [ "$i" = 1 ]; then
                cur=$(sed -n 2p $DEVNET/run/pids 2>/dev/null); [ -n "$cur" ] && p=$cur
                if ! kill -0 "$p" 2>/dev/null && [ "$(grep -c 'crash gate: kill [0-9]*/[0-9]* at bench' "$o/run.log" 2>/dev/null)" -gt "$(ls "$o"/crash-kill*.json 2>/dev/null | wc -l)" ]; then
                    i=$((i+1)); continue
                fi
            fi
            if ! kill -0 "$p" 2>/dev/null; then
                now=$(date '+%F %T.%3N')
                grep -q '^stopping pid ' "$o/run.log" && { stopwin "$o" "$hp" "$f"; return 0; }
                death "$o" "$hp" "$f" "node val$i" torus-node "$p" "last val$i log line: $(lastline $DEVNET/run/val$i.log)" "$now"
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

bookcf() { # label arm: every arm must show the 4 MiB book CF
    local o=$R/$1 arm=$2
    { [ "$(grep -c 'BOOKCF_VAR_UNSET' "$o/node-bookcf.txt")" = 3 ] && [ "$(grep -c 'target_file_size_base=4194304' "$o/node-bookcf.txt")" = 3 ]; } \
        || { echo "CHECK FAIL $1: book CF setting for arm $arm: $(tr '\n' ';' < "$o/node-bookcf.txt")"; return 1; }
    echo "CHECK OK $1 book CF: $(grep -o 'target_file_size_base=[0-9]*' "$o/node-bookcf.txt" | sort | uniq -c | tr -s ' ' | tr '\n' ';')"
}

check() { # label crash(0|1): trie off by default on every node (no TRIE var, one off-default line per validator; a restarted val1 logs it again)
    local o=$R/$1 crash=$2 n v
    grep -q 'TORUS_NATIVE_TRIE' "$o/node-environ-trie.txt" && { echo "CHECK FAIL $1: node has TRIE var"; return 1; }
    [ "$(grep -c 'TRIE_VAR_MISSING' "$o/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $1: environ not recorded for 3 nodes"; return 1; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | grep -c 'native trie maintenance off (default')
        if [ "$crash" = 1 ] && [ "$v" = 1 ]; then
            [ "$n" -ge 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
        else
            [ "$n" = 1 ] || { echo "CHECK FAIL $1: val$v off-default lines=$n"; return 1; }
        fi
    done
    echo "CHECK OK $1 trie (off-default line on val0-2$([ "$crash" = 1 ] && echo '; restarted val1 logs it again'))"; return 0
}

envok() { # label arm: the 6 node-local knobs and TORUS_BOOK_ROWS empty on all 3 nodes (Classic); cap 8
    local o=$R/$1 arm=$2 k n
    for k in TORUS_ROCKSDB_PIPELINED_WRITE TORUS_RESIDENT_BOOKS TORUS_NATIVE_ROOT_CACHE TORUS_PARALLEL_SETTLE TORUS_PARALLEL_BUCKET_HASH TORUS_BUCKET_MEMBER_CACHE_MB TORUS_BOOK_ROWS; do
        n=$(grep -c " $k= " "$o/node-env-all.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $1: $k not empty on 3 nodes ($n)"; return 1; }
    done
    n=$(grep -c " TORUS_COMMIT_LAG_BACKOFF_CAP=8 " "$o/node-env-all.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $1: cap 8 not on 3 nodes"; return 1; }
    echo "CHECK OK $1 env (arm $arm: Classic knobs empty, cap 8)"
}

knobok() { # label arm file lines: each new knob the arm sets is in the environ of every node line in file (3, or 1 = restarted val1),
    # and no other new knob is set on those lines
    local o=$R/$1 arm=$2 f=$3 want=$4 k kv n
    for k in TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_MAX_TOTAL_WAL_MB TORUS_TRADE_HISTORY; do
        kv=$(tr ' ' '\n' <<< "${KNOBX[$arm]}" | grep -E "^$k=" || true)
        if [ -z "$kv" ]; then
            n=$(grep -c -E " $k=" "$f")
            [ "$n" = 0 ] || { echo "CHECK FAIL $1: $k set on $n node lines (arm $arm must have none; $(basename "$f"))"; return 1; }
        else
            n=$(grep -c " $kv " "$f")
            [ "$n" = "$want" ] || { echo "CHECK FAIL $1: $kv on $n of $want node lines ($(basename "$f"))"; return 1; }
        fi
    done
    echo "CHECK OK $1 knobs ($(basename "$f"), arm $arm: ${KNOBX[$arm]:-none set})"
}

codeccheck() { # label arm: per-CF SST codec from each validator's LOG (table_file_creation) and the WAL option in the DB options dump
    local label=$1 arm=$2 v f wal z1 bad=0
    for v in 0 1 2; do
        f=$R/$label/codec-check-val$v.txt
        python3 -I "$PYCHK" "$R/$label/rocksdb-LOG-val$v.txt" > "$f" 2>&1 || { echo "CHECK FAIL $label: codec parse val$v"; return 1; }
        wal=$(sed -n 's/^wal_compression_opt=//p' "$f")
        z1=$(grep -c ' ZSTD level=1 ' "$f")
        case $arm in
            b|tw) [ "$wal" = 0 ] && [ "$z1" = 0 ] || bad=1 ;;
            twd)   [ -n "$wal" ] && [ "$wal" != 0 ] && [ "$wal" != none ] && [ "$z1" = 0 ] || bad=1 ;;
            *)     bad=1 ;;
        esac
        [ $bad = 0 ] || { echo "CHECK FAIL $label: codec val$v arm $arm: wal_opt=$wal zstd1=$z1 ($(tr '\n' ';' < "$f"))"; return 1; }
    done
    echo "CHECK OK $label codec (arm $arm: $(for v in 0 1 2; do printf 'val%s wal_opt=%s ' $v "$(sed -n 's/^wal_compression_opt=//p' "$R/$label/codec-check-val$v.txt")"; done))"
}

lsmcheck() { # label arm crash(0|1): per validator LOG facts (ozarchy-acc3-lsm.py). max_total_wal_size must be the arm's cap on every node;
    # with TORUS_TRADE_HISTORY=0 the trade CFs must have no data row on any validator. Their startup range tombstone (one table
    # file per CF, 0 rows) is expected and is reported, not counted as history. A history-on throughput cell must show rows
    # (positive control); a restart cell's restarted LOG covers only the time after the restart, so it is not checked for rows.
    local label=$1 arm=$2 crash=$3 v f mw rows trf=""
    for v in 0 1 2; do
        f=$R/$label/lsm-val$v.txt
        python3 -I "$LSM" "$R/$label/rocksdb-LOG-val$v.txt" > "$f" 2>&1 || { echo "CHECK FAIL $label: lsm parse val$v"; return 1; }
        mw=$(sed -n 's/^max_total_wal_size=//p' "$f")
        [ "$mw" = "${WALCAP[$arm]}" ] || { echo "CHECK FAIL $label: val$v max_total_wal_size=$mw (arm $arm wants ${WALCAP[$arm]})"; return 1; }
        rows=$(awk '/^cf=cf_native_(user_)?trades /{split($4,a,"="); s+=a[2]} END{print s+0}' "$f")
        if { [ "$arm" = tw ] || [ "$arm" = twd ]; } && [ "$rows" != 0 ]; then echo "CHECK FAIL $label: TORUS_TRADE_HISTORY=0 but val$v wrote $rows trade rows"; return 1; fi
        if [ "$arm" = b ] && [ "$crash" != 1 ] && [ "$rows" = 0 ]; then echo "CHECK FAIL $label: trade history on but val$v wrote 0 trade rows"; return 1; fi
        trf="$trf val$v=$rows"
    done
    echo "CHECK OK $label lsm (arm $arm: max_total_wal_size=${WALCAP[$arm]} on val0-2; trade-CF data rows:$trf)"
}

restpost() { # label: restart cells: restart metrics (ozarchy-acc3-restart.py -> restart-metrics.json), printed on one line
    local o=$R/$1
    [ -s "$o/crash-kill.json" ] || { echo "[$1] RESTART FAIL: no crash-kill.json (the SIGKILL+restart did not complete; see $o/../$1.campaign.log and run.log)"; return 1; }
    python3 -I "$RSTPY" "$o" > "$o/restart-metrics.stdout" 2>&1 || { echo "[$1] RESTART FAIL: ozarchy-acc3-restart.py (see $o/restart-metrics.stdout)"; return 1; }
    echo "[$1] RESTART $(cat "$o/restart-metrics.stdout")"
}

