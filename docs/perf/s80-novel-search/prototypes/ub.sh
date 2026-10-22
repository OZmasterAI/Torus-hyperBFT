#!/usr/bin/env bash
# s80 microbench runner: one ubench_econ invocation (UB_RUNS runs, prints MEDIAN lines) for an arm, under the
# heavy build lock so no build or cell overlaps it.
#   ub.sh <arm> <label> [VAR=value ...]     e.g.  ub.sh foo on-1 TORUS_S80_FOO=1 UB_MARKS=1
# Needs the test binary built first:
#   cd /home/oz/projects/wt/novel-s80/proto/<arm> && CARGO_TARGET_DIR=$HOME/.cargo-target-s80-<arm> \
#     flock -w 900 -o "$HOME/projects/wt/.cargo-heavy.lock" cargo test -q -p torus-bridge --release --test ubench_econ --no-run
# Defaults UB_MARKS=1 (oracle marks fed: without marks nothing fills), UB_RUNS=5. Calibration: base 2d2f3ed9 = 2.78 engine ms/1k fills,
# 11.3k fills/blk, 32 ms engine/blk (cell: 2.76 ms/1k, ~11.5k fills/blk) -> ubench_econ tracks the cell engine closely.
# TORUS_SETTLE_STREAMED=1 by default: L2.1 is opt-in on 29bfc705 (the CL21 cells set it), default-on on main; without it
# the h2h microbench reads ~5-8% slower (s80 check: 2.87-3.10 vs 2.79-2.93).
# Output: /home/oz/projects/wt/novel-s80/proto/results/ub-<arm>-<label>.log ; prints the MEDIAN lines.
# A/B rule: alternate arms/flags (A B B A or A B A B), >= 3 rounds each, compare medians; same UB_* env on both sides.
set -euo pipefail
P=/home/oz/projects/wt/novel-s80/proto
a=${1:?arm}; l=${2:?label}; shift 2
B=$(ls -t "$HOME/.cargo-target-s80-$a"/release/deps/ubench_econ-* 2>/dev/null | grep -v '\.d$' | head -1)
[ -x "$B" ] || { echo "no ubench_econ binary for $a (build it first)" >&2; exit 1; }
mkdir -p "$P/results"
O=$P/results/ub-$a-$l.log
cd "$P/$a"
flock -w 3600 -o "$HOME/projects/wt/.cargo-heavy.lock" env UB_MARKETS=${UB_MARKETS:-300} UB_RUNS=${UB_RUNS:-5} UB_MARKS=${UB_MARKS:-1} TORUS_SETTLE_STREAMED=${TORUS_SETTLE_STREAMED:-1} "$@" "$B" --ignored --nocapture > "$O" 2>&1 || { echo "ubench failed, see $O" >&2; tail -20 "$O" >&2; exit 1; }
echo "== $a $l ($*) bin=$(basename "$B")"; grep -h "MEDIAN" "$O" || tail -5 "$O"
