#!/usr/bin/env bash
# ozarchy-p3rss (campaign A: Phase 3 step 2 RSS split) and ozarchy-p23b (campaign B: P2-3 exec worker pool A/B), 18c s112.
# Copy of ozarchy-p3s2g-campaign.sh (section 47) generalised to N arms. MEASUREMENT ONLY (no node change).
# Shape for both: tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s cells, warm-up w0,
# rotated order, data on the shared NVMe (btrfs). ONE bench + harness from origin/main 8efcb07a (wt/p3rss-8efcb07a).
#
# Campaign A (CAMP=A, prefix ozarchy-p3rss-300m): split step 2's +1.5 GB RSS (section 47) into
#   backlog = ctl - cth (commit-tail exec backlog), layer = s1 - ctl (step 1 since-checkpoint layer), view = s2 - s1.
#   Binaries: c = control 2725e321 (= merge-base(step 1, main); step 1 a44bdde6 is 13 commits on it, step 2 2296be67 is
#   5 more on step 1: linear chain, clean split), s1 = step 1 a44bdde6, s2 = step 2 2296be67, m = main 8efcb07a (cross-check:
#   main adds corewriter results / RPC / action_status, not a clean control).
#   Arms: cth (c, tail head), ctl (c, commit), s1 (s1, commit, knob), s2 (s2, commit, knob), mn (m, commit);
#         heap-profiled (jemalloc LD_PRELOAD + prof, via a wrapper torus-node that execs the real binary):
#         hcth, hctl, hs1, hs2 (same as the unprofiled arms otherwise).
# Campaign B (CAMP=B, prefix ozarchy-p23b-300m): P2-3 pool efcd1548 vs its merge-base c97f56f9 (main adds corewriter results in
#   the exec path, so main is not a clean control). Commit tail. Arms k (control c97f56f9), p (pool efcd1548); perf-profiled
#   pk / pp: prof-sidecar.sh (perf record cycles:u + task events on val0, 35 s after bench start for 45 s, section 44 method).
# Every cell (all arms): memsamp = per-node smaps_rollup + /proc status every 5 s, val0 full smaps every 30 s (gz), and the
#   memory / queue gauges from /metrics every 5 s (mem-samples.txt).
# HARD per-cell checks: as p3s2g (md5 of /proc/<pid>/exe per arm, env, book CF, codec/WAL, trie, WAL cap, zero trade rows,
#   state hash off, btrfs, exec pipeline on) + knob only on knob arms, enable line + torus_exec_checkpoints >= 1 only on knob
#   arms, tailchk for the arm's tail mode (commit or head), heapchk (LD_PRELOAD jemalloc on heap arms only, dumps written).
# Usage: CAMP=A|B ozarchy-p3rss-campaign.sh 4     DRY_RUN=1 preflight only; SMOKE=<arm> one 60 s cell of that arm (label -smoke-).
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: CAMP=A|B $0 N   (MAX_IN_FLIGHT, e.g. 4)"; exit 2; }
unset TORUS_MODEL_PROFILE CRASH_KILL_AT_S TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_TRADE_HISTORY TORUS_ROCKSDB_MAX_TOTAL_WAL_MB TORUS_P3_PROTO TORUS_P3_PROTO_INTERVAL STATE_HASH_ACTIVATION KILL_SIG KILL_WAIT LD_PRELOAD MALLOC_CONF P3RSS_HEAP_PREFIX TORUS_CHECKPOINT_INTERVAL_MS
CAMP=${CAMP:-}
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
DEVNET=/home/oz/torus-wsl-devnet
SHM=/dev/shm/ozarchy-p3rss-devnet
KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000
ENABLE_RE='item 6 Phase 3: coalesced state checkpoints ON'
PIPE_RE=${PIPE_RE:-bl2 exec pipeline ENABLED}
SHA_HARNESS=8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4
WT_H=/home/oz/projects/wt/p3rss-8efcb07a
HARNESS=$WT_H/tools/matched-bench/run-cell.sh
DETACH=$WT_H/tools/matched-bench/campaign/detach.sh
TOOLS=$WT_H/tools/matched-bench
T=$R/ozarchy-14236fa-tools
T3=$R/ozarchy-p3s0r-tools
PYCHK=$R/ozarchy-acc-codec-check.py
LSM=$R/ozarchy-acc3-tools/ozarchy-acc3-lsm.py
RSTPY=$R/ozarchy-acc3-tools/ozarchy-acc3-restart.py
POST=$R/ozarchy-p3s2g-tools/ozarchy-p3c1-post.py
ST=$R/ozarchy-p3rss-stage
MD5_BUILD=$R/ozarchy-p3rss-build/md5s.txt
JEMALLOC=/usr/lib/libjemalloc.so.2
MK=300
METS=(9161 9162 9163)
CRASHCELL=0
# binaries: key -> sha, worktree (clean at sha), node md5 from MD5_BUILD ("<key> <sha> <md5> ..." lines)
declare -A BSHA BWT BMD5
BSHA[c]=2725e321bdc48f9ca137166f272b8eaefbd5ba3e;  BWT[c]=/home/oz/projects/wt/p3c1-2725e321
BSHA[s1]=a44bdde6d25cd3bb80723eaed961b2ef96052eca; BWT[s1]=/home/oz/projects/wt/p3r1-a44bdde6
BSHA[s2]=2296be6727eb712f4c41f5150c5b834cc939b415; BWT[s2]=/home/oz/projects/wt/p3s2g-2296be67
BSHA[m]=8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4;  BWT[m]=/home/oz/projects/wt/p3rss-8efcb07a
BSHA[k]=c97f56f927994d88a2a00f63d147ce3cfaf0ae6b;  BWT[k]=/home/oz/projects/wt/p3s2g-c97f56f9
BSHA[p]=efcd15481891dd29c9ec87c01ac679b897c25421;  BWT[p]=/home/oz/projects/wt/p23b-efcd1548
for b in "${!BSHA[@]}"; do BMD5[$b]=$(awk -v k="$b" -v s="${BSHA[$b]}" '$1==k && $2==s {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1); done
BENCH=$ST/b/release/bench-throughput
BENCH_MD5=$(awk -v s="$SHA_HARNESS" '$1=="b" && $2==s {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)
KNOBS="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_BOOK_ROWS="
# arms: ABIN binary key, ATAIL head|commit, AKNOB 1 = KNOB set (checkpoint arms), APROF none|heap|perf
declare -A ABIN ATAIL AKNOB APROF KNOBX WALCAP
arm() { ABIN[$1]=$2; ATAIL[$1]=$3; AKNOB[$1]=$4; APROF[$1]=$5; }
arm cth c head 0 none;   arm ctl c commit 0 none;   arm s1 s1 commit 1 none;   arm s2 s2 commit 1 none;   arm mn m commit 0 none
arm hcth c head 0 heap;  arm hctl c commit 0 heap;  arm hs1 s1 commit 1 heap;  arm hs2 s2 commit 1 heap
arm k k commit 0 none;   arm p p commit 0 none;     arm pk k commit 0 perf;    arm pp p commit 0 perf
DD=$DEVNET
KNOBX[tw]=""; WALCAP[tw]=2147483648
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
case $CAMP in
  A) RUN=$R/ozarchy-p3rss-run; P=ozarchy-p3rss-300m
     # rotated (Latin square) r1-r4 over cth ctl s1 s2; mn x2 and the heap cells (hctl hs1 hs2 x2, hcth x1) after r2 and r4
     CELLS=(
       "w0:ctl:60"
       "r1-cth:cth:120" "r1-ctl:ctl:120" "r1-s1:s1:120"   "r1-s2:s2:120"
       "r2-ctl:ctl:120" "r2-s1:s1:120"   "r2-s2:s2:120"   "r2-cth:cth:120"
       "m1:mn:120" "h1-ctl:hctl:120" "h1-s1:hs1:120" "h1-s2:hs2:120"
       "r3-s1:s1:120"   "r3-s2:s2:120"   "r3-cth:cth:120" "r3-ctl:ctl:120"
       "r4-s2:s2:120"   "r4-cth:cth:120" "r4-ctl:ctl:120" "r4-s1:s1:120"
       "m2:mn:120" "h2-s2:hs2:120" "h2-s1:hs1:120" "h2-ctl:hctl:120" "h1-cth:hcth:120"
     ) ;;
  B) RUN=$R/ozarchy-p23b-run; P=ozarchy-p23b-300m
     # ABBA u cells, perf cells (separate, as p23) after r2 and r4
     CELLS=(
       "w0:k:60"
       "r1-k:k:120" "r1-p:p:120" "r2-p:p:120" "r2-k:k:120"
       "x1-pp:pp:120" "x1-pk:pk:120"
       "r3-k:k:120" "r3-p:p:120" "r4-p:p:120" "r4-k:k:120"
       "x2-pk:pk:120" "x2-pp:pp:120"
     ) ;;
  *) echo "CAMP must be A or B"; exit 2 ;;
