#!/usr/bin/env bash
# Same flags as main 31a95c65 (ozarchy-prof10-build/build.sh): mold link-arg, frame pointers, line-tables-only debuginfo.
set -u
B=/home/oz/bench-results-matched/ozarchy-5524646-build
WT=/home/oz/projects/wt/item6-5524646
TD=/home/oz/.cargo-target-5524646
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
[ -e "$TD" ] || cp -a /home/oz/.cargo-target-c58775f-prof "$TD" || exit 1
cd "$WT" || exit 1
echo "wt=$WT head=$(git rev-parse HEAD) td=$TD RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
CARGO_TARGET_DIR=$TD cargo build --release -p torus-node || exit 1
CARGO_TARGET_DIR=$TD cargo build --release -p bench-throughput || exit 1
md5sum $TD/release/torus-node $TD/release/bench-throughput > $B/md5s.txt
for f in $TD/release/torus-node $TD/release/bench-throughput; do echo "$f $(readelf -n $f | awk '/Build ID/{print $3}')"; done > $B/buildids.txt
