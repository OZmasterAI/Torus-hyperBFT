#!/usr/bin/env python3
"""B-blind Gate 2 counter report (ozarchy). Read-only: parses the cells' metrics-before/after dumps.

  python3 -I ozarchy-bblind-gate2-counters.py <S7>              # cells ozarchy-bblind-<S7>-{300m,10m}-*
  python3 -I ozarchy-bblind-gate2-counters.py --prefix P [P..]  # any campaign, e.g. ozarchy-4acdc59-300m

Per cell: survivors only (Prometheus counters are process-lifetime; a val named in node-death.txt is skipped),
delta = after - before for counters/histograms, value = after for gauges, all per 1k placed
(delta torus_orders_placed_accepted_total of the same node), mean over survivors.
Sections: (1) headline + funnel, (2) pair ratios crab/main matched_s_avg, (3) PINNED B-blind counters,
(4) every series whose metric name is absent from main's dumps (= new vs main; flagged if also new vs 4acdc59),
(5) option A check: residual cut sells per 1k placed vs 5.0 (0.5% of placed, section 9.10).
"""

import pathlib
import re
import statistics as st
import sys

R = pathlib.Path("/home/oz/bench-results-matched")
CELLS = ("warm", "crab-r1", "main-r1", "crab-r2", "main-r2")
PLACED = "torus_orders_placed_accepted_total"

# ---- PIN B-blind names here once 18c pushes the commit (metric names, labels kept as series) ----
# PIN: shown in section 3 for every candidate cell. RESIDUAL: summed for the option A check (section 5);
# the default (zero-fill taker margin cancels, the section 18 "cut sells") stays valid if B-blind adds no counter.
# Pinned s92 (2026-10-06) from 31cea69 crates/torus-telemetry/src/lib.rs (MARGIN_CUT_TICK_BUCKETS, exported with _total)
# and run-cell.sh WIDE_COLS. Buckets = ticks above the reservation price.
BUCKETS = ("t0", "t1_2", "t3_5", "t6_10", "t11_30", "t31p")
SELL_CUTS = [
    f"torus_sell_cuts_{pool}_{fill}_{b}_total"
    for pool in ("pool", "nonpool")
    for fill in ("zero", "partial")
    for b in BUCKETS
]
PIN = SELL_CUTS + [
    "torus_sell_top_ups_full_total",
    "torus_sell_top_ups_partial_total",
    "torus_sell_top_ups_none_total",
    "torus_maker_margin_cancels_total",
    "torus_reduce_only_cuts_total",
]
# Option A metric: nonpool ZERO-fill sell cuts, all tick buckets summed (per 1k placed; 5.0 = 0.5%).
RESIDUAL = [f"torus_sell_cuts_nonpool_zero_{b}_total" for b in BUCKETS]
OLD_RESIDUAL = ["torus_orders_rejected_cancelled_total"]  # section 18 "cut sells" proxy, shown for comparison
# Name keywords that put a series in section 3 even before PIN is filled (also catches them if main has the name).
KEYWORDS = re.compile(
    r"cut|top_?up|margin_cancel|reduce_only|blind|reserv|distance|exhaust|sell|tick"
)
THRESHOLD_PER_1K = 5.0  # option A: residual > 0.5% of placed
FALLBACK_MAIN = [
    R / "ozarchy-4acdc59-300m-main-r1" / "metrics-after-val0.txt",
    R / "ozarchy-5524646-10m-main-r1" / "metrics-after-val0.txt",
]
PREV_CRAB = [R / "ozarchy-4acdc59-300m-crab-r1" / "metrics-after-val0.txt"]

LINE = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+(\S+)")


def parse(path):
    vals, types = {}, {}
    try:
        text = path.read_text(errors="replace")
    except OSError:
        return None, None
    for ln in text.splitlines():
        if ln.startswith("# TYPE "):
            p = ln.split()
            if len(p) >= 4:
                types[p[2]] = p[3]
            continue
        if not ln or ln[0] == "#":
            continue
        m = LINE.match(ln)
        if not m:
            continue
        try:
            vals[m.group(1) + (m.group(2) or "")] = float(m.group(3))
        except ValueError:
            pass
    return vals, types


def base(series):
    return series.split("{", 1)[0]


def family(name, types):
    for suf in ("_bucket", "_sum", "_count"):
        if name.endswith(suf) and name[: -len(suf)] in types:
            return name[: -len(suf)]
    return name


def cell_nodes(d):
    """[(i, delta_or_gauge dict, placed)] for surviving nodes of cell dir d."""
    death = (
        (d / "node-death.txt").read_text(errors="replace")
        if (d / "node-death.txt").exists()
        else ""
    )
    out = []
    for i in range(3):
        if f"DEATH node val{i} " in death:
            continue
        after, types = parse(d / f"metrics-after-val{i}.txt")
        if after is None:
            continue
        before, _ = parse(d / f"metrics-before-val{i}.txt")
        before = before or {}
        v = {}
        for s, a in after.items():
            t = types.get(family(base(s), types), "untyped")
            v[s] = a if t == "gauge" else a - before.get(s, 0.0)
        out.append((i, v, v.get(PLACED, 0.0), types))
    return out


def names(paths):
    n = set()
    for p in paths:
        vals, _ = parse(p)
        if vals:
            n |= {base(s) for s in vals}
    return n


def per1k(nodes, series):
    xs = [v.get(series, 0.0) / p * 1000 for _, v, p, _ in nodes if p > 0]
    return st.mean(xs) if xs else float("nan")


def raw(nodes, series):
    xs = [v.get(series, 0.0) for _, v, _, _ in nodes]
    return st.mean(xs) if xs else float("nan")


