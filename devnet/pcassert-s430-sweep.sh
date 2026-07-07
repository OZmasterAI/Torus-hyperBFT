#!/bin/bash
# PC-ASSERT TRIAGE (S430): run progress_and_validator_set_update_test in
# 8 parallel workers x up to 30 iterations each, until one hits the
# instrumented PC-ASSERT DIAG panic. Load from sibling workers recreates
# the "loaded box" scheduling jitter the failure needs.
set -u
cd "$(dirname "$0")/.."
BIN=$(ls -t target/debug/deps/progress_and_validator_set_update_test-* 2>/dev/null | grep -v '\.d$' | head -1)
OUT=devnet/pcassert-s430
mkdir -p "$OUT"
rm -f "$OUT"/repro.found "$OUT"/failures.txt "$OUT"/sweep.done
if [ -z "$BIN" ]; then echo "test binary not found" > "$OUT/failures.txt"; touch "$OUT/sweep.done"; exit 1; fi
echo "using $BIN" > "$OUT/binary.txt"
for w in $(seq 1 8); do
  (
    for it in $(seq 1 30); do
      [ -f "$OUT/repro.found" ] && break
      log="$OUT/w${w}-i${it}.log"
      timeout 300 "$BIN" --test-threads 1 --nocapture > "$log" 2>&1
      rc=$?
      if [ $rc -ne 0 ]; then
        echo "w$w i$it rc=$rc $log" >> "$OUT/failures.txt"
        if grep -aq "PC-ASSERT DIAG" "$log"; then
          echo "$log" >> "$OUT/repro.found"
        fi
      else
        rm -f "$log"
      fi
    done
  ) &
done
wait
touch "$OUT/sweep.done"
