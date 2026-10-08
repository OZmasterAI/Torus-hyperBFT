#!/usr/bin/env bash
# ipc-sidecar.sh <cell_out_dir> <start_s> <dur1_s> <gap_s> <dur2_s>
# Waits for the harness "bench:" line, sleeps start_s, then perf-records val0 (whole process,
# follows new threads via inherit) in two windows, each with 5 independently sampled user-space
# events (Zen 3 core PMU: 5 usable counters with the NMI watchdog on, so no multiplexing):
#   W1 (perf-w1.data): cycles, instructions, L1-dcache-load-misses, ls_dmnd_fills_from_sys.mem_io_local (DRAM demand fills = L3 miss), branch-misses
#   W2 (perf-w2.data): cycles, instructions, ls_dmnd_fills_from_sys.ext_cache_local (demand L2 miss served by L3/other CCX), ls_l1_d_tlb_miss.all, ic_tag_hit_miss.instruction_cache_miss
# Event counts per symbol/comm = sum of sample periods. /metrics + /proc stat + task list snapshotted around each window.
set -u
OUT=$1 START=$2 D1=$3 GAP=$4 D2=$5
PIDS=/home/oz/torus-wsl-devnet/run/pids
L=$OUT/ipc-sidecar.log
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
snap w1-before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) perf w1 start"
perf record -F 499 --call-graph fp -p "$P" -o "$OUT/perf-w1.data" \
    -e cycles:u -e instructions:u -e L1-dcache-load-misses:u -e ls_dmnd_fills_from_sys.mem_io_local:u -e branch-misses:u \
    -- sleep "$D1" > "$OUT/perf-w1-record.log" 2>&1
lg "perf w1 rc=$?"
snap w1-after
sleep "$GAP"
snap w2-before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) perf w2 start"
perf record -F 499 --call-graph fp -p "$P" -o "$OUT/perf-w2.data" \
    -e cycles:u -e instructions:u -e ls_dmnd_fills_from_sys.ext_cache_local:u -e ls_l1_d_tlb_miss.all:u -e ic_tag_hit_miss.instruction_cache_miss:u \
    -- sleep "$D2" > "$OUT/perf-w2-record.log" 2>&1
lg "perf w2 rc=$?"
snap w2-after; lg "windows done"
