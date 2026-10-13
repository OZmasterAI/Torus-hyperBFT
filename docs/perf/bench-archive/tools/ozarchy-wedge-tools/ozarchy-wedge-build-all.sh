#!/usr/bin/env bash
# ozarchy-wedge: the 2 builds in sequence (wm = main 925fd962, wf = fix/mesh-watchdog-resubscribe e3bb7cca), each into a FRESH
# target dir, with a free-space check (>= 15 GB) before each. Bench binary b 8efcb07a (md5 05d44313) reused from ozarchy-p3je-stage/b.
set -u
R=/home/oz/bench-results-matched
T=$R/ozarchy-wedge-tools
B=$R/ozarchy-wedge-build
mkdir -p "$B"
for spec in wm:925fd962b42f1e56cbcbf2ff97630c3667c7f4ad wf:e3bb7cca86d17e97ce0221200e16453fa863caf5; do
    k=${spec%%:*}; s=${spec#*:}
    free=$(df --output=avail -BG /home | tail -1 | tr -dc 0-9)
    echo "[$(date +%T)] arm $k sha $s free=${free}G"
    [ "$free" -ge 15 ] || { echo "[$(date +%T)] STOP: < 15 GB free before $k"; exit 1; }
    "$T/ozarchy-wedge-build.sh" "$k" "$s" "/home/oz/projects/wt/wedge-$k-${s:0:8}" "/home/oz/.cargo-target-wedge-$k-${s:0:8}" || exit 1
done
cp "$B/md5s.txt" "$B/md5s-all.txt"
grep '^b 8efcb07a' $R/ozarchy-p3je-build/md5s-all.txt | sed 's/(reused from ozarchy-p3q-stage\/b)/(reused from ozarchy-p3je-stage\/b)/' >> "$B/md5s-all.txt"
echo "[$(date +%T)] ALL BUILDS OK"
