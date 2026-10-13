#!/usr/bin/env bash
# ozarchy-p3cloid: build torus-node AND bench-throughput from main ef812b02 into a FRESH target dir (cloid changed the signed-order
# format: the bench must match the node commit). Same flags as ozarchy-p3q-build.sh (frame pointers, line tables, mold, flock).
# Default features = glibc malloc (no jemalloc feature in ef812b02 Cargo.toml). Stages both into ozarchy-p3cloid-stage/cl-run/release.
set -u
SHA=ef812b02506868729e04190f9a28935e0edda9cc; WT=/home/oz/projects/wt/p3cloid-ef812b02; TD=/home/oz/.cargo-target-p3cloid-ef812b02
R=/home/oz/bench-results-matched; B=$R/ozarchy-p3cloid-build; ST=$R/ozarchy-p3cloid-stage/cl-run/release
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/cl.done"; exit 1; }
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ "$(git -C "$WT" rev-parse HEAD)" = "$SHA" ] || die "$WT not at $SHA"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "$WT dirty"
[ ! -e "$TD" ] || die "$TD exists (must be fresh)"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
( cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD flock /tmp/claude-1000/torus-suite.lock cargo build --release -p torus-node -p bench-throughput
  echo "rc=$?" ) > "$B/cl.build.log" 2>&1
grep -q '^rc=0$' "$B/cl.build.log" || die "build (see $B/cl.build.log)"
for c in torus-types torus-state torus-core torus-bridge torus-consensus torus-node bench-throughput; do
    grep -q "Compiling $c v[^ ]* ($WT/" "$B/cl.build.log" || die "did not compile $c from $WT"
done
mkdir -p "$ST"
for bin in torus-node bench-throughput; do
    [ ! -e "$ST/$bin" ] || die "$ST/$bin already staged"
    cp -p --reflink=auto "$TD/release/$bin" "$ST/$bin" || die "stage $bin"
done
echo "cl $SHA $(md5sum < "$ST/torus-node" | cut -d' ' -f1) $(readelf -n "$ST/torus-node" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "b $SHA $(md5sum < "$ST/bench-throughput" | cut -d' ' -f1) $(readelf -n "$ST/bench-throughput" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "exit=0" > "$B/cl.done"
echo "[$(date +%T)] BUILD OK"
