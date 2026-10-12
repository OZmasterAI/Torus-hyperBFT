#!/usr/bin/env bash
# ozarchy-p3rss + ozarchy-p23b (RSS split; P2-3 pool A/B): build ONE package for one arm into a FRESH target dir.
# Copy of ozarchy-p3c1-build.sh (same flags: frame pointers, line tables, mold; same flock), but one package per arm:
#   m = main node 8efcb07a, p = P2-3 pool node efcd1548: -p torus-node
#   b = the one bench-throughput for every arm, from origin/main 8efcb07a: -p bench-throughput
# Usage: ozarchy-p3rss-build.sh ARM SHA WORKTREE TARGET_DIR
set -u
ARM=${1:?arm}; SHA=${2:?sha}; WT=${3:?worktree}; TD=${4:?target dir}
R=/home/oz/bench-results-matched
B=$R/ozarchy-p3rss-build
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/$ARM.done"; exit 1; }
case $ARM in m|p) PKG=torus-node; BIN=torus-node; CHK="torus-types torus-state torus-core torus-bridge torus-consensus torus-mempool torus-node" ;;
             b) PKG=bench-throughput; BIN=bench-throughput; CHK="bench-throughput" ;;
             *) die "arm must be m, p or b" ;; esac
[[ "$SHA" =~ ^[0-9a-f]{40}$ ]] || die "SHA must be a full 40-hex sha"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ "$(git -C "$WT" rev-parse HEAD)" = "$SHA" ] || die "$WT not at $SHA"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "$WT dirty"
[ ! -e "$TD" ] || die "$TD exists (must be fresh)"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
echo "[$(date +%T)] build arm $ARM $SHA -p $PKG"
( cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD flock /tmp/claude-1000/torus-suite.lock cargo build --release -p "$PKG"
  echo "rc=$?" ) > "$B/$ARM.build.log" 2>&1
grep -q '^rc=0$' "$B/$ARM.build.log" || die "build (see $B/$ARM.build.log)"
for c in $CHK; do
    grep -q "Compiling $c v[^ ]* ($WT/" "$B/$ARM.build.log" || die "did not compile $c from $WT"
done
st=$R/ozarchy-p3rss-stage/$ARM/release
mkdir -p "$st"
[ ! -e "$st/$BIN" ] || die "$st/$BIN already staged"
cp -p --reflink=auto "$TD/release/$BIN" "$st/$BIN" || die "stage $BIN"
echo "$ARM $SHA $(md5sum < "$st/$BIN" | cut -d' ' -f1) $(readelf -n "$st/$BIN" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "exit=0" > "$B/$ARM.done"
echo "[$(date +%T)] BUILD OK arm $ARM"
