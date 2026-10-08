#!/usr/bin/env python3
"""ozarchy-mif-analysis.py [tag ...]: tables for the max-in-flight sweep (ozarchy-mif-300m-<tag>).
Reads summary.json, metrics-before/after-val*.txt, sampler.csv, buckets.csv, cpu-ticks.txt, bench.log, *.cell.rc."""

import csv, json, os, re, statistics, sys

R = "/home/oz/bench-results-matched/"
P = "ozarchy-mif-300m-"
DEF = [
    "warm",
    "base",
    "tailcost",
    "base-b900",
    "n1",
    "n1-b900",
    "n2",
    "n2-b900",
    "n4",
    "n4-b900",
]
REJ = ["margin", "open_limit", "book", "cancelled", "other"]
TCK = os.sysconf("SC_CLK_TCK")


def rd(p):
    try:
        return open(p, errors="replace").read()
    except FileNotFoundError:
        return ""


def mval(text, name):
    for l in text.splitlines():
        p = l.split()
        if p and p[0] == name:
            return float(p[1])
    return None


def mdelta(lab, v, name):
    a = mval(rd(f"{R}{lab}/metrics-after-val{v}.txt"), name)
    b = mval(rd(f"{R}{lab}/metrics-before-val{v}.txt"), name)
    return None if a is None else a - (b or 0)


def hq(pairs, q):
    total = pairs[-1][1] if pairs else 0
    if total <= 0:
        return None
    t, pl, pc = q * total, 0.0, 0.0
    for le, c in pairs:
        if c >= t:
            if le == float("inf"):
                return pl
            return le if c <= pc else pl + (le - pl) * (t - pc) / (c - pc)
        pl, pc = le, c
    return pl


def age_commit(lab, lo, hi):
    first, last = {}, {}
    try:
        for r in csv.DictReader(open(R + lab + "/buckets.csv")):
            if (
                r["node"] != "val0"
                or r["metric"] != "torus_order_age_commit_seconds_bucket"
            ):
                continue
            ts = float(r["ts"])
            if not lo <= ts <= hi:
                continue
            le = float("inf") if "inf" in r["le"].lower() else float(r["le"])
            first.setdefault(le, float(r["count"]))
            last[le] = float(r["count"])
    except FileNotFoundError:
        return None, None
    pairs = sorted((le, last[le] - first[le]) for le in last)
    f = lambda q: None if hq(pairs, q) is None else hq(pairs, q) * 1e3
    return f(0.5), f(0.95)


def gaps(lab, t0, t1):
    """val0 sampler rows in [t0,t1]: longest stretch (s) without a committed block / without a native block."""
    rows = [
        r
        for r in csv.DictReader(open(R + lab + "/sampler.csv"))
        if r["node"] == "val0" and t0 <= int(r["ts"]) <= t1
    ]
    out = []
    for k in ("torus_blocks_committed_total", "torus_exec_native_blocks_total"):
        best, last_t, last_v = 0, None, None
        for r in rows:
            v, t = float(r[k] or 0), int(r["ts"])
            if last_v is None or v > last_v:
                last_t, last_v = t, v
            best = max(best, t - last_t)
        out.append(best)
    return out


def cpu(lab, pids, t0, t1):
    tk = {}
    for l in rd(R + lab + "/cpu-ticks.txt").splitlines():
        ts, p, u, s = l.split()
        if t0 <= int(ts) <= t1:
            tk.setdefault(p, []).append(int(u) + int(s))
    return [((tk[p][-1] - tk[p][0]) / TCK if tk.get(p) else None) for p in pids]


def f(x, d=0, pct=False):
    if x is None:
        return "-"
    return f"{x * 100:.{d}f}%" if pct else f"{x:,.{d}f}"


