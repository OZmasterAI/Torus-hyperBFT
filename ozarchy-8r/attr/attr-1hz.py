#!/usr/bin/env python3
"""1 Hz conditioning: per cell, post window, per node-second: exec block-seconds per fill (us), native blocks, view
duration, exec runqueue proxy n/a. Seconds split by state DB compaction activity (compaction_cpu_micros increment in
the second > 0) and by consensus DB compaction activity. Also seconds within +-2 s of a DA GC slow-pass WARN line and
of a state pruner pass. attr-1hz.py ROOT RUN_DIR"""

import csv
import datetime as dt
import gzip
import json
import pathlib
import re
import statistics as st
import sys

NODES = ("val0", "val1", "val2")
ORDER = [
    "r1-A",
    "r1-B",
    "r1-C",
    "r2-C",
    "r2-B",
    "r2-A",
    "r3-A",
    "r3-B",
    "r3-C",
    "r4-C",
    "r4-B",
    "r4-A",
]
COLS = (
    "torus_exec_block_seconds_sum",
    "torus_orders_matched_total",
    "torus_exec_native_blocks_total",
    "torus_rocksdb_compaction_cpu_micros",
    "torus_rocksdb_consensus_compaction_cpu_micros",
    "torus_view_duration_seconds_sum",
    "torus_view_duration_seconds_count",
    "torus_rocksdb_running_compactions",
    "torus_rocksdb_consensus_running_compactions",
)
TSRE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z")


def load(d):
    rows = {n: [] for n in NODES}
    with open(d / "sampler.csv") as f:
        for r in csv.DictReader(f):
            if r["node"] in rows:
                try:
                    rows[r["node"]].append(
                        (float(r["ts"]), [float(r[c]) for c in COLS])
                    )
                except (ValueError, KeyError):
                    continue
    for n in NODES:
        rows[n].sort()
    return rows


def log_times(d, n, pat):
    out = []
    with gzip.open(d / f"{n}.log.gz", "rt", errors="replace") as f:
        for line in f:
            if pat in line:
                m = TSRE.search(line)
                if m:
                    out.append(
                        dt.datetime.fromisoformat(m.group(1))
                        .replace(tzinfo=dt.timezone.utc)
                        .timestamp()
                    )
    return out


def near(ts, marks, lo, hi):
    return any(lo <= ts - m <= hi for m in marks)


def per_second(rows, t0, t1, marks):
    out = []
    for (ta, a), (tb, b) in zip(rows, rows[1:]):
        if not (t0 <= ta and tb <= t1) or tb - ta > 1.5:
            continue
        dm = b[1] - a[1]
        out.append(
            {
                "us_fill": (b[0] - a[0]) / dm * 1e6 if dm > 0 else None,
                "blk": (b[2] - a[2]) / (tb - ta),
                "fills": dm / (tb - ta),
                "scomp": b[3] - a[3] > 0 or a[7] > 0 or b[7] > 0,
                "ccomp": b[4] - a[4] > 0 or a[8] > 0 or b[8] > 0,
            "ex": b[0] - a[0],
            "dm": dm,
                "slow": near(tb, marks["slow"], -1.0, 2.0),
                "prune": near(tb, marks["prune"], -1.0, 5.0),
            }
        )
    return out


def summarize(xs, key, cond):
    a = [x for x in xs if cond(x)]
    if not a:
        return (float("nan"), 0)
    if key == "us_fill":
        a = [x for x in a if x["dm"] > 0]
        return (sum(x["ex"] for x in a) / sum(x["dm"] for x in a) * 1e6, len(a))
    return (st.mean(x[key] for x in a), len(a))


def main():
    root, run = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    meta = json.loads((run / "analyst" / "r8-analyse.json").read_text())["meta"]
    print(
        "| cell | s | us/fill no comp | us/fill state comp | n state | us/fill cons comp | us/fill +-slow pass | n slow | us/fill pruner +5 s | n prune | blk/s no comp | blk/s state comp |"
    )
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    agg = {}
    for k in ORDER:
        d = root / f"ozarchy-8r-300m-{k}"
        s = json.loads((d / "summary.json").read_text())
        tb0 = s["timing"]["t_bench0"]
        m = meta[k[:2]]
        t0, t1 = tb0 + m["w0"], tb0 + m["w1"]
        rows = load(d)
        xs = []
        for n in NODES:
            marks = {
                "slow": log_times(d, n, "slow pass"),
                "prune": log_times(d, n, "pruning complete"),
            }
            xs += per_second(rows[n], t0, t1, marks)
        none = lambda x: not x["scomp"] and not x["ccomp"]
        r = {
            "none": summarize(xs, "us_fill", none),
            "s": summarize(xs, "us_fill", lambda x: x["scomp"]),
            "c": summarize(xs, "us_fill", lambda x: x["ccomp"] and not x["scomp"]),
            "slow": summarize(xs, "us_fill", lambda x: x["slow"]),
            "prune": summarize(xs, "us_fill", lambda x: x["prune"]),
            "bn": summarize(xs, "blk", none),
            "bs": summarize(xs, "blk", lambda x: x["scomp"]),
        }
        agg.setdefault(k[-1], []).append(r)
        f = lambda v: "n/a" if v[1] == 0 else f"{v[0]:.3f}"
        print(
            f"| {k} | {len(xs)} | {f(r['none'])} | {f(r['s'])} | {r['s'][1]} | {f(r['c'])} | {f(r['slow'])} | {r['slow'][1]} | {f(r['prune'])} | {r['prune'][1]} | {r['bn'][0]:.2f} | {r['bs'][0]:.2f} |"
        )
    for arm, rs in sorted(agg.items()):
        mean = lambda key: st.mean(r[key][0] for r in rs if r[key][1])
        nsum = lambda key: sum(r[key][1] for r in rs)
        print(
            f"arm {arm}: us/fill none {mean('none'):.3f} state-comp {mean('s'):.3f} (n {nsum('s')}) cons-comp {mean('c'):.3f} (n {nsum('c')}) blk/s none {mean('bn'):.2f} state-comp {mean('bs'):.2f}"
        )


if __name__ == "__main__":
    main()
