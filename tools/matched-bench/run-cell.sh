#!/usr/bin/env bash
# run-cell.sh — reusable ONE-CELL runner for the matched/s devnet campaign.
#
#   run-cell.sh <worktree> <label> [MARKETS=10] [DUR=120] [RATE=76000] [EXTRA_ENV='K=V ...']
#
# BUILDS NOTHING. It stages the binary that already sits in
# $TARGET_DIR/release/torus-node (default /home/18c/.cargo-target-matched) into
# <worktree>/target/release/torus-node (devnet/wsl/env.sh hardcodes that path),
# proves the copy with md5sums, generates a MARKETS-market 3-val genesis, launches
# the bare-metal 3-validator devnet with the RE-PROOF5 record-cell env (+EXTRA_ENV
# overrides), waits for health, samples all 3 nodes' /metrics at 1 Hz, runs the
# record-cell bench-throughput command, drains, checks 3-validator agreement,
# stops the devnet and writes  $RESULTS_ROOT/<label>/summary.json.
#
# All throughput numbers come from NODE Prometheus counters
# (torus_orders_matched_total etc.) — never from bench-side math, never placed/s.
#
# Optional env overrides:
#   TARGET_DIR   cargo target dir holding torus-node + bench-throughput
#                (default /home/18c/.cargo-target-matched)
#   RESULTS_ROOT (default /home/18c/bench-results-matched)
#   DATA_ROOT    devnet data root (default $HOME/torus-wsl-devnet)
#   SENDERS      bench --senders (default 5000)   CONC  --concurrency (256)
#   BATCH        --batch-size (400)               SUBMIT --submit-batch (1)
#   BLOCK_CAP=N  block-cap-raise sweep bundle: exports the COHERENT set of
#                proposer-local selection caps for an N-action native block
#                (TORUS_NATIVE_TOTAL_BLOCK_CAP=N plus the companion caps that
#                would otherwise bind first — see block_cap_bundle below).
#                Applied after RECORD_ENV and before EXTRA_ENV, so EXTRA_ENV
#                can still override any single knob. DEFAULT 200 since the r3
#                merge (block-cap-raise-sweep winner); BLOCK_CAP=100 is the
#                cap-100 control (reproduces the compiled node defaults exactly).
#   OVERWRITE=1  allow reusing an existing non-empty results dir
#   HEALTH_TIMEOUT (240 s)  DRAIN_TIMEOUT (180 s)
set -uo pipefail

