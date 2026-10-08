#!/usr/bin/env bash
# prof-sidecar-2win.sh <cell_out_dir> <start_s_after_bench_start> <dur_s> <drain_dur_s> <drain_freq_hz>
# Copy of ozarchy-14236fa-tools/prof-sidecar.sh for the p2s0 crab-w10 cell (Phase 2 step 0, rows 77/78).
# Two perf windows on val0 (whole process, cycles:u -F 499 fp, same as the original):
#   load  : <start_s> after the harness "bench:" line, <dur_s> long -> perf.data,
#           prof-{before,after}.{ts,clk,lastpid} prof-metrics-/prof-stat-/prof-tasks-{before,after}.txt (original names)
#   drain : starts at the harness "bench exited rc=" line (0.2 s poll), <drain_dur_s> long at -F <drain_freq_hz> -> perf.drain.data
#           (higher rate than 499: oracle-only blocks take ~3 ms, so 499 Hz gives ~1 sample per block; ms are period-weighted
#           so the rate does not bias them),
#           the same snapshots tagged drain-before / drain-after.
# Differences to the original:
#   - perf record -k CLOCK_MONOTONIC (sample times = CLOCK_MONOTONIC); prof-<tag>.clk = "realtime monotonic" read
#     together, so log UTC = perf time + (realtime - monotonic). If perf rejects -k, the window is re-recorded with
#     the default clock (1 s shorter); perf.clock / perf.drain.clock say which clock was used.
#   - prof-<tag>.lastpid = /proc/loadavg field 5 (last pid allocated, system-wide) for the thread-creation rate.
#   - NO positions sampling (its 60k RPC calls would land inside the drain this cell measures).
set -u
OUT=$1 START=$2 DUR=$3 DDUR=$4 DFREQ=$5
PIDS=/home/oz/torus-wsl-devnet/run/pids
L=$OUT/prof-sidecar.log
lg() { printf '[%s] %s\n' "$(date +%H:%M:%S.%3N)" "$*" >> "$L"; }
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && { echo "no bench start" >> "$L"; exit 1; }; done
lg "bench start seen"
P=$(head -1 "$PIDS"); lg "val0 pid=$P cmd=$(tr '\0' ' ' < /proc/$P/cmdline | cut -c1-300)"
lg "exe=$(readlink /proc/$P/exe) md5=$(md5sum < /proc/$P/exe | cut -d' ' -f1)"
snap() { # tag
    python3 -c 'import time; print(f"{time.clock_gettime(time.CLOCK_REALTIME):.6f} {time.clock_gettime(time.CLOCK_MONOTONIC):.6f}")' > "$OUT/prof-$1.clk"
    date +%s.%N > "$OUT/prof-$1.ts"
    cut -d' ' -f5 /proc/loadavg > "$OUT/prof-$1.lastpid"
    curl -fsS -m 3 http://127.0.0.1:9161/metrics > "$OUT/prof-metrics-$1.txt"
    cat /proc/$P/stat > "$OUT/prof-stat-$1.txt"
    for d in /proc/$P/task/*; do echo "$(basename $d) $(cat $d/comm 2>/dev/null | tr ' ' _) $(awk '{print $14, $15}' $d/stat 2>/dev/null)"; done > "$OUT/prof-tasks-$1.txt"
}
rec() { # data_file dur record_log clock_file freq
    local o=$1 d=$2 rl=$3 ck=$4 fq=$5 pp rc
    perf record -k CLOCK_MONOTONIC -e cycles:u -F "$fq" -g --call-graph fp -p "$P" -o "$o" -- sleep "$d" > "$rl" 2>&1 &
    pp=$!
    sleep 1
    if kill -0 "$pp" 2>/dev/null; then
        wait "$pp"; rc=$?; echo CLOCK_MONOTONIC > "$ck"; lg "perf $(basename "$o") clock=CLOCK_MONOTONIC freq=$fq rc=$rc"; return
    fi
    wait "$pp"; rc=$?
    if [ "$rc" = 0 ]; then echo CLOCK_MONOTONIC > "$ck"; lg "perf $(basename "$o") clock=CLOCK_MONOTONIC rc=0 (ended within 1 s: val0 gone?)"; return; fi
    lg "perf -k CLOCK_MONOTONIC failed rc=$rc ($(tail -1 "$rl")); re-recording $(basename "$o") with the default clock for $((d-1)) s"
    mv -f "$rl" "$rl.mono-failed"
    perf record -e cycles:u -F "$fq" -g --call-graph fp -p "$P" -o "$o" -- sleep "$((d-1))" > "$rl" 2>&1; rc=$?
    echo default > "$ck"; lg "perf $(basename "$o") clock=default rc=$rc"
}
# ---- load window (as the original: <START> s after bench start, <DUR> s)
sleep "$START"
snap before; lg "load=$(cut -d' ' -f1-3 /proc/loadavg) perf start (load window)"
rec "$OUT/perf.data" "$DUR" "$OUT/perf-record.log" "$OUT/perf.clock" 499
snap after; lg "load window done"
# ---- drain window: from the harness "bench exited rc=" line
t=0; until grep -q '\] bench exited rc=' "$OUT/run.log"; do sleep 0.2; t=$((t+1)); [ $t -gt 3000 ] && { lg "no bench exit within 600 s"; exit 1; }; done
snap drain-before; lg "bench exit seen: $(grep -m1 '\] bench exited rc=' "$OUT/run.log"); load=$(cut -d' ' -f1-3 /proc/loadavg) perf start (drain window)"
rec "$OUT/perf.drain.data" "$DDUR" "$OUT/perf-record.drain.log" "$OUT/perf.drain.clock" "$DFREQ"
snap drain-after; lg "drain window done; harness at: $(tail -1 "$OUT/run.log" | cut -c1-160)"
