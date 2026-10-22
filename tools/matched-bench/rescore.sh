#!/usr/bin/env bash
# rescore.sh <cell-dir> <dest-root> — re-score a finished cell with THIS harness
# copy WITHOUT touching it: the cell is copied (reflink where the filesystem has
# it) to <dest-root>/<label>, log-summary.json is rebuilt there from the kept
# node logs (val<i>.log.gz, collect_logs.py: per-line dissemination evidence),
# and resummarize.sh rewrites the copy's summary.json. The original cell dir is
# only read. Prints "<label> <old verdict> -> <new verdict>" and writes
# <dest-root>/<label>/rescore.json {label, source, old_validity, new_validity}.
set -euo pipefail
[ $# = 2 ] || { echo "usage: $0 <cell-dir> <dest-root>" >&2; exit 2; }
SRC=$(cd "$1" && pwd); T=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
LABEL=$(basename "$SRC")
mkdir -p "$2"; DEST="$(cd "$2" && pwd)/$LABEL"
[ ! -e "$DEST" ] || { echo "rescore: $DEST exists (refusing to overwrite)" >&2; exit 1; }
[ "$DEST" != "$SRC" ] || { echo "rescore: dest is the source" >&2; exit 1; }
# val0..val2, plus val3 on a 4-validator cell (run-cell.sh N_VALIDATORS=4: run-id.json n_validators)
N=$(jq -r '.n_validators // 3' "$SRC/run-id.json" 2>/dev/null || echo 3)
case "$N" in 3|4) ;; *) echo "rescore: $SRC/run-id.json n_validators=$N (3 or 4)" >&2; exit 1 ;; esac
LOGS=()
for i in $(seq 0 $((N - 1))); do LOGS+=("val$i.log.gz"); done
for f in summary.json "${LOGS[@]}"; do
    [ -s "$SRC/$f" ] || { echo "rescore: $SRC/$f missing (incomplete cell: rerun it)" >&2; exit 1; }
done
cp -a --reflink=auto "$SRC" "$DEST"
DEST_LOGS=(); for f in "${LOGS[@]}"; do DEST_LOGS+=("$DEST/$f"); done
python3 "$T/collect_logs.py" --logs "${DEST_LOGS[@]}" \
    --out "$DEST/log-summary.json" > "$DEST/rescore-dissem.txt"
# the counts must be the ones the cell was scored on (same logs, same patterns)
OLD=$(jq -r '.dissemination.raw // ""' "$SRC/summary.json")
[ "$OLD" = "$(cat "$DEST/rescore-dissem.txt")" ] || {
    echo "rescore: $LABEL: counts from the kept logs differ from the scored ones:" >&2
    echo "  scored:  $OLD" >&2; echo "  rebuilt: $(cat "$DEST/rescore-dissem.txt")" >&2; exit 1; }
"$T/resummarize.sh" "$DEST" > "$DEST/rescore-summarize.out" 2>&1
jq -n --arg label "$LABEL" --arg source "$SRC" --arg tools "$T" \
    --slurpfile o "$SRC/summary.json" --slurpfile n "$DEST/summary.json" \
    '{label: $label, source: $source, tools: $tools, old_validity: $o[0].validity, new_validity: $n[0].validity,
      kill_exemption: $n[0].dissemination.kill_exemption}' > "$DEST/rescore.json"
echo "$LABEL $(jq -r .validity.verdict "$SRC/summary.json") -> $(jq -r .validity.verdict "$DEST/summary.json")"
