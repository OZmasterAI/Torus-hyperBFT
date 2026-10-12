#!/usr/bin/env bash
# ozarchy-p3s2g: Phase 3 STEP 2 gate (step 4 of the plan): step 2 (feat/item6-phase3-step2 2296be67, ingress readers on a
# published view, interval knob TORUS_CHECKPOINT_INTERVAL_MS=15000) vs control main c97f56f9, shared NVMe only, BOTH arms with
# the bench in-flight tail in COMMIT mode (IN_FLIGHT_TAIL=commit, unmerged bench/inflight-commit-tail 86d2b2bb).
# Copy of ozarchy-p3c1-campaign.sh. Changes (18c, s112):
#   - harness = the commit-tail worktree /home/oz/projects/wt/inflight-commit-tail at 86d2b2bb (run-cell.sh passes IN_FLIGHT_TAIL;
#     tools/matched-bench differs from 9b7e29b2 only by the commit-tail commits); TOOLS_DIR = its tools/matched-bench (no crash cells).
#   - ONE bench-throughput built from 86d2b2bb (stage b), copied into c-run and s-run beside each arm's node.
#   - control is NOT the step 2 merge-base: step 2 is off 2725e321, control c97f56f9 adds non-Phase-3 main commits (confound,
#     reported; preflight only logs the merge-base). Variable names S1_* are kept from p3c1 and mean the STEP 2 arm here.
#   - arms cd (control, disk) and sd (step 2, knob 15 s, disk); tmpfs arms dropped. Cells: w0 warm-up (cd, 60 s) +
#     r1..r4 alternating (ABBA: cd sd | sd cd | cd sd | sd cd), 120 s, n=4/arm.
#   - HARD per-cell check tailchk: run.log in_flight_tail='commit', bench cmd '--in-flight-tail commit', bench.log 'tail=commit',
#     summary.json cell.in_flight_tail == ingest.in_flight.tail_mode == commit. Any miss stops the campaign.
#   - ckptsamp also scrapes torus_exec_cancel_all* (cancel-all executed vs sent, question 3).
# Usage: S1_SHA=<step2 40hex> S1_NODE_MD5=<s md5> S1_KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000 ozarchy-p3s2g-campaign.sh 4
#   DRY_RUN=1 preflight only; SMOKE=1 one 60 s sd cell under ozarchy-p3s2g-smoke-300m-s0.
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N   (MAX_IN_FLIGHT, e.g. 4)"; exit 2; }
unset TORUS_MODEL_PROFILE CRASH_KILL_AT_S TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_TRADE_HISTORY TORUS_ROCKSDB_MAX_TOTAL_WAL_MB TORUS_P3_PROTO TORUS_P3_PROTO_INTERVAL STATE_HASH_ACTIVATION KILL_SIG KILL_WAIT
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
DEVNET=/home/oz/torus-wsl-devnet
RUN=$R/ozarchy-p3s2g-run
SHM=/dev/shm/ozarchy-p3s2g-devnet
CTL_SHA=${CTL_SHA:-c97f56f927994d88a2a00f63d147ce3cfaf0ae6b}
CTL_WT=${CTL_WT:-/home/oz/projects/wt/p3s2g-${CTL_SHA:0:8}}
S1_SHA=${S1_SHA:-}
S1_WT=${S1_WT:-/home/oz/projects/wt/p3s2g-${S1_SHA:0:8}}
S1_NODE_MD5=${S1_NODE_MD5:-}
S1_KNOB=${S1_KNOB:-}
S1_ENABLE_RE=${S1_ENABLE_RE-item 6 Phase 3: coalesced state checkpoints ON}
S1_REQUIRE_CKPT=${S1_REQUIRE_CKPT:-1}
S1_KNOB_NAME=${S1_KNOB%%=*}
# TORUS_EXEC_PIPELINE must be ON on every node of every arm (the step 1 interval is ignored at 0). Default ON since s64
# (exec_pipeline.rs: only "0" disables); the arms leave it unset. Positive control: the enable line on each validator log.
PIPE_RE=${PIPE_RE:-bl2 exec pipeline ENABLED}
SHA_HARNESS=86d2b2bbba9bdb553ffcca20cb6dc3b9d18b6c69
WT_H=/home/oz/projects/wt/inflight-commit-tail
HARNESS=$WT_H/tools/matched-bench/run-cell.sh
DETACH=$WT_H/tools/matched-bench/campaign/detach.sh
TOOLS=$WT_H/tools/matched-bench
T=$R/ozarchy-14236fa-tools
T3=$R/ozarchy-p3s0r-tools
PYCHK=$R/ozarchy-acc-codec-check.py
LSM=$R/ozarchy-acc3-tools/ozarchy-acc3-lsm.py
RSTPY=$R/ozarchy-acc3-tools/ozarchy-acc3-restart.py
POST=$R/ozarchy-p3s2g-tools/ozarchy-p3c1-post.py
ST=$R/ozarchy-p3s2g-stage
MD5_BUILD=$R/ozarchy-p3s2g-build/md5s.txt
MK=300
P=ozarchy-p3s2g-300m
METS=(9161 9162 9163)
declare -A ATD NMA
ATD[c]=$ST/c-run; ATD[s]=$ST/s-run
BENCH=$ST/b/release/bench-throughput
BENCH_MD5=$(md5sum < "$BENCH" 2>/dev/null | cut -d' ' -f1)
CTL_NODE_MD5=$(awk -v s="$CTL_SHA" '$1=="c" && $2==s {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)
NMA[c]=${CTL_NODE_MD5:0:8}; NMA[s]=${S1_NODE_MD5:0:8}
CTL_BENCH_MD5=$(awk -v s="$SHA_HARNESS" '$1=="b" && $2==s {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)  # the one commit-tail bench
KNOBS="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_BOOK_ROWS="
declare -A ARMX ARMSH MODE KNOBX WALCAP AROOT AFS ABIN
# Arm env: the base knobs empty (as in p3s/p3t), NO trade-history / WAL env (the node defaults are the tw baseline).
ARMX[cd]="$KNOBS";              ARMX[cm]="$KNOBS"
ARMX[sd]="$KNOBS $S1_KNOB";     ARMX[sm]="$KNOBS $S1_KNOB"
ABIN[cd]=c; ABIN[cm]=c; ABIN[sd]=s; ABIN[sm]=s
AROOT[cd]=$DEVNET; AROOT[sd]=$DEVNET; AROOT[cm]=$SHM; AROOT[sm]=$SHM
AFS[cd]=btrfs; AFS[sd]=btrfs; AFS[cm]=tmpfs; AFS[sm]=tmpfs
DD=$DEVNET
# The shared checks run with arm 'tw' = node defaults: no trade-history / WAL knob may be set; WAL cap = 2 GiB default.
KNOBX[tw]=""; WALCAP[tw]=2147483648
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
# tag:arm:dur:kind:killat      kind = none | graceful | crash   (Williams square as p3t: ad->sd, am->sm)
CELLS=(
  "w0:cd:60:none:"
  "r1-cd:cd:120:none:"  "r1-sd:sd:120:none:"
  "r2-sd:sd:120:none:"  "r2-cd:cd:120:none:"
  "r3-cd:cd:120:none:"  "r3-sd:sd:120:none:"
  "r4-sd:sd:120:none:"  "r4-cd:cd:120:none:"
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
            [ "$a" = absent ] && d=absent || d=$(( a - ${b/absent/0} ))
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

ckptsamp() { # o: every 2 s, each validator's /metrics: checkpoint family + reject/failure counters (time series, ckpt-samples.txt).
    # Header line per scrape: "T <epoch> val<i> ok|ERR". Before the nodes are up / after they stop, scrapes are ERR (tolerated).
    local o=$1 i m ts
    while :; do
        for i in 0 1 2; do
            ts=$(date +%s.%N)
            if m=$(curl -fsS -m 2 "http://127.0.0.1:${METS[$i]}/metrics" 2>/dev/null); then
                echo "T $ts val$i ok"
                grep -E '^torus_(exec_checkpoint|flush_worker_checkpoint|exec_action_(rejections|failures)|rpc_submit_admit_rejects|missing_action_rejections|orders_rejected_|order_age_future_nonce|consensus_dedup_dropped|rpc_forward_dropped|native_gossip_dropped|mempool_native_size|block_height |exec_cancel_all)' <<< "$m"
            else
                echo "T $ts val$i ERR"
            fi
        done
        sleep 2
    done >> "$o/ckpt-samples.txt"
}

statechk() { # label arm: the cell's genesis state hash (run.log has the generator's line)
    local label=$1 arm=$2 want
    want='"off"'
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
    n=$(grep -c " fstype=${AFS[$arm]}\$" "$o/node-datafs.txt" 2>/dev/null)
    [ "$n" = 3 ] || { echo "CHECK FAIL $1: data dir fs not ${AFS[$arm]} on 3 nodes: $(tr '\n' ';' < "$o/node-datafs.txt" 2>/dev/null)"; return 1; }
    echo "CHECK OK $1 data dirs on ${AFS[$arm]} ($(awk '{print $2}' "$o/node-datafs.txt" | tr '\n' ' '))"
}

tailchk() { # label: HARD check that the cell ran the bench in commit-tail mode
    local o=$R/$1
    grep -q "in_flight_tail='commit'" "$o/run.log" || { echo "CHECK FAIL $1: run.log has no in_flight_tail='commit'"; return 1; }
    grep -q -- '--in-flight-tail commit' "$o/run.log" || { echo "CHECK FAIL $1: bench cmd lacks --in-flight-tail commit"; return 1; }
    grep -q 'In-flight cap .*| tail=commit' "$o/bench.log" || { echo "CHECK FAIL $1: bench.log In-flight line lacks tail=commit"; return 1; }
    python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])); a=s["cell"].get("in_flight_tail"); b=(s["ingest"].get("in_flight") or {}).get("tail_mode"); sys.exit(0 if a==b=="commit" else (print("cell.in_flight_tail=%r ingest.in_flight.tail_mode=%r" % (a,b)) or 1))' "$o/summary.json" \
        || { echo "CHECK FAIL $1: summary.json tail mode is not commit"; return 1; }
    echo "CHECK OK $1 in-flight tail = commit (run.log, bench cmd, bench.log, summary.json cell + ingest)"
}

