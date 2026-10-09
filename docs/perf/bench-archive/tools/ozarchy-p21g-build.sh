#!/usr/bin/env bash
# p21g (ozarchy): builds for the item 6 Phase 2 P2-1 gate cell (cancel-all index).
#   ref = main e934fa0e (Phase 2 gate reference since plan 9.11)
#   p21 = perf/item6-phase2 2ecc2bdf (= d4b6a4b7 + a comment)
# Copy of ozarchy-c2h-build.sh: node-only builds, each from its own detached worktree into its own FRESH, UNSEEDED
# target dir, --release, line-tables-only, mold, frame pointers. bench-throughput: its source is unchanged
# 707f132f..2ecc2bdf but the fff899ca copies were pruned, so it is rebuilt once from the 2ecc2bdf worktree into the
# p21 target dir (separate build after the p21 node is staged) and staged for BOTH arms (byte-identical copy).
# Stage: ozarchy-p21g-stage/<arm>/release/{torus-node,bench-throughput}. Starts no node.
set -u
R=/home/oz/bench-results-matched
B=$R/ozarchy-p21g-build
mkdir -p "$B"
die() { echo "[$(date +%T)] BUILD FAIL: $*"; echo "exit=1" > "$B/build.done"; exit 1; }
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
    st=$R/ozarchy-p21g-stage/$tag/release
    mkdir -p "$st"
    cp -p --reflink=auto "$td/release/torus-node" "$st/torus-node" || die "stage $tag"
    echo "$tag $sha $(md5sum < "$st/torus-node" | cut -d' ' -f1) $(readelf -n "$st/torus-node" | awk '/Build ID/{print $3}')" >> "$B/md5s.txt"
done <<'EOF'
ref e934fa0e9a9e26d819cd85a665bc4b3ad0623328 /home/oz/projects/wt/gate-e934fa0e /home/oz/.cargo-target-gate-e934
p21 2ecc2bdf2046e163b9b8a1387a44ffab22f2693e /home/oz/projects/wt/gate-2ecc2bdf /home/oz/.cargo-target-gate-2ecc
EOF
WT=/home/oz/projects/wt/gate-2ecc2bdf TD=/home/oz/.cargo-target-gate-2ecc
echo "[$(date +%T)] bench-throughput from $WT"
( cd "$WT" || exit 1
  echo "bench wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
  CARGO_TARGET_DIR=$TD cargo build --release -p bench-throughput
  echo "rc=$?" ) > "$B/bench-build.log" 2>&1
grep -q '^rc=0$' "$B/bench-build.log" || die "bench build (see $B/bench-build.log)"
for a in ref p21; do cp -p --reflink=auto "$TD/release/bench-throughput" "$R/ozarchy-p21g-stage/$a/release/bench-throughput" || die "stage bench $a"; done
cmp -s "$R/ozarchy-p21g-stage/ref/release/bench-throughput" "$R/ozarchy-p21g-stage/p21/release/bench-throughput" || die "staged benches differ"
echo "bench 2ecc2bdf $(md5sum < "$R/ozarchy-p21g-stage/p21/release/bench-throughput" | cut -d' ' -f1)" >> "$B/md5s.txt"
"$R/ozarchy-p21g-stage/p21/release/bench-throughput" consensus --help 2>&1 | grep -q -- --cancel-by-id-fraction || die "bench lacks --cancel-by-id-fraction"
[ "$(head -2 "$B/md5s.txt" | cut -d' ' -f3 | sort -u | wc -l)" = 2 ] || die "arm nodes not distinct"
N_REF=$R/ozarchy-p21g-stage/ref/release/torus-node N_P21=$R/ozarchy-p21g-stage/p21/release/torus-node
LC_ALL=C grep -aq torus_exec_cancel_all_index_entries "$N_P21" || die "p21 node lacks the index gauge"
LC_ALL=C grep -aq torus_exec_cancel_all_books_visited "$N_P21" || die "p21 node lacks the cancel-all counter"
LC_ALL=C grep -aq torus_exec_cancel_all_index_entries "$N_REF" && die "ref node has the index gauge"
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running"
echo "exit=0" > "$B/build.done"
echo "[$(date +%T)] BUILD OK"
