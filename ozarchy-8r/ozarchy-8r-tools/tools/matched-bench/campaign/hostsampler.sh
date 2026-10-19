#!/usr/bin/env bash
# Per-second host sampler for one traced cell. Waits for 3 torus-node processes,
# then records until they are gone:
#   udp.csv      ts + cumulative /proc/net/snmp Udp counters (RcvbufErrors etc.)
#   sockets.log  ts + `ss -uampn` lines for torus-node sockets (skmem d = drops)
#   threads.log  pidstat -t (CPU + context switches) for the node threads, 1 s
#   host.log     pidstat -u for every active process on the host, 1 s
# Usage: hostsampler.sh OUT_DIR
set -u
OUT=$1; mkdir -p "$OUT"
for _ in $(seq 3600); do [ "$(pgrep -x torus-node | wc -l)" -ge 3 ] && break; sleep 1; done
[ "$(pgrep -x torus-node | wc -l)" -ge 3 ] || { echo "no nodes after 3600 s" >&2; exit 1; }
PIDS=$(pgrep -x torus-node | paste -sd,)
echo "$(date +%s) node pids $PIDS" > "$OUT/sampler-meta.txt"
pidstat -t -u -w -h -p "$PIDS" 1 > "$OUT/threads.log" 2>&1 & T=$!
pidstat -u -h 1 > "$OUT/host.log" 2>&1 & H=$!
vmstat -t 1 > "$OUT/vmstat.log" 2>&1 & V=$!
iostat -x -t 1 > "$OUT/iostat.log" 2>&1 & I=$!
# s65: 10 Hz state+wchan of each node's hotstuff-algo thread (bash builtins only, no forks)
wchan_loop() {
  local tids=() t p n i
  for i in $(seq 600); do   # the consensus thread starts after the process; wait up to 300 s
    tids=()
    for p in ${PIDS//,/ }; do for t in /proc/$p/task/*; do read -r n < "$t/comm" 2>/dev/null || continue; [ "$n" = hotstuff-algo ] && tids+=("$p:${t##*/}"); done; done
    [ ${#tids[@]} -ge 3 ] && break; sleep 0.5
  done
  echo "# tids ${tids[*]}"
  while :; do for x in "${tids[@]}"; do p=${x%%:*}; t=${x##*:}; read -r st < "/proc/$p/task/$t/stat" 2>/dev/null || exit 0; read -r w < "/proc/$p/task/$t/wchan" 2>/dev/null; st=${st##*) }; echo "$EPOCHREALTIME $p $t ${st%% *} ${w:-?}"; done; sleep 0.1; done
}
wchan_loop > "$OUT/wchan.log" 2>&1 & W=$!
echo "ts,$(awk '/^Udp:/{print; exit}' /proc/net/snmp | cut -d' ' -f2- | tr ' ' ',')" > "$OUT/udp.csv"
while pgrep -x torus-node >/dev/null; do
  ts=$(date +%s)
  echo "$ts,$(awk '/^Udp:/{n++} /^Udp:/&&n==2{print; exit}' /proc/net/snmp | cut -d' ' -f2- | tr ' ' ',')" >> "$OUT/udp.csv"
  ss -uampn 2>/dev/null | grep -A1 torus-node | sed "s/^/$ts /" >> "$OUT/sockets.log"
  [ $((ts % 5)) -eq 0 ] && { echo "== $ts"; grep -E "^(MemFree|MemAvailable|Cached|Dirty|Writeback|Active\(file\)|Inactive\(file\)|AnonPages|Shmem|Slab)" /proc/meminfo; } >> "$OUT/meminfo.log"
  sleep 1
done
kill "$T" "$H" "$V" "$I" "$W" 2>/dev/null
echo "$(date +%s) nodes gone, sampler stopped" >> "$OUT/sampler-meta.txt"
