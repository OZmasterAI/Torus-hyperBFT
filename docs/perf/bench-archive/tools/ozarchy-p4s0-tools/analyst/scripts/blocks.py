import gzip, sys, re, json, datetime
cell=sys.argv[1]; node=sys.argv[2] if len(sys.argv)>2 else 'val1'
s=json.load(open(f'{cell}/summary.json')); T=s['timing']
ansi=re.compile(r'\x1b\[[0-9;]*m')
def ts(x): return datetime.datetime.strptime(x[:26],'%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp()
start={}; done={}; ncount={}; sent={}; pipe={}
for line in gzip.open(f'{cell}/{node}.log.gz','rt',errors='replace'):
    line=ansi.sub('',line)
    if 'execution pipeline' not in line and 'sending to execution' not in line: continue
    m=re.search(r'height=(\d+)',line)
    if not m: continue
    h=int(m.group(1)); t=ts(line)
    if 'executing finalized block' in line:
        start[h]=t; ncount[h]=int(re.search(r'native_count=(\d+)',line).group(1)); pipe[h]=int('pipelined=true' in line)
    elif 'block done' in line: done[h]=t
    elif 'sending to execution' in line: sent[h]=t
rows=[]
for h in sorted(start):
    if h in done: rows.append((h,start[h],done[h],1000*(done[h]-start[h]),ncount[h],sent.get(h),pipe[h]))
out=f'{sys.argv[3]}' if len(sys.argv)>3 else None
w=[r for r in rows if T['t_bench0']<=r[1]<=T['t_drain']]
print(cell.split('/')[-1],node,'blocks in log',len(rows),'in bench+drain',len(w),'native>0',sum(1 for r in w if r[4]>0),'native>0 in load',sum(1 for r in w if r[4]>0 and r[1]<=T['t_bench1']))
if out:
    with open(out,'w') as f:
        f.write('height,t_start,t_done,exec_ms,native_count,t_sent,pipelined\n')
        for r in rows: f.write(','.join(str(x) for x in r)+'\n')
