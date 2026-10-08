#!/usr/bin/env python3
"""scan.py <out_prefix>: perf script (-F comm,tid,time,period,ip,sym --no-inline) on stdin, passed through unchanged to
stdout (pipe it into ozarchy-14236fa-tools/fold.py --period). Side outputs:
  <out_prefix>.exec.script.gz  the samples of the exec-side threads (comm torus-execution*, torus-end-resid*,
                               torus-flush-wor*, torus-trade-wri*), same text format, for per-block timelines
  <out_prefix>.tids.json       thread ids seen in the samples: per comm group the distinct tids, the span of sample times,
                               distinct tids per 60 s bin and tids per minute over the window (a lower bound: a thread
                               shows up only if it got >= 1 sample, i.e. ran >= ~1/freq s)."""

import gzip
import json
import re
import sys
from collections import defaultdict

pre = sys.argv[1]
EXEC = ("torus-execution", "torus-end-resid", "torus-flush-wor", "torus-trade-wri")
GROUPS = [
    ("exec", "torus-execution"),
    ("end_resident", "torus-end-resid"),
    ("flush", "torus-flush-wor"),
    ("rpc", "rpc-worker"),
    ("tokio", "tokio-rt-worker"),
    ("ingress", "torus-ingress-v"),
    ("gossip", "torus-gossip-ve"),
    ("rocksdb", "rocksdb"),
]
HDR = re.compile(r"^(.*?)\s+(\d+)\s+(\d+\.\d+):\s+(\d+)")


def grp(c):
    for g, p in GROUPS:
        if c.startswith(p):
            return g
    return "other"


ex = gzip.open(pre + ".exec.script.gz", "wt")
tids = defaultdict(dict)  # group -> tid -> (first_t, last_t)
t_min, t_max = None, None
keep = False
out = sys.stdout
for line in sys.stdin:
    out.write(line)
    if not line.strip():  # end of a sample
        if keep:
            ex.write("\n")
        keep = False
        continue
    if line[0] not in " \t":  # sample header: comm tid time: period
        keep = False
        m = HDR.match(line)
        if m:
            comm, tid, t = m.group(1).strip(), int(m.group(2)), float(m.group(3))
            t_min = t if t_min is None else min(t_min, t)
            t_max = t if t_max is None else max(t_max, t)
            d = tids[grp(comm)]
            a = d.get(tid)
            d[tid] = (t, t) if a is None else (min(a[0], t), max(a[1], t))
            keep = comm.startswith(EXEC)
    if keep:
        ex.write(line)
ex.close()
res = {
    "t_min": t_min,
    "t_max": t_max,
    "span_s": round((t_max - t_min), 2) if t_min is not None else 0,
    "groups": {},
}
span = res["span_s"] or 1
for g, d in sorted(tids.items()):
    bins = defaultdict(set)
    for tid, (a, b) in d.items():
        for k in range(int((a - t_min) // 60), int((b - t_min) // 60) + 1):
            bins[k].add(tid)
    res["groups"][g] = {
        "distinct_tids": len(d),
        "tids_per_min": round(len(d) / span * 60, 1),
        "per_60s_bin": [len(bins[k]) for k in sorted(bins)],
        "short_lived_lt_1s": sum(1 for a, b in d.values() if b - a < 1.0),
    }
res["all_distinct_tids"] = sum(v["distinct_tids"] for v in res["groups"].values())
json.dump(res, open(pre + ".tids.json", "w"), indent=1)
