#!/usr/bin/env bash
# 8r checks self-test: run checks with a given arm on a finished cell (read-only on cells; r8modechk/r8slowchk rewrite
# their own <cell>/r8-worker.txt / o8-slowpass.txt files, re-run with the right arm afterwards).
R=/home/oz/bench-results-matched
. /home/oz/bench-results-matched/ozarchy-8r-tools/8r-checks.sh
label=$1 arm=$2; shift 2
for f in "$@"; do ( "$f" "$label" "$arm" ) | cut -c1-150; done
