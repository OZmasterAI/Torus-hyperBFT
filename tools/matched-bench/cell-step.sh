# cell-step.sh — sourceable helper: the per-cell write-up step and the end-of-campaign rerun pass.
#
#   source tools/matched-bench/cell-step.sh
#   cell_step RUN_DIR CELL_DIR LABEL TAG ARM REP STAGED_MD5 SPEC
#   cell_step_rerun_pass RUN_DIR RUNNER        # RUNNER SPEC SUFFIX: the campaign's "one cell + its step" function
#
# cell_step runs cell_step.py (see its docstring for the cells.tsv contract, the bad reasons, the known
# artifacts and the analysis check) in the FOREGROUND: the campaign calls it right after a cell ends and the
# next cell starts only after it returns. Nothing is left running in the background.
#
# Environment (all optional):
#   CELL_STEP_TIMEOUT           outer limit for the whole step, seconds (default 900)
#   CELL_STEP_ANALYSIS_TIMEOUT  limit for the analysis command inside the step (default 600; keep it < the outer one)
#   CELL_STEP_ANALYSIS_CMD      the campaign's per-cell analysis, e.g.
#                               'python3 -I $ANALYST/a-cells.py {out} {prefix} {tag}:{arm}'   (empty = no analysis check)
#   CELL_STEP_ANALYSIS_KEY      key of the cell's row in the analysis JSON (default {tag})
#   CELL_STEP_ANALYSIS_METRIC   the row's matched/s field (a-cells.py: ms); unset = metric from summary.json
#   CELL_STEP_LOG               campaign log for CELL-FLAG lines (default RUN_DIR/campaign.log)
#   CELL_STEP_PY                cell_step.py path (default: next to this file)
#
# Timeout: when the step exceeds CELL_STEP_TIMEOUT, `timeout` sends SIGTERM (SIGKILL 15 s later); cell_step.py
# kills its analysis process group and exits 124. cell_step then writes the row itself if the label has none:
# agree=no md5_ok=no (unknown counts as no), metric = summary.json headline.matched_s_avg (jq, 10 s) or 0,
# flag bad:step-timeout, plus the CELL-FLAG line and the rerun queue entry (not for a -rerun label), and
# returns 124. The campaign goes on with the next cell.
#
# Return codes: 0 ok row, 1 bad row, 2 usage / foreign cells.tsv header (no row), 3 duplicate label (no row),
# 124 step timeout (fallback row written).

CELL_STEP_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CELL_STEP_PY=${CELL_STEP_PY:-$CELL_STEP_DIR/cell_step.py}

cell_step() { # RUN_DIR CELL_DIR LABEL TAG ARM REP STAGED_MD5 SPEC
    [ $# = 8 ] || { echo "cell_step: want 8 args (RUN_DIR CELL_DIR LABEL TAG ARM REP STAGED_MD5 SPEC), got $#" >&2; return 2; }
    local run=$1 cell=$2 label=$3 tag=$4 arm=$5 rep=$6 md5=$7 spec=$8 rc
    local t=${CELL_STEP_TIMEOUT:-900} at=${CELL_STEP_ANALYSIS_TIMEOUT:-600}
    local -a a=(record --run-dir "$run" --cell-dir "$cell" --label "$label" --tag "$tag" --arm "$arm" --rep "$rep"
                --staged-md5 "$md5" --spec "$spec" --analysis-timeout "$at")
    [ -n "${CELL_STEP_ANALYSIS_CMD:-}" ] && a+=(--analysis-cmd "$CELL_STEP_ANALYSIS_CMD")
    [ -n "${CELL_STEP_ANALYSIS_KEY:-}" ] && a+=(--analysis-key "$CELL_STEP_ANALYSIS_KEY")
    [ -n "${CELL_STEP_ANALYSIS_METRIC:-}" ] && a+=(--analysis-metric "$CELL_STEP_ANALYSIS_METRIC")
    [ -n "${CELL_STEP_LOG:-}" ] && a+=(--campaign-log "$CELL_STEP_LOG")
    timeout -k 15 "$t" python3 -I "$CELL_STEP_PY" "${a[@]}" < /dev/null
    rc=$?
    case $rc in
        124|137)
            echo "[$(date +%T)] CELL-STEP TIMEOUT $label after ${t}s (rc $rc): writing the bad:step-timeout row"
            cell_step_fallback "$run" "$cell" "$label" "$arm" "$rep" "$spec" step-timeout
            return 124 ;;
    esac
    return $rc
}

