#!/usr/bin/env python3
"""p2s0.py: item 6 Phase 2 step 0 profile numbers (ozarchy-p2s0 campaign). Subcommands:

  window   <cell_dir> load|drain   perf window totals -> <cell_dir>/p2s0-<tag>.json + text
           ms per native block and per 1k fills for: cancel/modify (and its book-scan part), cache flush (serialise),
           save books / diff_stop_rows, margin and matching (buckets2.py PHASE rules, first match wins),
           re-value (crab sums/marks), begin_block_oracle, end_resident, R rebuild; the same-named exec metrics
           (torus_exec_*_seconds sum/count deltas between the prof snapshots); thread ids (scan.py tids.json).
  threads  <cell_dir>             distinct thread ids per minute by comm group from tasks.txt (1 Hz, lower bound)
  timeline <cell_dir> load|drain  per-block timeline: val0 log (executing / block done, native_count) x perf samples
           (needs perf*.clock = CLOCK_MONOTONIC and prof-*.clk); drain: rows 77 (first oracle-only block after the
           load, per node) and 78 (oracle-only blocks with the feed live), with top functions
  rebuild  <cell_dir>             R rebuild cost (row 7): 'resident rows built' log lines + metrics
  compare  <cell_a> <cell_b>      load windows side by side (walk 0 vs walk 10), from p2s0-load.json
Weights are cycles:u periods; ms = period share x process utime of the window (as buckets2.py)."""

import ast
import bisect
import datetime
import gzip
import json
import math
import os
import re
import sys
from collections import Counter, defaultdict

TOOLS = "/home/oz/bench-results-matched/ozarchy-14236fa-tools"
MK = 300
FEED_MAX = 6 * (
    (MK + 255) // 256
)  # harness --feed-mempool-max: 2 rounds x 3 validators x ceil(MK/256) chunks
H = re.compile(r"\[[0-9a-f]{6,16}\]")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z")


def phase_rules():
    """buckets2.py PHASE list, read from its source (no execution: buckets2 runs on import)."""
    src = open(TOOLS + "/buckets2.py").read()
    for node in ast.parse(src).body:
        if isinstance(node, ast.Assign) and any(
            getattr(t, "id", "") == "PHASE" for t in node.targets
        ):
            return ast.literal_eval(node.value)
    raise SystemExit("PHASE not found in buckets2.py")


# inclusive buckets on comm torus-execution (any frame contains any key); they may overlap
CANCEL = [
    "exec_cancel_order",
    "exec_modify_order",
    "exec_cancel_all",
    "cancel_orders_and_stops",
    "cancel_all_many",  # exec_cancel_all is inlined in the --no-inline fold; its book work shows as OrderBook::cancel_all_many
    "try_cancel_all_batch",
]
SCAN = ["OrderBook>::get_order", "OrderBook>::cancel_order", "hashbrown"]
INCL = [
    ("cancel/modify (exec_cancel_order/modify/cancel_all, incl.)", CANCEL),
    (
        "cache flush (Pos/BalCache flush_all + add_cum_volume)",
        ["flush_all", "add_cum_volume"],
    ),
    (
        "save books (save_order_books + stash_resident)",
        ["save_order_books", "stash_resident"],
    ),
    ("  diff_stop_rows", ["diff_stop_rows"]),
    ("  encode_order_row", ["encode_order_row"]),
    (
        "re-value: crab sums/marks (SumsCarry, BlockMarks, *_sums, SumsCache)",
        [
            "SumsCarry",
            "into_carry",
            "BlockMarks",
            "build_sums",
            "sums_with_changes",
            "pos_sums",
            "dirty_sums",
            "delta_sums",
            "cached_sums",
            "direct_sums",
            "reference_sums",
            "sums_of",
            "SumsCache",
            "BatchSums",
            "BlockSums",
        ],
    ),
    ("re-value: unrealized_pnl", ["unrealized_pnl"]),
    ("begin_block_oracle", ["begin_block_oracle"]),
    ("end_resident (exec thread)", ["end_resident"]),
    ("begin_resident", ["begin_resident"]),
    (
        "R rebuild (ResidentRows/TraderPositions::build)",
        ["ResidentRows>::build", "TraderPositions>::build"],
    ),
    ("load_books / rebuild_book", ["load_books", "rebuild_book"]),
    ("verify (batch_verify)", ["batch_verify"]),
]
ANY_COMM = [
    ("end_resident worker (comm torus-end-resid, all)", "torus-end-resid"),
    ("flush worker (comm torus-flush-wor, all)", "torus-flush-wor"),
]
METRICS = [
    "phase1_actions",
    "cache_flush",
    "save_books",
    "save_books_drain",
    "save_books_write",
    "phase_margin",
    "phase_match",
    "phase_settle",
    "engine",
    "chain",
    "end_resident",
    "end_resident_wait",
    "verify",
    "block",
    "resident_rows_build",
]


