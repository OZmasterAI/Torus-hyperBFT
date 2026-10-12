"""ozarchy-p3s2g arm table: sd (step 2) vs cd (control), commit-tail mode. Reads cells.json (a-p3s2g.py) + cell dirs.
Usage: python3 -I a-arms.py [--drop r3-cd]"""
import json, math, statistics as st, sys, os
R = "/home/oz/bench-results-matched/ozarchy-p3s2g-300m-"
here = os.path.dirname(os.path.abspath(__file__))
C = json.load(open(here + "/cells.json"))
drop = set(sys.argv[sys.argv.index("--drop") + 1].split(",")) if "--drop" in sys.argv else set()
V = ("val0", "val1", "val2")
T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365, 8: 2.306, 9: 2.262, 10: 2.228}
tq = lambda df: T975.get(int(round(df)), 2.2 if df > 10 else 12.706)


def io(tag):
    s = json.load(open(R + tag + "/summary.json"))
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    rows = {}
    for l in open(R + tag + "/io-ticks.txt"):
        p = l.split()
        if len(p) >= 6:
            rows.setdefault(p[1], []).append((float(p[0]), int(p[3]), int(p[5])))
    wb = wc = 0
    for rs in rows.values():
        ins = [r for r in rs if t0 <= r[0] <= t1]
        if len(ins) >= 2:
            wb += ins[-1][2] - ins[0][2]
            wc += ins[-1][1] - ins[0][1]
    return wb / 3 / (t1 - t0) / 1e6, wc / 3 / (t1 - t0) / 1e6, wb / 3 / 1e9


rows = {}
for arm in ("cd", "sd"):
    tags = [t for t in C if t.endswith("-" + arm) and t not in drop]
    r = {k: [] for k in ("ms", "blk", "cpu", "rss_pk", "rss_mean", "submit", "chain", "apb", "fpb", "eng", "ci50", "ci95", "ci99", "exec_mean", "exec_p50", "exec_p99", "busy", "lag", "piped", "wr", "wchar", "wrGB", "cf", "cpu1k", "exec1k", "evict", "rej", "dup")}
    for t in tags:
        x = C[t]
        b = x["blocks"]
        r["ms"].append(x["ms"]); r["blk"].append(x["blk"]); r["cpu"].append(x["cpu_1m"]); r["rss_pk"].append(x["rss_peak"]); r["rss_mean"].append(x["rss_mean"])
        r["submit"].append(x["submit"]); r["chain"].append(x["chain_ms"]); r["apb"].append(x["apb"]); r["fpb"].append(x["fills_blk"]); r["eng"].append(x["eng_1k"])
        for k, kk in (("ci50", "ci_p50"), ("ci95", "ci_p95"), ("ci99", "ci_p99"), ("exec_mean", "exec_ms_mean"), ("exec_p50", "exec_ms_p50"), ("exec_p99", "exec_ms_p99"), ("busy", "busy"), ("lag", "lag_mean_s"), ("piped", "piped_frac")):
            r[k].append(st.mean(b[v][kk] for v in V))
        w = io(t)
        r["wr"].append(w[0]); r["wchar"].append(w[1]); r["wrGB"].append(w[2])
        r["cf"].append((x["cpu_node"], x["fills_node"]))
        r["cpu1k"].append(x["cpu_1m"])  # CPU-s per 1M fills == CPU-ms per 1k fills
        r["exec1k"].append(r["exec_mean"][-1] / (x["fills_blk"] / 1000))
        r["evict"].append(st.mean(x["nonce_evict"])); r["rej"].append(st.mean(x["action_rej"]))
    rows[arm] = (tags, r)


def welch(x, y):
    mx, my = st.mean(x), st.mean(y)
    vx, vy = st.variance(x) / len(x), st.variance(y) / len(y)
    se = math.sqrt(vx + vy)
    df = (vx + vy) ** 2 / (vx ** 2 / (len(x) - 1) + vy ** 2 / (len(y) - 1)) if vx + vy > 0 else 6
    d = my - mx
    t = tq(df)
    return 100 * d / mx, 100 * (d - t * se) / mx, 100 * (d + t * se) / mx


