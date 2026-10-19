#!/usr/bin/env bash
# 1 Hz val0 (:9161) funnel + exec phase-timer scraper -> CSV.
# Histograms expose _sum/_count; per-block avg = d(sum)/d(count).
#
# UNTIMED RESIDUAL. The flush timer (app.rs:1412-1501) wraps the root, state-write
# and evm-resync timers, so the work no phase timer covers is
#   residual = d(fl_s) - d(root_s) - d(sw_s) - d(evm_s)
# (NOT fl_s - root_s - sw_s: exec_evm_resync is inside the flush span too, and
# omitting it inflates the residual). All four components are already scraped.
# The residual contains `commit_finals` (backend.rs:737), which runs AFTER the
# root timer stops (backend.rs:704) and does per-bucket Theta(K/G) work — so
# root time systematically under-reports as state grows. Normalise the residual
# by bscan / mc_* below to separate cache churn from real growth.
#
# MEMBER CACHE. mc_evict > 0 is the early warning that total chain state has
# outgrown TORUS_BUCKET_MEMBER_CACHE_MB: the bucket member cache degrades on a
# knee, not a cliff, so the first eviction gives a full growth cycle of lead
# time before scans (bscan) start climbing. Counters expose with a _total suffix.
OUT="$1"
echo "ts,committed,height,placed,matched,resting,exec_resting,lb_s,lb_c,root_s,root_c,sw_s,sw_c,evm_s,evm_c,fl_s,fl_c,db_s,db_c,execq,bscan,mc_hit,mc_miss,mc_evict,mc_resident" > "$OUT"
while true; do
  M=$(curl -s --max-time 2 http://127.0.0.1:9161/metrics)
  ts=$(date +%s)
  g() { printf '%s' "$M" | awk -v m="$1" '$1==m {print $2; f=1} END{if(!f) print 0}'; }
  echo "$ts,$(g torus_blocks_committed_total),$(g torus_block_height),$(g torus_orders_placed_accepted_total),$(g torus_orders_matched_total),$(g torus_orders_resting_total),$(g torus_exec_resting_orders),$(g torus_exec_load_books_seconds_sum),$(g torus_exec_load_books_seconds_count),$(g torus_exec_root_seconds_sum),$(g torus_exec_root_seconds_count),$(g torus_exec_state_write_seconds_sum),$(g torus_exec_state_write_seconds_count),$(g torus_exec_evm_resync_seconds_sum),$(g torus_exec_evm_resync_seconds_count),$(g torus_exec_flush_seconds_sum),$(g torus_exec_flush_seconds_count),$(g torus_exec_root_dirty_buckets_sum),$(g torus_exec_root_dirty_buckets_count),$(g torus_exec_queue_depth),$(g torus_exec_root_bucket_scans_total),$(g torus_member_cache_hits_total),$(g torus_member_cache_misses_total),$(g torus_member_cache_evictions_total),$(g torus_member_cache_resident_buckets)" >> "$OUT"
  sleep 1
done
