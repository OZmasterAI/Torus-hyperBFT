#!/usr/bin/env python3
"""Aggregate the devnet fill-ladder legs into a comparison table.

Reports sustained AND peak-over-60s side by side because they answer different
questions: sustained includes bench pre-sign/ramp time inside the window, peak
is the steady-state estimate. A cell is only meaningful with its leg-config
provenance, so branch/taker/markets come from leg-config.json, never the label.
"""

import json, os, glob, statistics, sys

SP = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(SP, "results")

rows = []
for d in sorted(glob.glob(os.path.join(RES, "*/"))):
    s_p, c_p = os.path.join(d, "summary.json"), os.path.join(d, "leg-config.json")
    if not os.path.exists(s_p):
        continue
    label = os.path.basename(d.rstrip("/"))
    if label.startswith("smoke"):
        continue
    s = json.load(open(s_p))
    c = json.load(open(c_p)) if os.path.exists(c_p) else {}
    # Branch identity comes from the IMAGE the leg ran (leg-config.leg_image).
    # The git checkout is NOT reliable — legs run pre-built per-branch images
    # while the working tree may sit on any branch. Guessing from the label is
    # worse: it silently mislabels any leg not named after its branch.
    img = c.get("leg_image")
    if img:
        tag = img.rsplit(":", 1)[-1]
        # Strip only a LEADING "ladder-" — a bare replace() mangles legacy tags
        # such as "p3ladder-p3" into "p3p3".
        branch = tag[len("ladder-") :] if tag.startswith("ladder-") else tag
    else:
        branch = "UNKNOWN"
    peak = s.get("peak_over_60s_window", {})
    rows.append(
        dict(
            label=label,
            branch=branch,
            taker=c.get("taker_ratio"),
            markets=c.get("markets"),
            senders=c.get("senders"),
            sust_m=s["sustained"]["orders_matched_per_s"],
            sust_p=s["sustained"]["orders_placed_per_s"],
            peak_m=peak.get("orders_matched_per_s"),
            blk=s.get("blk_per_s"),
            win=s.get("window_s"),
            conv=s["sustained"].get("matched_per_executed_order_pct"),
        )
    )

print(
    f"{'leg':26s} {'br':9s} {'tk':5s} {'mkt':4s} {'sust_matched/s':>15s} {'peak60_m/s':>11s} {'blk/s':>6s} {'win':>4s}"
)
print("-" * 92)
for r in rows:
    print(
        f"{r['label']:26s} {r['branch']:9s} {str(r['taker']):5s} {str(r['markets']):4s} "
        f"{r['sust_m']:15.1f} {r['peak_m']:11.1f} {r['blk']:6.2f} {r['win']:4d}"
    )

# Head-to-head on cells run more than once.
print("\n=== head-to-head (same config) ===")
cells = {}
for r in rows:
    cells.setdefault((r["taker"], r["markets"]), {}).setdefault(r["branch"], []).append(
        r
    )
for (tk, mkt), by_br in sorted(
    cells.items(), key=lambda x: (str(x[0][0]), str(x[0][1]))
):
    if len(by_br) < 2:
        continue
    print(f"\ntaker={tk} markets={mkt}")
    stats = {}
    for br, rs in sorted(by_br.items()):
        ms = [x["sust_m"] for x in rs]
        mean = statistics.mean(ms)
        sd = statistics.stdev(ms) if len(ms) > 1 else 0.0
        stats[br] = (mean, sd, len(ms), ms)
        spread = (
            f" (n={len(ms)}, sd={sd:.0f}, range {min(ms):.0f}-{max(ms):.0f})"
            if len(ms) > 1
            else " (n=1)"
        )
        print(f"  {br:9s} mean sust_matched/s = {mean:8.1f}{spread}")
    if len(stats) == 2:
        (a, (ma, sda, na, _)), (b, (mb, sdb, nb, _)) = sorted(stats.items())
        hi, lo = (a, b) if ma > mb else (b, a)
        d = abs(ma - mb) / min(ma, mb) * 100
        pooled = max(sda, sdb)
        # With n=1 the stdev is 0, so ANY gap would print as "separated".
        # Refuse to render a verdict rather than manufacture confidence.
        if min(na, nb) < 2:
            verdict = "UNDETERMINED (n=1, no variance estimate)"
        elif abs(ma - mb) < 2 * pooled:
            verdict = "WITHIN NOISE"
        else:
            verdict = "separated"
        print(
            f"  -> {hi} higher by {d:.1f}%  [{verdict}; max sd={pooled:.0f}, gap={abs(ma - mb):.0f}]"
        )
