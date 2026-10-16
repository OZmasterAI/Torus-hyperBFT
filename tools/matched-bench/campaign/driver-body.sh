# ---- driver body (tools/matched-bench/campaign/driver-body.sh, copied in by make-driver.py; do not edit the copy) ----
# Everything below reads only the GENERATED CONFIG above. Modes:
#   DRIVER launch [smoke]   preflight, then start "DRIVER run [smoke]" as unit bench-$CAMPAIGN[-smoke] through
#                           detach.sh (the unit holds the build lock $HOME/projects/wt/.cargo-heavy.lock for its
#                           whole run); log $RUN[/smoke]/campaign.log; done marker $RUN[/smoke]/campaign.done
#   DRIVER dry [smoke]      preflight only (paths, staged md5s, tools), prints the cell list; starts nothing
#   DRIVER show [smoke]     the plan as tables (arms, node env, harness env, env checks, watchers, cells)
#   DRIVER run [smoke]      the campaign itself; refuses to run outside a bench-*.service unit
# smoke = the one SMOKE_CELL under $P-smoke in $RUN/smoke, with every check: run it (and read its checks) before
# launching the campaign. Library mode for tests: DRIVER_LIB_ONLY=1 source DRIVER (functions + config, no main).
# Runtime safety, from the hand-written Job 4 driver (testdata/make-driver/ozarchy-p4j4-{h,d}-campaign.sh; the
# golden test compares the copied functions verbatim and the cell() wiring). Every cell: watch_nodes (a node, the
# load generator or the oracle feed that dies mid-cell -> <cell>/node-death.txt and the harness is TERMed; the cell
# is a CELL FAIL, cell_step flags its row death and queues a rerun, the campaign goes on), memguard (MemAvailable
# < 6 GB stops the cell unit), hardscan (HARD FINDING lines: panic / fail-stop / state hash mismatch at WARN/ERROR).
# SAMPLERS=1 (spec "samplers", default on): du, memory, metrics, fd (a node above FD_STOP open fds stops the
# campaign after its cell), the 1 Hz soak sampler (SOAKPY: metrics-1hz.txt.gz + host-1hz.tsv), vmstat 1 Hz, the
# RocksDB LOG copies. HOST_CHECKS=1 (spec "host_checks", default on):
# run preflight MemAvailable >= MIN_MEM_AVAIL_KB; the writeback sysctls recorded at each cell's start and end
# (sysctl.txt) and held to SYSCTL_WANT (the spec's values; the dirty limits it does not give are pinned at the run's
# preflight); heavy analysis processes (perf, flamegraph, jeprof, ...) keep quiet() waiting.
# Warm-up gate (spec warmup.gate, default on): a GATE_TAGS cell (the warm-up, the smoke cell) must end run-cell rc 0,
# validity ACCEPT, agreement AGREE, liveness PASS, 0 panic/fail-stop lines (warmgatechk), else CHECK FAIL and the
# campaign stops before any measured cell. Campaign checks (spec checks): CHECKS_SOURCE is sourced at preflight
# (load_checks), each CHECK_FNS function runs after every cell as FN LABEL ARM; non-zero = CHECK FAIL (bad:check, stop).
# Check scoping (Codex P2: bash variables are dynamic, so a check saw every calling function's locals, rc=0 for its
# own rc=1): a check sees its file's top-level variables as sourced at preflight, the driver globals (DRIVER_VARS),
# the environment and the shell's own variables, and no driver function's local, in both directions: the file is
# sourced from frames without locals and its variables kept (load_checks); each check runs through _run_check,
# which unsets every other visible name and re-declares the file's. The prefix _DRV_ is the driver's (preflight
# refuses a checks file that uses it). Limits: load_checks' header.
# Test-only overrides: DRIVER_PROC (a fake /proc: node exe/environ, meminfo), DRIVER_DETACH (a fake detach.sh).
set -u
SELF=$(readlink -f "${BASH_SOURCE[0]}")
PROC=${DRIVER_PROC:-/proc}
HARNESS=$TOOLS/run-cell.sh
DETACH=${DRIVER_DETACH:-$TOOLS/campaign/detach.sh}
SOAKPY=$TOOLS/soak-sampler.py   # samplers: the hand drivers' 1 Hz sidecar (tools/matched-bench/soak-sampler.py)
UNIT_NAME=$CAMPAIGN
MIN_FREE_MB=${MIN_FREE_MB:-20000}
QUIET_WAIT_S=${QUIET_WAIT_S:-3600}
SMOKE=          # 1 = the smoke run (apply_smoke); the driver's own, never the caller's
GENESIS_ROOT=   # run-cell.sh's MAINREPO = $TOOLS/../.. (preflight_genesis)
DD=${DEVNET:-}          # the devnet data root (data/val<i>, run/pids, run/val<i>.log), the Job 4 helpers' name for it
METS=(9161 9162 9163)   # the devnet nodes' metrics ports (run-cell.sh METS): memsamp, ckptsamp and SOAKPY scrape them
FD_STOP=16384           # samplers: a node that held more open fds in a cell stops the campaign after it (stop-fd)
CRASHCELL=0             # restart cells are not a generator shape (CRASH_KILL_AT_S is a SPEC ERROR): watch_nodes' restart branch never runs
# host checks: the sysctls each cell's sysctl.txt line records (the Job 4 set); the dirty limits are held to the
# spec's values, else pinned at the run's preflight (sysctl_pin)
SYSCTL_REC="vm.dirty_bytes vm.dirty_background_bytes vm.dirty_ratio vm.dirty_background_ratio vm.dirty_expire_centisecs
 vm.dirty_writeback_centisecs kernel.perf_event_paranoid"
SYSCTL_PIN="vm.dirty_bytes vm.dirty_background_bytes"
# Every name the driver keeps state in (generated config + this body). scrub_env un-exports these and never
# unsets them: a caller that exported RUN, ARMS, ... must not delete (or, exported, leak) driver config.
# test_make_driver.py checks that each name the config or the body assigns at top level is listed.
DRIVER_VARS="CAMPAIGN R RUN P WT_H TOOLS DEVNET MARKETS RATE MAX_LOAD NODES BASELINE BDIR BMD5 BBMD5 BWRAP
 ABIN AENV AXE AHENV ARMS ENV_NAMES PROC_REC_RE EXP_ARM EXP_FILE EXP_RE EXP_MIN EXP_MAX CELLS SMOKE_CELL
 ANALYSIS_CMD ANALYSIS_METRIC SAMPLERS HOST_CHECKS MIN_MEM_AVAIL_KB SYSCTL_WANT SELF PROC HARNESS DETACH SOAKPY
 UNIT_NAME MIN_FREE_MB QUIET_WAIT_S SMOKE GENESIS_ROOT DD METS FD_STOP CRASHCELL SYSCTL_REC SYSCTL_PIN GENESIS_INPUTS MODE SM
 TAGS STARTED_LABEL CELL_FAIL GATE_TAGS CHECKS_SOURCE CHECK_FNS DRIVER_VARS"
