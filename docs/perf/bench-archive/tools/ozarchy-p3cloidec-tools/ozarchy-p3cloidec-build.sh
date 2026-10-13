#!/usr/bin/env bash
# ozarchy-p3cloidec-build.sh: build torus-node AND bench-throughput for arm F (bc44a845, perf/cloid-engine-cost) into a FRESH
# target dir. Same flags as ozarchy-p3gate-build.sh (line tables, frame pointers, mold, flock; default features = jemalloc).
# Stages into ozarchy-p3cloidec-stage/F-run/release and appends "F <sha> <md5> <buildid>", "bF <sha> <md5> <buildid>" and the
# jemalloc symbol count to md5s.txt. Arm B is NOT built here: its stage is reused from ozarchy-p3gate-stage/s2-run (verified).
set -u
K=F
SHA=bc44a8457cde688692106620e2c2d9ec00bea901
WT=/home/oz/projects/wt/p3cloidec-bc44a845
TD=/home/oz/.cargo-target-p3cloidec-bc44a845
R=/home/oz/bench-results-matched; B=$R/ozarchy-p3cloidec-build; ST=$R/ozarchy-p3cloidec-stage/$K-run/release
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL $K: $*"; echo "exit=1" > "$B/$K.done"; exit 1; }
[ "$(git -C "$WT" rev-parse HEAD)" = "$SHA" ] || die "$WT not at $SHA"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "$WT dirty"
[ ! -e "$TD" ] || die "$TD exists (must be fresh)"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
( cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD flock /tmp/claude-1000/torus-suite.lock cargo build --release -p torus-node -p bench-throughput
  echo "rc=$?" ) > "$B/$K.build.log" 2>&1
grep -q '^rc=0$' "$B/$K.build.log" || die "build (see $B/$K.build.log)"
for c in torus-types torus-state torus-core torus-bridge torus-consensus torus-node bench-throughput; do
    grep -q "Compiling $c v[^ ]* ($WT/" "$B/$K.build.log" || die "did not compile $c from $WT"
done
mkdir -p "$ST"
for bin in torus-node bench-throughput; do
    [ ! -e "$ST/$bin" ] || die "$ST/$bin already staged"
    cp -p --reflink=auto "$TD/release/$bin" "$ST/$bin" || die "stage $bin"
done
echo "$K $SHA $(md5sum < "$ST/torus-node" | cut -d' ' -f1) $(readelf -n "$ST/torus-node" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "b$K $SHA $(md5sum < "$ST/bench-throughput" | cut -d' ' -f1) $(readelf -n "$ST/bench-throughput" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "$K jemalloc symbols: $(nm "$ST/torus-node" 2>/dev/null | grep -c -E '_rjem_|jemalloc')" >> "$B/md5s.txt"
echo "exit=0" > "$B/$K.done"
echo "[$(date +%T)] BUILD OK $K"
