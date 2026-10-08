#!/usr/bin/env bash
# read-gas-stall: parts 4-5 (single-market churn + 50-market churn, 100 levels/market/block,
# 200 blocks), 3 reps, variants interleaved per rep, each started at 1-min load < 1.5.
# nocompact = compact build + trigger disabled (nocompact/nocompact.patch).
set -u
B=$1
VARIANTS="nocompact:0 compact:0 compact:16 compact:8 compact:4"
for rep in 1 2 3; do
  for v in $VARIANTS; do
    bin=${v%%:*}; mb=${v##*:}
    name=$bin-$([ "$mb" = 0 ] && echo 64 || echo "$mb")
    mkdir -p "$B/$name/tmp"
    until awk '{exit !($1 < 1.5)}' /proc/loadavg; do sleep 15; done
    echo "rep=$rep $name start $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)"
    if [ "$mb" = 0 ]; then unset TORUS_BOOK_CF_TARGET_FILE_MB; else export TORUS_BOOK_CF_TARGET_FILE_MB=$mb; fi
    TMPDIR=$B/$name/tmp TORUS_ROCKSDB_STATS=2 UB_RG_ONLY=churn UB_RG_MCHURN_BLOCKS=200 UB_RG_MCHURN_ORDERS=100 \
      "$B/$bin/ubench.bin" --ignored --nocapture --test-threads=1 > "$B/$name/rep$rep.log" 2>&1
    rc=$?
    echo "rep=$rep $name rc=$rc end $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)"
    [ $rc -eq 0 ] || exit $rc
  done
done
