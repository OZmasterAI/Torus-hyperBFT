"""val0 smaps snapshots (every 30 s) -> RSS by mapping class (python3 -I). Usage: a-smaps.py CELLS.json PREFIX
Classes: binary (torus-node file pages), libs (.so), heap ([heap] brk), anon-arena (anonymous mappings >= 32 MiB: glibc
thread arenas / jemalloc extents / large mmaps), anon-small (< 32 MiB: thread stacks + guard, small mmaps), other.
Reports, per cell, the snapshot with the largest total Rss inside the bench window and the window mean per class (MB)."""
import glob
import gzip
import json
import os
import re
import statistics as st
import sys

HDR = re.compile(r"^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$")


def classes(path):
    out = {}
    cur = None
    size = 0
    with gzip.open(path, "rt") as fh:
        for line in fh:
            m = HDR.match(line)
            if m:
                name = m.group(4).strip()
                size = int(m.group(2), 16) - int(m.group(1), 16)
                if name.endswith("/torus-node"):
                    cur = "binary"
                elif ".so" in name:
                    cur = "libs"
                elif name == "[heap]":
                    cur = "heap"
                elif name == "":
                    cur = "anon-arena" if size >= 32 << 20 else "anon-small"
                else:
                    cur = "other"
                continue
            if line.startswith("Rss:") and cur:
                out[cur] = out.get(cur, 0) + int(line.split()[1]) / 1024
    return out


def main():
    C = json.load(open(sys.argv[1]))
    P = sys.argv[2]
    res = {}
    for tag, x in C.items():
        snaps = []
        for f in sorted(glob.glob(f"{P}{tag}/smaps/val0-*.smaps.gz")):
            t = int(os.path.basename(f).split("-")[1].split(".")[0])
            if x["t0"] <= t <= x["t1"] + 5:
                snaps.append(classes(f))
        if not snaps:
            continue
        keys = sorted({k for s in snaps for k in s})
        peak = max(snaps, key=lambda s: sum(s.values()))
        res[tag] = dict(arm=x["arm"], n=len(snaps), peak={k: peak.get(k, 0) for k in keys},
                        mean={k: st.mean(s.get(k, 0) for s in snaps) for k in keys})
    arms = {}
    for tag, r in res.items():
        arms.setdefault(r["arm"], []).append(r)
    allk = sorted({k for r in res.values() for k in r["mean"]})
    print("val0 RSS by mapping class, bench window, mean over the arm's cells: peak-snapshot / window-mean MB")
    print("arm    n  " + "  ".join(f"{k:>18s}" for k in allk) + "  total")
    for a, rs in arms.items():
        pk = {k: st.mean(r["peak"].get(k, 0) for r in rs) for k in allk}
        mn = {k: st.mean(r["mean"].get(k, 0) for r in rs) for k in allk}
        print(f"{a:6s} {len(rs)}  " + "  ".join(f"{pk[k]:8.0f}/{mn[k]:8.0f}" for k in allk) + f"  {sum(pk.values()):.0f}/{sum(mn.values()):.0f}")
    json.dump(res, open(os.path.join(os.path.dirname(sys.argv[1]), "smaps.json"), "w"), indent=0)


main()