def cell(tag):
    lab = P + tag
    c = {
        "tag": tag,
        "rc": rd(R + lab + ".cell.rc").strip().replace("rc=", "") or "missing",
    }
    try:
        s = json.load(open(R + lab + "/summary.json"))
    except Exception:
        return c
    h, ing, tm = s["headline"], s.get("ingest", {}), s["timing"]
    c.update(
        agree=h.get("agreement_verdict"),
        live=h.get("liveness_verdict"),
        mps=h["matched_s_avg"],
        best60=h["matched_s_best60"],
        nblk=h["native_blk_s"],
        cmax=s["cell"].get("max_in_flight"),
        bud=s["cell"].get("open_order_budget"),
        rmb=s["cell"].get("rpc_max_response_mb"),
    )
    rej = {k: mdelta(lab, 0, f"torus_orders_rejected_{k}_total") or 0 for k in REJ}
    acc = mdelta(lab, 0, "torus_orders_placed_accepted_total") or 0
    tot = acc + sum(rej.values())
    c["ol"] = rej["open_limit"] / tot if tot else None
    c["ol_n"], c["orders"] = rej["open_limit"], tot
    c["proc"] = mdelta(lab, 0, "torus_native_actions_processed_total")
    c["admrej"] = mdelta(
        lab, 0, 'torus_rpc_submit_admit_rejects_total{reason="backlog_preverify"}'
    )
    mix = ing.get("econ_mix") or {}
    c["place_sh"], c["ca_sh"] = mix.get("place_share"), mix.get("cancel_all_share")
    c["mix"] = mix
    c["rate"] = ing.get("bench_submit_rate")
    c["evict"] = ing.get("mempool_nonce_expired_evictions_per_node")
    c["inf"] = ing.get("in_flight")
    c["age"] = age_commit(lab, tm["t_bench0"], tm["t_drain"])
    c["gap_c"], c["gap_n"] = gaps(lab, tm["t_bench0"], tm["t_bench1"])
    c["cpu"] = cpu(lab, s["cell"]["node_pids"], tm["t_bench0"], tm["t_bench1"])
    c["win"] = tm["t_bench1"] - tm["t_bench0"]
    c["warn"] = "WARNING: in-flight block tail" in rd(R + lab + "/bench.log")
    return c


def main():
    tags = sys.argv[1:] or DEF
    cs = [cell(t) for t in tags]
    by = {c["tag"]: c for c in cs}
    base = by.get("base")
    print("## Per cell")
    print(
        "| cell | cap | budget | rc | AGREE | liveness | matched/s | best60 | native blk/s | max gap commit / native (s) | open_limit rej / orders | admit rejects (val0) | place / cancel-all share | submit rate (act/s) | val0 processed | age commit p50 / p95 (ms) | nonce-expired evictions v0/v1/v2 |"
    )
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        if "mps" not in c:
            print(f"| {c['tag']} | | | {c['rc']} | no summary |")
            continue
        a = c["age"]
        print(
            f"| {c['tag']} | {c['cmax'] or '-'} | {c['bud'] or '-'} | {c['rc']} | {c['agree']} | {c['live']} | {f(c['mps'])} | {f(c['best60'])} | {f(c['nblk'], 3)} | "
            f"{c['gap_c']} / {c['gap_n']} | {f(c['ol'], 2, True)} ({f(c['ol_n'])} / {f(c['orders'])}) | {f(c['admrej'])} | "
            f"{f(c['place_sh'], 1, True)} / {f(c['ca_sh'], 1, True)} | {f(c['rate'])} | {f(c['proc'])} | {f(a[0])} / {f(a[1])} | "
            f"{'/'.join(str(x) for x in c['evict'] or [])} |"
        )
    print("\n## In-flight cap (bench.log) and node CPU-s over the load window")
    print(
        "| cell | released committed / refused / timeout | in flight at end | tail fetched / errors / missed | WARNING | rpc_max_response_mb | load window s | CPU-s val0 / val1 / val2 | val2 / mean(val0,val1) |"
    )
    print("|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        if "mps" not in c:
            continue
        i = c["inf"] or {}
        cp = c["cpu"]
        r = cp[2] / statistics.mean(cp[:2]) if None not in cp else None
        rel = (
            f"{i['released_committed']:,} / {i['released_refused']:,} / {i['released_timeout']:,}"
            if i
            else "-"
        )
        tl = (
            f"{i['tail_fetched']} / {i['tail_errors']} / {i['tail_missed']}"
            if i
            else "-"
        )
        print(
            f"| {c['tag']} | {rel} | {i.get('in_flight_at_end', '-')} | {tl} | {'YES' if c['warn'] else 'no'} | {c['rmb'] or '-'} | {c['win']} | "
            f"{' / '.join(f(x) for x in cp)} | {f(r, 3)} |"
        )
    if base and "mps" in base:
        print("\n## Ratios vs base")
        print("| cell | matched/s | best60 | native blk/s | submit rate | val2 CPU-s |")
        print("|---|---|---|---|---|---|")
        for c in cs:
            if "mps" not in c or c["tag"] in ("warm",):
                continue
            q = lambda k: (
                f(c[k] / base[k], 3) + "x" if c.get(k) and base.get(k) else "-"
            )
            v2 = (
                f(c["cpu"][2] / base["cpu"][2], 3) + "x"
                if c["cpu"][2] and base["cpu"][2]
                else "-"
            )
            print(
                f"| {c['tag']} | {q('mps')} | {q('best60')} | {q('nblk')} | {q('rate')} | {v2} |"
            )


main()