# run-cell.sh generates each cell's genesis from ITS OWN repo root ($TOOLS/../..), not from the harness worktree
# it is handed (which only launches the nodes): a frozen tools copy must carry these, equal to the harness's
GENESIS_INPUTS="devnet/wsl/gen-3val-genesis.sh devnet/wsl/bench-validator-keys.json devnet/genesis.json
 testnet/lib/cargo-bin.sh testnet/gen-weighted-genesis.sh"

log() { echo "[$(date +%T)] $*"; }

apply_smoke() { # the smoke run: SMOKE_CELL only, own prefix and run dir, no rerun pass
    RUN=$RUN/smoke; P=$P-smoke; UNIT_NAME=$CAMPAIGN-smoke; CELLS=("$SMOKE_CELL"); SMOKE=1
}

env_file() { # NAME -> the per-cell record that shows it: TORUS_* in node-env-all.txt, any other env in node-preload.txt
    case $1 in TORUS_*) echo node-env-all.txt ;; *) echo node-preload.txt ;; esac
}

absent_names() { # arm: every name in ENV_NAMES the arm's node env (base + arm) does not set
    local arm=$1 kv k set=" "
    for kv in ${AXE[$arm]}; do set+="${kv%%=*} "; done
    for k in $ENV_NAMES; do [[ "$set" == *" $k "* ]] || echo "$k"; done
}

step_summary_md5() { # arm: 0 = the step skips summary.json torus_node_md5 (run-cell.sh hashes the staged file,
    # a wrapper script on wrapper arms); the running-exe md5 records are checked on every arm
    [ "${BWRAP[${ABIN[$1]}]}" = 1 ] && echo 0 || echo 1
}

sysctl_names() { # the sysctls the host checks hold: SYSCTL_PIN and the spec's, sorted
    printf '%s\n' $SYSCTL_PIN "${!SYSCTL_WANT[@]}" | sort -u
}

show_plan() { # [smoke]: the plan as tables (make-driver tests compare these with earlier drivers)
    local a kv k
    [ "${1:-}" = smoke ] && apply_smoke
    echo "PATH RUN $RUN"; echo "PATH P $P"; echo "PATH MARKETS $MARKETS"; echo "PATH RATE $RATE"
    echo "PATH TOOLS $TOOLS"; echo "PATH WT_H $WT_H"; echo "PATH DEVNET $DEVNET"; echo "PATH BASELINE $BASELINE"
    echo "WATCH node-death memguard hardscan"
    if [ "$SAMPLERS" = 1 ]; then
        echo "SAMPLERS on du mem metrics fd soak-1hz vmstat rocksdb-log fd_stop=$FD_STOP"
        echo "SOAK on $SOAKPY metrics-1hz.txt.gz host-1hz.tsv"
    else
        echo "SAMPLERS off"
    fi
    if [ -n "$GATE_TAGS" ]; then echo "WARMGATE on $GATE_TAGS"; else echo "WARMGATE off"; fi
    if [ -n "$CHECKS_SOURCE" ]; then echo "CHECKS $CHECKS_SOURCE $CHECK_FNS"; else echo "CHECKS none"; fi
    if [ "$HOST_CHECKS" = 1 ]; then
        echo "HOSTCHECK on min_mem_avail_kb=$MIN_MEM_AVAIL_KB"
        for k in $(sysctl_names); do echo "SYSCTL $k ${SYSCTL_WANT[$k]:-pinned}"; done
    else
        echo "HOSTCHECK off"
    fi
    for a in "${ARMS[@]}"; do
        echo "ARM $a ${ABIN[$a]} ${BMD5[${ABIN[$a]}]} wrapper=${BWRAP[${ABIN[$a]}]} xe=${AXE[$a]}"
        [ -n "${AHENV[$a]}" ] || echo "HENV $a"
        for kv in ${AHENV[$a]}; do echo "HENV $a $kv"; done
        for kv in ${AXE[$a]}; do echo "PRESENT $a $kv $(env_file "${kv%%=*}")"; done
        for k in $(absent_names "$a"); do echo "ABSENT $a $k $(env_file "$k")"; done
    done
    for c in "${CELLS[@]}"; do IFS=: read -r t a d k _ <<< "$c"; echo "CELL $t $a $d $k"; done
}

# ---- host state ----
buildhits() { local n p; for n in rust-analyzer cargo rustc; do for p in $(pgrep -x "$n"); do printf '%s:%s ' "$n" "$p"; done; done; }
benchhits() { local p; for p in $(pgrep -x torus-node); do printf 'torus-node:%s ' "$p"; done
              for p in $(pgrep -f '^(\S*/)?bench-throughput( |$)'); do printf 'bench:%s ' "$p"; done; }
anahits() { # host checks: heavy analysis processes (perf record/report/script, inferno, flamegraph, jeprof), as Job 4's
    [ "$HOST_CHECKS" = 1 ] || return 0
    local p; for p in $(pgrep -f '^(\S*/)?(perf|inferno-\S+|flamegraph\S*|jeprof|addr2line|llvm-addr2line)( |$)|^perl \S*(stackcollapse|flamegraph)'); do printf '%s:%s ' "$(cat /proc/$p/comm 2>/dev/null)" "$p"; done
}
quiet() { # no build, no node or bench left over, no heavy analysis (host checks), 1-min load < MAX_LOAD; waits up to QUIET_WAIT_S
    local t=0 l1
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(buildhits)$(benchhits)$(anahits)" ] && awk -v l="$l1" -v m="$MAX_LOAD" 'BEGIN{exit !(l<m)}'; then return 0; fi
        [ $((t % 300)) = 0 ] && [ $t -gt 0 ] && log "waiting for a quiet host ${t}s (load $l1; $(buildhits)$(benchhits)$(anahits))"
        t=$((t+10)); [ $t -ge "$QUIET_WAIT_S" ] && { log "host not quiet after ${QUIET_WAIT_S}s (load $l1; $(buildhits)$(benchhits)$(anahits))"; return 1; }
        sleep 10
    done
}

# ---- per-cell records ----
node_pids() { cat "$DEVNET/run/pids" 2>/dev/null; }

envrec_now() { # o: per node pid (DEVNET/run/pids): exe md5 = md5 of PROC/PID/exe, the RUNNING exe (a wrapper script
    # has exec'd the real node by then; never hash the staged file), the TORUS_* environ, the other recorded environ
    local o=$1 p
    for p in $(node_pids); do
        echo "pid=$p exe_md5=$(md5sum < "$PROC/$p/exe" 2>/dev/null | cut -c1-32)"
    done > "$o/node-environ-trie.txt"
    for p in $(node_pids); do
        echo "pid=$p $(tr '\0' '\n' < "$PROC/$p/environ" 2>/dev/null | grep -E '^TORUS_' | sort | tr '\n' ' ')"
    done > "$o/node-env-all.txt"
    for p in $(node_pids); do
        echo "pid=$p $(tr '\0' '\n' < "$PROC/$p/environ" 2>/dev/null | grep -E "^($PROC_REC_RE)=" | sort | tr '\n' ' ')"
    done > "$o/node-preload.txt"
}

envrec() { # o: wait until the bench runs (run.log "] bench: "), then record (envrec_now)
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    envrec_now "$o"
}

