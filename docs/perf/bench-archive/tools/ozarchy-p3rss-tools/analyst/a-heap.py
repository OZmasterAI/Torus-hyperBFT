"""jemalloc heap_v2 dumps -> live heap by stack, symbolized with llvm-addr2line (inline frames, demangled). python3 -I.
Usage: a-heap.py BINARY DUMP [DUMP ...] [--top N] [--json OUT]
Unbiasing as jeprof (heap_v2): scale = 1 / (1 - exp(-(bytes/count) / sample_period)). Addresses in the node binary are mapped
to file vaddrs through the dump's MAPPED_LIBRARIES (load base = the binary's offset-0 mapping); return addresses - 1."""
import collections
import json
import math
import re
import subprocess
import sys

STK = re.compile(r"^@((?: 0x[0-9a-f]+)+)\s*$")
CNT = re.compile(r"^\s*t\*: (\d+): (\d+) ")
MAP = re.compile(r"^([0-9a-f]+)-([0-9a-f]+) (\S+) ([0-9a-f]+) \S+ \S+\s+(\S+)$")


def parse(path):
    period = None
    stacks = []
    maps = []
    cur = None
    inmaps = False
    for line in open(path):
        if period is None and line.startswith("heap_v2/"):
            period = int(line.split("/")[1])
            continue
        if line.startswith("MAPPED_LIBRARIES"):
            inmaps = True
            continue
        if inmaps:
            m = MAP.match(line.strip())
            if m:
                maps.append((int(m.group(1), 16), int(m.group(2), 16), int(m.group(4), 16), m.group(5)))
            continue
        m = STK.match(line)
        if m:
            cur = [int(a, 16) for a in m.group(1).split()]
            continue
        m = CNT.match(line)
        if m and cur is not None:
            c, b = int(m.group(1)), int(m.group(2))
            if c:
                r = (b / c) / period
                s = 1 / (1 - math.exp(-r)) if r > 0 else 1
                stacks.append((cur, b * s))
            cur = None
    return stacks, maps


def symbolize(binary, addrs):
    res = {}
    addrs = sorted(addrs)
    if not addrs:
        return res
    p = subprocess.run(["llvm-symbolizer", "--inlining", "--functions=linkage", "--demangle", "--obj=" + binary],
                       input="\n".join(hex(a) for a in addrs) + "\n", capture_output=True, text=True)
    out = p.stdout.split("\n\n")
    for a, blk in zip(addrs, out):
        ls = [x for x in blk.strip().split("\n") if x]
        res[a] = [ls[i] for i in range(0, len(ls), 2)]  # function names, innermost first
    return res


def segments(binary):
    segs = []
    for line in subprocess.run(["readelf", "-lW", binary], capture_output=True, text=True).stdout.splitlines():
        f = line.split()
        if f and f[0] == "LOAD":
            segs.append((int(f[1], 16), int(f[2], 16), int(f[4], 16)))  # file offset, vaddr, filesz
    return segs


def to_vaddr(a, maps, segs):
    for lo, hi, off, name in maps:
        if lo <= a < hi and name.endswith("/torus-node"):
            fo = a - lo + off
            for so, sv, sz in segs:
                if so <= fo < so + max(sz, 1) + 0x1000:
                    return fo - so + sv
    return None


def frames_for(binary, dumps):
    segs = segments(binary)
    allst = []
    for d in dumps:
        st, maps = parse(d)
        bm = [m for m in maps if m[3].endswith("/torus-node")]
        base = min(m[0] - m[2] for m in bm) if bm else 0
        lo = min(m[0] for m in bm) if bm else 0
        hi = max(m[1] for m in bm) if bm else 0
        allst.append((d, st, base, lo, hi, maps))
    need = set()
    for _, st, base, lo, hi, maps in allst:
        for stack, _ in st:
            for i, a in enumerate(stack):
                if lo <= a < hi:
                    v = to_vaddr(a, maps, segs)
                    if v is not None:
                        need.add(v - (1 if i else 0))
    sym = symbolize(binary, need)
    out = []
    for d, st, base, lo, hi, maps in allst:
        rows = []
        for stack, b in st:
            fr = []
            for i, a in enumerate(stack):
                v = to_vaddr(a, maps, segs) if lo <= a < hi else None
                if v is not None:
                    fr += sym.get(v - (1 if i else 0), ["?"])
                else:
                    lib = next((m[3].rsplit("/", 1)[-1] for m in maps if m[0] <= a < m[1]), "?")
                    fr.append("[" + lib + "]")
            rows.append((fr, b))
        out.append((d, rows))
    return out


def main():
    a = sys.argv[1:]
    top = int(a[a.index("--top") + 1]) if "--top" in a else 30
    js = a[a.index("--json") + 1] if "--json" in a else None
    files = [x for x in a[1:] if x.endswith(".heap")]
    res = frames_for(a[0], files)
    dump = {}
    for d, rows in res:
        tot = sum(b for _, b in rows)
        print(f"== {d}: {tot / 1e6:.1f} MB live (unbiased), {len(rows)} stacks")
        agg = collections.Counter()
        for fr, b in rows:
            key = " < ".join(f for f in fr if "torus" in f or "hotstuff" in f or "rocksdb" in f)[:600]
            agg[key or " < ".join(fr[:6])[:300]] += b
        for k, b in agg.most_common(top):
            print(f"  {b / 1e6:8.1f} MB  {k[:400]}")
        dump[d] = [(fr, b) for fr, b in rows]
    if js:
        json.dump(dump, open(js, "w"))


main()
