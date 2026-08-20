#!/usr/bin/env bash
# crash-kill.sh — SIGKILL exactly ONE devnet validator mid-cell and restart it
# from the same data dir. This is the crash gate for `TORUS_EXEC_PIPELINE`.
#
#   crash-kill.sh <worktree> <idx 1|2> <out-dir>
#   CRASH_KILL_LIB=1 source crash-kill.sh      # functions only (unit tests)
#
# Why: with the flush worker attached, block N's state batch + applied-height
# marker are written by W while E already runs N+1. The applied-height marker is
# the crash fence — a `kill -9` must therefore rewind at most the blocks that
# were committed-but-unexecuted anyway, plus the depth-1 hand-off, and the
# restarted node must converge byte-identically with the two survivors.
#
# What it records (crash-kill.json in <out-dir>): the target's counters right
# before the kill (block height, exec queue depth, flush-worker depth), the byte
# offset of its log at that moment (so run-cell.sh scans only the post-restart
# tail), and the restart. summarize.py turns run-cell.sh's merged crash.json into
# a PASS/FAIL verdict.
#
# SAFETY. This script sends SIGKILL. A live testnet validator runs on this box
# from ~/.cargo-target against testnet/data (ports 8555/9090/30333) and must
# NEVER be a target. `crash_target_ok` therefore refuses unless ALL of:
#   * idx is 1 or 2  (val0 serves the bench RPC and every headline number)
#   * the pid is listed in the devnet's own $RUN_DIR/pids
#   * the pid's /proc cmdline carries `--data-dir=$DATA_ROOT/data/val<idx>`
#     (the `=` form launch-3val.sh uses; the testnet unit uses the space form)
#   * the cmdline carries none of the protected markers below
#   * the pid is not listed in $TORUS_PROTECTED_PIDS
# Every one of those refusals is pinned in tools/matched-bench/test_harness.py
# (CrashKillGuardTest), including the live validator's real cmdline.
set -uo pipefail

# Markers that identify the LIVE testnet validator (or anything shaped like it).
# Matching any of them is an immediate refusal, whatever the other checks say.
CRASH_KILL_PROTECTED_MARKERS=(
    "/testnet/data"
    ".cargo-target/release/torus-node"
    "--keystore"
    "torus-18c-validator"
    ":8555"
    ":9090"
    ":30333"
)

# crash_target_ok <idx> <pid> <cmdline> <data_root> <pids_file>
crash_target_ok() {
    local idx=${1-} pid=${2-} cmdline=${3-} root=${4-} pidsf=${5-} marker
    case "$idx" in
        1|2) ;;
        *) echo "crash-kill: REFUSED idx='$idx' (only val1/val2 may be killed; val0 serves the bench RPC and every headline number)" >&2; return 1 ;;
    esac
    case "$pid" in
        ''|*[!0-9]*) echo "crash-kill: REFUSED pid='$pid' (not a pid)" >&2; return 1 ;;
    esac
    [ "$pid" -ge 2 ] || { echo "crash-kill: REFUSED pid=$pid" >&2; return 1; }
    for marker in ${TORUS_PROTECTED_PIDS:-}; do
        [ "$pid" = "$marker" ] && { echo "crash-kill: REFUSED pid=$pid (TORUS_PROTECTED_PIDS)" >&2; return 1; }
    done
    grep -qx -- "$pid" "$pidsf" 2>/dev/null || {
        echo "crash-kill: REFUSED pid=$pid (not listed in the devnet pids file $pidsf)" >&2; return 1; }
    case "$cmdline" in
        *torus-node*) ;;
        *) echo "crash-kill: REFUSED pid=$pid (not a torus-node)" >&2; return 1 ;;
    esac
    case "$cmdline" in
        *"--data-dir=$root/data/val$idx "*|*"--data-dir=$root/data/val$idx") ;;
        *) echo "crash-kill: REFUSED pid=$pid (cmdline is not devnet val$idx under $root)" >&2; return 1 ;;
    esac
    for marker in "${CRASH_KILL_PROTECTED_MARKERS[@]}"; do
        case "$cmdline" in
            *"$marker"*) echo "crash-kill: REFUSED pid=$pid (protected marker '$marker' — this looks like the LIVE validator)" >&2; return 1 ;;
        esac
    done
    return 0
}

