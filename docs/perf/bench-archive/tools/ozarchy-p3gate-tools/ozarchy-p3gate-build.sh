#!/usr/bin/env bash
# ozarchy-p3gate: build torus-node AND bench-throughput from each arm's own commit into FRESH target dirs (cloid changed the
# signed-order format). Same flags as ozarchy-p3cloid-build.sh (frame pointers, line tables, mold, flock). Default features =
# jemalloc in both commits. Stages into ozarchy-p3gate-stage/<key>-run/release. Usage: ozarchy-p3gate-build.sh s2|m
set -u
K=${1:?key s2|m}
case $K in
  s2) SHA=c88d307379418f2de31a5bad488e8b4db55dfe29; WT=/home/oz/projects/wt/p3gate-s2-c88d3073; TD=/home/oz/.cargo-target-p3gate-s2-c88d3073 ;;
  m)  SHA=de1a97c10d48d6986509d290ce87e73424d07758; WT=/home/oz/projects/wt/p3gate-m-de1a97c1;  TD=/home/oz/.cargo-target-p3gate-m-de1a97c1 ;;
  *) echo "key s2|m"; exit 2 ;;
esac
R=/home/oz/bench-results-matched; B=$R/ozarchy-p3gate-build; ST=$R/ozarchy-p3gate-stage/$K-run/release
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
echo "$K jemalloc symbols: $(nm -D --defined-only "$ST/torus-node" 2>/dev/null | grep -c -E ' (_rjem_|je_|mallctl|malloc_stats_print)'; nm "$ST/torus-node" 2>/dev/null | grep -c -i jemalloc)" >> "$B/md5s.txt"
echo "exit=0" > "$B/$K.done"
echo "[$(date +%T)] BUILD OK $K"
