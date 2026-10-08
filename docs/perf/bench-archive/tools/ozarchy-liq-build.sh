#!/usr/bin/env bash
# liq STEP 0: build bench/liq-stress @ af8529e torus-node + bench-throughput (wt/liq-stress) into ~/.cargo-target-liqstress (seeded from ~/.cargo-target-35e69b3) with
# the mif2 flags; stage crab = this node + this bench. Starts no node.
set -u
R=/home/oz/bench-results-matched
WT=/home/oz/projects/wt/liq-stress
TD=/home/oz/.cargo-target-liqstress
B=$R/ozarchy-liq-build
STC=$R/ozarchy-liq-stage/crab/release
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(git -C "$WT" rev-parse --short=7 HEAD)" = af8529e ] || die "wt HEAD"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "wt dirty"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -e "$TD" ] || cp -a --reflink=auto /home/oz/.cargo-target-35e69b3 "$TD" || die seed
(
  export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
  export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
  cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD cargo build --release -p torus-node -p bench-throughput
) > "$B/build.log" 2>&1 || die "cargo build (see $B/build.log)"
cp -p --reflink=auto "$TD/release/torus-node" "$STC/torus-node" || die stage-node
cp -p --reflink=auto "$TD/release/bench-throughput" "$STC/bench-throughput" || die stage-bench
md5sum "$TD/release/torus-node" "$STC/torus-node" "$TD/release/bench-throughput" "$STC/bench-throughput" > "$B/md5s.txt"
readelf -n "$STC/torus-node" | awk '/Build ID/{print "torus-node build-id "$3}' >> "$B/md5s.txt"
"$STC/bench-throughput" oracle-feed --help | grep -q -- --shock-bp || die "bench lacks --shock-bp"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
