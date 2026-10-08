#!/usr/bin/env bash
# c2h2 (ozarchy): second build of the c2h campaign (5 arms): sav = perf/position-v2-savings 37b28dd6 and
# both = local merge f1ab2166 (37b28dd6 + d7bd1c36). base/fix were built by ozarchy-c2h-build.sh.
# Original c2h header: A/B of the C2 holder-index fix (perf/c2-set-holder d7bd1c36; built from c051872b = d7bd1c36 + docs/perf only)
# against its parent origin/main bf2edda6 and d3ba3c0a (b, 193ae781, reused from ozarchy-p2s0r-stage/b). Copy of
# ozarchy-p2s0x-build.sh: node-only builds, each from its own worktree into its own FRESH, UNSEEDED target dir,
# --release, line-tables-only, mold, frame pointers. Bench fff899ca for every arm.
# Stage: ozarchy-c2h-stage/<tag>/release/{torus-node,bench-throughput}. Starts no node.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-c2h2-build
BENCH=$R/ozarchy-p2s0r-stage/b/release/bench-throughput
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
[ "$(md5sum < "$BENCH" | cut -c1-8)" = fff899ca ] || die "bench md5"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc running"
[ -z "$(pgrep -x torus-node)" ] || die "torus-node running"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
: > "$B/md5s.txt"
while read -r tag sha wt td; do
    [ "$(git -C "$wt" rev-parse HEAD)" = "$sha" ] || die "$tag: $wt not at $sha"
    [ -z "$(git -C "$wt" status --porcelain)" ] || die "$tag: $wt dirty"
    [ ! -e "$td" ] || die "$tag: $td exists (must be fresh)"
    echo "[$(date +%T)] $tag $sha"
    ( cd "$wt" || exit 1
      echo "tag=$tag wt=$wt head=$(git rev-parse HEAD) td=$td RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
      CARGO_TARGET_DIR=$td cargo build --release -p torus-node
      echo "rc=$?" ) > "$B/$tag-build.log" 2>&1
    grep -q '^rc=0$' "$B/$tag-build.log" || die "$tag build (see $B/$tag-build.log)"
    for c in torus-types torus-telemetry hotstuff_rs torus-state torus-core torus-bridge torus-consensus torus-node; do
        grep -q "Compiling $c v[^ ]* ($wt/" "$B/$tag-build.log" || die "$tag did not compile $c from $wt"
    done
    st=$R/ozarchy-c2h-stage/$tag/release
    mkdir -p "$st"
    cp -p --reflink=auto "$td/release/torus-node" "$st/torus-node" || die "stage $tag"
    cp -p --reflink=auto "$BENCH" "$st/bench-throughput" || die "stage bench $tag"
    echo "$tag $sha $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
done <<'EOF'
sav 37b28dd641774b3277b5a29ae24e9bc41c291a18 /home/oz/projects/wt/v2-savings /home/oz/.cargo-target-c2h-sav
both f1ab21664f196295ce334e4897cbeab087618d5d /home/oz/projects/wt/c2-v2-both /home/oz/.cargo-target-c2h-both
EOF
[ "$(cut -d' ' -f3 "$B/md5s.txt" | sort -u | wc -l)" = 2 ] && ! grep -qE ' (193ae781|29860dc0|b843f522)' "$B/md5s.txt" || die "arm nodes not all distinct (vs each other and b/base/fix)"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
