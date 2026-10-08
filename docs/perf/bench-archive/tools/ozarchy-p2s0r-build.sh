#!/usr/bin/env bash
# p2s0r STEP 0: arm A = main 35e69b3, rebuilt exactly as section 23.1's ozarchy-bd-build.sh did (same worktree path
# wt/main-35e69b3, same target-dir path ~/.cargo-target-35e69b3, same flags, combined -p torus-node -p bench-throughput)
# but UNSEEDED (the bd build seeded from ~/.cargo-target-59fa407; its log shows every workspace crate recompiled from
# wt/main-35e69b3, so it was clean). The 23.1 staged binary (c2ea1ff8) was pruned. Then a build-style check: the same
# combined build of d3ba3c0a in ~/.cargo-target-d3ba3c0a-fresh, to see if it gives the node-only build's md5 (193ae781).
# Stage A = ozarchy-p2s0r-stage/a/release/torus-node. Starts no node.
set -u
R=/home/oz/bench-results-matched
WT=/home/oz/projects/wt/main-35e69b3
TD=/home/oz/.cargo-target-35e69b3
WT_B=/home/oz/projects/wt/main-d3ba3c0a
TD_B=/home/oz/.cargo-target-d3ba3c0a-fresh
B=$R/ozarchy-p2s0r-build
ST=$R/ozarchy-p2s0r-stage/a/release
mkdir -p "$B" "$ST"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(git -C "$WT" rev-parse HEAD)" = 35e69b3d85c6ba56f24086604b703fcbbf1a15c4 ] || die "wt HEAD"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "wt dirty"
[ "$(git -C "$WT_B" rev-parse HEAD)" = d3ba3c0a47dcd0a262bf26dc8fbfa9687842ae4b ] || die "base wt HEAD"
[ -z "$(git -C "$WT_B" status --porcelain)" ] || die "base wt dirty"
[ ! -e "$TD" ] || die "$TD exists (must be fresh)"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
( cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD cargo build --release -p torus-node -p bench-throughput
  echo "rc=$?" ) > "$B/a-build.log" 2>&1
grep -q '^rc=0$' "$B/a-build.log" || die "A build (see $B/a-build.log)"
for c in torus-types torus-telemetry hotstuff_rs torus-state torus-core torus-bridge torus-consensus torus-node; do
    grep -q "Compiling $c v[^ ]* ($WT/" "$B/a-build.log" || die "A did not compile $c from $WT"
done
cp -p --reflink=auto "$TD/release/torus-node" "$ST/torus-node" || die stage-a
( cd "$WT_B" || exit 1
  echo "wt=$WT_B head=$(git rev-parse HEAD) td=$TD_B (build-style check: combined -p torus-node -p bench-throughput)"
  CARGO_TARGET_DIR=$TD_B cargo build --release -p torus-node -p bench-throughput
  echo "rc=$?" ) > "$B/b-combined-build.log" 2>&1
grep -q '^rc=0$' "$B/b-combined-build.log" || die "B combined build (see $B/b-combined-build.log)"
{ md5sum "$TD/release/torus-node" "$ST/torus-node" "$TD/release/bench-throughput" "$TD_B/release/torus-node" "$TD_B/release/bench-throughput"
  readelf -n "$ST/torus-node" | awk '/Build ID/{print "A torus-node build-id "$3}'
  echo "section 23.1 node: c2ea1ff8377ab6568f65b35af4273c0c build-id e2af56ad69e9defc7cc8304618161a731e729bf8"
  echo "B node-only build (staged): 193ae78196f73eab0676f3e41228455b"; } > "$B/md5s.txt"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
