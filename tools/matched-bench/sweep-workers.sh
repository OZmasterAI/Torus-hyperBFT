#!/usr/bin/env bash
# sweep-workers.sh — r5 root-and-save-workers env sweep, run SERIALLY through
# run-cell.sh (never two cells at once; builds nothing).
#
#   sweep-workers.sh <worktree> <prefix> [DUR=120] [REPS=2] [MARKETS=10] [RATE=76000]
#
# Cells (all node-local, byte-neutral by construction — cache on/off and ANY
# worker count produce identical roots/state; the 3-validator agreement + state
# digest check in run-cell.sh is still mandatory per cell):
#   ctl        record env (TORUS_PARALLEL_BUCKET_HASH=4, MEMBER_CACHE_MB=256,
#              TORUS_SAVE_BOOKS_WORKERS unset = host parallelism, effectively
#              min(host, dirty books) = 10 on a 10-market cell)
#   pbh8       TORUS_PARALLEL_BUCKET_HASH=8
#   pbh12      TORUS_PARALLEL_BUCKET_HASH=12
#   pbh8-mc512 TORUS_PARALLEL_BUCKET_HASH=8 TORUS_BUCKET_MEMBER_CACHE_MB=512
#   sbw4       TORUS_SAVE_BOOKS_WORKERS=4
#   sbw8       TORUS_SAVE_BOOKS_WORKERS=8
# Override the list with CELLS='name:K=V K=V|name2:...' (space-separated env
# inside a cell, '|' between cells; a cell with no '=' is the control).
# Best-combo cell: run it as a second invocation once the singles are read, e.g.
#   CELLS='combo:TORUS_PARALLEL_BUCKET_HASH=8 TORUS_SAVE_BOOKS_WORKERS=4' sweep-workers.sh ...
#
# Every cell publishes torus_exec_root_bucket_hash_workers /
# torus_exec_save_books_workers, so summary.json (phase_by_node.val0.workers)
# PROVES the worker count that engaged — the env value alone is not the answer
# (both pools are capped by dirty buckets / dirty books per block).
#
# Results: $RESULTS_ROOT/<prefix>-<cell>-r<N>/summary.json; compare with
#   tools/matched-bench/compare.py $RESULTS_ROOT/<prefix>-*
set -uo pipefail
[ $# -ge 2 ] || { sed -n '2,29p' "$0"; exit 2; }
WT=$1; PREFIX=$2; DUR=${3:-120}; REPS=${4:-2}; MARKETS=${5:-10}; RATE=${6:-76000}
TOOLS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
RESULTS_ROOT=${RESULTS_ROOT:-/home/18c/bench-results-matched}
CELLS=${CELLS:-'ctl:|pbh8:TORUS_PARALLEL_BUCKET_HASH=8|pbh12:TORUS_PARALLEL_BUCKET_HASH=12|pbh8-mc512:TORUS_PARALLEL_BUCKET_HASH=8 TORUS_BUCKET_MEMBER_CACHE_MB=512|sbw4:TORUS_SAVE_BOOKS_WORKERS=4|sbw8:TORUS_SAVE_BOOKS_WORKERS=8'}

LOG="$RESULTS_ROOT/$PREFIX-sweep.log"
mkdir -p "$RESULTS_ROOT"
say() { printf '[%s] %s\n' "$(date +%FT%T)" "$*" | tee -a "$LOG"; }
say "sweep prefix=$PREFIX worktree=$WT dur=$DUR reps=$REPS markets=$MARKETS rate=$RATE"
say "cells: $CELLS"

IFS='|' read -r -a CELL_LIST <<< "$CELLS"
# Interleave reps (r1 of every cell, then r2, ...) so a slow drift of the box
# (thermal, page cache, compaction debt) does not bias one cell.
for rep in $(seq 1 "$REPS"); do
    for cell in "${CELL_LIST[@]}"; do
        name=${cell%%:*}; envs=${cell#*:}
        [ "$name" = "$cell" ] && envs=""
        label="$PREFIX-$name-r$rep"
        if [ -s "$RESULTS_ROOT/$label/summary.json" ] && [ "${OVERWRITE:-0}" != 1 ]; then
            say "skip $label (summary.json exists)"; continue
        fi
        say "=== cell $label extra_env='$envs'"
        OVERWRITE=${OVERWRITE:-0} "$TOOLS_DIR/run-cell.sh" "$WT" "$label" "$MARKETS" "$DUR" "$RATE" "$envs" \
            > "$RESULTS_ROOT/$label.console" 2>&1
        rc=$?
        say "cell $label rc=$rc :: $(grep -E '^SUMMARY|^WORKERS' "$RESULTS_ROOT/$label.console" | tr '\n' ' ')"
        # 20 s settle between cells: let the previous devnet's RocksDB fully close
        # and the box's load1 decay before the next CLEAN=1 launch.
        sleep 20
    done
done
say "sweep done; compare: python3 $TOOLS_DIR/compare.py $RESULTS_ROOT/$PREFIX-*"
python3 "$TOOLS_DIR/compare.py" "$RESULTS_ROOT/$PREFIX"-*/ 2>&1 | tee -a "$LOG"
