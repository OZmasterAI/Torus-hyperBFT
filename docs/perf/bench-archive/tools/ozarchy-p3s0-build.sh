#!/usr/bin/env bash
# ozarchy-p3s0 (ozarchy): build for Phase 3 step 0 baseline: A = perf/item6-phase2 1b389700 (Phase 2 bench head) vs
# B = main 3efff0d6 (merge incl. R01 + R02 fail-stop checks). Neither arm's code matches a staged node
# (1b389700 differs from 029581e5/631becaa in crates/torus-mempool via main f5f28f89), so both are node-only builds
# from detached worktrees into FRESH target dirs (ozarchy-p22g-build.sh method, same flags), one after the other.
# Bench: ozarchy-p2byid-stage/p2/release/bench-throughput (md5 6c7ad1a7, built from bdd5b470), reused read-only.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-p3s0-build
BENCH_SRC=$R/ozarchy-p2byid-stage/p2/release/bench-throughput
declare -A SHA WT TD
SHA[a]=1b389700b8ba9f7e0f2f0a74c5e8b3d0ec2b3f1a; SHA[b]=3efff0d6
WT[a]=/home/oz/projects/wt/p3s0-1b389700; WT[b]=/home/oz/projects/wt/p3s0-3efff0d6
TD[a]=/home/oz/.cargo-target-p3s0-1b38; TD[b]=/home/oz/.cargo-target-p3s0-3eff
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
SHA[a]=$(git -C "${WT[a]}" rev-parse 1b389700); SHA[b]=$(git -C "${WT[b]}" rev-parse 3efff0d6)
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
[ "$(md5sum < "$BENCH_SRC" | cut -c1-8)" = 6c7ad1a7 ] || die "reused bench md5 changed"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
: > "$B/md5s.txt"
for x in a b; do
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
    st=$R/ozarchy-p3s0-stage/$x/release
    mkdir -p "$st"
    cp -p --reflink=auto "${TD[$x]}/release/torus-node" "$st/torus-node" || die "stage $x node"
    cp -p --reflink=auto "$BENCH_SRC" "$st/bench-throughput" || die "stage bench-throughput"
    echo "$x ${SHA[$x]} $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
done
echo "bench bdd5b470 $(md5sum < "$BENCH_SRC" | cut -d' ' -f1) (staged from ozarchy-p2byid-stage/p2)" >> "$B/md5s.txt"
[ "$(head -2 "$B/md5s.txt" | cut -d' ' -f3 | sort -u | wc -l)" = 2 ] || die "arm nodes not distinct"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
