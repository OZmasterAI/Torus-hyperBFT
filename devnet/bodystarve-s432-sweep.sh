#!/bin/bash
# S432 body-starvation fix prove: 8 workers x 3 iters of progress_and_vsu under load.
# Baseline (S430, identical methodology at this tip): ~84% fail rate, all rc=101
# poll timeouts = header-first body-starvation livelock. Pass bar: fail rate
# collapses (target <=20%), still ZERO rc=134 (S430 PC-assert stays fixed).
set -u
cd "$(dirname "$0")/.."
BIN=$(ls -t target/debug/deps/progress_and_validator_set_update_test-* 2>/dev/null | grep -v '\.d$' | head -1)
OUT=devnet/bodystarve-s432-sweep
mkdir -p "$OUT"
rm -f "$OUT"/failures.txt "$OUT"/sweep.done
echo "using $BIN" > "$OUT/binary.txt"
for w in $(seq 1 8); do
  (
    for it in $(seq 1 3); do
      log="$OUT/w${w}-i${it}.log"
      timeout 300 "$BIN" --test-threads 1 --nocapture > "$log" 2>&1
      rc=$?
      if [ $rc -ne 0 ]; then
        echo "w$w i$it rc=$rc" >> "$OUT/failures.txt"
        grep -aq "must be correct by construction" "$log" && echo "w$w i$it ASSERT-FIRED" >> "$OUT/failures.txt"
      else
        rm -f "$log"
      fi
    done
  ) &
done
wait
touch "$OUT/sweep.done"
