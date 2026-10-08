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
echo "detach.sh: starting $UNIT.service (log $LOG; stop: systemctl --user stop $UNIT.service)"
exec systemd-run --user --quiet --collect --same-dir --unit="$UNIT" \
    -p StandardInput=null -p StandardOutput="append:$LOG" -p StandardError="append:$LOG" \
    "${ENV_ARGS[@]}" -- "$@"
