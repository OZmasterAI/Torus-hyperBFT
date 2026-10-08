#!/usr/bin/env python3
"""ipc.py <cell_dir> [w1|w2 ...]: per thread group and per function event totals from ipc-sidecar perf data.
Event count = sum of sample periods (each event sampled independently, -F 499, call-graph fp).
Normalised per 1k fills (torus_orders_matched_total delta of val0 /metrics over the window).
Writes <cell_dir>/ipc-<w>.json and prints a table."""

import json, re, subprocess, sys
from collections import defaultdict

FUNCS = [  # name, symbol substring (matched on the closure-stripped symbol)
    ("match_market", "MarketWorkerPool>::match_market"),
    ("match_at_level", "OrderBook>::match_at_level"),
    ("insert_order", "OrderBook>::insert_order"),
    ("place_order_with_accounts", "OrderBook>::place_order_with_accounts"),
    ("settle_market_results_parallel", "settle_market_results_parallel"),
    ("compute_market_settle_plan", "compute_market_settle_plan"),
    ("execute_batch_phases", "NativeExecutor>::execute_batch_phases"),
    ("sort_native_actions", "sort_native_actions"),
    ("cancel_all_many", "cancel_all_many"),
    ("drain_book", "native_executor::drain_book"),
    ("PositionCache::flush_all", "PositionCache>::flush_all"),
    ("get_cf_raw", "::get_cf_raw"),
    ("positions_for_trader", "positions_for_trader"),
    ("maker_fill_fits", "maker_fill_fits"),
]
GROUPS = [
    ("exec", "torus-execution"),
    ("flush", "torus-flush-wor"),
    ("rpc", "rpc-worker"),
]
HDR = re.compile(r"^(.*?)\s+(\d+)\s+([\d.]+):\s+(\d+)\s+(\S+?):\s*$")
H = re.compile(r"\[[0-9a-f]{6,16}\]")
CL = re.compile(r"::\{closure#\d+\}|\{closure#\d+\}")


def metric(path, name):
    for l in open(path):
        if l.startswith(name + " ") or l.startswith(name + "{"):
            return float(l.split()[-1])
    raise KeyError(name)


def utime(path):  # process utime ticks (field 14)
    s = open(path).read()
    f = s[s.rindex(")") + 2 :].split()
    return int(f[11]), int(f[12])


def run(d, w):
    data = f"{d}/perf-{w}.data"
    grp = defaultdict(lambda: defaultdict(int))  # group -> event -> count
    inc = defaultdict(
        lambda: defaultdict(lambda: defaultdict(int))
    )  # group -> func -> event
    slf = defaultdict(lambda: defaultdict(lambda: defaultdict(int)))
    p = subprocess.Popen(
        ["perf", "script", "-i", data, "-F", "comm,tid,time,period,event,ip,sym", "--no-inline"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        errors="replace",
        bufsize=1 << 20,
    )
    cur = None
    tmin = [1e18]; tmax = [0.0]
    frames = []

    def flush():
        if cur is None:
            return
        comm, per, ev = cur
        gs = ["process"] + [g for g, pfx in GROUPS if comm.startswith(pfx)]
        syms = [CL.sub("", H.sub("", f)) for f in frames]
        hit_i = set()
        hit_s = set()
        for name, sub in FUNCS:
            if any(sub in s for s in syms):
                hit_i.add(name)
            if syms and sub in syms[0]:
                hit_s.add(name)
        for g in gs:
            grp[g][ev] += per
            for n in hit_i:
                inc[g][n][ev] += per
            for n in hit_s:
                slf[g][n][ev] += per

    for l in p.stdout:
        if l.startswith("\t") or l.startswith(" "):
            t = l.strip().split(None, 1)
            frames.append(t[1] if len(t) > 1 else "?")
        elif l.strip():
            flush()
            m = HDR.match(l.rstrip("\n"))
            if m:
                ts = float(m.group(3)); tmin[0] = min(tmin[0], ts); tmax[0] = max(tmax[0], ts)
            cur = (
                (m.group(1).strip(), int(m.group(4)), m.group(5).replace(":u", ""))
                if m
                else None
            )
            frames = []
    flush()
    p.wait()
    fills = metric(
        f"{d}/prof-metrics-{w}-after.txt", "torus_orders_matched_total"
    ) - metric(f"{d}/prof-metrics-{w}-before.txt", "torus_orders_matched_total")
    t0 = float(open(f"{d}/prof-{w}-before.ts").read())
    t1 = float(open(f"{d}/prof-{w}-after.ts").read())
    u0, s0 = utime(f"{d}/prof-stat-{w}-before.txt")
    u1, s1 = utime(f"{d}/prof-stat-{w}-after.txt")
    out = {
        "cell": d,
        "window": w,
        "fills": fills,
        "span_s": t1 - t0,
        "fills_per_s": fills / (t1 - t0),
        "utime_s": (u1 - u0) / 100,
        "stime_s": (s1 - s0) / 100,
        "groups": {g: dict(v) for g, v in grp.items()},
        "incl": {g: {n: dict(e) for n, e in v.items()} for g, v in inc.items()},
        "self": {g: {n: dict(e) for n, e in v.items()} for g, v in slf.items()},
    }
    json.dump(out, open(f"{d}/ipc-{w}.json", "w"), indent=1)
    samp = tmax[0] - tmin[0]
    out["sample_span_s"] = samp
    out["fills_sampled"] = fills / (t1 - t0) * samp
    json.dump(out, open(f"{d}/ipc-{w}.json", "w"), indent=1)
    print(f"sample span {samp:.1f}s, fills in sampled span (rate x span) {out['fills_sampled']:.0f}")
    k = 1000 / out["fills_sampled"]
    print(
        f"== {d} {w}: fills {fills:.0f} span {t1 - t0:.1f}s {fills / (t1 - t0):.0f}/s utime {out['utime_s']:.1f}s "
        f"GHz(user) {grp['process'].get('cycles', 0) / max(out['utime_s'], 1e-9) / 1e9:.2f}"
    )
    evs = sorted({e for v in grp.values() for e in v})
    print(
        "group/func".ljust(44)
        + "".join(e[:14].rjust(16) for e in evs)
        + "     IPC   (per 1k fills)"
    )

    def row(lbl, e):
        c = e.get("cycles", 0)
        i = e.get("instructions", 0)
        print(
            lbl[:44].ljust(44)
            + "".join(f"{e.get(x, 0) * k:16.0f}" for x in evs)
            + f"  {i / c if c else 0:6.2f}"
        )

    for g in ["process", "exec", "flush", "rpc"]:
        row(g, grp.get(g, {}))
    for g in ["process"]:
        for n, _ in FUNCS:
            row("incl " + n, inc[g].get(n, {}))
        for n, _ in FUNCS:
            row("self " + n, slf[g].get(n, {}))


if __name__ == "__main__":
    d = sys.argv[1]
    for w in sys.argv[2:] or ["w1", "w2"]:
        run(d, w)
