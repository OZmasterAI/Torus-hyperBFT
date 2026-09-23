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
echo "ts,$(awk '/^Udp:/{print; exit}' /proc/net/snmp | cut -d' ' -f2- | tr ' ' ',')" > "$OUT/udp.csv"
while pgrep -x torus-node >/dev/null; do
  ts=$(date +%s)
  echo "$ts,$(awk '/^Udp:/{n++} /^Udp:/&&n==2{print; exit}' /proc/net/snmp | cut -d' ' -f2- | tr ' ' ',')" >> "$OUT/udp.csv"
  ss -uampn 2>/dev/null | grep -A1 torus-node | sed "s/^/$ts /" >> "$OUT/sockets.log"
  [ $((ts % 5)) -eq 0 ] && { echo "== $ts"; grep -E "^(MemFree|MemAvailable|Cached|Dirty|Writeback|Active\(file\)|Inactive\(file\)|AnonPages|Shmem|Slab)" /proc/meminfo; } >> "$OUT/meminfo.log"
  sleep 1
done
kill "$T" "$H" "$V" "$I" 2>/dev/null
echo "$(date +%s) nodes gone, sampler stopped" >> "$OUT/sampler-meta.txt"
