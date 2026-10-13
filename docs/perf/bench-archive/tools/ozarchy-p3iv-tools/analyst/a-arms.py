"""Arm tables from a-cells.py output (python3 -I). Usage: a-arms.py CELLS.json BASE:OTHER [BASE:OTHER ...] [--io PREFIX]
Per arm means (sd) and OTHER vs BASE with Welch 95% CI; ANCOVA CPU at equal fills (common slope over the 2 arms)."""
import json
import math
import statistics as st
import sys

V = ("val0", "val1", "val2")
T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365, 8: 2.306, 9: 2.262, 10: 2.228}


def tq(df):
    return T975.get(int(round(df)), 2.2 if df > 10 else 12.706)


def io(prefix, tag, x):
    t0, t1 = x["t0"], x["t1"]
    rows = {}
    for line in open(prefix + tag + "/io-ticks.txt"):
        p = line.split()
        if len(p) >= 6:
            rows.setdefault(p[1], []).append((float(p[0]), int(p[3]), int(p[5])))
    wb = 0
    for rs in rows.values():
        ins = [r for r in rs if t0 <= r[0] <= t1]
        if len(ins) >= 2:
            wb += ins[-1][2] - ins[0][2]
    return wb / 3 / (t1 - t0) / 1e6


def m3(x, f):
    return st.mean(f(x["blocks"][v]) for v in V)


def mm(x, k):
    if not x["mem"]:
        return float("nan")
    return st.mean(x["mem"][v][k] for v in V if v in x["mem"])


def row(x, prefix):
    ph = x["phases"]
    r = dict(
        ms=x["ms"], submit=x["submit"], blk=x["blk"], chain=x["chain_ms"], apb=x["apb"], fpb=x["fills_blk"], eng=x["eng_1k"],
        ci50=m3(x, lambda b: b["ci_p50"]), ci99=m3(x, lambda b: b["ci_p99"]),
        exec_mean=m3(x, lambda b: b["exec_ms_mean"]), exec_p50=m3(x, lambda b: b["exec_ms_p50"]), exec_p99=m3(x, lambda b: b["exec_ms_p99"]),
        busy=m3(x, lambda b: b["busy"]), lag=m3(x, lambda b: b["lag_mean_s"]), backlog=m3(x, lambda b: b["backlog_blocks_mean"]),
        backlog_max=m3(x, lambda b: b["backlog_blocks_max"]),
        cpu=x["cpu_1m"], rss_pk=x["rss_peak"], rss_mean=x["rss_mean"],
        rss_pk_win=mm(x, "rss_peak_win"), rss_mean_win=mm(x, "rss_mean_win"), rss_slope=mm(x, "rss_slope_gb_min"),
        rss_first15=mm(x, "rss_first15"), rss_last15=mm(x, "rss_last15"), hwm=mm(x, "hwm"),
        anon=mm(x, "anon_mean_win"), file=mm(x, "file_mean_win"), swap=mm(x, "swap_max"), memtable=mm(x, "memtable_mb"),
        bcache=mm(x, "block_cache_mb"), mempool=mm(x, "mempool_mean"), mempool_max=mm(x, "mempool_max"), execq=mm(x, "execq_mean"),
        resident=mm(x, "resident_rows_mb"), layer_mb=mm(x, "layer_mb_per_ckpt"), resting=mm(x, "resting_mean"),
        block_ms=x["block_ms"], resid=ph.get("residual_untimed", float("nan")),
        ck_settle=ph.get("checkpoint_settle_wait", float("nan")), ck_upkeep=ph.get("checkpoint_upkeep", float("nan")),
        ck_wait=x["phase_sub"].get("checkpoint_wait", float("nan")),
        evict=st.mean(x["nonce_evict"] or [0]),
    )
    if prefix:
        r["wr"] = io(prefix, x["tag"], x)
    return r


