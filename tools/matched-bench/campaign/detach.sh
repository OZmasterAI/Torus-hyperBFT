#!/usr/bin/env bash
# detach.sh — start a bench command as its own transient systemd --user SERVICE.
#
#   detach.sh NAME LOGFILE CMD [ARGS...]
#
# Runs CMD in unit bench-NAME.service: parented by the user manager, not by the
# calling shell, in its own cgroup, with the caller's exported environment and
# working directory, stdin from /dev/null and stdout+stderr appended to LOGFILE.
# Returns once the unit has started. Stop it (and everything it started) with
#   systemctl --user stop bench-NAME.service
#
# Why: on 2026-10-06 three 300-market runs on ozarchy each lost one process
# (val1, val2, the load generator) to a SIGKILL that was no OOM kill and no
# kill/tkill/tgkill. Those cells ran as descendants of an agent's shell (setsid
# nohup keeps the process tree); a run started through this script is not.
# `systemd-run --scope` would not do: a scope keeps the caller as parent.
# run-cell.sh refuses to run outside a bench-*.service unit.
#
# Build lock (s1156): started from outside a bench unit (a campaign), the unit
# prints `build lock: waiting` and runs CMD under `flock -o
# "$HOME/projects/wt/.cargo-heavy.lock"`, the lock every builder takes with
# `flock -w 60 -o` (framework gate 26): it holds the lock for its whole run, so
# wrapped builds wait instead of skewing cells, and it waits while a build holds
# it (only `build lock: waiting` in the log). Started from inside a bench-*.service
# unit (a cell of that campaign) it takes no lock: the campaign holds it, and a
# second flock would wait forever. No $HOME/projects/wt (or no flock): no lock.
set -eu
usage() { echo "usage: detach.sh NAME LOGFILE CMD [ARGS...]" >&2; exit 2; }
[ $# -ge 3 ] || usage
NAME=$1 LOG=$2
shift 2
command -v systemd-run >/dev/null || { echo "detach.sh: systemd-run not found" >&2; exit 1; }
UNIT="bench-$(printf '%s' "$NAME" | tr -c 'A-Za-z0-9_.-' '-')"
if systemctl --user is-active --quiet "$UNIT.service"; then
    echo "detach.sh: $UNIT.service is already running" >&2
    exit 1
fi
LOG=$(realpath -m "$LOG")
mkdir -p "$(dirname "$LOG")"
# Secrets stay out of the unit: `systemctl --user show` prints a unit's env, and on
# 2026-10-08 that put the launching Claude session's messaging token into a transcript.
SECRET_RE='TOKEN|SECRET|PASSWORD|PASSWD|_KEY$|^ANTHROPIC_|^CLAUDE_CODE_MESSAGING_'
ENV_ARGS=()
while IFS= read -r v; do
    [[ $v =~ $SECRET_RE ]] || ENV_ARGS+=(-E "$v")
done < <(compgen -e)
LOCK_DIR="$HOME/projects/wt"
LOCK_NOTE=""
if grep -qE '/bench-[^/]*\.service$' "${DETACH_CGROUP_FILE:-/proc/self/cgroup}"; then
    LOCK_NOTE="; no build lock: started inside a bench unit, whose launch holds it"
elif [ -d "$LOCK_DIR" ] && command -v flock >/dev/null; then
    set -- bash -c 'echo "build lock: waiting"; lock=$1; shift; exec flock -o "$lock" "$@"' \
        detach-lock "$LOCK_DIR/.cargo-heavy.lock" "$@"
    LOCK_NOTE="; holds the build lock $LOCK_DIR/.cargo-heavy.lock"
fi
echo "detach.sh: starting $UNIT.service (log $LOG; stop: systemctl --user stop $UNIT.service$LOCK_NOTE)"
exec systemd-run --user --quiet --collect --same-dir --unit="$UNIT" \
    -p StandardInput=null -p StandardOutput="append:$LOG" -p StandardError="append:$LOG" \
    "${ENV_ARGS[@]}" -- "$@"