def met(f):
    m = {}
    for line in open(f):
        p = line.split()
        if len(p) >= 2 and not line.startswith("#"):
            try:
                m[p[0]] = float(p[1])
            except ValueError:
                pass
    return m


def snaps(d, tag):
    pre = "prof-" if tag == "load" else "prof-drain-"
    return pre + "before", pre + "after"


def window_base(d, tag):
    b0, b1 = snaps(d, tag)
    a, b = (
        met(f"{d}/{b0.replace('prof-', 'prof-metrics-', 1)}.txt"),
        met(f"{d}/{b1.replace('prof-', 'prof-metrics-', 1)}.txt"),
    )
    span = float(open(f"{d}/{b1}.ts").read()) - float(open(f"{d}/{b0}.ts").read())
    sa = (
        open(f"{d}/{b0.replace('prof-', 'prof-stat-', 1)}.txt")
        .read()
        .split(")")[1]
        .split()
    )
    sb = (
        open(f"{d}/{b1.replace('prof-', 'prof-stat-', 1)}.txt")
        .read()
        .split(")")[1]
        .split()
    )
    ut = (int(sb[11]) - int(sa[11])) / 100
    st = (int(sb[12]) - int(sa[12])) / 100
    dl = lambda k: b.get(k, math.nan) - a.get(k, math.nan)
    return a, b, dl, span, ut, st


def folded_path(d, tag):
    return f"{d}/perf.folded" if tag == "load" else f"{d}/perf.drain.folded"


def r(x, n=3):
    return (
        None if x is None or (isinstance(x, float) and math.isnan(x)) else round(x, n)
    )


