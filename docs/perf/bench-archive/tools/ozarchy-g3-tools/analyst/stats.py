#!/usr/bin/env python3
"""Ratios with Welch CIs on log matched/s (analyst, read-only on results)."""
import json, math, statistics as st, csv

def _tcdf(x, df, n=4000):
    c = math.exp(math.lgamma((df+1)/2) - math.lgamma(df/2)) / math.sqrt(df*math.pi)
    h = x / n; tot = 0.0
    for i in range(n+1):
        u = i*h; w = 1 if i in (0, n) else (4 if i % 2 else 2)
        tot += w * c * (1 + u*u/df) ** (-(df+1)/2)
    return 0.5 + tot*h/3
def tppf(q, df):
    lo, hi = 0.0, 100.0
    for _ in range(80):
        m = (lo+hi)/2
        if _tcdf(m, df) < q: lo = m
        else: hi = m
    return (lo+hi)/2
class S:
    class t:
        ppf = staticmethod(tppf)
cs = {c["label"]: c for c in json.load(open("/home/oz/bench-results-matched/ozarchy-g3-run/analyst/cells.json"))}
LS = {"r2-goff", "r3-mon", "r3-gjoff", "r3-goff", "r3-moff"}
def arm(a, include, drop_ls=False):
    return [cs[l]["matched_s"] for l in include if cs[l]["arm"] == a and not (drop_ls and l in LS)]
main = [f"r{i}-{a}" for i in range(1, 5) for a in ("goff", "moff", "mon", "gjoff")]
gjr = [f"r{i}-gjoff-rerun" for i in range(1, 5)]
sets = {
 "owner (r2-moff->rerun)": [l for l in main if l != "r2-moff"] + ["r2-moff-rerun"] + gjr,
 "kept (r2-moff + rerun)": main + ["r2-moff-rerun"] + gjr,
 "main only (r2-moff, no rerun, no GJ reruns)": main,
}
def ratio(a, b):
    la, lb = [math.log(x) for x in a], [math.log(x) for x in b]
    d = st.mean(la) - st.mean(lb)
    va, vb = st.variance(la) / len(la), st.variance(lb) / len(lb)
    se = math.sqrt(va + vb)
    df = (va + vb) ** 2 / (va ** 2 / (len(la) - 1) + vb ** 2 / (len(lb) - 1))
    t = S.t.ppf(0.975, df)
    r = st.mean(a) / st.mean(b)
    return r, math.exp(d - t * se), math.exp(d + t * se), df
pairs = [("moff", "goff"), ("mon", "goff"), ("gjoff", "goff"), ("moff", "gjoff"), ("mon", "moff")]
for name, inc in sets.items():
    for drop in (False, True):
        print(f"== {name}{' | without localsearch 5' if drop else ''}")
        for a in ("goff", "moff", "mon", "gjoff"):
            v = arm(a, inc, drop)
            print(f"  {a:6s} n={len(v)} mean={st.mean(v):10.1f} sd={st.stdev(v) if len(v)>1 else 0:8.1f} cv={100*st.stdev(v)/st.mean(v) if len(v)>1 else 0:5.2f}% min={min(v):.1f} max={max(v):.1f}")
        for a, b in pairs:
            r, lo, hi, df = ratio(arm(a, inc, drop), arm(b, inc, drop))
            va, vb = arm(a, inc, drop), arm(b, inc, drop)
            step = st.mean(va) - st.mean(vb)
            psd = math.sqrt(((len(va)-1)*st.variance(va) + (len(vb)-1)*st.variance(vb)) / (len(va)+len(vb)-2))
            print(f"  {a}/{b}: {r:.4f} [95% CI {lo:.4f}, {hi:.4f}] df={df:.1f} step={step:.0f} pooled_sd={psd:.0f} step/sd={step/psd:.1f}")