usage() { sed -n '2,27p' "$0"; exit 2; }
[ $# -ge 2 ] || usage

WT=$(cd "$1" && pwd) || { echo "FATAL: worktree '$1' not found" >&2; exit 2; }
LABEL=$2
MARKETS=${3:-10}
DUR=${4:-120}
RATE=${5:-76000}
EXTRA_ENV=${6:-}

TOOLS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
MAINREPO=$(cd "$TOOLS_DIR/../.." && pwd)
TARGET_DIR=${TARGET_DIR:-/home/18c/.cargo-target-matched}
RESULTS_ROOT=${RESULTS_ROOT:-/home/18c/bench-results-matched}
export DATA_ROOT=${DATA_ROOT:-$HOME/torus-wsl-devnet}
SENDERS=${SENDERS:-5000}
CONC=${CONC:-256}
BATCH=${BATCH:-400}
SUBMIT=${SUBMIT:-1}
HEALTH_TIMEOUT=${HEALTH_TIMEOUT:-240}
DRAIN_TIMEOUT=${DRAIN_TIMEOUT:-180}
OUT="$RESULTS_ROOT/$LABEL"

SRC_NODE="$TARGET_DIR/release/torus-node"
BENCH="$TARGET_DIR/release/bench-throughput"
DST_NODE="$WT/target/release/torus-node"
GENESIS="$WT/devnet/wsl/genesis-3val.json"
WSL="$WT/devnet/wsl"
RUN_DIR="$DATA_ROOT/run"

# RE-PROOF5 record cell (R190) node env, with TORUS_BOOK_ROWS bumped 2 -> 3
# after the r2 merge (level-hash-seq-chunked: mode 3 chunked level digest,
# consensus-visible, fleet-uniform, fresh genesis — all three hold here since
# every cell is CLEAN=1 with one env for all 3 nodes). The node-code default
# stays Classic (unset). EXTRA_ENV entries are applied AFTER these, so a cell
# can override any of them (e.g. EXTRA_ENV='TORUS_BOOK_ROWS=2' for a mode-2
# control, or 'TORUS_PARALLEL_SETTLE=0').
RECORD_ENV=(
    TORUS_BOOK_ROWS=3
    TORUS_RESIDENT_BOOKS=1
    TORUS_NATIVE_ROOT_CACHE=1
    TORUS_PARALLEL_SETTLE=1
    TORUS_PARALLEL_BUCKET_HASH=4
    TORUS_BUCKET_MEMBER_CACHE_MB=256
    TORUS_COMMIT_LAG_BACKOFF_CAP=8
)

# block-cap-raise sweep bundle (r2 candidate block-cap-raise-sweep). All four
# knobs are PROPOSER-LOCAL selection policy / node-local cache sizing
# (validate_block rejects on none of them), so they cannot fork; run-cell.sh
# exports the same values to all 3 nodes anyway. Companion caps, and why they
# must move with the action cap (P3 cap-sweep verdict, mem 74c34440):
#   TORUS_NATIVE_ORDERS_PER_BLOCK_CAP  default 50_000 binds FIRST at bs400
#       (125 actions) => max(50_000, N*BATCH*1.25): N=100 reproduces the 50_000
#       default exactly (clean control cell), never binding above it.
#   TORUS_VERIFIED_SENDER_CACHE_CAP    exec trust-cache must bridge the 64-block
#       exec queue: 64*N*2.5 (16_384 floor). The r2 binary derives this default
#       itself; exporting it keeps env-only cells on OLDER binaries equivalent.
#   TORUS_NATIVE_BLOCK_BYTES_CAP       6 MB default; N*BATCH*~70 B touches it at
#       N=200 => max(6 MB, N*BATCH*150 B) CAPPED at 12 MB — MAX_BLOCK_DATA_MSG_SIZE
#       (block-data sync codec) is 16 MB, a body above it could never be synced.
# NOT touched: TORUS_HASH_ONLY_PUSH_THRESHOLD (env.sh 6 MB, clamped to the 4 MB
# fleet floor) — bodies above ~4 MB (N>=150 at bs400) go out as a HASH manifest
# and are PULLED (the S387-hardened path); the dissemination counters below
# (manifest pushes / body-fetch exhaustion / sync fallbacks) record the cost.
block_cap_bundle() { # $1=N -> prints K=V lines
    local n=$1 orders bytes cache
    orders=$(( n * BATCH * 5 / 4 )); [ "$orders" -lt 50000 ] && orders=50000
    cache=$(( 64 * n * 5 / 2 )); [ "$cache" -lt 16384 ] && cache=16384
    bytes=$(( n * BATCH * 150 )); [ "$bytes" -lt 6000000 ] && bytes=6000000; [ "$bytes" -gt 12000000 ] && bytes=12000000
    printf 'TORUS_NATIVE_TOTAL_BLOCK_CAP=%s\nTORUS_NATIVE_ORDERS_PER_BLOCK_CAP=%s\nTORUS_VERIFIED_SENDER_CACHE_CAP=%s\nTORUS_NATIVE_BLOCK_BYTES_CAP=%s\n' \
        "$n" "$orders" "$cache" "$bytes"
}
# r3 merge: the harness defaults to the block-cap-raise-sweep winner (cap 200).
# The compiled node default stays 100 (WAN dissemination guard — the raise is
# env-only until a full-mesh bench earns it); BLOCK_CAP=100 = control cell.
BLOCK_CAP=${BLOCK_CAP:-200}
if [ -n "$BLOCK_CAP" ]; then
    [[ "$BLOCK_CAP" =~ ^[0-9]+$ ]] && [ "$BLOCK_CAP" -ge 1 ] || { echo "FATAL: BLOCK_CAP must be a positive integer" >&2; exit 2; }
    mapfile -t BLOCK_CAP_ENV < <(block_cap_bundle "$BLOCK_CAP")
else
    BLOCK_CAP_ENV=()
fi

RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)
METS=(9161 9162 9163)

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }
die() { log "FATAL: $*"; finish_fail; exit 1; }

