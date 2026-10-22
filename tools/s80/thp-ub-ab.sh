#!/usr/bin/env bash
# s80 thp-heap proxy A/B: ubench_econ under LD_PRELOAD of the system libjemalloc (the SAME version string as the
# node's bundled jemalloc, 5.3.1-0-g81034ce1), one MALLOC_CONF per arm, balanced order, THP share sampled per run.
#   [UBX='UB_WARM=200 ...'] thp-ub-ab.sh <rounds-label-prefix> <order...>   order words are arm keys below; each word
#   = one ub.sh invocation; UBX = extra ubench env words, the same on every arm.
set -u
read -r -a X <<< "${UBX:-}"
P=/home/oz/projects/wt/novel-s80/proto; S=$P/thp-heap/tools/s80/thp-sampler.sh; pfx=${1:?prefix}; shift
declare -A CONF=(
  [D]=''                                                         # jemalloc defaults (= node today)
  [N]='thp:never'                                                # 4K heap bound
  [A]='thp:always,metadata_thp:always'                           # arm (a)
  [H]='hpa:true'                                                 # arm (b), HPA defaults (lazy hugify)
  [E]='hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152'  # arm (b'), eager advice, purge whole hugepages only
  [G]='glibc'                                                    # no preload (ub.sh calibration path)
)
declare -A SEEN=()
for k in "$@"; do
  n=$(( ${SEEN[$k]:-0} + 1 )); SEEN[$k]=$n; lab="$pfx-$k$n"
  "$S" '^/home/oz/.cargo-target-s80-thp-heap/release/deps/ubench_econ' "$P/results/thp-ub-$lab.tsv" 1 3700 300 &
  sp=$!
  c=${CONF[$k]}
  if [ "$c" = glibc ]; then "$P/ub.sh" thp-heap "$lab" "${X[@]}"
  elif [ -z "$c" ]; then "$P/ub.sh" thp-heap "$lab" "${X[@]}" LD_PRELOAD=/usr/lib/libjemalloc.so
  else "$P/ub.sh" thp-heap "$lab" "${X[@]}" LD_PRELOAD=/usr/lib/libjemalloc.so "MALLOC_CONF=$c"; fi
  wait "$sp"
  awk -F'\t' -v l="$lab" 'NR>1{n++; s+=$5; if($5<m||m=="")m=$5; a+=$3} END{if(n) printf "THP %s samples=%d mean_thp_pct=%.1f min=%.1f mean_anon_MB=%.0f\n", l, n, s/n, m, a/n/1024}' "$P/results/thp-ub-$lab.tsv"
done