esac
if [ -n "${SMOKE:-}" ]; then P=${P%-300m}-smoke-300m; CELLS=("s0-$SMOKE:$SMOKE:60"); fi

declare -A AFS; for a in "${!ABIN[@]}"; do AFS[$a]=btrfs; done

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


knobchk() { # label arm file lines: KNOB (NAME=VALUE) on every node line of file for knob arms; the NAME on no line for the others.
    # No TORUS_P3_PROTO* and no TORUS_CHECKPOINT_MAX_BYTES anywhere.
    local label=$1 arm=$2 f=$3 want=$4 n name=${KNOB%%=*}
    n=$(grep -c ' TORUS_P3_PROTO' "$f")
    [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_P3_PROTO* set on $n node lines ($(basename "$f"))"; return 1; }
    n=$(grep -c ' TORUS_CHECKPOINT_MAX_BYTES=' "$f")
    [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_CHECKPOINT_MAX_BYTES set on $n node lines ($(basename "$f"))"; return 1; }
    if [ "${AKNOB[$arm]}" = 1 ]; then
        n=$(grep -c " $KNOB " "$f"); [ "$n" = "$want" ] || { echo "CHECK FAIL $label: $KNOB on $n of $want node lines"; return 1; }
    else
        n=$(grep -c " $name=" "$f"); [ "$n" = 0 ] || { echo "CHECK FAIL $label: $name set on $n node lines (arm $arm has no knob)"; return 1; }
    fi
    echo "CHECK OK $label knob (arm $arm: $([ "${AKNOB[$arm]}" = 1 ] && echo "$KNOB" || echo "$name unset"))"
}

enablechk() { # label arm: the Phase 3 enable line on each validator log: >= 1 on knob arms, 0 elsewhere
    local label=$1 arm=$2 o=$R/$1 v n tot=""
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -c -E "$ENABLE_RE")
        if [ "${AKNOB[$arm]}" = 1 ]; then
            [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: val$v has no enable line /$ENABLE_RE/"; return 1; }
        else
            [ "$n" = 0 ] || { echo "CHECK FAIL $label: val$v shows the enable line ($n) on arm $arm"; return 1; }
        fi
        tot="$tot val$v=$n"
    done
    echo "CHECK OK $label enable line (arm $arm:$tot)"
}

ckptchk() { # label arm: torus_exec_checkpoints delta >= 1 per node on knob arms; absent or 0 elsewhere
    local label=$1 arm=$2 o=$R/$1 v b a d tot=""
    for v in 0 1 2; do
        b=$(ckval "$o/metrics-before-val$v.txt"); a=$(ckval "$o/metrics-after-val$v.txt")
        if [ "${AKNOB[$arm]}" = 1 ]; then
            [ "$a" = absent ] && d=absent || d=$(( a - ${b/absent/0} ))
            { [ "$d" = absent ] || [ "$d" -lt 1 ]; } && { echo "CHECK FAIL $label: val$v torus_exec_checkpoints delta=$d (arm $arm)"; return 1; }
        else
            d=$a
            { [ "$a" = absent ] || [ "$a" = 0 ]; } || { echo "CHECK FAIL $label: val$v arm $arm has torus_exec_checkpoints=$a"; return 1; }
        fi
        tot="$tot val$v=$d"
    done
    echo "CHECK OK $label checkpoints (arm $arm:$tot)"
}

tailchk() { # label mode: HARD check that the cell ran the bench in the arm's tail mode (commit or head)
    local o=$R/$1 m=$2
    grep -q "in_flight_tail='$m'" "$o/run.log" || { echo "CHECK FAIL $1: run.log has no in_flight_tail='$m'"; return 1; }
    grep -q -- "--in-flight-tail $m" "$o/run.log" || { echo "CHECK FAIL $1: bench cmd lacks --in-flight-tail $m"; return 1; }
    if [ "$m" = commit ]; then
        grep -q 'In-flight cap .*| tail=commit' "$o/bench.log" || { echo "CHECK FAIL $1: bench.log In-flight line lacks tail=commit"; return 1; }
    else
        grep -q 'In-flight cap ' "$o/bench.log" && ! grep -q 'In-flight cap .*| tail=' "$o/bench.log" || { echo "CHECK FAIL $1: bench.log In-flight line not head mode"; return 1; }
    fi
    python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])); m=sys.argv[2]; a=s["cell"].get("in_flight_tail"); b=(s["ingest"].get("in_flight") or {}).get("tail_mode"); sys.exit(0 if a==b==m else (print("cell.in_flight_tail=%r ingest.in_flight.tail_mode=%r" % (a,b)) or 1))' "$o/summary.json" "$m" \
        || { echo "CHECK FAIL $1: summary.json tail mode is not $m"; return 1; }
    echo "CHECK OK $1 in-flight tail = $m (run.log, bench cmd, bench.log, summary.json cell + ingest)"
}

