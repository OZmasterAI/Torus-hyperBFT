#!/usr/bin/env bash
# ozarchy-p3s: Phase 3 SIZING campaign on the tw baseline (owner 18c s109), node + bench from main fd9e5dfa.
#   baseline tw = TORUS_TRADE_HISTORY=0 + TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048; TORUS_ROCKSDB_WAL_COMPRESSION and
#   TORUS_ROCKSDB_APPEND_CF_CODEC stay UNSET (the wrapper unsets them; the knob check fails if they are set on a node).
# Derived from ozarchy-acc3-campaign.sh (same harness tools/matched-bench at 9b7e29b2, same helpers, same cell shape:
# 300 markets, MAX_IN_FLIGHT N, OPEN_ORDER_BUDGET 900, 120 s cells, CLEAN=1 = fresh data dir per cell).
# Cells (tw only):
#   tw-r1      120 s, no perf. val0 WAL KEPT: every val0 *.log is hard-linked into wal-val0-keep/ as it appears (same
#              inode, so RocksDB's unlink after the memtable flush cannot drop the bytes). Misses: wal-val0-keep/miss.txt.
#              Task 1/2 input (coalescing per window, layer and checkpoint size). Also CPU sample 1.
#   tw-r2      120 s, no perf. CPU sample 2 on the fd9e5dfa node.
#   tw-old1    120 s, no perf, the ACC3 node (2ede76eb, fa8b646f) with the same tw knobs, same day: control for the -6% gate
#              (fd9e5dfa differs from fa8b646f in crates/torus-bridge/src/native_executor.rs).
#   tw-c1..c3  420 s, SIGKILL val1 at bench+60,180,300 s (s75 multi-crash, CRASH_KILL_AT_S list), restart from the same
#              data dir each time. n=3 cells = 9 restarts. Task 3. crash-freeze.py per cell -> crash-freeze.json.
#   tw-p1, tw-p2  120 s with perf (cycles:u 499 Hz -g fp, val0 whole process, 45 s from bench+35 s). Task 4 shares.
# Hard checks (CHECK FAIL stops the campaign): node exe md5, env (knobs empty / set per arm), book CF, codec and WAL option,
#   trie default, max_total_wal_size, zero trade data rows on tw, restarted val1 exe md5 and knobs.
# Known false FAIL: run-cell's crash gate on the INFO line 'state hash fail-stop ... on=false' (recorded, not used).
# A non-zero run-cell rc is recorded and the run goes on.
set -u
N=${1:-}
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "usage: $0 N   (MAX_IN_FLIGHT, e.g. 4)"; exit 2; }
unset TORUS_MODEL_PROFILE CRASH_KILL_AT_S TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_TRADE_HISTORY TORUS_ROCKSDB_MAX_TOTAL_WAL_MB
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
DEVNET=/home/oz/torus-wsl-devnet
WT_NEW=/home/oz/projects/wt/p3s-fd9e5dfa
SHA_NEW=fd9e5dfa9f288134eda6e3995c3dfdb305452d0c
MD5_NEW=fb460a0c59ff825f987a146f9e8a47db
SHA_HARNESS=9b7e29b2e2033babbe03078421e43a6c066cd53d
WT_H=/home/oz/projects/wt/p3s0r-9b7e29b2
T3=$R/ozarchy-p3s0r-tools
TOOLS=$WT_H/tools/matched-bench
HARNESS=$TOOLS/run-cell.sh
DETACH=$TOOLS/campaign/detach.sh
T=$R/ozarchy-14236fa-tools
PYCHK=$R/ozarchy-acc-codec-check.py
LSM=$R/ozarchy-acc3-tools/ozarchy-acc3-lsm.py
RSTPY=$R/ozarchy-acc3-tools/ozarchy-acc3-restart.py
TD=$R/ozarchy-p3s-stage/n
BENCH=$TD/release/bench-throughput
MD5_BUILD=$R/ozarchy-p3s-build/md5s.txt
# control: the acc3 node + bench (fa8b646f), same tw knobs
OLD_TD=$R/ozarchy-acc-stage/n
OLD_MD5=2ede76ebd7a02da7c25f928e934e7c92
OLD_BENCH_MD5=a33d82f5
MK=300
P=ozarchy-p3s-300m
NM=$(md5sum < "$TD/release/torus-node" | cut -c1-8)
BENCH_MD5=$(md5sum < "$BENCH" 2>/dev/null | cut -d' ' -f1)
KNOBS="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_BOOK_ROWS="
declare -A XE KNOBX WALCAP
XE[tw]="$KNOBS TORUS_TRADE_HISTORY=0 TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048"
KNOBX[tw]="TORUS_TRADE_HISTORY=0 TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048"
WALCAP[tw]=2147483648
CELLS=${CELLS:-"tw:tw-r1:120:keep tw:tw-r2:120 tw:tw-old1:120 tw:tw-c1:420:crash:60,180,300 tw:tw-c2:420:crash:60,180,300 tw:tw-c3:420:crash:60,180,300 tw:tw-p1:120 tw:tw-p2:120"}
SIGTRACE_RE='perf record -e signal:signal_generate'
