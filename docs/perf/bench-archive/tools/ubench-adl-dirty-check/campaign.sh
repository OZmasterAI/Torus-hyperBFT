#!/usr/bin/env bash
# ubench_adl case 3 A/B: A = main a746c408 (wt/adl-budget 50e3b9ce, same tree), B = c60ee4fa (wt/adl-dirty-check).
# Interleaved A,B x3; each run waits for 1-min load < 1.5; then one perf record of B.
set -u
D=/home/oz/bench-results-matched/ubench-adl-dirty-check
declare -A WT=([A]=/home/oz/projects/wt/adl-budget [B]=/home/oz/projects/wt/adl-dirty-check)
ENVS="UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10"
quiet() { until awk '{exit !($1<1.5)}' /proc/loadavg; do sleep 15; done; }
sha256sum "$D"/bin/A/ubench_adl "$D"/bin/B/ubench_adl
for r in 1 2 3; do
  for arm in A B; do
    quiet
    l0=$(cut -d' ' -f1-3 /proc/loadavg)
    cd "${WT[$arm]}"
    env $ENVS "$D/bin/$arm/ubench_adl" --ignored --nocapture > "$D/c3.$arm.r$r.log" 2>&1
    echo "== $arm r$r rc=$? load=$l0 $(date +%T) steps: $(grep -E '^h=.*adl_work' "$D/c3.$arm.r$r.log" | sed -E 's/.*step_ms=([0-9.]+).*adl_work=([0-9]+)/\1@\2/' | head -4 | tr '\n' ' ')"
  done
done
quiet
echo "== perf-c3.B load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T)"
cd "${WT[B]}"
env $ENVS perf record -F 4999 --call-graph fp -o "$D/perf-c3.B.data" -- "$D/bin/B/ubench_adl" --ignored --nocapture > "$D/perf-c3.B.log" 2>&1
echo "   perf rc=$?"
echo "== end $(date +%T)"