# ---- runtime watchers (every cell), verbatim from the Job 4 driver: memguard reads $PROC/meminfo (a test stubs it) ----
# watch_nodes O HARNESS_PID: the node pids of run.log's "node pids:" line, the load generator and the oracle feed,
# every 0.5 s until the harness stop ("stopping pid"); a death writes node-death.txt + node-life.txt and TERMs the
# harness (run-cell.sh's trap stops the rest). memguard UNIT: MemAvailable < 6 GB stops the cell unit.
stopbg() { local p; for p in "$@"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; return 0; }
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
memguard() { # unit: stop the cell unit if host MemAvailable < 6 GB (RAM headroom for long cells)
    local unit=$1 a
    while systemctl --user is-active -q "$unit.service" 2>/dev/null; do
        a=$(awk '/^MemAvailable:/{print $2}' "$PROC/meminfo")
        if [ "$a" -lt 6000000 ]; then echo "[$(date +%T)] MEMGUARD: MemAvailable ${a} kB < 6 GB -> stopping $unit"; systemctl --user stop "$unit.service"; return 0; fi
        sleep 5
    done
}
hardscan() { # label: correctness evidence per validator log (WARN/ERROR level only) -> HARD FINDING lines. Recorded, never aborts.
    local label=$1 o=$R/$1 v n pat='STATE HASH MISMATCH|FATAL|panicked|could not be fully replayed|FAIL-STOP|fail-stop|conflicting blocks|replay stopped'
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -E "$pat" | grep -cvE ' (INFO|DEBUG|TRACE) ')
        [ "$n" = 0 ] || echo "HARD FINDING $label: val$v has $n WARN/ERROR correctness lines: $(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -E "$pat" | grep -vE ' (INFO|DEBUG|TRACE) ' | head -2 | cut -c1-300 | tr '\n' '|')"
    done
    echo "CHECK OK $label hardscan done"
}
# ---- samplers (SAMPLERS=1), verbatim from the Job 4 driver: data-du-samples.txt, ckpt-samples.txt, mem-samples.txt
# (+ smaps/), node-nofile.txt + fds.txt (fdsum: the FD_STOP stop), after the cell rocksdb-LOG-val<i>.txt + data-du.txt ----
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
                echo "G $(date +%s.%N) val$i $(grep -E '^torus_(rocksdb_block_cache_bytes|rocksdb_memtable_bytes_all|rocksdb_immutable_memtables_all|mempool_native_size|mempool_evm_size|exec_queue_depth|exec_dispatch_deferred|exec_throttle_tier|mempool_native_committed_unexecuted|flush_worker_depth|block_height|blocks_committed_total|exec_native_blocks_total|exec_checkpoint_layer_bytes_(sum|count)|exec_checkpoints_total|exec_resident_rows_bytes|exec_resident_users_bytes|member_cache_resident_buckets|trade_writer_queued_batches|post_flush_writer_queued_batches|exec_resting_orders|exec_cancel_all_index_entries|exec_level_hash_cache_entries|exec_action_status_bytes_sum|exec_thread_spawns_[a-z_]+|exec_checkpoint_live_[a-z_]+|exec_checkpoint_read_views_live|exec_checkpoint_oldest_live_age_blocks|jemalloc_[a-z]+_bytes) ' <<< "$m" | tr ' ' '=' | tr '\n' ' ')"
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
soakchk() { # label dur: sampler coverage, recorded, never fails (after the Job 4 driver's soakchk): the soak sidecar's
    # metrics-1hz ok scrapes (3 nodes x dur expected) and host-1hz rows (dur expected) below that are a CHECK WARN
    local o=$R/$1 v m s f d k n h
    v=$(grep -c -E ':[0-9]{2}$' "$o/vmstat-1hz.txt" 2>/dev/null); m=$(grep -c '^M ' "$o/mem-samples.txt" 2>/dev/null)
    s=$(ls "$o"/smaps 2>/dev/null | wc -l); f=$(cat "$o/fds.txt" 2>/dev/null | wc -l)
    d=$(cat "$o/data-du-samples.txt" 2>/dev/null | wc -l); k=$(grep -c '^T .* ok$' "$o/ckpt-samples.txt" 2>/dev/null)
    n=$(zcat "$o/metrics-1hz.txt.gz" 2>/dev/null | grep -c '^T .* ok '); h=$(($(cat "$o/host-1hz.tsv" 2>/dev/null | wc -l) - 1))
    [ "$h" -ge 0 ] || h=0
    echo "CHECK SOAK $1: vmstat rows ${v:-0} (cell $2 s), mem samples ${m:-0}, smaps snapshots $s, fd rows $f, du rows $d, metrics ok scrapes ${k:-0}, metrics-1hz ok scrapes ${n:-0}, host-1hz rows $h"
    [ "${n:-0}" -ge $(($2 * 3)) ] || echo "CHECK WARN $1: metrics-1hz has ${n:-0} ok scrapes (< 3 x $2)"
    [ "$h" -ge "$2" ] || echo "CHECK WARN $1: host-1hz has $h rows (< $2)"
}

# ---- host checks (HOST_CHECKS=1) ----
sysctl_now() { sysctl -n "$1" 2>/dev/null || cat "/proc/sys/${1//.//}" 2>/dev/null; }

sysctlchk() { # label start|end: record the writeback / perf sysctls (sysctl.txt, the Job 4 driver's line) and hold
    # every SYSCTL_WANT value: a CHECK FAIL at the start aborts the campaign (abort-sysctl), at the end it fails the
    # cell's checks
    local o=$R/$1 k v tm line bad= got=
    tm=$(stat -c %a /sys/kernel/tracing 2>/dev/null)
    line="$2 $(date +%s.%N)"
    for k in $SYSCTL_REC; do line+=" ${k##*.}=$(sysctl_now "$k")"; done
    line+=" tracefs_mode=$tm"
    for k in "${!SYSCTL_WANT[@]}"; do [[ " ${SYSCTL_REC//$'\n'/ } " == *" $k "* ]] || line+=" $k=$(sysctl_now "$k")"; done
    echo "$line" >> "$o/sysctl.txt"
    for k in $(printf '%s\n' "${!SYSCTL_WANT[@]}" | sort); do
        v=$(sysctl_now "$k"); got+=" $k=$v"
        [ "$v" = "${SYSCTL_WANT[$k]}" ] || bad+=" $k=${v:-unreadable} (want ${SYSCTL_WANT[$k]})"
    done
    [ -z "$bad" ] || { echo "CHECK FAIL $1 ($2): sysctl$bad"; return 1; }
    echo "CHECK OK $1 sysctl ($2):$got paranoid=$(sysctl_now kernel.perf_event_paranoid) tracefs=$tm"
}

