#!/usr/bin/env bash
# 3 repetitions of the read precompile gas µbench, each started at 1-min load < 1.5.
set -u
R=/home/oz/bench-results-matched/read-precompile-gas
for rep in 1 2 3; do
  until awk '{exit !($1 < 1.5)}' /proc/loadavg; do sleep 15; done
  echo "rep=$rep start $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)"
  "$R/ubench_read_precompile_gas.bin" --ignored --nocapture --test-threads=1 > "$R/rep$rep.log" 2>&1
  rc=$?
  echo "rep=$rep rc=$rc end $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)"
  [ $rc -eq 0 ] || exit $rc
done
