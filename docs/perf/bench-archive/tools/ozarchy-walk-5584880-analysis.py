#!/usr/bin/env python3
"""ozarchy-walk-5584880-analysis.py [label ...]: tables for the moving-price merge-gate campaign.
Defaults to the 7 ozarchy-walk-5584880-300m-* cells. Reads summary.json, run.log, drain.json, drain-feed-live.tsv,
drain-samples.jsonl, metrics-before/after-val*.txt, liq-drain.tsv, node-environ-trie.txt, val*.log.gz."""

import gzip, json, os, re, statistics, sys
from datetime import datetime, timezone

R = "/home/oz/bench-results-matched/"
P = "ozarchy-walk-5584880-300m-"
CELLS = [P + c for c in ("warm", "w10-r1", "w0-r1", "m-r1", "w10-r2", "w0-r2", "m-r2")]
ANSI = re.compile(r"\x1b\[[0-9;]*m")
EXE = re.compile(
    r"^(\S+Z) .*execution pipeline: executing finalized block height=(\d+) has_evm=\w+ has_native=(\w+) .*?native_count=(\d+)"
)
DONE = re.compile(r"^(\S+Z) .*execution pipeline: block done height=(\d+)")


def rd(lab, f):
    try:
        return open(R + lab + "/" + f).read()
    except FileNotFoundError:
        return ""


def mval(text, name):
    for l in text.splitlines():
        p = l.split()
        if p and p[0] == name:
            return float(p[1])
    return None


def pct(xs, q):
    if not xs:
        return None
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(q * (len(xs) - 1))))]


def f2(x, d=2):
    return "-" if x is None else f"{x:.{d}f}"


def ts(s):
    return (
        datetime.strptime(s[:26].rstrip("Z"), "%Y-%m-%dT%H:%M:%S.%f")
        .replace(tzinfo=timezone.utc)
        .timestamp()
    )


def node_blocks(lab, v):
    """[(start_epoch, ms, has_native, native_count)] from executing -> block done."""
    p = R + lab + f"/val{v}.log.gz"
    if not os.path.exists(p):
        return []
    st, out = {}, []
    with gzip.open(p, "rt", errors="replace") as fh:
        for l in fh:
            if "execution pipeline" not in l:
                continue
            l = ANSI.sub("", l)
            m = EXE.match(l)
            if m:
                st[m[2]] = (ts(m[1]), m[3] == "true", int(m[4]))
                continue
            m = DONE.match(l)
            if m and m[2] in st:
                t0, nat, n = st.pop(m[2])
                out.append((t0, (ts(m[1]) - t0) * 1e3, nat, n))
    return out


