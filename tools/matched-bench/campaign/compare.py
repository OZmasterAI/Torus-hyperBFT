#!/usr/bin/env python3
"""s63 four-build comparison. Reads every s63-<build>-r<n> cell (accepted or not).

Per cell: gates, headline throughput, steady-window matched/s (t_bench0+10..t_bench1,
node counters from sampler.csv, median over validators), block timing, and RSS from
cpu.csv (peak and steady-window median, max over validators).
Per build: acceptance rate and stats over accepted cells and over all cells.
Usage: compare.py [--json out.json]
"""

import csv, json, pathlib, statistics, sys

R = pathlib.Path("/home/18c/bench-results-matched")
BUILDS = ["base080", "s61d733", "hash388", "ctl9af"]
COMMIT = {
    "base080": "080c4fa",
    "s61d733": "d73320e",
    "hash388": "388f7cd",
    "ctl9af": "9af0eea",
}


def steady_matched(cell, t0, t1):
    rates = []
    with open(cell / "sampler.csv") as f:
        rows = list(csv.DictReader(f))
    for node in sorted({r["node"] for r in rows}):
        nr = [r for r in rows if r["node"] == node and t0 + 10 <= int(r["ts"]) <= t1]
        if len(nr) < 2:
            continue
        a, b = nr[0], nr[-1]
        span = int(b["ts"]) - int(a["ts"])
        if span > 0:
            rates.append(
                (
                    float(b["torus_orders_matched_total"] or 0)
                    - float(a["torus_orders_matched_total"] or 0)
                )
                / span
            )
    return statistics.median(rates) if rates else None


def rss_mib(cell, t0, t1):
    by_pid, steady = {}, {}
    with open(cell / "cpu.csv") as f:
        for r in csv.DictReader(f):
            if r["comm"] != "torus-node":
                continue
            kb = int(r["rss_kb"])
            by_pid[r["pid"]] = max(by_pid.get(r["pid"], 0), kb)
            if t0 + 10 <= int(r["ts"]) <= t1:
                steady.setdefault(r["pid"], []).append(kb)
    peak = max(by_pid.values()) / 1024 if by_pid else None
    med = max(statistics.median(v) for v in steady.values()) / 1024 if steady else None
    return peak, med


def cell_row(label):
    cell = R / label
    row = {"label": label}
    try:
        s = json.loads((cell / "summary.json").read_text())
    except Exception:
        row["status"] = "NO_SUMMARY"
        return row
    h, t = s.get("headline", {}), s.get("timing", {})
    idle = s.get("idle_blk_s")
    row.update(
        accepted=bool(h.get("benchmark_accepted")),
        dissem=h.get("dissemination_clean"),
        agree=h.get("agreement_verdict"),
        liveness=h.get("liveness_verdict"),
        drained=t.get("drained"),
        idle_blk_s=idle,
        idle_ok=idle is not None and 12 <= idle <= 28,
        matched_s_avg=h.get("matched_s_avg"),
        blk_s_avg=h.get("blk_s_avg"),
        wall_ms_per_blk=h.get("wall_ms_per_committed_block"),
        commit_p50=h.get("commit_interval_ms_p50"),
        commit_p95=h.get("commit_interval_ms_p95"),
        engine_ms_per_1k=h.get("engine_ms_per_1k_fills"),
        hs_thread_ms_per_blk=h.get("consensus_thread_ms_per_committed_block_est"),
        timeouts=h.get("consensus_timeouts"),
        sync_fallback=[
            s.get("dissemination", {}).get(v, {}).get("sync_fallback")
            for v in ("val0", "val1", "val2")
        ],
    )
    t0, t1 = t.get("t_bench0"), t.get("t_bench1")
    if t0 and t1:
        row["matched_s_steady"] = steady_matched(cell, t0, t1)
        row["rss_peak_mib"], row["rss_steady_mib"] = rss_mib(cell, t0, t1)
    return row


def stats(vals):
    vals = [v for v in vals if v is not None]
    if not vals:
        return None
    return dict(
        n=len(vals),
        mean=round(statistics.mean(vals), 1),
        min=round(min(vals), 1),
        max=round(max(vals), 1),
        sd=round(statistics.stdev(vals), 1) if len(vals) > 1 else None,
    )


def main():
    rows = [cell_row(p.name) for p in sorted(R.glob("s63-*-r*")) if p.is_dir()]
    out = {"cells": rows, "builds": {}}
    for b in BUILDS:
        cs = [r for r in rows if r["label"].startswith(f"s63-{b}-")]
        acc = [r for r in cs if r.get("accepted")]
        out["builds"][b] = dict(
            commit=COMMIT[b],
            cells=len(cs),
            accepted=len(acc),
            idle_flagged=[
                r["label"] for r in cs if "idle_ok" in r and not r["idle_ok"]
            ],
            **{
                f"{k}_accepted": stats([r.get(k) for r in acc])
                for k in (
                    "matched_s_avg",
                    "matched_s_steady",
                    "blk_s_avg",
                    "wall_ms_per_blk",
                    "engine_ms_per_1k",
                    "rss_peak_mib",
                    "rss_steady_mib",
                )
            },
            matched_s_avg_all=stats([r.get("matched_s_avg") for r in cs]),
        )
    cols = [
        "label",
        "accepted",
        "dissem",
        "agree",
        "liveness",
        "drained",
        "idle_blk_s",
        "matched_s_avg",
        "matched_s_steady",
        "blk_s_avg",
        "wall_ms_per_blk",
        "commit_p95",
        "engine_ms_per_1k",
        "rss_peak_mib",
        "rss_steady_mib",
    ]
    print("\t".join(cols))
    for r in rows:
        print(
            "\t".join(
                str(round(r[c], 1) if isinstance(r.get(c), float) else r.get(c, ""))
                for c in cols
            )
        )
    print()
    print(json.dumps(out["builds"], indent=1))
    if "--json" in sys.argv:
        pathlib.Path(sys.argv[sys.argv.index("--json") + 1]).write_text(
            json.dumps(out, indent=1)
        )


main()
