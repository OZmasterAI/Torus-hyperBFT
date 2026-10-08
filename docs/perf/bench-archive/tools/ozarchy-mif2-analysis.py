#!/usr/bin/env python3
"""ozarchy-mif2-analysis.py A|B: tables for the mif2 campaign (ozarchy, 2026-10-06).
A: main N sweep, b900 + bases, cells from ozarchy-mif-300m-* (earlier sweep) and ozarchy-mif2-300m-* (this run).
B: crab 59fa407 vs main at the chosen setting (ozarchy-mif2-300m-{crab,main}-*), section 19.1 columns.
Cell metric code from ozarchy-mif-analysis.py; margin/match ms per 1k from ozarchy-walk-5584880-analysis.py;
CPU-s per 1M fills = mean node CPU-s over t_bench0..t_drain / (val0 torus_orders_matched_total delta / 1e6) (section 19.1)."""

import importlib.util, json, re, statistics, sys

R = "/home/oz/bench-results-matched/"
spec = importlib.util.spec_from_file_location("mif", R + "ozarchy-mif-analysis.py")
src = open(R + "ozarchy-mif-analysis.py").read().replace("\nmain()\n", "\n")
mif = type(sys)("mif")
exec(compile(src, "mif", "exec"), mif.__dict__)
rd, mdelta, f = mif.rd, mif.mdelta, mif.f


def cell(lab, name):
    mif.P = ""
    c = mif.cell(lab)
    c["tag"] = name
    c["lab"] = lab
    if "mps" not in c:
        return c
    s = json.load(open(R + lab + "/summary.json"))
    h, tm = s["headline"], s["timing"]
    c["fpb"], c["chain"], c["eng"] = h["fills_per_native_block"], h["chain_ms"], h["engine_ms_per_1k_fills"]
    run = rd(R + lab + "/run.log")
    m = re.search(r"^ENGINE val0: .*margin=([0-9.]+) match=([0-9.]+)", run, re.M)
    if m and c["fpb"]:
        k = c["fpb"] / 1e3
        c["margin"], c["match"] = float(m[1]) / k, float(m[2]) / k
    nb = mdelta(lab, 0, "torus_exec_native_blocks_total")
    c["apb"] = c["proc"] / nb if c["proc"] and nb else None
    fills = mdelta(lab, 0, "torus_orders_matched_total")
    cp = mif.cpu(lab, s["cell"]["node_pids"], tm["t_bench0"], tm["t_drain"])
    c["cpu1m"] = statistics.mean(cp) / (fills / 1e6) if fills and None not in cp else None
    c["drained"] = tm.get("drained")
    c["death"] = rd(R + lab + "/node-death.txt").strip() or "none"
    env = rd(R + lab + "/node-environ-trie.txt")
    md5s = re.findall(r"exe_md5=(\w+)", env)
    c["md5"] = f"{len(md5s)}x {','.join(sorted(set(md5s)))}"
    tv = re.findall(r"(TORUS_NATIVE_TRIE\S*|TRIE_VAR_MISSING)", env)
    c["trie"] = ",".join(sorted(set(tv)))
    of = s.get("oracle_feed") or {}
    c["oracle"] = (
        f"stale {of.get('stale_marks_at_bench_end')} fresh {(of.get('marks_at_bench_end') or {}).get('fresh')}"
        if of.get("oracle_feed") == 1 else "off"
    )
    return c