def cmd_window(d, tag):
    a, b, dl, span, ut, st = window_base(d, tag)
    fills, nblk, blk = (
        dl("torus_orders_matched_total"),
        dl("torus_exec_native_blocks_total"),
        dl("torus_block_height"),
    )
    PHASE = phase_rules()
    tot = 0
    exec_tot = 0
    inc = Counter()
    phase = Counter()
    anyc = Counter()
    scan = 0
    cancel_child = Counter()
    flush_child = Counter()
    for line in open(folded_path(d, tag)):
        s, n = line.rsplit(" ", 1)
        n = int(n)
        fr = s.split(";")
        tot += n
        for name, pfx in ANY_COMM:
            if fr[0].startswith(pfx):
                anyc[name] += n
        if not fr[0].startswith("torus-execution"):
            continue
        exec_tot += n
        for name, keys in INCL:
            if keys and any(any(k in x for k in keys) for x in fr[1:]):
                inc[name] += n
        for name, keys in PHASE:
            if any(any(k in x for k in keys) for x in fr[1:]):
                phase[name] += n
                break
        ci = [i for i, x in enumerate(fr) if any(k in x for k in CANCEL)]
        if ci:
            below = fr[ci[-1] + 1 :]
            nxt = below[0] if below else "(self)"
            cancel_child[nxt[:120]] += n
            if not below or any(any(k in x for k in SCAN) for x in below):
                scan += n
        fi = [i for i, x in enumerate(fr) if "flush_all" in x or "add_cum_volume" in x]
        if fi:
            below = fr[fi[0] + 1 :]
            flush_child[(below[0] if below else "(self)")[:120]] += n
    K1k = lambda v: v / tot * ut / fills * 1e6 if fills and fills > 0 else None
    Kblk = lambda v: v / tot * ut * 1e3 / nblk if nblk and nblk > 0 else None
    row = lambda v: {
        "ms_per_blk": r(Kblk(v)),
        "ms_per_1k": r(K1k(v)),
        "share_exec": r(v / exec_tot, 4) if exec_tot else None,
    }
    out = {
        "cell": d,
        "window": tag,
        "span_s": r(span, 1),
        "fills": fills,
        "native_blocks": nblk,
        "blocks": blk,
        "fills_per_native_blk": r(fills / nblk, 0) if nblk else None,
        "utime_s": ut,
        "stime_s": st,
        "exec_ms_per_blk": r(Kblk(exec_tot)),
        "exec_ms_per_1k": r(K1k(exec_tot)),
        "perf_inclusive": {name: row(inc[name]) for name, keys in INCL if keys},
        "cancel_modify_scan_part": row(scan),
        "buckets2_phase": {k: row(v) for k, v in phase.most_common()},
        "margin_total (buckets2 margin*)": row(
            sum(v for k, v in phase.items() if k.startswith("margin"))
        ),
        "other_comms": {k: row(v) for k, v in anyc.items()},
        "cancel_modify_children_ms_per_blk": {
            k: r(Kblk(v)) for k, v in cancel_child.most_common(10)
        },
        "cache_flush_children_ms_per_blk": {
            k: r(Kblk(v)) for k, v in flush_child.most_common(10)
        },
        "metrics": {},
    }
    for m in METRICS:
        sm, cn = dl(f"torus_exec_{m}_seconds_sum"), dl(f"torus_exec_{m}_seconds_count")
        if math.isnan(sm):
            out["metrics"][m] = None
            continue
        out["metrics"][m] = {
            "ms_per_obs": r(sm * 1e3 / cn) if cn else None,
            "obs": cn,
            "ms_per_native_blk": r(sm * 1e3 / nblk) if nblk else None,
            "ms_per_1k": r(sm * 1e6 / fills) if fills and fills > 0 else None,
        }
    for f in ("perf.tids.json" if tag == "load" else "perf.drain.tids.json",):
        if os.path.exists(f"{d}/{f}"):
            t = json.load(open(f"{d}/{f}"))
            out["perf_tids"] = {
                "span_s": t["span_s"],
                "all_distinct": t["all_distinct_tids"],
                **{
                    g: {
                        "distinct": v["distinct_tids"],
                        "per_min": v["tids_per_min"],
                        "lt_1s": v["short_lived_lt_1s"],
                    }
                    for g, v in t["groups"].items()
                },
            }
    b0, b1 = snaps(d, tag)
    if os.path.exists(f"{d}/{b0}.lastpid") and os.path.exists(f"{d}/{b1}.lastpid"):
        lp = int(open(f"{d}/{b1}.lastpid").read()) - int(
            open(f"{d}/{b0}.lastpid").read()
        )
        out["pids_allocated_per_min_systemwide"] = r(lp / span * 60, 0)
    tb = open(f"{d}/{b0.replace('prof-', 'prof-tasks-', 1)}.txt").read().split("\n")
    ta = open(f"{d}/{b1.replace('prof-', 'prof-tasks-', 1)}.txt").read().split("\n")
    out["threads_alive_before_after"] = [
        len([x for x in tb if x.strip()]),
        len([x for x in ta if x.strip()]),
    ]
    json.dump(out, open(f"{d}/p2s0-{tag}.json", "w"), indent=1)
    print(
        f"== {os.path.basename(d)} {tag} window: {out['span_s']} s, fills {fills:.0f}, native blocks {nblk:.0f} "
        f"({out['fills_per_native_blk']} fills/blk), utime {ut} s, exec {out['exec_ms_per_blk']} ms/blk "
        f"{out['exec_ms_per_1k']} ms/1k"
    )
    print(f"  {'item':72s} {'ms/blk':>8s} {'ms/1k':>8s}")
    pr = lambda name, x: print(
        f"  {name[:72]:72s} {str(x['ms_per_blk']):>8s} {str(x['ms_per_1k']):>8s}"
    )
    for name, x in out["perf_inclusive"].items():
        pr(name, x)
    pr(
        "  cancel/modify book-scan part (self / get_order / book cancel_order / hashbrown)",
        out["cancel_modify_scan_part"],
    )
    pr("margin total (buckets2 margin:* rules)", out["margin_total (buckets2 margin*)"])
    for k, x in out["buckets2_phase"].items():
        pr("buckets2: " + k, x)
    for k, x in out["other_comms"].items():
        pr(k, x)
    print("  metrics (prof snapshot deltas): ms/obs | ms/native blk | ms/1k")
    for m, x in out["metrics"].items():
        if x:
            print(
                f"    torus_exec_{m}_seconds {x['ms_per_obs']} | {x['ms_per_native_blk']} | {x['ms_per_1k']}  (n={x['obs']:.0f})"
            )
    print(
        "  cancel/modify children ms/blk:",
        json.dumps(out["cancel_modify_children_ms_per_blk"])[:600],
    )
    print(
        "  cache flush children ms/blk:",
        json.dumps(out["cache_flush_children_ms_per_blk"])[:600],
    )
    if "perf_tids" in out:
        print("  perf thread ids:", json.dumps(out["perf_tids"]))
    if "pids_allocated_per_min_systemwide" in out:
        print(
            "  pids allocated per minute (system-wide, /proc/loadavg):",
            out["pids_allocated_per_min_systemwide"],
        )
    print(
        "  val0 threads alive at window start/end:", out["threads_alive_before_after"]
    )


