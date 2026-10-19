#!/usr/bin/env python3
"""ozarchy-8r analysis (bench queue 8r, 8o rerun with the RH4 GC worker, archy s68). Run with python3 -I.

r8-analyse.py RUN_DIR [ROOT]   (RUN_DIR has cells.tsv + campaign.log + cell-step/; ROOT holds the cell dirs)

Adapted from ozarchy-8o-run/analyst/o8-analyse.py (helpers unchanged). Arms A (no retention), B (retention 1000, GC on
the torus-da-gc worker), C (retention 1000 + TORUS_DA_GC_INLINE=1). Per replicate rk (rk-A, rk-B, rk-C, adjacent cells
of one A B C / C B A half-block):
  whole = load +0 .. the shortest load end of the three cells;
  post  = the later of B's and C's first deleting GC pass (first 1 Hz scrape where torus_da_bodies_gc_deleted_total > 0
          on every node, relative to t_bench0) + 5 s .. the same end; pre = +0 .. post start. Same windows for A, B, C.
Per window as 8o. Worker metrics: torus_da_gc_handoff_seconds (consensus-thread cost: job build + enqueue; C: the whole
pass), queue depth / lag heights / fence pins (1 Hz max), coalesced heights, fence kept / refused, held passes by reason.
Paired ratios B/A, C/A, C/B: geometric mean with a 95% t CI (df = n - 1).
"""

import csv
import datetime as dt
import gzip
import json
import math
import pathlib
import re
import statistics as st
import sys

run = pathlib.Path(sys.argv[1])
root = pathlib.Path(
    sys.argv[2] if len(sys.argv) > 2 else pathlib.Path.home() / "bench-results-matched"
)
NODES = ("val0", "val1", "val2")
T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447}
MD5 = "94053e26bad87a43845cddb509800d62"
TICK = 100.0


def hq(pairs, q):
    if not pairs or pairs[-1][1] <= 0:
        return None
    tgt, pl, pc = q * pairs[-1][1], 0.0, 0.0
    for le, c in pairs:
        if c >= tgt:
            if le == math.inf:
                return pl
            return le if c <= pc else pl + (le - pl) * (tgt - pc) / (c - pc)
        pl, pc = le, c
    return pl


def at(series, t):
    best = None
    for ts, v in series:
        if ts > t:
            break
        best = (ts, v)
    return best


def m1hz(d):
    out = {n: {} for n in NODES}
    cur = None
    with gzip.open(d / "metrics-1hz.txt.gz", "rt", errors="replace") as f:
        for line in f:
            if line.startswith("T "):
                p = line.split()
                cur = (float(p[1]), p[2]) if len(p) > 3 and p[3] == "ok" else None
                continue
            if cur is None or not line.startswith("torus_da_"):
                continue
            p = line.rsplit(" ", 1)
            if len(p) != 2:
                continue
            try:
                v = float(p[1])
            except ValueError:
                continue
            out[cur[1]].setdefault(p[0], []).append((cur[0], v))
    return out


def gc_first(mm):
    firsts = []
    for n in NODES:
        s = mm[n].get("torus_da_bodies_gc_deleted_total", [])
        f = next((ts for ts, v in s if v > 0), None)
        if f is None:
            return None
        firsts.append(f)
    return max(firsts)


def sampler_rows(d):
    rows = {n: [] for n in NODES}
    with open(d / "sampler.csv") as f:
        for r in csv.DictReader(f):
            if r["node"] in rows:
                rows[r["node"]].append(r)
    for n in NODES:
        rows[n].sort(key=lambda r: float(r["ts"]))
    return rows


def delta(rows, t0, t1, name):
    def val(t):
        b = None
        for r in rows:
            if float(r["ts"]) > t:
                break
            if r.get(name) not in (None, "", "NA"):
                b = (float(r["ts"]), float(r[name]))
        return b

    a, b = val(t0), val(t1)
    if a is None or b is None:
        return None, None
    return b[1] - a[1], b[0] - a[0]


