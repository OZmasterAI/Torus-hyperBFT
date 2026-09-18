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
#   MPS=K        bench --markets-per-sender K (LOCALITY shape: each sender gets
#                a fixed set of K markets instead of drawing uniformly over all
#                of them). Unset (default) = flag omitted = today's uniform
#                shape. Recorded as cell.markets_per_sender in summary.json.
#   BLOCK_CAP=N  block-cap-raise sweep bundle: exports the COHERENT set of
#                proposer-local selection caps for an N-action native block
#                (TORUS_NATIVE_TOTAL_BLOCK_CAP=N plus the companion caps that
#                would otherwise bind first — see block_cap_bundle below).
#                Applied after RECORD_ENV and before EXTRA_ENV, so EXTRA_ENV
#                can still override any single knob. DEFAULT 200 since the r3
#                merge (block-cap-raise-sweep winner); since r4 the COMPILED
#                node defaults ARE the cap-200 bundle (BLOCK_CAP=200 exports
#                exactly the compiled values); BLOCK_CAP=100 is the pre-r4
#                cap-100 control.
#   OVERWRITE=1  allow reusing an existing non-empty results dir
#   HEALTH_TIMEOUT (240 s)
#   DRAIN_TIMEOUT  default 180 + 2*MARKETS s (300 markets => 780 s). A 300-market
#                  cell does NOT drain inside 180 s — r6-base-300m-r1 ended with
#                  drained=0 and ~20.8k nonce-expired evictions/node, which then
#                  poisoned the state digest.
#   DIGEST_PAR   per-node parallel RPCs during the state digest (default 8)
#   RPC_TIMEOUT  per-RPC curl timeout in the agreement step (default 60 s)
#   CRASH_KILL_AT_S=N  CRASH GATE (bl3). N seconds after the bench starts, SIGKILL
#                one devnet validator and restart it from the same data dir
#                (tools/matched-bench/crash-kill.sh). Unset = off. Must sit
#                INSIDE the load window (>= 10 s in, >= 30 s of load left):
#                killing during the drain hangs the agreement probe. 40-60 on a
#                120 s cell. The cell then also reports a crash verdict —
#                summary.json `.crash` / `.headline.crash_gate` — which is what
#                gates flipping TORUS_EXEC_PIPELINE on by default: the restarted
#                node must replay from its durable applied-height marker, rewind
#                no more than 2 blocks beyond the exec queue it already had, come
#                back with the flush worker attached, and still AGREE with the
#                two survivors.
#   KILL_NODE    which validator the crash gate kills: val1 (default) or val2.
#                NEVER val0 (it serves the bench RPC and every headline number),
#                and never anything outside this devnet — see the guard in
#                crash-kill.sh.
#   HOTSTUFF_CPUS=a/b/c  pin each node's "hotstuff-algo" thread (the consensus
#                thread, named by the node binary) to CPU list a / b / c (one
#                CPU or a range per node, taskset syntax, e.g. 2/6/10 or
#                2-3/6-7/10-11). Applied with `taskset -pc` right after the
#                node pids are known; a node whose binary does not name the
#                thread is logged as a WARNING and the cell continues unpinned.
#                If the process also runs under a cpuset, the chosen CPU must
#                lie inside that cpuset (not enforced here). Logged in the
#                "node env" line next to the TORUS_* vars.
#   (schedstat)  For every node the harness snapshots
#                /proc/<pid>/task/<tid>/schedstat (on_cpu_ns runqueue_wait_ns
#                timeslices) of the main thread and of the threads named
#                hotstuff-algo / torus-execution / torus-flush-worker at
#                metrics-before, bench end and metrics-after, into
#                $OUT/schedstat.json (summarize.py -> sched_by_node). A
#                thread the binary does not have is recorded as null.
#   TOOLS_FROM_WORKTREE  1 (default) scores the cell with <worktree>/tools/
#                matched-bench/summarize.py, i.e. the CANDIDATE's own summarizer,
#                whichever copy of run-cell.sh was invoked. 0 keeps the old
#                behaviour (this script's own directory). TOOLS_DIR=<dir> pins it
#                explicitly. RUN_CELL_PRINT_PATHS=1 prints the resolution and
#                exits without touching the devnet.
set -uo pipefail

