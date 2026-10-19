#!/usr/bin/env bash
# s75 convention: delete <campaign>/<label>/run/val*.log only after zcat|cmp proves
# each is byte-identical to the retained <results>/<label>/val*.log.gz; record in
# <label>.retention.json.  Usage: CAMPAIGN_DIR=<dir> prune-rawlogs.sh LABEL...
# Labels must be <PREFIX>-<arm>-rN / -loN (PREFIX env, default = basename of
# CAMPAIGN_DIR minus -YYYYMMDD, as in ab-driver.sh). RESULTS_ROOT default
# $HOME/bench-results-matched.
set -eu
C=${CAMPAIGN_DIR:?CAMPAIGN_DIR required}
R=${RESULTS_ROOT:-$HOME/bench-results-matched}
PREFIX=${PREFIX:-$(basename "$C" | sed 's/-[0-9]\{8\}$//')}
for l in "$@"; do
  case "$l" in */*|*..*) echo "refusing unexpected label: $l" >&2; exit 1 ;; "$PREFIX"-*-r[0-9]*|"$PREFIX"-*-lo[0-9]*) ;; *) echo "refusing unexpected label: $l (expected $PREFIX-<arm>-rN)" >&2; exit 1 ;; esac
  [ -s "$C/$l.retention.json" ] || { echo "$l: no retention.json, skipping raw logs" >&2; continue; }
  del=(); gz=(); bytes=0
  for n in 0 1 2; do
    f="$C/$l/run/val$n.log"; g="$R/$l/val$n.log.gz"
    [ -f "$f" ] || continue
    if [ -s "$g" ] && zcat "$g" | cmp -s - "$f"; then
      bytes=$((bytes + $(stat -c %s "$f"))); rm -f -- "$f"; del+=("$f"); gz+=("$g")
    else
      echo "$l: $f differs from $g (or gz missing); kept" >&2
    fi
  done
  [ ${#del[@]} -gt 0 ] || { echo "$l: no raw logs deleted"; continue; }
  python3 - "$C/$l.retention.json" "$bytes" "${del[*]}" "${gz[*]}" <<'PY'
import json, sys, time
p, b, d, g = sys.argv[1], int(sys.argv[2]), sys.argv[3].split(), sys.argv[4].split()
j = json.load(open(p))
j.update(raw_logs_deleted=d, raw_logs_deleted_bytes=b, raw_logs_verified_identical_to=g,
         raw_logs_note="run/val*.log deleted after zcat|cmp showed byte-identical to the retained val*.log.gz (s75 convention)",
         raw_logs_timestamp=int(time.time()))
open(p, 'w').write(json.dumps(j, indent=1) + '\n')
PY
  echo "$l: raw logs deleted $((bytes / 1024 / 1024)) MiB (verified identical)"
done
