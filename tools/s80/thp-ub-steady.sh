#!/usr/bin/env bash
# s80 thp-heap steady-state proxy: like thp-ub-ab.sh, but each ROUND (one pass over the given arms) runs under ONE
# hold of the heavy build lock, and the ubench runs long (UB_WARM=200 by default) so the jemalloc heap reaches the
# purge-fragmented state a 120 s cell node sits in. Same env as ub.sh (UB_MARKETS=300 UB_MARKS=1
# TORUS_SETTLE_STREAMED=1) plus UBX words.
#   [UBX='UB_WARM=200 UB_MEASURE=12 UB_RUNS=3'] thp-ub-steady.sh <prefix> <round1-arms> <round2-arms> ...
#   e.g. thp-ub-steady.sh st DENH HNED DENH      (arm keys: D N A H E G as in thp-ub-ab.sh)
set -u
P=/home/oz/projects/wt/novel-s80/proto; S=$P/thp-heap/tools/s80/thp-sampler.sh; pfx=${1:?prefix}; shift
B=$(ls -t "$HOME/.cargo-target-s80-thp-heap"/release/deps/ubench_econ-* | grep -v '\.d$' | head -1)
read -r -a X <<< "${UBX:-UB_WARM=200 UB_MEASURE=12 UB_RUNS=3}"
declare -A CONF=(
  [D]='' [N]='thp:never' [A]='thp:always,metadata_thp:always' [H]='hpa:true'
  [E]='hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152' [G]='glibc'
)
run_round() { # $1 = arm letters; runs inside the lock
  local arms=$1 k c lab sp i
  for (( i=0; i<${#arms}; i++ )); do
    k=${arms:i:1}; SEEN[$k]=$(( ${SEEN[$k]:-0} + 1 )); lab="$pfx-$k${SEEN[$k]}"; c=${CONF[$k]}
    "$S" "^$B" "$P/results/thp-ub-$lab.tsv" 1 60 600 & sp=$!
    local pre=()
    if [ "$c" != glibc ]; then pre=(LD_PRELOAD=/usr/lib/libjemalloc.so); [ -n "$c" ] && pre+=("MALLOC_CONF=$c"); fi
    ( cd "$P/thp-heap" && env UB_MARKETS=300 UB_MARKS=1 TORUS_SETTLE_STREAMED=1 "${X[@]}" "${pre[@]}" "$B" --ignored --nocapture ) \
      > "$P/results/ub-thp-heap-$lab.log" 2>&1
    wait "$sp"
    echo "== $lab ${pre[*]} ${X[*]}"; grep -h MEDIAN "$P/results/ub-thp-heap-$lab.log" | sed 's/.*MEDIAN/MEDIAN/'
    awk -F'\t' -v l="$lab" 'NR>1{n++; s+=$5; if(m==""||$5<m)m=$5; a+=$3} END{if(n) printf "THP %s samples=%d mean_thp_pct=%.1f min=%.1f mean_anon_MB=%.0f\n", l, n, s/n, m, a/n/1024}' "$P/results/thp-ub-$lab.tsv"
  done
}
declare -A SEEN=()
for r in "$@"; do
  exec 8>>"$HOME/projects/wt/.cargo-heavy.lock"
  flock -w 3600 8 || { echo "lock timeout" >&2; exit 1; }
  run_round "$r"
  flock -u 8; exec 8>&-
done
