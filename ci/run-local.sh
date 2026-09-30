#!/bin/bash
# run-local.sh: run the jobs of the archived GitHub CI (ci/github-ci.yml) on
# this machine.
#
# Jobs:
#   check    the CI "check" job: cargo check, clippy, fmt --check and test, all
#            with RUSTFLAGS="-D warnings" like CI. Every step runs even if an
#            earlier one fails, and a pass/fail summary is printed at the end.
#   uniswap  the CI "uniswap-e2e" job: devnet/uniswap/e2e-local.sh
#   all      both
#
# Usage: ci/run-local.sh [check|uniswap|all]   (default all)
#
# The -D warnings RUSTFLAGS change invalidates normal build caches, so "check"
# builds into $REPO/target-ci unless CARGO_TARGET_DIR is set.
set -uo pipefail

JOB="${1:-all}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO" || exit 1

declare -a RESULTS=()
step() {
    local name="$1"
    shift
    echo "=== $name: $*"
    if "$@"; then
        RESULTS+=("PASS  $name")
    else
        RESULTS+=("FAIL  $name")
    fi
}

run_check() {
    (
        export CARGO_TERM_COLOR=always RUSTFLAGS="-D warnings"
        export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target-ci}"
        step "check" cargo check --workspace
        step "clippy" cargo clippy --workspace -- -D warnings
        step "fmt" cargo fmt --all -- --check
        step "test" cargo test --workspace
        printf '%s\n' "${RESULTS[@]}"
        ! printf '%s\n' "${RESULTS[@]}" | grep -q '^FAIL'
    )
}

run_uniswap() {
    bash "$REPO/devnet/uniswap/e2e-local.sh"
}

case "$JOB" in
    check) run_check ;;
    uniswap) run_uniswap ;;
    all)
        run_check
        c=$?
        run_uniswap
        u=$?
        echo "=== check: $([ $c -eq 0 ] && echo PASS || echo FAIL)  uniswap: $([ $u -eq 0 ] && echo PASS || echo FAIL)"
        [ $c -eq 0 ] && [ $u -eq 0 ]
        ;;
    *)
        echo "usage: $0 [check|uniswap|all]"
        exit 2
        ;;
esac
