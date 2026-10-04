#!/usr/bin/env bash
# s80ab own quiet-host gate (in addition to bench_wait_quiet + run_cell.py load<1.5):
# wait until load1 < 1.0, no torus-node/bench-throughput/cargo/rustc process exists,
# and no single process uses > 40% CPU over a 5 s pidstat sample. Logs each attempt.
L=$1
while :; do
  read -r l1 l5 l15 _ < /proc/loadavg
  procs=$(pgrep -x torus-node; pgrep -x bench-throughpu; pgrep -x cargo; pgrep -x rustc)
  busy=$(pidstat -u -h 5 1 2>/dev/null | awk '$1 !~ /^#/ && NF>=10 && $8+0>40 {printf "%s(%s,%s%%) ", $10, $3, $8}')
  ok=$(awk -v l="$l1" 'BEGIN{print (l<1.0)?1:0}')
  echo "$(date +%FT%T) $L load=$l1/$l5/$l15 bench_procs=[${procs//$'\n'/,}] busy=[$busy] ok=$ok"
  if [ "$ok" = 1 ] && [ -z "$procs" ] && [ -z "$busy" ]; then
    echo "$(date +%FT%T) $L QUIET; other agents: $(pgrep -a -f 'codex|claude' | grep -v -- '--http\|tg_mirror' | awk '{print $1":"$2}' | paste -sd' ')"
    exit 0
  fi
  sleep 20
done