# ---------------------------------------------------------------- pre-flight
[ -x "$SRC_NODE" ] || { echo "FATAL: missing $SRC_NODE" >&2; exit 1; }
[ -x "$BENCH" ]    || { echo "FATAL: missing $BENCH" >&2; exit 1; }
[ -x "$WSL/launch-3val.sh" ] || { echo "FATAL: $WSL/launch-3val.sh missing" >&2; exit 1; }
for t in jq curl python3 md5sum awk; do command -v $t >/dev/null || { echo "FATAL: need $t" >&2; exit 1; }; done
[[ "$MARKETS" =~ ^[0-9]+$ && "$DUR" =~ ^[0-9]+$ && "$RATE" =~ ^[0-9]+$ ]] || usage

if pgrep -f "bench-throughput consensus" >/dev/null; then echo "FATAL: a bench is already running" >&2; exit 1; fi
if pgrep -f "cargo build" >/dev/null; then echo "FATAL: a cargo build is running — never bench while building" >&2; exit 1; fi
if [ -f "$RUN_DIR/pids" ] && xargs -a "$RUN_DIR/pids" -r -I{} kill -0 {} 2>/dev/null; then
    echo "FATAL: a devnet is already running ($RUN_DIR/pids) — run $WSL/stop-3val.sh first" >&2; exit 1
fi
# The live testnet validator (8555/9090/30333) is never touched: we only use
# 8645-8647 / 9161-9163 / 30401-30403 and never write into ~/.cargo-target.
if [ -d "$OUT" ] && [ -n "$(ls -A "$OUT" 2>/dev/null)" ] && [ "${OVERWRITE:-0}" != 1 ]; then
    echo "FATAL: $OUT exists and is non-empty (pick a new label or OVERWRITE=1)" >&2; exit 1
fi
mkdir -p "$OUT"
: > "$OUT/run.log"

SAMPLER_PID=""; CPU_PID=""; BENCH_PID=""
finish_fail() {
    [ -n "$SAMPLER_PID" ] && kill "$SAMPLER_PID" 2>/dev/null
    [ -n "$CPU_PID" ] && kill "$CPU_PID" 2>/dev/null
    [ -n "$BENCH_PID" ] && kill "$BENCH_PID" 2>/dev/null
    "$WSL/stop-3val.sh" >>"$OUT/run.log" 2>&1 || true
    python3 - "$OUT" <<'PY' 2>/dev/null || true
import json,sys,os,time
p=os.path.join(sys.argv[1],'summary.json')
d={}
if os.path.exists(p):
    try:
        d=json.load(open(p))
    except (OSError, ValueError):
        d={}
d['status']='FAILED'; d['failed_at']=time.strftime('%Y-%m-%dT%H:%M:%S')
json.dump(d,open(p,'w'),indent=1)
PY
}
trap 'log "interrupted"; finish_fail; exit 130' INT TERM

log "cell=$LABEL worktree=$WT markets=$MARKETS dur=${DUR}s rate=$RATE senders=$SENDERS block_cap='${BLOCK_CAP:-unset}' extra_env='$EXTRA_ENV'"
[ -n "$BLOCK_CAP" ] && log "block-cap bundle (BLOCK_CAP=$BLOCK_CAP, BATCH=$BATCH): ${BLOCK_CAP_ENV[*]}"
WT_COMMIT=$(git -C "$WT" rev-parse HEAD)
WT_DIRTY=$(git -C "$WT" status --porcelain --untracked-files=no | wc -l)
log "worktree commit=$WT_COMMIT dirty_files=$WT_DIRTY"

# ---------------------------------------------------------------- 1. stage binary
mkdir -p "$(dirname "$DST_NODE")"
cp -f "$SRC_NODE" "$DST_NODE" || die "copy failed"
MD5_SRC=$(md5sum "$SRC_NODE" | cut -d' ' -f1)
MD5_DST=$(md5sum "$DST_NODE" | cut -d' ' -f1)
MD5_BENCH=$(md5sum "$BENCH" | cut -d' ' -f1)
log "torus-node md5 src=$MD5_SRC ($SRC_NODE, mtime $(date -r "$SRC_NODE" +%FT%T))"
log "torus-node md5 dst=$MD5_DST ($DST_NODE)"
[ "$MD5_SRC" = "$MD5_DST" ] || die "md5 mismatch after copy (stale-binary trap)"
log "bench-throughput md5=$MD5_BENCH ($BENCH, mtime $(date -r "$BENCH" +%FT%T))"

