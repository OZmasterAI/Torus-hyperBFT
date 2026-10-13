#!/usr/bin/env bash
# ozarchy-p3cloidprof analysis (run after campaign.done; read-only on the cells except the derived *.script.gz / perf-report-*.txt files).
set -u
R=/home/oz/bench-results-matched
A=$R/ozarchy-p3cloidprof-run/analyst
P=$R/ozarchy-p3cloidprof-300m
CELLS=()
for t in r1-c0 r1-c100 r2-c100 r2-c0 r3-c0 r3-c100; do [ -s "$P-$t/perf.data" ] && [ -s "$P-$t/summary.json" ] && CELLS+=("$P-$t"); done
echo "cells: ${CELLS[*]}"
for c in "${CELLS[@]}"; do
    [ -s "$c/prof.script.gz" ] || perf script -i "$c/perf.data" -F comm,tid,period,ip,sym,symoff --no-inline 2>/dev/null | gzip -1 > "$c/prof.script.gz" &
    [ -s "$c/w2.script.gz" ] || perf script -i "$c/perf-w2.data" -F comm,tid,period,event,ip,sym,symoff --no-inline 2>/dev/null | gzip -1 > "$c/w2.script.gz" &
done
wait
for c in "${CELLS[@]}"; do
    n=$(basename "$c")
    { perf report -i "$c/perf.data" --stdio --no-children --sort comm,sym --percent-limit 0.3 -g none 2>/dev/null | grep -v '^$' | head -120
      echo; echo "==== children (inclusive) ===="
      perf report -i "$c/perf.data" --stdio --children --sort sym --percent-limit 1 -g none 2>/dev/null | grep -v '^$' | head -120
      echo; echo "==== exec thread only (tid $(cat "$c/exec-tid.txt")) self ===="
      perf report -i "$c/perf.data" --stdio --no-children --tid "$(cat "$c/exec-tid.txt")" --sort sym --percent-limit 0.3 -g none 2>/dev/null | grep -v '^$' | head -80
      echo; echo "==== torus-exec-pool (market workers) self ===="
      perf report -i "$c/perf.data" --stdio --no-children --comms torus-exec-pool --sort sym --percent-limit 0.3 -g none 2>/dev/null | grep -v '^$' | head -80
      echo; echo "==== W2 exec thread, per event (perf-w2.data) ===="
      perf report -i "$c/perf-w2.data" --stdio --no-children --sort sym --percent-limit 1 -g none 2>/dev/null | grep -v '^$' | head -150
      echo; perf report -i "$c/perf.data" --stats 2>/dev/null | grep -iE 'lost|SAMPLE' | head
    } > "$A/perf-report-$n.txt" 2>&1 &
done
wait
python3 -I "$A/a-prof.py" "$A" "${CELLS[@]}"
for a in c0 c100; do python3 -I "$A/fg.py" "$A/folded-$a.txt" "$A/flamegraph-$a.svg" "ozarchy-p3cloidprof val1 W1 cycles:u, arm $a (mean of cells), us CPU per 1k fills, inline-expanded"; done
python3 -I "$A/a-metrics.py" "${CELLS[@]}" > "$A/metrics.txt"
# throughput / validity (copies of the p3cloid analyst scripts)
TAGS=(w0:c0); for c in "${CELLS[@]}"; do t=${c#"$P-"}; TAGS+=("$t:${t##*-}"); done
# Archive fix (docs commit, s116): a-cells.py and a-paired.py build cell paths as PREFIX + tag, so the prefix needs the
# trailing "-" ("$P-"); the analyst's copy on ozarchy passes "$P" and would open ozarchy-p3cloidprof-300mw0/. The archived
# cells.json / paired.txt were made with the hyphenated prefix (their w0 and wr values match the cells).
python3 -I "$A/a-cells.py" "$A/cells.json" "$P-" "${TAGS[@]}" > "$A/cells.txt" 2>&1
python3 -I -c 'import json,sys; c=json.load(open(sys.argv[1])); json.dump({k:v for k,v in c.items() if k!="w0"}, open(sys.argv[2],"w"))' "$A/cells.json" "$A/cells-abba.json"
python3 -I "$A/a-arms.py" "$A/cells-abba.json" c0:c100 > "$A/arms.txt" 2>&1
python3 -I "$A/a-paired.py" "$A/cells-abba.json" "$P-" c0 c100 > "$A/paired.txt" 2>&1
echo "analysis done"
