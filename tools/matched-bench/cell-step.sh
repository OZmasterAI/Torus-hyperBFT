# cell-step.sh — sourceable helper: the per-cell write-up step and the end-of-campaign rerun pass.
#
#   source tools/matched-bench/cell-step.sh
#   cell_step RUN_DIR CELL_DIR LABEL TAG ARM REP STAGED_MD5 SPEC [EXTRA_REASONS]
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
# EXTRA_REASONS (optional, comma-separated, e.g. "check"): the campaign's own failures of this cell (a failed hard
# check, a STOP site). They make the row bad:<reasons> (CELL-FLAG + rerun) even when the run itself was clean.
#
# Timeout: when the step exceeds CELL_STEP_TIMEOUT, `timeout` sends SIGTERM (SIGKILL 15 s later); cell_step.py
# kills its analysis process group and exits 124. cell_step then writes the row itself if the label has none:
# agree=no md5_ok=no (unknown counts as no), metric = summary.json headline.matched_s_avg (jq, 10 s) or 0,
# flag bad:[EXTRA_REASONS,]step-timeout, plus the CELL-FLAG line and the rerun queue entry (not for a -rerun
# label), and returns 124. The campaign goes on with the next cell.
#
# Step error: any other step exit that left no row for the label (a crash, an unexpected error, exit 5) gets the
# same fallback row with flag bad:[EXTRA_REASONS,]step-error, and cell_step returns 1: a finished cell is never lost.
#
# Return codes: 0 ok row, 1 bad row (also a step-error fallback row), 2 usage / foreign cells.tsv header / fields
# that cannot go into a row (no row),
# 3 duplicate label (no new row), 124 step timeout (fallback row written).

CELL_STEP_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CELL_STEP_PY=${CELL_STEP_PY:-$CELL_STEP_DIR/cell_step.py}

cell_step() { # RUN_DIR CELL_DIR LABEL TAG ARM REP STAGED_MD5 SPEC [EXTRA_REASONS]
    [ $# = 8 ] || [ $# = 9 ] || { echo "cell_step: want 8 or 9 args (RUN_DIR CELL_DIR LABEL TAG ARM REP STAGED_MD5 SPEC [EXTRA_REASONS]), got $#" >&2; return 2; }
    local run=$1 cell=$2 label=$3 tag=$4 arm=$5 rep=$6 md5=$7 spec=$8 xr=${9:-} rc pre=
    local t=${CELL_STEP_TIMEOUT:-900} at=${CELL_STEP_ANALYSIS_TIMEOUT:-600}
    local -a a=(record --run-dir "$run" --cell-dir "$cell" --label "$label" --tag "$tag" --arm "$arm" --rep "$rep"
                --staged-md5 "$md5" --spec "$spec" --analysis-timeout "$at")
    [ -n "${CELL_STEP_ANALYSIS_CMD:-}" ] && a+=(--analysis-cmd "$CELL_STEP_ANALYSIS_CMD")
    [ -n "${CELL_STEP_ANALYSIS_KEY:-}" ] && a+=(--analysis-key "$CELL_STEP_ANALYSIS_KEY")
    [ -n "${CELL_STEP_ANALYSIS_METRIC:-}" ] && a+=(--analysis-metric "$CELL_STEP_ANALYSIS_METRIC")
    [ -n "${CELL_STEP_LOG:-}" ] && a+=(--campaign-log "$CELL_STEP_LOG")
    xr=$(printf '%s' "$xr" | tr 'A-Z' 'a-z' | sed -E 's/[^a-z0-9,]+/-/g; s/(^|,)-+/\1/g; s/-+(,|$)/\1/g; s/,+/,/g; s/^,//; s/,$//')
    [ -n "$xr" ] && { a+=(--extra-reasons "$xr"); pre=$xr,; }
    timeout -k 15 "$t" python3 -I "$CELL_STEP_PY" "${a[@]}" < /dev/null
    rc=$?
    case $rc in
        124|137)
            echo "[$(date +%T)] CELL-STEP TIMEOUT $label after ${t}s (rc $rc): writing the bad:${pre}step-timeout row"
            cell_step_fallback "$run" "$cell" "$label" "$arm" "$rep" "$spec" "${pre}step-timeout"
            return 124 ;;
    esac
    if ! cell_step_has_row "$run" "$label"; then
        echo "[$(date +%T)] CELL-STEP ERROR $label: step exited $rc without a row: writing the bad:${pre}step-error row"
        cell_step_fallback "$run" "$cell" "$label" "$arm" "$rep" "$spec" "${pre}step-error" || return 2
        return 1
    fi
    return $rc
}

