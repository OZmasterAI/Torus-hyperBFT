#!/usr/bin/env bash
# summarize.sh <case> -> SUMMARY text for an R01b case
c=$1; R=/home/oz/r01-fault/results/$c
s() { sed -E 's/\x1b\[[0-9;]*m//g' "$1"; }
L=$(s "$R/v3.log" | grep -an '^===== start v3' | sed -n 2p | cut -d: -f1)
pre() { s "$R/v3.log" | head -n $((L-1)); }
post() { s "$R/v3.log" | tail -n +$L; }
echo "case:               $c"
grep -m2 -E 'CASE=|node md5' "$R/events.log" | sed 's/^/events:             /'
echo "attempts:"; grep -E '^=== attempt' "$R/attempts.txt" | sed 's/^/  /'
fe=$(pre | grep -aE 'No space|panicked' | grep -aoE '^[0-9T:.-]+Z' | head -1); [ -z "$fe" ] && fe=$(pre | grep -aE 'No space|panicked' -m1 -B0 | head -1)
echo "FATAL/panic lines before the first fill error (warm-up): $(pre | grep -aE '^[0-9T:.-]+Z.*(FATAL|panicked)' | awk -v t="$fe" '$1 < t' | grep -c .)"
echo "first failure line: $(pre | grep -aE 'ERROR|panicked|FATAL' | head -1 | cut -c1-260)"
echo "first FATAL line:   $(pre | grep -a 'FATAL' | head -1 | cut -c1-260)"
echo "all FATAL lines:"; pre | grep -a 'FATAL' | cut -c1-200 | sed 's/^/  /'
echo "first error ts:     $(pre | grep -aE 'ERROR|panicked' | grep -aoE '^[0-9T:.-]+Z' | head -1)"
echo "last FATAL ts:      $(pre | grep -a 'FATAL' | grep -aoE '^[0-9T:.-]+Z' | tail -1)"
echo "fill logged:        $(grep -m1 'attempt 1: v3 applied' "$R/events.log")"
echo "last block done before exit (marker): $(pre | grep -a 'block done height=' | tail -1 | grep -o 'height=[0-9]*')"
echo "last executing before exit:           $(pre | grep -a 'executing finalized block height=' | tail -1 | grep -o 'height=[0-9]*')"
echo "restart gap line:   $(post | grep -a 'execution gap' | head -1 | cut -c1-200)"
echo "first exec after restart (H):  $(post | grep -a 'executing finalized block height=' | head -1 | grep -o 'height=[0-9]*')"
echo "FATAL/panic after restart (run-case): $(post | grep -acE 'FATAL|panicked')"
echo "run-case RESULT:    $(grep RESULT "$R/events.log" | head -2 | tr '\n' ' ')"
echo "check-hashes:       $(grep RESULT "$R.check-hashes.log")  ($(grep -c '^h=' "$R.check-hashes.log") checkpoints)"
echo "check-hashes v3 FATAL/panic: $(s "$R/v3.log" | awk -v n=$(s "$R/v3.log" | grep -an '^===== start v3' | sed -n 3p | cut -d: -f1) 'NR>=n' | grep -acE 'FATAL|panicked')"
