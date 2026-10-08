#!/usr/bin/env bash
# Reproduce the margin-phase breakdown (ozarchy-margin-c7-analysis.md). Read-only on the cells.
set -euo pipefail
cd /home/oz/bench-results-matched
T=ozarchy-margin-c7-tools
CRAB=/home/oz/.cargo-target-82bd1a4-prof/release/torus-node      # md5 2783579b, build-id be880ac0
MAIN=/home/oz/projects/wt/main/target/release/torus-node          # build-id 0c1dc011 (= c3pf1-300m-main-r1 perf.data)
MAIN_SCRIPT=ozarchy-gap-after-c7-tools/ozarchy-c3pf1-300m-main-r1.exec-script.gz
for c in r1 r2; do
  s=$T/ozarchy-82bd1a4-c7-$c.exec-script.gz
  [ -s "$s" ] || perf script -F comm,tid,period,ip,sym,symoff --no-inline --comms torus-execution \
      -i ozarchy-82bd1a4-c7-$c/perf.data 2>/dev/null | gzip -1 > "$s"
  python3 $T/margin.py ozarchy-82bd1a4-c7-$c $CRAB "$s" crab '<NativeExecutor>::prepare_one' get_position position_px \
    cached_sums build_with phase2_reservation_basis same_batch_bid_top_ups can_rest_shape account_check open_order_counts \
    d2_pool_takers take_open_slot try_reserve_for_qty_cfg placement_need stitch_outcome reduce_only_positions_for \
    phase2_bid_floors position_terms > $T/c7-$c.txt
  python3 $T/margin.py ozarchy-82bd1a4-c7-$c $CRAB "$s" crab '<NativeExecutor>::execute_batch_phases' maker_position_px \
    maker_free liq_view > $T/ebp-c7-$c.txt
  python3 $T/cats.py ozarchy-82bd1a4-c7-$c $CRAB "$s" crab > $T/cats-c7-$c.txt
  python3 $T/hashsites.py ozarchy-82bd1a4-c7-$c $CRAB "$s" crab > $T/hash-c7-$c.txt
done
python3 $T/margin.py ozarchy-c3pf1-300m-main-r1 $MAIN $MAIN_SCRIPT main take_open_slot phase2_reservation_basis \
  open_order_counts try_reserve_for_qty_cfg '<NativeExecutor>::execute_batch_phases' > $T/main-r1.txt
python3 $T/cats.py ozarchy-c3pf1-300m-main-r1 $MAIN $MAIN_SCRIPT main > $T/cats-main-r1.txt
python3 $T/hashsites.py ozarchy-c3pf1-300m-main-r1 $MAIN $MAIN_SCRIPT main > $T/hash-main-r1.txt
