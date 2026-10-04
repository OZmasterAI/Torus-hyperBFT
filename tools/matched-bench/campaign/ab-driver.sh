#!/usr/bin/env bash
# Generic sequential A/B driver (s63+). Usage: ab-driver.sh CAMPAIGN_DIR
# Walkthrough: README.md next to this script.
#
# Environment (defaults reproduce the original host layout under $HOME):
#   RESULTS_ROOT  per-cell results go to $RESULTS_ROOT/<label>
#                 (default $HOME/bench-results-matched; the global bench lock
#                 $RESULTS_ROOT/.bench-global.lock lives there too)
#   HARNESS_WT    clean worktree whose tools/matched-bench/run-cell.sh and devnet/
#                 run every cell (default $HOME/projects/wt/s87-harness). Do not
#                 build, commit or switch branches there while the campaign runs.
#   PREFIX        label prefix (default: basename of CAMPAIGN_DIR minus -YYYYMMDD)
#   DRY_RUN=1     print each cell's resolved run_cell.py command and exit; takes no
#                 lock and writes nothing
#   ORDER_OVERRIDE="r1:main ..."  run this ORDER instead of arms.conf's ORDER line
#   RELAX_QUIET_R0=1  for round r0 cells ONLY (the unscored warm-up): skip
#     quiet-check.sh and pass --no-idle-wait to run_cell.py (no load<1.5 wait).
#     bench_wait_quiet (no node/bench/cargo/rustc) and run-cell.sh's own
#     "cargo build" abort stay on. Rounds r1+ are never relaxed.
#   DONE_FILE=name    done marker name in CAMPAIGN_DIR (default campaign.done)
#
# CAMPAIGN_DIR/arms.conf (format: arms.conf.example) defines, one per line
# (tab-separated; '-' in any column = empty, bash read collapses empty tab fields):
#   arm  artifacts_dir  node_env  [rate=76000]  [crash_at]  [duration=300]
#        [runner_env]  [cap=200]  [markets=10]
# and one line:  ORDER<TAB>r1:armA r1:armB r2:armB ...
#   crash_at: --crash-at seconds, or a comma list (multi-crash); empty = no crash.
#   runner_env: space-separated KEY=VAL passed as --runner-env (run_cell.py allowlist,
#     e.g. NODE_CPUS=0-5/6-10/11-15 BENCH_CPUS=16-17 ORACLE_FEED=1).
# arms.conf (incl. ORDER) is read ONCE, after the global lock is acquired.
# Labels are <prefix>-<arm>-<round>; a label whose results dir exists is skipped,
# so a relaunch resumes.
#
# Stop file: to stop cleanly while running, create CAMPAIGN_DIR/STOP (checked after
#   the quiet-host waits, right before each cell launches; a running cell is never
#   interrupted):
#     empty file       -> stop before the next cell
#     "N" or "rN"      -> finish round N, stop before the first cell of round > N
#     "loK"            -> finish low-load round loK
#   ORDER rounds are rN (full load) or loK (low-load cells, listed after every rN).
#   loK counts as round MAXR+K, MAXR = highest rN in ORDER, so with 4 full rounds:
#   STOP "4" = finish r4 and skip all low-load cells; "5" = "lo1". Any other
#   non-empty content = stop before next. The driver writes the done marker
#   "CAMPAIGN STOPPED ..." and exits 0; finished cells are kept.
# Global lock + quiet-host wait from bench-guard.sh; every cell kept; per-cell
# host sampler; RocksDB pruned only after evidence is retained (non-AGREE kept);
# raw run/val*.log deleted only after zcat|cmp against the retained .gz.
set -u
C=${1:?usage: ab-driver.sh CAMPAIGN_DIR}
C=$(cd "$C" && pwd) || exit 1
D=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)  # this tooling dir (run_cell.py, samplers, pruners)
export RESULTS_ROOT=${RESULTS_ROOT:-$HOME/bench-results-matched}
R=$RESULTS_ROOT
WT=${HARNESS_WT:-$HOME/projects/wt/s87-harness}
[ -x "$WT/tools/matched-bench/run-cell.sh" ] || { echo "HARNESS_WT=$WT has no tools/matched-bench/run-cell.sh; set HARNESS_WT" >&2; exit 1; }
[ -f "$C/arms.conf" ] || { echo "no $C/arms.conf (see $D/arms.conf.example)" >&2; exit 1; }
[ "${DRY_RUN:-0}" = 1 ] || source "$D/bench-guard.sh"
export PREFIX=${PREFIX:-$(basename "$C" | sed 's/-[0-9]\{8\}$//')}
declare -A ART ENV RATE CRASH DURATION RUNNER CAP MKT
ORDER=""
while IFS=$'\t' read -r a b c d e f g h i; do
  [ -z "$a" ] && continue
  case "$a" in \#*) continue ;; ORDER) ORDER=$b ;; *) ART[$a]=$b; ENV[$a]=$c; RATE[$a]=${d:-76000}; CRASH[$a]=${e:-}; DURATION[$a]=${f:-300}; RUNNER[$a]=${g:-}; CAP[$a]=${h:-200}; MKT[$a]=${i:-10}; [ "${CRASH[$a]}" = - ] && CRASH[$a]=; [ "${RUNNER[$a]}" = - ] && RUNNER[$a]=; [ "${ENV[$a]}" = - ] && ENV[$a]= ;; esac