print("cells:", {a: rows[a][0] for a in rows}, "dropped:", sorted(drop))
keys = ["ms", "submit", "blk", "chain", "apb", "fpb", "eng", "ci50", "ci95", "ci99", "exec_mean", "exec_p50", "exec_p99", "exec1k", "busy", "lag", "piped", "cpu", "wr", "wchar", "wrGB", "rss_pk", "rss_mean", "evict", "rej"]
print(f"{'metric':10s} {'cd mean (sd)':>20s} {'sd mean (sd)':>20s}   sd vs cd [95% CI]")
for k in keys:
    a, b = rows["cd"][1][k], rows["sd"][1][k]
    try:
        p, lo, hi = welch(a, b)
        ci = f"{p:+.2f}% [{lo:+.2f},{hi:+.2f}]"
    except (ZeroDivisionError, st.StatisticsError):
        ci = "n/a"
    print(f"{k:10s} {st.mean(a):12.3f} ({st.stdev(a):7.3f}) {st.mean(b):12.3f} ({st.stdev(b):7.3f})   {ci}")
# equal fills ANCOVA, common slope over the 2 arms
sxx = sxy = 0
for arm in rows:
    xs = rows[arm][1]["cf"]
    mx = st.mean(f for _, f in xs); my = st.mean(c for c, _ in xs)
    for c, f in xs:
        sxx += (f - mx) ** 2; sxy += (f - mx) * (c - my)
beta = sxy / sxx
res = []
for arm in rows:
    xs = rows[arm][1]["cf"]
    mx = st.mean(f for _, f in xs); my = st.mean(c for c, _ in xs)
    res += [c - my - beta * (f - mx) for c, f in xs]
dfr = len(res) - 3
s2 = sum(e * e for e in res) / dfr
b = rows["cd"][1]["cf"]; x = rows["sd"][1]["cf"]
mb = (st.mean(c for c, _ in b), st.mean(f for _, f in b)); mxx = (st.mean(c for c, _ in x), st.mean(f for _, f in x))
dx = mxx[1] - mb[1]; adj = mxx[0] - (mb[0] + beta * dx); ref = mb[0] + beta * dx
se = math.sqrt(s2 * (1 / len(b) + 1 / len(x)) + dx ** 2 * s2 / sxx)
t = tq(dfr)
print(f"\nANCOVA slope {beta:.2f} CPU-s per 1M fills/node (df {dfr}); cd CPU-s/node {mb[0]:.1f} fills {mb[1]:.3f}M; sd {mxx[0]:.1f} fills {mxx[1]:.3f}M")
print(f"sd vs cd at equal fills: {adj:+.1f} CPU-s/node = {100 * adj / ref:+.2f}% [{100 * (adj - t * se) / ref:+.2f},{100 * (adj + t * se) / ref:+.2f}]")
# checkpoints (sd)
print("\ncheckpoint metrics per sd cell (mean over nodes):")
for tg in rows["sd"][0]:
    k = C[tg]["ckpt"]
    m = lambda n: st.mean(k[n])
    n = m("torus_exec_checkpoints_total")
    print(f"  {tg}: checkpoints {n:.1f}, layer_shared {max(k['torus_exec_checkpoint_layer_shared_total']):.0f}, wait {1000 * m('torus_exec_checkpoint_wait_seconds_sum') / n:.2f} ms/ckpt, "
          f"layer mean {m('torus_exec_checkpoint_layer_bytes_sum') / m('torus_exec_checkpoint_layer_bytes_count') / 1e6:.1f} MB, flush {m('torus_flush_worker_checkpoint_seconds_sum') / n:.3f} s/ckpt")