# ---------------------------------------------------------------- 2. genesis
log "generating $MARKETS-market 3-val genesis -> $GENESIS"
MARKETS=$MARKETS OUT="$GENESIS" BENCH_BIN="$BENCH" "$MAINREPO/devnet/wsl/gen-3val-genesis.sh" >>"$OUT/run.log" 2>&1 \
    || die "genesis generation failed (see run.log)"
GEN_MARKETS=$(jq '.markets|length' "$GENESIS")
GEN_ACCTS=$(jq '.native_balances|length' "$GENESIS")
GEN_MD5=$(md5sum "$GENESIS" | cut -d' ' -f1)
[ "$GEN_MARKETS" = "$MARKETS" ] || die "genesis has $GEN_MARKETS markets, wanted $MARKETS"
log "genesis markets=$GEN_MARKETS native_balances=$GEN_ACCTS md5=$GEN_MD5"

# ---------------------------------------------------------------- 3. launch
# start from a clean TORUS_* env: only the record env + EXTRA_ENV reach the nodes
for v in $(env | grep -oE '^TORUS_[A-Za-z0-9_]+'); do unset "$v"; done
for kv in "${RECORD_ENV[@]}"; do export "$kv"; done
for kv in "${BLOCK_CAP_ENV[@]}"; do export "$kv"; done
for kv in $EXTRA_ENV; do export "$kv"; done
NODE_ENV_JSON=$(env | grep -E '^TORUS_' | sort | python3 -c 'import sys,json;print(json.dumps(dict(l.rstrip("\n").split("=",1) for l in sys.stdin)))')
log "node env: $NODE_ENV_JSON"
T_LAUNCH=$(date +%s)
CLEAN=1 "$WSL/launch-3val.sh" >>"$OUT/run.log" 2>&1 || die "launch-3val.sh failed"
sleep 2
mapfile -t PIDS < "$RUN_DIR/pids"
log "node pids: ${PIDS[*]}"
# verify the env actually reached each node process (fleet-uniform)
ENV_VERIFY=$(for p in "${PIDS[@]}"; do tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -E '^TORUS_' | sort | md5sum | cut -c1-8; done | sort -u | tr '\n' ' ')
log "per-node TORUS_* env digest(s): $ENV_VERIFY (must be a single value)"

# ---------------------------------------------------------------- 4. health
scrape_one() { curl -s -m 3 "http://127.0.0.1:$1/metrics"; }
mval() { awk -v m="$1" '$1==m {print $2; f=1} END{if(!f) print 0}'; }

log "waiting for health (all 3 committing, peers>=2, timeout ${HEALTH_TIMEOUT}s)"
ok=0; t0=$(date +%s)
declare -a C0=(0 0 0)
while [ $(( $(date +%s) - t0 )) -lt "$HEALTH_TIMEOUT" ]; do
    good=0
    for i in 0 1 2; do
        M=$(scrape_one "${METS[$i]}") || true
        c=$(printf '%s' "$M" | mval torus_blocks_committed_total)
        pc=$(printf '%s' "$M" | mval torus_peers_connected)
        c=${c%.*}; pc=${pc%.*}
        if [ "${c:-0}" -ge $(( ${C0[$i]} + 5 )) ] && [ "${pc:-0}" -ge 2 ]; then good=$((good+1)); fi
        [ "${C0[$i]}" = 0 ] && C0[$i]=${c:-0}
    done
    for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null || die "node pid $p died during startup"; done
    [ "$good" = 3 ] && { ok=1; break; }
    sleep 2
done
[ "$ok" = 1 ] || die "devnet not healthy after ${HEALTH_TIMEOUT}s"
T_HEALTHY=$(date +%s)
log "healthy after $((T_HEALTHY - T_LAUNCH))s"
# idle probe (10 s) for idle blk/s; startup view-timeout wobble can make the
# first probe read low, so re-probe up to 3x until the mesh settles (>=10 blk/s).
for _try in 1 2 3; do
    ic0=$(scrape_one "${METS[0]}" | mval torus_blocks_committed_total); sleep 10
    ic1=$(scrape_one "${METS[0]}" | mval torus_blocks_committed_total)
    IDLE_BLKS=$(awk -v a="$ic0" -v b="$ic1" 'BEGIN{printf "%.2f",(b-a)/10}')
    log "idle blk/s (val0, 10s, probe $_try) = $IDLE_BLKS"
    awk -v v="$IDLE_BLKS" 'BEGIN{exit !(v>=10)}' && break
