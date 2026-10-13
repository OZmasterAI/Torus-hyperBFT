#!/usr/bin/env bash
# cloidprof-sidecar.sh <cell_out_dir>   (ozarchy-p3cloidprof: where the cloid engine cost comes from; 18c)
# Copy of ozarchy-p2s0y-tools/perf2-sidecar.sh "prof" layout, moved from val0 (serves the bench RPC and the headline
# numbers) to val1 (pids line 2, metrics :9162). Same windows, user space only (:u), no sudo (perf_event_paranoid=2):
#   W1 35..80 s after the harness "bench:" line: perf record -e cycles:u -F 499 -g --call-graph fp -p val1 -o perf.data
#      (whole process, inherited: exec thread + short-lived market workers; cycles only = sections 22/25, ~5% cost)
#   W2 82..102 s: perf record -t <exec tid> --no-inherit -F 499 --call-graph fp -o perf-w2.data with 5 sampled events
#      {cycles, instructions, L1-dcache-load-misses, DRAM demand fills (= LLC miss; LLC-load-misses is not supported on
#      Zen 3), demand fills from the other CCD's L3}: exec thread only, no inherit (no worker spawn cost).
# exec tid = the torus-execution task of val1 with the most CPU at W1 start (workers share the comm but live < 1 s).
# Snapshots (val1 /metrics, /proc stat, per-task utime) around each window: prof-{metrics,stat,tasks}-w{1,2}-{before,after}.
set -u
OUT=$1
PIDS=/home/oz/torus-wsl-devnet/run/pids
PORT=9162
L=$OUT/cloidprof-sidecar.log
EV5=cycles:u,instructions:u,L1-dcache-load-misses:u,ls_dmnd_fills_from_sys.mem_io_local:u,ls_dmnd_fills_from_sys.ext_cache_local:u
lg() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >> "$L"; }
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && { echo "no bench start" >> "$L"; exit 1; }; done
lg "bench start seen (val1 profile)"
P=$(sed -n 2p "$PIDS"); lg "val1 pid=$P cmd=$(tr '\0' ' ' < /proc/$P/cmdline | cut -c1-200)"
grep -q -- 'val1' /proc/$P/cmdline || lg "WARN: pid $P cmdline does not mention val1"
lg "exe=$(readlink /proc/$P/exe) md5=$(md5sum < /proc/$P/exe | cut -d' ' -f1)"
snap() { # tag
    date +%s.%N > "$OUT/prof-$1.ts"
    curl -fsS -m 3 http://127.0.0.1:$PORT/metrics > "$OUT/prof-metrics-$1.txt"
    cat /proc/$P/stat > "$OUT/prof-stat-$1.txt"
    for d in /proc/$P/task/*; do echo "$(basename $d) $(cat $d/comm 2>/dev/null | tr ' ' _) $(awk '{print $14, $15}' $d/stat 2>/dev/null)"; done > "$OUT/prof-tasks-$1.txt"
}
sleep 35
snap w1-before
ET=$(awk '$2 == "torus-execution" {print $3 + $4, $1}' "$OUT/prof-tasks-w1-before.txt" | sort -nr | head -1 | cut -d' ' -f2)
lg "exec tid=$ET ($(awk -v t="$ET" '$1 == t' "$OUT/prof-tasks-w1-before.txt")); torus-execution tasks now: $(awk '$2 == "torus-execution"' "$OUT/prof-tasks-w1-before.txt" | wc -l); top 3: $(awk '$2 == "torus-execution" {print $3 + $4, $1}' "$OUT/prof-tasks-w1-before.txt" | sort -nr | head -3 | tr '\n' ';')"
echo "$ET" > "$OUT/exec-tid.txt"
lg "load=$(cut -d' ' -f1-3 /proc/loadavg) w1 start"
perf record -e cycles:u -F 499 -g --call-graph fp -p "$P" -o "$OUT/perf.data" -- sleep 45 > "$OUT/perf-record.log" 2>&1
lg "w1 rc=$?"
snap w1-after
# buckets2.py / inl.py read prof-{before,after}*: point them at W1
for a in before after; do
    ln -sf "prof-w1-$a.ts" "$OUT/prof-$a.ts"; ln -sf "prof-metrics-w1-$a.txt" "$OUT/prof-metrics-$a.txt"; ln -sf "prof-stat-w1-$a.txt" "$OUT/prof-stat-$a.txt"
done
sleep 2
snap w2-before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) w2 start; exec tid $ET alive: $([ -d /proc/$P/task/$ET ] && echo yes || echo NO)"
perf record -e "$EV5" -F 499 --call-graph fp --no-inherit -t "$ET" -o "$OUT/perf-w2.data" -- sleep 20 > "$OUT/perf-w2-record.log" 2>&1
lg "w2 rc=$?"
snap w2-after; lg "windows done"
