#!/usr/bin/env bash
# prof-sidecar.sh <cell_out_dir> <start_s_after_bench_start> <dur_s> <bench_bin> [markets]
# Waits for the harness "bench:" line, then perf-records val0 (whole process) for dur_s,
# snapshotting val0 /metrics + /proc stat around the window; after "bench exited" samples positions.
set -u
OUT=$1 START=$2 DUR=$3 BENCH=$4 MK=${5:-10}
PIDS=/home/oz/torus-wsl-devnet/run/pids
L=$OUT/prof-sidecar.log
lg() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >> "$L"; }
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && { echo "no bench start" >> "$L"; exit 1; }; done
lg "bench start seen"
P=$(head -1 "$PIDS"); lg "val0 pid=$P cmd=$(tr '\0' ' ' < /proc/$P/cmdline | cut -c1-300)"
lg "exe=$(readlink /proc/$P/exe) md5=$(md5sum < /proc/$P/exe | cut -d' ' -f1)"
sleep "$START"
snap() { # tag
    date +%s.%N > "$OUT/prof-$1.ts"
    curl -fsS -m 3 http://127.0.0.1:9161/metrics > "$OUT/prof-metrics-$1.txt"
    cat /proc/$P/stat > "$OUT/prof-stat-$1.txt"
    for d in /proc/$P/task/*; do echo "$(basename $d) $(cat $d/comm 2>/dev/null | tr ' ' _) $(awk '{print $14, $15}' $d/stat 2>/dev/null)"; done > "$OUT/prof-tasks-$1.txt"
}
snap before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) perf start"
perf record -e cycles:u -F 499 -g --call-graph fp -p "$P" -o "$OUT/perf.data" -- sleep "$DUR" > "$OUT/perf-record.log" 2>&1
lg "perf rc=$?"
snap after; lg "window done"
t=0; until grep -q '\] bench exited' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 600 ] && { lg "no bench exit"; exit 1; }; done
lg "bench exited; sampling positions"
"$BENCH" gen-accounts --offset 60 --count 5000 2>/dev/null | awk 'NR%25==1{print $2}' > "$OUT/pos-accounts.txt"
python3 /home/oz/bench-results-matched/ozarchy-14236fa-tools/positions.py "$OUT/pos-accounts.txt" "$MK" http://127.0.0.1:8646 > "$OUT/positions.json" 2>>"$L"
lg "positions rc=$? $(head -c 300 "$OUT/positions.json")"