done

# ---------------------------------------------------------------- 5. samplers
# Wide CSV: one row per node per second, plus per-node CSVs in the exact
# layouts of the RE-PROOF5 scrape.sh / scrape-phase.sh so win60.awk / phase60.awk
# run unchanged.
FUNNEL_COLS="torus_native_actions_processed_total torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_orders_rejected_margin_total torus_orders_rejected_book_total torus_orders_rejected_cancelled_total torus_orders_rejected_other_total torus_orders_self_trade_cancels_total torus_orders_cancelled_partial_fill_total torus_blocks_committed_total torus_block_height torus_exec_queue_depth"
PHASE_COLS="torus_blocks_committed_total torus_block_height torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_exec_resting_orders torus_exec_load_books_seconds_sum torus_exec_load_books_seconds_count torus_exec_root_seconds_sum torus_exec_root_seconds_count torus_exec_state_write_seconds_sum torus_exec_state_write_seconds_count torus_exec_evm_resync_seconds_sum torus_exec_evm_resync_seconds_count torus_exec_flush_seconds_sum torus_exec_flush_seconds_count torus_exec_root_dirty_buckets_sum torus_exec_root_dirty_buckets_count torus_exec_queue_depth torus_exec_root_bucket_scans_total torus_member_cache_hits_total torus_member_cache_misses_total torus_member_cache_evictions_total torus_member_cache_resident_buckets"
WIDE_COLS="torus_blocks_committed_total torus_block_height torus_native_actions_processed_total torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_exec_resting_orders torus_orders_rejected_margin_total torus_orders_rejected_book_total torus_orders_rejected_cancelled_total torus_orders_rejected_other_total torus_exec_queue_depth torus_mempool_native_size torus_exec_block_seconds_sum torus_exec_block_seconds_count torus_exec_engine_seconds_count torus_exec_verify_seconds_count torus_exec_flush_seconds_count torus_exec_evm_seconds_sum torus_exec_verify_seconds_sum torus_exec_replay_guard_seconds_sum torus_exec_load_books_seconds_sum torus_exec_engine_seconds_sum torus_exec_phase_margin_seconds_sum torus_exec_phase_match_seconds_sum torus_exec_phase_settle_seconds_sum torus_exec_save_books_seconds_sum torus_exec_flush_seconds_sum torus_exec_root_seconds_sum torus_exec_state_write_seconds_sum torus_exec_evm_resync_seconds_sum torus_exec_body_persist_seconds_sum torus_exec_root_dirty_buckets_sum torus_exec_root_dirty_buckets_count torus_exec_root_bucket_scans_total torus_member_cache_evictions_total torus_commit_interval_seconds_sum torus_commit_interval_seconds_count torus_commit_persist_seconds_sum torus_commit_persist_seconds_count torus_commit_body_encode_seconds_sum torus_commit_persist_write_seconds_sum torus_consensus_timeout_total_total torus_consensus_view torus_block_transactions_count_sum torus_block_transactions_count_count torus_native_gossip_published_actions_total torus_native_gossip_dropped_full_total torus_rocksdb_memtable_bytes torus_rocksdb_l0_files torus_rocksdb_pending_compaction_bytes torus_db_size_bytes"

extract() { # stdin=metrics text, $1=space-separated metric names -> csv values (0 if absent)
    awk -v names="$1" 'BEGIN{n=split(names,a," "); for(i=1;i<=n;i++) want[a[i]]=1}
        ($1 in want){v[$1]=$2}
        END{for(i=1;i<=n;i++) printf "%s%s", (i>1?",":""), (a[i] in v ? v[a[i]] : 0); printf "\n"}'
}

echo "ts,node,$(echo $WIDE_COLS | tr ' ' ',')" > "$OUT/sampler.csv"
for i in 0 1 2; do
    echo "ts,actions_processed,placed_accepted,matched,resting,rej_margin,rej_book,rej_cancelled,rej_other,self_trade_cancels,cancelled_partial_fill,blocks_committed,block_height,exec_queue_depth" > "$OUT/funnel-val$i.csv"
    echo "ts,committed,height,placed,matched,resting,exec_resting,lb_s,lb_c,root_s,root_c,sw_s,sw_c,evm_s,evm_c,fl_s,fl_c,db_s,db_c,execq,bscan,mc_hit,mc_miss,mc_evict,mc_resident" > "$OUT/phase-val$i.csv"
