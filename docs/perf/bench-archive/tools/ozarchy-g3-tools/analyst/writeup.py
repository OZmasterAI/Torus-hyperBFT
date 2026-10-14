#!/usr/bin/env python3
"""ozarchy-g3 write-up helper (section 65; python3 -I, read-only on the cells). Reads the analyst's cells.json and each
cell's cpu.csv / summary.json; prints the per-cell table and the per-arm values that section 65 adds to stats.py /
secondary.py: RSS in the sections 61-63 convention (kB / 10^6, per-node peak and mean over the bench window, mean of the
3 nodes), jemalloc allocated / resident (bytes / 10^9), nonce-expired evictions, Welch ratios of the secondary metrics
(same method as stats.py) and the extra sensitivities. Usage: writeup.py [CELLS_JSON] (default: the run's cells.json)."""
import csv, datetime as dt, json, math, statistics as st, sys

B = "/home/oz/bench-results-matched"
CJ = sys.argv[1] if len(sys.argv) > 1 else B + "/ozarchy-g3-run/analyst/cells.json"
cs = {c["label"]: c for c in json.load(open(CJ))}


def _tcdf(x, df, n=4000):  # stats.py's t CDF (Simpson), so the CIs match it
    c = math.exp(math.lgamma((df + 1) / 2) - math.lgamma(df / 2)) / math.sqrt(df * math.pi)
    h = x / n; tot = 0.0
    for i in range(n + 1):
        u = i * h; w = 1 if i in (0, n) else (4 if i % 2 else 2)
        tot += w * c * (1 + u * u / df) ** (-(df + 1) / 2)
    return 0.5 + tot * h / 3


def tppf(q, df):
    lo, hi = 0.0, 100.0
    for _ in range(80):
        m = (lo + hi) / 2
        if _tcdf(m, df) < q: lo = m
        else: hi = m
    return (lo + hi) / 2


def welch(a, b):
    la, lb = [math.log(x) for x in a], [math.log(x) for x in b]
    va, vb = st.variance(la) / len(la), st.variance(lb) / len(lb); se = math.sqrt(va + vb)
    df = (va + vb) ** 2 / (va ** 2 / (len(la) - 1) + vb ** 2 / (len(lb) - 1)); t = tppf(0.975, df)
    d = st.mean(la) - st.mean(lb)
    return st.mean(a) / st.mean(b), math.exp(d - t * se), math.exp(d + t * se)


ARMS = ("goff", "moff", "mon", "gjoff")
main = [f"r{i}-{a}" for i in range(1, 5) for a in ARMS]
gjr = [f"r{i}-gjoff-rerun" for i in range(1, 5)]
owner = [l for l in main if l != "r2-moff"] + ["r2-moff-rerun"] + gjr

for l, c in cs.items():
    d = f"{B}/ozarchy-g3-300m-{l}"; rss = {}
    for r in csv.DictReader(open(d + "/cpu.csv")):
        if c["t0"] <= int(r["ts"]) <= c["t1"] and r["comm"] == "torus-node":
            rss.setdefault(r["pid"], []).append(int(r["rss_kb"]) / 1e6)
    c["rss_pk_gb"] = st.mean(max(v) for v in rss.values())
    c["rss_mean_gb"] = st.mean(st.mean(v) for v in rss.values())
    c["evict_m"] = st.mean(c["evict"])
    m = c["mix"]; c["place_share"] = m["place"] / (m["place"] + m["cancel_all"])
    c["je_alloc_gb"] = c["je_alloc_mb"] * 2 ** 20 / 1e9 if c["je_alloc_mb"] else None
    c["je_res_gb"] = c["je_resident_mb"] * 2 ** 20 / 1e9 if c["je_resident_mb"] else None

print("PER CELL (run order; local time of the bench window; load1 = cpu.csv mean in the window; node %CPU = summary mean of 3 nodes)")
print("cell arm start end matched/s cpu_s_per_1M node_pcpu load1 blk_s place_share evict_per_node rss_mean/peak_GB")
for c in sorted(cs.values(), key=lambda c: c["t0"]):
    f = lambda t: dt.datetime.fromtimestamp(t).strftime("%H:%M:%S")
    print(f"{c['label']} {c['arm']} {f(c['t0'])} {f(c['t1'])} {c['matched_s']:.1f} {c['cpu_s_per_1M']:.2f} {c['node_pcpu']:.1f} "
          f"{c['load1_mean']:.1f} {c['native_blk_s']:.2f} {c['place_share']:.3f} {c['evict_m']:.0f} {c['rss_mean_gb']:.2f}/{c['rss_pk_gb']:.2f}")


def arm(a, k, sel=owner):
    return [cs[l][k] for l in sel if cs[l]["arm"] == a]


print("\nPER ARM, owner set (r2-moff replaced by its rerun, GJ reruns included): mean [min, max]")
for a in ARMS:
    print(a, "n=%d" % len(arm(a, "matched_s")))
    for k in ("rss_mean_gb", "rss_pk_gb", "je_alloc_gb", "je_res_gb", "evict_m", "place_share", "submitted"):
        v = [x for x in arm(a, k) if x is not None]
        if v: print(f"   {k:14s} {st.mean(v):10.3f} [{min(v):.3f}, {max(v):.3f}]")

print("\nWELCH RATIOS of the per-arm means, owner set (log scale, 95% CI, as stats.py)")
for k in ("cpu_s_per_1M", "node_pcpu", "native_blk_s", "fills_per_blk", "engine_ms_1k", "bt_p50", "bt_p99", "rss_mean_gb", "rss_pk_gb"):
    print(" ", k, "  ".join("%s/%s %.3f [%.3f, %.3f]" % ((a, b) + welch(arm(a, k), arm(b, k)))
                            for a, b in (("moff", "goff"), ("mon", "goff"), ("gjoff", "goff"), ("moff", "gjoff"), ("mon", "moff"))))

print("\nSENSITIVITY (matched/s, owner set)")
sel = [l for l in owner if l != "r1-gjoff-rerun"]
print("  without r1-gjoff-rerun (load1 38.3): gjoff/goff %.3f [%.3f, %.3f]" % welch(arm("gjoff", "matched_s", sel), arm("goff", "matched_s", sel)),
      " moff/gjoff %.3f [%.3f, %.3f]" % welch(arm("moff", "matched_s", sel), arm("gjoff", "matched_s", sel)))
mean4 = st.mean(arm("moff", "matched_s"))
print("  r2-moff vs the owner-set moff mean: %+.2f%%; vs r1-moff %+.2f%%; r3-moff vs r1-moff %+.2f%%" % (
    100 * (cs["r2-moff"]["matched_s"] / mean4 - 1), 100 * (cs["r2-moff"]["matched_s"] / cs["r1-moff"]["matched_s"] - 1),
    100 * (cs["r3-moff"]["matched_s"] / cs["r1-moff"]["matched_s"] - 1)))
