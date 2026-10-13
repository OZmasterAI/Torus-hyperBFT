"""ozarchy-p3soakje: per-node time series for the two Phase 3 soaks (glibc ozarchy-p3soak, jemalloc ozarchy-p3soakje).

python3 -I a-series.py OUTDIR  (reads the 4 soak cells under /home/oz/bench-results-matched)

Outputs in OUTDIR:
  series.json            per cell: per-minute table (generator vs node) and per-node RSS / layer summaries
  series.txt             the same as text
  fig-rss.svg            RSS per node over time, ctl vs s2, glibc and je
  fig-layer-<soak>.svg   per node: RSS next to the Phase 3 layer / checkpoint metrics and the data gauges
  fig-load.svg           generator vs node per minute (submit/s, processed actions/s, matched/s, expiry, queue, CPU)
Sources per cell: mem-samples.txt (M: VmRSS / RssAnon 5 s; G: gauges incl. exec_checkpoint_layer_bytes_sum/count,
exec_checkpoints_total, exec_resident_rows_bytes, exec_resting_orders, exec_cancel_all_index_entries, mempool, exec queue),
ckpt-samples.txt (2 s: settle wait / upkeep / checkpoint wait / flush-worker checkpoint histograms sum+count, layer
shared, drop inline, cancel-all counters), sampler.csv (1 s val0 counters), bench.log (cumulative submitted per second),
cpu-ticks.txt (node utime+stime 1 Hz), cpu.csv (bench-throughput ps pcpu, lifetime average, 10 s -> interval CPU),
val0.log.gz ("evicted nonce-expired ... evicted=N")."""

import datetime as dt
import gzip
import json
import math
import re
import sys

R = "/home/oz/bench-results-matched"
CELLS = [
    ("glibc", "ctl", "ozarchy-p3soak-300m-k1-ctl"),
    ("glibc", "s2", "ozarchy-p3soak-300m-k1-s2"),
    ("je", "ctl", "ozarchy-p3soakje-300m-k1-ctl"),
    ("je", "s2", "ozarchy-p3soakje-300m-k1-s2"),
]
V = ("val0", "val1", "val2")
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def load(cell):
    c = f"{R}/{cell}"
    s = json.load(open(c + "/summary.json"))
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    d = {
        "t0": t0,
        "t1": t1,
        "summary": s,
        "rss": {v: [] for v in V},
        "anon": {v: [] for v in V},
        "g": {v: [] for v in V},
        "pid": {},
        "ck": {v: [] for v in V},
    }
    for line in open(c + "/mem-samples.txt"):
        p = line.split()
        if len(p) > 3 and p[0] == "M" and p[2] in d["rss"]:
            kv = dict(x.split(":=") for x in p[4:] if ":=" in x)
            if "VmRSS" in kv:
                d["rss"][p[2]].append((float(p[1]) - t0, int(kv["VmRSS"]) / 1024))
                d["anon"][p[2]].append(
                    (float(p[1]) - t0, int(kv.get("RssAnon", 0)) / 1024)
                )
            d["pid"][p[3]] = p[2]
        elif len(p) > 3 and p[0] == "G" and p[2] in d["g"]:
            kv = {}
            for x in p[3:]:
                if "=" in x:
                    k, val = x.split("=", 1)
                    try:
                        kv[k] = float(val)
                    except ValueError:
                        pass
            d["g"][p[2]].append((float(p[1]) - t0, kv))
    cur = None
    for line in open(c + "/ckpt-samples.txt"):
        if line.startswith("T "):
            p = line.split()
            cur = None
            if p[3] == "ok":
                cur = {"t": float(p[1]) - t0}
                d["ck"][p[2]].append(cur)
            continue
        if cur is None or "_bucket" in line:
            continue
        p = line.split()
        if len(p) == 2 and "{" not in p[0]:
            try:
                cur[p[0]] = float(p[1])
            except ValueError:
                pass
    # sampler.csv (val0..2, 1 s)
    hdr = None
    d["smp"] = {v: [] for v in V}
    for line in open(c + "/sampler.csv"):
        p = line.rstrip("\n").split(",")
        if hdr is None:
            hdr = p
            continue
        if len(p) == len(hdr) and p[1] in d["smp"]:
            row = {}
            for k, x in zip(hdr[2:], p[2:]):
                try:
                    row[k] = float(x)
                except ValueError:
                    pass
            d["smp"][p[1]].append((int(p[0]) - t0, row))
    # bench.log cumulative submitted (bench seconds since start ~= t - t0)
    sub = {}
    for line in open(c + "/bench.log"):
        m = re.match(r"\[(\d+)s\] submitted: ([\d,]+) actions", line)
        if m:
            sub[int(m.group(1))] = int(m.group(2).replace(",", ""))
    d["sub"] = sorted(sub.items())
    # node cpu ticks
    d["cpu"] = {v: [] for v in V}
    for line in open(c + "/cpu-ticks.txt"):
        p = line.split()
        if len(p) == 4 and p[1] in d["pid"]:
            d["cpu"][d["pid"][p[1]]].append(
                (int(p[0]) - t0, (int(p[2]) + int(p[3])) / 100)
            )
    # bench cpu (ps pcpu = lifetime average; bench starts at ~t0): cumulative CPU-s = pcpu/100 * (t - t0)
    d["bcpu"] = []
    for line in open(c + "/cpu.csv"):
        p = line.split(",")
        if len(p) == 8 and p[5].startswith("bench-through"):
            t = int(p[0]) - t0
            if t > 0:
                d["bcpu"].append((t, float(p[6]) / 100 * t))
    # nonce expiry from val0 log
    d["exp"] = []
    with gzip.open(c + "/val0.log.gz", "rt", errors="replace") as f:
        for line in f:
            if "nonce-expired" not in line:
                continue
            line = ANSI.sub("", line)
            m = re.match(r"(\S+Z) .*evicted=(\d+)", line)
            if m:
                ts = dt.datetime.fromisoformat(
                    m.group(1).replace("Z", "+00:00")
                ).timestamp()
                d["exp"].append((ts - t0, int(m.group(2))))
    return d