cell() { # tag arm dur kind killat. return 0 = go on, 1 = abort
    local tg=$1 arm=$2 dur=$3 kind=$4 killat=${5:-} label rcf unit sig=none
    local td=${ATD[${ABIN[$arm]:-x}]:-} nm=${NMA[${ABIN[$arm]:-x}]:-} bench=$BENCH bmd5=$BENCH_MD5 mif=$N bud=$BUDGET xe=${ARMX[$arm]:-}
    local EP= CP= TP= FP= IP= RP= WSP= WP= DP= KP= mp= HP= t=0 rc fdok=0 nt=0 f
    DD=${AROOT[$arm]:-}; [ -n "$DD" ] || { echo "ABORT: no data root for arm $arm"; return 1; }
    local -a E
    label=$P-$tg; rcf=$R/$label.cell.rc; unit=bench-$label
    CRASHCELL=0; [ "$kind" = none ] || CRASHCELL=1
    case $kind in graceful) sig=TERM ;; crash) sig=KILL ;; esac
    [ -n "${ARMX[$arm]:-}" ] || { echo "ABORT: unknown arm $arm"; return 1; }
    { [ -n "$td" ] && [ -n "$nm" ]; } || { echo "ABORT: no binary dir / md5 for arm $arm"; return 1; }
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    hostlog "$label"
    if [ "${AFS[$arm]}" = tmpfs ]; then # tmpfs fit: free + this campaign's leftover tmpfs data (wiped by this cell's CLEAN=1) >= 16 GB
        f=$(du -sk "$SHM/data" 2>/dev/null | cut -f1); f=$(( $(df --output=avail -k /dev/shm | tail -1 | tr -d ' ') + ${f:-0} ))
        [ "$f" -ge 16000000 ] || { echo "[$(date +%T)] ABORT before $label: tmpfs room ${f} kB < 16 GB"; return 1; }
        [ "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" -ge 30000000 ] || { echo "[$(date +%T)] ABORT before $label: MemAvailable < 30 GB"; return 1; }
    fi
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm dur=$dur kind=$kind crash_kill_at=${killat:-none} sig=$sig state_hash=${ARMSH[$arm]:-0} max_in_flight=$N budget=$BUDGET extra_env=${xe:-none} node_md5=$nm bench_md5=${bmd5:0:8} load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & CP=$!
    "$T3/io-sampler.sh" "$R/$label" 2>/dev/null & IP=$!
    dusamp "$R/$label" & DP=$!
    ckptsamp "$R/$label" & KP=$!
    python3 "$T/task-sampler.py" "$R/$label" & TP=$!
    fdsampler "$R/$label" & FP=$!
    if [ "$CRASHCELL" = 1 ]; then restenv "$R/$label" & RP=$!; walsamp "$R/$label" & WSP=$!; fi
    E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
       ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 DATA_ROOT="$DD" IN_FLIGHT_TAIL=commit)
    if [ "${ARMSH[$arm]:-0}" = 1 ]; then E+=(STATE_HASH_ACTIVATION=1); fi
    case $kind in
        crash)    E+=(CRASH_KILL_AT_S="$killat") ;;
        graceful) E+=(CRASH_KILL_AT_S="$killat" KILL_SIG=TERM KILL_WAIT=60) ;;
    esac
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MK" "$dur" 76000 "$xe" || { stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP" "$DP" "$KP"; return 1; }
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP" "$DP" "$KP"; return 1; }
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
    savelog "$label"
    if [ -e "$R/$label/node-death.txt" ]; then
        stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP"
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
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    if [ "$fdok" = 1 ]; then echo "[$(date +%T)] STOP: a node held > $FD_STOP open fds in $label (set LimitNOFILE for both arms first)"; return 1; fi
    return 0
}

