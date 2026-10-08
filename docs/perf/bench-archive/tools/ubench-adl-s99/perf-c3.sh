#!/usr/bin/env bash
set -u
BIN=/home/oz/.cargo-target-adl-budget/release/deps/ubench_adl-5626de9bfbcc1f30
D=/home/oz/bench-results-matched/ubench-adl-s99
cd /home/oz/projects/wt/adl-budget
until awk '{exit !($1<1.5)}' /proc/loadavg; do sleep 15; done
echo "== perf-c3 load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T)"
env UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10 perf record -F 4999 --call-graph fp -o "$D/perf-c3.data" -- "$BIN" --ignored --nocapture > "$D/perf-c3.log" 2>&1
echo "   perf rc=$?"
echo "== end $(date +%T)"