done < "$C/arms.conf"
[ -n "${ORDER_OVERRIDE:-}" ] && ORDER=$ORDER_OVERRIDE
[ -n "$ORDER" ] || { echo "no ORDER in arms.conf" >&2; exit 1; }
MAXR=0  # highest full-load round number in ORDER
for item in $ORDER; do
  r=${item%%:*}; arm=${item#*:}
  [ -n "${ART[$arm]+x}" ] || { echo "ORDER item $item names an arm not defined in arms.conf" >&2; exit 1; }
  [[ "$r" =~ ^r([0-9]+)$ ]] && [ "${BASH_REMATCH[1]}" -gt "$MAXR" ] && MAXR=${BASH_REMATCH[1]}
done
DONE="$C/${DONE_FILE:-campaign.done}"
P="$C/progress.tsv"
if [ "${DRY_RUN:-0}" != 1 ]; then
  echo "$(date +%FT%T) driver start pid $$ ORDER='$ORDER' RELAX_QUIET_R0=${RELAX_QUIET_R0:-0} done=$DONE tooling=$D@$(git -C "$D" rev-parse --short HEAD 2>/dev/null)$(git -C "$D" diff --quiet HEAD -- . 2>/dev/null || echo -dirty) harness=$WT@$(git -C "$WT" rev-parse --short HEAD 2>/dev/null)" >> "$C/campaign.log"
  [ -f "$P" ] || printf 'label\tstart\tend\texit\taccepted\tmatched_s_avg\tblk_s_avg\tdissem\tagree\tliveness\tidle_blk_s\n' > "$P"
fi
round_num() {  # rN -> N, loK -> MAXR+K, anything else -> empty
  if [[ "$1" =~ ^r?([0-9]+)$ ]]; then echo "${BASH_REMATCH[1]}"
  elif [[ "$1" =~ ^lo([0-9]+)$ ]]; then echo $((MAXR + BASH_REMATCH[1])); fi
}
stop_requested() {  # $1 = round of the next cell (rN or loK); see header
  [ -e "$C/STOP" ] || return 1
  local n cur; n=$(round_num "$(tr -d ' \t\n' < "$C/STOP")"); cur=$(round_num "$1")
  [ -n "$n" ] && [ -n "$cur" ] || return 0
  [ "$cur" -gt "$n" ]
}

for item in $ORDER; do
  r=${item%%:*}; arm=${item#*:}; label="$PREFIX-$arm-$r"
  [ -e "$R/$label" ] && { echo "skip existing $label"; continue; }
  RELAX=0; [ "${RELAX_QUIET_R0:-0}" = 1 ] && [ "$r" = r0 ] && RELAX=1  # unscored warm-up only
  RUNNER_ARGS=(); for kv in ${RUNNER[$arm]}; do RUNNER_ARGS+=(--runner-env "$kv"); done
  IDLE_ARGS=(); [ "$RELAX" = 1 ] && IDLE_ARGS=(--no-idle-wait)
  CMD=(python3 "$D/run_cell.py" "$label" "${IDLE_ARGS[@]}" --campaign-dir "$C" --results-root "$R"
    --artifacts "${ART[$arm]}" --duration "${DURATION[$arm]}" --rate "${RATE[$arm]}"
    --cap "${CAP[$arm]}" --markets "${MKT[$arm]}" ${CRASH[$arm]:+--crash-at "${CRASH[$arm]}"}
    --extra-env "${ENV[$arm]}" "${RUNNER_ARGS[@]}" --worktree "$WT")
  if [ "${DRY_RUN:-0}" = 1 ]; then printf '%s:' "$label"; printf ' %q' "${CMD[@]}"; echo; continue; fi
  bench_wait_quiet
  if [ "$RELAX" = 1 ]; then
    echo "$(date +%FT%T) $label RELAXED (RELAX_QUIET_R0=1, unscored r0): quiet-check.sh and run_cell idle wait skipped; load=$(cut -d' ' -f1-3 /proc/loadavg)" >> "$C/quiet-checks.log"
  else
    "$D/quiet-check.sh" "$label" >> "$C/quiet-checks.log" 2>&1  # stricter own check
  fi
  bench_wait_quiet
  if stop_requested "$r"; then
    echo "$(date +%T) STOP file ('$(tr -d '\n' < "$C/STOP")') present: stopping before $label" >> "$C/campaign.log"
    echo "CAMPAIGN STOPPED before $label $(date +%FT%T)" > "$DONE"; exit 0
  fi
  "$D/hostsampler.sh" "$C/$label-host" > "$C/$label-host.sampler.log" 2>&1 &
  S=$!
  start=$(date +%FT%T)
  "${CMD[@]}" > "$C/$label.driver.log" 2>&1
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
  # Keep each node's RocksDB info LOG (incl. LOG.old.* from a crash restart) before prune.
  for v in "$C/$label"/data/val*; do
    [ -d "$v" ] || continue
    mkdir -p "$R/$label/rocksdb-log/$(basename "$v")" && cp -p "$v"/LOG* "$R/$label/rocksdb-log/$(basename "$v")/"
  done
  gzip -r "$R/$label/rocksdb-log" 2>/dev/null
  [ -s "$R/$label/summary.json" ] && CAMPAIGN_DIR="$C" "$D/prune-cell.sh" "$label" >> "$C/campaign.log" 2>&1
  [ -s "$C/$label.retention.json" ] && CAMPAIGN_DIR="$C" "$D/prune-rawlogs.sh" "$label" >> "$C/campaign.log" 2>&1
done
[ "${DRY_RUN:-0}" = 1 ] && exit 0
echo "CAMPAIGN DONE $(date +%FT%T)" > "$DONE"
