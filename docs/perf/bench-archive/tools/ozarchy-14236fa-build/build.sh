#!/usr/bin/env bash
# identical flags for both arms: debuginfo line tables + frame pointers; RUSTFLAGS repeats the repo's mold link-arg
set -u
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
wt=$1 td=$2
cd "$wt" || exit 1
echo "wt=$wt head=$(git rev-parse HEAD) td=$td RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
CARGO_TARGET_DIR=$td cargo build --release -p torus-node || exit 1
CARGO_TARGET_DIR=$td cargo build --release -p bench-throughput || exit 1