heapchk() { # label arm: LD_PRELOAD jemalloc + MALLOC_CONF prof on the 3 nodes of heap arms (and >= 1 dump per node); on no node elsewhere
    local label=$1 arm=$2 o=$R/$1 n p tot=""
    if [ "${APROF[$arm]}" = heap ]; then
        n=$(grep -c "LD_PRELOAD=$JEMALLOC" "$o/node-preload.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $label: jemalloc preload on $n of 3 nodes"; return 1; }
        n=$(grep -c 'MALLOC_CONF=prof:true' "$o/node-preload.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $label: MALLOC_CONF prof on $n of 3 nodes"; return 1; }
        n=$(grep -c "$JEMALLOC" "$o/node-maps-jemalloc.txt"); [ "$n" = 3 ] || { echo "CHECK FAIL $label: libjemalloc mapped in $n of 3 nodes"; return 1; }
        for p in $(awk '{sub("pid=","",$1); print $1}' "$o/node-preload.txt"); do
            n=$(ls "$o"/heap/jeprof.$p.*.heap 2>/dev/null | wc -l); [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: no heap dump for pid $p"; return 1; }
            tot="$tot $p=$n"
        done
        echo "CHECK OK $label heap profile (jemalloc preload + prof on 3 nodes; dumps:$tot)"
    else
        n=$(grep -c 'LD_PRELOAD=' "$o/node-preload.txt"); [ "$n" = 0 ] || { echo "CHECK FAIL $label: LD_PRELOAD on $n nodes (arm $arm)"; return 1; }
        n=$(grep -c 'jemalloc' "$o/node-maps-jemalloc.txt"); [ "$n" = 0 ] || { echo "CHECK FAIL $label: jemalloc mapped on arm $arm"; return 1; }
        echo "CHECK OK $label no preload (arm $arm, glibc malloc)"
    fi
}

preloadrec() { # o: LD_PRELOAD / MALLOC_CONF and mapped jemalloc of each node pid, once the bench runs
    local o=$1 t=0 p
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p $(tr '\0' '\n' < /proc/$p/environ | grep -E '^(LD_PRELOAD|MALLOC_CONF)=' | sort | tr '\n' ' ')"
    done > "$o/node-preload.txt" 2>&1
    for p in $(cat $DEVNET/run/pids); do
        echo "pid=$p $(grep -m1 -o '/[^ ]*jemalloc[^ ]*' /proc/$p/maps 2>/dev/null)"
    done > "$o/node-maps-jemalloc.txt" 2>&1
}

memsamp() { # o: every 5 s per node: smaps_rollup + /proc status memory lines + memory/queue gauges; val0 full smaps every 30 s (gz)
    local o=$1 pids= t=0 p i n=0 ts m
    until pids=$(grep -m1 '\] node pids: ' "$o/run.log" 2>/dev/null | sed 's/.*node pids: //'); [ -n "$pids" ]; do
        sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 0
    done
    mkdir -p "$o/smaps"
    while :; do
        i=0
        for p in $pids; do
            ts=$(date +%s.%N)
            if [ -r /proc/$p/smaps_rollup ]; then
                echo "M $ts val$i $p $(awk '/^(Rss|Pss_Anon|Pss_File|Pss_Shmem|Anonymous|Swap|Private_Dirty|Private_Clean|Shared_Clean|AnonHugePages):/{printf "%s=%s ", $1, $2}' /proc/$p/smaps_rollup 2>/dev/null)$(awk '/^(VmHWM|VmRSS|RssAnon|RssFile|VmSwap|Threads):/{printf "%s=%s ", $1, $2}' /proc/$p/status 2>/dev/null)"
            fi
            if m=$(curl -fsS -m 2 "http://127.0.0.1:${METS[$i]}/metrics" 2>/dev/null); then
                echo "G $(date +%s.%N) val$i $(grep -E '^torus_(rocksdb_block_cache_bytes|rocksdb_memtable_bytes_all|rocksdb_immutable_memtables_all|mempool_native_size|mempool_evm_size|exec_queue_depth|flush_worker_depth|block_height|blocks_committed_total|exec_native_blocks_total|exec_checkpoint_layer_bytes_(sum|count)|exec_checkpoints_total|exec_resident_rows_bytes|member_cache_resident_buckets|trade_writer_queued_batches|post_flush_writer_queued_batches|exec_resting_orders|exec_cancel_all_index_entries|exec_level_hash_cache_entries|exec_action_status_bytes_sum|exec_thread_spawns_[a-z_]+) ' <<< "$m" | tr ' ' '=' | tr '\n' ' ')"
            fi
            i=$((i+1))
        done
        if [ $((n % 6)) = 0 ]; then
            p=${pids%% *}; [ -r /proc/$p/smaps ] && { cat /proc/$p/smaps 2>/dev/null | gzip -1 > "$o/smaps/val0-$(date +%s).smaps.gz"; }
        fi
        n=$((n+1))
        sleep 5
    done >> "$o/mem-samples.txt"
}

cell() { # tag arm dur. return 0 = go on, 1 = abort
    local tg=$1 arm=$2 dur=$3 label rcf unit
    local b=${ABIN[$arm]:-} prof=${APROF[$arm]:-} tail=${ATAIL[$arm]:-} td nm bench=$BENCH bmd5=$BENCH_MD5 mif=$N bud=$BUDGET xe=$KNOBS
    local EP= CP= TP= FP= IP= WP= DP= KP= MP= LP= PP= mp= HP= t=0 rc fdok=0 nt=0
    local -a E
    [ -n "$b" ] && [ -n "$tail" ] || { echo "ABORT: unknown arm $arm"; return 1; }
    nm=${BMD5[$b]:0:8}
    case $prof in heap) td=$ST/$b-heap ;; *) td=$ST/$b-run ;; esac
    [ "${AKNOB[$arm]}" = 1 ] && xe="$KNOBS $KNOB"
    DD=$DEVNET
    label=$P-$tg; rcf=$R/$label.cell.rc; unit=bench-$label
    [ -n "$nm" ] && [ -x "$td/release/torus-node" ] || { echo "ABORT: no binary dir / md5 for arm $arm"; return 1; }
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    hostlog "$label"
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm bin=$b sha=${BSHA[$b]:0:8} tail=$tail knob=$([ "${AKNOB[$arm]}" = 1 ] && echo "$KNOB" || echo none) prof=$prof dur=$dur max_in_flight=$N budget=$BUDGET node_md5=$nm bench_md5=${bmd5:0:8} load=$(cut -d' ' -f1 /proc/loadavg) mem_avail_kb=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo) swap_used_kb=$(awk '/^SwapTotal:/{t=$2} /^SwapFree:/{f=$2} END{print t-f}' /proc/meminfo)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & EP=$!
    preloadrec "$R/$label" & LP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & CP=$!
    "$T3/io-sampler.sh" "$R/$label" 2>/dev/null & IP=$!
    dusamp "$R/$label" & DP=$!
    ckptsamp "$R/$label" & KP=$!
    memsamp "$R/$label" & MP=$!
    python3 "$T/task-sampler.py" "$R/$label" & TP=$!
    fdsampler "$R/$label" & FP=$!
    E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
       ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 DATA_ROOT="$DD" IN_FLIGHT_TAIL="$tail")
    if [ "$prof" = heap ]; then mkdir -p "$R/$label/heap"; E+=(P3RSS_HEAP_PREFIX="$R/$label/heap/jeprof"); fi
    if [ "$prof" = perf ]; then "$T/prof-sidecar.sh" "$R/$label" 35 45 "$bench" "$MK" & PP=$!; echo "[$(date +%T)] $label perf sidecar pid $PP (35 s + 45 s, val0)"; fi
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MK" "$dur" 76000 "$xe" || { stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$DP" "$KP" "$MP" "$LP" "$PP"; return 1; }
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$DP" "$KP" "$MP" "$LP" "$PP"; return 1; }
    done
    echo "[$(date +%T)] $label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & WP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    while [ -n "$(pgrep -x torus-node)" ] && [ $nt -lt 60 ]; do sleep 1; nt=$((nt+1)); done
    [ -z "$(pgrep -x torus-node)" ] || echo "[$(date +%T)] WARN $label: torus-node still running at LOG copy"
    stopbg "$DP" "$KP" "$MP"; wait "$DP" "$KP" "$MP" 2>/dev/null
    savelog "$label"
    if [ -e "$R/$label/node-death.txt" ]; then
        stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$LP" "$PP"
        echo "[$(date +%T)] CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { echo "ABORT: torus-node left running after $label"; return 1; }
        return 0
    fi
    wait "$EP" "$CP" "$TP" "$FP" "$IP" "$LP"
    if [ -n "$PP" ]; then wait "$PP"; echo "[$label] perf: $(tail -2 "$R/$label/prof-sidecar.log" | tr '\n' ' ') perf.data $(stat -c %s "$R/$label/perf.data" 2>/dev/null) bytes"; fi
    echo "[$(date +%T)] END $label rc=$rc $(tail -1 "$R/$label.campaign.log")"
    grep -E 'Econ mix|In-flight cap|Cancel-by-id|WARNING: in-flight' "$R/$label/run.log" "$R/$label/bench.log" 2>/dev/null | sed "s/^/[$label] /" | head -20
    echo "[$label] $(fdsum "$label")" || true; fdsum "$label" > /dev/null || fdok=1
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        check "$label" 0 || return 1
        envok "$label" tw || return 1
        bookcf "$label" tw || return 1
        knobok "$label" tw "$R/$label/node-env-all.txt" 3 || return 1
        codeccheck "$label" tw || return 1
        lsmcheck "$label" tw 0 || return 1
        knobchk "$label" "$arm" "$R/$label/node-env-all.txt" 3 || return 1
        enablechk "$label" "$arm" || return 1
        ckptchk "$label" "$arm" || return 1
        pipechk "$label" "$arm" || return 1
        statechk "$label" "$arm" || return 1
        fschk "$label" "$arm" || return 1
        tailchk "$label" "$tail" || return 1
        heapchk "$label" "$arm" || return 1
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
    echo "[$label] heap dumps: $(ls "$R/$label"/heap/*.heap 2>/dev/null | wc -l) ($(du -sh "$R/$label/heap" 2>/dev/null | cut -f1)); smaps snapshots: $(ls "$R/$label"/smaps 2>/dev/null | wc -l); mem samples: $(grep -c '^M ' "$R/$label/mem-samples.txt" 2>/dev/null)"
    python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); h=s["headline"]; print("[rates] matched/s", h["matched_s_avg"], "placed/s", h["placed_s_avg"], "submit/s", s["ingest"].get("bench_submit_rate"), "native blk/s", h["native_blk_s"], "verdict", s["validity"]["verdict"], h.get("agreement_verdict"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    awk -F, '$6=="torus-node"{v=$5; r=$8/1e6; if(r>m[v])m[v]=r; s[v]+=r; c[v]++} END{for(v in m) printf "%s rss peak/mean GB %.2f/%.2f  ", v, m[v], s[v]/c[v]; print ""}' "$R/$label/cpu.csv" 2>/dev/null | sed "s/^/[$label] [rss] /"
    python3 -I "$POST" "$R/$label" "$arm" 2>&1 | sed "s/^/[$label] /"
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    if [ "$fdok" = 1 ]; then echo "[$(date +%T)] STOP: a node held > $FD_STOP open fds in $label"; return 1; fi
    return 0
}

# ---- preflight ----
PF() { echo "PREFLIGHT FAIL: $*"; exit 1; }
mkdir -p "$RUN"
[ -n "$BENCH_MD5" ] || PF "no 'b $SHA_HARNESS' line in $MD5_BUILD"
[ "$(md5sum < "$BENCH" | cut -c1-32)" = "$BENCH_MD5" ] || PF "staged bench md5 is not $BENCH_MD5"
USED=$(for c in "${CELLS[@]}"; do IFS=: read -r _ a _ <<< "$c"; echo "${ABIN[$a]:-?}:${APROF[$a]:-?}"; done | sort -u)
for bp in $USED; do
    b=${bp%%:*}; pr=${bp##*:}
    [ -n "${BSHA[$b]:-}" ] || PF "unknown binary key $b"
    [[ "${BMD5[$b]}" =~ ^[0-9a-f]{32}$ ]] || PF "no '$b ${BSHA[$b]}' md5 line in $MD5_BUILD"
    { [ "$(git -C "${BWT[$b]}" rev-parse HEAD)" = "${BSHA[$b]}" ] && [ -z "$(git -C "${BWT[$b]}" status --porcelain)" ]; } || PF "${BWT[$b]} not clean at ${BSHA[$b]}"
    [ "$(md5sum < "$ST/$b-run/release/torus-node" | cut -c1-32)" = "${BMD5[$b]}" ] || PF "$ST/$b-run torus-node md5 is not ${BMD5[$b]}"
    [ "$(md5sum < "$ST/$b-run/release/bench-throughput" | cut -c1-32)" = "$BENCH_MD5" ] || PF "$ST/$b-run bench md5 is not $BENCH_MD5"
    if [ "$pr" = heap ]; then
        grep -q "^exec $ST/$b-run/release/torus-node \"\$@\"\$" "$ST/$b-heap/release/torus-node" || PF "$ST/$b-heap wrapper does not exec $ST/$b-run/release/torus-node"
        grep -q "LD_PRELOAD=$JEMALLOC" "$ST/$b-heap/release/torus-node" || PF "$ST/$b-heap wrapper lacks the jemalloc preload"
        [ -x "$ST/$b-heap/release/torus-node" ] || PF "$ST/$b-heap wrapper not executable"
        [ "$(md5sum < "$ST/$b-heap/release/bench-throughput" | cut -c1-32)" = "$BENCH_MD5" ] || PF "$ST/$b-heap bench md5 is not $BENCH_MD5"
    fi
done
for b1 in $(for bp in $USED; do echo "${bp%%:*}"; done | sort -u); do for b2 in $(for bp in $USED; do echo "${bp%%:*}"; done | sort -u); do
    [ "$b1" = "$b2" ] || [ "${BMD5[$b1]}" != "${BMD5[$b2]}" ] || PF "binaries $b1 and $b2 have the same md5"; done; done
case $CAMP in
  A) [ "$(git -C "$REPO" merge-base "${BSHA[s1]}" "${BSHA[m]}")" = "${BSHA[c]}" ] || PF "control is not merge-base(step 1, main)"
     git -C "$REPO" merge-base --is-ancestor "${BSHA[s1]}" "${BSHA[s2]}" || PF "step 1 is not an ancestor of step 2"
     echo "[$(date +%T)] chain: control ${BSHA[c]:0:8} = merge-base(step 1, main 8efcb07a); step 1 = control + $(git -C "$REPO" rev-list --count "${BSHA[c]}..${BSHA[s1]}") commits; step 2 = step 1 + $(git -C "$REPO" rev-list --count "${BSHA[s1]}..${BSHA[s2]}"); main = control + $(git -C "$REPO" rev-list --count "${BSHA[c]}..${BSHA[m]}") (cross-check only)" ;;
  B) [ "$(git -C "$REPO" merge-base "${BSHA[p]}" "${BSHA[k]}")" = "${BSHA[k]}" ] || PF "control is not the pool branch's merge-base"
     echo "[$(date +%T)] pool ${BSHA[p]:0:8} = control ${BSHA[k]:0:8} + $(git -C "$REPO" rev-list --count "${BSHA[k]}..${BSHA[p]}") commits; main 8efcb07a = control + $(git -C "$REPO" rev-list --count "${BSHA[k]}..${BSHA[m]}") (corewriter results in the exec path: not a clean control)"
     [ -x "$T/prof-sidecar.sh" ] || PF "prof-sidecar.sh missing"; command -v perf > /dev/null || PF "perf missing" ;;
