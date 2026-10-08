#!/usr/bin/env bash
set -u
BIN=/home/oz/.cargo-target-adl-budget/release/deps/ubench_adl-5626de9bfbcc1f30
D=/home/oz/bench-results-matched/ubench-adl-c2
cd /home/oz/projects/wt/adl-budget
quiet() { until awk '{exit !($1<1.5)}' /proc/loadavg; do sleep 15; done; }
run() {
  local name=$1 runs=$2; shift 2
  quiet
  echo "== $name load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T) env: $*"
  for r in $(seq 1 "$runs"); do
    env "$@" "$BIN" --ignored --nocapture > "$D/$name.r$r.log" 2>&1
    echo "   run $r rc=$? $(grep -E '^h=.*adl_work' "$D/$name.r$r.log" | awk '{print $4}' | head -6 | tr '\n' ' ')"
  done
}
S="UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270"
run c4-w100000 3 $S UB_ADL_WORK=100000
run c4-w50000 3 $S UB_ADL_WORK=50000
run c1-w150000 3 UB_ADL_HL=1 UB_ADL_TRADERS=5000 UB_ADL_WORK=150000
quiet
echo "== perf-c4 load=$(cut -d' ' -f1-3 /proc/loadavg) $(date +%T)"
env $S UB_ADL_WORK=200000 perf record -F 4999 --call-graph fp -o "$D/perf-c4.data" -- "$BIN" --ignored --nocapture > "$D/perf-c4.log" 2>&1
echo "   perf rc=$?"
echo "== end $(date +%T)"