def cmd_threads(d):
    s = json.load(open(d + "/summary.json"))
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    seen = defaultdict(lambda: defaultdict(set))  # group -> minute -> tids
    comm = {}
    alive = defaultdict(list)
    per_ts = defaultdict(Counter)
    for line in open(d + "/tasks.txt"):
        ts, tid, c, _, _ = line.split()
        ts = int(ts)
        if tid == "PROC" or not (t0 <= ts <= t1):
            continue
        g = (
            c.split("-")[0]
            if not c.startswith(("torus-", "rpc-", "tokio-"))
            else c[:15]
        )
        comm[tid] = g
        seen[g][(ts - t0) // 60].add(tid)
        per_ts[ts][g] += 1
    for ts, cc in per_ts.items():
        for g, n in cc.items():
            alive[g].append(n)
    print(
        f"== {os.path.basename(d)} thread ids per minute, load window {t1 - t0} s (tasks.txt 1 Hz: lower bound)"
    )
    for g in sorted(seen, key=lambda g: -sum(len(v) for v in seen[g].values())):
        allt = set().union(*seen[g].values())
        print(
            f"  {g:18s} distinct {len(allt):6d}  per min {len(allt) / (t1 - t0) * 60:8.1f}  per minute bins "
            f"{[len(seen[g][k]) for k in sorted(seen[g])]}  alive mean {sum(alive[g]) / len(alive[g]):6.1f}"
        )


def ts_utc(s):
    return (
        datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%S.%f")
        .replace(tzinfo=datetime.timezone.utc)
        .timestamp()
    )


def blocks_from_log(path):
    start, done, nat, cnt = {}, {}, {}, {}
    hh = re.compile(r"height=(\d+)")
    nc = re.compile(r"native_count=(\d+)")
    for line in gzip.open(path, "rt", errors="replace"):
        if "execution pipeline" not in line:
            continue
        line = ANSI.sub("", line)
        m, h = TS.match(line), hh.search(line)
        if not m or not h:
            continue
        t, h = ts_utc(m.group(1)), int(h.group(1))
        if "executing finalized block" in line:
            start[h] = t
            nat[h] = "has_native=true" in line
            c = nc.search(line)
            cnt[h] = int(c.group(1)) if c else -1
        elif "block done" in line:
            done[h] = t
    return [(h, start[h], done[h], nat[h], cnt[h]) for h in sorted(start) if h in done]


def klass(nat, cnt):
    if not nat:
        return "empty"
    return "oracle" if 0 <= cnt <= FEED_MAX else "load"


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))] if v else float("nan")


