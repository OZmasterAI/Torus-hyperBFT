import sys,collections,re
def load(p):
    d={}
    for l in open(p):
        x=l.split()
        if len(x)<5: continue
        d[(x[0],x[1])]=(x[2],int(x[3]),int(x[4]),int(x[5]))
    return d
cell=sys.argv[1]
s=load(cell+'/io-threads-start.txt'); e=load(cell+'/io-threads-end.txt')
t0=float(open(cell+'/io-threads-start.txt.ts').read()); t1=float(open(cell+'/io-threads-end.txt.ts').read())
agg=collections.Counter(); aggc=collections.Counter()
for k,(n,r,w,c) in e.items():
    n0=re.sub(r'\d+$','',n)
    w0=s.get(k,(n,0,0,0))[2]; c0=s.get(k,(n,0,0,0))[3]
    agg[n0]+=w-w0; aggc[n0]+=c-c0
dur=t1-t0
print(cell.split('-300m-')[1], 'dur %.1f'%dur)
for n,v in agg.most_common(8): print('  %-22s write %.1f MB/s wchar %.1f MB/s'%(n,v/dur/1e6,aggc[n]/dur/1e6))
