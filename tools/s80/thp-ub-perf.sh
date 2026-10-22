#!/usr/bin/env bash
# s80 thp-heap: perf stat (user-space counters, perf_event_paranoid=2) of the steady ubench_econ proxy per arm,
# all arms in ONE hold of the heavy build lock.   thp-ub-perf.sh <prefix> <arm letters, e.g. DENEDN>
set -u
P=/home/oz/projects/wt/novel-s80/proto; pfx=${1:?prefix}; arms=${2:?arms}
B=$(ls -t "$HOME/.cargo-target-s80-thp-heap"/release/deps/ubench_econ-* | grep -v '\.d$' | head -1)
declare -A CONF=([D]='' [N]='thp:never' [H]='hpa:true' [E]='hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152')
EV=cycles:u,instructions:u,ls_l1_d_tlb_miss.all:u,ls_l1_d_tlb_miss.tlb_reload_4k_l2_miss:u,ls_l1_d_tlb_miss.tlb_reload_2m_l2_miss:u,ls_tablewalker.dside:u,page-faults:u
declare -A SEEN=()
exec 8>>"$HOME/projects/wt/.cargo-heavy.lock"; flock -w 3600 8 || exit 1
for (( i=0; i<${#arms}; i++ )); do
  k=${arms:i:1}; SEEN[$k]=$(( ${SEEN[$k]:-0} + 1 )); lab="$pfx-$k${SEEN[$k]}"; c=${CONF[$k]}
  pre=(LD_PRELOAD=/usr/lib/libjemalloc.so); [ -n "$c" ] && pre+=("MALLOC_CONF=$c")
  ( cd "$P/thp-heap" && env UB_MARKETS=300 UB_MARKS=1 TORUS_SETTLE_STREAMED=1 UB_WARM=200 UB_MEASURE=12 UB_RUNS=3 "${pre[@]}" \
      perf stat -x, -o "$P/results/thp-perf-$lab.csv" -e "$EV" "$B" --ignored --nocapture ) > "$P/results/ub-thp-heap-$lab.log" 2>&1
  echo "== $lab ${pre[*]} $(grep -h MEDIAN "$P/results/ub-thp-heap-$lab.log" | sed 's/.*MEDIAN/MEDIAN/; s/ runs=.*//')"
done
flock -u 8