def st(v):
    return (
        f"n {len(v):3d} mean {sum(v) / len(v):8.2f} p50 {pct(v, 50):8.2f} p90 {pct(v, 90):8.2f} max {max(v):8.2f}"
        if v
        else "n   0"
    )


def samples(path):
    out = []  # (comm, tid, t, period, frames outer->leaf)
    hdr = re.compile(r"^(.*?)\s+(\d+)\s+(\d+\.\d+):\s+(\d+)")
    cur = None
    fr = []
    for line in gzip.open(path, "rt"):
        if not line.strip():
            if cur:
                out.append(cur + (list(reversed(fr)),))
            cur, fr = None, []
            continue
        if line[0] not in " \t":
            m = hdr.match(line)
            cur = (
                (
                    m.group(1).strip(),
                    int(m.group(2)),
                    float(m.group(3)),
                    int(m.group(4)),
                )
                if m
                else None
            )
            continue
        p = line.strip().split(" ", 1)
        if len(p) > 1 and not p[1].startswith("[unknown]"):
            fr.append(H.sub("", p[1]))
    if cur:
        out.append(cur + (list(reversed(fr)),))
    return out


def child(fr):
    for i, f in enumerate(fr):
        if "execute_committed_block_with" in f:
            return fr[i + 1] if i + 1 < len(fr) else "(self)"
    for i, f in enumerate(fr):
        if "execution_loop" in f:
            return "LOOP:" + (fr[i + 1] if i + 1 < len(fr) else "(self)")
    return "OTHER:" + (fr[-1] if fr else "?")


def top(sm, MS, nblk, label, n=12):
    inc, slf, ch = Counter(), Counter(), Counter()
    for c, tid, t, per, fr in sm:
        for x in set(fr):
            inc[x] += per
        if fr:
            slf[fr[-1]] += per
        ch[child(fr)] += per
    print(f"    top functions, {label} (ms per block over {nblk} block(s)):")
    for title, cc in (
        ("child of execute_committed_block_with", ch),
        ("inclusive", inc),
        ("self", slf),
    ):
        print(f"      -- {title}")
        for k, v in cc.most_common(n):
            print(
                f"      {v * MS / nblk:8.3f} {re.sub(r'::{closure#[0-9]+}', '{cl}', k)[:150]}"
            )


