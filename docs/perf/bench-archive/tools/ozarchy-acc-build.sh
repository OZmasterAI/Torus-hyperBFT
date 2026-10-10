#!/usr/bin/env bash
# ozarchy-acc (append-CF codec + WAL compression screen): build node + bench-throughput from perf/append-cf-compaction fa8b646f
# (worktree /home/oz/projects/wt/append-cf-compaction, FRESH target dir). Copy of ozarchy-p3s0r-build.sh (same flags:
# frame pointers, line tables, mold); holds /tmp/claude-1000/torus-suite.lock for the cargo command.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-acc-build
WT=/home/oz/projects/wt/append-cf-compaction
TD=/home/oz/.cargo-target-acc
SHA=fa8b646f447fa3b7ad21dcb4b070273af15214a9
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ "$(git -C "$WT" rev-parse HEAD)" = "$SHA" ] || die "$WT not at $SHA"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "$WT dirty"
[ ! -e "$TD" ] || die "$TD exists (must be fresh)"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
echo "[$(date +%T)] build $SHA"
( cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD flock /tmp/claude-1000/torus-suite.lock cargo build --release -p torus-node -p bench-throughput
  echo "rc=$?" ) > "$B/build.log" 2>&1
grep -q '^rc=0$' "$B/build.log" || die "build (see $B/build.log)"
for c in torus-types torus-state torus-core torus-bridge torus-consensus torus-mempool torus-node bench-throughput; do
    grep -q "Compiling $c v[^ ]* ($WT/" "$B/build.log" || die "did not compile $c from $WT"
done
st=$R/ozarchy-acc-stage/n/release
mkdir -p "$st"
cp -p --reflink=auto "$TD/release/torus-node" "$st/torus-node" || die "stage node"
cp -p --reflink=auto "$TD/release/bench-throughput" "$st/bench-throughput" || die "stage bench"
: > "$B/md5s.txt"
echo "n $SHA $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
echo "bench $SHA $(md5sum < "$st/bench-throughput" | cut -d' ' -f1)" >> "$B/md5s.txt"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
