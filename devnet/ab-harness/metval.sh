#!/usr/bin/env bash
# Print a metric value (sum across label sets). $1=port $2=exact_metric_name
curl -s -m 2 "http://127.0.0.1:$1/metrics" 2>/dev/null \
 | awk -v m="$2" '
   $0 ~ /^#/ {next}
   { n=$1; sub(/\{.*/,"",n); if(n==m){s+=$2; found=1} }
   END{ if(found) printf "%s", s; else printf "NA" }'
