import re,sys,collections
def clippy(p):
    t=re.sub(r'\x1b\[[0-9;]*m','',open(p).read())
    c=collections.Counter(); lines=t.split('\n')
    for i,l in enumerate(lines):
        m=re.match(r'(warning|error)(\[\w+\])?: (.*)',l)
        if m and not re.match(r'.*(generated \d+ warning|could not compile)',l) and i+1<len(lines):
            loc=re.match(r'\s*--> (\S+?):(\d+):\d+',lines[i+1])
            if loc: c[(loc.group(1),m.group(3))]+=1
    return c
def fmt(p):
    t=open(p).read()
    c=collections.Counter()
    for m in re.finditer(r'Diff in (\S+?):(\d+)',t): c[re.sub(r'^.*?/(crates|tools|devnet|ci|tests)/',r'\1/',m.group(1))]+=1
    return c
a,b=clippy('4-cand-clippy.log'),clippy('4-base-clippy.log')
print('clippy cand',sum(a.values()),'base',sum(b.values()))
for k,v in (a-b).items(): print('NEW clippy',k,v)
print('gone',sum((b-a).values()))
for k,v in (b-a).items(): print(' gone',k,v)
a,b=fmt('4-cand-fmt.log'),fmt('4-base-fmt.log')
print('fmt hunks cand',sum(a.values()),'base',sum(b.values()))
for k,v in (a-b).items(): print('NEW fmt',k,v)
for k,v in (b-a).items(): print(' gone fmt',k,v)