done
sampler() {
    while true; do
        ts=$(date +%s)
        for i in 0 1 2; do
            M=$(scrape_one "${METS[$i]}") || M=""
            echo "$ts,val$i,$(printf '%s' "$M" | extract "$WIDE_COLS")" >> "$OUT/sampler.csv"
            echo "$ts,$(printf '%s' "$M" | extract "$FUNNEL_COLS")" >> "$OUT/funnel-val$i.csv"
            echo "$ts,$(printf '%s' "$M" | extract "$PHASE_COLS")" >> "$OUT/phase-val$i.csv"
        done
        # 1 Hz cadence: the three scrapes take ~100-300 ms; keep to a 1 s grid.
        now=$(date +%s); [ "$now" = "$ts" ] && sleep 1
    done
}
cpusampler() {
    echo "ts,load1,load5,ncpu,pid,comm,pcpu,rss_kb" > "$OUT/cpu.csv"
    while true; do
        ts=$(date +%s); read -r l1 l5 _ < /proc/loadavg
        ps -o pid=,comm=,pcpu=,rss= -p "$(tr '\n' ',' < "$RUN_DIR/pids" | sed 's/,$//')${BENCH_PID:+,$BENCH_PID}" 2>/dev/null \
            | awk -v ts="$ts" -v l1="$l1" -v l5="$l5" -v n="$(nproc)" '{printf "%s,%s,%s,%s,%s,%s,%s,%s\n",ts,l1,l5,n,$1,$2,$3,$4}' >> "$OUT/cpu.csv"
        sleep 10
    done
}
for i in 0 1 2; do scrape_one "${METS[$i]}" > "$OUT/metrics-before-val$i.txt"; done
sampler & SAMPLER_PID=$!
sleep 3

# ---------------------------------------------------------------- 6. bench
BENCH_CMD=("$BENCH" consensus --rpc-urls "${RPCS[0]}" --econ --senders "$SENDERS" --sender-offset 60 \
    --markets "$MARKETS" --batch-size "$BATCH" --submit-batch "$SUBMIT" --format bin --concurrency "$CONC" \
    --duration "$DUR" --target-margin 1500 --cross-fraction 0.5 --cancel-fraction 0.05 --band 5 --rate-total "$RATE")
log "bench: ${BENCH_CMD[*]}"
T_BENCH0=$(date +%s)
"${BENCH_CMD[@]}" > "$OUT/bench.log" 2>&1 & BENCH_PID=$!
cpusampler & CPU_PID=$!
wait "$BENCH_PID"; BENCH_RC=$?
T_BENCH1=$(date +%s)
BENCH_PID=""
log "bench exited rc=$BENCH_RC after $((T_BENCH1 - T_BENCH0))s"

# ---------------------------------------------------------------- 7. drain
# The mempool/exec-queue gauges can read 0 while actions are still in flight
# (forward batcher, deferred-exec park, consensus stalls), so "drained" means:
# placed/matched/actions/resting counters UNCHANGED on all 3 nodes for QUIET_S
# consecutive polls. Idle empty blocks keep flowing (exec queue may read 1-2).
QUIET_S=${QUIET_S:-10}
log "draining (order/action counters quiescent for ${QUIET_S}s on all nodes, timeout ${DRAIN_TIMEOUT}s)"
td=$(date +%s); DRAINED=0; stable=0; prev=""
while [ $(( $(date +%s) - td )) -lt "$DRAIN_TIMEOUT" ]; do
    cur=""; q=0
    for i in 0 1 2; do
        M=$(scrape_one "${METS[$i]}") || M=""
        cur="$cur|$(printf '%s' "$M" | extract "torus_orders_placed_accepted_total torus_orders_matched_total torus_native_actions_processed_total torus_orders_resting_total")"
    done
    # (exec_queue_depth is NOT required to be 0: idle empty blocks keep it at 1-2)
    if [ "$cur" = "$prev" ]; then stable=$((stable+1)); else stable=0; fi
    prev="$cur"
    [ "$stable" -ge "$QUIET_S" ] && { DRAINED=1; break; }
    sleep 1
