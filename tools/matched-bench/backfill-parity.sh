#!/usr/bin/env bash
# backfill-parity.sh <result-dir>... — add the r5 duration-parity fields
# (headline.matched_s_first120 / early60 / late60 / decay_ratio) to existing cell
# result dirs by re-running summarize.py via resummarize.sh. summary.json is
# backed up to summary.json.pre-parity first (only once: an existing backup is
# kept). matched_s_avg and every other field are unchanged (fields added to
# summarize.py after the cell ran come out as null).
set -euo pipefail
T=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
[ $# -ge 1 ] || { echo "usage: $0 <result-dir>..." >&2; exit 2; }
for D in "$@"; do
    [ -f "$D/summary.json" ] && [ -f "$D/sampler.csv" ] || { echo "SKIP $D (no summary.json/sampler.csv)"; continue; }
    [ -f "$D/summary.json.pre-parity" ] || cp -p "$D/summary.json" "$D/summary.json.pre-parity"
    bash "$T/resummarize.sh" "$D" | grep '^SUMMARY' || true
    jq -c '{label, dur: .cell.duration_s, matched_s_avg: .headline.matched_s_avg, first120: .headline.matched_s_first120,
            early60: .headline.matched_s_early60, late60: .headline.matched_s_late60, decay: .headline.decay_ratio,
            best60: .headline.matched_s_best60, agree: .headline.validators_agree}' "$D/summary.json"
done
