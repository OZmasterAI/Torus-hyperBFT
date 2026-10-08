#!/usr/bin/env bash
# stat-sidecar.sh <cell_out_dir> <start_s> <dur1_s> <gap_s> <dur2_s>
# perf stat twin of ozarchy-ipc-tools/ipc-sidecar.sh (same windows, same snapshots, file names perf-stat-w1/w2.txt):
# counts (not samples) on val0, whole process; perf stat -p also counts threads created after attach (checked
# 2026-10-08 with a python thread test). 5 user-space events per window (Zen 3: 5 free core counters with the NMI
# watchdog on, so no multiplexing; LLC-loads / LLC-load-misses are <not supported> on this host).
#   W1: cycles, instructions, L1-dcache-load-misses, ls_dmnd_fills_from_sys.mem_io_local (DRAM demand fills), ls_l1_d_tlb_miss.all
#   W2: cycles, instructions, cache-references, cache-misses, ls_dmnd_fills_from_sys.ext_cache_local (L2 miss served by L3)
set -u
OUT=$1 START=$2 D1=$3 GAP=$4 D2=$5
PIDS=/home/oz/torus-wsl-devnet/run/pids
L=$OUT/stat-sidecar.log
lg() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >> "$L"; }
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && { echo "no bench start" >> "$L"; exit 1; }; done
lg "bench start seen"
P=$(head -1 "$PIDS"); lg "val0 pid=$P cmd=$(tr '\0' ' ' < /proc/$P/cmdline | cut -c1-200)"
lg "exe=$(readlink /proc/$P/exe) md5=$(md5sum < /proc/$P/exe | cut -d' ' -f1)"
snap() { # tag
    date +%s.%N > "$OUT/prof-$1.ts"
    curl -fsS -m 3 http://127.0.0.1:9161/metrics > "$OUT/prof-metrics-$1.txt"
    cat /proc/$P/stat > "$OUT/prof-stat-$1.txt"
    for d in /proc/$P/task/*; do echo "$(basename $d) $(cat $d/comm 2>/dev/null | tr ' ' _) $(awk '{print $14, $15}' $d/stat 2>/dev/null)"; done > "$OUT/prof-tasks-$1.txt"
}
sleep "$START"
snap w1-before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) perf stat w1 start"
perf stat -x, -o "$OUT/perf-stat-w1.txt" -p "$P" \
    -e cycles:u,instructions:u,L1-dcache-load-misses:u,ls_dmnd_fills_from_sys.mem_io_local:u,ls_l1_d_tlb_miss.all:u \
    -- sleep "$D1" > "$OUT/perf-stat-w1.log" 2>&1
lg "perf stat w1 rc=$?"
snap w1-after
sleep "$GAP"
snap w2-before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) perf stat w2 start"
perf stat -x, -o "$OUT/perf-stat-w2.txt" -p "$P" \
    -e cycles:u,instructions:u,cache-references:u,cache-misses:u,ls_dmnd_fills_from_sys.ext_cache_local:u \
    -- sleep "$D2" > "$OUT/perf-stat-w2.log" 2>&1
lg "perf stat w2 rc=$?"
snap w2-after; lg "windows done"