def load_buckets(d, metric):
    rows = {}
    with open(d / "buckets.csv") as f:
        for r in csv.DictReader(f):
            if r["metric"] != metric:
                continue
            rows.setdefault(r["node"], {}).setdefault(int(float(r["ts"])), {})[
                math.inf if r["le"] in ("+Inf", "inf") else float(r["le"])
            ] = float(r["count"])
    return rows


def hist_window(rows, t0, t1):
    out = {}
    for n in NODES:
        ts = sorted(rows.get(n, {}))
        a = [t for t in ts if t <= t0]
        b = [t for t in ts if t <= t1]
        if not b:
            continue
        A = rows[n][a[-1]] if a else {}
        B = rows[n][b[-1]]
        pairs = sorted((le, B[le] - A.get(le, 0.0)) for le in B)
        out[n] = (hq(pairs, 0.5), hq(pairs, 0.99), pairs[-1][1] if pairs else 0)
    return out


def metric_file(p, name):
    if not p.exists():
        return None
    for line in open(p):
        q = line.split()
        if len(q) == 2 and q[0] == name:
            return float(q[1])
    return None


def held_file(p):
    out = {}
    if not p.exists():
        return out
    for line in open(p):
        if line.startswith("torus_da_gc_held_passes") and 'reason="' in line:
            r = line.split('reason="', 1)[1].split('"', 1)[0]
            out[r] = out.get(r, 0.0) + float(line.rsplit(" ", 1)[1])
    return out


