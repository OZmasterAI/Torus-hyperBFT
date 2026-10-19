# shellcheck shell=bash
# cargo-bin.sh — resolve a cargo-built release binary, honouring a redirected target-dir.
#
# ======================== THE TARGET-DIR TRAP ========================
# ~/.cargo/config.toml may set `[build] target-dir` to a shared location
# (e.g. /home/<user>/.cargo-target). When it does, `cargo build --release`
# exits 0 and writes NOTHING to <repo>/target/release. Every script that
# defaults to ./target/release/<bin> then dies with "No such file or
# directory" on a build that just SUCCEEDED — a confusing failure that has
# cost real time more than once.
#
# Resolve via `cargo metadata` instead of assuming the path.
#
# Usage:
#   . "<repo>/testnet/lib/cargo-bin.sh"
#   BIN="${BIN:-$(cargo_bin bench-throughput "$REPO")}" || exit 1
#
# Honours an explicit override first: if the caller already set BIN/BENCH_BIN,
# don't call this at all. CARGO_TARGET_DIR in the environment wins over
# cargo metadata, matching cargo's own precedence.
#
# NOTE: staging a binary into <repo>/target/release is legitimate for
# bench-throughput (host-side load generator) and REQUIRED for torus-node
# (devnet/Dockerfile does `COPY target/release/torus-node` relative to the
# build context). What is NOT safe is a stale loose torus-node sitting there —
# see build-image.sh for why that produces a silent false-null A/B.
# =====================================================================

cargo_bin() {
    local name="${1:?usage: cargo_bin <binary-name> [repo-root]}"
    local root="${2:-$PWD}"
    local td out

    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        td="$CARGO_TARGET_DIR"
    else
        td=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
             | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])' \
             2>/dev/null) || td=""
    fi

    for out in ${td:+"$td/release/$name"} "$root/target/release/$name"; do
        if [ -x "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    done

    {
        echo "FATAL: '$name' not found."
        echo "  looked in: ${td:+$td/release/$name, }$root/target/release/$name"
        echo "  build it:  cargo build --release -p $name"
        [ -n "$td" ] && echo "  NOTE: your cargo target-dir is redirected to $td —"
        [ -n "$td" ] && echo "        a successful build does NOT populate $root/target/release."
    } >&2
    return 1
}
