#!/usr/bin/env bash
# p2s0x STEP 0 (bisect of the 35e69b3 -> d3ba3c0a regression, results doc section 26): node-only builds (like p2s0b's
# base B 193ae781), each from its own detached worktree into its own FRESH, UNSEEDED target dir (reflink lesson),
# flags as mif2: --release, line-tables-only, mold, frame pointers. Probes:
#   a  = 35e69b3 (A, node-only this time; p2s0r's A was the combined build)   wt/main-35e69b3
#   p0 = 8582e827 (a746c408^1)   p1 = a746c408 (ADL budget)   p2 = 2ebe1a14 (9e695364^1)   p3 = 9e695364 (exact cost basis)
# B (d3ba3c0a node-only, 193ae781) is reused from ozarchy-p2s0r-stage/b. Bench fff899ca for every arm.
# Stage: ozarchy-p2s0x-stage/<tag>/release/{torus-node,bench-throughput}. Starts no node.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-p2s0x-build
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
    st=$R/ozarchy-p2s0x-stage/$tag/release
    mkdir -p "$st"
    cp -p --reflink=auto "$td/release/torus-node" "$st/torus-node" || die "stage $tag"
    cp -p --reflink=auto "$BENCH" "$st/bench-throughput" || die "stage bench $tag"
    echo "$tag $sha $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
done <<'EOF'
a 35e69b3d85c6ba56f24086604b703fcbbf1a15c4 /home/oz/projects/wt/main-35e69b3 /home/oz/.cargo-target-p2s0x-a
p0 8582e827feed09222e14a889a31c7280e75194a0 /home/oz/projects/wt/bisect-8582e827 /home/oz/.cargo-target-p2s0x-p0
p1 a746c408624b0c84729adab7f2f817bf7b6b3827 /home/oz/projects/wt/bisect-a746c408 /home/oz/.cargo-target-p2s0x-p1
p2 2ebe1a145d744ef94fdb27a9f3d419b5c5be7b2a /home/oz/projects/wt/bisect-2ebe1a14 /home/oz/.cargo-target-p2s0x-p2
p3 9e695364972ce759e81cbf2dbb7b88989b6ca73d /home/oz/projects/wt/bisect-9e695364 /home/oz/.cargo-target-p2s0x-p3
EOF
[ "$(cut -d' ' -f3 "$B/md5s.txt" | sort -u | wc -l)" = 5 ] || die "probe nodes not all distinct"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
