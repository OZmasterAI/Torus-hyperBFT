#!/usr/bin/env bash
# 3 repetitions of parts 3-5, each started at 1-min load < 1.5.
set -u
R=$1
for rep in 1 2 3; do
  until awk '{exit !($1 < 1.5)}' /proc/loadavg; do sleep 15; done
  echo "rep=$rep start $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)"
  TMPDIR=$R/tmp UB_RG_ONLY=churn "$R/ubench.bin" --ignored --nocapture --test-threads=1 > "$R/rep$rep.log" 2>&1
  rc=$?
  echo "rep=$rep rc=$rc end $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)"
  [ $rc -eq 0 ] || { touch "$R/campaign.failed"; exit $rc; }
done
touch "$R/campaign.done"
