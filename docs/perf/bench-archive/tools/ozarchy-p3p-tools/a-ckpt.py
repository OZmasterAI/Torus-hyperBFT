import gzip, re, json, statistics as st, csv
R="/home/oz/bench-results-matched/ozarchy-p3p-300m-"
pat=re.compile(r"p3proto checkpoint height=Some\((\d+)\) blocks=(\d+) rows=(\d+) bytes=(\d+) build_ms=([\d.e-]+) write_ms=([\d.e-]+) absorb_ms=([\d.e-]+)")
for arm in ("a","aS","k"):
    B=[];W=[];A=[];BY=[];RW=[];tot=[]
    for r in range(1,5):
        tag=f"r{r}-{arm}"
        for v in ("val0","val1","val2"):
            n=0;sumbw=0
            for l in gzip.open(R+tag+f"/{v}.log.gz","rt",errors="replace"):
                m=pat.search(re.sub(r"\x1b\[[0-9;]*m","",l))
                if not m: continue
                h,b,rows,by,bm,wm,am=int(m[1]),int(m[2]),int(m[3]),int(m[4]),float(m[5]),float(m[6]),float(m[7])
                if b==100 and rows>(100000 if arm!="k" else 299):  # load-window checkpoints
                    B.append(bm);W.append(wm);A.append(am/b);BY.append(by/1e6);RW.append(rows);n+=1;sumbw+=bm+wm
            tot.append((n,sumbw/1000))
    q=lambda xs:(round(min(xs),3),round(st.median(xs),3),round(max(xs),3))
    print(arm,"n_ckpt",len(B),"build ms",q(B),"write ms",q(W),"absorb ms/blk",q(A),"MB",q(BY),"rows",q(RW),"per node: ckpts,(build+write s)",(min(t[0] for t in tot),max(t[0] for t in tot)),round(st.mean(t[1] for t in tot),2))
# RSS peak per arm
for arm in ("c","a","k","cS","aS"):
    pk=[]
    for r in range(1,5):
        rows=list(csv.DictReader(open(R+f"r{r}-{arm}/cpu.csv")))
        by={}
        for x in rows:
            if x["comm"]=="torus-node": by.setdefault(x["pid"],[]).append(int(x["rss_kb"]))
        pk+= [max(v)/1024 for v in by.values()]
    print(arm,"peak RSS MB per node mean %.0f min %.0f max %.0f"%(st.mean(pk),min(pk),max(pk)))
