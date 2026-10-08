#!/usr/bin/env bash
# Gate 3 ubench: ubench_econ d52a33f vs d9ef4f7, default release flags, 3 alternating pairs per shape.
set -u
O=/home/oz/bench-results-matched/ozarchy-c3pf1-ubench
unset RUSTFLAGS CARGO_PROFILE_RELEASE_DEBUG
quiet() { local t=0; while :; do read -r l1 _ < /proc/loadavg
    if [ -z "$(pgrep 'cargo|rustc')" ] && awk -v l="$l1" 'BEGIN{exit !(l<2)}'; then return 0; fi
    t=$((t+10)); [ $t -ge 900 ] && { echo "not quiet ($l1)"; return 1; }; sleep 10; done; }
run() { # tag wt td shape envs...
    local tag=$1 wt=$2 td=$3 shape=$4; shift 4
    quiet || return 1
    echo "[$(date +%T)] START $shape $tag load=$(cut -d' ' -f1 /proc/loadavg)"
    ( cd "$wt" && env "$@" CARGO_TARGET_DIR="$td" cargo test -p torus-bridge --release --test ubench_econ -- --ignored --nocapture ) > "$O/$shape-$tag.log" 2>&1
    echo "[$(date +%T)] END $shape $tag rc=$? $(grep -c Compiling "$O/$shape-$tag.log") compiles"
}
for i in 1 2 3; do
    run d52-$i   /home/oz/projects/wt/ub-d52a33f   /home/oz/.cargo-target-ub-d52   a UB_MARKS=1 UB_MARKETS=300
    run c3pf1-$i /home/oz/projects/wt/item6-c3-pf1 /home/oz/.cargo-target-ub-c3pf1 a UB_MARKS=1 UB_MARKETS=300
done
for i in 1 2 3; do
    run d52-$i   /home/oz/projects/wt/ub-d52a33f   /home/oz/.cargo-target-ub-d52   b UB_MARKS=1 UB_MARKETS=300 UB_MARK_WALK=10
    run c3pf1-$i /home/oz/projects/wt/item6-c3-pf1 /home/oz/.cargo-target-ub-c3pf1 b UB_MARKS=1 UB_MARKETS=300 UB_MARK_WALK=10
done
