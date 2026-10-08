#!/usr/bin/env bash
# mif2 STEP 0: build main @ 59fa407 torus-node (wt/main-59fa407) into ~/.cargo-target-59fa407 with main 31a95c65's flags,
# stage crab = this node + bench 9b32d897 (from ~/.cargo-target-mif). Starts no node.
set -u
R=/home/oz/bench-results-matched
WT=/home/oz/projects/wt/main-59fa407
TD=/home/oz/.cargo-target-59fa407
B=$R/ozarchy-mif2-build
ST=$R/ozarchy-mif2-stage/crab/release
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(git -C "$WT" rev-parse HEAD)" = 59fa407bd4e5c61fc35c784f8e74f24a144d984c ] || die "wt HEAD"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "wt dirty"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -e "$TD" ] || cp -a --reflink=auto /home/oz/.cargo-target-main "$TD" || die seed
(
  export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
  export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
  cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD cargo build --release -p torus-node
) > "$B/build.log" 2>&1 || die "cargo build (see $B/build.log)"
cp -p --reflink=auto "$TD/release/torus-node" "$ST/torus-node" || die stage-node
cp -p --reflink=auto /home/oz/.cargo-target-mif/release/bench-throughput "$ST/bench-throughput" || die stage-bench
md5sum "$TD/release/torus-node" "$ST/torus-node" "$ST/bench-throughput" /home/oz/.cargo-target-mif/release/bench-throughput \
  /home/oz/bench-results-matched/ozarchy-mif-stage/main/release/torus-node /home/oz/bench-results-matched/ozarchy-mif-stage/main/release/bench-throughput > "$B/md5s.txt"
readelf -n "$ST/torus-node" | awk '/Build ID/{print "torus-node build-id "$3}' >> "$B/md5s.txt"
[ "$(md5sum < "$ST/bench-throughput" | cut -c1-8)" = 9b32d897 ] || die "bench md5"
[ "$(md5sum < "$ST/torus-node" | cut -c1-8)" != 31a95c65 ] || die "node == main node"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