def interp(series, t):
    """value of a cumulative series [(t, v)] at t (linear)."""
    if not series:
        return float("nan")
    if t <= series[0][0]:
        return series[0][1]
    for (a, x), (b, y) in zip(series, series[1:]):
        if a <= t <= b:
            return x if b == a else x + (y - x) * (t - a) / (b - a)
    return series[-1][1]


def mean_in(series, a, b):
    xs = [v for t, v in series if a <= t < b]
    return sum(xs) / len(xs) if xs else float("nan")


def ols(xs, ys):
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    b = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    res = [y - my - b * (x - mx) for x, y in zip(xs, ys)]
    ss = sum(r * r for r in res)
    s2 = ss / (n - 2)
    r1 = sum(a * c for a, c in zip(res, res[1:])) / ss if ss > 0 else 0
    r1 = max(0.0, min(r1, 0.99))
    neff = max(3.0, n * (1 - r1) / (1 + r1))
    se = math.sqrt(s2 / sxx) * math.sqrt(n / neff)
    T = {
        1: 12.706,
        2: 4.303,
        3: 3.182,
        4: 2.776,
        5: 2.571,
        6: 2.447,
        7: 2.365,
        8: 2.306,
        9: 2.262,
        10: 2.228,
        15: 2.131,
        20: 2.086,
        30: 2.042,
    }
    df = neff - 2
    t = (
        next((T[k] for k in sorted(T) if df <= k), 1.96 if df > 120 else 2.0)
        if df > 0
        else float("inf")
    )
    return b, b - t * se, b + t * se, neff


