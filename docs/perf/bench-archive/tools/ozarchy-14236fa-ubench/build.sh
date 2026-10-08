#!/usr/bin/env bash
# default release flags (no prof flags): pre-build ubench test binaries for 14236fa and 9c4be2c
set -u
unset RUSTFLAGS CARGO_PROFILE_RELEASE_DEBUG CARGO_ENCODED_RUSTFLAGS
for p in "/home/oz/projects/wt/item6-phase1 /home/oz/.cargo-target-ub-14236fa" "/home/oz/projects/wt/ub-9c4be2c /home/oz/.cargo-target-ub-9c4be2c"; do
    set -- $p
    cd "$1" || exit 1
    echo "== wt=$1 head=$(git rev-parse HEAD) td=$2"
    CARGO_TARGET_DIR=$2 cargo test -p torus-bridge --release --test ubench_econ --test ubench_epoch --no-run 2>&1 | tail -6 || exit 1
done
