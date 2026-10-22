#!/usr/bin/env bash
# s80 short local cell runner (NOT a queue item). One 300-market, 120 s "R-shape" cell
# (Classic + owner s136 defaults = CL21: running hash + attest, L6.1, L2.1) of one prototype arm, run detached in a bench unit,
# and this script waits for it to finish.
#   cell.sh <arm-dir-name-under-proto> <round-label> [extra EXTRA_ENV words, e.g. TORUS_S80_FOO=1 ...]
# Arms: best = 29bfc705 binary + 0f8eedb3 tools (the 9j2 CL21 bar: 270.3k matched/s, 18.1 blk/s at 120 s) = the BASE for s80;
# h2h = 29bfc705 rebuilt locally (prototype base target); base = main 2d2f3ed9 (250.3k / 17.4, n=1, reference only).
# Prototype cell:  TOOLS=best cell.sh <slug> on-1 TORUS_S80_<FLAG>=1   (its target dir needs release/bench-throughput = best's, md5 fa8fd4bd)
# Parallel callers queue on $P/.cell.lock; builds/microbenches queue on ~/projects/wt/.cargo-heavy.lock (held by the cell unit).
# Arm worktree:  /home/oz/projects/wt/novel-s80/proto/<arm>
# Target dir:    $HOME/.cargo-target-s80-<arm>  (must hold release/torus-node + release/bench-throughput)
# Results:       /home/oz/projects/wt/novel-s80/proto/results/s80-<arm>-<round>/summary.json
# Only one cell may run at a time on this host (fixed ports); the unit holds the build lock.
set -euo pipefail
P=/home/oz/projects/wt/novel-s80/proto
a=${1:?arm}; r=${2:?round}; shift 2
# TOOLS=<arm> runs this arm's binaries with another arm's harness worktree (tools, devnet scripts, genesis):
# prototypes on 29bfc705 use TOOLS=best (0f8eedb3 tools, exactly as the 9j2 reference). Default: the arm's own worktree.
W=$P/${TOOLS:-$a}; L=s80-$a-$r; DUR=120   # owner s80: 120 s max per cell in this workflow
[ -x "$HOME/.cargo-target-s80-$a/release/torus-node" ] || { echo "no node binary for $a" >&2; exit 1; }
[ -x "$HOME/.cargo-target-s80-$a/release/bench-throughput" ] || { echo "no bench-throughput for $a" >&2; exit 1; }
# serialize cells from parallel agents: wait (up to 3 h) for the s80 cell lock, then for any active bench unit to end
exec 9>"$P/.cell.lock"; echo "cell lock: waiting ($(date +%T))"; flock -w 10800 9 || { echo "cell lock timeout" >&2; exit 1; }
echo "cell lock: held ($(date +%T))"
while pgrep -f 'bench-throughput consensus' >/dev/null || systemctl --user list-units --state=active 'bench-*' --no-legend | grep -q .; do sleep 10; done
pkill -x rust-analyzer 2>/dev/null || true   # an LSP indexing during a cell skews it
# genesis cache keyed by the arm's tools base: main-based arms share b10 (2d2f3ed9); the best arm (0f8eedb3 tools + 29bfc705 binary, = 9j2) gets its own
GCK=$(git -C "$W" merge-base HEAD 2d2f3ed9 | cut -c1-8); GC=$P/genesis-cache-$GCK
mkdir -p "$P/results" "$GC"
XE="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_ROCKSDB_STATS=2 TORUS_ROCKSDB_STATS_INTERVAL_SECS=1 TORUS_SHARD_CUSTODY=0 TORUS_CHECKPOINT_INTERVAL_MS=15000 TORUS_SETTLE_STREAMED=1 $*"
cd "$W"
env TARGET_DIR=$HOME/.cargo-target-s80-$a RESULTS_ROOT=$P/results GENESIS_CACHE_DIR=$GC OVERWRITE=1 \
  BLOCK_CAP=400 RETRY_BUSY=1 MAX_IN_FLIGHT=4 OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 ORACLE_PRICE=30000 \
  ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 IN_FLIGHT_TAIL=commit CLOID_SHARE=0 \
  BOOK_WINDOW_COMMIT=1 STATE_HASH_ACTIVATION=1 STATE_HASH_ATTEST=1 \
  "$W/tools/matched-bench/campaign/detach.sh" "$L" "$P/results/$L.log" \
  "$W/tools/matched-bench/run-cell.sh" "$W" "$L" 300 "$DUR" 76000 "$XE"
U="bench-$L"
sleep 20
while systemctl --user is-active --quiet "$U.service"; do sleep 15; done
S=$P/results/$L/summary.json
if [ -f "$S" ]; then
  jq -r '[.label,.validity.verdict,.headline.agreement_verdict,.headline.matched_s_avg,.headline.matched_s_first120,.headline.blk_s_avg,.headline.chain_ms,.headline.engine_ms_per_1k_fills,.phase_by_node.val0.order_age_ms.commit.p50,.phase_by_node.val0.order_age_ms.commit.p99,.phase_by_node.val0.order_age_ms.exec.p99,.proc_cpu_by_node.val0.load.user_ms_per_1k_fills]|@tsv' "$S"
else
  echo "NO SUMMARY for $L; tail of log:"; tail -30 "$P/results/$L.log"; exit 1
fi
