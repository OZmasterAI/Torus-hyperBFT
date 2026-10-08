#!/usr/bin/env bash
# cpu-sampler.sh <cell_out_dir>: 1 Hz utime/stime (clock ticks) of the 3 node pids -> cpu-ticks.txt
# "ts pid utime stime"; starts at the harness "bench:" line, stops when summary.json appears or nodes exit.
set -u
OUT=$1; PIDS=/home/oz/torus-wsl-devnet/run/pids
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && exit 1; done
mapfile -t P < <(head -3 "$PIDS")
for i in $(seq 1500); do
    [ -f "$OUT/summary.json" ] && break
    alive=0
    for p in "${P[@]}"; do
        read -r st < "/proc/$p/stat" 2>/dev/null || continue; alive=1
        st=${st##*) }; set -- $st; echo "$EPOCHSECONDS $p ${12} ${13}"
    done
    [ $alive = 0 ] && break
    sleep 1
done >> "$OUT/cpu-ticks.txt"
