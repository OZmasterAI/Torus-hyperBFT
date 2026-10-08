#!/usr/bin/env bash
# ubench_adl re-measure after the s99 owner decisions (perf/adl-budget @ f6a54382), default W = 100,000.
# Each case (and each perf) waits for 1-min load < 1.5.
set -u
BIN=/home/oz/.cargo-target-adl-budget/release/deps/ubench_adl-5626de9bfbcc1f30
D=/home/oz/bench-results-matched/ubench-adl-s99
cd /home/oz/projects/wt/adl-budget
quiet() { until awk '{exit !($1<1.5)}' /proc/loadavg; do sleep 15; done; }
# run NAME RUNS ENV...
run() {
  local name=$1 runs=$2; shift 2
  quiet
  echo "== $name load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T) env: $*"
  for r in $(seq 1 "$runs"); do
    local l0; l0=$(cut -d' ' -f1 /proc/loadavg)
    env "$@" "$BIN" --ignored --nocapture > "$D/$name.r$r.log" 2>&1
    echo "   run $r rc=$? load=$l0 steps: $(grep -E '^h=.*adl_work' "$D/$name.r$r.log" | sed -E 's/.*step_ms=([0-9.]+).*adl_work=([0-9]+)/\1@\2/' | head -8 | tr '\n' ' ')"
  done
}
HL="UB_ADL_HL=1 UB_ADL_TRADERS=5000"
S="UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270"
run c1 3 $HL
run c2 3 $HL UB_ADL_HOLDERS_PCT=10
run c3 3 UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10
run c4 3 $S
# perf profiles (whole run; blocks are split afterwards by sample time)
for c in "c4 $S" "c1 $HL"; do
  set -- $c; name=$1; shift
  quiet
  echo "== perf-$name load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T)"
  env "$@" perf record -F 4999 --call-graph fp -o "$D/perf-$name.data" -- "$BIN" --ignored --nocapture > "$D/perf-$name.log" 2>&1
  echo "   perf rc=$?"
done
echo "== end $(date +%T)"
