#!/usr/bin/env bash
# ozarchy-p3r1 (Phase 3 step 1 restart bench; copy of ozarchy-p3c1-build.sh): build node + bench-throughput for ONE arm into a FRESH target dir.
# Copy of ozarchy-p23-build.sh (same flags: frame pointers, line tables, mold; same flock on /tmp/claude-1000/torus-suite.lock),
# parameterised by arm so the control and the step 1 binary are built the same way.
# Usage: ozarchy-p3r1-build.sh ARM SHA WORKTREE TARGET_DIR
#   ARM c = control (main / step 1 merge-base), s = step 1 (feat/item6-phase3-step1)
# Stages $R/ozarchy-p3r1-stage/ARM/release/{torus-node,bench-throughput}; appends "ARM SHA node_md5 build_id" and
# "bench-ARM SHA bench_md5" to $R/ozarchy-p3r1-build/md5s.txt. Marker: $R/ozarchy-p3r1-build/ARM.done
set -u
ARM=${1:?arm}; SHA=${2:?sha}; WT=${3:?worktree}; TD=${4:?target dir}
R=/home/oz/bench-results-matched
B=$R/ozarchy-p3r1-build
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/$ARM.done"; exit 1; }
case $ARM in c|s) ;; *) die "arm must be c or s" ;; esac
[[ "$SHA" =~ ^[0-9a-f]{40}$ ]] || die "SHA must be a full 40-hex sha"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ "$(git -C "$WT" rev-parse HEAD)" = "$SHA" ] || die "$WT not at $SHA"
[ -z "$(git -C "$WT" status --porcelain)" ] || die "$WT dirty"
[ ! -e "$TD" ] || die "$TD exists (must be fresh)"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
echo "[$(date +%T)] build arm $ARM $SHA"
( cd "$WT" || exit 1
  echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD flock /tmp/claude-1000/torus-suite.lock cargo build --release -p torus-node -p bench-throughput
  echo "rc=$?" ) > "$B/$ARM.build.log" 2>&1
grep -q '^rc=0$' "$B/$ARM.build.log" || die "build (see $B/$ARM.build.log)"
for c in torus-types torus-state torus-core torus-bridge torus-consensus torus-mempool torus-node bench-throughput; do
    grep -q "Compiling $c v[^ ]* ($WT/" "$B/$ARM.build.log" || die "did not compile $c from $WT"
done
st=$R/ozarchy-p3r1-stage/$ARM/release
mkdir -p "$st"
[ ! -e "$st/torus-node" ] || die "$st/torus-node already staged"
cp -p --reflink=auto "$TD/release/torus-node" "$st/torus-node" || die "stage node"
cp -p --reflink=auto "$TD/release/bench-throughput" "$st/bench-throughput" || die "stage bench"
echo "$ARM $SHA $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "bench-$ARM $SHA $(md5sum < "$st/bench-throughput" | cut -d' ' -f1)" >> "$B/md5s.txt"
echo "exit=0" > "$B/$ARM.done"
echo "[$(date +%T)] BUILD OK arm $ARM"