# crash_kill_at_ok <at_s> <dur_s> — the kill must land inside the LOAD window:
# early enough that >= 30 s of load remains for the restarted node to replay and
# rejoin under pressure, late enough that it has executed real blocks, and never
# in the drain (where it would hang the agreement probe).
crash_kill_at_ok() {
    local at=${1-} dur=${2-}
    case "$at"  in ''|*[!0-9]*) return 1 ;; esac
    case "$dur" in ''|*[!0-9]*) return 1 ;; esac
    [ "$at" -ge 10 ] || return 1
    [ "$dur" -ge 40 ] || return 1
    [ "$at" -le $(( dur - 30 )) ] || return 1
    return 0
}

# crash_replace_pid_line <pids_file> <idx> <new_pid> — REPLACE, never append:
# stop-3val.sh only kills what the file lists, so a lost line leaves an orphan
# node holding 8646/9162 and the next cell dies in pre-flight.
crash_replace_pid_line() {
    local f=${1-} idx=${2-} newpid=${3-} tmp
    [ -f "$f" ] || return 1
    tmp=$(mktemp "$f.XXXXXX") || return 1
    awk -v n=$(( idx + 1 )) -v p="$newpid" 'NR==n{print p; next} {print}' "$f" > "$tmp" || { rm -f "$tmp"; return 1; }
    mv -f "$tmp" "$f"
}

crash_read_cmdline() { tr '\0' ' ' < "/proc/$1/cmdline" 2>/dev/null; }

# crash_log_field <tail-file> <line-pattern> <field> — first value of `field` on
# the first matching line. Accepts both log shapes the node can emit:
# `applied_height=1231` (fmt::layer(), what the devnet runs) and
# `"applied_height":1231` (fmt::layer().json()).
crash_log_field() {
    grep -m1 -E "$2" "$1" 2>/dev/null \
        | grep -oE "\"?$3\"?[=:] ?[0-9]+" | head -1 | grep -oE '[0-9]+$'
}

# crash_scan_restart_tail <tail-file> — the post-restart evidence, as JSON on
# stdout. The replay line is app.rs `replay_committed`:
#   "crash recovery: execution gap detected, replaying"
#     committed_height=<C> applied_height=<A> gap=<C-A>
# <A> IS the durable applied-height marker at the instant of the SIGKILL — the
# fence the exec pipeline must hold — so <gap> is exactly what the node rewound
# (r9 waloff crash proof, mem d448f539: "...committed_height=90 applied_height=79
# gap=11"). No replay line at all = applied was already == committed = 0 rewind.
crash_scan_restart_tail() {
    local f=$1 gap app com watt panics holes errs pipe
    local replay='crash recovery: execution gap detected'
    gap=$(crash_log_field "$f" "$replay" gap)
    app=$(crash_log_field "$f" "$replay" applied_height)
    com=$(crash_log_field "$f" "$replay" committed_height)
    watt=$(crash_log_field "$f" 'bl2 exec pipeline ENABLED' applied)
    cnt() { grep -c -E "$1" "$f" 2>/dev/null || true; }
    panics=$(cnt 'panicked|FAIL-STOP|fail-stop|conflicting blocks')
    holes=$(cnt 'could not be fully replayed LOCALLY|hole_height')
    errs=$(cnt ' ERROR ')
    pipe=$(cnt 'bl2 exec pipeline ENABLED')
    python3 -c '
import json, sys
def n(v):
    v = v.strip()
    return int(v) if v.isdigit() else None
gap, app, com, watt, panics, holes, errs, pipe = sys.argv[1:9]
json.dump({
 "replay_line_found": bool(gap.strip()),
 "gap": n(gap) or 0,
 "applied_height_at_crash": n(app),
 "committed_height_at_crash": n(com),
 "pipeline_enabled_line": (n(pipe) or 0) > 0,
 "worker_attached_applied": n(watt),
 "panic_or_failstop_lines": n(panics) or 0,
 "hole_lines": n(holes) or 0,
 "error_lines": n(errs) or 0,
}, sys.stdout, indent=1)
' "${gap:-}" "${app:-}" "${com:-}" "${watt:-}" "${panics:-0}" "${holes:-0}" "${errs:-0}" "${pipe:-0}"
}

if [ "${CRASH_KILL_LIB:-0}" = 1 ]; then
    return 0 2>/dev/null || exit 0
fi

