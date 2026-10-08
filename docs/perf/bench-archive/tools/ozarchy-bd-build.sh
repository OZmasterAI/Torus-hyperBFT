#!/usr/bin/env bash
# bd STEP 0: build main @ 35e69b3 torus-node + bench-throughput (wt/main-35e69b3) into ~/.cargo-target-35e69b3 with
# the mif2 flags; stage crab = this node + this bench, main = node 31a95c65 (92a02ed) + this bench. Starts no node.
set -u
R=/home/oz/bench-results-matched
WT=/home/oz/projects/wt/main-35e69b3
TD=/home/oz/.cargo-target-35e69b3
B=$R/ozarchy-bd-build
STC=$R/ozarchy-bd-stage/crab/release
STM=$R/ozarchy-bd-stage/main/release
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(git -C "$WT" rev-parse --short=7 HEAD)" = 35e69b3 ] || die "wt HEAD"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "wt dirty"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -e "$TD" ] || cp -a --reflink=auto /home/oz/.cargo-target-59fa407 "$TD" || die seed
(
  export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
  export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
  cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD cargo build --release -p torus-node -p bench-throughput
) > "$B/build.log" 2>&1 || die "cargo build (see $B/build.log)"
cp -p --reflink=auto "$TD/release/torus-node" "$STC/torus-node" || die stage-node
cp -p --reflink=auto "$TD/release/bench-throughput" "$STC/bench-throughput" || die stage-bench
cp -p --reflink=auto "$R/ozarchy-mif-stage/main/release/torus-node" "$STM/torus-node" || die stage-main-node
cp -p --reflink=auto "$TD/release/bench-throughput" "$STM/bench-throughput" || die stage-main-bench
md5sum "$TD/release/torus-node" "$STC/torus-node" "$TD/release/bench-throughput" "$STC/bench-throughput" "$STM/torus-node" "$STM/bench-throughput" > "$B/md5s.txt"
readelf -n "$STC/torus-node" | awk '/Build ID/{print "torus-node build-id "$3}' >> "$B/md5s.txt"
[ "$(md5sum < "$STM/torus-node" | cut -c1-8)" = 31a95c65 ] || die "main node md5"
[ "$(md5sum < "$STC/torus-node" | cut -c1-8)" != 31a95c65 ] || die "node == main node"
cmp -s "$STC/bench-throughput" "$STM/bench-throughput" || die "bench differs"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
