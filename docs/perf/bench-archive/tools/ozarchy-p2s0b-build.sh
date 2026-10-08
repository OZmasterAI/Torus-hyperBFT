#!/usr/bin/env bash
# p2s0b STEP 0: builds for item 6 Phase 2 step 0.3 + Gate 0 (perf/item6-phase2 @ 707f132f vs base main d3ba3c0a).
# Flags as mif2 / adlcells: --release, CARGO_PROFILE_RELEASE_DEBUG=line-tables-only,
# RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes".
#   step0: wt/item6-phase2 (clean at 707f132f; = 1a6573dc + docs) into ~/.cargo-target-read-gas (builder's, incremental):
#          torus-node, then bench-throughput (separate builds), then the ubench_hasher test binary (--no-run).
#   base:  detached wt/main-d3ba3c0a into ~/.cargo-target-d3ba3c0a-fresh (NOT seeded: a reflink seed from read-gas
#          kept step0 crates as fresh, mtime trap, first attempt in ~/.cargo-target-d3ba3c0a-STALE-seeded):
#          torus-node only (both arms use the step0 bench).
# Stage: ozarchy-p2s0b-stage/{step0,base}/release/{torus-node,bench-throughput}. Starts no node.
set -u
R=/home/oz/bench-results-matched
WT_S=/home/oz/projects/wt/item6-phase2
WT_B=/home/oz/projects/wt/main-d3ba3c0a
TD_S=/home/oz/.cargo-target-read-gas
TD_B=/home/oz/.cargo-target-d3ba3c0a-fresh
B=$R/ozarchy-p2s0b-build
ST_S=$R/ozarchy-p2s0b-stage/step0/release
ST_B=$R/ozarchy-p2s0b-stage/base/release
mkdir -p "$B" "$ST_S" "$ST_B"
[ ! -e "$B/build.done" ] || mv "$B/build.done" "$B/build.done.attempt1"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(git -C "$WT_S" rev-parse HEAD)" = 707f132f07ec31d1e5a3413e98c673a448d5ec84 ] || die "step0 wt HEAD"
[ -z "$(git -C "$WT_S" status --porcelain)" ] || die "step0 wt dirty"
[ "$(git -C "$WT_B" rev-parse HEAD)" = d3ba3c0a47dcd0a262bf26dc8fbfa9687842ae4b ] || die "base wt HEAD"
[ -z "$(git -C "$WT_B" status --porcelain)" ] || die "base wt dirty"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
bld() { # wt td log args...
    local wt=$1 td=$2 log=$3; shift 3
    ( cd "$wt" || exit 1
      echo "[$(date +%T)] wt=$wt head=$(git rev-parse HEAD) td=$td RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG: cargo $*"
      CARGO_TARGET_DIR=$td cargo "$@"
      echo "[$(date +%T)] rc=$?" ) > "$log" 2>&1
    grep -q '\] rc=0$' "$log"
}
echo "[$(date +%T)] step0 torus-node"
bld "$WT_S" "$TD_S" "$B/step0-node.log" build --release -p torus-node || die "step0 node (see $B/step0-node.log)"
cp -p --reflink=auto "$TD_S/release/torus-node" "$ST_S/torus-node" || die stage-step0-node
echo "[$(date +%T)] step0 bench-throughput"
bld "$WT_S" "$TD_S" "$B/step0-bench.log" build --release -p bench-throughput || die "step0 bench (see $B/step0-bench.log)"
cp -p --reflink=auto "$TD_S/release/bench-throughput" "$ST_S/bench-throughput" || die stage-step0-bench
echo "[$(date +%T)] step0 ubench_hasher --no-run"
bld "$WT_S" "$TD_S" "$B/step0-hasher.log" test --release -p torus-bridge --test ubench_hasher --no-run || die "hasher test build (see $B/step0-hasher.log)"
echo "[$(date +%T)] base torus-node"
bld "$WT_B" "$TD_B" "$B/base-node.log" build --release -p torus-node || die "base node (see $B/base-node.log)"
grep -q 'Compiling torus-node' "$B/base-node.log" || die "base torus-node was not recompiled (stale seed binary?)"
for c in $(grep -oE 'Compiling (torus-[a-z-]+|hotstuff[a-z_-]*) v[^ ]+ \(/home/oz/projects/wt/item6-phase2' "$B/step0-node.log" | awk '{print $2}'); do
    grep -q "Compiling $c v[^ ]* (/home/oz/projects/wt/main-d3ba3c0a" "$B/base-node.log" || die "base did not compile $c from wt/main-d3ba3c0a"
done
cp -p --reflink=auto "$TD_B/release/torus-node" "$ST_B/torus-node" || die stage-base-node
cp -p --reflink=auto "$ST_S/bench-throughput" "$ST_B/bench-throughput" || die stage-base-bench
md5sum "$TD_S/release/torus-node" "$ST_S/torus-node" "$TD_S/release/bench-throughput" "$ST_S/bench-throughput" \
    "$TD_B/release/torus-node" "$ST_B/torus-node" "$ST_B/bench-throughput" > "$B/md5s.txt"
for n in "$ST_S/torus-node" "$ST_B/torus-node"; do readelf -n "$n" | awk -v n="$n" '/Build ID/{print "build-id "$3" "n}'; done >> "$B/md5s.txt"
cmp -s "$ST_S/bench-throughput" "$ST_B/bench-throughput" || die "staged benches differ"
[ "$(md5sum < "$ST_S/torus-node")" != "$(md5sum < "$ST_B/torus-node")" ] || die "step0 node == base node"
LC_ALL=C grep -aq torus_exec_cancel_all_books_visited "$ST_S/torus-node" || die "step0 node lacks the cancel-all counter"
LC_ALL=C grep -aq torus_exec_cancel_all_books_visited "$ST_B/torus-node" && die "base node has the cancel-all counter"
"$ST_S/bench-throughput" consensus --help 2>&1 | grep -q -- --cancel-by-id-fraction || die "bench lacks --cancel-by-id-fraction"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
