#!/usr/bin/env bash
# ozarchy-bkm (ozarchy): Classic vs mode 3 book-layout control. ONE node for both arms = origin/main 1eced05c (b88b0c90 on top is docs only).
# Copy of ozarchy-p3s1-build.sh (same flags, node-only, detached worktree, FRESH target dir). Run under
# `flock /tmp/claude-1000/torus-suite.lock` (other worktrees' suites hold that lock); cargo/rustc outside the lock only logged.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-bkm-build
BENCH_SRC=$R/ozarchy-p2byid-stage/p2/release/bench-throughput
declare -A SHA WT TD
WT[m]=/home/oz/projects/wt/bkm-1eced05c; TD[m]=/home/oz/.cargo-target-bkm-1ece
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
echo "[$(date +%T)] lock acquired (suite lock); load $(cut -d' ' -f1-3 /proc/loadavg); other cargo/rustc: $(pgrep -a 'cargo|rustc' | cut -c1-120 | tr '\n' ';')"
SHA[m]=$(git -C "${WT[m]}" rev-parse 1eced05c)
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ "$(md5sum < "$BENCH_SRC" | cut -c1-8)" = 6c7ad1a7 ] || die "reused bench md5 changed"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
: > "$B/md5s.txt"
for x in m; do
    [ "$(git -C "${WT[$x]}" rev-parse HEAD)" = "${SHA[$x]}" ] || die "${WT[$x]} not at ${SHA[$x]}"
    [ -z "$(git -C "${WT[$x]}" status --porcelain)" ] || die "${WT[$x]} dirty"
    [ ! -e "${TD[$x]}" ] || die "${TD[$x]} exists (must be fresh)"
    echo "[$(date +%T)] build $x ${SHA[$x]}"
    ( cd "${WT[$x]}" || exit 1
      echo "tag=$x wt=${WT[$x]} head=$(git rev-parse HEAD) td=${TD[$x]} RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
      CARGO_TARGET_DIR=${TD[$x]} cargo build --release -p torus-node
      echo "rc=$?" ) > "$B/$x-build.log" 2>&1
    grep -q '^rc=0$' "$B/$x-build.log" || die "$x build (see $B/$x-build.log)"
    for c in torus-types torus-telemetry hotstuff_rs torus-state torus-core torus-bridge torus-consensus torus-mempool torus-node; do
        grep -q "Compiling $c v[^ ]* (${WT[$x]}/" "$B/$x-build.log" || die "$x did not compile $c from ${WT[$x]}"
    done
    st=$R/ozarchy-bkm-stage/$x/release
    mkdir -p "$st"
    cp -p --reflink=auto "${TD[$x]}/release/torus-node" "$st/torus-node" || die "stage $x node"
    cp -p --reflink=auto "$BENCH_SRC" "$st/bench-throughput" || die "stage bench-throughput"
    echo "$x ${SHA[$x]} $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
done
echo "bench bdd5b470 $(md5sum < "$BENCH_SRC" | cut -d' ' -f1) (staged from ozarchy-p2byid-stage/p2)" >> "$B/md5s.txt"
N=$(cut -d' ' -f3 "$B/md5s.txt" | head -1)
grep -q " $N " $R/ozarchy-p3s0-build/md5s.txt $R/ozarchy-p3s1-build/md5s.txt && die "node identical to a p3s0/p3s1 node"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