def headline(d):
    try:
        import json

        return json.loads((d / "summary.json").read_text())["headline"].get(
            "matched_s_avg"
        )
    except Exception:
        return None


def report(prefix):
    print(f"\n######## {prefix}")
    cells = {c: R / f"{prefix}-{c}" for c in CELLS if (R / f"{prefix}-{c}").is_dir()}
    if not cells:
        print("no cells")
        return
    data = {c: cell_nodes(d) for c, d in cells.items()}
    main_files = [
        d / f"metrics-after-val{i}.txt"
        for c, d in cells.items()
        if c.startswith("main")
        for i in range(3)
    ]
    main_names = names([p for p in main_files if p.exists()]) or names(FALLBACK_MAIN)
    prev_names = names(PREV_CRAB)

    print(
        "\n(1) cell | survivors | matched/s | placed (M, mean) | per 1k placed: rejected_cancelled open_limit margin other matched"
    )
    for c, nodes in data.items():
        sv = ",".join(f"val{i}" for i, *_ in nodes)
        pl = st.mean([p for *_, p, _ in nodes]) / 1e6 if nodes else float("nan")
        cols = [
            per1k(nodes, f"torus_orders_{k}_total")
            for k in (
                "rejected_cancelled",
                "rejected_open_limit",
                "rejected_margin",
                "rejected_other",
                "matched",
            )
        ]
        print(
            f"  {c:8} | {sv:14} | {headline(cells[c]) or float('nan'):9.1f} | {pl:6.2f} | "
            + " ".join(f"{x:9.2f}" for x in cols)
        )

    print("\n(2) pair ratio crab/main matched_s_avg")
    rs = []
    for r in ("r1", "r2"):
        a, b = headline(cells.get(f"crab-{r}", R)), headline(cells.get(f"main-{r}", R))
        if a and b:
            rs.append(a / b)
            print(f"  {r}: {a:.1f} / {b:.1f} = {a / b:.3f}x")
    if rs:
        print(f"  mean of pairs: {st.mean(rs):.3f}x")

    crab = [c for c in data if not c.startswith("main")]
    every = sorted({s for c in crab for _, v, _, _ in data[c] for s in v})
    pinned = [
        s
        for s in every
        if base(s) in PIN or base(s) in RESIDUAL or KEYWORDS.search(base(s))
    ]
    new = [s for s in every if base(s) not in main_names]

    def table(title, series):
        print(
            f"\n{title}  (per 1k placed, mean of survivors; raw mean delta in brackets)"
        )
        if not series:
            print("  none")
            return
        print("  series | " + " | ".join(crab))
        for s in series:
            print(
                f"  {s} | "
                + " | ".join(
                    f"{per1k(data[c], s):.3f} [{raw(data[c], s):.0f}]" for c in crab
                )
            )

    table("(3) PINNED / keyword B-blind counters", pinned)
    src = "this campaign" if main_files else "fallback"
    # 4acdc59 is the newest crab dump on this host; ab12c75's own additions (cuts 1/2/5/6) also land in 4a.
    fresh = [s for s in new if base(s) not in prev_names]
    table(
        f"(4a) new vs main AND vs 4acdc59 = B-blind / ab12c75 additions ({len(fresh)}; main names: {src})",
        fresh,
    )
    older = [s for s in new if base(s) in prev_names and "_seconds" not in base(s)]
    table(
        f"(4b) non-timer series new vs main, already in 4acdc59 ({len(older)})", older
    )

    print(
        f"\n(5) option A check: residual = {' + '.join(RESIDUAL)} per 1k placed, threshold {THRESHOLD_PER_1K} (0.5%)"
    )
    meas = [c for c in crab if c != "warm"]
    vals = []
    for c in meas:
        x = sum(per1k(data[c], s) for s in RESIDUAL)
        vals.append(x)
        print(f"  {c}: {x:.3f}")
    if vals:
        m = st.mean(vals)
        print(
            f"  mean {m:.3f} -> {'ABOVE threshold: option A' if m > THRESHOLD_PER_1K else 'below threshold: no option A'}"
        )
    print("\n(5b) sell cuts by group and tick bucket, per 1k placed (mean of measured candidate cells; raw mean delta in brackets)")
    for pool in ("pool", "nonpool"):
        for fill in ("zero", "partial"):
            ser = [f"torus_sell_cuts_{pool}_{fill}_{b}_total" for b in BUCKETS]
            cols = []
            for s_ in ser:
                xs = [per1k(data[c], s_) for c in meas]
                rw = [raw(data[c], s_) for c in meas]
                cols.append(f"{st.mean(xs):.3f} [{st.mean(rw):.0f}]" if xs else "nan")
            tot = st.mean([sum(per1k(data[c], s_) for s_ in ser) for c in meas]) if meas else float("nan")
            print(f"  {pool:7} {fill:7} total {tot:.3f} | " + " | ".join(f"{b} {x}" for b, x in zip(BUCKETS, cols)))
    for s_ in PIN[len(SELL_CUTS):] + OLD_RESIDUAL:
        xs = [per1k(data[c], s_) for c in meas]
        rw = [raw(data[c], s_) for c in meas]
        if xs:
            print(f"  {s_}: {st.mean(xs):.3f} per 1k [{st.mean(rw):.0f}]")


def main(argv):
    if len(argv) >= 2 and argv[0] == "--prefix":
        prefixes = argv[1:]
    elif len(argv) == 1:
        prefixes = [
            f"ozarchy-bblind-{argv[0][:7]}-300m",
            f"ozarchy-bblind-{argv[0][:7]}-10m",
        ]
    else:
        print(__doc__)
        return 2
    for p in prefixes:
        report(p)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