sysctl_pin() { # run preflight: every spec sysctl must have its value on the host now (Job 4 required its phase's
    # dirty limits); the dirty limits the spec does not give are pinned to the host's values now. sysctlchk holds them.
    local k v bad=
    for k in $SYSCTL_PIN; do
        [ -n "${SYSCTL_WANT[$k]+x}" ] && continue
        v=$(sysctl_now "$k")
        [ -n "$v" ] || PF "cannot read sysctl $k (host checks pin it; \"host_checks\": false runs without them)"
        SYSCTL_WANT[$k]=$v
    done
    for k in $(printf '%s\n' "${!SYSCTL_WANT[@]}" | sort); do
        v=$(sysctl_now "$k"); [ "$v" = "${SYSCTL_WANT[$k]}" ] || bad+=" $k=${v:-unreadable} (want ${SYSCTL_WANT[$k]})"
    done
    [ -z "$bad" ] || PF "host sysctl differs from the spec (host_checks.sysctl):$bad; set them as root before the launch"
}

preflight_mem() { # run preflight: MemAvailable >= MIN_MEM_AVAIL_KB (Job 4: 40 GB)
    local a
    a=$(awk '/^MemAvailable:/{print $2}' "$PROC/meminfo" 2>/dev/null)
    [ "${a:-0}" -ge "$MIN_MEM_AVAIL_KB" ] || PF "MemAvailable ${a:-unreadable} kB < $MIN_MEM_AVAIL_KB kB (spec host_checks.min_mem_avail_gb)"
}

# ---- per-cell checks (each prints CHECK OK / CHECK FAIL lines; rc 1 = fail) ----
md5chk() { # o arm: every node runs the arm's node (exe md5 of the running process = the arm's staged md5)
    local o=$1 arm=$2 want=${BMD5[${ABIN[$2]}]} f=$1/node-environ-trie.txt n m
    # a record is the full md5 (this driver) or its first 8+ hex digits (older drivers' records)
    n=$(grep -c '^pid=' "$f" 2>/dev/null)
    m=$(awk -v w="$want" '{for (i = 1; i <= NF; i++) if ($i ~ /^exe_md5=[0-9a-f]{8,32}$/) {v = substr($i, 9); if (v == substr(w, 1, length(v))) c++}} END {print c + 0}' "$f" 2>/dev/null)
    [ "${n:-0}" = "$NODES" ] && [ "${m:-0}" = "$NODES" ] \
        || { echo "CHECK FAIL $(basename "$o"): node exe md5 ${want:0:8} on ${m:-0} of ${n:-0} node lines (want $NODES; $f)"; return 1; }
    echo "CHECK OK $(basename "$o") node exe md5 ${want:0:8} on $NODES nodes (arm $arm)"
}

envchk() { # o arm: the arm's node env (base + arm) on every node, every other name of ENV_NAMES on none.
    # TORUS_* from node-env-all.txt, other env (MALLOC_CONF, ...) from node-preload.txt (env_file)
    local o=$1 arm=$2 kv k f n rc=0 lb
    lb=$(basename "$o")
    for f in node-env-all.txt node-preload.txt; do
        n=$(grep -c '^pid=' "$o/$f" 2>/dev/null)
        [ "${n:-0}" = "$NODES" ] || { echo "CHECK FAIL $lb: $f has ${n:-0} node lines (want $NODES)"; rc=1; }
    done
    [ $rc = 0 ] || return 1
    for kv in ${AXE[$arm]}; do
        f=$(env_file "${kv%%=*}"); n=$(grep -F -c " $kv " "$o/$f")
        [ "$n" = "$NODES" ] || { echo "CHECK FAIL $lb: $kv on $n of $NODES node lines in $f (arm $arm)"; rc=1; }
    done
    for k in $(absent_names "$arm"); do
        f=$(env_file "$k"); n=$(grep -c " $k=" "$o/$f")
        [ "$n" = 0 ] || { echo "CHECK FAIL $lb: $k set on $n node lines in $f (arm $arm does not set it)"; rc=1; }
    done
    [ $rc = 0 ] && echo "CHECK OK $lb node env on $NODES nodes (arm $arm: ${AENV[$arm]:-no arm env}; unset: $(absent_names "$arm" | tr '\n' ' '))"
    return $rc
}

marketchk() { # o: the cell ran MARKETS markets (summary.json cell.markets)
    local o=$1 m
    m=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["cell"]["markets"])' "$o/summary.json" 2>/dev/null)
    [ "$m" = "$MARKETS" ] || { echo "CHECK FAIL $(basename "$o"): summary cell.markets=${m:-none} (want $MARKETS)"; return 1; }
    echo "CHECK OK $(basename "$o") markets=$m"
}

expectchk() { # o arm: the spec's expectations for every arm (EXP_ARM '*') and for this arm: grep -E -c in a cell file
    local o=$1 arm=$2 i n rc=0
    for i in "${!EXP_ARM[@]}"; do
        [ "${EXP_ARM[$i]}" = '*' ] || [ "${EXP_ARM[$i]}" = "$arm" ] || continue
        # a missing or unreadable file proves nothing, also for an absence (max 0) expectation
        [ -f "$o/${EXP_FILE[$i]}" ] && [ -r "$o/${EXP_FILE[$i]}" ] \
            || { echo "CHECK FAIL $(basename "$o"): ${EXP_FILE[$i]} missing or unreadable (expectation '${EXP_RE[$i]}'; arm $arm)"; rc=1; continue; }
        n=$(grep -E -c -- "${EXP_RE[$i]}" "$o/${EXP_FILE[$i]}" 2>/dev/null)
        [ $? -le 1 ] && [[ "$n" =~ ^[0-9]+$ ]] \
            || { echo "CHECK FAIL $(basename "$o"): grep -E error on '${EXP_RE[$i]}' in ${EXP_FILE[$i]} (arm $arm)"; rc=1; continue; }
        if [ "$n" -lt "${EXP_MIN[$i]}" ] || { [ -n "${EXP_MAX[$i]}" ] && [ "$n" -gt "${EXP_MAX[$i]}" ]; }; then
            echo "CHECK FAIL $(basename "$o"): ${EXP_FILE[$i]} has $n lines matching '${EXP_RE[$i]}' (want ${EXP_MIN[$i]}..${EXP_MAX[$i]:-any}; arm $arm)"; rc=1
        else
            echo "CHECK OK $(basename "$o") ${EXP_FILE[$i]}: $n lines matching '${EXP_RE[$i]}' (arm $arm)"
        fi
    done
    return $rc
}

gated() { # tag: 0 = the cell is a warm-up gate cell (GATE_TAGS; its rerun too)
    local g
    for g in $GATE_TAGS; do [ "$1" = "$g" ] || [ "$1" = "$g-rerun" ] && return 0; done
    return 1
}

