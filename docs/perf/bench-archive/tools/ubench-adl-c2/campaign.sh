#!/usr/bin/env bash
# ubench_adl on C2 (perf/adl-budget @ 3139c36f). Each case waits for 1-min load < 1.5.
set -u
BIN=/home/oz/.cargo-target-adl-budget/release/deps/ubench_adl-5626de9bfbcc1f30
D=/home/oz/bench-results-matched/ubench-adl-c2
cd /home/oz/projects/wt/adl-budget
quiet() { until awk '{exit !($1<1.5)}' /proc/loadavg; do sleep 15; done; }
# run NAME RUNS ENV...
run() {
  local name=$1 runs=$2; shift 2
  quiet
  echo "== $name load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T) env: $*"
  for r in $(seq 1 "$runs"); do
    env "$@" "$BIN" --ignored --nocapture > "$D/$name.r$r.log" 2>&1
    echo "   run $r rc=$? $(grep -E '^h=.*adl_work' "$D/$name.r$r.log" | awk '{print $4}' | tr '\n' ' ')"
  done
}
A="UB_ADL_HL=1 UB_ADL_TRADERS=5000"
# case 1: HL all-hold; case 2: HL 10 %; W sweep (option b)
for W in 630000 400000 300000 200000 100000; do
  run c1-w$W 3 $A UB_ADL_WORK=$W
  run c2-w$W 3 $A UB_ADL_HOLDERS_PCT=10 UB_ADL_WORK=$W
done
# case 4: S=750-like on C2, default W plus the sweep
for W in 630000 300000 200000; do
  run c4-w$W 3 UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270 UB_ADL_WORK=$W
done
# case 3: HL 100k accounts, 10 % holders (3M rows): W = HL-sized (1.25 x U), U, 630k, 300k
for W in 12510000 10000800 630000 300000; do
  run c3-w$W 3 UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10 UB_ADL_WORK=$W
done
# perf profiles of the HL block, cases 1 and 2 at W = 630,000
for c in "c1 $A" "c2 $A UB_ADL_HOLDERS_PCT=10"; do
  set -- $c; name=$1; shift
  quiet
  echo "== perf-$name load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T)"
  env "$@" perf record -F 4999 --call-graph fp -o "$D/perf-$name.data" -- "$BIN" --ignored --nocapture > "$D/perf-$name.log" 2>&1
  echo "   perf rc=$?"
done
echo "== end $(date +%T)"
