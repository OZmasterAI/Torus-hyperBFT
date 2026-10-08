#!/usr/bin/env bash
# p2s0r smoke: arm A (35e69b3 node with the 707f132f harness genesis + bench fff899ca) and arm C (d3ba3c0a +
# TORUS_BOOK_CF_TARGET_FILE_MB=64: env on the nodes + RocksDB OPTIONS of the book CF), 10 markets, 30 s each.
set -u
R=/home/oz/bench-results-matched
H=/home/oz/projects/wt/p2s0b-707f132f/tools/matched-bench
E=(TOOLS_DIR=$H BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT=4 OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0)
env "${E[@]}" TARGET_DIR=$R/ozarchy-p2s0r-stage/a "$H/run-cell.sh" /home/oz/projects/wt/main-35e69b3 ozarchy-p2s0r-smoke-10m-a 10 30 76000
echo "a=$?" > $R/ozarchy-p2s0r-smoke.rc
cenv() {
    local o=$R/ozarchy-p2s0r-smoke-10m-c t=0 p f
    until grep -q '\] bench: ' "$o/run.log" 2>/dev/null; do sleep 1; t=$((t+1)); [ $t -gt 600 ] && return 1; done
    for p in $(cat /home/oz/torus-wsl-devnet/run/pids); do tr '\0' '\n' < /proc/$p/environ | grep BOOK_CF || echo unset; done > $R/ozarchy-p2s0r-smoke-c-env.txt
    for v in 0 1 2; do
        f=$(ls /home/oz/torus-wsl-devnet/data/val$v/OPTIONS-* | sort -V | tail -1)
        echo "val$v $(awk '/^\[CFOptions "cf_native_order_books"\]/{x=1} x && /target_file_size_base=/{print; exit}' "$f")"
    done >> $R/ozarchy-p2s0r-smoke-c-env.txt
}
cenv &
env "${E[@]}" TARGET_DIR=$R/ozarchy-p2s0r-stage/b "$H/run-cell.sh" /home/oz/projects/wt/main-d3ba3c0a ozarchy-p2s0r-smoke-10m-c 10 30 76000 TORUS_BOOK_CF_TARGET_FILE_MB=64
echo "c=$?" >> $R/ozarchy-p2s0r-smoke.rc
wait
echo done > $R/ozarchy-p2s0r-smoke.done
