#!/usr/bin/env bash
# run-bench2.sh <label> <dur> <bench-args...> — node-sourced honest split
label="$1"; dur="$2"; shift 2
M=127.0.0.1:29090
samp="devnet/samp-$label.tsv"; : > "$samp"
get(){ echo "$1" | grep -E "^$2 " | awk '{print $2}'; }
( end=$((SECONDS+dur+18))
  while [ $SECONDS -lt $end ]; do
    m=$(curl -s --max-time 3 $M/metrics)
    printf '%s\t%s\t%s\t%s\t%s\n' "$(date +%s.%N)" \
      "$(get "$m" torus_native_actions_processed_total)" \
      "$(get "$m" torus_orders_matched_total)" \
      "$(get "$m" torus_orders_placed_accepted_total)" \
      "$(get "$m" torus_blocks_committed_total)" >> "$samp"
    sleep 1
  done ) & SAMP=$!
./target/release/bench-throughput consensus "$@" > "devnet/benchout-$label.log" 2>&1
kill $SAMP 2>/dev/null; wait $SAMP 2>/dev/null
awk -F'\t' '{n++;t[n]=$1;a[n]=$2;f[n]=$3;p[n]=$4;b[n]=$5}
END{W=14;bA=bF=bP=bB=0;
 for(i=1;i<=n;i++)for(j=i+1;j<=n;j++){d=t[j]-t[i]; if(d>=W&&d<=W+2){
   x=(a[j]-a[i])/d;if(x>bA)bA=x; y=(f[j]-f[i])/d;if(y>bF)bF=y;
   z=(p[j]-p[i])/d;if(z>bP)bP=z; w=(b[j]-b[i])/d;if(w>bB)bB=w;}}
 pA=pF=pP=0;
 for(i=2;i<=n;i++){d=t[i]-t[i-1];if(d>0){
   x=(a[i]-a[i-1])/d;if(x>pA)pA=x; y=(f[i]-f[i-1])/d;if(y>pF)pF=y;
   z=(p[i]-p[i-1])/d;if(z>pP)pP=z;}}
 printf "SUSTAINED(best %ds win): actions/s=%.0f  placed/s=%.0f  fills/s=%.0f  blocks/s=%.1f\n",W,bA,bP,bF,bB;
 printf "BURST(peak 1s):          actions/s=%.0f  placed/s=%.0f  fills/s=%.0f\n",pA,pP,pF;
 printf "samples=%d\n",n}' "$samp"
echo "--- bench HEADLINE (its own node-counter read, cross-check) ---"
grep -iE 'HEADLINE|Matched:|Placed accepted|blk/s|block rate|health' "devnet/benchout-$label.log" | head -12
echo "DONE-$label"
