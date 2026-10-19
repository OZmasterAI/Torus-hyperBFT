# Early-vs-late per-block phase-timer analysis over scrape-phase CSV.
# cols: ts,committed,height,placed,matched,resting,exec_resting,
#       lb_s,lb_c,root_s,root_c,sw_s,sw_c,evm_s,evm_c,fl_s,fl_c,db_s,db_c,execq
BEGIN{FS=","}
NR==1{next}
{ n++;
  ts[n]=$1; cm[n]=$2; ht[n]=$3; pl[n]=$4; mt[n]=$5; rest[n]=$6; er[n]=$7;
  lbs[n]=$8; lbc[n]=$9; rts[n]=$10; rtc[n]=$11; sws[n]=$12; swc[n]=$13;
  evs[n]=$14; evc[n]=$15; fls[n]=$16; flc[n]=$17; dbs[n]=$18; dbc[n]=$19; eq[n]=$20;
}
END{
  if(n<4){print "insufficient samples"; exit}
  # ---- funnel window / sliding-60s ----
  worst=1e9; bpl=0; bmt=0; peakq=0;
  for(i=1;i<=n;i++){ if(eq[i]+0>peakq) peakq=eq[i]+0;
    j=0; for(k=i+1;k<=n;k++){ if(ts[k]-ts[i]>=60){j=k;break} }
    if(j==0) continue; dt=ts[j]-ts[i];
    b=(ht[j]-ht[i])/dt; if(b<worst) worst=b;
    p=(pl[j]-pl[i])/dt; if(p>bpl) bpl=p;
    m=(mt[j]-mt[i])/dt; if(m>bmt) bmt=m;
  }
  span=ts[n]-ts[1];
  printf "FUNNEL: span=%ds placed/s(win)=%.1f matched/s(win)=%.1f blk/s(win)=%.3f | best60 placed=%.0f matched=%.0f | worst60 blk/s=%.3f | peak_execq=%d\n",
    span,(pl[n]-pl[1])/span,(mt[n]-mt[1])/span,(ht[n]-ht[1])/span,bpl,bmt,worst,peakq;
  # early window: first sample .. first sample >= t0+60
  e1=1; e2=0;
  for(k=2;k<=n;k++){ if(ts[k]-ts[e1]>=60){e2=k;break} }
  if(e2==0) e2=n;
  # late window: last sample <= tN-60 .. last sample
  l2=n; l1=0;
  for(k=n-1;k>=1;k--){ if(ts[l2]-ts[k]>=60){l1=k;break} }
  if(l1==0) l1=1;
  printf "EARLY window: t=%ds..%ds (resting %d->%d, exec_resting %d->%d)\n", ts[e1]-ts[1], ts[e2]-ts[1], rest[e1], rest[e2], er[e1], er[e2];
  printf "LATE  window: t=%ds..%ds (resting %d->%d, exec_resting %d->%d)\n", ts[l1]-ts[1], ts[l2]-ts[1], rest[l1], rest[l2], er[l1], er[l2];
  printf "%-22s %12s %12s %10s   %s\n","phase(ms/block)","EARLY","LATE","x(late/early)","[obs/blk E,L]";
  rpt("load_books", lbs[e1],lbs[e2],lbc[e1],lbc[e2], lbs[l1],lbs[l2],lbc[l1],lbc[l2], cm[e1],cm[e2],cm[l1],cm[l2]);
  rpt("root",       rts[e1],rts[e2],rtc[e1],rtc[e2], rts[l1],rts[l2],rtc[l1],rtc[l2], cm[e1],cm[e2],cm[l1],cm[l2]);
  rpt("state_write",sws[e1],sws[e2],swc[e1],swc[e2], sws[l1],sws[l2],swc[l1],swc[l2], cm[e1],cm[e2],cm[l1],cm[l2]);
  rpt("evm_resync", evs[e1],evs[e2],evc[e1],evc[e2], evs[l1],evs[l2],evc[l1],evc[l2], cm[e1],cm[e2],cm[l1],cm[l2]);
  rpt("flush",      fls[e1],fls[e2],flc[e1],flc[e2], fls[l1],fls[l2],flc[l1],flc[l2], cm[e1],cm[e2],cm[l1],cm[l2]);
  # dirty_buckets: avg buckets/obs (not ms)
  de=davg(dbs[e1],dbs[e2],dbc[e1],dbc[e2]); dl=davg(dbs[l1],dbs[l2],dbc[l1],dbc[l2]);
  printf "%-22s %12.1f %12.1f %10s\n","dirty_buckets/obs", de, dl, (de>0?sprintf("%.2f",dl/de):"-");
}
function davg(s1,s2,c1,c2,  ds,dc){ ds=s2-s1; dc=c2-c1; return (dc>0? ds/dc : 0) }
function rpt(name, es1,es2,ec1,ec2, ls1,ls2,lc1,lc2, cme1,cme2,cml1,cml2,   ea,la,obe,obl){
  ea=davg(es1,es2,ec1,ec2)*1000.0; la=davg(ls1,ls2,lc1,lc2)*1000.0;
  obe=(cme2-cme1>0?(ec2-ec1)/(cme2-cme1):0); obl=(cml2-cml1>0?(lc2-lc1)/(cml2-cml1):0);
  printf "%-22s %12.2f %12.2f %10s   [%.1f,%.1f]\n", name, ea, la, (ea>0?sprintf("%.2f",la/ea):"-"), obe, obl;
}
