#!/usr/bin/env bash
# task-sampler.sh <cell_out_dir>: 1 Hz per-thread utime/stime (ticks) of val0 -> tasks.txt
# lines "ts tid comm utime stime"; plus "ts PROC - utime stime" for the whole process (incl. exited threads).
# starts at the harness "bench:" line, stops when summary.json appears or val0 exits.
set -u
OUT=$1; PIDS=/home/oz/torus-wsl-devnet/run/pids
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && exit 1; done
P=$(head -1 "$PIDS")
for i in $(seq 1500); do
    [ -f "$OUT/summary.json" ] && break
    [ -r "/proc/$P/stat" ] || break
    ts=$EPOCHSECONDS
    st=$(cat "/proc/$P/stat" 2>/dev/null) || break; st=${st##*) }; set -- $st; echo "$ts PROC - ${12} ${13}"
    for d in /proc/$P/task/*; do
        s=$(cat "$d/stat" 2>/dev/null) || continue
        c=${s#*(}; c=${c%%)*}; s=${s##*) }; set -- $s
        echo "$ts ${d##*/} ${c// /_} ${12} ${13}"
    done
    sleep 1
done >> "$OUT/tasks.txt"
