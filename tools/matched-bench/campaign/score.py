#!/usr/bin/env python3
"""Campaign scoring: throughput + latency, per cell, per arm, and paired per round.

Usage: score.py [CAMPAIGN_DIR] [LABEL ...]
  CAMPAIGN_DIR defaults to $CAMPAIGN_DIR, else the current directory; cells =
  progress.tsv rows (arm "warmup" excluded) unless LABELs are given. Labels are
  <prefix>-<arm>-<round>, round = rN (full load) or loK (low load).
  Results are read from $RESULTS_ROOT/<label> (default ~/bench-results-matched).

Per cell, from <results>/<label>/summary.json (all values = mean over val0..2):
  matched/s        headline matched_s_avg
  CPU-s/1M         node CPU-s per 1M matched over [t_bench0, t_drain): pidstat
                   PROCESS rows (TID '-') of <campaign>/<label>-host/threads.log
                   (per-thread rows are ignored), matched delta from sampler.csv,
                   divided by 3 nodes
  rej              funnel_by_node delta_orders_rejected_open_limit_total, and as %
                   of place attempts (rejected + delta_orders_placed_accepted_total)
  vd / vvd         consensus_by_node view_duration_ms / view_vote_delay_ms, whole run
                   and the `load` sub-object (bench window, suffix L)
  ci50 / ci95      phase_by_node commit_interval_ms_p50/p95 (load window)
  age              phase_by_node order_age_ms {admit,commit,exec,fills_visible} p50/p99
Paired deltas: each arm vs the baseline arm of the same round. The baseline is the
one arm whose name starts with "main" that shares the arm's rounds (main for rN,
e.g. mainlo for loK), so name the baseline arm main*. Push effect: XYZpush vs XYZ
as a ratio of arm means (different rounds, so not paired).
"""

import csv
import datetime
import json
import os
import pathlib
import statistics as st
import sys

R = pathlib.Path(
    os.environ.get("RESULTS_ROOT") or pathlib.Path.home() / "bench-results-matched"
)
STAGES = ("admit", "commit", "exec", "fills_visible")
LAT = [
    ("vd", "vd"),
    ("vd_load", "vdL"),
    ("vvd", "vvd"),
    ("vvd_load", "vvdL"),
    ("ci50", "ci50"),
    ("ci95", "ci95"),
]
LAT += [
    (f"{sg}_{q}", ("fill" if sg == "fills_visible" else sg[:4]) + q[1:])
    for sg in STAGES
    for q in ("p50", "p99")
]


def mean(xs):
    xs = [x for x in xs if x is not None]
    return sum(xs) / len(xs) if xs else None


def counter_delta(d, t0, t1):
    ks = sorted(d)
    a = min(ks, key=lambda k: abs(k - t0))
    b = min(ks, key=lambda k: abs(k - t1))
    return d[b] - d[a]


def node_cpu_per_1m(camp, lab, s):
    """Node CPU-s per 1M matched (per node), or None if the host logs are missing."""
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_drain"]
    host = camp / f"{lab}-host" / "threads.log"
    samp = R / lab / "sampler.csv"
    if not host.exists() or not samp.exists():
        return None
    m = {}
    for x in csv.DictReader(open(samp)):
        m.setdefault(x["node"], {})[int(x["ts"])] = float(
            x["torus_orders_matched_total"]
        )
    matched = sum(counter_delta(d, t0, t1) for d in m.values()) / len(m)
    day = datetime.datetime.fromtimestamp(t0).date()
    node = 0.0
    for line in open(host):
        f = line.split()
        if len(f) < 13 or not f[0][:2].isdigit() or f[1] == "UID":
            continue
        ts = datetime.datetime.combine(
            day, datetime.time.fromisoformat(f[0])
        ).timestamp()
        if ts < t0 - 43200:  # cell crossed midnight
            ts += 86400
        if t0 <= ts < t1 and f[3] == "-":  # process rows (TID '-'), %CPU column
            node += float(f[8]) / 100
    return node / 3 / (matched / 1e6) if matched else None


def cell(camp, lab):
    s = json.load(open(R / lab / "summary.json"))
    h = s["headline"]
    cons = s.get("consensus_by_node") or {}
    ph = s.get("phase_by_node") or {}
    fu = s.get("funnel_by_node") or {}
    r = dict(
        label=lab,
        agree=h.get("agreement_verdict"),
        live=h.get("liveness_verdict"),
        ms=h.get("matched_s_avg"),
        blk=h.get("blk_s_avg"),
        cpu=node_cpu_per_1m(camp, lab, s),
    )
    rej = mean([f.get("delta_orders_rejected_open_limit_total") for f in fu.values()])
    acc = mean([f.get("delta_orders_placed_accepted_total") for f in fu.values()])
    r["rej"] = rej
    r["rej_pct"] = 100 * rej / (rej + acc) if rej is not None and acc else None
    for k, key in (("vd", "view_duration_ms"), ("vvd", "view_vote_delay_ms")):
        r[k] = mean([c.get(key) for c in cons.values()])
        r[k + "_load"] = mean([(c.get("load") or {}).get(key) for c in cons.values()])
    r["ci50"] = mean([p.get("commit_interval_ms_p50") for p in ph.values()])
    r["ci95"] = mean([p.get("commit_interval_ms_p95") for p in ph.values()])
    for sg in STAGES:
        for q in ("p50", "p99"):
            r[f"{sg}_{q}"] = mean(
                [
                    ((p.get("order_age_ms") or {}).get(sg) or {}).get(q)
                    for p in ph.values()
                ]
            )
    return r


def fmt(v, w=7, d=1):
    return f"{v:{w}.{d}f}" if isinstance(v, (int, float)) else f"{'-':>{w}s}"


