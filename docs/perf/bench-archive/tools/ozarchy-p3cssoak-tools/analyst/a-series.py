"""ozarchy-p3cssoak: per-node time series for the cancel-share glibc soak pair (cs2 = test/p3-cancel-share 41a95355 vs
step 2 g2 = 7b006739, both 15 s, glibc), next to the section 53 glibc soak (main 219f0c20 = RAM reference, step 2 7b006739).

python3 -I a-series.py OUTDIR [PREFIX]   (PREFIX default ozarchy-p3cssoak-300m; reads cells under /home/oz/bench-results-matched)

Copy of ozarchy-p3soakje-run/analyst/a-series.py (load / analyse / ols / plotting helpers unchanged) plus, per node per minute:
nonce-expired evictions from each node's log (val0-2 .log.gz), cancel-alls executed/s, margin-rejected orders/s, placed/s,
mempool size (sampler), block-build select ms per call (torus_block_build_select_seconds, proposer builds only); per cell:
cancel-alls sent (bench.log Econ mix) vs executed (metrics-after - metrics-before per node) and the bench's releases.
Outputs: series.json, series.txt, fig-rss.svg, fig-node.svg, fig-load.svg (rsvg-convert -> png separately)."""

import datetime as dt
import gzip
import json
import math
import re
import sys

R = "/home/oz/bench-results-matched"
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


# ------------------------------------------------------------------ ozarchy-p3cssoak additions
PREFIX = sys.argv[2] if len(sys.argv) > 2 else "ozarchy-p3cssoak-300m"
CELLS = [
    ("m53", "main 219f0c20 (section 53, glibc)", "ozarchy-p3soak-300m-k1-ctl"),
    ("s53", "step 2 7b006739 (section 53, glibc)", "ozarchy-p3soak-300m-k1-s2"),
    ("g2", "step 2 7b006739 (this pair)", PREFIX + "-k1-g2"),
    ("cs2", "cancel-share 41a95355 (this pair)", PREFIX + "-k1-cs2"),
]
LAB = {k: lab for k, lab, _ in CELLS}
CC = {"m53": "#888888", "s53": "#ff7f0e", "g2": "#d62728", "cs2": "#1f77b4"}
DASH = {"m53": True, "s53": True, "g2": False, "cs2": False}


def mval(path, name):
    s, f = 0.0, False
    for line in open(path):
        p = line.split()
        if len(p) == 2 and (p[0] == name or p[0].startswith(name + "{")):
            s += float(p[1])
            f = True
    return s if f else float("nan")


def load_extra(cell, d):
    c = f"{R}/{cell}"
    t0 = d["t0"]
    d["expn"] = {v: [] for v in V}
    for i, v in enumerate(V):
        with gzip.open(f"{c}/val{i}.log.gz", "rt", errors="replace") as f:
            for line in f:
                if "nonce-expired" not in line:
                    continue
                line = ANSI.sub("", line)
                m = re.match(r"(\S+Z) .*evicted=(\d+)", line)
                if m:
                    ts = dt.datetime.fromisoformat(m.group(1).replace("Z", "+00:00")).timestamp()
                    d["expn"][v].append((ts - t0, int(m.group(2))))
    b = {}
    for line in open(c + "/bench.log"):
        m = re.search(r"\[sent: place (\d+) cancel-all (\d+)\]", line)
        if m:
            b["sent_place"], b["sent_cancel_all"] = int(m.group(1)), int(m.group(2))
        m = re.search(r"Econ mix \(load-gen accepted\): place (\d+) .*\| cancel-all (\d+)", line)
        if m:
            b["acc_place"], b["acc_cancel_all"] = int(m.group(1)), int(m.group(2))
        m = re.search(r"released committed=(\d+) refused=(\d+) timeout=(\d+)", line)
        if m:
            b["rel_committed"], b["rel_refused"], b["rel_timeout"] = map(int, m.groups())
    d["bench"] = b
    d["metr"] = {}
    for i, v in enumerate(V):
        r = {}
        for k in ("torus_exec_cancel_all_total", "torus_orders_rejected_margin_total", "torus_exec_action_failures_total",
                  "torus_orders_placed_accepted_total", "torus_orders_matched_total"):
            r[k] = mval(f"{c}/metrics-after-val{i}.txt", k) - mval(f"{c}/metrics-before-val{i}.txt", k)
        d["metr"][v] = r