def checks(cs):
    print("| cell | rc | AGREE | liveness | drained | deaths | node exe md5 | trie var | oracle | tail WARNING |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        print(f"| {c['tag']} | {c['rc']} | {c.get('agree')} | {c.get('live')} | {c.get('drained')} | {c.get('death', '-')[:50]} | {c.get('md5')} | {c.get('trie')} | {c.get('oracle')} | {'YES' if c.get('warn') else 'no'} |")


def A():
    o, n = "ozarchy-mif-300m-", "ozarchy-mif2-300m-"
    L = [(o + "base", "base (mif r1)"), (o + "base-r2", "base (mif r2)"), (o + "base-r3", "base (mif r3)"), (n + "base", "base (mif2)"),
         (o + "base-b900", "base-b900 (mif)"),
         (o + "n1-b900", "n1-b900"), (o + "n2-b900", "n2-b900"), (o + "n4-b900", "n4-b900 r1"), (o + "n4-b900-r2", "n4-b900 r2"),
         (o + "n4-b900-r3", "n4-b900 r3"), (n + "warm", "n4-b900 warm 60s (mif2)"), (n + "n8-b900", "n8-b900"), (n + "n16-b900", "n16-b900")]
    cs = [cell(l, t) for l, t in L]
    print("## A. Checks"); checks(cs)
    print("\n## A. main N sweep, b900")
    print("| cell | matched/s | best60 | native blk/s | actions / native blk | max gap commit / native (s) | open_limit share | place / cancel-all | submit act/s | age_commit p50 / p95 (ms) | nonce-expired v0/v1/v2 | released committed / refused / timeout | tail fetched / errors / missed |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        if "mps" not in c:
            print(f"| {c['tag']} | no summary (rc {c['rc']}) |"); continue
        a, i = c["age"], c["inf"] or {}
        rel = f"{i['released_committed']:,} / {i['released_refused']:,} / {i['released_timeout']:,}" if i else "-"
        tl = f"{i['tail_fetched']} / {i['tail_errors']} / {i['tail_missed']}" if i else "-"
        print(f"| {c['tag']} | {f(c['mps'])} | {f(c['best60'])} | {f(c['nblk'], 3)} | {f(c['apb'], 0)} | {c['gap_c']} / {c['gap_n']} | {f(c['ol'], 2, True)} | "
              f"{f(c['place_sh'], 1, True)} / {f(c['ca_sh'], 1, True)} | {f(c['rate'])} | {f(a[0])} / {f(a[1])} | {'/'.join(str(x) for x in c['evict'] or [])} | {rel} | {tl} |")
    by = {c["tag"]: c for c in cs}
    old = [by[k]["mps"] for k in ("base (mif r1)", "base (mif r2)", "base (mif r3)") if "mps" in by[k]]
    n4 = [by[k]["mps"] for k in ("n4-b900 r1", "n4-b900 r2", "n4-b900 r3") if "mps" in by[k]]
    print(f"\nold base mean {statistics.mean(old):,.0f} sd {statistics.stdev(old):,.0f}; n4-b900 r1-r3 mean {statistics.mean(n4):,.0f} sd {statistics.stdev(n4):,.0f}")
    if "mps" in by["base (mif2)"]:
        d = by["base (mif2)"]["mps"] / statistics.mean(old) - 1
        print(f"new base {by['base (mif2)']['mps']:,.0f} = {d:+.1%} vs old mean{'  ** FLAG: >5% **' if abs(d) > 0.05 else ''}")
    pts = {"1": by["n1-b900"].get("mps"), "2": by["n2-b900"].get("mps"), "4": statistics.mean(n4),
           "8": by["n8-b900"].get("mps"), "16": by["n16-b900"].get("mps")}
    best = max(v for v in pts.values() if v)
    print("N -> matched/s (n4 = r1-r3 mean), % of best: " + "; ".join(f"N={k} {v:,.0f} ({v / best:.1%})" for k, v in pts.items() if v))


def B():
    n = "ozarchy-mif2-300m-"
    L = [(n + t, t) for t in ("crab-warm", "crab-r1", "main-r1", "crab-r2", "main-r2")]
    cs = [cell(l, t) for l, t in L]
    print("## B. Checks"); checks(cs)
    print("\n## B. crab 59fa407 vs main 92a02ed")
    print("| cell | matched/s | best60 | native blk/s | actions / native blk | fills / native blk | chain ms | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills | open_limit share | cancel-all share | max gap native (s) | tail err / missed |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for c in cs:
        if "mps" not in c:
            print(f"| {c['tag']} | no summary (rc {c['rc']}) |"); continue
        i = c["inf"] or {}
        print(f"| {c['tag']} | {f(c['mps'])} | {f(c['best60'])} | {f(c['nblk'], 3)} | {f(c['apb'], 0)} | {c['fpb'] / 1e3:.1f}k | {c['chain']:.0f} | {c['eng']:.2f} | "
              f"{f(c.get('margin'), 2)} | {f(c.get('match'), 2)} | {f(c['cpu1m'], 1)} | {f(c['ol'], 2, True)} | {f(c['ca_sh'], 1, True)} | {c['gap_n']} | {i.get('tail_errors', '-')} / {i.get('tail_missed', '-')} |")
    by = {c["tag"]: c for c in cs}
    keys = [("mps", "matched/s"), ("best60", "best60"), ("nblk", "native blk/s"), ("apb", "actions / native blk"), ("fpb", "fills / native blk"),
            ("chain", "chain ms"), ("eng", "engine ms/1k"), ("margin", "margin ms/1k"), ("match", "match ms/1k"), ("cpu1m", "CPU-s per 1M fills")]
    print("\n| crab / main | r1 pair | r2 pair | mean | section 19 |\n|---|---|---|---|---|")
    s19 = {"mps": "1.097x", "best60": "0.989x", "nblk": "1.14x", "fpb": "0.956x", "chain": "0.87x", "eng": "0.89x", "cpu1m": "0.92x"}
    for k, name in keys:
        rs = []
        for r in ("r1", "r2"):
            a, b = by.get("crab-" + r, {}), by.get("main-" + r, {})
            rs.append(a[k] / b[k] if a.get(k) and b.get(k) else None)
        ok = [x for x in rs if x is not None]
        print(f"| {name} | {f(rs[0], 3)}x | {f(rs[1], 3)}x | {f(sum(ok) / len(ok), 3) if ok else '-'}x | {s19.get(k, '-')} |")


{"A": A, "B": B}[sys.argv[1]]()