warmgatechk() { # label rc: the warm-up gate: run-cell rc 0, summary.json validity ACCEPT, agreement AGREE, liveness
    # PASS, agreement.panic_or_failstop_lines 0. rc 1 = CHECK FAIL (the campaign stops)
    local label=$1 rc=$2 r
    [ "$rc" = 0 ] || { echo "CHECK FAIL $label: warm-up gate: run-cell rc=$rc"; return 1; }
    r=$(python3 -I -c '
import json, sys
try:
    s = json.load(open(sys.argv[1]))
except (OSError, ValueError) as ex:
    print("cannot read %s: %s" % (sys.argv[1], ex)); sys.exit(1)
a, h = s.get("agreement") or {}, s.get("headline") or {}
v = (s.get("validity") or {}).get("verdict")
lv = (s.get("liveness") or {}).get("verdict")
ag = h.get("agreement_verdict")
pf = a.get("panic_or_failstop_lines")
print("validity=%s agreement=%s liveness=%s panic_or_failstop_lines=%s consensus_timeouts=%s matched/s=%s" % (v, ag, lv, pf, h.get("consensus_timeouts"), h.get("matched_s_avg")))
sys.exit(0 if (v == "ACCEPT" and ag == "AGREE" and lv == "PASS" and pf == 0) else 1)
' "$R/$label/summary.json" 2>&1) || { echo "CHECK FAIL $label: warm-up gate: $r"; return 1; }
    echo "CHECK OK $label warm-up gate: $r"
}

_run_check() { # FN LABEL ARM: one campaign check, in campaignchecks' subshell. Bash variables are dynamically
    # scoped (a subshell too): FN would see the locals of every function on the stack (campaignchecks, cell,
    # run_spec, run_cells or cell_step_rerun_pass), e.g. campaignchecks' rc=0 for its file's rc=1 (Codex P2). So:
    # unset every visible variable that is not the shell's own, exported, a driver global (DRIVER_VARS) or one the
    # checks file set (_DRV_KEEP, load_checks), each unset peeling one calling frame's local until none is left;
    # then the file's variables as sourced (_DRV_DECL: declare inside a function = a local of this one, so no
    # caller's local can shadow them; FN's helpers see them too); then FN LABEL ARM (no variable holds FN's name)
    [ -n "${_DRV_KEEP:-}" ] || { echo "CHECK FAIL $2: campaign check $1: the checks are not loaded (load_checks)"; return 2; }
    _DRV_KEEP+="$(compgen -e | tr '\n' ' ') "
    for _DRV_N in $(declare -p | sed -n -E 's/^declare -[^ ]* ([A-Za-z_][A-Za-z0-9_]*)(=.*)?$/\1/p'); do
        case $_DRV_N in BASH*|_DRV_*) continue ;; esac
        case $_DRV_KEEP in *" $_DRV_N "*) continue ;; esac
        _DRV_I=0
        while [ $((_DRV_I += 1)) -le 64 ] && declare -p "$_DRV_N" > /dev/null 2>&1; do
            unset -v "$_DRV_N" 2>/dev/null || break
        done
    done
    eval "$_DRV_DECL"
    unset -v "${!_DRV_@}"
    "$@"
}

campaignchecks() { # label arm: each spec checks.per_cell function (CHECK_FNS) as FN LABEL ARM, in a subshell through
    # _run_check: it sees its checks file's variables as sourced at preflight and the driver's globals (R, AENV,
    # AXE, ...), never a local of a driver function; it cannot change driver state. All run, so one cell shows
    # every failure
    local fn r rc=0
    for fn in $CHECK_FNS; do
        ( _run_check "$fn" "$1" "$2" ); r=$?
        [ "$r" = 0 ] || { echo "CHECK FAIL $1: campaign check $fn returned $r (arm $2)"; rc=1; }
    done
    return $rc
}

cellchecks() { # o arm: every standard check (all run, so one cell shows every failure)
    local rc=0
    md5chk "$1" "$2" || rc=1
    envchk "$1" "$2" || rc=1
    marketchk "$1" || rc=1
    expectchk "$1" "$2" || rc=1
    return $rc
}

# ---- one cell ----
cell() { # tag arm dur kind killat. return 0 = go on, 1 = stop the campaign (a started cell sets CELL_FAIL)
    local tg=$1 arm=$2 dur=$3 b label rcf unit EP= t=0 rc nt=0 kv crc=0 fdok=0
    local WP= GP= DP= KP= MP= FP= SKP= VMP= mp= HP=
    local -a E
    b=${ABIN[$arm]:-}
    [ -n "$b" ] || { log "ABORT: unknown arm $arm"; return 1; }
    label=$P-$tg; rcf=$R/$label.cell.rc; unit=bench-$label
    quiet || return 1
    [ ! -e "$rcf" ] && [ ! -e "$R/$label" ] || { log "ABORT: $R/$label or its .cell.rc exists"; return 1; }
    t=$(buildhits); kv=$(anahits); log "HOST before $label: builds ${t:-none}; analysis ${kv:-none}; load $(cut -d' ' -f1-3 /proc/loadavg)"; t=0
    log "START $label arm=$arm bin=$b node_md5=${BMD5[$b]:0:8} wrapper=${BWRAP[$b]} env=${AENV[$arm]:-none} dur=$dur markets=$MARKETS rate=$RATE mem_avail_kb=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo) swap_used_kb=$(awk '/^SwapTotal:/{t=$2} /^SwapFree:/{f=$2} END{print t-f}' /proc/meminfo)"
    mkdir -p "$R/$label"; STARTED_LABEL=$label
    if [ "$HOST_CHECKS" = 1 ]; then
        sysctlchk "$label" start || { CELL_FAIL=abort-sysctl; return 1; }
    fi
    envrec "$R/$label" & EP=$!
    if [ "$SAMPLERS" = 1 ]; then
        dusamp "$R/$label" & DP=$!
        ckptsamp "$R/$label" & KP=$!
        memsamp "$R/$label" & MP=$!
        fdsampler "$R/$label" & FP=$!
        python3 -I "$SOAKPY" "$R/$label" "${METS[@]}" & SKP=$!
        timeout $((dur + 1200)) vmstat -n -t 1 > "$R/$label/vmstat-1hz.txt" 2>&1 & VMP=$!
    fi
    # OVERWRITE=1 (as Job 4): sysctl.txt and the samplers' files are in the cell dir before run-cell.sh, which refuses
    # a non-empty dir without it; this function refused an existing cell dir above (the spec may only repeat it)
    E=(TARGET_DIR="${BDIR[$b]}" TOOLS_DIR="$TOOLS" DATA_ROOT="$DEVNET" RESULTS_ROOT="$R" OVERWRITE=1)
    for kv in ${AHENV[$arm]}; do E+=("$kv"); done
    [ "${BWRAP[$b]}" = 1 ] || E+=(EXPECT_NODE_MD5="${BMD5[$b]}")
    [ -z "${BBMD5[$b]}" ] || E+=(EXPECT_BENCH_MD5="${BBMD5[$b]}")
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MARKETS" "$dur" "$RATE" "${AXE[$arm]}" \
        || { stopbg "$EP" "$DP" "$KP" "$MP" "$FP" "$SKP" "$VMP"; CELL_FAIL=abort-detach; return 1; }
    # the harness pid: run-cell.sh, the child of the unit's main process (the cellwrap bash); watch_nodes TERMs it
    until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \
          && HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do
        sleep 0.2; t=$((t+1))
        [ $t -gt 150 ] && { log "ABORT: no run-cell.sh pid in $unit"; systemctl --user stop "$unit.service"; stopbg "$EP" "$DP" "$KP" "$MP" "$FP" "$SKP" "$VMP"; CELL_FAIL=abort-no-pid; return 1; }
    done
    log "$label unit=$unit main_pid=$mp harness_pid=$HP ($(cat /proc/$HP/comm 2>/dev/null))"
    watch_nodes "$R/$label" "$HP" & WP=$!
    memguard "$unit" & GP=$!
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    wait "$WP"
    while [ -n "$(pgrep -x torus-node)" ] && [ $nt -lt 60 ]; do sleep 1; nt=$((nt+1)); done
    [ -z "$(pgrep -x torus-node)" ] || log "WARN $label: torus-node still running at LOG copy"
    stopbg "$DP" "$KP" "$MP" "$GP" "$SKP" "$VMP"; wait "$DP" "$KP" "$MP" "$SKP" "$VMP" 2>/dev/null
    if [ "$SAMPLERS" = 1 ]; then
        savelog "$label"
    fi
    # a mid-cell death (watch_nodes): CELL FAIL, no checks; the step flags the row death and queues a rerun
    if [ -e "$R/$label/node-death.txt" ]; then
        stopbg "$EP" "$FP"
        if gated "$tg"; then   # a warm-up gate cell that lost a node: the campaign stops
            echo "CHECK FAIL $label: warm-up gate: node death ($(head -1 "$R/$label/node-death.txt"))"
            sleep 5; [ -z "$(pgrep -x torus-node)" ] || log "WARN: torus-node left running after $label"
            CELL_FAIL=check; return 1
        fi
        log "CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing"
        sleep 5; [ -z "$(pgrep -x torus-node)" ] || { log "ABORT: torus-node left running after $label"; CELL_FAIL=abort-node-left; return 1; }
        return 0
    fi
    kill "$EP" 2>/dev/null; wait "$EP" 2>/dev/null
    if [ -n "$FP" ]; then   # fdsampler ends at the harness stop (5 s poll); bounded: no node pids line = a 900 s wait
        t=0; while kill -0 "$FP" 2>/dev/null && [ $t -lt 10 ]; do sleep 1; t=$((t+1)); done
        stopbg "$FP"; wait "$FP" 2>/dev/null
    fi
    log "END $label rc=$rc $(tail -1 "$R/$label.campaign.log" 2>/dev/null)"
    [ -z "$(pgrep -x torus-node)" ] || { log "ABORT: torus-node left running after $label"; CELL_FAIL=abort-node-left; return 1; }
    if [ "$SAMPLERS" = 1 ]; then
        echo "[$label] $(fdsum "$label")" || true; fdsum "$label" > /dev/null || fdok=1
    fi
    cellchecks "$R/$label" "$arm" || crc=1
    campaignchecks "$label" "$arm" || crc=1
    if [ "$HOST_CHECKS" = 1 ]; then
        sysctlchk "$label" end || crc=1
    fi
    if gated "$tg"; then
        warmgatechk "$label" "$rc" || crc=1
    fi
    [ "$crc" = 0 ] || { CELL_FAIL=check; return 1; }
    hardscan "$label"
    if [ "$SAMPLERS" = 1 ]; then
        soakchk "$label" "$dur"
    fi
    [ "$rc" = 0 ] || log "CELL FAIL $label: run-cell rc=$rc -> recorded (cells.tsv row + rerun), continuing"
    if [ "$fdok" = 1 ]; then log "STOP: a node held > $FD_STOP open fds in $label"; CELL_FAIL=stop-fd; return 1; fi
    return 0
}

