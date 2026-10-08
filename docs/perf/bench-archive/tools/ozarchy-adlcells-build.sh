#!/usr/bin/env bash
# adlcells STEP 0: build torus-node at 6a25e20 (perf/adl-budget: A1-A8 + review fixes + drain caches) from the detached
# worktree wt/adl-cells-6a25e20 into ~/.cargo-target-adl-cells (seeded from ~/.cargo-target-matched) with the mif2 flags
# (copy of ozarchy-liq-build.sh). bench-throughput is NOT rebuilt: the staged ozarchy-liq one (ec4e14a9, built from
# bench/liq-stress af8529e; tools/bench-throughput is unchanged af8529e..b49e40b and 6a25e20 lacks --shock-bp) is copied.
# Stage = ozarchy-adlcells-stage/release/{torus-node,bench-throughput}. Starts no node.
set -u
R=/home/oz/bench-results-matched
WT=/home/oz/projects/wt/adl-cells-6a25e20
TD=/home/oz/.cargo-target-adl-cells
B=$R/ozarchy-adlcells-build
ST=$R/ozarchy-adlcells-stage/release
OLDBENCH=$R/ozarchy-liq-stage/crab/release/bench-throughput
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(git -C "$WT" rev-parse --short=7 HEAD)" = 6a25e20 ] || die "wt HEAD"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "wt dirty"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ "$(md5sum < "$OLDBENCH" | cut -c1-32)" = ec4e14a90152e355db385d6859427695 ] || die "old staged bench md5"
[ -e "$TD" ] || cp -a --reflink=auto /home/oz/.cargo-target-matched "$TD" || die seed
(
  export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
  export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
  cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD cargo build --release -p torus-node
) > "$B/build.log" 2>&1 || die "cargo build (see $B/build.log)"
grep -q 'Compiling torus-node' "$B/build.log" || die "torus-node was not recompiled (stale seed binary?)"
cp -p --reflink=auto "$TD/release/torus-node" "$ST/torus-node" || die stage-node
cp -p --reflink=auto "$OLDBENCH" "$ST/bench-throughput" || die stage-bench
md5sum "$TD/release/torus-node" "$ST/torus-node" "$OLDBENCH" "$ST/bench-throughput" > "$B/md5s.txt"
readelf -n "$ST/torus-node" | awk '/Build ID/{print "torus-node build-id "$3}' >> "$B/md5s.txt"
strings "$ST/torus-node" | grep -q TORUS_LIQ_VALUE_SUM || die "node lacks TORUS_LIQ_VALUE_SUM"
strings "$ST/torus-node" | grep -q 'liquidation: ADL to escrow' || die "node lacks adl-budget log line"
"$ST/bench-throughput" oracle-feed --help | grep -q -- --shock-bp || die "bench lacks --shock-bp"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
