# Sliding 60s window analysis over scrape CSV.
# cols: ts,actions,placed,matched,resting,rejm,rejb,rejc,rejo,stc,cpf,committed,height,execq
BEGIN{FS=","}
NR==1{next}
{
  n++; ts[n]=$1; act[n]=$2; pl[n]=$3; mt[n]=$4; cm[n]=$12; ht[n]=$13; eq[n]=$14;
  if(eq[n]+0>maxeq) maxeq=eq[n]+0;
}
END{
  if(n<2){print "insufficient samples"; exit}
  worst_blk=1e9; best_pl=0; best_mt=0; best_blk=0;
  for(i=1;i<=n;i++){
    # find j = first sample >= ts[i]+60
    j=0;
    for(k=i+1;k<=n;k++){ if(ts[k]-ts[i]>=60){j=k;break} }
    if(j==0) continue;
    dt=ts[j]-ts[i];
    blk=(ht[j]-ht[i])/dt;
    plr=(pl[j]-pl[i])/dt;
    mtr=(mt[j]-mt[i])/dt;
    cmr=(cm[j]-cm[i])/dt;
    if(blk<worst_blk) worst_blk=blk;
    if(blk>best_blk) best_blk=blk;
    if(plr>best_pl) best_pl=plr;
    if(mtr>best_mt) best_mt=mtr;
  }
  span=ts[n]-ts[1];
  wpl=(pl[n]-pl[1])/span; wmt=(mt[n]-mt[1])/span; wblk=(ht[n]-ht[1])/span; wcm=(cm[n]-cm[1])/span; wact=(act[n]-act[1])/span;
  printf "samples=%d span=%ds\n", n, span;
  printf "WINDOW-AVG: placed/s=%.1f matched/s=%.1f blk/s(height)=%.3f committed/s=%.3f actions/s=%.2f\n", wpl,wmt,wblk,wcm,wact;
  printf "BEST-60s:   placed/s=%.1f matched/s=%.1f blk/s=%.3f\n", best_pl,best_mt,best_blk;
  printf "WORST-60s:  blk/s(height)=%.3f\n", worst_blk;
  printf "PEAK exec_queue_depth=%d\n", maxeq;
  printf "TOTALS: placed=%d matched=%d resting=%d committed=%d height_delta=%d\n", pl[n]-pl[1], mt[n]-mt[1], 0, cm[n]-cm[1], ht[n]-ht[1];
}
