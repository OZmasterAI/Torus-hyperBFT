#!/usr/bin/env bash
# Delete a finished s63 cell's disposable devnet RocksDB (<campaign>/<label>/data)
# after confirming its evidence is retained. User-authorized 2026-09-23 (s63).
# Retained: results dir (summary.json, CSVs, metrics, digests, val*.log.gz) and
# the raw run/ logs. Writes <label>.retention.json.  Usage: prune-cell.sh LABEL...
set -eu
D=${CAMPAIGN_DIR:-/home/18c/bench-results-matched/s63-4build-20260923}
R=/home/18c/bench-results-matched
for l in "$@"; do
  case "$l" in s63-*-r[0-9]*) ;; *) echo "refusing unexpected label: $l" >&2; exit 1 ;; esac
  t="$D/$l/data"
  if [ ! -d "$t" ]; then echo "$l: no data dir"; continue; fi
  for f in "$R/$l/summary.json" "$R/$l/val0.log.gz" "$R/$l/val1.log.gz" "$R/$l/val2.log.gz"; do
    [ -s "$f" ] || { echo "$l: missing retained $f, not deleting" >&2; exit 1; }
  done
  [ -d "$D/$l/run" ] || { echo "$l: missing raw run/ logs, not deleting" >&2; exit 1; }
  # Keep the database as evidence for any cell whose validators did not agree.
  verdict=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["headline"].get("agreement_verdict"))' "$R/$l/summary.json")
  if [ "$verdict" != "AGREE" ]; then echo "$l: agreement=$verdict, keeping data/ as evidence"; continue; fi
  sz=$(du -sb "$t" | cut -f1)
  rm -rf -- "$t"
  [ ! -e "$t" ] || { echo "$l: delete failed" >&2; exit 1; }
  printf '{\n "label": "%s",\n "data_path": "%s",\n "deleted_bytes": %s,\n "reason": "Disposable per-cell devnet RocksDB; fresh DB per cell.",\n "retained_results": "%s",\n "retained_raw_logs": "%s",\n "cleanup_executed": true,\n "authorized": "user, 2026-09-23 s63",\n "timestamp": %s\n}\n' \
    "$l" "$t" "$sz" "$R/$l" "$D/$l/run" "$(date +%s)" > "$D/$l.retention.json"
  echo "$l: deleted $((sz / 1024 / 1024 / 1024)) GiB"
done
