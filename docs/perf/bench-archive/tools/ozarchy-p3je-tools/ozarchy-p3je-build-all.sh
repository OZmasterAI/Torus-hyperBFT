#!/usr/bin/env bash
# ozarchy-p3je: the 4 builds in sequence (ja, jb, jcs, jmf), each into a FRESH target dir, with a free-space check (>= 15 GB) before each.
set -u
R=/home/oz/bench-results-matched
T=$R/ozarchy-p3je-tools
B=$R/ozarchy-p3je-build
mkdir -p "$B"
for spec in ja:88ddc4c29d807e298fddf68f1385e42884d092d1 jb:deeacdb35e63a29083ba3f75b2636a009f47ad10 jcs:e7be8460ac81d559ee039f07ac5de66e38d1ed94 jmf:0360c840dc751faa37966db0bd9eba3022ae6b30; do
    k=${spec%%:*}; s=${spec#*:}
    free=$(df --output=avail -BG /home | tail -1 | tr -dc 0-9)
    echo "[$(date +%T)] arm $k sha $s free=${free}G"
    [ "$free" -ge 15 ] || { echo "[$(date +%T)] STOP: < 15 GB free before $k"; exit 1; }
    "$T/ozarchy-p3je-build.sh" "$k" "$s" "/home/oz/projects/wt/p3je-$k-${s:0:8}" "/home/oz/.cargo-target-p3je-$k-${s:0:8}" || exit 1
done
cp "$B/md5s.txt" "$B/md5s-all.txt"
grep '^b 8efcb07a' $R/ozarchy-p3q-build/md5s-all.txt | sed 's/(reused from ozarchy-p3rss-stage\/b)/(reused from ozarchy-p3q-stage\/b)/' >> "$B/md5s-all.txt"
echo "[$(date +%T)] ALL BUILDS OK"
