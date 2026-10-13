import collections, sys
d = collections.defaultdict(collections.Counter)
for l in open(sys.argv[1]):
    t, p, k = l.rstrip("\n").split(" ", 2); d[k][t.split("-")[1]] += int(p) / 3
tot = {a: sum(v[a] for v in d.values()) for a in ("c0", "c100")}
print("Keccak-leaf samples WITH an intact user chain (~2% of Keccak samples: wrapper/absorb frames), share of period by thread | caller | caller's caller")
for k, v in sorted(d.items(), key=lambda x: -(x[1]["c0"] + x[1]["c100"]))[:25]:
    print(f"c0 {v['c0']/tot['c0']*100:5.1f}%  c100 {v['c100']/tot['c100']*100:5.1f}%  {k}")
