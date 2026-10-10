#!/usr/bin/env bash
# io-sampler.sh <cell_out_dir>: disk I/O of the 3 node pids.
#  io-ticks.txt: "ts pid rchar wchar read_bytes write_bytes cancelled_write_bytes" at 1 Hz from the harness "bench:" line
#                until summary.json appears or the nodes exit.
#  io-threads-{start,end}.txt: per-thread "pid tid comm read_bytes write_bytes wchar utime stime" at the "bench:" line and at
#                the "bench exited" line (load window); write_bytes = bytes the thread caused to be written to storage.
set -u
OUT=$1; PIDS=/home/oz/torus-wsl-devnet/run/pids
t=0; until [ -f "$OUT/run.log" ] && grep -q '\] bench: ' "$OUT/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && exit 1; done
mapfile -t P < <(head -3 "$PIDS")
thr() { # file
    for p in "${P[@]}"; do
        for d in /proc/$p/task/*; do
            [ -r "$d/io" ] || continue
            awk -v p="$p" -v tid="${d##*/}" -v c="$(tr ' ' _ < "$d/comm" 2>/dev/null)" -v st="$(awk '{print $14, $15}' "$d/stat" 2>/dev/null)" \
                '/^read_bytes/{r=$2} /^write_bytes/{w=$2} /^wchar/{wc=$2} END{print p, tid, c, r, w, wc, st}' "$d/io" 2>/dev/null
        done
    done > "$1"
    date +%s.%N >> "$1.ts"
}
thr "$OUT/io-threads-start.txt"
endsnap=0
for i in $(seq 1500); do
    [ -f "$OUT/summary.json" ] && break
    if [ $endsnap = 0 ] && grep -q '\] bench exited' "$OUT/run.log" 2>/dev/null; then thr "$OUT/io-threads-end.txt"; endsnap=1; fi
    alive=0
    for p in "${P[@]}"; do
        [ -r /proc/$p/io ] || continue; alive=1
        awk -v ts="$EPOCHSECONDS" -v p="$p" '{v[$1]=$2} END{print ts, p, v["rchar:"], v["wchar:"], v["read_bytes:"], v["write_bytes:"], v["cancelled_write_bytes:"]}' /proc/$p/io
    done
    [ $alive = 0 ] && break
    sleep 1
done >> "$OUT/io-ticks.txt"
