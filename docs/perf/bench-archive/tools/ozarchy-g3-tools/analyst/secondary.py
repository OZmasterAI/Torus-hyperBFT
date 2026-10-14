#!/usr/bin/env python3
"""Per-arm secondary metrics and excess host load (analyst, read-only on results)."""
import json, csv, statistics as st
B = "/home/oz/bench-results-matched"
cs = json.load(open(B + "/ozarchy-g3-run/analyst/cells.json"))
# excess load = load1 - (sum pcpu of nodes + bench)/100 per sample ts in the bench window
for c in cs:
    rows = list(csv.DictReader(open(f"{B}/ozarchy-g3-300m-{c['label']}/cpu.csv")))
    by = {}
    for r in rows:
        t = int(r["ts"])
        if c["t0"] <= t <= c["t1"]:
            e = by.setdefault(t, [float(r["load1"]), 0.0]); e[1] += float(r["pcpu"]) / 100
    ex = [l - p for l, p in by.values()]
    c["excess_load"] = st.mean(ex); c["own_cores"] = st.mean(p for l, p in by.values())
owner = [c for c in cs if c["label"] != "w0" and c["label"] != "r2-moff"]
print("label arm load1 own_cores excess")
for c in cs: print(c["label"], c["arm"], round(c["load1_mean"], 1), round(c["own_cores"], 1), round(c["excess_load"], 1))
keys = ["cpu_s_per_1M", "cpu_s_per_1M_whole", "node_pcpu", "bt_p50", "bt_p95", "bt_p99", "ci_avg", "wall_ms_per_block", "rss_peak_mb", "rss_mean_mb", "vmhwm_mb", "je_alloc_mb", "je_resident_mb", "native_blk_s", "fills_per_blk", "engine_ms_1k", "submitted", "excess_load"]
print("\nPER ARM (owner set: r2-moff replaced by rerun, GJ reruns included): mean [min,max]")
for a in ("goff", "moff", "mon", "gjoff"):
    g = [c for c in owner if c["arm"] == a]
    print(a, "n=", len(g))
    for k in keys:
        v = [c[k] for c in g if c[k] is not None]
        if v: print(f"   {k:20s} {st.mean(v):10.2f} [{min(v):.2f}, {max(v):.2f}]")
    pl = st.mean(c["mix"]["place"] / (c["mix"]["place"] + c["mix"]["cancel_all"]) for c in g)
    print(f"   place_share          {pl:.3f}   evictions/node mean {st.mean(st.mean(c['evict']) for c in g):.0f}")
print("\nDRIFT matched/s in run order")
for c in sorted(cs, key=lambda c: c["t0"]): print(c["label"], c["arm"], c["matched_s"])