done
T_DRAIN=$(date +%s)
log "drained=$DRAINED after $((T_DRAIN - T_BENCH1))s"
sleep 2
kill "$SAMPLER_PID" 2>/dev/null; SAMPLER_PID=""
kill "$CPU_PID" 2>/dev/null; CPU_PID=""

# ---------------------------------------------------------------- 8. after-snapshots + agreement
for i in 0 1 2; do scrape_one "${METS[$i]}" > "$OUT/metrics-after-val$i.txt"; done
rpc() { # $1=url $2=method $3=params-json
    curl -s -m 10 -H 'content-type: application/json' "$1" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}"
}
declare -a HGT
for i in 0 1 2; do HGT[$i]=$(mval torus_block_height < "$OUT/metrics-after-val$i.txt"); HGT[$i]=${HGT[$i]%.*}; done
HMIN=$(printf '%s\n' "${HGT[@]}" | sort -n | head -1); HMAX=$(printf '%s\n' "${HGT[@]}" | sort -n | tail -1)
HCMP=$(( HMIN - 5 )); HHEX=$(printf '0x%x' "$HCMP")
log "heights after drain: ${HGT[*]} (spread $((HMAX-HMIN))); comparing block $HCMP on all nodes"
: > "$OUT/agreement.jsonl"
for i in 0 1 2; do
    blk=$(rpc "${RPCS[$i]}" eth_getBlockByNumber "[\"$HHEX\",false]")
    bh=$(printf '%s' "$blk" | jq -r '.result.hash // "ERR"'); sr=$(printf '%s' "$blk" | jq -r '.result.stateRoot // "ERR"')
    # state digest: every market's order book + open interest + 50 sender balances
    dig=$( {
        for m in $(seq 1 "$MARKETS"); do
            rpc "${RPCS[$i]}" torus_getOrderBook "[\"$(printf '0x%x' "$m")\"]" | jq -cS '.result // .error'
            rpc "${RPCS[$i]}" torus_getOpenInterest "[\"$(printf '0x%x' "$m")\"]" | jq -cS '.result // .error'
        done
        "$BENCH" gen-accounts --offset 60 --count 50 2>/dev/null | awk '{print $2}' | while read -r a; do
            rpc "${RPCS[$i]}" torus_getBalances "[\"$a\"]" | jq -cS '.result // .error'
        done
    } | tee "$OUT/state-digest-val$i.txt" | sha256sum | cut -d' ' -f1)
    matched=$(mval torus_orders_matched_total < "$OUT/metrics-after-val$i.txt")
    placed=$(mval torus_orders_placed_accepted_total < "$OUT/metrics-after-val$i.txt")
    resting=$(mval torus_orders_resting_total < "$OUT/metrics-after-val$i.txt")
    actions=$(mval torus_native_actions_processed_total < "$OUT/metrics-after-val$i.txt")
    lg="$RUN_DIR/val$i.log"
    panics=$(grep -c -E 'panicked|FAIL-STOP|fail-stop|Latching fail-stop|conflicting blocks' "$lg" 2>/dev/null); panics=${panics:-0}
    errors=$(grep -c ' ERROR ' "$lg" 2>/dev/null); errors=${errors:-0}
    printf '{"node":"val%s","height":%s,"cmp_height":%s,"block_hash":"%s","header_state_root":"%s","state_digest":"%s","matched":%s,"placed":%s,"resting":%s,"actions":%s,"panic_or_failstop_lines":%s,"error_lines":%s}\n' \
        "$i" "${HGT[$i]}" "$HCMP" "$bh" "$sr" "$dig" "${matched%.*}" "${placed%.*}" "${resting%.*}" "${actions%.*}" "$panics" "$errors" >> "$OUT/agreement.jsonl"
done
cat "$OUT/agreement.jsonl" >> "$OUT/run.log"

# ingest accounting: actions the mempool evicted as nonce-expired (NONCE_WINDOW_MS=60s —
# a backlog older than 60 s is dropped silently; the bench's "submitted" therefore
# overstates what the chain executed).
EVICTED=$(for i in 0 1 2; do sed -E 's/\x1b\[[0-9;]*m//g' "$RUN_DIR/val$i.log" | grep "evicted nonce-expired" \
    | awk '{for(k=1;k<=NF;k++) if($k ~ /^evicted=/){split($k,a,"="); s+=a[2]}} END{printf "%d ", s+0}'; done)