cell_step_fallback() { # RUN_DIR CELL_DIR LABEL ARM REP SPEC REASON: bash-only row for a step that did not finish
    local run=$1 cell=$2 label=$3 arm=$4 rep=$5 spec=$6 reason=$7
    local tsv=$1/cells.tsv log=${CELL_STEP_LOG:-$1/campaign.log} q=$1/rerun-queue.tsv rc metric
    local hdr=$'label\tarm\trep\trc\tagree\tmd5_ok\tmetric\tflag'
    if [ -s "$tsv" ]; then
        [ "$(head -1 "$tsv")" = "$hdr" ] || { echo "cell_step_fallback: $tsv has a foreign header; no row" >&2; return 2; }
        awk -F'\t' -v l="$label" 'NR > 1 && $1 == l {f = 1} END {exit !f}' "$tsv" && return 0   # the step got its row in
    else
        printf '%s\n' "$hdr" > "$tsv"
    fi
    rc=$(sed -n 's/^rc=//p' "$cell.cell.rc" 2>/dev/null | head -1); [[ "$rc" =~ ^-?[0-9]+$ ]] || rc=missing
    metric=$(timeout 10 jq -r '.headline.matched_s_avg // 0' "$cell/summary.json" 2>/dev/null)
    [[ "$metric" =~ ^[0-9]+(\.[0-9]+)?$ ]] || metric=0
    if [[ "$label" != *-rerun ]] && ! awk -F'\t' -v l="$label-rerun" '$1 == l {f = 1} END {exit !f}' "$q" 2>/dev/null; then
        printf '%s\t%s\t%s\t%s\t%s\n' "$label-rerun" "$label" "$arm" "$rep" "$spec" >> "$q"
    fi
    echo "CELL-FLAG $label $reason" >> "$log"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$label" "$arm" "$rep" "$rc" no no "$metric" "bad:$reason" >> "$tsv"
}

cell_step_rerun_pass() { # RUN_DIR RUNNER: run each queued <label>-rerun once, through RUNNER SPEC -rerun
    local run=$1 fn=$2 line rl ol arm rep spec n=0 out
    local -a q=()
    out=$(timeout 60 python3 -I "$CELL_STEP_PY" pending-reruns --run-dir "$run" < /dev/null) \
        || { echo "[$(date +%T)] RERUN PASS: cannot read the queue"; return 2; }
    [ -n "$out" ] && mapfile -t q <<< "$out"
    if [ ${#q[@]} = 0 ]; then echo "[$(date +%T)] RERUN PASS: nothing queued"; return 0; fi
    echo "[$(date +%T)] RERUN PASS: ${#q[@]} queued: $(for line in "${q[@]}"; do printf '%s ' "${line%%$'\t'*}"; done)"
    for line in "${q[@]}"; do
        IFS=$'\t' read -r rl ol arm rep spec <<< "$line"
        n=$((n + 1))
        echo "[$(date +%T)] RERUN $n/${#q[@]} $rl (bad cell $ol, arm $arm, rep $rep, spec $spec)"
        "$fn" "$spec" -rerun || { echo "[$(date +%T)] RERUN PASS STOP at $rl"; return 1; }
    done
    echo "[$(date +%T)] RERUN PASS END"
    return 0
}