def analyse(d):
    W = d["t1"] - d["t0"]
    out = {"window_s": W, "nodes": {}, "minutes": []}
    for v in V:
        r = [(t, x) for t, x in d["rss"][v] if 0 <= t <= W]
        g = [(t, kv) for t, kv in d["g"][v] if 0 <= t <= W]
        nd = {}
        for name, a, b in (
            ("early", 0, 300),
            ("late", W - 300, W),
            ("m2_5", 120, 300),
            ("m5_8", 300, 480),
            ("m8_10", 480, W),
        ):
            w = [(t / 60, x) for t, x in r if a <= t <= b]
            nd["slope_" + name] = ols([x[0] for x in w], [x[1] for x in w])
        nd["rss_start"] = r[0][1]
        nd["rss_1min"] = min(r, key=lambda x: abs(x[0] - 60))[1]
        nd["rss_5min"] = min(r, key=lambda x: abs(x[0] - 300))[1]
        nd["rss_end"] = r[-1][1]
        nd["rss_mean_2_5"] = mean_in(r, 120, 300)
        nd["rss_mean_5_10"] = mean_in(r, 300, W + 1)
        nd["rss_mean_8_10"] = mean_in(r, 480, W + 1)
        nd["rss_peak"] = max(x for _, x in r)
        nd["delta_1_10"] = nd["rss_end"] - nd["rss_1min"]
        nd["delta_5_10"] = nd["rss_end"] - nd["rss_5min"]
        # gauges at 5 min and end (MB / count)
        for k in (
            "torus_exec_resident_rows_bytes",
            "torus_exec_resting_orders",
            "torus_exec_cancel_all_index_entries",
            "torus_rocksdb_memtable_bytes_all",
            "torus_rocksdb_block_cache_bytes",
            "torus_mempool_native_size",
            "torus_exec_queue_depth",
        ):
            s = [(t, kv.get(k, float("nan"))) for t, kv in g]
            nd[k] = (
                min(s, key=lambda x: abs(x[0] - 60))[1],
                min(s, key=lambda x: abs(x[0] - 300))[1],
                s[-1][1],
            )
        ck = [c for c in d["ck"][v] if 0 <= c["t"] <= W]
        if ck and "torus_exec_checkpoints_total" in ck[-1]:
            nd["checkpoints"] = ck[-1]["torus_exec_checkpoints_total"] - ck[0].get(
                "torus_exec_checkpoints_total", 0
            )
            lb = ck[-1].get("torus_exec_checkpoint_layer_bytes_sum", 0) - ck[0].get(
                "torus_exec_checkpoint_layer_bytes_sum", 0
            )
            lc = ck[-1].get("torus_exec_checkpoint_layer_bytes_count", 0) - ck[0].get(
                "torus_exec_checkpoint_layer_bytes_count", 0
            )
            nd["layer_mb_mean"] = lb / lc / 1e6 if lc else float("nan")
            # mean layer MB in the first and the last 5 min
            for name, a, b in (("early", 0, 300), ("late", W - 300, W)):
                ca = min(ck, key=lambda c: abs(c["t"] - a))
                cb = min(ck, key=lambda c: abs(c["t"] - b))
                n = cb.get("torus_exec_checkpoint_layer_bytes_count", 0) - ca.get(
                    "torus_exec_checkpoint_layer_bytes_count", 0
                )
                nd["layer_mb_" + name] = (
                    (
                        cb.get("torus_exec_checkpoint_layer_bytes_sum", 0)
                        - ca.get("torus_exec_checkpoint_layer_bytes_sum", 0)
                    )
                    / n
                    / 1e6
                    if n
                    else float("nan")
                )
                sw = cb.get(
                    "torus_exec_checkpoint_settle_wait_seconds_sum", 0
                ) - ca.get("torus_exec_checkpoint_settle_wait_seconds_sum", 0)
                sc = cb.get(
                    "torus_exec_checkpoint_settle_wait_seconds_count", 0
                ) - ca.get("torus_exec_checkpoint_settle_wait_seconds_count", 0)
                nd["settle_ms_per_block_" + name] = (
                    sw / sc * 1000 if sc else float("nan")
                )
                fw = cb.get("torus_flush_worker_checkpoint_seconds_sum", 0) - ca.get(
                    "torus_flush_worker_checkpoint_seconds_sum", 0
                )
                fc = cb.get("torus_flush_worker_checkpoint_seconds_count", 0) - ca.get(
                    "torus_flush_worker_checkpoint_seconds_count", 0
                )
                nd["flush_ckpt_s_" + name] = fw / fc if fc else float("nan")
            nd["layer_shared"] = ck[-1].get(
                "torus_exec_checkpoint_layer_shared_total", float("nan")
            )
            nd["drop_inline"] = ck[-1].get(
                "torus_exec_checkpoint_drop_inline_total", float("nan")
            )
        out["nodes"][v] = nd
    # per minute, generator vs node (val0 chain counters; CPU per node and bench)
    s0 = d["smp"]["val0"]

    def cum(k):
        return [(t, r.get(k, float("nan"))) for t, r in s0]

    act, plc, mat, blk = (
        cum("torus_native_actions_processed_total"),
        cum("torus_orders_placed_accepted_total"),
        cum("torus_orders_matched_total"),
        cum("torus_exec_native_blocks_total"),
    )
    exb, chs = cum("torus_exec_block_seconds_sum"), cum("torus_exec_chain_seconds_sum")
    mp, eq = cum("torus_mempool_native_size"), cum("torus_exec_queue_depth")
    rest = cum("torus_exec_resting_orders")
    ca = cum("torus_exec_cancel_all_total")
    rjm = cum("torus_orders_rejected_margin_total")
    sub = [(t, x) for t, x in d["sub"]]
    for k in range(int(W // 60)):
        a, b = 60 * k, 60 * (k + 1)
        row = {"min": k + 1}
        row["submit_s"] = (interp(sub, b) - interp(sub, a)) / 60
        row["processed_s"] = (interp(act, b) - interp(act, a)) / 60
        row["placed_s"] = (interp(plc, b) - interp(plc, a)) / 60
        row["rej_margin_s"] = (interp(rjm, b) - interp(rjm, a)) / 60
        row["matched_s"] = (interp(mat, b) - interp(mat, a)) / 60
        row["native_blk_s"] = (interp(blk, b) - interp(blk, a)) / 60
        row["matched_per_action"] = (
            row["matched_s"] / row["processed_s"]
            if row["processed_s"]
            else float("nan")
        )
        row["exec_busy"] = (interp(exb, b) - interp(exb, a)) / 60
        row["chain_busy"] = (interp(chs, b) - interp(chs, a)) / 60
        row["mempool"] = mean_in(mp, a, b)
        row["exec_queue"] = mean_in(eq, a, b)
        row["resting"] = mean_in(rest, a, b)
        row["cancel_all_s"] = (interp(ca, b) - interp(ca, a)) / 60
        row["expired"] = sum(n for t, n in d["exp"] if a <= t < b)
        row["node_cores"] = [
            (interp(d["cpu"][v], b) - interp(d["cpu"][v], a)) / 60 for v in V
        ]
        row["bench_cores"] = (
            (interp(d["bcpu"], b) - interp(d["bcpu"], a)) / 60
            if d["bcpu"]
            else float("nan")
        )
        out["minutes"].append(row)
    # per node per minute (18c list): RSS at minute end, resting orders (gauge, end), flush_worker_depth (mean),
    # exec_handoff_wait ms per block, layer MB per checkpoint and checkpoints in the minute, drop_inline delta, matched/s
    out["node_minutes"] = {}
    for v in V:
        sv = d["smp"][v]

        def cv(k, sv=sv):
            return [(t, r.get(k, float("nan"))) for t, r in sv]

        rows = []
        ck = [c for c in d["ck"][v]]

        def ckv(k, ck=ck):
            return [(c["t"], c[k]) for c in ck if k in c]

        for k in range(int(W // 60)):
            a, b = 60 * k, 60 * (k + 1)
            row = {"min": k + 1}
            row["rss_mb"] = interp([(t, x) for t, x in d["rss"][v]], b)
            row["rss_anon_mb"] = interp([(t, x) for t, x in d["anon"][v]], b)
            row["matched_s"] = (
                interp(cv("torus_orders_matched_total"), b)
                - interp(cv("torus_orders_matched_total"), a)
            ) / 60
            row["resting_m"] = interp(cv("torus_exec_resting_orders"), b) / 1e6
            row["flush_depth"] = mean_in(cv("torus_flush_worker_depth"), a, b)
            hs = interp(cv("torus_exec_handoff_wait_seconds_sum"), b) - interp(
                cv("torus_exec_handoff_wait_seconds_sum"), a
            )
            hc = interp(cv("torus_exec_handoff_wait_seconds_count"), b) - interp(
                cv("torus_exec_handoff_wait_seconds_count"), a
            )
            row["handoff_ms"] = hs / hc * 1000 if hc else float("nan")
            ls_, lc_ = (
                ckv("torus_exec_checkpoint_layer_bytes_sum"),
                ckv("torus_exec_checkpoint_layer_bytes_count"),
            )
            if lc_:
                n = interp(lc_, b) - interp(lc_, a)
                row["ckpts"] = n
                row["layer_mb"] = (
                    (interp(ls_, b) - interp(ls_, a)) / n / 1e6
                    if n > 0.5
                    else float("nan")
                )
                di = ckv("torus_exec_checkpoint_drop_inline_total")
                row["drop_inline"] = (
                    interp(di, b) - interp(di, a) if di else float("nan")
                )
            else:
                row["ckpts"] = row["layer_mb"] = row["drop_inline"] = float("nan")
            rows.append(row)
        out["node_minutes"][v] = rows
    return out


# ------------------------------------------------------------------ SVG plotting (no matplotlib on this host)
COL = {"val0": "#1f77b4", "val1": "#d62728", "val2": "#2ca02c"}


def nice(lo, hi, n=5):
    if not (math.isfinite(lo) and math.isfinite(hi)):
        return 0, 1, [0, 1]
    if hi == lo:
        hi = lo + 1
    span = hi - lo
    step = 10 ** math.floor(math.log10(span / n))
    for m in (1, 2, 2.5, 5, 10):
        if span / (step * m) <= n:
            step *= m
            break
    a = math.floor(lo / step) * step
    b = math.ceil(hi / step) * step
    ticks = []
    x = a
    while x <= b + step * 1e-9:
        ticks.append(round(x, 10))
        x += step
    return a, b, ticks


def fmt(x):
    if abs(x) >= 1e6:
        return f"{x / 1e6:g}M"
    if abs(x) >= 1e4:
        return f"{x / 1e3:g}k"
    return f"{x:g}"


def panel(
    o,
    x0,
    y0,
    w,
    h,
    series,
    title,
    ylab,
    xmax,
    ymin0=True,
    xlab="minutes since bench start",
):
    ys = [y for s in series for _, y in s["pts"] if math.isfinite(y)]
    lo = min(ys) if ys else 0
    hi = max(ys) if ys else 1
    if ymin0:
        lo = min(0, lo)
    lo, hi, ticks = nice(lo, hi)
    L, B = 62, 34
    pw, ph = w - L - 10, h - B - 26
    px, py = x0 + L, y0 + 22
    o.append(
        f'<text x="{x0 + w / 2}" y="{y0 + 14}" font-size="13" text-anchor="middle" font-weight="bold">{title}</text>'
    )
    o.append(
        f'<rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="none" stroke="#444"/>'
    )
    for t in ticks:
        yy = py + ph - (t - lo) / (hi - lo) * ph
        o.append(
            f'<line x1="{px}" y1="{yy:.1f}" x2="{px + pw}" y2="{yy:.1f}" stroke="#ddd"/>'
        )
        o.append(
            f'<text x="{px - 4}" y="{yy + 4:.1f}" font-size="10" text-anchor="end">{fmt(t)}</text>'
        )
    _, xm, xt = nice(0, xmax, 10)
    for t in xt:
        if t > xmax + 1e-9:
            continue
        xx = px + t / xmax * pw
        o.append(
            f'<line x1="{xx:.1f}" y1="{py}" x2="{xx:.1f}" y2="{py + ph}" stroke="#eee"/>'
        )
        o.append(
            f'<text x="{xx:.1f}" y="{py + ph + 13}" font-size="10" text-anchor="middle">{fmt(t)}</text>'
        )
    o.append(
        f'<text x="{px + pw / 2}" y="{py + ph + 27}" font-size="10" text-anchor="middle">{xlab}</text>'
    )
    o.append(
        f'<text x="{x0 + 11}" y="{py + ph / 2}" font-size="10" text-anchor="middle" transform="rotate(-90 {x0 + 11} {py + ph / 2})">{ylab}</text>'
    )
    ly = py + 12
    for s in series:
        pts = [(x, y) for x, y in s["pts"] if math.isfinite(y) and 0 <= x <= xmax]
        if not pts:
            continue
        dash = ' stroke-dasharray="5,3"' if s.get("dash") else ""
        if s.get("step"):
            path = []
            for i, (x, y) in enumerate(pts):
                X = px + x / xmax * pw
                Y = py + ph - (y - lo) / (hi - lo) * ph
                if i:
                    path.append(f"L{X:.1f},{prevY:.1f}")
                path.append(("M" if not i else "L") + f"{X:.1f},{Y:.1f}")
                prevY = Y
            d = " ".join(path)
        else:
            d = " ".join(
                ("M" if i == 0 else "L")
                + f"{px + x / xmax * pw:.1f},{py + ph - (y - lo) / (hi - lo) * ph:.1f}"
                for i, (x, y) in enumerate(pts)
            )
        o.append(
            f'<path d="{d}" fill="none" stroke="{s["color"]}" stroke-width="{s.get("width", 1.4)}"{dash}/>'
        )
        if s.get("label"):
            o.append(
                f'<line x1="{px + 6}" y1="{ly - 4}" x2="{px + 24}" y2="{ly - 4}" stroke="{s["color"]}" stroke-width="2"{dash}/>'
            )
            o.append(f'<text x="{px + 28}" y="{ly}" font-size="10">{s["label"]}</text>')
            ly += 12


def svg(path, W, H, body, title):
    with open(path, "w") as f:
        f.write(
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="DejaVu Sans, sans-serif">\n'
        )
        f.write(f'<rect width="{W}" height="{H}" fill="white"/>\n')
        f.write(
            f'<text x="{W / 2}" y="20" font-size="15" text-anchor="middle" font-weight="bold">{title}</text>\n'
        )
        f.write("\n".join(body))
        f.write("\n</svg>\n")


def rate(ck, ksum, kcnt=None, scale=1.0, win=30.0):
    """per-window rate from cumulative scrapes: (sum delta / count delta) * scale, or sum delta per second when kcnt is None."""
    out = []
    i = 0
    for j in range(len(ck)):
        while ck[j]["t"] - ck[i]["t"] > win:
            i += 1
        if j == i or ksum not in ck[j] or ksum not in ck[i]:
            continue
        ds = ck[j][ksum] - ck[i][ksum]
        dtt = ck[j]["t"] - ck[i]["t"]
        if kcnt:
            dc = ck[j].get(kcnt, 0) - ck[i].get(kcnt, 0)
            if dc > 0:
                out.append((ck[j]["t"] / 60, ds / dc * scale))
        elif dtt > 0:
            out.append((ck[j]["t"] / 60, ds / dtt * scale))
    return out


def main():
    od = sys.argv[1]
    D, A = {}, {}
    for alloc, arm, cell in CELLS:
        D[(alloc, arm)] = load(cell)
        A[(alloc, arm)] = analyse(D[(alloc, arm)])
    json.dump(
        {f"{a}-{b}": v for (a, b), v in A.items()},
        open(od + "/series.json", "w"),
        indent=1,
    )
    with open(od + "/series.txt", "w") as f:
        for (alloc, arm), r in A.items():
            f.write(f"== {alloc} {arm} window {r['window_s']} s\n")
            for v in V:
                n = r["nodes"][v]
                f.write(
                    f"  {v} RSS MB mean 2-5 {n['rss_mean_2_5']:.0f} 5-10 {n['rss_mean_5_10']:.0f} 8-10 {n['rss_mean_8_10']:.0f} | start {n['rss_start']:.0f} @1 {n['rss_1min']:.0f} @5 {n['rss_5min']:.0f} end {n['rss_end']:.0f} peak {n['rss_peak']:.0f} | "
                    f"delta 1->10 {n['delta_1_10']:+.0f} 5->10 {n['delta_5_10']:+.0f} | slope MB/min early(0-5) {n['slope_early'][0]:+.1f} [{n['slope_early'][1]:+.1f}, {n['slope_early'][2]:+.1f}] "
                    f"late(5-10) {n['slope_late'][0]:+.1f} [{n['slope_late'][1]:+.1f}, {n['slope_late'][2]:+.1f}] n_eff {n['slope_late'][3]:.0f} | "
                    f"2-5 {n['slope_m2_5'][0]:+.1f} 5-8 {n['slope_m5_8'][0]:+.1f} 8-10 {n['slope_m8_10'][0]:+.1f}\n"
                )
                g = " ".join(
                    f"{k.replace('torus_', '')}={'/'.join(fmt(round(x)) if math.isfinite(x) else 'nan' for x in n[k])}"
                    for k in n
                    if k.startswith("torus_")
                )
                f.write(f"     gauges @1/@5/end: {g}\n")
                if "checkpoints" in n:
                    f.write(
                        f"     checkpoints {n['checkpoints']:.0f} layer MB mean {n['layer_mb_mean']:.1f} early {n['layer_mb_early']:.1f} late {n['layer_mb_late']:.1f} | "
                        f"settle ms/block early {n['settle_ms_per_block_early']:.1f} late {n['settle_ms_per_block_late']:.1f} | flush-worker ckpt s early {n['flush_ckpt_s_early']:.2f} late {n['flush_ckpt_s_late']:.2f} | "
                        f"layer_shared {n['layer_shared']:.0f} drop_inline {n['drop_inline']:.0f}\n"
                    )
            f.write(
                "  min submit/s processed/s placed/s matched/s matched/action blk/s exec_busy mempool exec_q resting cancel-all/s expired node_cores(v0,v1,v2) bench_cores margin_rejected/s\n"
            )
            for m in r["minutes"]:
                f.write(
                    f"  {m['min']:>3} {m['submit_s']:7.0f} {m['processed_s']:7.0f} {m['placed_s']:9.0f} {m['matched_s']:9.0f} {m['matched_per_action']:6.1f} "
                    f"{m['native_blk_s']:5.2f} {m['exec_busy']:5.2f} {m['mempool']:7.0f} {m['exec_queue']:5.1f} {m['resting']:9.0f} {m['cancel_all_s']:6.0f} {m['expired']:6d} "
                    f"{'/'.join(f'{x:.2f}' for x in m['node_cores'])} {m['bench_cores']:.2f} {m['rej_margin_s']:.0f}\n"
                )
            f.write(
                "  per node per minute: min RSS_MB anon_MB matched/s resting_M flush_depth handoff_ms ckpts layer_MB drop_inline\n"
            )
            for v in V:
                for m in r["node_minutes"][v]:
                    f.write(
                        f"  {v} {m['min']:>2} {m['rss_mb']:6.0f} {m['rss_anon_mb']:6.0f} {m['matched_s']:8.0f} {m['resting_m']:6.2f} {m['flush_depth']:6.1f} "
                        f"{m['handoff_ms']:6.2f} {m['ckpts']:4.0f} {m['layer_mb']:6.1f} {m['drop_inline']:5.0f}\n"
                    )
            # does RSS track resting orders or layer bytes? per node OLS of RSS on resting orders over the minute ends,
            # and Pearson r of the per-minute RSS change with the resting change / layer MB
            for v in V:
                ms = r["node_minutes"][v]
                ms = [m for m in ms if m["min"] >= 3]  # minutes 3-10: past the start-up ramp
                xs = [m["resting_m"] for m in ms]
                ys = [m["rss_mb"] for m in ms]
                dr = [b["rss_mb"] - a["rss_mb"] for a, b in zip(ms, ms[1:])]
                dx = [b["resting_m"] - a["resting_m"] for a, b in zip(ms, ms[1:])]
                lm = [m["layer_mb"] for m in ms[1:]]

                def pear(a, b):
                    pr = [
                        (x, y)
                        for x, y in zip(a, b)
                        if math.isfinite(x) and math.isfinite(y)
                    ]
                    if len(pr) < 3:
                        return float("nan")
                    mx = sum(x for x, _ in pr) / len(pr)
                    my = sum(y for _, y in pr) / len(pr)
                    sxy = sum((x - mx) * (y - my) for x, y in pr)
                    sxx = sum((x - mx) ** 2 for x, _ in pr)
                    syy = sum((y - my) ** 2 for _, y in pr)
                    return (
                        sxy / math.sqrt(sxx * syy)
                        if sxx > 0 and syy > 0
                        else float("nan")
                    )

                f.write(
                    f"  track {v}: r(RSS level, resting level) {pear(xs, ys):+.2f}; r(dRSS, dresting) {pear(dr, dx):+.2f}; r(dRSS, layer MB) {pear(dr, lm):+.2f}; "
                    f"(minutes 3-10) resting min3->min10 {xs[0]:.2f}->{xs[-1]:.2f} M; RSS {ys[0]:.0f}->{ys[-1]:.0f} MB\n"
                )
    # ---- fig-rss: 2 panels (glibc, je), ctl dashed, s2 solid
    body = []
    xmax = 10.2
    for i, alloc in enumerate(("glibc", "je")):
        ser = []
        for arm in ("ctl", "s2"):
            d = D[(alloc, arm)]
            for v in V:
                ser.append(
                    {
                        "pts": [
                            (t / 60, x / 1024)
                            for t, x in d["rss"][v]
                            if -0.2 * 60 <= t <= d["t1"] - d["t0"]
                        ],
                        "color": COL[v],
                        "dash": arm == "ctl",
                        "label": f"{'main' if arm == 'ctl' else 'step 2'} {v}",
                    }
                )
        panel(
            body,
            10 + i * 560,
            30,
            550,
            400,
            ser,
            f"{'glibc' if alloc == 'glibc' else 'jemalloc 5.3.1 (defaults)'}: RSS per node",
            "VmRSS GB",
            xmax,
        )
    svg(
        od + "/fig-rss.svg",
        1130,
        440,
        body,
        "Phase 3 soak, 600 s per arm: main 219f0c20 (dashed) vs step 2 7b006739 @15 s (solid)",
    )
    # ---- fig-layer-<alloc>: rows = metrics, columns = val0..2; s2 solid, ctl dashed where the metric exists
    for alloc in ("glibc", "je"):
        ds, dc = D[(alloc, "s2")], D[(alloc, "ctl")]
        rows = [
            (
                "VmRSS GB",
                lambda d, v: [(t / 60, x / 1024) for t, x in d["rss"][v]],
                True,
            ),
            (
                "flush-worker queue depth (sampler, 1 s)",
                lambda d, v: [(t / 60, r.get("torus_flush_worker_depth", float("nan"))) for t, r in d["smp"][v]],
                True,
            ),
            (
                "exec handoff wait ms per block (30 s window)",
                lambda d, v: rate([dict(r, t=t) for t, r in d["smp"][v]], "torus_exec_handoff_wait_seconds_sum", "torus_exec_handoff_wait_seconds_count", 1e3, 30),
                True,
            ),
            (
                "checkpoint drop_inline per s (60 s window)",
                lambda d, v: rate(d["ck"][v], "torus_exec_checkpoint_drop_inline_total", None, 1, 60),
                False,
            ),
            (
                "resting orders (M)",
                lambda d, v: [
                    (t / 60, kv.get("torus_exec_resting_orders", float("nan")) / 1e6)
                    for t, kv in d["g"][v]
                ],
                True,
            ),
            (
                "cancel-all index entries (k)",
                lambda d, v: [
                    (
                        t / 60,
                        kv.get("torus_exec_cancel_all_index_entries", float("nan"))
                        / 1e3,
                    )
                    for t, kv in d["g"][v]
                ],
                True,
            ),
            (
                "checkpoints (cumulative)",
                lambda d, v: [
                    (c["t"] / 60, c.get("torus_exec_checkpoints_total", float("nan")))
                    for c in d["ck"][v]
                ],
                False,
            ),
            (
                "layer MB per checkpoint (60 s window)",
                lambda d, v: rate(
                    d["ck"][v],
                    "torus_exec_checkpoint_layer_bytes_sum",
                    "torus_exec_checkpoint_layer_bytes_count",
                    1e-6,
                    60,
                ),
                False,
            ),
            (
                "settle wait ms per block (30 s window)",
                lambda d, v: rate(
                    d["ck"][v],
                    "torus_exec_checkpoint_settle_wait_seconds_sum",
                    "torus_exec_checkpoint_settle_wait_seconds_count",
                    1e3,
                    30,
                ),
                False,
            ),
            (
                "flush-worker checkpoint s (60 s window)",
                lambda d, v: rate(
                    d["ck"][v],
                    "torus_flush_worker_checkpoint_seconds_sum",
                    "torus_flush_worker_checkpoint_seconds_count",
                    1,
                    60,
                ),
                False,
            ),
            ("mempool / exec queue (blocks x100)", None, True),
        ]
        body = []
        PW, PH = 400, 190
        for ri, (lab, fn, both) in enumerate(rows):
            for ci, v in enumerate(V):
                ser = []
                if fn is None:
                    for d, arm in ((ds, "s2"), (dc, "ctl")):
                        ser.append(
                            {
                                "pts": [
                                    (
                                        t / 60,
                                        kv.get(
                                            "torus_mempool_native_size", float("nan")
                                        ),
                                    )
                                    for t, kv in d["g"][v]
                                ],
                                "color": "#555",
                                "dash": arm == "ctl",
                                "label": f"mempool {'step 2' if arm == 's2' else 'main'}"
                                if ci == 0
                                else None,
                            }
                        )
                        ser.append(
                            {
                                "pts": [
                                    (
                                        t / 60,
                                        kv.get("torus_exec_queue_depth", float("nan"))
                                        * 100,
                                    )
                                    for t, kv in d["g"][v]
                                ],
                                "color": "#ff7f0e",
                                "dash": arm == "ctl",
                                "label": f"exec queue x100 {'step 2' if arm == 's2' else 'main'}"
                                if ci == 0
                                else None,
                            }
                        )
                else:
                    ser.append(
                        {
                            "pts": fn(ds, v),
                            "color": COL[v],
                            "label": "step 2" if ci == 0 else None,
                        }
                    )
                    if both:
                        ser.append(
                            {
                                "pts": fn(dc, v),
                                "color": "#888",
                                "dash": True,
                                "label": "main" if ci == 0 else None,
                            }
                        )
                panel(
                    body,
                    10 + ci * (PW + 10),
                    30 + ri * (PH + 6),
                    PW,
                    PH,
                    ser,
                    f"{v}: {lab}",
                    "",
                    xmax,
                    ymin0=ri != 0,
                )
        svg(
            od + f"/fig-layer-{alloc}.svg",
            3 * (PW + 10) + 10,
            40 + len(rows) * (PH + 6),
            body,
            f"{'glibc' if alloc == 'glibc' else 'jemalloc'} soak: RSS next to the Phase 3 layer / checkpoint metrics and the data gauges (step 2 solid, main grey dashed)",
        )
    # ---- fig-load: generator vs node per minute, glibc + je, both arms
    body = []
    rows = [
        (
            "actions/s: bench submit (solid) vs val0 processed (dashed)",
            ("submit_s", "processed_s"),
        ),
        ("orders/s: placed accepted (solid) vs margin-rejected (dashed)", ("placed_s", "rej_margin_s")),
            ("matched/s (val0)", ("matched_s",)),
        ("nonce-expired actions per minute (val0 log)", ("expired",)),
        (
            "exec queue (blocks, mean) and exec busy x100",
            ("exec_queue", "exec_busy100"),
        ),
        ("mempool native size (mean)", ("mempool",)),
        ("CPU cores: node mean (solid), bench (dashed)", ("node_mean", "bench_cores")),
    ]
    cmap = {
        ("glibc", "ctl"): "#1f77b4",
        ("glibc", "s2"): "#d62728",
        ("je", "ctl"): "#2ca02c",
        ("je", "s2"): "#9467bd",
    }
    PW, PH = 560, 200
    for ri, (lab, keys) in enumerate(rows):
        for ci, alloc in enumerate(("glibc", "je")):
            ser = []
            for arm in ("ctl", "s2"):
                ms = A[(alloc, arm)]["minutes"]
                for ki, k in enumerate(keys):

                    def val(m, k=k):
                        if k == "exec_busy100":
                            return m["exec_busy"] * 100
                        if k == "node_mean":
                            return sum(m["node_cores"]) / 3
                        return m[k]

                    ser.append(
                        {
                            "pts": [(m["min"] - 0.5, val(m)) for m in ms],
                            "color": cmap[(alloc, arm)],
                            "dash": ki == 1,
                            "label": (
                                f"{'main' if arm == 'ctl' else 'step 2'}"
                                + (f" {k}" if len(keys) > 1 else "")
                            )
                            if True
                            else None,
                        }
                    )
            panel(
                body,
                10 + ci * (PW + 10),
                30 + ri * (PH + 6),
                PW,
                PH,
                ser,
                f"{alloc}: {lab}",
                "",
                10,
                xlab="minute (midpoint)",
            )
    svg(
        od + "/fig-load.svg",
        2 * (PW + 10) + 10,
        40 + len(rows) * (PH + 6),
        body,
        "Generator vs node per minute (val0 counters; bench.log submitted; cpu-ticks; cpu.csv)",
    )


main()