def cell(lab):
    c = {"lab": lab.replace(P, "")}
    rcl = (
        rd(lab + ".cell.rc", "")
        if False
        else (
            open(R + lab + ".cell.rc").read().strip()
            if os.path.exists(R + lab + ".cell.rc")
            else "missing"
        )
    )
    c["rc"] = rcl.replace("rc=", "")
    run = rd(lab, "run.log")
    try:
        s = json.load(open(R + lab + "/summary.json"))
    except Exception:
        s = None
    c["s"] = s
    if s:
        h = s["headline"]
        c.update(
            agree=h.get("agreement_verdict"),
            live=h.get("liveness_verdict"),
            valid=s.get("validity", {}).get("verdict"),
            mps=h["matched_s_avg"],
            best60=h["matched_s_best60"],
            nblk=h["native_blk_s"],
            fpb=h["fills_per_native_block"],
            chain=h["chain_ms"],
            eng=h["engine_ms_per_1k_fills"],
        )
        m = re.search(r"^ENGINE val0: .*margin=([0-9.]+) match=([0-9.]+)", run, re.M)
        if m and h["fills_per_native_block"]:
            k = h["fills_per_native_block"] / 1e3
            c["margin"], c["match"] = float(m[1]) / k, float(m[2]) / k
        of = s.get("oracle_feed") or {}
        c["oracle"] = (
            (
                f"stale {of.get('stale_marks_at_bench_end')} fresh {(of.get('marks_at_bench_end') or {}).get('fresh')} "
                f"acc {of.get('accepted')}/{of.get('sent')}"
            )
            if of.get("oracle_feed") == 1
            else "off"
        )
    m = re.search(r"drained=(\d) after (\d+)s", run)
    c["drained"] = f"{m[1]} ({m[2]} s)" if m else "-"
    m = re.search(r"settle drained=(\d) after (\S+?)s", run)
    c["settle"] = f"{m[1]} ({m[2]} s)" if m else "-"
    c["death"] = rd(lab, "node-death.txt").strip() or "none"
    env = rd(lab, "node-environ-trie.txt")
    md5s = re.findall(r"exe_md5=(\w+)", env)
    c["md5"] = f"{len(md5s)}x {','.join(sorted(set(md5s)))}" if md5s else "-"
    # liquidations
    liq = []
    for v in range(3):
        b, a = (
            mval(
                rd(lab, f"metrics-before-val{v}.txt"),
                "torus_liquidations_triggered_total",
            ),
            mval(
                rd(lab, f"metrics-after-val{v}.txt"),
                "torus_liquidations_triggered_total",
            ),
        )
        liq.append(None if a is None or b is None else a - b)
    c["liq"] = liq
    # drain window (epoch): first drain sample ts .. + elapsed_s
    dj = {}
    try:
        dj = json.load(open(R + lab + "/drain.json"))
        d0 = (
            json.loads(open(R + lab + "/drain-samples.jsonl").readline())["ts"]
            - json.loads(open(R + lab + "/drain-samples.jsonl").readline())["elapsed_s"]
        )
    except Exception:
        d0 = None
    c["dj"] = dj
    c["d0"] = d0
    lt = [l.split("\t") for l in rd(lab, "liq-drain.tsv").splitlines()[1:]]
    c["liq_rows"] = lt
    if lt and d0 is not None:
        d1 = d0 + dj.get("elapsed_s", 0)
        inw = {}
        for r in lt:
            t, n = float(r[0]), r[1]
            if r[2] == "":
                continue
            inw.setdefault(n, []).append(
                (t, float(r[2]), float(r[4] or 0), float(r[3] or 0), float(r[5] or 0))
            )
        dd = {}
        for n, rows in inw.items():
            w = [x for x in rows if d0 - 3 <= x[0] <= d1 + 1]
            if w:
                dd[n] = dict(
                    liq=w[-1][1] - w[0][1],
                    matched=w[-1][2] - w[0][2],
                    placed=w[-1][3] - w[0][3],
                    resting=w[-1][4] - w[0][4],
                    n=len(w),
                )
        c["liq_drain"] = dd
    # exec lag from drain-feed-live.tsv
    rows = [l.split("\t") for l in rd(lab, "drain-feed-live.tsv").splitlines()[1:]]
    if rows:
        qs = dj.get("quiet_elapsed_s") or 0
        el = dj.get("elapsed_s") or 0
        quiet = [float(r[3]) for r in rows if r[3] and float(r[0]) >= el - qs - 0.01]
        bys = {}
        for r in rows:
            if r[3]:
                bys.setdefault(float(r[0]), []).append(float(r[3]))
        t0 = next((t for t in sorted(bys) if max(bys[t]) == 0), None)
        t2 = next((t for t in sorted(bys) if max(bys[t]) <= 2), None)
        c["lag"] = dict(
            start=max(bys[min(bys)]),
            qmin=min(quiet) if quiet else None,
            qmed=statistics.median(quiet) if quiet else None,
            qmax=max(quiet) if quiet else None,
            t0=t0,
            t2=t2,
        )
    # node-log per-block exec ms in the drain window
    if d0 is not None and dj:
        d1 = d0 + dj["elapsed_s"]
        q0 = d1 - (dj.get("quiet_elapsed_s") or 0)
        orc, orq, emp, big = [], [], [], []
        for v in range(3):
            for t, ms, nat, n in node_blocks(lab, v):
                if not (d0 <= t <= d1):
                    continue
                if not nat:
                    emp.append(ms)
                elif n <= 12:
                    orc.append(ms)
                    if t >= q0:
                        orq.append(ms)
                else:
                    big.append(ms)
        c["blk"] = dict(orc=orc, orq=orq, emp=emp, big=big)
    return c