def iso(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def slow_lines(d, tb0):
    out = []
    p = d / "r8-slowpass.txt"
    if not p.exists():
        return out
    for line in open(p):
        parts = line.split()
        if len(parts) < 3:
            continue
        kv = dict(re.findall(r"(\w+)=(\S+)", line))
        out.append(
            {
                "node": parts[0],
                "t_rel": iso(parts[1]) - tb0,
                **{
                    k: kv.get(k)
                    for k in (
                        "height",
                        "total_ms",
                        "write_ms",
                        "restore_ms",
                        "record_ms",
                        "delete_scan_ms",
                        "tail_ms",
                        "tip_ms",
                        "backfill_ms",
                        "bookkeeping_read_ms",
                        "bodies_recorded",
                        "rows_deleted",
                        "bodies_deleted",
                        "dispatch_lag",
                        "held",
                        "error",
                        "backlog",
                        "fence_pinned",
                        "committed_tip",
                    )
                },
            }
        )
    return out


def win(d, s, sr, ci_rows, hr, t0, t1):
    c = {}
    per = {k: [] for k in ("matched", "nblk", "fpb", "blk_ms", "to", "nblocks")}
    for n in NODES:
        dm, dtt = delta(sr[n], t0, t1, "torus_orders_matched_total")
        db, _ = delta(sr[n], t0, t1, "torus_exec_native_blocks_total")
        es, _ = delta(sr[n], t0, t1, "torus_exec_block_seconds_sum")
        ec, _ = delta(sr[n], t0, t1, "torus_exec_block_seconds_count")
        to, _ = delta(sr[n], t0, t1, "torus_consensus_timeout_total_total")
        per["matched"].append(dm / dtt)
        per["nblk"].append(db / dtt)
        per["fpb"].append(dm / db)
        per["blk_ms"].append(1000 * es / ec)
        per["to"].append(to)
        per["nblocks"].append(db)
    for k in per:
        c[k] = st.mean(per[k])
    bt = hist_window(ci_rows, t0, t1)
    c["p50"] = st.mean(v[0] * 1000 for v in bt.values())
    c["p99"] = st.mean(v[1] * 1000 for v in bt.values())
    cpu = []
    for n, rate in zip(NODES, per["matched"]):
        ser = [
            (float(r["ts"]), float(r[f"{n}_cpu_ticks"]))
            for r in hr
            if r.get(f"{n}_cpu_ticks") not in (None, "", "NA")
        ]
        a, b = at(ser, t0), at(ser, t1)
        fills = rate * (b[0] - a[0])
        cpu.append((b[1] - a[1]) / TICK / (fills / 1e6))
    c["cpu_1m"] = st.mean(cpu)
    c["_nblocks_per_node"] = per["nblocks"]
    return c


def hist_file(p, metric):
    b = {}
    if p.exists():
        for line in open(p):
            if line.startswith(metric + "_bucket"):
                le = line.split('le="')[1].split('"')[0]
                b[math.inf if le == "+Inf" else float(le)] = float(line.rsplit(" ", 1)[1])
    return sorted(b.items())


def cell(label, arm, w0, w1, gf_rel):
    d = root / label
    s = json.loads((d / "summary.json").read_text())
    tb0 = s["timing"]["t_bench0"]
    sr = sampler_rows(d)
    ci_rows = load_buckets(d, "torus_commit_interval_seconds_bucket")
    with open(d / "host-1hz.tsv") as fh:
        hr = list(csv.DictReader(fh, delimiter="\t"))
    c = {"label": label, "arm": arm, "tb0": tb0, "load_s": s["timing"]["t_bench1"] - tb0}
    c["whole"] = win(d, s, sr, ci_rows, hr, tb0, tb0 + w1)
    c["post"] = win(d, s, sr, ci_rows, hr, tb0 + w0, tb0 + w1)
    c["pre"] = win(d, s, sr, ci_rows, hr, tb0, tb0 + w0)
    mins, m = [], 0
    while (m + 1) * 60 <= w1:
        a, b = tb0 + 60 * m, tb0 + 60 * (m + 1)
        v = []
        for n in NODES:
            db, dtt = delta(sr[n], a, b, "torus_exec_native_blocks_total")
            v.append(db / dtt if dtt else float("nan"))
        mins.append(st.mean(v))
        m += 1
    c["per_min"] = mins
    c["gf_rel"] = gf_rel
    c["gf_min"] = int(gf_rel // 60) + 1 if gf_rel is not None else None
    fl = [s["proc_cpu_by_node"][n]["load"]["fills"] for n in NODES]
    ex = [s["sched_by_node"][n]["threads"]["torus-execution"]["on_cpu_ms"] for n in NODES]
    c["exec_cpu_1k"] = st.mean(e / (f / 1000) for e, f in zip(ex, fl))
    h = s["headline"]
    c["h_matched"], c["h_nblk"], c["h_to"] = h["matched_s_avg"], h["native_blk_s"], h["consensus_timeouts"]
    c["agree"], c["live"] = h["agreement_verdict"], h["liveness_verdict"]
    c["valid"] = s["validity"]["verdict"] if isinstance(s.get("validity"), dict) else s.get("validity")
    c["rc"] = s["timing"].get("bench_rc")
    md5s = [l.split("exe_md5=")[1].strip() for l in open(d / "node-environ-trie.txt") if "exe_md5=" in l]
    c["md5_ok"] = len(md5s) == 3 and all(x == MD5 for x in md5s)
    ma = [d / f"metrics-after-{n}.txt" for n in NODES]
    mf = lambda name: [metric_file(p, name) or 0.0 for p in ma]
    c["gc_cnt_cell"] = st.mean(mf("torus_da_gc_seconds_count"))
    c["gc_sum_cell"] = st.mean(mf("torus_da_gc_seconds_sum"))
    c["gc_del_cell"] = st.mean(mf("torus_da_bodies_gc_deleted_total"))
    c["gc_slow_counter"] = sum(mf("torus_da_gc_slow_passes_total"))
    c["ho_cnt_cell"] = st.mean(mf("torus_da_gc_handoff_seconds_count"))
    c["ho_sum_cell"] = st.mean(mf("torus_da_gc_handoff_seconds_sum"))
    c["coalesced"] = sum(mf("torus_da_gc_coalesced_heights_total"))
    c["fence_kept"] = sum(mf("torus_da_gc_fence_kept_total"))
    c["fence_refused"] = sum(mf("torus_da_gc_fence_refused_pins_total"))
    c["qd_end"] = max(mf("torus_da_gc_queue_depth"))
    c["lag_end"] = max(mf("torus_da_gc_lag_heights"))
    held = {}
    for p in ma:
        for r, v in held_file(p).items():
            held[r] = held.get(r, 0.0) + v
    c["held_cell"] = held
    mm = m1hz(d)
    gw_s, gw_c, hw_s, hw_c, lpm, qd, lag, pins = [], [], [], [], [], [], [], []
    for n in NODES:

        def dd(name, a_t, b_t):
            ser = mm[n].get(name, [])
            a, b = at(ser, a_t), at(ser, b_t)
            if b is None:
                return 0.0
            return b[1] - (a[1] if a else 0.0)

        gw_s.append(dd("torus_da_gc_seconds_sum", tb0 + w0, tb0 + w1))
        gw_c.append(dd("torus_da_gc_seconds_count", tb0 + w0, tb0 + w1))
        hw_s.append(dd("torus_da_gc_handoff_seconds_sum", tb0 + w0, tb0 + w1))
        hw_c.append(dd("torus_da_gc_handoff_seconds_count", tb0 + w0, tb0 + w1))
        lpm.extend(v for _, v in mm[n].get("torus_da_gc_last_pass_max_seconds", []))
        qd.extend(v for _, v in mm[n].get("torus_da_gc_queue_depth", []))
        lag.extend(v for _, v in mm[n].get("torus_da_gc_lag_heights", []))
        pins.extend(v for _, v in mm[n].get("torus_da_gc_fence_pins", []))
    nb = c["post"]["_nblocks_per_node"]
    c["gc_ms_blk_post"] = st.mean(1000 * x / b for x, b in zip(gw_s, nb))
    c["gc_ms_pass_post"] = 1000 * sum(gw_s) / sum(gw_c) if sum(gw_c) else None
    c["ho_ms_blk_post"] = st.mean(1000 * x / b for x, b in zip(hw_s, nb))
    c["ho_us_post"] = 1e6 * sum(hw_s) / sum(hw_c) if sum(hw_c) else None
    c["gc_last_pass_max_1hz_ms"] = 1000 * max(lpm) if lpm else None
    c["qd_max_1hz"] = max(qd) if qd else None
    c["lag_max_1hz"] = max(lag) if lag else None
    c["pins_max_1hz"] = max(pins) if pins else None
    c["slow"] = slow_lines(d, tb0)
    q50, q99, h50, h99 = [], [], [], []
    for p in ma:
        pr = hist_file(p, "torus_da_gc_seconds")
        if pr and pr[-1][1] > 0:
            q50.append(hq(pr, 0.5) * 1000)
            q99.append(hq(pr, 0.99) * 1000)
        pr = hist_file(p, "torus_da_gc_handoff_seconds")
        if pr and pr[-1][1] > 0:
            h50.append(hq(pr, 0.5) * 1000)
            h99.append(hq(pr, 0.99) * 1000)
    c["gc_p50_ms"] = st.mean(q50) if q50 else None
    c["gc_p99_ms"] = st.mean(q99) if q99 else None
    c["gc_p99_ms_max"] = max(q99) if q99 else None
    c["ho_p50_ms"] = st.mean(h50) if h50 else None
    c["ho_p99_ms"] = st.mean(h99) if h99 else None
    return c


def ci(xs):
    v = [math.log(x) for x in xs]
    m = st.mean(v)
    hw = T975[len(v) - 1] * st.stdev(v) / math.sqrt(len(v)) if len(v) > 1 else float("nan")
    return math.exp(m), math.exp(m - hw), math.exp(m + hw)


def diff_ci(xs):
    m = st.mean(xs)
    hw = T975[len(xs) - 1] * st.stdev(xs) / math.sqrt(len(xs)) if len(xs) > 1 else float("nan")
    return m, m - hw, m + hw


ARMS = ("A", "B", "C")
cells = list(csv.DictReader(open(run / "cells.tsv"), delimiter="\t"))
main = [r for r in cells if re.fullmatch(r"r\d+", r["rep"])]
reps = sorted({r["rep"] for r in main}, key=lambda x: int(x[1:]))
res, meta = {}, {}
for rep in reps:
    lab = {r["arm"]: r["label"] for r in main if r["rep"] == rep}
    if set(lab) != set(ARMS):
        print(f"rep {rep}: incomplete {lab}")
        continue
    tim = {a: json.loads((root / lab[a] / "summary.json").read_text())["timing"] for a in ARMS}
    gf = {a: gc_first(m1hz(root / lab[a])) for a in ("B", "C")}
    gfr = {a: gf[a] - tim[a]["t_bench0"] for a in gf}
    w0 = math.ceil(max(gfr.values())) + 5
    w1 = min(tim[a]["t_bench1"] - tim[a]["t_bench0"] for a in ARMS)
    meta[rep] = {"gf_B": gfr["B"], "gf_C": gfr["C"], "w0": w0, "w1": w1}
    for a in ARMS:
        res[(rep, a)] = cell(lab[a], a, w0, w1, gfr.get(a))
reps = [p for p in reps if (p, "A") in res]

f = lambda x, p=1: "n/a" if x is None else f"{x:,.{p}f}"
print("### windows")
for rep in reps:
    m = meta[rep]
    print(f"{rep}: first deleting pass B +{m['gf_B']:.0f} s, C +{m['gf_C']:.0f} s; whole +0..+{m['w1']} s; post +{m['w0']}..+{m['w1']} s ({m['w1'] - m['w0']} s)")

ROWS = [
    ("nblk", "native blocks/s", 3),
    ("fpb", "fills per native block", 0),
    ("p50", "block p50 ms", 1),
    ("p99", "block p99 ms", 0),
    ("to", "timeouts (node mean)", 1),
    ("matched", "matched/s", 0),
    ("cpu_1m", "CPU-s per 1M fills", 2),
    ("blk_ms", "exec block_ms", 1),
]
order = sorted(res.items(), key=lambda kv: kv[1]["tb0"])
for wn in ("whole", "post", "pre"):
    print(f"\n### per cell, window {wn} (run order)")
    print("| cell | " + " | ".join(r[1] for r in ROWS) + " |")
    print("|---" * (len(ROWS) + 1) + "|")
    for (rep, arm), c in order:
        print(f"| {rep}-{arm} | " + " | ".join(f(c[wn][k], p) for k, _, p in ROWS) + " |")

print("\n### per cell: validity, md5, headline, exec on-CPU")
print("| cell | rc | valid/agree/live | md5 ok | load s | headline matched/s | headline blk/s | headline timeouts | exec on-CPU ms/1k fills |")
print("|---" * 9 + "|")
for (rep, arm), c in order:
    print(f"| {rep}-{arm} | {c['rc']} | {c['valid']}/{c['agree']}/{c['live']} | {c['md5_ok']} | {c['load_s']} | {f(c['h_matched'], 0)} | {f(c['h_nblk'], 3)} | {f(c['h_to'], 1)} | {f(c['exec_cpu_1k'], 3)} |")

print("\n### GC + worker per cell (B, C; node mean unless sum)")
print("| cell | first del pass s | GC passes | GC s | GC ms/pass post | GC ms/native blk post | GC p50/p99 ms (p99 max) | handoffs | handoff s | handoff us/height post | handoff ms/native blk post | handoff p50/p99 ms | bodies deleted | queue max 1Hz / end | lag max 1Hz / end | coalesced (sum) | fence kept / refused (sum) | pins max 1Hz | held by reason (sum) | slow lines / counter | last_pass max 1Hz ms |")
print("|---" * 21 + "|")
for (rep, arm), c in order:
    if arm == "A":
        continue
    print(
        f"| {rep}-{arm} | {f(c['gf_rel'], 0)} | {f(c['gc_cnt_cell'], 0)} | {f(c['gc_sum_cell'], 3)} | {f(c['gc_ms_pass_post'], 3)} | {f(c['gc_ms_blk_post'], 3)} | {f(c['gc_p50_ms'], 3)}/{f(c['gc_p99_ms'], 2)} ({f(c['gc_p99_ms_max'], 2)}) | "
        f"{f(c['ho_cnt_cell'], 0)} | {f(c['ho_sum_cell'], 4)} | {f(c['ho_us_post'], 1)} | {f(c['ho_ms_blk_post'], 4)} | {f(c['ho_p50_ms'], 4)}/{f(c['ho_p99_ms'], 3)} | {f(c['gc_del_cell'], 0)} | "
        f"{f(c['qd_max_1hz'], 0)} / {f(c['qd_end'], 0)} | {f(c['lag_max_1hz'], 0)} / {f(c['lag_end'], 0)} | {f(c['coalesced'], 0)} | {f(c['fence_kept'], 0)} / {f(c['fence_refused'], 0)} | {f(c['pins_max_1hz'], 0)} | {c['held_cell'] or 0} | {len(c['slow'])} / {f(c['gc_slow_counter'], 0)} | {f(c['gc_last_pass_max_1hz_ms'], 1)} |"
    )
for (rep, arm), c in order:
    if arm == "A" and (c["gc_cnt_cell"] or c["ho_cnt_cell"] or c["held_cell"]):
        print(f"WARNING {rep}-A has GC metrics: passes {c['gc_cnt_cell']} handoffs {c['ho_cnt_cell']} held {c['held_cell']}")

print("\n### native blocks/s per minute (node mean; * = minute of this cell's first deleting pass)")
nm = max(len(c["per_min"]) for c in res.values())
print("| cell | " + " | ".join(f"m{i + 1}" for i in range(nm)) + " |")
print("|---" * (nm + 1) + "|")
for (rep, arm), c in order:
    print(f"| {rep}-{arm} | " + " | ".join(f"{v:.2f}{'*' if c['gf_min'] == i + 1 else ''}" for i, v in enumerate(c["per_min"])) + " |")

print("\n### arm means (mean of the n cells; sd)")
print("| window | metric | A | B | C |")
print("|---|---|---|---|---|")
for wn in ("whole", "post", "pre"):
    for k, name, p in ROWS:
        vals = {a: [res[(r, a)][wn][k] for r in reps] for a in ARMS}
        print(f"| {wn} | {name} | " + " | ".join(f"{f(st.mean(vals[a]), p)} ({f(st.stdev(vals[a]) if len(vals[a]) > 1 else 0, p)})" for a in ARMS) + " |")
vals = {a: [res[(r, a)]["exec_cpu_1k"] for r in reps] for a in ARMS}
print("| whole load | exec on-CPU ms/1k fills | " + " | ".join(f"{st.mean(vals[a]):.3f} ({st.stdev(vals[a]) if len(vals[a]) > 1 else 0:.3f})" for a in ARMS) + " |")

summary = {}
for num, den in (("B", "A"), ("C", "A"), ("C", "B")):
    print(f"\n### paired {num}/{den} (geo-mean ratio, 95% t CI, df = n - 1); {num} < {den} count")
    print(f"| window | metric | {den} mean | {num} mean | " + " | ".join(f"{p}" for p in reps) + f" | geo-mean [95% CI] | {num}<{den} |")
    print("|---" * (len(reps) + 6) + "|")
    for wn in ("whole", "post", "pre"):
        for k, name, p in ROWS + [("exec_cpu_1k", "exec on-CPU ms/1k fills (whole load)", 3)]:
            if k == "exec_cpu_1k" and wn != "whole":
                continue
            get = (lambda c: c[k]) if k == "exec_cpu_1k" else (lambda c: c[wn][k])
            a = [get(res[(r, den)]) for r in reps]
            b = [get(res[(r, num)]) for r in reps]
            lower = sum(1 for x, y in zip(a, b) if y < x)
            if k == "to":
                dm, lo, hi = diff_ci([y - x for x, y in zip(a, b)])
                cs = f"diff {dm:+.2f} [{lo:+.2f}, {hi:+.2f}]"
                rs = [f"{y - x:+.1f}" for x, y in zip(a, b)]
            else:
                rat = [y / x for x, y in zip(a, b)]
                g, lo, hi = ci(rat)
                cs = f"{g:.4f} [{lo:.4f}, {hi:.4f}]"
                rs = [f"{x:.4f}" for x in rat]
                summary[f"{num}/{den}:{wn}:{k}"] = {"geo": g, "lo": lo, "hi": hi, "ratios": rat, "lower": lower}
            print(f"| {wn} | {name} | {f(st.mean(a), p)} | {f(st.mean(b), p)} | " + " | ".join(rs) + f" | {cs} | {lower}/{len(reps)} |")
    print(f"per minute {num}/{den} (native blocks/s):")
    nm = min(len(res[(r, a)]["per_min"]) for r in reps for a in ARMS)
    for i in range(nm):
        rat = [res[(r, num)]["per_min"][i] / res[(r, den)]["per_min"][i] for r in reps]
        g, lo, hi = ci(rat)
        print(f"  m{i + 1}: " + " ".join(f"{x:.3f}" for x in rat) + f" | geo {g:.4f} [{lo:.4f}, {hi:.4f}] | {num}<{den} {sum(1 for x in rat if x < 1)}/{len(rat)}")

print("\n### slow-pass WARN lines (B, C cells; t = s after load start)")
print("| cell | node | t s | height | total ms | write ms | restore ms | record ms | delete_scan ms | tail ms | bodies deleted | rows deleted | fence_pinned | dispatch_lag | held | error |")
print("|---" * 16 + "|")
allslow = {"B": [], "C": []}
for (rep, arm), c in order:
    for x in c["slow"]:
        allslow.setdefault(arm, []).append(x)
        print(f"| {rep}-{arm} | {x['node']} | {x['t_rel']:.0f} | {x['height']} | {x['total_ms']} | {x['write_ms']} | {x['restore_ms']} | {x['record_ms']} | {x['delete_scan_ms']} | {x['tail_ms']} | {x['bodies_deleted']} | {x['rows_deleted']} | {x['fence_pinned']} | {x['dispatch_lag']} | {x['held']} | {x['error']} |")
for arm, xs in allslow.items():
    if not xs:
        print(f"arm {arm}: 0 slow lines")
        continue
    tot = [float(x["total_ms"]) for x in xs]
    wr = [float(x["write_ms"]) for x in xs]
    print(f"arm {arm}: {len(xs)} slow lines: total_ms min {min(tot):.1f} median {st.median(tot):.1f} max {max(tot):.1f}; write_ms share {sum(wr) / sum(tot):.3f}; bodies_deleted=0: {sum(1 for x in xs if x['bodies_deleted'] == '0')}; before +80 s: {sum(1 for x in xs if x['t_rel'] < 80)}; in +120..+135 s: {sum(1 for x in xs if 120 <= x['t_rel'] <= 135)}")

print("\n### gaps END -> next START (campaign.log), post-cell step = END -> cell-step/<label>.json mtime")
log = (run / "campaign.log").read_text().splitlines()
ev = []
for l in log:
    m = re.match(r"\[(\d\d:\d\d:\d\d)\] (START|END) (\S+)", l)
    if m:
        ev.append((m.group(2), m.group(3), m.group(1)))


def tsec(hms):
    h, mi, se = map(int, hms.split(":"))
    return h * 3600 + mi * 60 + se


def tdiff(a, b):
    return (tsec(b) - tsec(a)) % 86400


print("| after cell | END | next START | gap s | post-cell step s | rest s |")
print("|---|---|---|---|---|---|")
gaps = []
for i, (kind, lab, hms) in enumerate(ev):
    if kind != "END":
        continue
    nxt = next((e for e in ev[i + 1:] if e[0] == "START"), None)
    if not nxt:
        continue
    g = tdiff(hms, nxt[2])
    cs = run / "cell-step" / f"{lab}.json"
    if cs.exists():
        mt = dt.datetime.fromtimestamp(cs.stat().st_mtime)
        step = tdiff(hms, mt.strftime("%H:%M:%S"))
    else:
        step = None
    gaps.append((g, step))
    print(f"| {lab} | {hms} | {nxt[2]} | {g} | {step} | {None if step is None else g - step} |")
if gaps:
    print(f"gap s min {min(g for g, _ in gaps)} max {max(g for g, _ in gaps)}; rest s max {max((g - s) for g, s in gaps if s is not None)}")

json.dump(
    {"cells": {f"{k[0]}-{k[1]}": v for k, v in res.items()}, "meta": meta, "summary": summary},
    open(run.parent / "analyst" / "r8-analyse.json", "w"),
    indent=1,
    default=str,
)
