#!/usr/bin/env bash
# perf2-sidecar.sh <cell_out_dir> <mode prof|stat>  (p2s0y second layout, after the 5-event inherited perf record of
# ipc-sidecar.sh cost ~37% matched/s and ~16 cores of sys time: every short-lived worker thread inherits the events)
# Windows as ipc-sidecar.sh: W1 35..80 s after the harness "bench:" line, W2 82..102 s. val0 only. User space only (:u).
#   prof: W1 perf record -e cycles:u -F 499 -g --call-graph fp -p val0 -o perf.data (= prof-sidecar.sh, sections 22/25)
#         W2 perf stat -t <exec tid> --no-inherit  {cycles,instructions,L1-dcache-load-misses,DRAM fill,L3 fill}
#   stat: W1 perf record -t <exec tid> --no-inherit -F 499 --call-graph fp -o perf-w1.data, the same 5 events sampled
#         (exec thread only: pass B and the exec-thread glue; no thread inherits, so no spawn cost)
#         W2 perf stat -p val0 (whole process, inherit) {cycles,instructions,cache-references,cache-misses,L1-dcache-load-misses}
# exec tid = the torus-execution task of val0 with the most CPU at W1 start (workers share the comm but live < 1 s).
set -u
OUT=$1 MODE=$2
PIDS=/home/oz/torus-wsl-devnet/run/pids
L=$OUT/perf2-sidecar.log
EV5=cycles:u,instructions:u,L1-dcache-load-misses:u,ls_dmnd_fills_from_sys.mem_io_local:u,ls_dmnd_fills_from_sys.ext_cache_local:u
lg() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >> "$L"; }
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && { echo "no bench start" >> "$L"; exit 1; }; done
lg "bench start seen mode=$MODE"
P=$(head -1 "$PIDS"); lg "val0 pid=$P cmd=$(tr '\0' ' ' < /proc/$P/cmdline | cut -c1-200)"
lg "exe=$(readlink /proc/$P/exe) md5=$(md5sum < /proc/$P/exe | cut -d' ' -f1)"
snap() { # tag
    date +%s.%N > "$OUT/prof-$1.ts"
    curl -fsS -m 3 http://127.0.0.1:9161/metrics > "$OUT/prof-metrics-$1.txt"
    cat /proc/$P/stat > "$OUT/prof-stat-$1.txt"
    for d in /proc/$P/task/*; do echo "$(basename $d) $(cat $d/comm 2>/dev/null | tr ' ' _) $(awk '{print $14, $15}' $d/stat 2>/dev/null)"; done > "$OUT/prof-tasks-$1.txt"
}
sleep 35
snap w1-before
ET=$(awk '$2 == "torus-execution" {print $3 + $4, $1}' "$OUT/prof-tasks-w1-before.txt" | sort -nr | head -1 | cut -d' ' -f2)
lg "exec tid=$ET ($(awk -v t="$ET" '$1 == t' "$OUT/prof-tasks-w1-before.txt")); torus-execution tasks now: $(awk '$2 == "torus-execution"' "$OUT/prof-tasks-w1-before.txt" | wc -l); top 3: $(awk '$2 == "torus-execution" {print $3 + $4, $1}' "$OUT/prof-tasks-w1-before.txt" | sort -nr | head -3 | tr '\n' ';')"
echo "$ET" > "$OUT/exec-tid.txt"
lg "load=$(cut -d' ' -f1-3 /proc/loadavg) w1 start"
if [ "$MODE" = prof ]; then
    perf record -e cycles:u -F 499 -g --call-graph fp -p "$P" -o "$OUT/perf.data" -- sleep 45 > "$OUT/perf-record.log" 2>&1
else
    perf record -e "$EV5" -F 499 --call-graph fp --no-inherit -t "$ET" -o "$OUT/perf-w1.data" -- sleep 45 > "$OUT/perf-w1-record.log" 2>&1
fi
lg "w1 rc=$?"
snap w1-after
# buckets2.py / inl.py read prof-{before,after}*: point them at W1
for a in before after; do
    ln -sf "prof-w1-$a.ts" "$OUT/prof-$a.ts"; ln -sf "prof-metrics-w1-$a.txt" "$OUT/prof-metrics-$a.txt"; ln -sf "prof-stat-w1-$a.txt" "$OUT/prof-stat-$a.txt"
done
sleep 2
snap w2-before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) w2 start; exec tid $ET alive: $([ -d /proc/$P/task/$ET ] && echo yes || echo NO)"
if [ "$MODE" = prof ]; then
    perf stat -x, -o "$OUT/perf-stat-w2.txt" --no-inherit -t "$ET" -e "$EV5" -- sleep 20 > "$OUT/perf-stat-w2.log" 2>&1
else
    perf stat -x, -o "$OUT/perf-stat-w2.txt" -p "$P" \
        -e cycles:u,instructions:u,cache-references:u,cache-misses:u,L1-dcache-load-misses:u -- sleep 20 > "$OUT/perf-stat-w2.log" 2>&1
fi
lg "w2 rc=$?"
snap w2-after; lg "windows done"