def extra(d, W):
    out = {"nodes": {}, "minutes": {}}
    for v in V:
        sv = d["smp"][v]

        def cv(k, sv=sv):
            return [(t, r.get(k, float("nan"))) for t, r in sv]

        rows = []
        for k in range(int(W // 60)):
            a, b = 60 * k, 60 * (k + 1)
            row = {"min": k + 1}
            row["expired"] = sum(n for t, n in d["expn"][v] if a <= t < b)
            for name, key in (("cancel_all_s", "torus_exec_cancel_all_total"), ("rej_margin_s", "torus_orders_rejected_margin_total"),
                              ("placed_s", "torus_orders_placed_accepted_total")):
                s = cv(key)
                row[name] = (interp(s, b) - interp(s, a)) / 60
            row["pool"] = mean_in(cv("torus_mempool_native_size"), a, b)
            ss, sc = cv("torus_block_build_select_seconds_sum"), cv("torus_block_build_select_seconds_count")
            n = interp(sc, b) - interp(sc, a)
            row["select_n"] = n
            row["select_ms"] = (interp(ss, b) - interp(ss, a)) / n * 1000 if n > 0.5 else float("nan")
            rows.append(row)
        out["minutes"][v] = rows
        exp_all = sum(n for t, n in d["expn"][v])
        exp_win = sum(n for t, n in d["expn"][v] if 0 <= t <= W)
        first = next((t for t, n in sorted(d["expn"][v]) if n > 0), float("nan"))
        out["nodes"][v] = {"expired_cell": exp_all, "expired_window": exp_win, "first_expiry_s": first, **d["metr"][v]}
    out["bench"] = d["bench"]
    return out


def main():
    od = sys.argv[1]
    D, A, X = {}, {}, {}
    for key, lab, cell in CELLS:
        D[key] = load(cell)
        load_extra(cell, D[key])
        A[key] = analyse(D[key])
        X[key] = extra(D[key], A[key]["window_s"])
        A[key]["extra"] = X[key]
        A[key]["cell"] = cell
    json.dump(A, open(od + "/series.json", "w"), indent=1)
    ref = A["m53"]["nodes"]
    with open(od + "/series.txt", "w") as f:
        for key, lab, cell in CELLS:
            r, x = A[key], X[key]
            f.write(f"== {key}: {lab} [{cell}] window {r['window_s']:.0f} s\n")
            b = x["bench"]
            f.write(f"  bench: sent place {b.get('sent_place')} cancel-all {b.get('sent_cancel_all')} | accepted cancel-all {b.get('acc_cancel_all')} | "
                    f"released committed {b.get('rel_committed')} refused {b.get('rel_refused')} timeout {b.get('rel_timeout')}\n")
            for v in V:
                n, e = r["nodes"][v], x["nodes"][v]
                f.write(f"  {v} cancel-alls executed {e['torus_exec_cancel_all_total']:.0f} (sent - executed {b.get('sent_cancel_all', 0) - e['torus_exec_cancel_all_total']:.0f}) | "
                        f"nonce-expired evictions cell {e['expired_cell']} window {e['expired_window']} first at {e['first_expiry_s']:.0f} s | "
                        f"margin-rejected {e['torus_orders_rejected_margin_total']:.0f} | action failures {e['torus_exec_action_failures_total']:.0f}\n")
                f.write(
                    f"  {v} RSS MB mean 2-5 {n['rss_mean_2_5']:.0f} 5-10 {n['rss_mean_5_10']:.0f} (change {n['rss_mean_5_10'] - n['rss_mean_2_5']:+.0f}) | @1 {n['rss_1min']:.0f} @5 {n['rss_5min']:.0f} end {n['rss_end']:.0f} peak {n['rss_peak']:.0f} | "
                    f"slope MB/min 2-5 {n['slope_m2_5'][0]:+.1f} [{n['slope_m2_5'][1]:+.1f}, {n['slope_m2_5'][2]:+.1f}] last5 {n['slope_late'][0]:+.1f} [{n['slope_late'][1]:+.1f}, {n['slope_late'][2]:+.1f}] n_eff {n['slope_late'][3]:.0f} | "
                    f"vs main53: mean5-10 {(n['rss_mean_5_10'] - ref[v]['rss_mean_5_10']) / 1024:+.2f} GB end {(n['rss_end'] - ref[v]['rss_end']) / 1024:+.2f} peak {(n['rss_peak'] - ref[v]['rss_peak']) / 1024:+.2f}\n")
                if "checkpoints" in n:
                    f.write(f"     checkpoints {n['checkpoints']:.0f} layer MB mean {n['layer_mb_mean']:.1f} early {n['layer_mb_early']:.1f} late {n['layer_mb_late']:.1f} | "
                            f"settle ms/block early {n['settle_ms_per_block_early']:.1f} late {n['settle_ms_per_block_late']:.1f} | layer_shared {n['layer_shared']:.0f} drop_inline {n['drop_inline']:.0f}\n")
            f.write("  min submit/s processed/s placed/s matched/s matched/action blk/s exec_busy mempool exec_q resting cancel-all/s expired(val0) node_cores bench_cores margin_rejected/s\n")
            for m in r["minutes"]:
                f.write(
                    f"  {m['min']:>3} {m['submit_s']:7.0f} {m['processed_s']:7.0f} {m['placed_s']:9.0f} {m['matched_s']:9.0f} {m['matched_per_action']:6.1f} "
                    f"{m['native_blk_s']:5.2f} {m['exec_busy']:5.2f} {m['mempool']:7.0f} {m['exec_queue']:5.1f} {m['resting']:9.0f} {m['cancel_all_s']:6.0f} {m['expired']:6d} "
                    f"{'/'.join(f'{y:.2f}' for y in m['node_cores'])} {m['bench_cores']:.2f} {m['rej_margin_s']:.0f}\n")
            f.write("  per node per minute: min RSS_MB matched/s expired cancel_all/s margin_rej/s placed/s resting_M pool select_ms(n) flush_depth handoff_ms ckpts layer_MB drop_inline\n")
            for v in V:
                for m, e in zip(r["node_minutes"][v], x["minutes"][v]):
                    f.write(
                        f"  {v} {m['min']:>2} {m['rss_mb']:6.0f} {m['matched_s']:8.0f} {e['expired']:6d} {e['cancel_all_s']:6.0f} {e['rej_margin_s']:7.0f} {e['placed_s']:7.0f} "
                        f"{m['resting_m']:5.2f} {e['pool']:6.0f} {e['select_ms']:6.2f}({e['select_n']:.0f}) {m['flush_depth']:5.1f} {m['handoff_ms']:6.2f} {m['ckpts']:4.0f} {m['layer_mb']:6.1f} {m['drop_inline']:5.0f}\n")
        # Phase 3 deltas (per node, vs main 53) and cs2 vs g2
        f.write("== deltas per node (GB): mean min 5-10 / @10 min / peak\n")
        for key in ("s53", "g2", "cs2"):
            for v in V:
                n, rf = A[key]["nodes"][v], ref[v]
                f.write(f"  {key} - m53 {v}: {(n['rss_mean_5_10'] - rf['rss_mean_5_10']) / 1024:+.2f} / {(n['rss_end'] - rf['rss_end']) / 1024:+.2f} / {(n['rss_peak'] - rf['rss_peak']) / 1024:+.2f}\n")
            mm = lambda k: sum(A[key]["nodes"][v][k] - ref[v][k] for v in V) / 3 / 1024
            f.write(f"  {key} - m53 mean: {mm('rss_mean_5_10'):+.2f} / {mm('rss_end'):+.2f} / {mm('rss_peak'):+.2f}\n")
        for v in V:
            n, g = A["cs2"]["nodes"][v], A["g2"]["nodes"][v]
            f.write(f"  cs2 - g2 {v}: {(n['rss_mean_5_10'] - g['rss_mean_5_10']) / 1024:+.2f} / {(n['rss_end'] - g['rss_end']) / 1024:+.2f} / {(n['rss_peak'] - g['rss_peak']) / 1024:+.2f}\n")
    # ---- fig-rss: one panel per node, 4 cells
    body = []
    xmax = 10.2
    for ci, v in enumerate(V):
        ser = []
        for key, lab, _ in CELLS:
            d = D[key]
            ser.append({"pts": [(t / 60, y / 1024) for t, y in d["rss"][v] if -12 <= t <= d["t1"] - d["t0"]],
                        "color": CC[key], "dash": DASH[key], "label": lab})
        panel(body, 10 + ci * 470, 30, 460, 380, ser, f"{v}: VmRSS", "GB", xmax, ymin0=False)
    svg(od + "/fig-rss.svg", 3 * 470 + 10, 420, body,
        "Cancel-share soak (glibc, 600 s): RSS per node, cs2 41a95355 vs step 2 7b006739, with section 53's main and step 2 (dashed)")
    # ---- fig-node: rows = metric per minute, columns = nodes, 4 cells
    rows = [
        ("nonce-expired evictions per minute (node log)", lambda key, v: [(e["min"] - 0.5, e["expired"]) for e in X[key]["minutes"][v]]),
        ("cancel-alls executed per s", lambda key, v: [(e["min"] - 0.5, e["cancel_all_s"]) for e in X[key]["minutes"][v]]),
        ("margin-rejected orders per s", lambda key, v: [(e["min"] - 0.5, e["rej_margin_s"]) for e in X[key]["minutes"][v]]),
        ("matched per s", lambda key, v: [(m["min"] - 0.5, m["matched_s"]) for m in A[key]["node_minutes"][v]]),
        ("resting orders (M, gauge)", lambda key, v: [(t / 60, kv.get("torus_exec_resting_orders", float("nan")) / 1e6) for t, kv in D[key]["g"][v]]),
        ("cancel-all index entries (k, gauge)", lambda key, v: [(t / 60, kv.get("torus_exec_cancel_all_index_entries", float("nan")) / 1e3) for t, kv in D[key]["g"][v]]),
        ("mempool native size (per minute mean)", lambda key, v: [(e["min"] - 0.5, e["pool"]) for e in X[key]["minutes"][v]]),
        ("block-build select ms per build (per minute)", lambda key, v: [(e["min"] - 0.5, e["select_ms"]) for e in X[key]["minutes"][v]]),
        ("layer MB per checkpoint (60 s window)", lambda key, v: rate(D[key]["ck"][v], "torus_exec_checkpoint_layer_bytes_sum", "torus_exec_checkpoint_layer_bytes_count", 1e-6, 60)),
        ("flush-worker queue depth (sampler, 1 s)", lambda key, v: [(t / 60, r.get("torus_flush_worker_depth", float("nan"))) for t, r in D[key]["smp"][v]]),
    ]
    body = []
    PW, PH = 420, 190
    for ri, (lab, fn) in enumerate(rows):
        for ci, v in enumerate(V):
            ser = [{"pts": fn(key, v), "color": CC[key], "dash": DASH[key], "label": (LAB[key] if ci == 0 and ri == 0 else None)} for key, _, _ in CELLS]
            panel(body, 10 + ci * (PW + 10), 30 + ri * (PH + 6), PW, PH, ser, f"{v}: {lab}", "", xmax)
    svg(od + "/fig-node.svg", 3 * (PW + 10) + 10, 40 + len(rows) * (PH + 6), body,
        "Cancel-share soak: per node expiry, cancel-alls, margin rejections, matched/s, book, pool, select time, layer, flush queue")


main()
