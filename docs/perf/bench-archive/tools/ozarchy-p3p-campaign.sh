#!/usr/bin/env bash
# ozarchy-p3p: Phase 3 checkpoint-cost PROTOTYPE campaign (owner decision 18c, s111). Derived from ozarchy-p3s-campaign.sh:
# same harness (tools/matched-bench at 9b7e29b2, run-cell.sh unchanged), same 300-market Classic setup, same cell shape
# (MAX_IN_FLIGHT N, OPEN_ORDER_BUDGET 900, 120 s cells, CLEAN=1 = fresh data dir per cell).
# ONE node binary for every arm: proto/p3-coalesce 682a8df5 (THROWAWAY, NOT for merge), torus-node md5 6e40fa3c.
#   c    control: TORUS_P3_PROTO unset (tw node defaults since 8c5ca921)
#   a    TORUS_P3_PROTO=all    (every state row written in one batch every 100 blocks)
#   k    TORUS_P3_PROTO=books  (only Classic book blobs coalesced; not crash-recoverable -> graceful restarts only)
#   cS   control + STATE_HASH_ACTIVATION=1 (run-cell's own env -> gen-3val-genesis: consensus.state_hash_activation_height)
#   aS   all + STATE_HASH_ACTIVATION=1
# Throughput cells (120 s, round-robin, n=4 per arm, rounds rotated): r1..r4. w0 = warm-up (60 s, not analysed).
# Restart cells (120 s, val1 restarted from the same data dir, kill at bench+60 s):
#   g1, g2  graceful SIGTERM (KILL_SIG=TERM, KILL_WAIT=60; crash-kill.sh patched in ozarchy-p3p-tools/matched-bench):
#           arms c, a, k, each twice. CAVEAT: the control node only waits for SIGTERM when the p3 layer is on (main.rs
#           ~L1326); on the control SIGTERM is the default action (process killed, no shutdown). Recorded as-is.
#   x1      s75 multi-crash SIGKILL at bench+60,180,300 s (420 s cell), arms c and a, one cell each.
# Known false FAILs (recorded, not used): run-cell crash gate on the INFO line 'state hash fail-stop ... on=false', and the
# all-arm rewind of up to 100 blocks against the 2-block limit. The real check is validator agreement (AGREE in summarize).
# Hard checks (CHECK FAIL stops the campaign): node exe md5, env (base knobs empty, trade history and WAL unset), book CF,
#   codec and WAL option, trie default, max_total_wal_size 2 GiB, zero trade rows, TORUS_P3_PROTO per arm, the P3 layer's
#   enable line per arm (positive control), genesis state hash per arm, restarted val1 md5 and knobs.
# Usage: ozarchy-p3p-campaign.sh N   (MAX_IN_FLIGHT, e.g. 4).  DRY_RUN=1 runs the preflight only.
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N   (MAX_IN_FLIGHT, e.g. 4)"; exit 2; }
unset TORUS_MODEL_PROFILE CRASH_KILL_AT_S TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_TRADE_HISTORY TORUS_ROCKSDB_MAX_TOTAL_WAL_MB TORUS_P3_PROTO TORUS_P3_PROTO_INTERVAL STATE_HASH_ACTIVATION KILL_SIG KILL_WAIT
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
DEVNET=/home/oz/torus-wsl-devnet
RUN=$R/ozarchy-p3p-run
PROTO_WT=/home/oz/projects/wt/p3proto
PROTO_BUILD_TD=/home/oz/.cargo-target-p3proto/release
PROTO_SHA=682a8df53e9f5736904f3ed60c4ace6687be84aa
PROTO_NODE_MD5=6e40fa3c30c2f2602e792183c591177b
PROTO_BENCH_MD5=19f7ac94452b5fb725a467e28feab983
SHA_HARNESS=9b7e29b2e2033babbe03078421e43a6c066cd53d
WT_H=/home/oz/projects/wt/p3s0r-9b7e29b2
HARNESS=$WT_H/tools/matched-bench/run-cell.sh
DETACH=$WT_H/tools/matched-bench/campaign/detach.sh
TOOLS=$R/ozarchy-p3p-tools/matched-bench
T=$R/ozarchy-14236fa-tools
T3=$R/ozarchy-p3s0r-tools
PYCHK=$R/ozarchy-acc-codec-check.py
LSM=$R/ozarchy-acc3-tools/ozarchy-acc3-lsm.py
RSTPY=$R/ozarchy-acc3-tools/ozarchy-acc3-restart.py
TD=$R/ozarchy-p3p-stage/n
BENCH=$TD/release/bench-throughput
MD5_BUILD=$R/ozarchy-p3p-build/md5s.txt
MK=300
P=ozarchy-p3p-300m
NM=$(md5sum < "$TD/release/torus-node" 2>/dev/null | cut -c1-8)
BENCH_MD5=$(md5sum < "$BENCH" 2>/dev/null | cut -d' ' -f1)
KNOBS="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_BOOK_ROWS="
declare -A ARMX ARMSH MODE KNOBX WALCAP
# Arm env: the base knobs empty (as in p3s), NO trade-history / WAL env (the node defaults are the tw baseline).
ARMX[c]="$KNOBS";                    ARMX[cS]="$KNOBS"
ARMX[a]="$KNOBS TORUS_P3_PROTO=all"; ARMX[aS]="$KNOBS TORUS_P3_PROTO=all"
ARMX[k]="$KNOBS TORUS_P3_PROTO=books"
ARMSH[cS]=1; ARMSH[aS]=1
MODE[all]=All; MODE[books]=Books
# The shared checks run with arm 'tw' = node defaults: no trade-history / WAL knob may be set; WAL cap = 2 GiB default.
KNOBX[tw]=""; WALCAP[tw]=2147483648
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
# tag:arm:dur:kind:killat      kind = none | graceful | crash
CELLS=(
  "w0:c:60:none:"
  "r1-c:c:120:none:"    "r1-a:a:120:none:"    "r1-k:k:120:none:"    "r1-cS:cS:120:none:"    "r1-aS:aS:120:none:"
  "r2-a:a:120:none:"    "r2-k:k:120:none:"    "r2-cS:cS:120:none:"  "r2-aS:aS:120:none:"    "r2-c:c:120:none:"
  "r3-k:k:120:none:"    "r3-cS:cS:120:none:"  "r3-aS:aS:120:none:" "r3-c:c:120:none:"      "r3-a:a:120:none:"
  "r4-cS:cS:120:none:"  "r4-aS:aS:120:none:"  "r4-c:c:120:none:"  "r4-a:a:120:none:"      "r4-k:k:120:none:"
  "g1-c:c:120:graceful:60" "g1-a:a:120:graceful:60" "g1-k:k:120:graceful:60"
  "g2-k:k:120:graceful:60" "g2-a:a:120:graceful:60" "g2-c:c:120:graceful:60"
  "x1-c:c:420:crash:60,180,300" "x1-a:a:420:crash:60,180,300"
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

walsamp() { # o: restart cells only. val1's WAL files (name, bytes) every 1 s until the cell ends: the WAL state just before the kill
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

p3knob() { # label arm file lines: TORUS_P3_PROTO in each node line of file is the arm's mode (c, cS: unset; a, aS: all; k: books),
    # on 3 lines (full cell) or 1 (restarted val1); TORUS_P3_PROTO_INTERVAL is never set
    local label=$1 arm=$2 f=$3 want=$4 m n
    case $arm in a|aS) m=all ;; k) m=books ;; c|cS) m=none ;; *) echo "CHECK FAIL $label: no P3 mode for arm $arm"; return 1 ;; esac
    if [ "$m" = none ]; then
        n=$(grep -c ' TORUS_P3_PROTO' "$f")
        [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_P3_PROTO set on $n node lines (arm $arm: none; $(basename "$f"))"; return 1; }
    else
        n=$(grep -c " TORUS_P3_PROTO=$m " "$f")
        [ "$n" = "$want" ] || { echo "CHECK FAIL $label: TORUS_P3_PROTO=$m on $n of $want node lines ($(basename "$f"))"; return 1; }
    fi
    n=$(grep -c ' TORUS_P3_PROTO_INTERVAL' "$f")
    [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_P3_PROTO_INTERVAL is set ($(basename "$f"))"; return 1; }
    echo "CHECK OK $label p3 knob ($(basename "$f"), arm $arm: $m)"
}

p3log() { # label arm: the P3 layer's enable line on each validator log (positive control for the arm; none on the control arms)
    local label=$1 arm=$2 o=$R/$1 m v n tot=""
    case $arm in a|aS) m=all ;; k) m=books ;; *) m=none ;; esac
    for v in 0 1 2; do
        if [ "$m" = none ]; then
            n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -c 'coalesced checkpoint layer enabled')
            [ "$n" = 0 ] || { echo "CHECK FAIL $label: val$v shows the P3 layer ($n enable lines) on arm $arm"; return 1; }
        else
            n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -c "coalesced checkpoint layer enabled mode=${MODE[$m]}")
            [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: val$v has no 'mode=${MODE[$m]}' layer enable line"; return 1; }
        fi
        tot="$tot val$v=$n"
    done
    echo "CHECK OK $label p3 layer (arm $arm:$tot enable lines)"
}