cell_step_has_row() { # RUN_DIR LABEL: 0 iff cells.tsv has a row for LABEL
    awk -F'\t' -v l="$2" 'NR > 1 && $1 == l {f = 1} END {exit !f}' "$1/cells.tsv" 2>/dev/null
}

cell_step_fallback() { # RUN_DIR CELL_DIR LABEL ARM REP SPEC REASON: bash-only row for a step that did not finish
    local run=$1 cell=$2 label=$3 arm=$4 rep=$5 spec=$6 reason=$7
    local tsv=$1/cells.tsv log=${CELL_STEP_LOG:-$1/campaign.log} q=$1/rerun-queue.tsv rc metric
    local hdr=$'label\tarm\trep\trc\tagree\tmd5_ok\tmetric\tflag'
    [[ -n "$label$arm$rep" && ! "$label$arm$rep$reason" =~ [[:space:]] && ! "$spec" =~ [$'\t\n'] ]] \
        || { echo "cell_step_fallback: label/arm/rep/reason empty or with whitespace (spec: tab/newline); no row" >&2; return 2; }
    if [ -s "$tsv" ]; then
        [ "$(head -1 "$tsv")" = "$hdr" ] || { echo "cell_step_fallback: $tsv has a foreign header; no row" >&2; return 2; }
        cell_step_has_row "$run" "$label" && return 0   # the step got its row in
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
    # RUNNER runs in this shell, and bash locals are dynamically scoped: a runner that assigns a name without
    # `local` writes OUR variable (the template's cell() sets n=0, so every rerun of ozarchy-p4gate printed 1/N).
    # Hence the _csrp_ names, and the progress index comes from the for word list (expanded once, before the
    # first call), so nothing the runner assigns can change the count, the total or the label we print.
    local _csrp_run=$1 _csrp_fn=$2 _csrp_line _csrp_rl _csrp_ol _csrp_arm _csrp_rep _csrp_spec _csrp_out _csrp_i
    local _csrp_total
    local -a _csrp_q=()
    _csrp_out=$(timeout 60 python3 -I "$CELL_STEP_PY" pending-reruns --run-dir "$_csrp_run" < /dev/null) \
        || { echo "[$(date +%T)] RERUN PASS: cannot read the queue"; return 2; }
    [ -n "$_csrp_out" ] && mapfile -t _csrp_q <<< "$_csrp_out"
    _csrp_total=${#_csrp_q[@]}
    if [ "$_csrp_total" = 0 ]; then echo "[$(date +%T)] RERUN PASS: nothing queued"; return 0; fi
    echo "[$(date +%T)] RERUN PASS: $_csrp_total queued: $(for _csrp_line in "${_csrp_q[@]}"; do printf '%s ' "${_csrp_line%%$'\t'*}"; done)"
    for _csrp_i in $(seq 1 "$_csrp_total"); do
        IFS=$'\t' read -r _csrp_rl _csrp_ol _csrp_arm _csrp_rep _csrp_spec <<< "${_csrp_q[_csrp_i - 1]}"
        echo "[$(date +%T)] RERUN $_csrp_i/$_csrp_total $_csrp_rl (bad cell $_csrp_ol, arm $_csrp_arm, rep $_csrp_rep, spec $_csrp_spec)"
        "$_csrp_fn" "$_csrp_spec" -rerun || { echo "[$(date +%T)] RERUN PASS STOP at $_csrp_rl"; return 1; }
    done
    echo "[$(date +%T)] RERUN PASS END"
    return 0
}
