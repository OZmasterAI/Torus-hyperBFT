#!/usr/bin/env python3
"""attrib.py <cell> <elf> <bucket> <anchor-substr> [out.json]
For exec samples in a buckets2.py bucket, take the innermost frame whose symbol contains anchor, symbolize its
address (return address - 1 for non-leaf) with llvm-addr2line -i, and aggregate period by the anchor-function
source line (outermost inline entry) and by innermost inlined function / callee. Output ms/1k (buckets2 method)."""
import sys, re, gzip, json, subprocess, collections, importlib.util, io, contextlib
cell, elf, bucket, anchor = sys.argv[1:5]
spec = importlib.util.spec_from_file_location('b', '/home/oz/bench-results-matched/ozarchy-14236fa-tools/buckets2.py')
sys.argv = ['x', cell]
with contextlib.redirect_stdout(io.StringIO()):
    b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)
tot = sum(int(l.rsplit(' ', 1)[1]) for l in open(cell + '/perf.folded'))
K = lambda v: b.K(v / tot)
H = re.compile(r'\[[0-9a-f]{6,16}\]')
nm = subprocess.run(['nm', '-C', elf], capture_output=True, text=True).stdout
base = None
def samples():
    name = cell.rstrip('/').split('/')[-1]
    f = gzip.open(f'/home/oz/bench-results-matched/ozarchy-gap-after-c7-tools/{name}.exec-script.gz', 'rt')
    per = 0; fr = []
    for line in f:
        if not line.strip():
            if fr: yield per, fr
            fr = []; continue
        if line[0] not in ' \t':
            per = int(line.split()[-1]); continue
        p = line.strip().split(' ', 1)
        ip = int(p[0], 16); sym = p[1] if len(p) > 1 else '[unknown]'
        off = 0
        m = re.match(r'^(.*)\+0x([0-9a-f]+)$', sym)
        if m: sym, off = m.group(1), int(m.group(2), 16)
        fr.append((ip, H.sub('', sym).replace(';', ','), off))
    if fr: yield per, fr
agg = collections.Counter(); btot = 0; anch = 0
for per, fr in samples():
    names = [s for _, s, _ in fr if not s.startswith('[unknown]')]
    outer = list(reversed(names))
    for name, keys in b.PHASE:
        if any(any(k in x for k in keys) for x in outer): break
    else: continue
    if bucket != '*' and name != bucket: continue
    btot += per
    idx = next((i for i, (_, s, _) in enumerate(fr) if anchor in s), None)
    if idx is None: agg[(None, '(anchor not on stack)')] += per; continue
    ip, s, off = fr[idx]
    if base is None:
        for l in nm.splitlines():
            a, t, n = l.split(' ', 2)
            if n == s: base = (ip - off) - int(a, 16); break
        assert base is not None, s
    addr = ip - base - (1 if idx > 0 else 0)
    callee = '(self)' if idx == 0 else fr[idx - 1][1]
    if idx > 0 and callee.startswith('[unknown]'): callee = '(self)'
    agg[(addr, callee)] += per; anch += per
addrs = sorted({a for a, _ in agg if a is not None})
out = subprocess.run(['llvm-addr2line', '-a', '-i', '-f', '-C', '-e', elf], input='\n'.join(hex(a) for a in addrs),
                     capture_output=True, text=True).stdout.splitlines()
chain = {}; cur = None; k = 0
while k < len(out):
    l = out[k]
    if l.startswith('0x'): cur = int(l, 16); chain[cur] = []; k += 1; continue
    fn = l; loc = out[k + 1] if k + 1 < len(out) else '?'; k += 2
    loc = re.sub(r'^.*/(crates|library|src|\.cargo/registry/src/[^/]+)/', r'\1/', loc)
    chain[cur].append((fn, loc))
def short(f): return re.sub(r'<torus_state::backend::NativeStateOverlay>', '', re.sub(r'::\{closure#\d+\}', '{cl}', f))[:110]
byline = collections.Counter(); byline_what = collections.Counter(); byinner = collections.Counter()
for (a, callee), v in agg.items():
    if a is None: byline['(no anchor)'] += v; continue
    c = chain.get(a, [('?', '?')])
    line = c[-1][1]
    # innermost: if leaf -> innermost inlined fn:line; else the callee symbol
    what = (short(c[0][0]) + ' @' + c[0][1]) if callee == '(self)' else 'CALL ' + short(callee)
    # first-level inlined fn under anchor (c[-2]) if any
    lvl1 = short(c[-2][0]) if len(c) > 1 else '-'
    byline[line] += v; byline_what[(line, lvl1, what)] += v; byinner[what] += v
print(f"bucket {bucket}: {K(btot):.3f} ms/1k (anchor on stack {K(anch):.3f})")
print("=== by anchor source line"); [print(f"{K(v):7.3f} {k}") for k, v in byline.most_common(70)]
print("=== by line / first inlined fn / innermost-or-callee"); [print(f"{K(v):7.3f} {k[0]} | {k[1]} | {k[2]}") for k, v in byline_what.most_common(120)]
print("=== by innermost-or-callee"); [print(f"{K(v):7.3f} {k}") for k, v in byinner.most_common(60)]
