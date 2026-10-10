# Parse a node log (gz) into per-height times: commit (on_committed_block sending), exec start, exec done.
import gzip,re,sys,json,datetime
ANSI=re.compile(r'\x1b\[[0-9;]*m')
TS=re.compile(r'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z')
def ts(s):
    m=TS.match(s)
    if not m: return None
    return datetime.datetime.strptime(m.group(1),'%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp()
def parse(path):
    commit={};start={};done={};native={}
    for line in gzip.open(path,'rt',errors='replace'):
        if 'execution pipeline' not in line and 'on_committed_block' not in line: continue
        l=ANSI.sub('',line)
        m=re.search(r'height=(\d+)',l)
        if not m: continue
        h=int(m.group(1)); t=ts(l)
        if 'on_committed_block: sending' in l: commit[h]=t
        elif 'executing finalized block' in l:
            start[h]=t; native[h]=('has_native=true' in l)
        elif 'block done' in l: done[h]=t
    return commit,start,done,native
if __name__=='__main__':
    c,s,d,n=parse(sys.argv[1])
    out={'commit':c,'start':s,'done':d,'native':n}
    json.dump(out,open(sys.argv[2],'w'))
    print(len(c),len(s),len(d),sum(n.values()))