statechk() { # label arm: the cell's genesis state hash (run.log has the generator's line)
    local label=$1 arm=$2 want
    case $arm in cS|aS) want='1' ;; *) want='"off"' ;; esac
    grep -q -x "  state_hash_activation = $want" "$R/$label/run.log" || { echo "CHECK FAIL $label: genesis state_hash_activation is not $want (arm $arm)"; return 1; }
    echo "CHECK OK $label genesis state_hash_activation = $want (arm $arm)"
}


cell() { # tag arm dur kind killat. return 0 = go on, 1 = abort
    local tg=$1 arm=$2 dur=$3 kind=$4 killat=${5:-} label rcf unit sig=none
    local td=$TD nm=$NM bench=$BENCH bmd5=$BENCH_MD5 mif=$N bud=$BUDGET xe=${ARMX[$arm]:-}
    local EP= CP= TP= FP= IP= RP= WSP= WP= mp= HP= t=0 rc fdok=0 nt=0 f
    local -a E
    label=$P-$tg; rcf=$R/$label.cell.rc; unit=bench-$label
    CRASHCELL=0; [ "$kind" = none ] || CRASHCELL=1
    case $kind in graceful) sig=TERM ;; crash) sig=KILL ;; esac
    [ -n "${ARMX[$arm]:-}" ] || { echo "ABORT: unknown arm $arm"; return 1; }
    [ -z "$(pgrep -x torus-node)" ] || { echo "[$(date +%T)] ABORT before $label: torus-node still running"; return 1; }
    quiet || return 1
    hostlog "$label"
    [ ! -e "$rcf" ] || { echo "[$(date +%T)] ABORT: $rcf exists"; return 1; }
    echo "[$(date +%T)] START $label arm=$arm dur=$dur kind=$kind crash_kill_at=${killat:-none} sig=$sig state_hash=${ARMSH[$arm]:-0} max_in_flight=$N budget=$BUDGET extra_env=${xe:-none} node_md5=$nm bench_md5=${bmd5:0:8} load=$(cut -d' ' -f1 /proc/loadavg)"
    mkdir -p "$R/$label"
    envchk "$R/$label" & EP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & CP=$!
    "$T3/io-sampler.sh" "$R/$label" 2>/dev/null & IP=$!
    python3 "$T/task-sampler.py" "$R/$label" & TP=$!
    fdsampler "$R/$label" & FP=$!
    if [ "$CRASHCELL" = 1 ]; then restenv "$R/$label" & RP=$!; walsamp "$R/$label" & WSP=$!; fi
    E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
       ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
    if [ "${ARMSH[$arm]:-0}" = 1 ]; then E+=(STATE_HASH_ACTIVATION=1); fi
    case $kind in
        crash)    E+=(CRASH_KILL_AT_S="$killat") ;;
        graceful) E+=(CRASH_KILL_AT_S="$killat" KILL_SIG=TERM KILL_WAIT=60) ;;
    esac
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MK" "$dur" 76000 "$xe" || { stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP"; return 1; }
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { echo "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; stopbg "$EP" "$CP" "$TP" "$FP" "$IP" "$RP" "$WSP"; return 1; }
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
        p3knob "$label" "$arm" "$R/$label/node-env-all.txt" 3 || return 1
        p3log "$label" "$arm" || return 1
        statechk "$label" "$arm" || return 1
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
        p3knob "$label" "$arm" "$R/$label/node-restart-env.txt" 1 || return 1
    fi
    python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); h=s["headline"]; print("[rates] matched/s", h["matched_s_avg"], "placed/s", h["placed_s_avg"], "submit/s", s["ingest"].get("bench_submit_rate"), "native blk/s", h["native_blk_s"])' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    python3 -c 'import json,sys; o=json.load(open(sys.argv[1])).get("oracle_feed") or {}; print("[oracle] stale", o.get("stale_marks_at_bench_end"), "fresh", (o.get("marks_at_bench_end") or {}).get("fresh"), "acc", o.get("accepted"), "/", o.get("sent"))' "$R/$label/summary.json" 2>&1 | sed "s/^/[$label] /"
    [ "$rc" = 0 ] || echo "CELL FAIL $label: run-cell rc=$rc -> recorded, continuing"
    if grep -q 'WARNING: in-flight block tail' "$R/$label/bench.log" 2>/dev/null; then
        echo "[$(date +%T)] STOP: tail WARNING in capped cell $label"; return 1
    fi
    if [ "$fdok" = 1 ]; then echo "[$(date +%T)] STOP: a node held > $FD_STOP open fds in $label (set LimitNOFILE for both arms first)"; return 1; fi
    return 0
}

