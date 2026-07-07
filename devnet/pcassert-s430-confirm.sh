#!/bin/bash
# S430 final confirmation: fix + Finding-4 restructure. 8 workers x 3 iters.
# Pass bar: ZERO rc=134 (assert SIGABRT), fail rate <= historical ~84% baseline
# (rc=101 poll timeouts are the PRE-EXISTING body-starvation livelock).
set -u
cd "$(dirname "$0")/.."
BIN=$(ls -t target/debug/deps/progress_and_validator_set_update_test-* 2>/dev/null | grep -v '\.d$' | head -1)
OUT=devnet/pcassert-s430-confirm
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
