#!/usr/bin/env bash
# default release flags (no prof flags): pre-build ubench_econ test binaries for d52a33f and d9ef4f7
set -u
unset RUSTFLAGS CARGO_PROFILE_RELEASE_DEBUG CARGO_ENCODED_RUSTFLAGS
for p in "/home/oz/projects/wt/ub-d52a33f /home/oz/.cargo-target-ub-d52" "/home/oz/projects/wt/item6-c3-pf1 /home/oz/.cargo-target-ub-c3pf1"; do
    set -- $p
    cd "$1" || exit 1
    echo "== wt=$1 head=$(git rev-parse HEAD) td=$2"
    CARGO_TARGET_DIR=$2 cargo test -p torus-bridge --release --test ubench_econ --no-run 2>&1 | tail -5 || exit 1
done