def cmd_timeline(d, tag):
    pre = "perf" if tag == "load" else "perf.drain"
    b0, b1 = snaps(d, tag)
    clock = (
        open(f"{d}/{pre}.clock").read().strip()
        if os.path.exists(f"{d}/{pre}.clock")
        else "missing"
    )
    rt0, mono0 = map(float, open(f"{d}/{b0}.clk").read().split())
    rt1, mono1 = map(float, open(f"{d}/{b1}.clk").read().split())
    off = rt0 - mono0
    w0, w1 = float(open(f"{d}/{b0}.ts").read()), float(open(f"{d}/{b1}.ts").read())
    print(
        f"== {os.path.basename(d)} {tag} timeline: window {datetime.datetime.fromtimestamp(w0):%T}-"
        f"{datetime.datetime.fromtimestamp(w1):%T} ({w1 - w0:.1f} s), perf clock {clock}, "
        f"realtime-monotonic drift over the window {((rt1 - mono1) - off) * 1e3:.2f} ms, oracle-only = native_count <= {FEED_MAX}"
    )
    blk = {v: blocks_from_log(f"{d}/val{v}.log.gz") for v in (0, 1, 2)}
    t_end = None
    if os.path.exists(f"{d}/summary.json"):
        t_end = json.load(open(f"{d}/summary.json"))["timing"].get("t_bench1")
    if tag == "drain":
        t_end = w0  # the sidecar snapped at the "bench exited" line (+ <= 0.2 s)
    # row 77 per node, log only: first oracle-only block after the last load block that started after bench end
    for v in (0, 1, 2):
        B = [x for x in blk[v] if x[2] >= (t_end or 0) - 1]
        if tag != "drain" or not B:
            break
        lastload = max(
            (i for i, x in enumerate(B) if klass(x[3], x[4]) == "load"), default=-1
        )
        orc = [x for x in B[lastload + 1 :] if klass(x[3], x[4]) == "oracle"]
        steady = [(x[2] - x[1]) * 1e3 for x in orc[1:]]
        first = orc[0] if orc else None
        print(
            f"  val{v}: load blocks after bench exit {sum(1 for x in B if klass(x[3], x[4]) == 'load')}, last load block "
            + (f"done +{(B[lastload][2] - t_end):.2f} s" if lastload >= 0 else "none")
            + "; first oracle-only block "
            + (
                f"h={first[0]} n={first[4]} wall {(first[2] - first[1]) * 1e3:.2f} ms at +{first[1] - t_end:.2f} s"
                if first
                else "none"
            )
            + f"; later oracle-only blocks wall ms {st(steady)}; empty (no native) blocks wall ms "
            f"{st([(x[2] - x[1]) * 1e3 for x in B[lastload + 1 :] if not x[3]])}"
        )
    if clock != "CLOCK_MONOTONIC":
        print(
            f"  perf clock is {clock}: no per-block perf attribution (log-only numbers above)"
        )
        return
    S = samples(f"{d}/{pre}.exec.script.gz")
    if not S:
        print("  no exec samples")
        return
    ptot = sum(int(line.rsplit(" ", 1)[1]) for line in open(folded_path(d, tag)))
    sa = (
        open(f"{d}/{b0.replace('prof-', 'prof-stat-', 1)}.txt")
        .read()
        .split(")")[1]
        .split()
    )
    sb = (
        open(f"{d}/{b1.replace('prof-', 'prof-stat-', 1)}.txt")
        .read()
        .split(")")[1]
        .split()
    )
    MS = (
        (int(sb[11]) - int(sa[11])) * 10 / ptot
    )  # ms per period unit (process utime / all samples' period)
    S = [(c, tid, t + off, per, fr) for c, tid, t, per, fr in S]
    S.sort(key=lambda x: x[2])
    main = Counter(x[1] for x in S if x[0].startswith("torus-execution")).most_common(
        1
    )[0][0]
    T = [x[2] for x in S]
    pmin, pmax = T[0], T[-1]
    rows = []
    for h, a, b, nat, cnt in blk[0]:
        if a < pmin or b > pmax:
            continue
        i0, i1 = bisect.bisect_left(T, a), bisect.bisect_right(T, b)
        ss = S[i0:i1]
        mcpu = sum(x[3] for x in ss if x[1] == main) * MS
        acpu = sum(x[3] for x in ss) * MS
        rows.append(
            dict(
                h=h,
                a=a,
                b=b,
                k=klass(nat, cnt),
                n=cnt,
                wall=(b - a) * 1e3,
                mcpu=mcpu,
                acpu=acpu,
                ss=ss,
            )
        )
    print(
        f"  val0 exec main tid {main}; {len(rows)} blocks fully inside the perf window "
        f"(perf samples {datetime.datetime.fromtimestamp(pmin):%T.%f}-{datetime.datetime.fromtimestamp(pmax):%T.%f})"
    )
    for k in ("load", "oracle", "empty"):
        R = [x for x in rows if x["k"] == k]
        print(f"  {k:6s} wall ms {st([x['wall'] for x in R])}")
        print(
            f"  {'':6s} main-thread CPU ms {st([x['mcpu'] for x in R])}; exec-side threads CPU ms {st([x['acpu'] for x in R])}"
        )
    if tag == "drain":
        print(
            "  first 25 blocks after bench exit (val0): h class native_count wall_ms main_cpu_ms exec_side_cpu_ms start_s"
        )
        for x in [x for x in rows if x["a"] >= t_end - 1][:25]:
            print(
                f"    {x['h']:6d} {x['k']:6s} {x['n']:6d} {x['wall']:8.2f} {x['mcpu']:8.2f} {x['acpu']:8.2f} +{x['a'] - t_end:6.2f}"
            )
        lastload = max((i for i, x in enumerate(rows) if x["k"] == "load"), default=-1)
        orc = [x for x in rows[lastload + 1 :] if x["k"] == "oracle"]
        if orc:
            f = orc[0]
            print(
                f"  row 77: first oracle-only block after the load: h={f['h']} wall {f['wall']:.2f} ms, main CPU {f['mcpu']:.2f} ms, "
                f"exec-side CPU {f['acpu']:.2f} ms, {sum(1 for x in f['ss'] if x[1] == main)} main-thread samples"
            )
            top(
                [x for x in f["ss"] if x[1] == main],
                MS,
                1,
                f"row 77 block h={f['h']}, main thread",
            )
            top(
                [x for x in f["ss"] if x[1] != main],
                MS,
                1,
                f"row 77 block h={f['h']}, other exec-side threads",
                6,
            )
        if len(orc) > 1:
            st78 = orc[1:]
            ss = [s for x in st78 for s in x["ss"] if s[1] == main]
            print(
                f"  row 78: oracle-only blocks with the feed live after the first: {len(st78)} blocks, wall ms "
                f"{st([x['wall'] for x in st78])}; main CPU ms {st([x['mcpu'] for x in st78])}; {len(ss)} samples"
            )
            top(ss, MS, len(st78), "row 78 oracle-only blocks, main thread")
            emp = [x for x in rows[lastload + 1 :] if x["k"] == "empty"]
            if emp:
                ss = [s for x in emp for s in x["ss"] if s[1] == main]
                print(
                    f"  no-native blocks after the load: {len(emp)}, wall ms {st([x['wall'] for x in emp])}; {len(ss)} samples"
                )
                top(ss, MS, len(emp), "no-native blocks, main thread", 6)
        # gaps between blocks (exec thread busy outside execute_committed_block_with?)
        gap = [
            s
            for s in S
            if s[1] == main
            and not any(x["a"] <= s[2] <= x["b"] for x in rows[lastload + 1 :])
            and s[2] > (rows[lastload]["b"] if lastload >= 0 else t_end)
        ]
        if gap:
            print(
                f"  main-thread samples between blocks after the load: {sum(s[3] for s in gap) * MS:.2f} ms total"
            )
            top(gap, MS, 1, "main thread outside blocks after the load (total ms)", 6)
    else:
        L = [x for x in rows if x["k"] == "load"]
        if L:
            top(
                [s for x in L for s in x["ss"] if s[1] == main],
                MS,
                len(L),
                "load blocks, main thread",
                15,
            )