BENCH_SUBMITTED=$(grep -oE 'Submitted \(load-gen accepted\): [0-9,]+' "$OUT/bench.log" | grep -oE '[0-9,]+$' | tr -d ,)
log "ingest: bench submitted=${BENCH_SUBMITTED:-?} actions; mempool nonce-expired evictions per node: $EVICTED"

# ---------------------------------------------------------------- 9. stop + collect logs
"$WSL/stop-3val.sh" >>"$OUT/run.log" 2>&1
# Body-dissemination / exec-pacing accounting per node (block-cap-raise sweep:
# a raised cap is REJECTED if bodies stop disseminating, whatever matched/s says).
#   manifest = pre-proposal HASH-ONLY manifest pushes (body set over the push floor)
#   exhausted = body fetch exhausted retries / no remaining targets -> sync fallback
#   sync_fallback = any "falling back to sync" (body or justify)
#   da_outbound_fail = native-da / block-data OUTBOUND FAILURE
#   starvation = header-first body starvation
#   pacing = exec-backlog pacing lines (selection caps scaled down / cancels-only)
DISSEM=""
for i in 0 1 2; do
    lg=$(sed -E 's/\x1b\[[0-9;]*m//g' "$RUN_DIR/val$i.log" 2>/dev/null)
    cnt() { printf '%s' "$lg" | grep -c -E "$1"; }
    DISSEM="$DISSEM val$i:manifest=$(cnt 'HASH-ONLY manifest push'),exhausted=$(cnt 'body fetch (exhausted|has no remaining targets)'),sync_fallback=$(cnt 'falling back to sync'),da_outbound_fail=$(cnt '(native-da|block-data) OUTBOUND FAILURE'),starvation=$(cnt 'header-first body starvation'),pacing=$(cnt 'exec-backlog pacing')"
done
DISSEM=${DISSEM# }
log "dissemination/pacing log counts: $DISSEM"
for i in 0 1 2; do
    grep -E ' ERROR | WARN |panicked|FAIL-STOP|fail-stop|book mode|resident|parallel|member cache|commit-lag|S470|manifest|body fetch|OUTBOUND FAILURE|exec-backlog pacing' "$RUN_DIR/val$i.log" | head -400 > "$OUT/val$i.log.excerpt"
    gzip -c "$RUN_DIR/val$i.log" > "$OUT/val$i.log.gz"
done

# ---------------------------------------------------------------- 10. analysis
for i in 0 1 2; do
    { echo "== win60 val$i (all samples)"; awk -f "$TOOLS_DIR/win60.awk" "$OUT/funnel-val$i.csv";
      echo "== phase60 val$i"; awk -f "$TOOLS_DIR/phase60.awk" "$OUT/phase-val$i.csv"; } > "$OUT/analysis-val$i.txt" 2>&1
done

python3 "$TOOLS_DIR/summarize.py" \
    --out "$OUT" --label "$LABEL" --worktree "$WT" --commit "$WT_COMMIT" --dirty "$WT_DIRTY" \
    --markets "$MARKETS" --dur "$DUR" --rate "$RATE" --senders "$SENDERS" \
    --t-bench0 "$T_BENCH0" --t-bench1 "$T_BENCH1" --t-drain "$T_DRAIN" --drained "$DRAINED" \
    --bench-rc "$BENCH_RC" --idle-blks "$IDLE_BLKS" --md5-node "$MD5_SRC" --md5-bench "$MD5_BENCH" \
    --genesis-md5 "$GEN_MD5" --genesis-markets "$GEN_MARKETS" --genesis-accounts "$GEN_ACCTS" \
    --node-env "$NODE_ENV_JSON" --env-digests "$ENV_VERIFY" --extra-env "$EXTRA_ENV" \
    --bench-cmd "${BENCH_CMD[*]}" --pids "${PIDS[*]}" \
    --evicted "$EVICTED" --bench-submitted "${BENCH_SUBMITTED:-0}" \
    --block-cap "${BLOCK_CAP:-}" --dissem "$DISSEM" \
    | tee -a "$OUT/run.log"
rc=${PIPESTATUS[0]}
log "done -> $OUT/summary.json"
exit $rc
