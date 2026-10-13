#!/usr/bin/env bash
# perf-report-comm-sym.sh: per-cell W1 perf report, self time sorted by thread name (comm) and symbol, no call graph.
# The analyst's inline command from the 2026-10-13 noise re-analysis (section 60.6), saved as a script for the archive.
# Writes <out dir>/perf-report-comm-sym-<round>-<arm>.txt. Needs the cells' perf.data (Tier A) and the node binary
# (symbols). The outputs (50-66 KB each) are not in the repo: regenerate them with this script.
# Usage: perf-report-comm-sym.sh <bench-results-matched dir> <out dir>
set -euo pipefail
B=${1:?usage: perf-report-comm-sym.sh <bench-results-matched dir> <out dir>}
N=${2:?usage: perf-report-comm-sym.sh <bench-results-matched dir> <out dir>}
for t in r1-c0 r1-c100 r2-c0 r2-c100 r3-c0 r3-c100; do
  c="$B/ozarchy-p3cloidprof-300m-$t"
  ( nice -n 19 perf report -i "$c/perf.data" --stdio --no-children --sort comm,sym --percent-limit 0.05 -g none 2>/dev/null \
      | grep -v '^$' > "$N/perf-report-comm-sym-$t.txt" ) &
done
wait