usage() { sed -n '2,84p' "$0"; exit 2; }
[ $# -ge 2 ] || usage

WT=$(cd "$1" && pwd) || { echo "FATAL: worktree '$1' not found" >&2; exit 2; }
LABEL=$2
MARKETS=${3:-10}
DUR=${4:-120}
RATE=${5:-76000}
EXTRA_ENV=${6:-}

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
MAINREPO=$(cd "$SELF_DIR/../.." && pwd)
# A CANDIDATE is scored by ITS OWN summarizer. This script is invoked from the
# integration repo but handed a candidate worktree, so resolving the scoring
# scripts next to the SCRIPT scores a harness candidate with the head's
# summarize.py -- which is how a summarize.py fix can read as a no-op.
# TOOLS_FROM_WORKTREE=0 forces the old behaviour (score every cell with the
# integration repo's tools); an explicit TOOLS_DIR= wins over both.
TOOLS_FROM_WORKTREE=${TOOLS_FROM_WORKTREE:-1}
if [ -n "${TOOLS_DIR:-}" ]; then
    TOOLS_DIR=$(cd "$TOOLS_DIR" && pwd) || { echo "FATAL: TOOLS_DIR not found" >&2; exit 2; }
    TOOLS_FROM_WORKTREE=explicit
elif [ "$TOOLS_FROM_WORKTREE" = 1 ] && [ -f "$WT/tools/matched-bench/summarize.py" ]; then
    TOOLS_DIR="$WT/tools/matched-bench"
else
    [ "$TOOLS_FROM_WORKTREE" = 1 ] && TOOLS_FROM_WORKTREE=0
    TOOLS_DIR="$SELF_DIR"
fi
if [ -n "${RUN_CELL_PRINT_PATHS:-}" ]; then
    printf 'WT=%s\nSELF_DIR=%s\nTOOLS_DIR=%s\nTOOLS_FROM_WORKTREE=%s\nMAINREPO=%s\n' \
        "$WT" "$SELF_DIR" "$TOOLS_DIR" "$TOOLS_FROM_WORKTREE" "$MAINREPO"
    exit 0
fi
TARGET_DIR=${TARGET_DIR:-/home/18c/.cargo-target-matched}
RESULTS_ROOT=${RESULTS_ROOT:-/home/18c/bench-results-matched}
export DATA_ROOT=${DATA_ROOT:-$HOME/torus-wsl-devnet}
SENDERS=${SENDERS:-5000}
CONC=${CONC:-256}
BATCH=${BATCH:-400}
SUBMIT=${SUBMIT:-1}
MPS=${MPS:-}
BAND=${BAND:-5}
CROSS_FRACTION=${CROSS_FRACTION:-0.5}
CANCEL_FRACTION=${CANCEL_FRACTION:-0.05}
RATE_SCHEDULE=${RATE_SCHEDULE:-}
HEALTH_TIMEOUT=${HEALTH_TIMEOUT:-240}
# Drain scales with the market count: the mempool backlog at 300 markets needs
# far longer than 180 s to execute, and a cell that stops draining early is
# digested mid-flight (see step 8).
DRAIN_TIMEOUT=${DRAIN_TIMEOUT:-$(( 180 + 2 * MARKETS ))}
DIGEST_PAR=${DIGEST_PAR:-8}
RPC_TIMEOUT=${RPC_TIMEOUT:-60}
CRASH_KILL_AT_S=${CRASH_KILL_AT_S:-}
KILL_NODE=${KILL_NODE:-val1}
HOTSTUFF_CPUS=${HOTSTUFF_CPUS:-}
SCHED_THREADS="hotstuff-algo torus-execution torus-flush-worker"
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
# TORUS_PARALLEL_BUCKET_HASH: 4 -> 8 after the r5 root-and-save-workers sweep
# (+10.8 % n=3 vs n=3, agreement clean); 8 is ALSO the compiled default now
# (DEFAULT_BUCKET_HASH_THREADS), the explicit export keeps env-only cells on
# OLDER binaries equivalent. EXTRA_ENV='TORUS_PARALLEL_BUCKET_HASH=4' = the
# pre-r5 control; '=1' = serial. Leave TORUS_SAVE_BOOKS_WORKERS at the host
# default (4 is a -34 % cliff, r5).
# TORUS_ROCKSDB_PIPELINED_WRITE=1: s46 n=2 at cap 25, view_ms 132.6-132.9 ->
# 119.2-121.5 (-9 %), db.write avg 7 -> 5.8 ms, matched/s +7-13 %, AGREE. A
# RocksDB write-scheduling option (WAL and memtable writers overlap), no
# durability change. Control = EXTRA_ENV='TORUS_ROCKSDB_PIPELINED_WRITE=0'.
RECORD_ENV=(
    TORUS_ROCKSDB_PIPELINED_WRITE=1
    TORUS_BOOK_ROWS=3
    TORUS_RESIDENT_BOOKS=1
    TORUS_NATIVE_ROOT_CACHE=1
    TORUS_PARALLEL_SETTLE=1
    TORUS_PARALLEL_BUCKET_HASH=8
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
# NOT touched: TORUS_HASH_ONLY_PUSH_THRESHOLD — r4 raised the direct-push body
# floor 4 MB -> 8 MB (TORUS_DIRECT_PUSH_BODY_BYTES) and env.sh sets the
# threshold AT it (8000000), so cap-200/300 body sets (~5.6-8 MB at bs400) stay
# on the direct push path; only larger sets go out as a HASH manifest and are
# PULLED (the S387-hardened path). The dissemination counters below (manifest
# pushes / body-fetch exhaustion / sync fallbacks) record which path a cell took;
# EXTRA_ENV='TORUS_DIRECT_PUSH_BODY_BYTES=4194304' reproduces the pre-r4 clamp.
block_cap_bundle() { # $1=N -> prints K=V lines
    local n=$1 orders bytes cache
    orders=$(( n * BATCH * 5 / 4 )); [ "$orders" -lt 50000 ] && orders=50000
    cache=$(( 64 * n * 5 / 2 )); [ "$cache" -lt 16384 ] && cache=16384
    bytes=$(( n * BATCH * 150 )); [ "$bytes" -lt 6000000 ] && bytes=6000000; [ "$bytes" -gt 12000000 ] && bytes=12000000
    printf 'TORUS_NATIVE_TOTAL_BLOCK_CAP=%s\nTORUS_NATIVE_ORDERS_PER_BLOCK_CAP=%s\nTORUS_VERIFIED_SENDER_CACHE_CAP=%s\nTORUS_NATIVE_BLOCK_BYTES_CAP=%s\n' \
        "$n" "$orders" "$cache" "$bytes"
}
# r3 merge: the harness defaults to the block-cap-raise-sweep winner (cap 200).
# r4: the compiled node defaults are now that same bundle (rate_limit.rs
# NATIVE_TOTAL_BLOCK_CAP 200 / ORDERS_PER_BLOCK 100_000 / BLOCK_BYTES 12 MB /
# derived trust-cache 32_000 — unit-pinned against this function's N=200 output
# in rate_limit.rs `compiled_defaults_reproduce_the_r3_cap200_bundle`), so the
# BLOCK_CAP=200 export is a no-op on r4+ binaries and keeps OLDER binaries
# equivalent; BLOCK_CAP=100 = the pre-r4 control cell.
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

# ---- thread helpers (HOTSTUFF_CPUS pinning + schedstat snapshots) -----------
find_tid() { # $1=pid $2=thread comm -> first matching tid on stdout, rc 1 if none
    local t
    for t in /proc/"$1"/task/*/; do
        [ "$(cat "$t/comm" 2>/dev/null)" = "$2" ] && { basename "$t"; return 0; }
    done
    return 1
}
# schedstat_snapshot <phase> <pid>...: appends one
# "phase node thread tid on_cpu_ns wait_ns slices" line per (node, thread) to
# $OUT/schedstat.raw. thread "main" = the pid itself; a thread the binary does
# not name gets tid=- and no counters (=> null in schedstat.json).
schedstat_snapshot() {
    local phase=$1 i=0 p t tid st
    shift
    for p in "$@"; do
        for t in main $SCHED_THREADS; do
            if [ "$t" = main ]; then tid=$p; else tid=$(find_tid "$p" "$t") || tid=""; fi
            if [ -n "$tid" ] && read -r st < /proc/"$p"/task/"$tid"/schedstat 2>/dev/null; then
                printf '%s val%s %s %s %s\n' "$phase" "$i" "$t" "$tid" "$st"
            else
                printf '%s val%s %s -\n' "$phase" "$i" "$t"
            fi
        done >> "$OUT/schedstat.raw"
        i=$((i+1))
    done
}
# schedstat_json <raw> <out.json>:
# {"val0": {"main": {"tid": N, "before": [on_cpu_ns, wait_ns, slices], "bench_end": [...], "after": [...]},
#           "hotstuff-algo": null | {...}, ...}, ...}
schedstat_json() {
    python3 - "$1" "$2" <<'PYJ'
import json, sys
out = {}
for line in open(sys.argv[1]):
    f = line.split()
    if len(f) < 4:
        continue
    phase, node, thread, tid = f[:4]
    d = out.setdefault(node, {})
    if tid == "-" or len(f) < 7:
        d.setdefault(thread, None)
        continue
    e = d.get(thread) or {"tid": int(tid)}
    e[phase] = [int(x) for x in f[4:7]]
    d[thread] = e
json.dump(out, open(sys.argv[2], "w"), indent=1)
PYJ
}

# ---------------------------------------------------------------- pre-flight
[ -x "$SRC_NODE" ] || { echo "FATAL: missing $SRC_NODE" >&2; exit 1; }
[ -x "$BENCH" ]    || { echo "FATAL: missing $BENCH" >&2; exit 1; }
[ -x "$WSL/launch-3val.sh" ] || { echo "FATAL: $WSL/launch-3val.sh missing" >&2; exit 1; }
for t in jq curl python3 md5sum awk; do command -v $t >/dev/null || { echo "FATAL: need $t" >&2; exit 1; }; done
[[ "$MARKETS" =~ ^[0-9]+$ && "$DUR" =~ ^[0-9]+$ && "$RATE" =~ ^[0-9]+$ ]] || usage
[ -z "$MPS" ] || [[ "$MPS" =~ ^[0-9]+$ ]] || { echo "FATAL: MPS must be an integer" >&2; exit 2; }
WORKLOAD_JSON=$(python3 "$SELF_DIR/workload.py" "$BAND" "$CROSS_FRACTION" "$CANCEL_FRACTION" "$RATE_SCHEDULE" "$DUR") || exit 2
[ -x "$TOOLS_DIR/digest-node.sh" ] || { echo "FATAL: $TOOLS_DIR/digest-node.sh missing" >&2; exit 1; }

# ---- crash gate (bl3) pre-flight: validated HERE, before anything is launched,
# so a bad kill window can never be discovered mid-cell.
KILL_IDX=""
if [ -n "$CRASH_KILL_AT_S" ]; then
    [ -x "$TOOLS_DIR/crash-kill.sh" ] || { echo "FATAL: $TOOLS_DIR/crash-kill.sh missing" >&2; exit 1; }
    # shellcheck source=/dev/null
    CRASH_KILL_LIB=1 source "$TOOLS_DIR/crash-kill.sh"
    case "$KILL_NODE" in
        val1) KILL_IDX=1 ;;
        val2) KILL_IDX=2 ;;
        *) echo "FATAL: KILL_NODE must be val1 or val2 (never val0 — it serves the bench RPC and every headline number)" >&2; exit 2 ;;
    esac
    crash_kill_at_ok "$CRASH_KILL_AT_S" "$DUR" || {
        echo "FATAL: CRASH_KILL_AT_S=$CRASH_KILL_AT_S is not inside the load window of a ${DUR}s cell (need >= 10 and <= $((DUR-30)); killing during the drain hangs the agreement probe)" >&2; exit 2; }
fi

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
printf '%s\n' "$WORKLOAD_JSON" > "$OUT/workload.json"
: > "$OUT/run.log"

SAMPLER_PID=""; CPU_PID=""; BENCH_PID=""; CRASH_PID=""
stop_sampler() {
    [ -n "$SAMPLER_PID" ] || return 0
    local pid="$SAMPLER_PID" rc=0
    kill "$pid" 2>/dev/null || true
    # The Python sampler cancels, terminates and reaps every in-flight curl
    # before exiting. Reap it too, including during an interrupted cell.
    wait "$pid" || rc=$?
    SAMPLER_PID=""
    return "$rc"
}
finish_fail() {
    stop_sampler || true
    [ -n "$CPU_PID" ] && kill "$CPU_PID" 2>/dev/null
    [ -n "$BENCH_PID" ] && kill "$BENCH_PID" 2>/dev/null
    [ -n "$CRASH_PID" ] && kill "$CRASH_PID" 2>/dev/null
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

log "cell=$LABEL worktree=$WT markets=$MARKETS dur=${DUR}s rate=$RATE senders=$SENDERS block_cap='${BLOCK_CAP:-unset}' mps='${MPS:-unset}' extra_env='$EXTRA_ENV'"
log "drain_timeout=${DRAIN_TIMEOUT}s digest_par=$DIGEST_PAR rpc_timeout=${RPC_TIMEOUT}s"
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
# HOTSTUFF_CPUS is a HARNESS knob (taskset, not read by the node) but is logged
# alongside the node env so a pinned cell is recognisable from its summary.
# Any TORUS_* var in EXTRA_ENV (e.g. TORUS_ROCKSDB_STATS=2) is already covered.
[ -n "$HOTSTUFF_CPUS" ] && export HOTSTUFF_CPUS
NODE_ENV_JSON=$(env | grep -E '^(TORUS_|HOTSTUFF_CPUS=)' | sort | python3 -c 'import sys,json;print(json.dumps(dict(l.rstrip("\n").split("=",1) for l in sys.stdin)))')
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
# ---- HOTSTUFF_CPUS: pin each node's consensus thread -----------------------
if [ -n "$HOTSTUFF_CPUS" ]; then
    IFS=/ read -r -a HS_CPU <<< "$HOTSTUFF_CPUS"
    [ "${#HS_CPU[@]}" = 3 ] || die "HOTSTUFF_CPUS must be a/b/c (one CPU list per node), got '$HOTSTUFF_CPUS'"
    command -v taskset >/dev/null || die "HOTSTUFF_CPUS set but taskset not found"
    for i in 0 1 2; do
        p=${PIDS[$i]}
        tid=""
        for _try in $(seq 1 20); do tid=$(find_tid "$p" hotstuff-algo) && break; sleep 0.5; done
        if [ -n "$tid" ]; then
            if taskset -pc "${HS_CPU[$i]}" "$tid" >>"$OUT/run.log" 2>&1; then
                log "val$i pid=$p hotstuff-algo tid=$tid Cpus_allowed_list=$(awk '/^Cpus_allowed_list/{print $2}' /proc/$p/task/$tid/status) (HOTSTUFF_CPUS='$HOTSTUFF_CPUS')"
            else
                log "WARNING: val$i pid=$p taskset -pc ${HS_CPU[$i]} $tid FAILED — hotstuff-algo left unpinned"
            fi
        else
            log "WARNING: val$i pid=$p has no thread named hotstuff-algo (binary predates the naming, or not spawned within 10 s) — left unpinned (HOTSTUFF_CPUS='$HOTSTUFF_CPUS')"
        fi
    done
fi
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
# Wide CSV: independent per-node 1 Hz attempts (bounded requests, no catch-up),
# timestamped at response completion; per-node CSVs retain the exact
# layouts of the RE-PROOF5 scrape.sh / scrape-phase.sh so win60.awk / phase60.awk
# run unchanged.
FUNNEL_COLS="torus_native_actions_processed_total torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_orders_rejected_margin_total torus_orders_rejected_book_total torus_orders_rejected_cancelled_total torus_orders_rejected_other_total torus_orders_self_trade_cancels_total torus_orders_cancelled_partial_fill_total torus_blocks_committed_total torus_block_height torus_exec_queue_depth"
PHASE_COLS="torus_blocks_committed_total torus_block_height torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_exec_resting_orders torus_exec_load_books_seconds_sum torus_exec_load_books_seconds_count torus_exec_root_seconds_sum torus_exec_root_seconds_count torus_exec_state_write_seconds_sum torus_exec_state_write_seconds_count torus_exec_state_write_build_seconds_sum torus_exec_state_write_build_seconds_count torus_exec_state_write_db_seconds_sum torus_exec_state_write_db_seconds_count torus_exec_state_write_batch_bytes_sum torus_exec_state_write_batch_bytes_count torus_exec_evm_resync_seconds_sum torus_exec_evm_resync_seconds_count torus_exec_flush_seconds_sum torus_exec_flush_seconds_count torus_exec_root_dirty_buckets_sum torus_exec_root_dirty_buckets_count torus_exec_queue_depth torus_exec_root_bucket_scans_total torus_member_cache_hits_total torus_member_cache_misses_total torus_member_cache_evictions_total torus_member_cache_resident_buckets"
WIDE_COLS="torus_blocks_committed_total torus_block_height torus_native_actions_processed_total torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_exec_resting_orders torus_orders_rejected_margin_total torus_orders_rejected_book_total torus_orders_rejected_cancelled_total torus_orders_rejected_other_total torus_exec_queue_depth torus_mempool_native_size torus_exec_block_seconds_sum torus_exec_block_seconds_count torus_exec_engine_seconds_count torus_exec_verify_seconds_count torus_exec_flush_seconds_count torus_exec_evm_seconds_sum torus_exec_verify_seconds_sum torus_exec_replay_guard_seconds_sum torus_exec_load_books_seconds_sum torus_exec_engine_seconds_sum torus_exec_phase_margin_seconds_sum torus_exec_phase_match_seconds_sum torus_exec_phase_settle_seconds_sum torus_exec_phase1_actions_seconds_sum torus_exec_phase1_actions_seconds_count torus_exec_settle_pass_a_seconds_sum torus_exec_settle_pass_b_seconds_sum torus_exec_cache_flush_seconds_sum torus_exec_post_engine_tail_seconds_sum torus_exec_engine_untimed_seconds_sum torus_exec_engine_untimed_seconds_count torus_exec_save_books_seconds_sum torus_exec_flush_seconds_sum torus_exec_root_seconds_sum torus_exec_state_write_seconds_sum torus_exec_state_write_build_seconds_sum torus_exec_state_write_build_seconds_count torus_exec_state_write_db_seconds_sum torus_exec_state_write_db_seconds_count torus_exec_state_write_batch_bytes_sum torus_exec_state_write_batch_bytes_count torus_exec_evm_resync_seconds_sum torus_exec_body_persist_seconds_sum torus_exec_root_dirty_buckets_sum torus_exec_root_dirty_buckets_count torus_exec_root_bucket_scans_total torus_member_cache_evictions_total torus_commit_interval_seconds_sum torus_commit_interval_seconds_count torus_exec_chain_seconds_sum torus_exec_chain_seconds_count torus_exec_handoff_wait_seconds_sum torus_exec_handoff_wait_seconds_count torus_flush_worker_seconds_sum torus_flush_worker_seconds_count torus_flush_worker_depth torus_exec_save_books_seconds_count torus_exec_save_books_drain_seconds_sum torus_exec_save_books_drain_seconds_count torus_exec_save_books_write_seconds_sum torus_exec_save_books_write_seconds_count torus_exec_native_blocks_total torus_consensus_timeout_total_total torus_consensus_view torus_block_transactions_count_sum torus_block_transactions_count_count torus_native_gossip_published_actions_total torus_native_gossip_dropped_full_total torus_rocksdb_memtable_bytes torus_rocksdb_l0_files torus_rocksdb_pending_compaction_bytes torus_db_size_bytes torus_exec_body_persist_write_seconds_sum torus_exec_body_persist_write_seconds_count torus_commit_persist_seconds_sum torus_commit_persist_seconds_count torus_commit_body_encode_seconds_sum torus_commit_persist_write_seconds_sum torus_trade_writer_queued_batches torus_rocksdb_memtable_bytes_all torus_rocksdb_immutable_memtables_all torus_rocksdb_l0_files_max torus_rocksdb_pending_compaction_bytes_all torus_rocksdb_delayed_write_rate torus_rocksdb_write_stopped torus_rocksdb_running_compactions torus_rocksdb_running_flushes torus_rocksdb_stall_micros torus_rocksdb_write_self torus_rocksdb_write_other torus_rocksdb_bytes_written torus_rocksdb_wal_bytes torus_rocksdb_flush_write_bytes torus_rocksdb_compact_read_bytes torus_rocksdb_compact_write_bytes torus_rocksdb_compaction_cpu_micros torus_rocksdb_db_write_count torus_rocksdb_db_write_sum_micros torus_rocksdb_db_write_p99_micros torus_rocksdb_db_write_max_micros torus_rocksdb_write_stall_count torus_rocksdb_write_stall_sum_micros torus_rocksdb_write_stall_p99_micros torus_rocksdb_write_stall_max_micros torus_rocksdb_flush_count torus_rocksdb_flush_sum_micros torus_rocksdb_compaction_count torus_rocksdb_compaction_sum_micros torus_view_duration_seconds_sum torus_view_duration_seconds_count torus_view_propose_delay_seconds_sum torus_view_propose_delay_seconds_count torus_view_propose_build_seconds_sum torus_view_propose_build_seconds_count torus_view_propose_finalize_seconds_sum torus_view_propose_finalize_seconds_count torus_view_qc_collect_seconds_sum torus_view_qc_collect_seconds_count torus_view_proposal_arrival_seconds_sum torus_view_proposal_arrival_seconds_count torus_view_insert_persist_seconds_sum torus_view_insert_persist_seconds_count torus_view_vote_delay_seconds_sum torus_view_vote_delay_seconds_count torus_block_build_seconds_sum torus_block_build_seconds_count torus_validate_block_seconds_sum torus_validate_block_seconds_count torus_validate_block_decode_seconds_sum torus_validate_block_decode_seconds_count torus_validate_block_da_reconstruct_seconds_sum torus_validate_block_da_reconstruct_seconds_count torus_validate_block_attest_seconds_sum torus_validate_block_attest_seconds_count torus_validate_block_custody_seconds_sum torus_validate_block_custody_seconds_count torus_on_committed_block_seconds_sum torus_on_committed_block_seconds_count torus_mempool_remove_committed_seconds_sum torus_mempool_remove_committed_seconds_count"

# bl1 exec-chain-sub-100-attribution: histogram BUCKET series in LONG format
# (ts,node,metric,le,count), read out of the SAME scrape as the wide row above
# (no extra HTTP). summarize.py turns these into commit-cadence p50/p95 and
# chain / hand-off percentiles; a cell run by an older harness simply has no
# buckets.csv and gets None for every percentile.
BUCKET_METRICS="torus_commit_interval_seconds_bucket torus_exec_chain_seconds_bucket torus_exec_handoff_wait_seconds_bucket torus_flush_worker_seconds_bucket"
WIDE_COLS="$WIDE_COLS scrape_valid"
echo "ts,node,$(echo $WIDE_COLS | tr ' ' ',')" > "$OUT/sampler.csv"
echo "ts,node,metric,le,count" > "$OUT/buckets.csv"
for i in 0 1 2; do
    echo "ts,actions_processed,placed_accepted,matched,resting,rej_margin,rej_book,rej_cancelled,rej_other,self_trade_cancels,cancelled_partial_fill,blocks_committed,block_height,exec_queue_depth" > "$OUT/funnel-val$i.csv"
    echo "ts,committed,height,placed,matched,resting,exec_resting,lb_s,lb_c,root_s,root_c,sw_s,sw_c,evm_s,evm_c,fl_s,fl_c,db_s,db_c,execq,bscan,mc_hit,mc_miss,mc_evict,mc_resident" > "$OUT/phase-val$i.csv"
done
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
: > "$OUT/schedstat.raw"; schedstat_snapshot before "${PIDS[@]}"
# This collector belongs to the runner; scoring scripts still come from
# TOOLS_DIR. Keeping it beside run-cell.sh also supports older node worktrees.
python3 "$SELF_DIR/sample_metrics.py" --out "$OUT" \
    --wide "$WIDE_COLS" --funnel "$FUNNEL_COLS" --phase "$PHASE_COLS" --buckets "$BUCKET_METRICS" \
    "http://127.0.0.1:${METS[0]}/metrics" "http://127.0.0.1:${METS[1]}/metrics" "http://127.0.0.1:${METS[2]}/metrics" \
    >"$OUT/sampler.log" 2>&1 & SAMPLER_PID=$!
sleep 3
kill -0 "$SAMPLER_PID" 2>/dev/null || die "metrics sampler exited (see sampler.log)"

# ---------------------------------------------------------------- 6. bench
BENCH_CMD=("$BENCH" consensus --rpc-urls "${RPCS[0]}" --econ --senders "$SENDERS" --sender-offset 60 \
    --markets "$MARKETS" --batch-size "$BATCH" --submit-batch "$SUBMIT" --format bin --concurrency "$CONC" \
    --duration "$DUR" --target-margin 1500 --cross-fraction "$CROSS_FRACTION" --cancel-fraction "$CANCEL_FRACTION" --band "$BAND" --rate-total "$RATE")
[ -z "$RATE_SCHEDULE" ] || BENCH_CMD+=(--rate-schedule "$RATE_SCHEDULE")
# LOCALITY shape (unset = flag omitted = uniform draw over 1..=MARKETS, i.e. the
# shape every campaign cell so far used). Needs a bench-throughput built at or
# after cand/r6-harness-300m-digest-and-parity.
[ -n "$MPS" ] && BENCH_CMD+=(--markets-per-sender "$MPS")
log "bench: ${BENCH_CMD[*]}"
T_BENCH0=$(date +%s)
"${BENCH_CMD[@]}" > "$OUT/bench.log" 2>&1 & BENCH_PID=$!
cpusampler & CPU_PID=$!
# bl3 crash gate: SIGKILL + restart one validator CRASH_KILL_AT_S into the load
# window. Off unless CRASH_KILL_AT_S is set; the target guard lives in
# crash-kill.sh and refuses anything but this devnet's val1/val2.
CRASH_RC=""
if [ -n "$CRASH_KILL_AT_S" ]; then
    log "crash gate ARMED: SIGKILL $KILL_NODE at bench+${CRASH_KILL_AT_S}s, restart from the same data dir"
    ( sleep "$CRASH_KILL_AT_S"; "$TOOLS_DIR/crash-kill.sh" "$WT" "$KILL_IDX" "$OUT" ) >>"$OUT/run.log" 2>&1 &
    CRASH_PID=$!
fi
wait "$BENCH_PID"; BENCH_RC=$?
if [ -n "$CRASH_PID" ]; then
    # rc is informational: bash may already have reaped the sub-shell, and a
    # spurious 127 from `wait` must not throw away a good cell. The ARTIFACT is
    # the truth — crash-kill.sh writes it last, after the restart succeeded.
    wait "$CRASH_PID"; CRASH_RC=$?; CRASH_PID=""
    log "crash gate exited rc=$CRASH_RC"
fi
if [ -n "$CRASH_KILL_AT_S" ] && [ ! -s "$OUT/crash-kill.json" ]; then
    die "crash gate did not complete (no crash-kill.json) — the node was not killed+restarted, see run.log"
fi
T_BENCH1=$(date +%s)
BENCH_PID=""
# a SIGKILLed+restarted node has a new pid: re-read so the snapshot follows the live process
mapfile -t PIDS_NOW < "$RUN_DIR/pids"; schedstat_snapshot bench_end "${PIDS_NOW[@]}"
log "bench exited rc=$BENCH_RC after $((T_BENCH1 - T_BENCH0))s"

# ---------------------------------------------------------------- 7. drain
# Counter quiescence alone also describes a wedged chain. Require complete
# scrapes, quiet native counters, no pending native/flush work, and commit
# progress on every validator during the quiet interval. Empty blocks may leave
# the execution queue at 1-2. health.py retains all drain observations.
QUIET_S=${QUIET_S:-10}
log "draining (quiet counters, no pending work, per-node commit progress for ${QUIET_S}s; timeout ${DRAIN_TIMEOUT}s)"
DRAINED=0
if python3 "$TOOLS_DIR/health.py" drain --out "$OUT" --timeout "$DRAIN_TIMEOUT" --quiet "$QUIET_S" \
    --urls "http://127.0.0.1:${METS[0]}/metrics" "http://127.0.0.1:${METS[1]}/metrics" "http://127.0.0.1:${METS[2]}/metrics" \
    >>"$OUT/run.log" 2>&1; then DRAINED=1; fi
T_DRAIN=$(date +%s)
log "drained=$DRAINED after $((T_DRAIN - T_BENCH1))s (evidence: drain.json and drain-samples.jsonl)"
sleep 2
stop_sampler || die "metrics sampler failed (see sampler.log)"
kill "$CPU_PID" 2>/dev/null; CPU_PID=""

# ---------------------------------------------------------------- 8. after-snapshots + agreement
for i in 0 1 2; do scrape_one "${METS[$i]}" > "$OUT/metrics-after-val$i.txt"; done
mapfile -t PIDS_NOW < "$RUN_DIR/pids"; schedstat_snapshot after "${PIDS_NOW[@]}"
schedstat_json "$OUT/schedstat.raw" "$OUT/schedstat.json" || log "WARNING: schedstat.json not written"
rpc() { # $1=url $2=method $3=params-json
    curl -s -m "$RPC_TIMEOUT" -H 'content-type: application/json' "$1" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}"
}
declare -a HGT
for i in 0 1 2; do HGT[$i]=$(mval torus_block_height < "$OUT/metrics-after-val$i.txt"); HGT[$i]=${HGT[$i]%.*}; done
HMIN=$(printf '%s\n' "${HGT[@]}" | sort -n | head -1); HMAX=$(printf '%s\n' "${HGT[@]}" | sort -n | tail -1)
HCMP=$(( HMIN - 5 )); HHEX=$(printf '0x%x' "$HCMP")
log "heights after drain: ${HGT[*]} (spread $((HMAX-HMIN))); comparing block $HCMP on all nodes"

# --- state digest -----------------------------------------------------------
# The digest is the ONLY determinism check that sees executed native state (the
# header stateRoot is 0x0 on this branch). It is therefore only worth anything
# when all three nodes are digested over the SAME state:
#   * the per-market RPCs run DIGEST_PAR-way parallel inside each node
#     (digest-node.sh, order-stable), and
#   * the three nodes are digested CONCURRENTLY, in one window, and
#   * the funnel counters are snapshotted either side of that window: if they
#     moved, the digest is flagged NOT quiescent and summarize.py reports
#     DIGEST_UNVERIFIED instead of pretending the fleet forked.
# At 300 markets the old serial loop was ~4 min/node (~12 min end to end) —
# val0 and val2 were digested minutes apart (r6-base-300m-r1).
"$BENCH" gen-accounts --offset 60 --count 50 2>/dev/null | awk '{print $2}' > "$OUT/digest-accounts.txt"
DIG_ACCTS=$(grep -c . "$OUT/digest-accounts.txt")
funnel_snapshot() {
    for i in 0 1 2; do
        scrape_one "${METS[$i]}" | extract "torus_orders_placed_accepted_total torus_orders_matched_total torus_native_actions_processed_total torus_orders_resting_total"
    done
}
declare -a DHGT DIGSHA DIGSECS
Q_BEFORE=$(funnel_snapshot)
for i in 0 1 2; do
    h=$(scrape_one "${METS[$i]}" | mval torus_block_height); DHGT[$i]=${h%.*}
done
log "state digest: $MARKETS markets + $DIG_ACCTS accounts, 3 nodes CONCURRENTLY (par=$DIGEST_PAR/node, rpc timeout ${RPC_TIMEOUT}s), heights ${DHGT[*]}"
TDIG0=$(date +%s)
for i in 0 1 2; do
    "$TOOLS_DIR/digest-node.sh" "${RPCS[$i]}" "$MARKETS" "$OUT/digest-accounts.txt" \
        "$OUT/state-digest-val$i.txt" "$DIGEST_PAR" "$RPC_TIMEOUT" > "$OUT/digest-val$i.out" 2>>"$OUT/run.log" &
done
wait
TDIG1=$(date +%s)
Q_AFTER=$(funnel_snapshot)
if [ "$Q_BEFORE" = "$Q_AFTER" ]; then DIGEST_QUIESCENT=1; else DIGEST_QUIESCENT=0; fi
for i in 0 1 2; do
    sha=""; secs=""; read -r sha secs < "$OUT/digest-val$i.out" || true
    DIGSHA[$i]=${sha:-ERR}; DIGSECS[$i]=${secs:-0}
done
log "state digest done in $((TDIG1 - TDIG0))s wall (per node: ${DIGSECS[*]} s), quiescent=$DIGEST_QUIESCENT"
[ "$DIGEST_QUIESCENT" = 1 ] || log "WARNING: funnel counters MOVED during the digest — digest is NOT a determinism proof for this cell"

: > "$OUT/agreement.jsonl"
for i in 0 1 2; do
    blk=$(rpc "${RPCS[$i]}" eth_getBlockByNumber "[\"$HHEX\",false]")
    bh=$(printf '%s' "$blk" | jq -r '.result.hash // "ERR"'); sr=$(printf '%s' "$blk" | jq -r '.result.stateRoot // "ERR"')
    dig=${DIGSHA[$i]}
    matched=$(mval torus_orders_matched_total < "$OUT/metrics-after-val$i.txt")
    placed=$(mval torus_orders_placed_accepted_total < "$OUT/metrics-after-val$i.txt")
    resting=$(mval torus_orders_resting_total < "$OUT/metrics-after-val$i.txt")
    actions=$(mval torus_native_actions_processed_total < "$OUT/metrics-after-val$i.txt")
    # r3 resident-books-stale-rebuild: full O(resting depth) reloads of the rank8
    # holder. Expect exactly 1 per process (the cold start); anything more is a
    # mid-run "resident books stale" stall — check val$i.log.excerpt for the reason.
    rebuilds=$(mval torus_exec_resident_rebuilds_total < "$OUT/metrics-after-val$i.txt")
    lg="$RUN_DIR/val$i.log"
    panics=$(grep -c -E 'panicked|FAIL-STOP|fail-stop|Latching fail-stop|conflicting blocks' "$lg" 2>/dev/null); panics=${panics:-0}
    errors=$(grep -c ' ERROR ' "$lg" 2>/dev/null); errors=${errors:-0}
    printf '{"node":"val%s","height":%s,"cmp_height":%s,"block_hash":"%s","header_state_root":"%s","state_digest":"%s","digest_height":%s,"digest_seconds":%s,"matched":%s,"placed":%s,"resting":%s,"actions":%s,"resident_rebuilds":%s,"panic_or_failstop_lines":%s,"error_lines":%s}\n' \
        "$i" "${HGT[$i]}" "$HCMP" "$bh" "$sr" "$dig" "${DHGT[$i]}" "${DIGSECS[$i]}" "${matched%.*}" "${placed%.*}" "${resting%.*}" "${actions%.*}" "${rebuilds%.*}" "$panics" "$errors" >> "$OUT/agreement.jsonl"
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
#   body_push = pre-proposal FULL-BODY pushes (r4 log line; body set under the floor)
#   body_push_max_bytes = largest full-body push payload seen (vs the 8 MB floor)
#   exhausted = body fetch exhausted retries / no remaining targets -> sync fallback
#   sync_fallback = any "falling back to sync" (body or justify)
#   da_outbound_fail = native-da / block-data OUTBOUND FAILURE
#   starvation = header-first body starvation
#   pacing = exec-backlog pacing lines (selection caps scaled down / cancels-only)
DISSEM=$(python3 "$TOOLS_DIR/collect_logs.py" --logs "$RUN_DIR/val0.log" "$RUN_DIR/val1.log" "$RUN_DIR/val2.log" \
    --out "$OUT/log-summary.json") || { log "log collection failed"; exit 1; }
log "dissemination/pacing log counts: $DISSEM"
for i in 0 1 2; do
    grep -E ' ERROR | WARN |panicked|FAIL-STOP|fail-stop|book mode|resident|parallel|member cache|commit-lag|S470|manifest|FULL-BODY push|native block-selection caps|body fetch|OUTBOUND FAILURE|exec-backlog pacing' "$RUN_DIR/val$i.log" | head -400 > "$OUT/val$i.log.excerpt"
    gzip -c "$RUN_DIR/val$i.log" > "$OUT/val$i.log.gz"
done

# ---- crash gate: scan the killed node's log TAIL — everything it wrote AFTER
# the SIGKILL (crash-kill.sh restarted it with the log appended and recorded the
# byte offset) — and merge that into crash.json, which summarize.py turns into
# the PASS/FAIL verdict.
if [ -n "$CRASH_KILL_AT_S" ] && [ -s "$OUT/crash-kill.json" ]; then
    OFFB=$(jq -r '.pre_kill.log_bytes // 0' "$OUT/crash-kill.json")
    TAILF="$OUT/crash-restart-tail.log"
    tail -c "+$(( OFFB + 1 ))" "$RUN_DIR/val$KILL_IDX.log" | sed -E 's/\x1b\[[0-9;]*m//g' > "$TAILF"
    log "crash gate: scanning val$KILL_IDX log tail from byte $OFFB ($(wc -l < "$TAILF") lines)"
    "$TOOLS_DIR/crash-kill.sh" scan "$TAILF" "$OUT/crash-kill.json" "$OUT/crash.json" "$CRASH_KILL_AT_S" \
        | tee -a "$OUT/run.log" || log "WARNING: crash gate scan failed"
fi

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
    --drain-timeout "$DRAIN_TIMEOUT" --markets-per-sender "${MPS:-}" \
    --digest-quiescent "$DIGEST_QUIESCENT" --digest-secs "${DIGSECS[*]}" --digest-heights "${DHGT[*]}" \
    | tee -a "$OUT/run.log"
rc=${PIPESTATUS[0]}
log "done -> $OUT/summary.json"
[ "$rc" = 0 ] || exit "$rc"
# A successful report write is not benchmark acceptance. Preserve artifacts and
# return nonzero for stalled, undrained, inconsistent or unverified cells.
python3 "$TOOLS_DIR/health.py" accept "$OUT/summary.json"
