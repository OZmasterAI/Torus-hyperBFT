#!/usr/bin/env bash
# Shadow cell dirs for a-prof.py (section 60): symlinks to the raw cell files + derived perf script output, all under analyst/.
# Nothing is written into the cell dirs. Names end -c100/-c0 as a-prof.py's arm_of() expects.
# ARCHIVE COPY (section 63 write-up): the analyst's prep-prof.sh on ozarchy has no --symfs. Both arms ran from one exe path
# (the harness worktree's target/release/torus-node) and the ~/.debug build-id cache entry of B's build id held F's binary,
# so a plain `perf script` mis-symbolises B. The dumps that a-prof-arm.py read were made by an inline loop with
# `perf script --symfs $PWD/symfs-$arm` (symfs-<arm>/<recorded exe path> -> the arm's staged torus-node). This copy adds the
# symfs links and the --symfs flag; everything else is the analyst's script unchanged.
set -u
R=/home/oz/bench-results-matched
A=$R/ozarchy-p3cloidec-run/analyst
EXE=/home/oz/projects/wt/p3gate-s2-c88d3073/target/release/torus-node
for arm in F B; do
    mkdir -p "$A/symfs-$arm$(dirname "$EXE")"
    [ -e "$A/symfs-$arm$EXE" ] || ln -s "$R/ozarchy-p3cloidec-stage/$arm-run/release/torus-node" "$A/symfs-$arm$EXE"
done
for m in F:r1-F100:r1-c100 F:r4-F100:r4-c100 F:r6-F100:r6-c100 F:r7-F100:r7-c100 \
         B:r2-B100:r2-c100 B:r3-B100:r3-c100 B:r5-B100:r5-c100 B:r8-B100:r8-c100 B:q2-B0:q2-c0 B:q3-B0:q3-c0; do
    IFS=: read -r arm tag short <<< "$m"
    src=$R/ozarchy-p3cloidec-300m-$tag
    dst=$A/prof-$arm/cells/ec-$short
    mkdir -p "$dst"
    for f in exec-tid.txt perf.data perf-w2.data prof-metrics-w1-before.txt prof-metrics-w1-after.txt prof-metrics-w2-before.txt prof-metrics-w2-after.txt \
             prof-stat-w1-before.txt prof-stat-w1-after.txt prof-stat-w2-before.txt prof-stat-w2-after.txt prof-w1-before.ts prof-w1-after.ts \
             prof-w2-before.ts prof-w2-after.ts prof-tasks-w1-before.txt prof-tasks-w1-after.txt; do
        [ -e "$dst/$f" ] || ln -s "$src/$f" "$dst/$f"
    done
    [ -s "$dst/prof.script.gz" ] || perf script --symfs "$A/symfs-$arm" -i "$dst/perf.data" -F comm,tid,period,ip,sym,symoff --no-inline 2>/dev/null | gzip -1 > "$dst/prof.script.gz" &
    [ -s "$dst/w2.script.gz" ] || perf script --symfs "$A/symfs-$arm" -i "$dst/perf-w2.data" -F comm,tid,period,event,ip,sym,symoff --no-inline 2>/dev/null | gzip -1 > "$dst/w2.script.gz" &
done
wait
ls -la $A/prof-*/cells/*/*.script.gz
