#!/usr/bin/env bash
# ozarchy-p3term: SIGTERM restart cells on the NEXT Phase 3 step 2 head (with main's SIGTERM graceful shutdown, 219f0c20) and its
# control main (owner via 18c, s112). MEASUREMENT ONLY. Copy of ozarchy-p3r1-campaign.sh (section 46 method) with:
#   - commit tail (IN_FLIGHT_TAIL=commit) + tailchk; harness = wt/summarize-ckpt-timers 218ad251 (run-cell.sh, IN_FLIGHT_TAIL,
#     checkpoint timers); TOOLS_DIR = ozarchy-p3term-tools/matched-bench = that tools dir with crash-kill.sh patched for KILL_SIG,
#     0.1 s exit polling and exit_ts / exit_s / exit_forced in crash-kill*.json (a stop still running after KILL_WAIT is SIGKILLed
#     and flagged, not FATAL). KILL_WAIT=75 > the node's default shutdown bound (TORUS_SHUTDOWN_TIMEOUT_SECS unset = 60, recorded).
#   - binaries are parameters (one variable each): S2_SHA / S2_WT (step 2, key g2) and CTL_SHA / CTL_WT (control, key m), from the
#     ozarchy-p3next stage (g2-run / m-run, md5s in ozarchy-p3next-build/md5s.txt); bench 05d44313 (the sections 48-49 load).
#   - arms: s = step 2 + TORUS_CHECKPOINT_INTERVAL_MS=15000; c = control (knob unset). STATE_HASH_ACTIVATION=1 in every cell (p3r1).
#   - cells: w0 (s, 60 s, no kill), t1-s (540 s, SIGTERM val1 at 60,180,300,420), t1-c (control, same offsets), t2-s (65,187,309,431):
#     8 step 2 kills + 4 control kills; offsets spread over the checkpoint window as p3r1.
# Usage: SHA_HARNESS=218ad251... S2_SHA=.. S2_WT=.. CTL_SHA=.. CTL_WT=.. ozarchy-p3term-campaign.sh 4   (DRY_RUN=1 preflight only)
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N   (MAX_IN_FLIGHT, e.g. 4)"; exit 2; }
unset TORUS_MODEL_PROFILE CRASH_KILL_AT_S TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_TRADE_HISTORY TORUS_ROCKSDB_MAX_TOTAL_WAL_MB TORUS_P3_PROTO TORUS_P3_PROTO_INTERVAL STATE_HASH_ACTIVATION KILL_SIG KILL_WAIT
unset TORUS_CHECKPOINT_INTERVAL_MS TORUS_CHECKPOINT_MAX_BYTES TORUS_EXEC_PIPELINE TORUS_STATE_HASH_FAILSTOP TORUS_SHUTDOWN_TIMEOUT_SECS LD_PRELOAD MALLOC_CONF
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
DEVNET=/home/oz/torus-wsl-devnet
RUN=$R/ozarchy-p3term-run
S2_SHA=${S2_SHA:?step 2 sha}; S2_WT=${S2_WT:?step 2 worktree}; CTL_SHA=${CTL_SHA:?control sha}; CTL_WT=${CTL_WT:?control worktree}
MD5_BUILD=${MD5_BUILD:-$R/ozarchy-p3next-build/md5s.txt}
S2_NODE_MD5=$(awk -v s="$S2_SHA" '$1=="g2" && $2==s {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)
CTL_NODE_MD5=$(awk -v s="$CTL_SHA" '$1=="m" && $2==s {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)
S1_SHA=$S2_SHA; S1_WT=$S2_WT; S1_NODE_MD5=$S2_NODE_MD5
S1_KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000
S1_ENABLE_RE=${S1_ENABLE_RE-item 6 Phase 3: coalesced state checkpoints ON}
S1_REQUIRE_CKPT=${S1_REQUIRE_CKPT:-1}
S1_KNOB_NAME=${S1_KNOB%%=*}
# TORUS_EXEC_PIPELINE must be ON on every node of every arm (the step 1 interval is ignored at 0). Default ON since s64
# (exec_pipeline.rs: only "0" disables); the arms leave it unset. Positive control: the enable line on each validator log.
PIPE_RE=${PIPE_RE:-bl2 exec pipeline ENABLED}
SHA_HARNESS=${SHA_HARNESS:?harness sha}
WT_H=/home/oz/projects/wt/summarize-ckpt-timers
HARNESS=$WT_H/tools/matched-bench/run-cell.sh
DETACH=$WT_H/tools/matched-bench/campaign/detach.sh
TOOLS=$R/ozarchy-p3term-tools/matched-bench
T=$R/ozarchy-14236fa-tools
T3=$R/ozarchy-p3s0r-tools
PYCHK=$R/ozarchy-acc-codec-check.py
LSM=$R/ozarchy-acc3-tools/ozarchy-acc3-lsm.py
RSTPY=$R/ozarchy-acc3-tools/ozarchy-acc3-restart.py
POST=$R/ozarchy-p3s2g-tools/ozarchy-p3c1-post.py
ST=${ST:-$R/ozarchy-p3next-stage}
BENCH_SHA=8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4
BENCH_MD5_REC=05d44313c54313208207b9297e202384
MK=300
P=ozarchy-p3term-300m
METS=(9161 9162 9163)
RPCS=(8645 8646 8647)
declare -A ATD NMA
ATD[c]=$ST/m-run; ATD[s]=$ST/g2-run
BENCH=$ST/b/release/bench-throughput
BENCH_MD5=$(md5sum < "$BENCH" 2>/dev/null | cut -d' ' -f1)
NMA[c]=${CTL_NODE_MD5:0:8}; NMA[s]=${S2_NODE_MD5:0:8}
KNOBS="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_BOOK_ROWS="
declare -A ARMX ARMSH KNOBX WALCAP ABIN
# Arm env: the base knobs empty (as in p3s/p3t/p3c1), NO trade-history / WAL env (the node defaults are the tw baseline).
# ABIN = which knob semantics the checks expect: c = interval knob unset, s = S1_KNOB set (same binary in both).
ARMX[c]="$KNOBS";            ABIN[c]=c
ARMX[s]="$KNOBS $S1_KNOB";   ABIN[s]=s
ARMSH[c]=1; ARMSH[s]=1
DD=$DEVNET
# The shared checks run with arm 'tw' = node defaults: no trade-history / WAL knob may be set; WAL cap = 2 GiB default.
KNOBX[tw]=""; WALCAP[tw]=2147483648
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
# tag:arm:dur:kind:killat      kind = none | KILL | TERM | INT
KX=60,180,300,420; KY=65,187,309,431
CELLS=(
  "w0:s:60:none:"
  "t1-s-TERM:s:540:TERM:$KX" "t1-c-TERM:c:540:TERM:$KX" "t2-s-TERM:s:540:TERM:$KY"
)

stopbg() { local p; for p in "$@"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; return 0; }

# buildhits: any rust-analyzer / cargo / rustc process (exact process names; prints nothing when the host is quiet)
buildhits() { local n p; for n in rust-analyzer cargo rustc; do for p in $(pgrep -x "$n"); do printf '%s:%s ' "$n" "$p"; done; done; }

hostlog() { # label: the no-compile rule, logged before every cell
    local h; h=$(buildhits)
    echo "[$(date +%T)] HOST before $1: ${h:-none (no rust-analyzer, cargo or rustc)}"
}

quiet() {
    local t=0
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(buildhits)" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
        t=$((t+10)); [ $t -ge 900 ] && { echo "[$(date +%T)] host not quiet after 900s (load $l1, hits: $(buildhits))"; return 1; }
        sleep 10
    done
}

dumeas() { # label: du -sb of each validator data dir, plus SST and WAL bytes and SST count (before the next CLEAN start)
    local label=$1 v d o=$R/$1/data-du.txt
    : > "$o"
    for v in 0 1 2; do
        d=$DD/data/val$v
        echo "val$v total_bytes $(du -sb "$d" | cut -f1) sst_bytes $(find "$d" -maxdepth 1 -name '*.sst' -printf '%s\n' | awk '{s+=$1} END{print s+0}') sst_files $(find "$d" -maxdepth 1 -name '*.sst' | wc -l) wal_bytes $(find "$d" -maxdepth 1 -name '*.log' -printf '%s\n' | awk '{s+=$1} END{print s+0}')" >> "$o"
    done
    echo "[$(date +%T)] data sizes for $label: $(tr '\n' ';' < "$o")"
}

savelog() { # label: copy each validator's RocksDB LOG (+ LOG.old.* rotated during the cell) into the cell dir.
    # run-cell.sh runs launch-3val.sh with CLEAN=1 (data dirs wiped before every cell), so LOG is this cell's only.
    # A restart cell: LOG.old.* = val1's pre-kill open, LOG = the restarted process's open.
    local label=$1 v f b
    for v in 0 1 2; do
        if [ -s $DD/data/val$v/LOG ]; then
            cp -p $DD/data/val$v/LOG "$R/$label/rocksdb-LOG-val$v.txt"
        else
            echo "[$(date +%T)] LOG MISSING $label val$v"
        fi
        for f in $DD/data/val$v/LOG.old.*; do
            [ -e "$f" ] || continue
            b=$(basename "$f"); cp -p "$f" "$R/$label/rocksdb-${b}-val$v.txt"
            echo "[$(date +%T)] LOG rotated during $label: $b (val$v)"
        done
    done
    echo "[$(date +%T)] saved RocksDB LOG for $label: $(ls "$R/$label"/rocksdb-LOG* 2>/dev/null | wc -l) files"
    dumeas "$label"
}

envchk() { # record node environ TRIE var + exe md5 once the bench runs
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_NATIVE_TRIE' || echo TRIE_VAR_MISSING)"
    done > "$o/node-environ-trie.txt" 2>&1
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_' | sort | tr '\n' ' ')"
    done > "$o/node-env-all.txt" 2>&1
    local v f dd
    for p in $(cat $DEVNET/run/pids); do
        dd=$(tr '\0' '\n' < /proc/$p/cmdline | sed -n 's/^--data-dir=//p')
        echo "pid=$p $dd fstype=$(stat -f -c %T "$dd" 2>/dev/null)"
    done > "$o/node-datafs.txt" 2>&1
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p $(tr '\0' '\n' < /proc/$p/environ | grep -E '^TORUS_BOOK_CF_TARGET_FILE_MB=' || echo BOOKCF_VAR_UNSET)"
    done > "$o/node-bookcf.txt" 2>&1
    for v in 0 1 2; do
        f=$(ls $DD/data/val$v/OPTIONS-* 2>/dev/null | sort -V | tail -1)
        echo "val$v $(basename "${f:-none}") cf_native_order_books $(awk '/^\[CFOptions "cf_native_order_books"\]/{x=1} x && /target_file_size_base=/{print; exit}' "$f" 2>/dev/null | tr -d ' ')"
    done >> "$o/node-bookcf.txt" 2>&1
}

restenv() { # o: restart cells only. The restarted val1's pid is line 2 of the devnet pids file (crash-kill.sh rewrites it before it writes crash-kill.json)
    local o=$1 t=0 p
    until [ -s "$o/crash-kill.json" ]; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    p=$(sed -n 2p $DEVNET/run/pids)
    { echo "pid=$p exe_md5=$(md5sum < /proc/$p/exe 2>/dev/null | cut -c1-8) $(tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -E '^TORUS_' | sort | tr '\n' ' ')"; } > "$o/node-restart-env.txt"
}

walsamp() { # o: restart cells only. val1's WAL files (name, bytes) every 1 s until the cell ends: the WAL state just before the kill
    local o=$1
    while :; do
        echo "T $(date +%s.%N)"
        find $DD/data/val1 -maxdepth 1 -name '*.log' -printf 'F %f %s\n' 2>/dev/null
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

watch_nodes() { # from ozarchy-acc-campaign.sh; restart cells: val1 may be dead between its kill and its restart, so its pid is read
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

envok() { # label arm: the 7 node-local knobs (empty) and TORUS_BOOK_ROWS on all 3 nodes (Classic); cap 8
    local o=$R/$1 arm=$2 k n
    for k in TORUS_ROCKSDB_PIPELINED_WRITE TORUS_RESIDENT_BOOKS TORUS_NATIVE_ROOT_CACHE TORUS_PARALLEL_SETTLE TORUS_PARALLEL_BUCKET_HASH TORUS_BUCKET_MEMBER_CACHE_MB TORUS_BOOK_ROWS; do
        n=$(grep -c " $k= " "$o/node-env-all.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $1: $k not empty on 3 nodes ($n)"; return 1; }
    done
    n=$(grep -c " TORUS_COMMIT_LAG_BACKOFF_CAP=8 " "$o/node-env-all.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $1: cap 8 not on 3 nodes"; return 1; }
    echo "CHECK OK $1 env (arm $arm: Classic knobs empty, cap 8)"
}

knobok() { # label arm file lines: each trade/WAL knob the arm sets is in the environ of every node line in file (3, or 1 = restarted val1),
    # and no other such knob is set on those lines (arm tw: none)
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
    # with the trade history default (off) the trade CFs must have no data row on any validator. Their startup range tombstone
    # (one table file per CF, 0 rows) is expected and is reported, not counted as history. A restart cell's restarted LOG covers
    # only the time after the restart, so it is not checked for rows.
    local label=$1 arm=$2 crash=$3 v f mw rows trf=""
    for v in 0 1 2; do
        f=$R/$label/lsm-val$v.txt
        python3 -I "$LSM" "$R/$label/rocksdb-LOG-val$v.txt" > "$f" 2>&1 || { echo "CHECK FAIL $label: lsm parse val$v"; return 1; }
        mw=$(sed -n 's/^max_total_wal_size=//p' "$f")
        [ "$mw" = "${WALCAP[$arm]}" ] || { echo "CHECK FAIL $label: val$v max_total_wal_size=$mw (arm $arm wants ${WALCAP[$arm]})"; return 1; }
        rows=$(awk '/^cf=cf_native_(user_)?trades /{split($4,a,"="); s+=a[2]} END{print s+0}' "$f")
        if [ "$arm" = tw ] && [ "$rows" != 0 ]; then echo "CHECK FAIL $label: trade history is off by default but val$v wrote $rows trade rows"; return 1; fi
        trf="$trf val$v=$rows"
    done
    echo "CHECK OK $label lsm (arm $arm: max_total_wal_size=${WALCAP[$arm]} on val0-2; trade-CF data rows:$trf)"
}

restpost() { # label: restart cells: restart metrics (ozarchy-acc3-restart.py -> restart-metrics.json), printed on one line
    local o=$R/$1
    [ -s "$o/crash-kill.json" ] || { echo "[$1] RESTART FAIL: no crash-kill.json (the kill+restart did not complete; see $o/../$1.campaign.log and run.log)"; return 1; }
    python3 -I "$RSTPY" "$o" > "$o/restart-metrics.stdout" 2>&1 || { echo "[$1] RESTART FAIL: ozarchy-acc3-restart.py (see $o/restart-metrics.stdout)"; return 1; }
    echo "[$1] RESTART $(cat "$o/restart-metrics.stdout")"
}

s1knob() { # label arm file lines: the step 1 interval knob (S1_KNOB, NAME=VALUE) on every node line of file for s arms
    # (3 lines, or 1 = restarted val1); the knob NAME on no node line for control arms. No TORUS_P3_PROTO* anywhere.
    local label=$1 arm=$2 f=$3 want=$4 n
    n=$(grep -c ' TORUS_P3_PROTO' "$f")
    [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_P3_PROTO* set on $n node lines ($(basename "$f"))"; return 1; }
    n=$(grep -c ' TORUS_CHECKPOINT_MAX_BYTES=' "$f")
    [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_CHECKPOINT_MAX_BYTES set on $n node lines ($(basename "$f"))"; return 1; }
    case ${ABIN[$arm]:-} in
        c) n=$(grep -c " $S1_KNOB_NAME=" "$f")
           [ "$n" = 0 ] || { echo "CHECK FAIL $label: $S1_KNOB_NAME set on $n node lines (control arm $arm; $(basename "$f"))"; return 1; } ;;
        s) n=$(grep -c " $S1_KNOB " "$f")
           [ "$n" = "$want" ] || { echo "CHECK FAIL $label: $S1_KNOB on $n of $want node lines ($(basename "$f"))"; return 1; } ;;
        *) echo "CHECK FAIL $label: no binary for arm $arm"; return 1 ;;
    esac
    echo "CHECK OK $label s1 knob ($(basename "$f"), arm $arm: $([ "${ABIN[$arm]}" = s ] && echo "$S1_KNOB" || echo "$S1_KNOB_NAME unset"))"
}

s1log() { # label arm: optional enable line (S1_ENABLE_RE, extended regex) on each validator log: >= 1 on s arms, 0 on control arms
    local label=$1 arm=$2 o=$R/$1 v n tot=""
    [ -n "$S1_ENABLE_RE" ] || { echo "CHECK SKIP $label s1 enable line (S1_ENABLE_RE unset)"; return 0; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -c -E "$S1_ENABLE_RE")
        if [ "${ABIN[$arm]}" = s ]; then
            [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: val$v has no step 1 enable line /$S1_ENABLE_RE/"; return 1; }
        else
            [ "$n" = 0 ] || { echo "CHECK FAIL $label: val$v shows the step 1 enable line ($n) on control arm $arm"; return 1; }
        fi
        tot="$tot val$v=$n"
    done
    echo "CHECK OK $label s1 enable line (arm $arm:$tot)"
}

pipechk() { # label arm: TORUS_EXEC_PIPELINE unset or not 0 on the 3 nodes (default ON) + >= 1 enable line per validator log
    local label=$1 arm=$2 o=$R/$1 v n tot=""
    n=$(grep -c -E ' TORUS_EXEC_PIPELINE=0? ' "$o/node-env-all.txt")
    [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_EXEC_PIPELINE off/empty on $n node lines ($(grep -o 'TORUS_EXEC_PIPELINE=[^ ]*' "$o/node-env-all.txt" | sort | uniq -c | tr -s ' ' | tr '\n' ';'))"; return 1; }
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -c -E "$PIPE_RE")
        [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: val$v has no exec pipeline enable line /$PIPE_RE/ (arm $arm)"; return 1; }
        tot="$tot val$v=$n"
    done
    n=$(grep -o 'TORUS_EXEC_PIPELINE=[^ ]*' "$o/node-env-all.txt" | sort -u | tr '\n' ' ')
    echo "CHECK OK $label exec pipeline on (env: ${n:-unset = default ON}; enable lines:$tot)"
}

ckval() { # file: sum of torus_exec_checkpoints[_total] series values in a metrics dump ("absent" if no such series)
    awk '$1 ~ /^torus_exec_checkpoints(_total)?($|\{)/ {s+=$2; f=1} END{if (f) printf "%d", s; else print "absent"}' "$1" 2>/dev/null
}

s1ckpt() { # label arm: positive control from the harness's metrics-before/after-val*.txt. s arms: checkpoints delta >= 1 on every
    # node (S1_REQUIRE_CKPT=1). Control arms: series absent or 0 (control binaries do not export it).
    local label=$1 arm=$2 o=$R/$1 v b a d tot=""
    for v in 0 1 2; do
        b=$(ckval "$o/metrics-before-val$v.txt"); a=$(ckval "$o/metrics-after-val$v.txt")
        if [ "${ABIN[$arm]}" = s ]; then
            # restart cells: val1's after-dump is from the last restarted process (counter restarted at 0) -> its value alone
            if [ "$CRASHCELL" = 1 ] && [ "$v" = 1 ]; then d=$a; else { [ "$a" = absent ] && d=absent || d=$(( a - ${b/absent/0} )); }; fi
            if [ "$S1_REQUIRE_CKPT" = 1 ] && { [ "$d" = absent ] || [ "$d" -lt 1 ]; }; then
                echo "CHECK FAIL $label: val$v torus_exec_checkpoints delta=$d (step 1 arm $arm, interval on)"; return 1
            fi
        else
            d=$a
            { [ "$a" = absent ] || [ "$a" = 0 ]; } || { echo "CHECK FAIL $label: val$v control arm $arm has torus_exec_checkpoints=$a"; return 1; }
        fi
        tot="$tot val$v=$d"
    done
    echo "CHECK OK $label checkpoints (arm $arm:$tot)"
}

ckptsamp() { # o: every 1 s, each validator's /metrics: checkpoint family + reject/failure counters + head/exec/state-hash
    # gauges (time series, ckpt-samples.txt; p3c1 sampled every 2 s without the head gauges).
    # Header line per scrape: "T <epoch> val<i> ok|ERR". Before the nodes are up / after they stop / while val1 is down, ERR (tolerated).
    local o=$1 i m ts
    while :; do
        for i in 0 1 2; do
            ts=$(date +%s.%N)
            if m=$(curl -fsS -m 2 "http://127.0.0.1:${METS[$i]}/metrics" 2>/dev/null); then
                echo "T $ts val$i ok"
                grep -E '^torus_(exec_checkpoint|flush_worker_checkpoint|exec_action_(rejections|failures)|rpc_submit_admit_rejects|missing_action_rejections|orders_rejected_|order_age_future_nonce|consensus_dedup_dropped|rpc_forward_dropped|native_gossip_dropped|mempool_native_size|block_height |blocks_committed_total |exec_queue_depth |flush_worker_depth |exec_native_blocks_total |state_hash)' <<< "$m"
            else
                echo "T $ts val$i ERR"
            fi
        done
        sleep 1
    done >> "$o/ckpt-samples.txt"
}

shsamp() { # o: every 15 s until the harness stop: torus_getStateHash (no height = latest retained checkpoint) on each validator,
    # then every 100-block checkpoint above the last one recorded for that validator (retain 64 = whole cell). One line per answer:
    # "<epoch> val<i> h=<dec> <json result|error>" -> state-hashes.txt. A down validator (val1 between kill and restart) is skipped.
    local o=$1 i r h hx want
    local -a last=(0 0 0)
    until grep -q '\] bench: ' "$o/run.log" 2>/dev/null; do sleep 2; done
    while :; do
        for i in 0 1 2; do
            r=$(curl -fsS -m 5 -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"torus_getStateHash","params":[]}' "http://127.0.0.1:${RPCS[$i]}" 2>/dev/null) || continue
            hx=$(jq -r '.result.height // empty' <<< "$r" 2>/dev/null); [ -n "$hx" ] || continue
            h=$((hx))
            want=$(( last[i] + 100 )); [ "$want" -lt $(( h - 6300 )) ] && want=$(( (h - 6300) / 100 * 100 ))
            while [ "$want" -le "$h" ]; do
                r=$(curl -fsS -m 5 -H 'content-type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"torus_getStateHash\",\"params\":[$want]}" "http://127.0.0.1:${RPCS[$i]}" 2>/dev/null) || break
                echo "$(date +%s.%N) val$i h=$want $(jq -c '.result // .error' <<< "$r" 2>/dev/null)" >> "$o/state-hashes.txt"
                last[i]=$want; want=$(( want + 100 ))
            done
        done
        grep -q '^stopping pid ' "$o/run.log" 2>/dev/null && break
        for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do grep -q '^stopping pid ' "$o/run.log" 2>/dev/null && break; sleep 1; done
    done
}

hardscan() { # label: correctness evidence per validator log (WARN/ERROR level only; INFO config echoes excluded) -> HARD FINDING lines.
    # Recorded, never aborts: the analysis decides. Also prints the per-validator count of replay lines and state-hash answers.
    local label=$1 o=$R/$1 v n pat='STATE HASH MISMATCH|FATAL|panicked|could not be fully replayed|FAIL-STOP|fail-stop|conflicting blocks|replay stopped'
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -E "$pat" | grep -cvE ' (INFO|DEBUG|TRACE) ')
        [ "$n" = 0 ] || echo "HARD FINDING $label: val$v has $n WARN/ERROR correctness lines: $(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -E "$pat" | grep -vE ' (INFO|DEBUG|TRACE) ' | head -2 | cut -c1-300 | tr '\n' '|')"
    done
    n=$(grep -c -E '"matches_?[lL]ocal":false' "$o/state-hashes.txt" 2>/dev/null)
    [ "${n:-0}" = 0 ] || echo "HARD FINDING $label: $n state-hash answers with a vote that does not match local"
    echo "[$label] state hashes: $(awk '{print $2}' "$o/state-hashes.txt" 2>/dev/null | sort | uniq -c | tr -s ' ' | tr '\n' ';') distinct local hashes per height: $(python3 -I -c '
import json,sys,collections
d=collections.defaultdict(dict)
for l in open(sys.argv[1]):
    p=l.split(" ",3)
    try: j=json.loads(p[3])
    except Exception: continue
    if isinstance(j,dict) and "localHash" in j: d[p[2]][p[1]]=j["localHash"]
full=[h for h in d if len(d[h])==3]; bad=[h for h in full if len(set(d[h].values()))>1]
print("heights=%d with3=%d disagree=%d %s"%(len(d),len(full),len(bad)," ".join(sorted(bad)[:5])))' "$o/state-hashes.txt" 2>&1)"
    grep -q 'disagree=0 ' <<< "$(python3 -I -c '
import json,sys,collections
d=collections.defaultdict(dict)
for l in open(sys.argv[1]):
    p=l.split(" ",3)
    try: j=json.loads(p[3])
    except Exception: continue
    if isinstance(j,dict) and "localHash" in j: d[p[2]][p[1]]=j["localHash"]
print("disagree=%d "%sum(1 for h in d if len(set(d[h].values()))>1))' "$o/state-hashes.txt" 2>&1)" || echo "HARD FINDING $label: validators disagree on a state hash checkpoint (state-hashes.txt)"
}

statechk() { # label arm: the cell's genesis state hash (run.log has the generator's line)
    local label=$1 arm=$2 want
    want='1'
    grep -q -x "  state_hash_activation = $want" "$R/$label/run.log" || { echo "CHECK FAIL $label: genesis state_hash_activation is not $want (arm $arm)"; return 1; }
    echo "CHECK OK $label genesis state_hash_activation = $want (arm $arm)"
}


dusamp() { # o: every 2 s: epoch, du -sb of each validator data dir under $DD, MemAvailable kB, /dev/shm used kB (peak data size).
    # NOTE: no tmpfs cleanup after a tmpfs cell (deletion is gated on this host); the next tmpfs cell's launch-3val.sh CLEAN=1
    # wipes it, so up to one tmpfs cell's data stays in /dev/shm during the following disk cells (logged here as shm_used_kb).
    local o=$1 v line
    while :; do
        line="$(date +%s.%N)"
        for v in 0 1 2; do line="$line $(du -sb "$DD/data/val$v" 2>/dev/null | cut -f1)"; done
        line="$line mem_avail_kb=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo) shm_used_kb=$(df --output=used -k /dev/shm | tail -1 | tr -d ' ')"
        echo "$line" >> "$o/data-du-samples.txt"
        sleep 2
    done
}

fschk() { # label arm: each node's --data-dir (from envchk's node-datafs.txt) is on the arm's filesystem type
    local o=$R/$1 arm=$2 n
    n=$(grep -c " fstype=btrfs\$" "$o/node-datafs.txt" 2>/dev/null)
    [ "$n" = 3 ] || { echo "CHECK FAIL $1: data dir fs not btrfs on 3 nodes: $(tr '\n' ';' < "$o/node-datafs.txt" 2>/dev/null)"; return 1; }
    echo "CHECK OK $1 data dirs on btrfs ($(awk '{print $2}' "$o/node-datafs.txt" | tr '\n' ' '))"
}

tailchk() { # label: HARD check that the cell ran the bench in commit-tail mode
    local o=$R/$1
    grep -q "in_flight_tail='commit'" "$o/run.log" || { echo "CHECK FAIL $1: run.log has no in_flight_tail='commit'"; return 1; }
    grep -q -- '--in-flight-tail commit' "$o/run.log" || { echo "CHECK FAIL $1: bench cmd lacks --in-flight-tail commit"; return 1; }
    grep -q 'In-flight cap .*| tail=commit' "$o/bench.log" || { echo "CHECK FAIL $1: bench.log In-flight line lacks tail=commit"; return 1; }
    python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])); a=s["cell"].get("in_flight_tail"); b=(s["ingest"].get("in_flight") or {}).get("tail_mode"); sys.exit(0 if a==b=="commit" else 1)' "$o/summary.json" \
        || { echo "CHECK FAIL $1: summary.json tail mode is not commit"; return 1; }
    echo "CHECK OK $1 in-flight tail = commit (run.log, bench cmd, bench.log, summary.json cell + ingest)"
}

cell() { # tag arm dur kind killat. return 0 = go on, 1 = abort
    local tg=$1 arm=$2 dur=$3 kind=$4 killat=${5:-} label rcf unit sig=none
    local td=${ATD[${ABIN[$arm]:-x}]:-} nm=${NMA[${ABIN[$arm]:-x}]:-} bench=$BENCH bmd5=$BENCH_MD5 mif=$N bud=$BUDGET xe=${ARMX[$arm]:-}
    local EP= CP= TP= FP= IP= RP= WSP= WP= DP= KP= SP= mp= HP= t=0 rc fdok=0 nt=0 f
    local -a E
    label=$P-$tg; rcf=$R/$label.cell.rc; unit=bench-$label
    CRASHCELL=0; [ "$kind" = none ] || CRASHCELL=1
    case $kind in none) ;; KILL|TERM|INT) sig=$kind ;; *) echo "ABORT: unknown kind $kind"; return 1 ;; esac
    [ -n "${ARMX[$arm]:-}" ] || { echo "ABORT: unknown arm $arm"; return 1; }
    { [ -n "$td" ] && [ -n "$nm" ]; } || { echo "ABORT: no binary dir / md5 for arm $arm"; return 1; }
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    hostlog "$label"
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm dur=$dur kind=$kind crash_kill_at=${killat:-none} sig=$sig state_hash=${ARMSH[$arm]:-0} max_in_flight=$N budget=$BUDGET extra_env=${xe:-none} node_md5=$nm bench_md5=${bmd5:0:8} load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & CP=$!
    "$T3/io-sampler.sh" "$R/$label" 2>/dev/null & IP=$!
    dusamp "$R/$label" & DP=$!
    ckptsamp "$R/$label" & KP=$!
    shsamp "$R/$label" & SP=$!
    python3 "$T/task-sampler.py" "$R/$label" & TP=$!
    fdsampler "$R/$label" & FP=$!
    if [ "$CRASHCELL" = 1 ]; then restenv "$R/$label" & RP=$!; walsamp "$R/$label" & WSP=$!; fi
    E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
       ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 DATA_ROOT="$DD" IN_FLIGHT_TAIL=commit)
    if [ "${ARMSH[$arm]:-0}" = 1 ]; then E+=(STATE_HASH_ACTIVATION=1); fi
    [ "$kind" = none ] || E+=(CRASH_KILL_AT_S="$killat" KILL_SIG="$sig" KILL_WAIT=75)
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MK" "$dur" 76000 "$xe" || { stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP" "$DP" "$KP" "$SP"; return 1; }
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP" "$DP" "$KP" "$SP"; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    while [ -n "$(pgrep -x torus-node)" ] && [ $nt -lt 60 ]; do sleep 1; nt=$((nt+1)); done
    [ -z "$(pgrep -x torus-node)" ] || echo "[$(date +%T)] WARN $label: torus-node still running at LOG copy"
    stopbg "$WSP"; wait "$WSP" 2>/dev/null
    stopbg "$DP" "$KP"; wait "$DP" "$KP" 2>/dev/null
    t=0; while kill -0 "$SP" 2>/dev/null && [ $t -lt 30 ]; do sleep 1; t=$((t+1)); done; stopbg "$SP"; wait "$SP" 2>/dev/null
    savelog "$label"
    if [ -e "$R/$label/node-death.txt" ]; then
        stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$SP"
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    wait "$EP" "$CP" "$TP" "$FP" "$IP"
    if [ -n "$RP" ]; then [ -s "$R/$label/crash-kill.json" ] || stopbg "$RP"; wait "$RP" 2>/dev/null; fi
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|Cancel-by-id|WARNING: in-flight|crash gate: kill' "$R/$label/run.log" "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /" | head -20
    echo "[$label] $(fdsum "$label")" || true; fdsum "$label" > /dev/null || fdok=1
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" "$CRASHCELL" || return 1
        envok "$label" tw || return 1
        bookcf "$label" tw || return 1
        knobok "$label" tw "$R/$label/node-env-all.txt" 3 || return 1
        codeccheck "$label" tw || return 1
        lsmcheck "$label" tw "$CRASHCELL" || return 1
        s1knob "$label" "$arm" "$R/$label/node-env-all.txt" 3 || return 1
        s1log "$label" "$arm" || return 1
        s1ckpt "$label" "$arm" || return 1
        pipechk "$label" "$arm" || return 1
        statechk "$label" "$arm" || return 1
        fschk "$label" "$arm" || return 1
        tailchk "$label" || return 1
        n=$(grep -c ' TORUS_SHUTDOWN_TIMEOUT_SECS=' "$R/$label/node-env-all.txt"); [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_SHUTDOWN_TIMEOUT_SECS set on $n nodes (want the default 60)"; return 1; }
        echo "CHECK OK $label TORUS_SHUTDOWN_TIMEOUT_SECS unset on 3 nodes (node default 60 s)"
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
    if [ "$CRASHCELL" = 1 ]; then
        restpost "$label"
        python3 -I "$TOOLS/crash-freeze.py" "$R/$label" --json > "$R/$label/crash-freeze.json" 2> "$R/$label/crash-freeze.stderr" \
            && echo "[$label] crash-freeze.json written ($(wc -c < "$R/$label/crash-freeze.json") bytes)" || echo "[$label] crash-freeze.py failed (see crash-freeze.stderr)"
        [ -s "$R/$label/node-restart-env.txt" ] || { echo "CHECK FAIL $label: no restarted-node environ (node-restart-env.txt)"; return 1; }
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-restart-env.txt")" = 1 ] || { echo "CHECK FAIL $label: restarted val1 exe md5 != $nm"; return 1; }
        knobok "$label" tw "$R/$label/node-restart-env.txt" 1 || return 1
        s1knob "$label" "$arm" "$R/$label/node-restart-env.txt" 1 || return 1
    fi
    python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); h=s["headline"]; print("[rates] matched/s", h["matched_s_avg"], "placed/s", h["placed_s_avg"], "submit/s", s["ingest"].get("bench_submit_rate"), "native blk/s", h["native_blk_s"])' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    python3 -c 'import json,sys; o=json.load(open(sys.argv[1])).get("oracle_feed") or {}; print("[oracle] stale", o.get("stale_marks_at_bench_end"), "fresh", (o.get("marks_at_bench_end") or {}).get("fresh"), "acc", o.get("accepted"), "/", o.get("sent"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    python3 -I "$POST" "$R/$label" "$arm" 2>&1 | sed "s/^/[$label] /"
    hardscan "$label"
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    if [ "$fdok" = 1 ]; then echo "[$(date +%T)] STOP: a node held > $FD_STOP open fds in $label (set LimitNOFILE for both arms first)"; return 1; fi
    return 0
}

# SMOKE=1: one 150 s s-arm INT cell (kill at 60) under its own label prefix (prep check of the clean-stop path and the new
# samplers; not part of the measured campaign)
if [ "${SMOKE:-0}" = 1 ]; then P=ozarchy-p3term-smoke-300m; CELLS=("s0-s-TERM:s:150:TERM:60"); fi
# ---- preflight ----
PF() { echo "PREFLIGHT FAIL: $*"; exit 1; }
for v in S2_SHA CTL_SHA SHA_HARNESS; do [[ "${!v}" =~ ^[0-9a-f]{40}$ ]] || PF "$v must be 40 hex"; done
[[ "$S2_NODE_MD5" =~ ^[0-9a-f]{32}$ ]] || PF "no 'g2 $S2_SHA <md5>' line in $MD5_BUILD"
[[ "$CTL_NODE_MD5" =~ ^[0-9a-f]{32}$ ]] || PF "no 'm $CTL_SHA <md5>' line in $MD5_BUILD"
[ "$S2_NODE_MD5" != "$CTL_NODE_MD5" ] || PF "step 2 and control node md5 are equal"
for v in S2 CTL; do w=${v}_WT; h=${v}_SHA; { [ "$(git -C "${!w}" rev-parse HEAD)" = "${!h}" ] && [ -z "$(git -C "${!w}" status --porcelain)" ]; } || PF "${!w} not clean at ${!h}"; done
[ "$(git -C "$REPO" merge-base "$S2_SHA" "$CTL_SHA")" = "$CTL_SHA" ] || PF "control $CTL_SHA is not merge-base(step 2, control)"
echo "[$(date +%T)] step 2 $S2_SHA: merge-base with origin/main ($(git -C "$REPO" rev-parse origin/main | cut -c1-8)) = $(git -C "$REPO" merge-base "$S2_SHA" origin/main | cut -c1-8); control $CTL_SHA"
git -C "$REPO" merge-base --is-ancestor 72591b89 "$S2_SHA" || PF "step 2 lacks the SIGTERM shutdown (72591b89)"
git -C "$REPO" merge-base --is-ancestor 72591b89 "$CTL_SHA" || PF "control lacks the SIGTERM shutdown (72591b89)"
[ "$(md5sum < "$ST/g2-run/release/torus-node" | cut -c1-32)" = "$S2_NODE_MD5" ] || PF "g2-run node md5"
[ "$(md5sum < "$ST/m-run/release/torus-node" | cut -c1-32)" = "$CTL_NODE_MD5" ] || PF "m-run node md5"
for d in b g2-run m-run; do [ "$(md5sum < "$ST/$d/release/bench-throughput" | cut -c1-32)" = "$BENCH_MD5_REC" ] || PF "$d bench md5"; done
grep -q "^b $BENCH_SHA $BENCH_MD5_REC" "$MD5_BUILD" || PF "no bench record in $MD5_BUILD"
[ -f "$POST" ] || PF "$POST missing"
command -v jq > /dev/null || PF "jq missing"
{ [ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ]; } || PF "$WT_H not clean at $SHA_HARNESS"
[ "$(diff -rq -x __pycache__ "$WT_H/tools/matched-bench" "$TOOLS" | wc -l)" = 1 ] || PF "harness tools copy differs from $WT_H beyond crash-kill.sh"
grep -q 'exit_forced' "$TOOLS/crash-kill.sh" || PF "crash-kill.sh copy lacks the exit timing patch"
grep -q 'IN_FLIGHT_TAIL' "$HARNESS" || PF "$HARNESS does not know IN_FLIGHT_TAIL"
grep -q 'KILL_SIG' "$TOOLS/crash-kill.sh" || PF "crash-kill.sh copy has no KILL_SIG"
[ "$(stat -f -c %T "$DEVNET")" = btrfs ] || PF "$DEVNET is not on btrfs (shared NVMe)"
[ "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" -ge 30000000 ] || PF "MemAvailable < 30 GB"
for f in "$PYCHK" "$LSM" "$RSTPY" "$TOOLS/crash-freeze.py" "$TOOLS/crash-kill.sh" "$HARNESS" "$DETACH" "$T/task-sampler.py" "$T/positions.py"; do [ -f "$f" ] || PF "$f missing"; done
for f in "$HARNESS" "$DETACH" "$T/cpu-sampler.sh" "$T3/io-sampler.sh"; do [ -x "$f" ] || PF "$f not executable"; done
[ -z "$(pgrep -x torus-node)" ] || PF "torus-node running"
[ -z "$(buildhits)" ] || PF "rust-analyzer/cargo/rustc running: $(buildhits)"
hostlog preflight
TAGS=$(for c in "${CELLS[@]}"; do c=${c%%:*}; printf '%s ' "$c"; done)
for g in $TAGS; do [ ! -e "$R/$P-$g" ] && [ ! -e "$R/$P-$g.cell.rc" ] && [ ! -e "$R/$P-$g.campaign.log" ] || PF "$R/$P-$g exists (old attempt: move it away first)"; done
[ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge 20000 ] || PF "< 20 GB free under $R"
SELF_UNIT=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
OTHER=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "${SELF_UNIT:-none}")
[ -z "$OTHER" ] || PF "other bench units active: $(echo $OTHER)"
[ -n "$SELF_UNIT" ] || echo "WARNING: campaign driver is not inside a bench-*.service unit"
echo "[$(date +%T)] PREFLIGHT OK harness=${SHA_HARNESS:0:8} step2=${NMA[s]} ($S2_SHA) control=${NMA[c]} ($CTL_SHA) knob=$S1_KNOB enable_re=${S1_ENABLE_RE:-none} require_ckpt=$S1_REQUIRE_CKPT bench=${BENCH_MD5:0:8} N=$N budget=$BUDGET nofile_soft=$(ulimit -Sn) cells=${#CELLS[@]}: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
for c in "${CELLS[@]}"; do
    IFS=: read -r tg arm dur kind killat <<< "$c"
    cell "$tg" "$arm" "$dur" "$kind" "$killat" || { echo "[$(date +%T)] CAMPAIGN STOP at $tg"; exit 1; }
done
echo "[$(date +%T)] CAMPAIGN END"