run_spec() { # SPEC [SUFFIX]: one cell, then its write-up step (cells.tsv row, CELL-FLAG + rerun when bad), in sequence.
    # Returns cell()'s code: 0 = go on, 1 = stop. A started cell that cell() failed is passed as bad:<CELL_FAIL>.
    local spec=$1 sfx=${2:-} tg arm dur kind killat label crc xr=
    IFS=: read -r tg arm dur kind killat <<< "$spec"
    tg=$tg$sfx; label=$P-$tg
    STARTED_LABEL= CELL_FAIL=
    cell "$tg" "$arm" "$dur" "$kind" "$killat"; crc=$?
    if [ "$STARTED_LABEL" = "$label" ]; then
        [ "$crc" = 0 ] || { xr=${CELL_FAIL:-check}; log "$label failed the campaign (cell rc $crc): step flags it bad:$xr"; }
        CELL_STEP_SUMMARY_MD5=$(step_summary_md5 "$arm") \
            cell_step "$RUN" "$R/$label" "$label" "$tg" "$arm" "${tg%%-*}" "${BMD5[${ABIN[$arm]}]}" "$spec" "$xr"
    else
        log "$label did not start: no cells.tsv row"
    fi
    return $crc
}

run_cells() { # the campaign: each of CELLS through run_spec in order; 1 = stopped (a cell said stop)
    local c
    for c in "${CELLS[@]}"; do
        run_spec "$c" || { log "CAMPAIGN STOP at ${c%%:*} (queued reruns not run: $RUN/rerun-queue.tsv)"; return 1; }
    done
}

# ---- preflight ----
PF() { echo "PREFLIGHT FAIL: $*"; exit 1; }

decl_warn() { # FILE: PREFLIGHT WARN for each top-level (column 0) `declare -A|-a` without -g: sourced inside
    # load_checks, bash makes it a local of that function (load_checks keeps it for the checks, no other driver code
    # sees it), without any error
    awk -v f="$1" '/^declare[ \t]/ { o = ""; for (i = 2; i <= NF && $i ~ /^-/; i++) o = o $i
        if (o ~ /[aA]/ && o !~ /g/) printf "PREFLIGHT WARN: %s:%d: %s: top-level declare -A/-a without -g is a local of load_checks (the checks still get it, as sourced; nothing else does): use declare -g%s\n", f, NR, $0, (o ~ /A/ ? "A" : "a") }' "$1"
}

