#!/usr/bin/env bash
# Generic sequential A/B driver (s63+). Usage: ab-driver.sh CAMPAIGN_DIR
# CAMPAIGN_DIR/arms.conf defines, one per line (tab-separated):
#   arm<TAB>artifacts_dir<TAB>extra_env
# and one line:  ORDER<TAB>r1:armA r1:armB r2:armB ...
# Labels are <prefix>-<arm>-<rN>, prefix = basename of CAMPAIGN_DIR minus date.
# Global lock + quiet-host wait from bench-guard.sh; every cell kept; per-cell
# host sampler; RocksDB pruned only after evidence is retained (non-AGREE kept).
set -u
C=$1
R=/home/18c/bench-results-matched
source "$R/bench-guard.sh"
# run_cell.py puts each cell's devnet DB next to itself: use a per-campaign copy
# so prune-cell.sh (CAMPAIGN_DIR=$C) finds it.
[ -f "$C/run_cell.py" ] || cp "$R/s60-campaign-20260918/run_cell.py" "$C/run_cell.py"
WT=/home/18c/projects/wt/s63-body-fetch
PREFIX=$(basename "$C" | sed 's/-[0-9]\{8\}$//')
declare -A ART ENV
ORDER=""
while IFS=$'\t' read -r a b c; do
  [ -z "$a" ] && continue
  case "$a" in \#*) continue ;; ORDER) ORDER=$b ;; *) ART[$a]=$b; ENV[$a]=$c ;; esac
done < "$C/arms.conf"
[ -n "$ORDER" ] || { echo "no ORDER in arms.conf" >&2; exit 1; }
P="$C/progress.tsv"
[ -f "$P" ] || printf 'label\tstart\tend\texit\taccepted\tmatched_s_avg\tblk_s_avg\tdissem\tagree\tliveness\tidle_blk_s\n' > "$P"

for item in $ORDER; do
  r=${item%%:*}; arm=${item#*:}; label="$PREFIX-$arm-$r"
  [ -e "$R/$label" ] && { echo "skip existing $label"; continue; }
  bench_wait_quiet
  "$R/s63-pipe-20260923/hostsampler.sh" "$C/$label-host" > "$C/$label-host.sampler.log" 2>&1 &
  S=$!
  start=$(date +%FT%T)
  python3 "$C/run_cell.py" "$label" --artifacts "${ART[$arm]}" --duration 300 --rate 76000 \
    --cap 200 --markets 10 --extra-env "${ENV[$arm]}" --worktree "$WT" > "$C/$label.driver.log" 2>&1
  rc=$?
  if [ ! -d "$R/$label" ]; then
    kill "$S" 2>/dev/null; echo "$(date +%T) $label did not start (see $label.driver.log); stopping" >> "$C/campaign.log"; exit 1
  fi
  wait "$S"
  row=$(python3 - "$R/$label/summary.json" <<'EOF'
import json, sys
try:
    s = json.load(open(sys.argv[1])); h = s.get('headline', {})
    vals = [h.get(k) for k in ('benchmark_accepted', 'matched_s_avg', 'blk_s_avg',
            'dissemination_clean', 'agreement_verdict', 'liveness_verdict')] + [s.get('idle_blk_s')]
    sys.stdout.write('\t'.join(str(v) for v in vals))
except (OSError, ValueError):
    sys.stdout.write('\t'.join(['NO_SUMMARY'] + ['-'] * 6))
EOF
)
  printf '%s\t%s\t%s\t%s\t%s\n' "$label" "$start" "$(date +%FT%T)" "$rc" "$row" >> "$P"
  [ -s "$R/$label/summary.json" ] && CAMPAIGN_DIR="$C" "$R/s63-4build-20260923/prune-cell.sh" "$label" >> "$C/campaign.log" 2>&1
done
echo "CAMPAIGN DONE $(date +%FT%T)" > "$C/campaign.done"
