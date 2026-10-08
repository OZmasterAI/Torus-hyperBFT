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
for W in 200000 100000; do
  run c3-w$W 3 UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10 UB_ADL_WORK=$W
done
echo "== end $(date +%T)"
