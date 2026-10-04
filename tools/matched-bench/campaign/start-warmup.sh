#!/usr/bin/env bash
# Launch ONLY the unscored warm-up cell(s), detached: the r0 items of arms.conf's
# ORDER (or ORDER_OVERRIDE if set), with the quiet checks relaxed for r0
# (RELAX_QUIET_R0=1, see ab-driver.sh). Done marker CAMPAIGN_DIR/warmup.done
# ("CAMPAIGN DONE ..." from the driver, then "exit=N").
# Usage: start-warmup.sh CAMPAIGN_DIR   (RESULTS_ROOT / HARNESS_WT / PREFIX pass through)
set -eu
C=$(cd "${1:?usage: start-warmup.sh CAMPAIGN_DIR}" && pwd)
D=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
[ -e "$C/warmup.done" ] && { echo "warmup.done exists; refusing" >&2; exit 1; }
ORDER=${ORDER_OVERRIDE:-$(awk -F'\t' '$1=="ORDER"{o=$2} END{print o}' "$C/arms.conf")}
W=$(for i in $ORDER; do [ "${i%%:*}" = r0 ] && printf '%s ' "$i"; done; true)
[ -n "$W" ] || { echo "no r0 (warm-up) item in ORDER '$ORDER'" >&2; exit 1; }
cd "$C"
ORDER_OVERRIDE=${W% } RELAX_QUIET_R0=1 DONE_FILE=warmup.done \
  setsid nohup bash -c '"$1" "$2"; echo "exit=$?" >> "$2/warmup.done"' _ "$D/ab-driver.sh" "$C" > "$C/warmup.nohup" 2>&1 < /dev/null &
echo "warm-up driver pid $! ORDER='${W% }'; logs $C/*-r0.log $C/campaign.log; done marker $C/warmup.done"