def welch(a, b):
    mx, my = st.mean(a), st.mean(b)
    vx, vy = st.variance(a) / len(a), st.variance(b) / len(b)
    se = math.sqrt(vx + vy)
    df = (vx + vy) ** 2 / (vx ** 2 / (len(a) - 1) + vy ** 2 / (len(b) - 1)) if vx + vy > 0 else 6
    d = my - mx
    t = tq(df)
    return d, 100 * d / mx, 100 * (d - t * se) / mx, 100 * (d + t * se) / mx


def ancova(A, B):
    sxx = sxy = 0.0
    for xs in (A, B):
        mx = st.mean(f for _, f in xs)
        my = st.mean(c for c, _ in xs)
        for c, f in xs:
            sxx += (f - mx) ** 2
            sxy += (f - mx) * (c - my)
    beta = sxy / sxx
    res = []
    for xs in (A, B):
        mx = st.mean(f for _, f in xs)
        my = st.mean(c for c, _ in xs)
        res += [c - my - beta * (f - mx) for c, f in xs]
    dfr = len(res) - 3
    s2 = sum(e * e for e in res) / dfr
    ma = (st.mean(c for c, _ in A), st.mean(f for _, f in A))
    mb = (st.mean(c for c, _ in B), st.mean(f for _, f in B))
    dx = mb[1] - ma[1]
    ref = ma[0] + beta * dx
    adj = mb[0] - ref
    se = math.sqrt(s2 * (1 / len(A) + 1 / len(B)) + dx ** 2 * s2 / sxx)
    t = tq(dfr)
    return 100 * adj / ref, 100 * (adj - t * se) / ref, 100 * (adj + t * se) / ref, beta


def main():
    args = [a for a in sys.argv[2:] if not a.startswith("--")]
    prefix = sys.argv[sys.argv.index("--io") + 1] if "--io" in sys.argv else None
    if prefix:
        args = [a for a in args if a != prefix]
    C = json.load(open(sys.argv[1]))
    for t, x in C.items():
        x["tag"] = t
    arms = {}
    for t, x in C.items():
        arms.setdefault(x["arm"], []).append(t)
    R = {a: [row(C[t], prefix) for t in ts] for a, ts in arms.items()}
    keys = list(next(iter(R.values()))[0].keys())
    print("cells per arm:", arms)
    print("\nper arm mean (sd):")
    print("metric      " + "".join(f"{a:>22s}" for a in R))
    for k in keys:
        cols = []
        for a in R:
            xs = [r[k] for r in R[a] if r[k] == r[k]]
            cols.append(f"{st.mean(xs):12.3f} ({(st.stdev(xs) if len(xs) > 1 else 0):6.3f})" if xs else f"{'nan':>22s}")
        print(f"{k:12s}" + "".join(cols))
    for pair in args:
        a, b = pair.split(":")
        print(f"\n{b} vs {a}  (delta, % [95% CI])")
        for k in keys:
            xa = [r[k] for r in R[a] if r[k] == r[k]]
            xb = [r[k] for r in R[b] if r[k] == r[k]]
            if len(xa) < 2 or len(xb) < 2:
                continue
            try:
                d, p, lo, hi = welch(xa, xb)
                print(f"  {k:12s} {st.mean(xa):10.3f} -> {st.mean(xb):10.3f}  delta {d:+10.3f}  {p:+7.2f}% [{lo:+7.2f}, {hi:+7.2f}]")
            except (ZeroDivisionError, st.StatisticsError):
                pass
        A = [(C[t]["cpu_node"], C[t]["fills_node"]) for t in arms[a]]
        B = [(C[t]["cpu_node"], C[t]["fills_node"]) for t in arms[b]]
        p, lo, hi, beta = ancova(A, B)
        print(f"  CPU at equal fills (ANCOVA slope {beta:.2f}): {p:+.2f}% [{lo:+.2f}, {hi:+.2f}]")


main()