def main():
    labs = sys.argv[1:] or CELLS
    cs = [cell(l) for l in labs]
    print("## 1. Checks")
    print(
        "| cell | rc | AGREE | liveness | validity | drained (s) | settle | deaths | node exe md5 | oracle |"
    )
    print("|---|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        print(
            f"| {c['lab']} | {c['rc']} | {c.get('agree')} | {c.get('live')} | {c.get('valid')} | {c['drained']} | {c['settle']} | {c['death'][:60]} | {c['md5']} | {c.get('oracle', '-')} |"
        )
    print("\n## 2. Throughput and cost")
    print(
        "| cell | matched/s | best60 | native blk/s | fills per native block | chain ms | engine ms/1k | margin ms/1k | match ms/1k |"
    )
    print("|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        if "mps" in c:
            print(
                f"| {c['lab']} | {c['mps']:,.0f} | {c['best60']:,.0f} | {c['nblk']:.3f} | {c['fpb'] / 1e3:.1f}k | {c['chain']:.0f} | {c['eng']:.2f} | {f2(c.get('margin'))} | {f2(c.get('match'))} |"
            )
    by = {c["lab"]: c for c in cs}
    keys = [
        ("mps", "matched/s"),
        ("best60", "best60"),
        ("nblk", "native blk/s"),
        ("fpb", "fills per native block"),
        ("chain", "chain ms"),
        ("eng", "engine ms/1k"),
        ("margin", "margin ms/1k"),
        ("match", "match ms/1k"),
    ]
    for num, den in (("w10", "m"), ("w0", "m"), ("w10", "w0")):
        print(
            f"\n| {num.upper()} / {den.upper()} | r1 | r2 | mean |\n|---|---|---|---|"
        )
        for k, name in keys:
            rs = []
            for r in ("r1", "r2"):
                a, b = by.get(f"{num}-{r}", {}), by.get(f"{den}-{r}", {})
                rs.append(a[k] / b[k] if a.get(k) and b.get(k) else None)
            ok = [x for x in rs if x is not None]
            print(
                f"| {name} | {f2(rs[0], 3)}x | {f2(rs[1], 3)}x | {f2(sum(ok) / len(ok), 3) if ok else '-'}x |"
            )
    print("\n## 3. Liquidations (torus_liquidations_triggered_total)")
    print(
        "| cell | delta before->after val0/1/2 | drain window (liq-drain.tsv) per node: liq / matched / placed / resting deltas |"
    )
    print("|---|---|---|")
    for c in cs:
        dd = c.get("liq_drain") or {}
        dw = (
            "; ".join(
                f"{n}: {x['liq']:.0f}/{x['matched']:.0f}/{x['placed']:.0f}/{x['resting']:.0f} ({x['n']} samples)"
                for n, x in sorted(dd.items())
            )
            or "-"
        )
        print(f"| {c['lab']} | {'/'.join(f2(x, 0) for x in c['liq'])} | {dw} |")
    print(
        "\n## 4. Feed-live drain: per-block exec ms from node logs (executing finalized block -> block done), drain window, pooled 3 nodes"
    )
    print(
        "| cell | oracle-only blocks (native_count<=12) n / p50 / p95 / max | in quiet window n / p50 / max | empty n / p50 | load-size blocks n / max | exec lag at start | quiet min/med/max | time to all 0 / all <=2 (s) |"
    )
    print("|---|---|---|---|---|---|---|---|")
    pools = {}
    for c in cs:
        b, l = c.get("blk"), c.get("lag")
        if not b:
            continue
        arm = c["lab"].split("-")[0]
        pools.setdefault(arm, {"orc": [], "orq": [], "emp": []})
        for k in ("orc", "orq", "emp"):
            pools[arm][k] += b[k]
        o, q, e = b["orc"], b["orq"], b["emp"]
        print(
            f"| {c['lab']} | {len(o)} / {f2(pct(o, 0.5))} / {f2(pct(o, 0.95))} / {f2(max(o) if o else None)} | {len(q)} / {f2(pct(q, 0.5))} / {f2(max(q) if q else None)} | "
            f"{len(e)} / {f2(pct(e, 0.5))} | {len(b['big'])} / {f2(max(b['big']) if b['big'] else None, 0)} | "
            f"{f2(l and l['start'], 0)} | {f2(l and l['qmin'], 0)}/{f2(l and l['qmed'], 1)}/{f2(l and l['qmax'], 0)} | {f2(l and l['t0'], 1)} / {f2(l and l['t2'], 1)} |"
        )
    print(
        "\n| arm (pooled cells) | oracle-only n / p50 / p95 / max | quiet window n / p50 / max | empty p50 |\n|---|---|---|---|"
    )
    for arm, p in pools.items():
        o, q, e = p["orc"], p["orq"], p["emp"]
        print(
            f"| {arm} | {len(o)} / {f2(pct(o, 0.5))} / {f2(pct(o, 0.95))} / {f2(max(o) if o else None)} | {len(q)} / {f2(pct(q, 0.5))} / {f2(max(q) if q else None)} | {f2(pct(e, 0.5))} |"
        )


main()