esac
[ -f "$POST" ] || PF "$POST missing"
[ -r "$JEMALLOC" ] || PF "$JEMALLOC missing"
{ [ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ]; } || PF "$WT_H not clean at $SHA_HARNESS"
grep -q 'IN_FLIGHT_TAIL' "$HARNESS" || PF "$HARNESS does not know IN_FLIGHT_TAIL"
[ "$(stat -f -c %T "$DEVNET")" = btrfs ] || PF "$DEVNET is not on btrfs (shared NVMe)"
[ "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" -ge 40000000 ] || PF "MemAvailable < 40 GB"
for f in "$PYCHK" "$LSM" "$HARNESS" "$DETACH" "$T/task-sampler.py" "$T/positions.py"; do [ -f "$f" ] || PF "$f missing"; done
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
echo "[$(date +%T)] PREFLIGHT OK CAMP=$CAMP harness=${SHA_HARNESS:0:8} bench=${BENCH_MD5:0:8} binaries: $(for bp in $USED; do b=${bp%%:*}; printf '%s=%s/%s(%s) ' "$b" "${BSHA[$b]:0:8}" "${BMD5[$b]:0:8}" "${bp##*:}"; done) knob=$KNOB N=$N budget=$BUDGET nofile_soft=$(ulimit -Sn) cells=${#CELLS[@]}: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
for c in "${CELLS[@]}"; do
    IFS=: read -r tg arm dur <<< "$c"
    cell "$tg" "$arm" "$dur" || { echo "[$(date +%T)] CAMPAIGN STOP at $tg"; exit 1; }
done
echo "[$(date +%T)] CAMPAIGN END"
