"""ozarchy-p3soak per-node time series (python3 -I a-soak.py PREFIX TAG ...).
Bench window t0..t1 from summary.json timing. Per node (mem-samples.txt, VmRSS, 5 s): RSS at t0+300 / t0+600 (nearest sample),
peak, OLS slope over the last 300 s of the bench window (MB/min, 95% CI with an AR(1) effective-n correction), and over the
full window. Chain (sampler.csv, val0 torus_orders_matched_total, 1 s): matched/s over the last 300 s and per minute;
drift = OLS slope of the per-minute matched/s (95% CI). CPU (cpu-ticks.txt, utime+stime at 100 Hz, 3 node pids):
CPU-s per 1M matched over the last 300 s. Load generator: summary ingest (tail errors / missed, releases, nonce expiry)."""
import json
import math
import statistics as st
import sys

V = ("val0", "val1", "val2")
T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365, 8: 2.306, 9: 2.262, 10: 2.228, 15: 2.131, 20: 2.086, 30: 2.042}


def tq(df):
    if df <= 0:
        return float("inf")
    ks = sorted(T975)
    for k in ks:
        if df <= k:
            return T975[k]
    return 1.96 if df > 120 else 2.0


def ols(xs, ys):
    n = len(xs)
    mx, my = st.mean(xs), st.mean(ys)
    sxx = sum((x - mx) ** 2 for x in xs)
    b = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    res = [y - my - b * (x - mx) for x, y in zip(xs, ys)]
    s2 = sum(r * r for r in res) / (n - 2)
    # AR(1) correction of the residuals: n_eff = n (1 - r1) / (1 + r1)
    r1 = sum(a * c for a, c in zip(res, res[1:])) / sum(r * r for r in res) if sum(r * r for r in res) > 0 else 0
    r1 = max(0.0, min(r1, 0.99))
    neff = max(3.0, n * (1 - r1) / (1 + r1))
    se = math.sqrt(s2 / sxx) * math.sqrt(n / neff)
    t = tq(neff - 2)
    return b, b - t * se, b + t * se, r1, neff


def mem(cell):
    m = {v: [] for v in V}
    for line in open(cell + "/mem-samples.txt"):
        p = line.split()
        if len(p) > 3 and p[0] == "M" and p[2] in m:
            kv = dict(x.split(":=") for x in p[4:] if ":=" in x)
            if "VmRSS" in kv:
                m[p[2]].append((float(p[1]), int(kv["VmRSS"]) / 1024))
    return m


def at(series, t):
    return min(series, key=lambda s: abs(s[0] - t))


def main():
    P = sys.argv[1]
    out = {}
    for tag in sys.argv[2:]:
        c = P + tag
        s = json.load(open(c + "/summary.json"))
        t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
        r = {"window_s": t1 - t0}
        M = mem(c)
        for v in V:
            w = [x for x in M[v] if t0 <= x[0] <= t1]
            last = [x for x in w if x[0] >= t1 - 300]
            b, lo, hi, r1, neff = ols([x[0] / 60 for x in last], [x[1] for x in last])
            bf, lof, hif, _, _ = ols([x[0] / 60 for x in w], [x[1] for x in w])
            r[v] = dict(rss5=at(w, t0 + 300)[1], rss10=at(w, t0 + 600)[1], peak=max(x[1] for x in w), end=w[-1][1],
                        slope_last5=b, ci=(lo, hi), r1=r1, neff=neff, slope_full=bf, ci_full=(lof, hif))
        mt = []
        for line in open(c + "/sampler.csv"):
            p = line.split(",")
            if len(p) > 6 and p[1] == "val0" and p[0].isdigit():
                mt.append((int(p[0]), float(p[6])))
        mt = [x for x in mt if t0 <= x[0] <= t1]
        a = min(mt, key=lambda x: abs(x[0] - (t1 - 300)))
        z = mt[-1]
        r["ms_last5"] = (z[1] - a[1]) / (z[0] - a[0])
        r["ms_whole"] = (z[1] - mt[0][1]) / (z[0] - mt[0][0])
        per = []
        k = 0
        while t0 + 60 * (k + 1) <= t1:
            x0 = min(mt, key=lambda x: abs(x[0] - (t0 + 60 * k)))
            x1 = min(mt, key=lambda x: abs(x[0] - (t0 + 60 * (k + 1))))
            per.append((x1[1] - x0[1]) / (x1[0] - x0[0]))
            k += 1
        r["ms_per_min"] = per
        r["drift"] = ols(list(range(len(per))), per)[:3] if len(per) >= 4 else (float("nan"),) * 3
        pids = {}
        for line in open(c + "/cpu-ticks.txt"):
            p = line.split()
            if len(p) == 4:
                pids.setdefault(p[1], []).append((int(p[0]), int(p[2]) + int(p[3])))
        cs = 0.0
        for pid, xs in pids.items():
            xa = min(xs, key=lambda x: abs(x[0] - a[0]))
            xz = min(xs, key=lambda x: abs(x[0] - z[0]))
            cs += (xz[1] - xa[1]) / 100
        r["cpu_1m_last5"] = cs / 3 / ((z[1] - a[1]) / 1e6)
        ing = s["ingest"]
        r["ingest"] = dict(submit=ing.get("bench_submit_rate"), in_flight=ing.get("in_flight"), evict=ing.get("mempool_nonce_expired_evictions_per_node"),
                           submitted=ing.get("bench_submitted_actions"), processed=ing.get("val0_actions_processed"))
        r["valid"] = s["validity"]["verdict"]
        r["agree"] = s["headline"].get("agreement_verdict")
        out[tag] = r
        print(f"== {tag} window {t1 - t0} s valid={r['valid']} agree={r['agree']}")
        for v in V:
            x = r[v]
            print(f"  {v} RSS MB @5min {x['rss5']:.0f} @10min {x['rss10']:.0f} peak {x['peak']:.0f} end {x['end']:.0f} | slope last 5 min {x['slope_last5']:+.1f} MB/min "
                  f"[{x['ci'][0]:+.1f}, {x['ci'][1]:+.1f}] (r1 {x['r1']:.2f}, n_eff {x['neff']:.0f}) | whole window {x['slope_full']:+.1f} [{x['ci_full'][0]:+.1f}, {x['ci_full'][1]:+.1f}]")
        print(f"  matched/s last 5 min {r['ms_last5']:.0f}, whole {r['ms_whole']:.0f}; per minute {[round(p) for p in per]}; drift {r['drift'][0]:+.0f} /min [{r['drift'][1]:+.0f}, {r['drift'][2]:+.0f}]")
        print(f"  node CPU-s per 1M matched, last 5 min: {r['cpu_1m_last5']:.2f}")
        print(f"  ingest: {json.dumps(r['ingest'])}")
    json.dump(out, open("soak.json", "w"), indent=1)


main()
