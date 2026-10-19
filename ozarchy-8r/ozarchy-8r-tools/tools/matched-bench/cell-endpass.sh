#!/usr/bin/env bash
# cell-endpass.sh <cell-dir> — the report-only node-log scans a cell run with DEFER_REPORT_SCANS=1 left for the
# campaign end pass (log-scans.sh: REPORT_SCANS), from the cell's preserved val<i>.log.gz:
#   val<i>.log.excerpt                  (as run-cell.sh writes it in-line)
#   run.log "ingest: ..." line          (the same text; the ingest scan reads each log only up to the byte
#                                        length it had when run-cell.sh would have scanned it, deferred-scans.json)
#   summary.json .ingest.mempool_nonce_expired_evictions_per_node   (when summarize.py wrote summary.json)
# then removes the cell's deferred-scans.json. No marker = nothing deferred: prints so, rc 0 (safe to rerun).
# rc 1 = a log is missing or corrupt (the marker stays: fix and rerun). The campaign driver runs it for every
# cell after the campaign's last cell (end_pass); by hand: tools/matched-bench/cell-endpass.sh <results>/<label>.
set -uo pipefail
D=${1:?usage: cell-endpass.sh <cell-dir>}
D=$(cd "$D" && pwd) || exit 1
L=$(basename "$D")
# shellcheck source=log-scans.sh
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/log-scans.sh" || exit 1
M=$D/$DEFERRED_MARKER
[ -f "$M" ] || { echo "END PASS $L: nothing deferred (no $DEFERRED_MARKER)"; exit 0; }
read -r SUB B0 B1 B2 < <(python3 -I -c '
import json, sys
m = json.load(open(sys.argv[1]))
b = m["ingest_log_bytes"]
assert len(b) == 3 and all(isinstance(x, int) and x >= 0 for x in b)
print(m.get("bench_submitted") or "-", *b)' "$M") || { echo "END PASS FAIL $L: unreadable $M"; exit 1; }
[ "$SUB" = - ] && SUB=
BYTES=("$B0" "$B1" "$B2")
for i in 0 1 2; do
    g=$D/val$i.log.gz
    [ -s "$g" ] && gzip -t "$g" 2>/dev/null || { echo "END PASS FAIL $L: $g missing or corrupt (marker kept)"; exit 1; }
done
T0=$(date +%s)
EVICTED=
for i in 0 1 2; do
    zcat "$D/val$i.log.gz" 2>/dev/null | log_excerpt > "$D/val$i.log.excerpt"
    EVICTED+=$(zcat "$D/val$i.log.gz" 2>/dev/null | head -c "${BYTES[$i]}" | ingest_evicted)
done
printf '[%s] %s\n' "$(date +%H:%M:%S)" "$(ingest_line "$SUB" "$EVICTED")" >> "$D/run.log"
S="no summary.json"
if [ -f "$D/summary.json" ]; then
    S=$(python3 -I -c '
import json, os, sys
p, ev = sys.argv[1], [int(x) for x in sys.argv[2].split()]
d = json.load(open(p))
if not isinstance(d.get("ingest"), dict):
    print("no .ingest: summarize.py did not finish")
    sys.exit(0)
d["ingest"]["mempool_nonce_expired_evictions_per_node"] = ev
with open(p + ".endpass.tmp", "w") as f:
    json.dump(d, f, indent=1)
os.replace(p + ".endpass.tmp", p)
print("patched")' "$D/summary.json" "$EVICTED" 2>&1) || { echo "END PASS FAIL $L: summary.json patch: $S"; exit 1; }
fi
rm -f -- "${M:?}"
echo "END PASS OK $L: excerpts, ingest evictions per node $EVICTED(summary.json: $S) in $(( $(date +%s) - T0 ))s"
