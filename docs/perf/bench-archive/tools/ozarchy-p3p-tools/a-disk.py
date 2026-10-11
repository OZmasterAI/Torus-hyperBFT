import json, statistics as st
R="/home/oz/bench-results-matched/ozarchy-p3p-300m-"
V=("val0","val1","val2")
def created(path):
    c={};
    for line in open(path,errors="replace"):
        i=line.find("EVENT_LOG_v1 ")
        if i<0 or "table_file_creation" not in line: continue
        try: d=json.loads(line[i+13:])
        except ValueError: continue
        if d.get("file_size",0)>0: c[d["file_number"]]=(d.get("cf_name"),d["file_size"])
    return c
for arm in ("c","a","k","cS","aS"):
    sst=[];wal=[];cr=[];flushB=[]
    for r in range(1,5):
        t=f"r{r}-{arm}"
        for l in open(R+t+"/data-du.txt"):
            p=l.split()
            if p and p[0] in V: sst.append(int(p[4])/1e9); wal.append(int(p[-1])/1e9)
        for v in V:
            cr.append(sum(s for _,s in created(R+t+f"/rocksdb-LOG-{v}.txt").values())/1e9)
    print(arm,"end SST GB %.2f WAL GB %.2f | SST created GB/validator %.2f (sd %.2f)"%(st.mean(sst),st.mean(wal),st.mean(cr),st.stdev(cr)))