# SMOKE=1: one 60 s sd cell under its own label prefix (prep check of the step 1 arm; not part of the measured campaign)
if [ "${SMOKE:-0}" = 1 ]; then P=ozarchy-p3s2g-smoke-300m; CELLS=("s0:sd:60:none:"); fi
# ---- preflight ----
PF() { echo "PREFLIGHT FAIL: $*"; exit 1; }
# step 1 parameters (given at launch, from the pushed feat/item6-phase3-step1 commit)
[[ "$CTL_SHA" =~ ^[0-9a-f]{40}$ ]] || PF "CTL_SHA must be 40 hex"
[[ "$S1_SHA" =~ ^[0-9a-f]{40}$ ]] || PF "S1_SHA must be 40 hex (step 1 commit)"
[[ "$S1_NODE_MD5" =~ ^[0-9a-f]{32}$ ]] || PF "S1_NODE_MD5 must be 32 hex (from $MD5_BUILD)"
[[ "$S1_KNOB" =~ ^TORUS_[A-Z0-9_]+=[^\ ]+$ ]] || PF "S1_KNOB must be one TORUS_NAME=VALUE (the step 1 interval knob at 15 s)"
case $S1_KNOB_NAME in TORUS_P3_PROTO*|TORUS_ROCKSDB_*|TORUS_TRADE_HISTORY|TORUS_BOOK_ROWS) PF "S1_KNOB $S1_KNOB_NAME collides with a checked knob" ;; esac
# control = the step 1 commit's merge-base with origin/main (step 1 branched from it, or rebased on it = current main).
# Run 'git -C $REPO fetch origin' before the launch; the script does not fetch.
git -C "$REPO" cat-file -e "$S1_SHA^{commit}" 2>/dev/null || PF "$S1_SHA not in $REPO (fetch first)"
git -C "$REPO" cat-file -e "$CTL_SHA^{commit}" 2>/dev/null || PF "$CTL_SHA not in $REPO"
MB=$(git -C "$REPO" merge-base "$S1_SHA" "$CTL_SHA") || PF "merge-base $S1_SHA $CTL_SHA failed"
echo "[$(date +%T)] control note (confound, 18c): control=$CTL_SHA, step 2 base = merge-base(step2, control) = $MB; control adds $(git -C "$REPO" rev-list --count "$MB..$CTL_SHA") commits not in step 2"
{ [ "$(git -C "$CTL_WT" rev-parse HEAD)" = "$CTL_SHA" ] && [ -z "$(git -C "$CTL_WT" status --porcelain)" ]; } || PF "$CTL_WT not clean at $CTL_SHA"
{ [ "$(git -C "$S1_WT" rev-parse HEAD)" = "$S1_SHA" ] && [ -z "$(git -C "$S1_WT" status --porcelain)" ]; } || PF "$S1_WT not clean at $S1_SHA"
# binaries: build records + staged copies
[ -n "$CTL_NODE_MD5" ] && [ -n "$CTL_BENCH_MD5" ] || PF "no 'c $CTL_SHA' / 'bench-c $CTL_SHA' line in $MD5_BUILD"
grep -q "^s $S1_SHA $S1_NODE_MD5 " "$MD5_BUILD" || PF "no 's $S1_SHA $S1_NODE_MD5' line in $MD5_BUILD"
mkdir -p "$ST/c-run/release" || PF "mkdir c-run"
[ -e "$ST/c-run/release/torus-node" ] || cp -p --reflink=auto "$ST/c/release/torus-node" "$ST/c-run/release/torus-node" || PF "stage c-run node"
[ -e "$ST/c-run/release/bench-throughput" ] || cp -p --reflink=auto "$BENCH" "$ST/c-run/release/bench-throughput" || PF "stage c-run bench"
[ "$(md5sum < "$ST/c-run/release/torus-node" | cut -c1-32)" = "$CTL_NODE_MD5" ] || PF "c-run torus-node md5 is not $CTL_NODE_MD5"
[ "$(md5sum < "$ST/c-run/release/bench-throughput" | cut -c1-32)" = "$CTL_BENCH_MD5" ] || PF "c-run bench md5 is not $CTL_BENCH_MD5"
[ "$CTL_NODE_MD5" != "$S1_NODE_MD5" ] || PF "control and step 1 node md5 are equal"
[ "$(md5sum < "$ST/c/release/torus-node" | cut -c1-32)" = "$CTL_NODE_MD5" ] || PF "staged control torus-node md5 is not $CTL_NODE_MD5"
[ "$(md5sum < "$BENCH" | cut -c1-32)" = "$CTL_BENCH_MD5" ] || PF "staged commit-tail bench-throughput md5 is not $CTL_BENCH_MD5"
[ "$(md5sum < "$ST/s/release/torus-node" | cut -c1-32)" = "$S1_NODE_MD5" ] || PF "staged step 1 torus-node md5 is not $S1_NODE_MD5"
mkdir -p "$ST/s-run/release" || PF "mkdir s-run"
[ -e "$ST/s-run/release/torus-node" ] || cp -p --reflink=auto "$ST/s/release/torus-node" "$ST/s-run/release/torus-node" || PF "stage s-run node"
[ -e "$ST/s-run/release/bench-throughput" ] || cp -p --reflink=auto "$BENCH" "$ST/s-run/release/bench-throughput" || PF "stage s-run bench"
[ "$(md5sum < "$ST/s-run/release/torus-node" | cut -c1-32)" = "$S1_NODE_MD5" ] || PF "s-run torus-node md5 is not $S1_NODE_MD5"
[ "$(md5sum < "$ST/s-run/release/bench-throughput" | cut -c1-32)" = "$CTL_BENCH_MD5" ] || PF "s-run bench md5 is not the control bench $CTL_BENCH_MD5"
[ -x "$ST/s-run/release/torus-node" ] && [ -x "$ST/c/release/torus-node" ] || PF "staged node not executable"
[ -f "$POST" ] || PF "$POST missing"
{ [ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ]; } || PF "$WT_H not clean at $SHA_HARNESS"
grep -q 'IN_FLIGHT_TAIL' "$HARNESS" || PF "$HARNESS does not know IN_FLIGHT_TAIL"
[ "$(stat -f -c %T /dev/shm)" = tmpfs ] || PF "/dev/shm is not tmpfs"
[ "$(stat -f -c %T "$DEVNET")" = btrfs ] || PF "$DEVNET is not on btrfs (shared NVMe)"
[ "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" -ge 40000000 ] || PF "MemAvailable < 40 GB"
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
echo "[$(date +%T)] PREFLIGHT OK harness=${SHA_HARNESS:0:8} control=${NMA[c]} ($CTL_SHA) step1=${NMA[s]} ($S1_SHA) knob=$S1_KNOB enable_re=${S1_ENABLE_RE:-none} require_ckpt=$S1_REQUIRE_CKPT bench=${BENCH_MD5:0:8} N=$N budget=$BUDGET nofile_soft=$(ulimit -Sn) cells=${#CELLS[@]}: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
for c in "${CELLS[@]}"; do
    IFS=: read -r tg arm dur kind killat <<< "$c"
    cell "$tg" "$arm" "$dur" "$kind" "$killat" || { echo "[$(date +%T)] CAMPAIGN STOP at $tg"; exit 1; }
done
echo "[$(date +%T)] CAMPAIGN END"
