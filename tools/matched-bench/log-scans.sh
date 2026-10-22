# shellcheck shell=bash
# log-scans.sh — the node-log scans run-cell.sh runs after a cell, sourced by run-cell.sh and cell-endpass.sh
# (one copy of each scan, so a cell scanned in-line and one scanned by the campaign end pass get the same files).
#
# Post-cell work, classified (bench queue pacing rule, s131). IN-LINE = what decides the cell's validity or the
# next cell, or reads state the next cell overwrites; END PASS = report-only, run once after the campaign's last
# cell from the preserved logs (DEFER_REPORT_SCANS=1, set by the campaign driver; default 0 = all in-line):
#   in-line   panic / fail-stop and ERROR line counts (agreement.jsonl -> AGREE, validity, the warm-up gate),
#             dissemination counts (collect_logs.py -> log-summary.json -> dissemination_clean -> validity and
#             health.py's rc), the val<i>.log.gz copies (the raw logs live in DATA_ROOT/run, which the next
#             cell's launch overwrites; checks and analysis read the .gz), the crash-gate tail scans (crash verdict)
#   end pass  REPORT_SCANS: the ingest eviction counts (run.log "ingest:" line, summary.json
#             .ingest.mempool_nonce_expired_evictions_per_node) and val<i>.log.excerpt
# A deferred cell carries deferred-scans.json (the byte length of each node log when the ingest scan would have
# run, the bench's submitted count) until cell-endpass.sh has run on it.
REPORT_SCANS="ingest-evictions log-excerpts"
DEFERRED_MARKER=deferred-scans.json

# ingest_evicted [LOG]: "N " = the sum of evicted=N over a node log's "evicted nonce-expired" lines (the mempool
# dropped a backlog older than NONCE_WINDOW_MS); reads stdin without LOG
ingest_evicted() {
    sed -E 's/\x1b\[[0-9;]*m//g' "$@" | grep "evicted nonce-expired" \
        | awk '{for(k=1;k<=NF;k++) if($k ~ /^evicted=/){split($k,a,"="); s+=a[2]}} END{printf "%d ", s+0}'
}

# log_excerpt [LOG]: the first 400 lines of a node log that matter for a quick read; reads stdin without LOG
log_excerpt() {
    grep -E ' ERROR | WARN |panicked|FAIL-STOP|fail-stop|book mode|resident|parallel|member cache|commit-lag|S470|manifest|FULL-BODY push|native block-selection caps|body fetch|OUTBOUND FAILURE|exec-backlog pacing' "$@" | head -400
}

# ingest_line SUBMITTED EVICTED: run.log's ingest line (in-line or from the end pass: the same text)
ingest_line() {
    echo "ingest: bench submitted=${1:-?} actions; mempool nonce-expired evictions per node: $2"
}

# node_indices: the devnet's validator indices, one per line: 0 1 2 (+ 3 with run-cell.sh N_VALIDATORS=4)
node_indices() {
    printf '%s\n' 0 1 2
    [ "${N_VALIDATORS:-3}" = 3 ] || echo 3
}

# file_bytes FILE...: "b0 b1 b2" (0 for a missing file; one per FILE: "b0 b1 b2 b3" on the 4-validator devnet)
file_bytes() {
    local f out=
    for f in "$@"; do out+="$(stat -c %s -- "$f" 2>/dev/null || echo 0) "; done
    echo "${out% }"
}

# write_deferred_marker OUT BENCH_SUBMITTED "b0 b1 b2": the end pass's to-do for this cell
# ("b0 b1 b2 b3" on the 4-validator devnet, run-cell N_VALIDATORS=4: four entries; else exactly three)
write_deferred_marker() {
    local out=$1 sub=$2 b=($3)
    if [ "${#b[@]}" = 4 ]; then
        printf '{"version": 1, "scans": "%s", "bench_submitted": "%s", "ingest_log_bytes": [%s, %s, %s, %s]}\n' \
            "$REPORT_SCANS" "$sub" "${b[0]:-0}" "${b[1]:-0}" "${b[2]:-0}" "${b[3]:-0}" > "$out/$DEFERRED_MARKER"
        return
    fi
    printf '{"version": 1, "scans": "%s", "bench_submitted": "%s", "ingest_log_bytes": [%s, %s, %s]}\n' \
        "$REPORT_SCANS" "$sub" "${b[0]:-0}" "${b[1]:-0}" "${b[2]:-0}" > "$out/$DEFERRED_MARKER"
}
