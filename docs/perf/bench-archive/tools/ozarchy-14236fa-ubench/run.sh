#!/usr/bin/env bash
# ubench 14236fa vs 9c4be2c, default release flags, 3 alternating runs per shape.
# (b) UB_MARK_WALK is not supported by 9c4be2c's ubench_econ (added in 3924b76) -> 14236fa only.
set -u
O=/home/oz/bench-results-matched/ozarchy-14236fa-ubench
unset RUSTFLAGS CARGO_PROFILE_RELEASE_DEBUG CARGO_ENCODED_RUSTFLAGS
N=/home/oz/projects/wt/item6-phase1; NT=/home/oz/.cargo-target-ub-14236fa
B=/home/oz/projects/wt/ub-9c4be2c;   BT=/home/oz/.cargo-target-ub-9c4be2c
quiet() { local t=0; while :; do read -r l1 _ < /proc/loadavg
    if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
    t=$((t+10)); [ $t -ge 900 ] && { echo "not quiet ($l1)"; return 1; }; sleep 10; done; }
run() { # test tag wt td shape envs...
    local test=$1 tag=$2 wt=$3 td=$4 shape=$5; shift 5
    quiet || return 1
    echo "[$(date +%T)] START $shape $tag load=$(cut -d' ' -f1 /proc/loadavg)"
    ( cd "$wt" && env "$@" CARGO_TARGET_DIR="$td" cargo test -p torus-bridge --release --test "$test" -- --ignored --nocapture ) > "$O/$shape-$tag.log" 2>&1
    local rc=$?
    echo "[$(date +%T)] END $shape $tag rc=$rc $(grep -c Compiling "$O/$shape-$tag.log") compiles"
}
for i in 1 2 3; do
    run ubench_econ 9c4-$i $B $BT a UB_MARKS=1 UB_MARKETS=300
    run ubench_econ new-$i $N $NT a UB_MARKS=1 UB_MARKETS=300
done
for i in 1 2 3; do
    run ubench_econ 9c4-$i $B $BT c UB_MARKS=1 UB_MARKETS=10
    run ubench_econ new-$i $N $NT c UB_MARKS=1 UB_MARKETS=10
done
for i in 1 2 3; do
    run ubench_econ new-$i $N $NT b UB_MARKS=1 UB_MARKETS=300 UB_MARK_WALK=10
done
for i in 1 2 3; do
    run ubench_epoch new-$i $N $NT d UB_DRAIN=fresh
    run ubench_epoch new-$i $N $NT e UB_DRAIN=fresh TORUS_NATIVE_TRIE_MAINTENANCE=0
done