# ---- preflight ----
PF() { echo "PREFLIGHT FAIL: $*"; exit 1; }
[ -x "$TD/release/torus-node" ] || PF "staged node missing ($TD/release/torus-node)"
[ "$(md5sum < "$TD/release/torus-node" | cut -c1-32)" = "$PROTO_NODE_MD5" ] || PF "staged torus-node md5 is not $PROTO_NODE_MD5"
[ "$(md5sum < "$PROTO_BUILD_TD/torus-node" | cut -c1-32)" = "$PROTO_NODE_MD5" ] || PF "build torus-node md5 is not $PROTO_NODE_MD5"
[ "$(md5sum < "$BENCH" | cut -c1-32)" = "$PROTO_BENCH_MD5" ] || PF "staged bench-throughput md5 is not $PROTO_BENCH_MD5"
grep -q "^n $PROTO_SHA $PROTO_NODE_MD5" "$MD5_BUILD" || PF "node $PROTO_NODE_MD5 not in $MD5_BUILD"
grep -q "^bench $PROTO_SHA $PROTO_BENCH_MD5" "$MD5_BUILD" || PF "bench $PROTO_BENCH_MD5 not in $MD5_BUILD"
{ [ "$(git -C "$PROTO_WT" rev-parse HEAD)" = "$PROTO_SHA" ] && [ -z "$(git -C "$PROTO_WT" status --porcelain)" ]; } || PF "$PROTO_WT not clean at $PROTO_SHA"
{ [ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ]; } || PF "$WT_H not clean at $SHA_HARNESS"
[ "$(diff -rq -x __pycache__ "$WT_H/tools/matched-bench" "$TOOLS" | wc -l)" = 1 ] || PF "harness tools copy differs from $WT_H beyond crash-kill.sh"
grep -q 'KILL_SIG' "$TOOLS/crash-kill.sh" || PF "crash-kill.sh copy has no KILL_SIG"
[ "$(stat -c %d "$DEVNET")" = "$(stat -c %d "$R")" ] || PF "$DEVNET and $R on different filesystems (hard links)"
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
echo "[$(date +%T)] PREFLIGHT OK harness=${SHA_HARNESS:0:8} node=$NM ($PROTO_SHA) control=same binary bench=${BENCH_MD5:0:8} N=$N budget=$BUDGET nofile_soft=$(ulimit -Sn) cells=${#CELLS[@]}: $TAGS"
[ "${DRY_RUN:-0}" = 1 ] && exit 0
for c in "${CELLS[@]}"; do
    IFS=: read -r tg arm dur kind killat <<< "$c"
    cell "$tg" "$arm" "$dur" "$kind" "$killat" || { echo "[$(date +%T)] CAMPAIGN STOP at $tg"; exit 1; }
done
echo "[$(date +%T)] CAMPAIGN END"
