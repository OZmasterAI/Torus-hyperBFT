#!/usr/bin/env bash
# 3 CPU snapshots at ~110s intervals -> $1
OUT="$1"
for i in 1 2 3; do
  sleep 110
  { echo "===sample_${i}_$(date +%s)==="; top -bn1 | head -22; } >> "$OUT"
done
