#!/usr/bin/env bash
# Start the MEASURED rounds, detached, with every quiet check ON (quiet-check.sh
# load<1.0 / no busy process, run_cell.py load<1.5 wait, bench_wait_quiet).
# ORDER = arms.conf's ORDER without its r0 (warm-up) items, or ORDER_OVERRIDE if
# set. RELAX_QUIET_R0 and DONE_FILE are explicitly unset; done marker
# CAMPAIGN_DIR/campaign.done ("CAMPAIGN DONE|STOPPED ..." then "exit=N").
# Refuses if campaign.done exists or the warm-up has not finished (warmup.done).
# NO_WARMUP=1 skips the warm-up check, for a one-off smoke without an r0 item
# (unscored-style check; never for a scored A/B campaign).
# Usage: [NO_WARMUP=1] start-rounds.sh CAMPAIGN_DIR   (RESULTS_ROOT / HARNESS_WT / PREFIX pass through)
set -eu
C=$(cd "${1:?usage: start-rounds.sh CAMPAIGN_DIR}" && pwd)
D=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
[ -e "$C/campaign.done" ] && { echo "campaign.done already exists; refusing" >&2; exit 1; }
[ "${NO_WARMUP:-0}" = 1 ] || [ -e "$C/warmup.done" ] || { echo "warm-up not finished yet (no warmup.done); run start-warmup.sh first and wait for it (one-off smoke without a warm-up: NO_WARMUP=1 start-rounds.sh CAMPAIGN_DIR)" >&2; exit 1; }
ORDER=${ORDER_OVERRIDE:-$(awk -F'\t' '$1=="ORDER"{o=$2} END{print o}' "$C/arms.conf" | tr ' ' '\n' | grep -v '^r0:' | paste -sd' ')}
[ -n "$ORDER" ] || { echo "no measured (non-r0) items in ORDER" >&2; exit 1; }
cd "$C"
env -u RELAX_QUIET_R0 -u DONE_FILE ORDER_OVERRIDE="$ORDER" \
  "$D/detach.sh" "$(basename "$C")-rounds" "$C/campaign.nohup" \
  bash -c '"$1" "$2"; echo "exit=$?" >> "$2/campaign.done"' _ "$D/ab-driver.sh" "$C"
echo "started measured-rounds driver (unit bench-$(basename "$C")-rounds.service) ORDER='$ORDER'; progress: $C/progress.tsv $C/campaign.log $C/quiet-checks.log; done marker: $C/campaign.done"
