#!/usr/bin/env bash
# 1 Hz val0 (:9161) funnel + exec phase-timer scraper -> CSV.
# Histograms expose _sum/_count; per-block avg = d(sum)/d(count).
OUT="$1"
echo "ts,committed,height,placed,matched,resting,exec_resting,lb_s,lb_c,root_s,root_c,sw_s,sw_c,evm_s,evm_c,fl_s,fl_c,db_s,db_c,execq" > "$OUT"
while true; do
  M=$(curl -s --max-time 2 http://127.0.0.1:9161/metrics)
  ts=$(date +%s)
  g() { printf '%s' "$M" | awk -v m="$1" '$1==m {print $2; f=1} END{if(!f) print 0}'; }
  echo "$ts,$(g torus_blocks_committed_total),$(g torus_block_height),$(g torus_orders_placed_accepted_total),$(g torus_orders_matched_total),$(g torus_orders_resting_total),$(g torus_exec_resting_orders),$(g torus_exec_load_books_seconds_sum),$(g torus_exec_load_books_seconds_count),$(g torus_exec_root_seconds_sum),$(g torus_exec_root_seconds_count),$(g torus_exec_state_write_seconds_sum),$(g torus_exec_state_write_seconds_count),$(g torus_exec_evm_resync_seconds_sum),$(g torus_exec_evm_resync_seconds_count),$(g torus_exec_flush_seconds_sum),$(g torus_exec_flush_seconds_count),$(g torus_exec_root_dirty_buckets_sum),$(g torus_exec_root_dirty_buckets_count),$(g torus_exec_queue_depth)" >> "$OUT"
  sleep 1
done