# ------------------------------------------------------------------ CLI
# `scan` mode (run-cell.sh, after the cell): merge the post-restart log scan into
# the record crash-kill.sh left at kill time, producing the crash.json that
# summarize.py turns into a PASS/FAIL verdict.
if [ "${1-}" = scan ]; then
    [ $# -ge 5 ] || { echo "usage: crash-kill.sh scan <tail-log> <crash-kill.json> <crash.json> <kill_at_s>" >&2; exit 2; }
    crash_scan_restart_tail "$2" > "$2.restart.json" || exit 1
    python3 - "$3" "$2.restart.json" "$4" "$5" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
d["restart"] = json.load(open(sys.argv[2]))
d["kill_at_s"] = int(sys.argv[4])
json.dump(d, open(sys.argv[3], "w"), indent=1)
r = d["restart"]
print("crash gate scan: rewind %s blocks from durable marker %s (committed %s), "
      "worker_attached=%s panics=%s holes=%s errors=%s"
      % (r["gap"], r["applied_height_at_crash"], r["committed_height_at_crash"],
         r["pipeline_enabled_line"], r["panic_or_failstop_lines"], r["hole_lines"],
         r["error_lines"]))
PY
    exit $?
fi

[ $# -ge 3 ] || { sed -n '2,6p' "$0" >&2; exit 2; }
WT=$(cd "$1" && pwd) || exit 2
IDX=$2
OUT=$3
KILL_WAIT=${KILL_WAIT:-15}

# shellcheck source=/dev/null
source "$WT/devnet/wsl/env.sh"
# shellcheck source=/dev/null
source "$WT/devnet/wsl/start-node.sh"

PIDSF="$RUN_DIR/pids"
LOG="$RUN_DIR/val$IDX.log"
MET=$(node_var MET "$IDX")

mapfile -t PIDS < "$PIDSF" 2>/dev/null || { echo "crash-kill: no pids file $PIDSF" >&2; exit 1; }
PID=${PIDS[$IDX]:-}
CMDLINE=$(crash_read_cmdline "$PID")
crash_target_ok "$IDX" "$PID" "$CMDLINE" "$DATA_ROOT" "$PIDSF" || exit 1

mval() { awk -v m="$1" '$1==m {print $2; f=1} END{if(!f) print 0}'; }
M=$(curl -s -m 5 "http://127.0.0.1:$MET/metrics") || M=""
snap() { printf '%s' "$M" | mval "$1" | cut -d. -f1; }
PRE_H=$(snap torus_block_height)
PRE_C=$(snap torus_blocks_committed_total)
PRE_Q=$(snap torus_exec_queue_depth)
PRE_W=$(snap torus_flush_worker_depth)
PRE_M=$(snap torus_orders_matched_total)
LOG_BYTES=$(stat -c%s "$LOG" 2>/dev/null || echo 0)

echo "crash-kill: SIGKILL val$IDX pid=$PID (height=$PRE_H exec_queue=$PRE_Q flush_worker=$PRE_W log_bytes=$LOG_BYTES)"
T_KILL=$(date +%s.%N)
kill -9 "$PID" 2>/dev/null
for _ in $(seq 1 "$KILL_WAIT"); do kill -0 "$PID" 2>/dev/null || break; sleep 1; done
if kill -0 "$PID" 2>/dev/null; then
    echo "crash-kill: FATAL pid $PID survived SIGKILL" >&2; exit 1
fi

# Same argv as the launch, same data dir, log APPENDED so the pre-crash lines
# stay next to the replay lines.
STARTED_PID=""
start_node "$IDX" append || { echo "crash-kill: FATAL restart failed" >&2; exit 1; }
T_UP=$(date +%s.%N)
crash_replace_pid_line "$PIDSF" "$IDX" "$STARTED_PID" || {
    echo "crash-kill: FATAL could not update $PIDSF (orphan node risk)" >&2; exit 1; }
DOWN=$(awk -v a="$T_KILL" -v b="$T_UP" 'BEGIN{printf "%.2f", b-a}')
echo "crash-kill: val$IDX back as pid=$STARTED_PID after ${DOWN}s"

python3 - "$OUT/crash-kill.json" <<PY
import json, sys
json.dump({
 "enabled": True,
 "kill_node": "val$IDX", "kill_idx": $IDX,
 "killed_pid": $PID, "restarted_pid": $STARTED_PID,
 "kill_ts": float("$T_KILL"), "restart_ts": float("$T_UP"), "down_s": float("$DOWN"),
 "pre_kill": {"block_height": $PRE_H, "blocks_committed": $PRE_C,
              "exec_queue_depth": $PRE_Q, "flush_worker_depth": $PRE_W,
              "matched": $PRE_M, "log_bytes": $LOG_BYTES},
}, open(sys.argv[1], "w"), indent=1)
PY
