import sys, re, collections
agg = collections.Counter(); sec=None
for l in open(sys.argv[1]):
    if l.startswith('==='): sec=l; continue
    if not sec or 'first inlined' not in sec: continue
    v, rest = l.split(None, 1); parts = rest.split(' | ')
    line = re.sub(r' \(discriminator \d+\)', '', parts[0]); lv = re.sub(r'<.*', '', parts[1]); what = parts[2].strip()
    key = f"{line} {lv}"
    if what.startswith('CALL') and lv == '-': key += ' ' + re.sub(r'<torus_core::order_book::|<torus_types::', '<', what[:70])
    agg[key] += float(v)
for k, v in agg.most_common(int(sys.argv[2]) if len(sys.argv)>2 else 25): print(f"{v:7.3f} {k}")
