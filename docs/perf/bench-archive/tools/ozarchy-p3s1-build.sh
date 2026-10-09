#!/usr/bin/env bash
# ozarchy-p3s1 (ozarchy): build arm B = perf/item6-phase2 3aa516e0 (Phase 2 + main 95b01af2: R02 + EVM RPC typing, no R01).
# Copy of ozarchy-p3s0-build.sh (same flags, node-only, detached worktree, FRESH target dir); one arm only.
# Arms A (1b389700, node 86477b00) and C (3efff0d6, node 0c100f3b) reuse ozarchy-p3s0-stage/a and /b.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-p3s1-build
BENCH_SRC=$R/ozarchy-p2byid-stage/p2/release/bench-throughput
declare -A SHA WT TD
WT[b]=/home/oz/projects/wt/p3s1-3aa516e0; TD[b]=/home/oz/.cargo-target-p3s1-3aa5
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
SHA[b]=$(git -C "${WT[b]}" rev-parse 3aa516e0)
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ "$(md5sum < "$BENCH_SRC" | cut -c1-8)" = 6c7ad1a7 ] || die "reused bench md5 changed"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
: > "$B/md5s.txt"
for x in b; do
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
    st=$R/ozarchy-p3s1-stage/$x/release
    mkdir -p "$st"
    cp -p --reflink=auto "${TD[$x]}/release/torus-node" "$st/torus-node" || die "stage $x node"
    cp -p --reflink=auto "$BENCH_SRC" "$st/bench-throughput" || die "stage bench-throughput"
    echo "$x ${SHA[$x]} $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
done
echo "bench bdd5b470 $(md5sum < "$BENCH_SRC" | cut -d' ' -f1) (staged from ozarchy-p2byid-stage/p2)" >> "$B/md5s.txt"
N=$(cut -d' ' -f3 "$B/md5s.txt" | head -1)
grep -q " $N " $R/ozarchy-p3s0-build/md5s.txt && die "b node identical to a p3s0 node"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