drv_state() { # load_checks: md5 of every driver function (_DRV_FNS) and of every DRIVER_VARS value
    { for _DRV_F in $_DRV_FNS; do declare -f "$_DRV_F"; done; declare -p ${DRIVER_VARS//$'\n'/ } 2>/dev/null; } | md5sum
}

load_checks() { # the spec checks: source CHECKS_SOURCE, then every CHECK_FNS function must be defined, and no driver
    # function or name may have changed. Scoping (Codex P2; bash variables are dynamic): this function has no
    # locals (its state is in _DRV_ names, a prefix the checks file may not use) and preflight_static calls it from a
    # frame without locals, so the file sees only driver globals at source time and each variable its top level
    # sets is a global or a local of this function (declare without -g, local). _DRV_DECL keeps `declare -p` of each
    # (the new or changed lines of declare -p across the source, minus the shell's own names, DRIVER_VARS and _DRV_*),
    # _DRV_KEEP the names _run_check leaves visible. Limits: a top-level assignment that leaves a variable exactly
    # as it was is not seen (only possible for a pre-existing non-driver global: an env name stays visible anyway);
    # the snapshot is the preflight's (a check never sees a later change, as before: it runs in a subshell); a
    # readonly global (readonly NAME at top level) is not in _DRV_DECL (no function can shadow it); the file's
    # functions are not isolated (it must not redefine a driver function: the md5 below)
    [ -n "$CHECKS_SOURCE" ] || return 0
    [ -f "$CHECKS_SOURCE" ] && [ -r "$CHECKS_SOURCE" ] || PF "checks source $CHECKS_SOURCE missing or unreadable (spec checks.source)"
    _DRV_L=$(grep -n -m1 '_DRV_' "$CHECKS_SOURCE") && PF "$CHECKS_SOURCE:${_DRV_L%%:*}: uses the reserved prefix _DRV_ (the driver keeps its check state in _DRV_ names, comments count too); rename it there"
    decl_warn "$CHECKS_SOURCE"
    _DRV_FNS=$(compgen -A function)
    _DRV_SUM=$(drv_state)
    _DRV_PRE=$(declare -p)
    # shellcheck disable=SC1090
    source "$CHECKS_SOURCE" || PF "cannot source $CHECKS_SOURCE (spec checks.source): it returned $?"
    _DRV_POST=$(declare -p)
    _DRV_LOC=$(local)
    [ "$_DRV_SUM" = "$(drv_state)" ] || PF "$CHECKS_SOURCE redefines a driver function or driver variable (DRIVER_VARS); rename it there"
    _DRV_KEEP=" $(env -i "$BASH" --norc --noprofile -c 'compgen -v' | tr '\n' ' ')FUNCNAME PIPESTATUS OPTARG COPROC COLUMNS LINES OLDPWD PS1 PS2 PS3 TIMEFORMAT ${DRIVER_VARS//$'\n'/ } "
    _DRV_DECL=
    for _DRV_N in $(grep -Fxv -f <(printf '%s\n' "$_DRV_PRE") <<< "$_DRV_POST" | sed -n -E 's/^declare -[^ ]* ([A-Za-z_][A-Za-z0-9_]*)(=.*)?$/\1/p'); do
        case $_DRV_N in BASH*|_DRV_*) continue ;; esac
        case $_DRV_KEEP in *" $_DRV_N "*) continue ;; esac
        _DRV_KEEP+="$_DRV_N "
        _DRV_L=$(declare -p "$_DRV_N")
        _DRV_F=${_DRV_L#declare }; _DRV_F=${_DRV_F%% *}
        if [[ $_DRV_F == *r* ]] && ! grep -q -E "^declare -[^ ]* $_DRV_N(=|\$)" <<< "$_DRV_LOC"; then continue; fi
        _DRV_DECL+=$_DRV_L$'\n'
    done
    for _DRV_N in $CHECK_FNS; do
        declare -F "$_DRV_N" > /dev/null || PF "check function $_DRV_N is not defined by $CHECKS_SOURCE (spec checks.per_cell)"
    done
}

wrapper_target() { # FILE: the absolute path a wrapper script execs (its last `exec PATH` line); fails on a binary
    local t
    grep -qI . "$1" 2>/dev/null || return 1
    t=$(sed -n -E 's/^[[:space:]]*exec[[:space:]]+"?([^" ]+)"?.*/\1/p' "$1" | tail -1)
    [[ "$t" == /* ]] && [ -x "$t" ] || return 1
    echo "$t"
}

preflight_genesis() { # the genesis inputs under run-cell.sh's repo root GENESIS_ROOT = $TOOLS/../.. (its MAINREPO):
    # present, and equal to the harness worktree's (genesis from one tree, nodes launched from another = a mixed
    # cell). Job 4 froze them beside its tools copy (ozarchy-p4j4-tools/{devnet,testnet}) and diffed devnet/wsl.
    # testnet/genesis-weighted-full.json is untracked: required (else the first cell builds it), not compared.
    local f d
    GENESIS_ROOT=$(cd "$TOOLS/../.." 2>/dev/null && pwd) || PF "cannot resolve $TOOLS/../.. (run-cell.sh's repo root)"
    for f in $GENESIS_INPUTS testnet/genesis-weighted-full.json; do
        [ -s "$GENESIS_ROOT/$f" ] || PF "genesis input $GENESIS_ROOT/$f missing or empty: run-cell.sh reads its genesis inputs from its own repo root (tools_dir/../..), not from harness_worktree; freeze them beside the tools copy (README: Campaign driver generator)"
    done
    [ -x "$GENESIS_ROOT/devnet/wsl/gen-3val-genesis.sh" ] || PF "$GENESIS_ROOT/devnet/wsl/gen-3val-genesis.sh not executable"
    d=$(diff -rq -x genesis-3val.json "$WT_H/devnet/wsl" "$GENESIS_ROOT/devnet/wsl" 2>&1
        for f in $GENESIS_INPUTS; do
            case $f in devnet/wsl/*) ;; *) cmp -s "$WT_H/$f" "$GENESIS_ROOT/$f" || echo "$f differs";; esac
        done)
    [ -z "$d" ] || PF "genesis inputs under $GENESIS_ROOT differ from the harness worktree $WT_H (run-cell.sh generates genesis from the first, launches nodes from the second): $(echo $d | head -c 600)"
    return 0
}

preflight_static() { # preflight_paths, then the spec checks (load_checks), from this frame: it has no locals, which
    # the checks file would see at source time or set instead of its own variables (load_checks)
    preflight_paths
    load_checks
}

preflight_paths() { # paths, tools, genesis inputs, staged md5s (a wrapper's md5 = the md5 of the node it execs)
    local f b node m tgt
    for f in "$HARNESS" "$DETACH" "$TOOLS/cell-step.sh" "$TOOLS/cell_step.py"; do [ -f "$f" ] || PF "$f missing"; done
    for f in pgrep zcat timeout; do command -v "$f" > /dev/null || PF "$f missing (the watchers use it)"; done
    if [ "$SAMPLERS" = 1 ]; then
        for f in vmstat curl gzip du python3; do command -v "$f" > /dev/null || PF "$f missing (the samplers use it; spec \"samplers\": false runs without them)"; done
        [ -f "$SOAKPY" ] || PF "$SOAKPY missing (the 1 Hz soak sampler: copy tools/matched-bench/soak-sampler.py into tools_dir; spec \"samplers\": false runs without it)"
        python3 -I -c 'import ast, sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' "$SOAKPY" 2>/dev/null || PF "$SOAKPY does not compile"
        grep -q torus_rpc_submit_admit_seconds_bucket "$SOAKPY" && grep -q torus_commit_interval_seconds_bucket "$SOAKPY" || PF "$SOAKPY lacks the slow-tail buckets (an old soak sampler: copy tools/matched-bench/soak-sampler.py)"
    fi
    [ -d "$WT_H" ] || PF "harness worktree $WT_H missing"
    preflight_genesis
    for b in "${!BDIR[@]}"; do
        node=${BDIR[$b]}/release/torus-node
        [ -x "$node" ] || PF "binary $b: $node missing or not executable"
        if [ "${BWRAP[$b]}" = 1 ]; then
            tgt=$(wrapper_target "$node") || PF "binary $b: $node is declared a wrapper but has no 'exec /abs/path' line to an executable"
            m=$(md5sum < "$tgt" | cut -c1-32)
            [ "$m" = "${BMD5[$b]}" ] || PF "binary $b: wrapper $node execs $tgt, md5 $m != staged ${BMD5[$b]}"
            grep -q -- '--no-summary-md5' "$TOOLS/cell_step.py" || PF "$TOOLS/cell_step.py lacks --no-summary-md5 (wrapper arm $b): copy current tools"
        else
            m=$(md5sum < "$node" | cut -c1-32)
            [ "$m" = "${BMD5[$b]}" ] || PF "binary $b: $node md5 $m != staged ${BMD5[$b]}"
        fi
        if [ -n "${BBMD5[$b]}" ]; then
            m=$(md5sum < "${BDIR[$b]}/release/bench-throughput" 2>/dev/null | cut -c1-32)
            [ "$m" = "${BBMD5[$b]}" ] || PF "binary $b: bench-throughput md5 ${m:-missing} != staged ${BBMD5[$b]}"
        fi
    done
    for f in $ANALYSIS_CMD; do case $f in */*.py|*/*.sh) [ -f "$f" ] || PF "analysis script $f missing";; esac; done
    return 0
}

preflight_host() { # run mode only: inside a bench unit, nothing else running, no old attempt; host checks: memory,
    # sysctls (the dirty limits pinned for the whole run)
    local c g self other f x
    self=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
    [ -n "$self" ] || PF "not inside a bench-*.service unit: start it with: $SELF launch${SMOKE:+ smoke} (detach.sh holds the build lock)"
    other=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "$self")
    [ -z "$other" ] || PF "other bench units active: $(echo $other)"
    [ -z "$(buildhits)" ] || PF "build running: $(buildhits)"
    [ -z "$(benchhits)" ] || PF "node or bench running: $(benchhits)"
    for f in cells.tsv rerun-queue.tsv; do [ ! -e "$RUN/$f" ] || PF "$RUN/$f exists (old attempt: move it away first)"; done
    for c in "${CELLS[@]}"; do g=${c%%:*}
        for x in "$P-$g" "$P-$g-rerun"; do [ ! -e "$R/$x" ] && [ ! -e "$R/$x.cell.rc" ] || PF "$R/$x exists (old attempt)"; done
    done
    [ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge "$MIN_FREE_MB" ] || PF "< $MIN_FREE_MB MB free under $R"
    if [ "$HOST_CHECKS" = 1 ]; then
        preflight_mem
        sysctl_pin
    fi
}

host_str() { # the PREFLIGHT OK line's warm-up gate / campaign checks / samplers / host checks part
    local k g=${GATE_TAGS// /,} c=${CHECK_FNS// /,}
    printf 'warmgate=%s checks=%s samplers=%s host_checks=%s' "${g:-off}" "${c:-none}" "$SAMPLERS" "$HOST_CHECKS"
    [ "$HOST_CHECKS" = 1 ] || return 0
    printf ' min_mem_avail_kb=%s' "$MIN_MEM_AVAIL_KB"
    for k in $(sysctl_names); do printf ' %s=%s' "$k" "${SYSCTL_WANT[$k]:-pinned}"; done
}

scrub_env() { # the units get this process's EXPORTED env (detach.sh): only the basics stay exported, so no
    # TORUS_*, MALLOC_CONF, LD_PRELOAD or run-cell.sh knob from the launching shell reaches a cell. A driver name
    # (DRIVER_VARS) the caller happened to export keeps the driver's value and is only un-exported: unsetting it
    # would delete driver config (RUN exported by the caller -> RUN unbound under set -u)
    local _sv
    for _sv in $(compgen -e); do
        case $_sv in PATH|HOME|USER|LOGNAME|SHELL|LANG|LC_*|TZ|TMPDIR|XDG_RUNTIME_DIR|DBUS_SESSION_BUS_ADDRESS) continue ;; esac
        if [[ " ${DRIVER_VARS//$'\n'/ } " == *" $_sv "* ]]; then export -n "$_sv"; else unset "$_sv"; fi
    done
    export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
}

[ "${DRIVER_LIB_ONLY:-0}" = 1 ] && return 0

# ---- main ----
MODE=${1:-}; SM=${2:-}
case "$MODE:$SM" in
    launch:|launch:smoke|dry:|dry:smoke|run:|run:smoke|show:|show:smoke) ;;
    *) echo "usage: $0 launch|dry|show|run [smoke]   (see the header)"; exit 2 ;;
esac
if [ "$MODE" = show ]; then show_plan "$SM"; exit 0; fi
[ "$SM" = smoke ] && apply_smoke
TAGS=$(for c in "${CELLS[@]}"; do printf '%s ' "${c%%:*}"; done)

case $MODE in
dry)
    preflight_static
    echo "PREFLIGHT OK (dry: host state not checked) CAMPAIGN=$UNIT_NAME RUN=$RUN P=$P genesis_root=$GENESIS_ROOT arms=${ARMS[*]} baseline=$BASELINE markets=$MARKETS rate=$RATE $(host_str) cells=${#CELLS[@]}: $TAGS"
    exit 0 ;;
launch)
    preflight_static
    grep -qE '/bench-[^/]*\.service$' /proc/self/cgroup && PF "launch from outside a bench unit (this shell is inside one)"
    [ ! -e "$RUN/campaign.done" ] || PF "$RUN/campaign.done exists (old attempt: move it away first)"
    mkdir -p "$RUN" || PF "cannot create $RUN"
    scrub_env
    exec "$DETACH" "$UNIT_NAME" "$RUN/campaign.log" "$SELF" run $SM ;;
esac

# run
mkdir -p "$RUN" || exit 1
trap 'rc=$?; echo "exit=$rc $(date +%FT%T)" > "$RUN/campaign.done"' EXIT
scrub_env
preflight_static
preflight_host
export CELL_STEP_LOG=$RUN/campaign.log
export CELL_STEP_TIMEOUT=${CELL_STEP_TIMEOUT:-900} CELL_STEP_ANALYSIS_TIMEOUT=${CELL_STEP_ANALYSIS_TIMEOUT:-600}
[ -z "$ANALYSIS_CMD" ] || export CELL_STEP_ANALYSIS_CMD=$ANALYSIS_CMD
[ -z "$ANALYSIS_METRIC" ] || export CELL_STEP_ANALYSIS_METRIC=$ANALYSIS_METRIC
# shellcheck source=../cell-step.sh
source "$TOOLS/cell-step.sh" || PF "cannot source $TOOLS/cell-step.sh"
log "PREFLIGHT OK CAMPAIGN=$UNIT_NAME driver=$SELF RUN=$RUN P=$P genesis_root=$GENESIS_ROOT arms=${ARMS[*]} baseline=$BASELINE markets=$MARKETS rate=$RATE $(host_str) cells=${#CELLS[@]}: $TAGS"
run_cells || exit 1
log "MAIN CELLS END"
if [ "${SMOKE:-0}" != 1 ]; then
    cell_step_rerun_pass "$RUN" run_spec || { log "CAMPAIGN STOP in the rerun pass"; exit 1; }
fi
log "CAMPAIGN END"
[ -s "$RUN/cells.tsv" ] && awk -F'\t' 'NR > 1 {n++; if ($8 != "ok") b++} END {printf "cells.tsv: %d rows, %d bad\n", n, b}' "$RUN/cells.tsv"
exit 0