def cmd_rebuild(d):
    print(f"== {os.path.basename(d)} R rebuild (row 7)")
    pat = re.compile(
        r"resident rows built height=(\d+) rows=(\d+) bytes=(\d+) build_ms=([0-9.]+)"
    )
    for v in (0, 1, 2):
        p = f"{d}/val{v}.log.gz"
        if not os.path.exists(p):
            continue
        n = 0
        for line in gzip.open(p, "rt", errors="replace"):
            line = ANSI.sub("", line)
            m = pat.search(line)
            if m:
                h, rows, by, ms = (
                    int(m.group(1)),
                    int(m.group(2)),
                    int(m.group(3)),
                    float(m.group(4)),
                )
                print(
                    f"  val{v} {line[:26]} height={h} rows={rows} bytes={by} build_ms={ms:.1f} "
                    f"({ms / rows * 1e6 / 1e3:.2f} s per 1M rows)"
                    if rows
                    else f"  val{v} {line[:26]} rows=0"
                )
                n += 1
            elif "rebuilding R" in line or "resident rows stale" in line:
                print(f"  val{v} STALE/REBUILD: {line.strip()[:200]}")
        if n == 0:
            print(f"  val{v}: no 'resident rows built' line (main arm: no R)")
    p = f"{d}/metrics-after-val0.txt"
    if os.path.exists(p):
        m = met(p)
        print(
            f"  val0 metrics after: rebuilds={m.get('torus_exec_resident_rows_rebuilds_total')} "
            f"build_s_sum={m.get('torus_exec_resident_rows_build_seconds_sum')} rows={m.get('torus_exec_resident_rows')} "
            f"(R at cell end: a restart would rebuild this many rows; row 7: 2.07 s per 1M rows -> "
            f"~{(m.get('torus_exec_resident_rows') or 0) / 1e6 * 2.07:.2f} s)"
        )


