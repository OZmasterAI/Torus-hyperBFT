#!/usr/bin/env bash
# s80 thp-heap: sample the THP share of every process matching a pattern.
#   thp-sampler.sh <pgrep -f pattern> <out.tsv> [interval_s=5] [wait_start_s=900] [max_s=1800]
#   UNIT=bench-s80-<arm>-<round> thp-sampler.sh 'release/torus-node --genesis' out.tsv 5 10800 1800
# Without UNIT: waits up to wait_start_s for a match, then samples every interval_s until no match is left or max_s.
# With UNIT (a cell.sh / detach.sh user unit name, without .service): waits up to wait_start_s for the unit to be
# active, then samples only matching pids inside that unit's cgroup while the unit is active (or until max_s), so
# another agent's cell on the same host is never sampled.
# One row per pid per sample: epoch pid Anonymous_kB AnonHugePages_kB thp_pct Rss_kB MALLOC_CONF (from environ).
# Cost: one smaps_rollup read per pid per sample (cheap; safe beside a cell).
set -u
pat=${1:?pattern}; out=${2:?out}; iv=${3:-5}; ws=${4:-900}; mx=${5:-1800}; unit=${UNIT:-}
# matching pids, minus this sampler (its argv holds the pattern too, as do its subshells); with UNIT, only that cgroup
match() {
    local p
    for p in $(pgrep -f -- "$pat"); do
        grep -qa thp-sampler "/proc/$p/cmdline" 2>/dev/null && continue
        [ -z "$unit" ] || grep -qF "/$unit.service" "/proc/$p/cgroup" 2>/dev/null || continue
        echo "$p"
    done
}
alive() { if [ -n "$unit" ]; then systemctl --user is-active --quiet "$unit.service"; else [ -n "$(match)" ]; fi; }
t0=$(date +%s)
until alive; do
    [ $(( $(date +%s) - t0 )) -ge "$ws" ] && { echo "nothing to sample ('$pat' ${unit:+unit $unit}) within ${ws}s" >&2; exit 1; }
    sleep 0.5
done
printf 'epoch\tpid\tanon_kB\tthp_kB\tthp_pct\trss_kB\tmalloc_conf\n' > "$out"
t1=$(date +%s)
while alive; do
    for p in $(match); do
        r=$(awk '/^Rss:/{r=$2} /^Anonymous:/{a=$2} /^AnonHugePages:/{h=$2} END{if(a>0) printf "%d\t%d\t%.1f\t%d", a, h, 100*h/a, r}' /proc/$p/smaps_rollup 2>/dev/null) || continue
        [ -n "$r" ] || continue
        mc=$(tr '\0' '\n' < /proc/$p/environ 2>/dev/null | sed -n 's/^MALLOC_CONF=//p')
        printf '%s\t%s\t%s\t%s\n' "$(date +%s.%N | cut -c1-14)" "$p" "$r" "${mc:--}" >> "$out"
    done
    [ $(( $(date +%s) - t1 )) -ge "$mx" ] && break
    sleep "$iv"
done