def rnd_key(r):
    return (r.startswith("lo"), int(r.lstrip("rlo")))


def mean_sd(xs, k, d=1):
    v = [x[k] for x in xs if x[k] is not None]
    if not v:
        return "-"
    return f"{st.mean(v):.{d}f}" + (f" ({st.stdev(v):.{d}f})" if len(v) > 1 else "")


def print_cells(res):
    cells = sorted(res.items(), key=lambda kv: (rnd_key(kv[0][1]), kv[1]["label"]))
    print(
        "== per cell: throughput (CPU/1M = node CPU-s per 1M matched; "
        "rej = open-limit rejects, mean/node; rej% of place attempts)"
    )
    print(
        f"{'label':28s} {'agree':6s} {'live':5s} {'match/s':>8s} {'blk/s':>6s} "
        f"{'CPU/1M':>7s} {'rej':>9s} {'rej%':>5s}"
    )
    for _, x in cells:
        print(
            f"{x['label']:28s} {str(x['agree']):6s} {str(x['live']):5s} {fmt(x['ms'], 8, 0)} "
            f"{fmt(x['blk'], 6, 2)} {fmt(x['cpu'])} {fmt(x['rej'], 9, 0)} {fmt(x['rej_pct'], 5, 2)}"
        )
    print(
        "\n== per cell: latency ms (vd/vvd = view_duration/view_vote_delay whole run, "
        "L = load window; ci = commit_interval; admi/comm/exec/fill = order_age)"
    )
    print(f"{'label':28s} " + " ".join(f"{h:>7s}" for _, h in LAT))
    for _, x in cells:
        print(f"{x['label']:28s} " + " ".join(fmt(x[k]) for k, _ in LAT))


def print_arms(res, arms):
    print("\n== per arm: mean (sd) over cells")
    for a in arms:
        xs = [v for k, v in res.items() if k[0] == a]
        print(
            f"{a:7s} n={len(xs)} matched/s {mean_sd(xs, 'ms', 0)} | CPU-s/1M {mean_sd(xs, 'cpu')} "
            f"| rej {mean_sd(xs, 'rej', 0)} ({mean_sd(xs, 'rej_pct', 2)}%)"
        )
        print("        " + " | ".join(f"{h} {mean_sd(xs, k)}" for k, h in LAT))


def baseline(res, a):
    """The main-binary arm that shares a's rounds (main / mainlo / mainlopush)."""
    rounds = {r for (b, r) in res if b == a}
    bases = {b for (b, r) in res if r in rounds and b.startswith("main")}
    return bases.pop() if len(bases) == 1 else None


def print_deltas(pairs, title):
    """pairs: list of (label, [(x_cell, base_cell), ...], tags)."""
    print(title)
    for name, cells, tags in pairs:
        out = []
        for k, h in [("ms", "matched/s"), ("cpu", "CPU/1M"), ("rej", "rej")] + LAT:
            d = [x[k] / b[k] - 1 for x, b in cells if x[k] is not None and b[k]]
            if d:
                out.append(
                    f"{h} {st.mean(d) * 100:+.1f}% [{' '.join(f'{v * 100:+.0f}' for v in d)}]"
                )
        print(f"{name} ({tags}):")
        for i in range(0, len(out), 4):
            print("   " + " | ".join(out[i : i + 4]))


def print_paired(res, arms):
    pairs = []
    for a in arms:
        base = baseline(res, a)
        if a.startswith("main") or base is None:
            continue
        rnds = sorted((r for (b, r) in res if b == a and (base, r) in res), key=rnd_key)
        pairs.append(
            (
                f"{a} vs {base}",
                [(res[(a, r)], res[(base, r)]) for r in rnds],
                ",".join(rnds),
            )
        )
    print_deltas(
        pairs,
        "\n== paired deltas vs the main-binary arm of the same round "
        "(main for rN, mainlo / mainlopush for loK): mean % [per round]",
    )
    # Push effect (s84-5arm): XYZlopush vs XYZlo, different rounds -> NOT paired;
    # ratio of arm means, shown as one number per metric.
    push = []
    for a in arms:
        if a.endswith("push") and a[: -len("push")] in arms:

            def avg(arm):
                xs = [v for (b, _), v in res.items() if b == arm]
                return {
                    k: mean([x[k] for x in xs])
                    for k in xs[0]
                    if k not in ("label", "agree", "live")
                }

            push.append(
                (
                    f"{a} vs {a[: -len('push')]}",
                    [(avg(a), avg(a[: -len("push")]))],
                    "arm means, unpaired",
                )
            )
    if push:
        print_deltas(
            push, "\n== push effect (TORUS_BODY_PUSH_MAX_BYTES): ratio of arm means - 1"
        )


def main():
    args = sys.argv[1:]
    if args and pathlib.Path(args[0]).is_dir():
        camp = pathlib.Path(args.pop(0))
    else:
        camp = pathlib.Path(os.environ.get("CAMPAIGN_DIR") or ".").resolve()
    labels = args or [
        ln.split("\t")[0]
        for ln in open(camp / "progress.tsv").read().splitlines()[1:]
        if ln.strip()
    ]
    res = {}
    for lab in labels:
        _, arm, rnd = lab.rsplit("-", 2)
        if arm != "warmup" and (R / lab / "summary.json").exists():
            res[(arm, rnd)] = cell(camp, lab)
    if not res:
        sys.exit("no scored cells")
    arms = list(dict.fromkeys(a for a, _ in res))
    print_cells(res)
    print_arms(res, arms)
    print_paired(res, arms)


if __name__ == "__main__":
    main()