def cmd_compare(da, db):
    A, B = (
        json.load(open(da + "/p2s0-load.json")),
        json.load(open(db + "/p2s0-load.json")),
    )
    print(
        f"== load windows: A={os.path.basename(da)} B={os.path.basename(db)}   (ms/1k fills | ms/native blk; B/A)"
    )
    print(
        f"  fills/s A {A['fills'] / A['span_s']:.0f} B {B['fills'] / B['span_s']:.0f}; fills/native blk A {A['fills_per_native_blk']} B {B['fills_per_native_blk']}"
    )

    def line(name, x, y):
        f = lambda v: "-" if v is None else f"{v:8.3f}"
        q = lambda a, b: "-" if not a or b is None else f"{b / a:5.2f}x"
        print(
            f"  {name[:64]:64s} A {f(x['ms_per_1k'])} | {f(x['ms_per_blk'])}   B {f(y['ms_per_1k'])} | {f(y['ms_per_blk'])}   "
            f"B/A {q(x['ms_per_1k'], y['ms_per_1k'])} {q(x['ms_per_blk'], y['ms_per_blk'])}"
        )

    line(
        "exec thread total",
        {"ms_per_1k": A["exec_ms_per_1k"], "ms_per_blk": A["exec_ms_per_blk"]},
        {"ms_per_1k": B["exec_ms_per_1k"], "ms_per_blk": B["exec_ms_per_blk"]},
    )
    for k in ("buckets2_phase",):
        for name in sorted(
            set(A[k]) | set(B[k]),
            key=lambda n: -(A[k].get(n, {}).get("ms_per_1k") or 0),
        ):
            z = {"ms_per_1k": None, "ms_per_blk": None}
            line("buckets2: " + name, A[k].get(name, z), B[k].get(name, z))
    line(
        "margin total",
        A["margin_total (buckets2 margin*)"],
        B["margin_total (buckets2 margin*)"],
    )
    for name in A["perf_inclusive"]:
        line(
            name,
            A["perf_inclusive"][name],
            B["perf_inclusive"].get(name, {"ms_per_1k": None, "ms_per_blk": None}),
        )
    for m in (
        "phase_match",
        "phase_margin",
        "phase_settle",
        "phase1_actions",
        "cache_flush",
        "save_books",
        "engine",
        "chain",
    ):
        x, y = A["metrics"].get(m), B["metrics"].get(m)
        if x and y:
            line(
                "metric " + m,
                {"ms_per_1k": x["ms_per_1k"], "ms_per_blk": x["ms_per_native_blk"]},
                {"ms_per_1k": y["ms_per_1k"], "ms_per_blk": y["ms_per_native_blk"]},
            )


if __name__ == "__main__":
    c, args = sys.argv[1], sys.argv[2:]
    {
        "window": cmd_window,
        "threads": cmd_threads,
        "timeline": cmd_timeline,
        "rebuild": cmd_rebuild,
        "compare": cmd_compare,
    }[c](*args)
