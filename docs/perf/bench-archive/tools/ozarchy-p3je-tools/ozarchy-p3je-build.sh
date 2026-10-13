#!/usr/bin/env bash
# ozarchy-p3je (2026-10-13): copy of ozarchy-p3cssoak-build.sh; arm keys ja (feat/jemalloc-allocator 88ddc4c2), jb (main deeacdb3, glibc),
#   jcs (test/p3-cs-je e7be8460) and jmf (test/main-fix-je 0360c840); build dir ozarchy-p3je-build, stage ozarchy-p3je-stage.
# ozarchy-p3cssoak (2026-10-13): copy of ozarchy-p3q-build.sh; only change: arm key cs2 (test/p3-cancel-share 41a95355 = fix/cancel-share-oldest-first de97682f merged onto step 2 7b006739) allowed.
# ozarchy-p3q (queue after the soak: merge-wait mw, cancel-share cs): build the control (m, main) or step 2 (g2) node into a FRESH target dir (p3g2 build copy).
# Copy of ozarchy-p3c1-build.sh (same flags: frame pointers, line tables, mold; same flock), but one package per arm:
# Usage: ozarchy-p3next-build.sh ARM SHA WORKTREE TARGET_DIR
set -u
ARM=${1:?arm}; SHA=${2:?sha}; WT=${3:?worktree}; TD=${4:?target dir}
R=/home/oz/bench-results-matched
B=$R/ozarchy-p3je-build
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/$ARM.done"; exit 1; }
case $ARM in ja|jb|jcs|jmf) PKG=torus-node; BIN=torus-node; CHK="torus-types torus-state torus-core torus-bridge torus-consensus torus-mempool torus-node" ;;
             *) die "arm must be ja, jb, jcs or jmf" ;; esac
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
st=$R/ozarchy-p3je-stage/$ARM/release
mkdir -p "$st"
[ ! -e "$st/$BIN" ] || die "$st/$BIN already staged"
cp -p --reflink=auto "$TD/release/$BIN" "$st/$BIN" || die "stage $BIN"
echo "$ARM $SHA $(md5sum < "$st/$BIN" | cut -d' ' -f1) $(readelf -n "$st/$BIN" | awk '/Build ID/{print $3}') (built $(date '+%F %T'))" >> "$B/md5s.txt"
echo "exit=0" > "$B/$ARM.done"
echo "[$(date +%T)] BUILD OK arm $ARM"
