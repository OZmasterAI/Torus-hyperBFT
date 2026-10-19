#!/usr/bin/env bash
# 1 Hz Prometheus funnel scraper for val0 (:9161) -> CSV
OUT="$1"
echo "ts,actions_processed,placed_accepted,matched,resting,rej_margin,rej_book,rej_cancelled,rej_other,self_trade_cancels,cancelled_partial_fill,blocks_committed,block_height,exec_queue_depth" > "$OUT"
while true; do
  M=$(curl -s --max-time 2 http://127.0.0.1:9161/metrics)
  ts=$(date +%s)
  g() { echo "$M" | awk -v m="$1" '$1==m {print $2; f=1} END {if(!f) print 0}'; }
  echo "$ts,$(g torus_native_actions_processed_total),$(g torus_orders_placed_accepted_total),$(g torus_orders_matched_total),$(g torus_orders_resting_total),$(g torus_orders_rejected_margin_total),$(g torus_orders_rejected_book_total),$(g torus_orders_rejected_cancelled_total),$(g torus_orders_rejected_other_total),$(g torus_orders_self_trade_cancels_total),$(g torus_orders_cancelled_partial_fill_total),$(g torus_blocks_committed_total),$(g torus_block_height),$(g torus_exec_queue_depth)" >> "$OUT"
  sleep 1
done
