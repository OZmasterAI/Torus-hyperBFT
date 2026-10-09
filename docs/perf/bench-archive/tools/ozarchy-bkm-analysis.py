#!/usr/bin/env python3
"""ozarchy-bkm analysis: Classic (A, TORUS_BOOK_ROWS=0) vs mode 3 (B, RECORD_ENV TORUS_BOOK_ROWS=3), one node 1eced05c.
Per cell validity (rc, AGREE, liveness, acc, oracle stale 0, node md5 3/3, no panic/ERROR/exit 70, no death, book mode per validator),
per arm per shape mean (r1/r2), ratio B/A of means with pairwise ratios (b-rI / a-rI). Output: ozarchy-bkm-handoff-tables.txt.
--test: run on the p3s1 300 mk layout (a-r1/a-r2 as A, b-r1/b-r2 as B; book-mode files absent there -> n/a), print only."""

import gzip
import json
import os
import re
import statistics as st
import sys

R = "/home/oz/bench-results-matched"
TEST = "--test" in sys.argv
ANSI = re.compile(r"\x1b\[[0-9;]*m")
BAD = re.compile(r"panic|ERROR|exit code 70|exit 70|exit_code=70", re.I)
LA = "order books loaded from DB (level authority)"
if TEST:
    SHAPES = {"300m": "ozarchy-p3s1-300m"}
    NODE = None
    WANT = {"a": "3", "b": "3"}
else:
    SHAPES = {"300m": "ozarchy-bkm-300m", "10m": "ozarchy-bkm-10m"}
    NODE = open(f"{R}/ozarchy-bkm-build/md5s.txt").read().split("\n")[0].split()[2][:8]
    WANT = {"a": "0", "b": "3"}
ROUNDS = 2
M = [
    ("matched", "matched/s"),
    ("nblk", "native blk/s"),
    ("blkms", "val0 block ms"),
    ("chainms", "val0 chain ms"),
    ("fpb", "fills/blk"),
    ("sb", "save_books ms/blk"),
    ("sbd", "save_books drain ms"),
    ("sbw", "save_books write ms"),
    ("fl", "flush ms/blk"),
    ("sbk", "save_books /1k fills"),
    ("sbk3", "save_books /1k (val0-2)"),
    ("flk", "flush /1k fills"),
    ("flk3", "flush /1k (val0-2)"),
    ("eng1k", "engine /1k fills"),
]


def num(x):
    return x.get("avg") if isinstance(x, dict) else x


def scan_logs(o):
    """(bad line count, level-authority load lines per validator)"""
    bad, la = 0, []
    for v in range(3):
        n = 0
        with gzip.open(f"{o}/val{v}.log.gz", "rt", errors="replace") as f:
            for line in f:
                line = ANSI.sub("", line)
                bad += bool(BAD.search(line))
                n += LA in line
        la.append(n)
    return bad, la


def book_mode(o, s, arm, la):
    """per-validator book mode evidence -> (text, mode_ok, wal_ok)"""
    want = WANT[arm]
    senv = str((s.get("cell") or {}).get("node_env", {}).get("TORUS_BOOK_ROWS"))
    path = f"{o}/node-bookmode.txt"
    bm = open(path).read() if os.path.exists(path) else ""
    penv = re.findall(
        r"^pid=\d+ (?:TORUS_BOOK_ROWS=(\S+)|BOOKROWS_VAR_UNSET)", bm, re.M
    )
    wal = [w.strip() for w in re.findall(r"^val\d wal_marker (.*)$", bm, re.M)]
    la_ok = all(n == 0 for n in la) if want == "0" else all(n >= 1 for n in la)
    proc_ok = (len(penv) == 3 and all(p == want for p in penv)) if bm else True
    wal_ok = (
        (len(wal) == 3 and all(re.fullmatch(rf"byte={want} hits=\d+", w) for w in wal))
        if bm
        else None
    )
    text = (
        f"env {senv} proc {'/'.join(p or 'unset' for p in penv) or 'n/a'} la {'/'.join(map(str, la))} "
        f"wal {'/'.join(wal) or 'n/a'}"
    )
    return text, bool(la_ok and proc_ok and senv == want), wal_ok


def per1k(s, v, phase):
    pv = s["phase_by_node"][f"val{v}"]
    return 1000 * pv["phases"][phase]["ms"] / pv["fills_per_native_block"]


def cell(o, arm):
    s = json.load(open(f"{o}/summary.json"))
    h = s["headline"]
    of = s.get("oracle_feed") or {}
    md5s = re.findall(r"exe_md5=(\w+)", open(f"{o}/node-environ-trie.txt").read())
    bad, la = scan_logs(o)
    mode, mode_ok, wal_ok = book_mode(o, s, arm, la)
    pv = s["phase_by_node"]["val0"]
    ph = pv["phases"]
    sb = ph["save_books"]
    d = {
        "rc": open(f"{o}.cell.rc").read().strip(),
        "agree": h["agreement_verdict"],
        "live": h["liveness_verdict"],
        "acc": h["benchmark_accepted"],
        "stale": of.get("stale_marks_at_bench_end"),
        "oracle": f"{of.get('accepted')}/{of.get('sent')}",
        "md5ok": len(md5s) == 3
        and len(set(md5s)) == 1
        and (NODE is None or md5s[0] == NODE),
        "bad": bad,
        "death": os.path.exists(f"{o}/node-death.txt"),
        "mode": mode,
        "mode_ok": mode_ok,
        "wal_ok": wal_ok,
        "matched": h["matched_s_avg"],
        "nblk": h["native_blk_s"],
        "fpb": pv["fills_per_native_block"],
        "blkms": num(pv["block_ms"]),
        "chainms": num(pv.get("chain_ms", h.get("chain_ms"))),
        "sb": sb["ms"],
        "sbd": sb.get("save_books_drain_ms"),
        "sbw": sb.get("save_books_write_ms"),
        "fl": ph["flush"]["ms"],
        "sbk": per1k(s, 0, "save_books"),
        "flk": per1k(s, 0, "flush"),
        "sbk3": st.mean(per1k(s, v, "save_books") for v in range(3)),
        "flk3": st.mean(per1k(s, v, "flush") for v in range(3)),
        "eng1k": pv.get("engine_ms_per_1k_fills"),
        "act": h.get("actions_per_exec_block"),
        "tmo": h.get("consensus_timeouts"),
    }
    d["valid"] = (
        d["rc"] == "rc=0"
        and d["agree"] == "AGREE"
        and d["live"] == "PASS"
        and d["acc"] is True
        and d["stale"] == 0
        and d["md5ok"]
        and d["bad"] == 0
        and not d["death"]
        and d["mode_ok"]
    )
    return d


def fmt(x):
    return "n/a" if x is None else f"{x:,.3f}"


def shape_tables(pr, shape, P):
    tags = ["a-warm"] + [f"{x}-r{i}" for i, rd in ((1, "ab"), (2, "ba")) for x in rd]
    C = {}
    pr(f"\n=== {shape} ({P}-*) ===")
    for t in tags:
        try:
            C[t] = cell(f"{R}/{P}-{t}", t[0])
        except Exception as e:
            pr(f"{t}: MISSING/ERROR {e!r}")
    pr(
        "| cell | rc | agree | live | acc | stale | oracle | md5 3/3 | bad lines | death | book mode | mode ok | wal ok | valid | "
        + " | ".join(n for _, n in M)
        + " | actions/blk | timeouts |"
    )
    for t in (t for t in tags if t in C):
        d = C[t]
        pr(
            f"| {t} | {d['rc']} | {d['agree']} | {d['live']} | {d['acc']} | {d['stale']} | {d['oracle']} | {d['md5ok']} | "
            f"{d['bad']} | {d['death']} | {d['mode']} | {d['mode_ok']} | {d['wal_ok']} | {d['valid']} | "
            + " | ".join(fmt(d[k]) for k, _ in M)
            + f" | {d['act']} | {d['tmo']} |"
        )
    pr(
        f"invalid counted cells: {[t for t in tags[1:] if t in C and not C[t]['valid']] or 'none'}"
    )
    A = {
        x: [
            C[f"{x}-r{i}"]
            for i in range(1, ROUNDS + 1)
            if f"{x}-r{i}" in C and C[f"{x}-r{i}"]["valid"]
        ]
        for x in "ab"
    }
    pr("\nper arm (valid counted cells): mean (r1 / r2; spread = |r1-r2| / mean)")
    S = {"a": {}, "b": {}}
    for x, name in (("a", "A Classic"), ("b", "B mode 3")):
        pr(f"  {name} (n={len(A[x])}):")
        for k, n in M:
            vals = [c[k] for c in A[x] if c[k] is not None]
            if not vals:
                S[x][k] = None
                pr(f"    {n} n/a")
                continue
            m = S[x][k] = st.mean(vals)
            sp = (
                f", spread {abs(vals[0] - vals[1]) / m * 100:.1f}%"
                if len(vals) == 2 and m
                else ""
            )
            pr(f"    {n} {m:,.3f} ({' / '.join(f'{v:,.3f}' for v in vals)}{sp})")
    pr(
        "\nratio B/A (mode 3 / Classic): ratio of means; pairwise b-rI/a-rI (r1 = cells 2/1 of the block, r2 = cells 3/4)"
    )
    for k, n in M:
        a, b = S["a"].get(k), S["b"].get(k)
        if not a or b is None:
            pr(f"  {n}: n/a")
            continue
        pairs = []
        for i in range(1, ROUNDS + 1):
            u, w = C.get(f"b-r{i}"), C.get(f"a-r{i}")
            ok = u and w and u["valid"] and w["valid"] and u[k] is not None and w[k]
            pairs.append(f"{u[k] / w[k]:.4f}" if ok else "n/a")
        pr(f"  {n}: {b / a:.4f}x  diff {b - a:+,.3f}  pairs {' '.join(pairs)}")


def main():
    out = []
    pr = out.append
    pr(
        "ozarchy-bkm (ozarchy): Classic vs mode 3 book layout, ONE node origin/main 1eced05c"
        + (f" (md5 {NODE})" if NODE else "")
        + "; A = EXTRA_ENV TORUS_BOOK_ROWS=0 (Classic), B = RECORD_ENV TORUS_BOOK_ROWS=3 (default bench config); bench 6c7ad1a7, harness bdd5b470"
    )
    pr(
        "shape: N=4 budget 900, cap 400, rate 76,000, RETRY_BUSY=1, 120 s, oracle 30000/2000 ms walk 0, trie off, no perf; "
        "300 mk and 10 mk; per shape warm (60 s, arm A, excluded) then A B B A"
    )
    pr(
        "val0 unless noted; ms per native block; /1k = ms per 1k fills; (val0-2) = mean over validators. "
        "save_books drain/write: summary split (write is not a subset of the exec-thread save_books ms)"
    )
    pr(
        'book mode: env = summary node_env TORUS_BOOK_ROWS; proc = /proc environ per validator; la = "level authority" load_books '
        "lines per validator (Classic 0, mode 3 >= 1); wal = __book_mode__ marker byte from the WAL at bench start (best effort)"
    )
    if TEST:
        pr("*** TEST RUN on p3s1 cells (both arms mode 3) ***")
    for shape, P in SHAPES.items():
        shape_tables(pr, shape, P)
    txt = "\n".join(out) + "\n"
    if not TEST:
        open(f"{R}/ozarchy-bkm-handoff-tables.txt", "w").write(txt)
    sys.stdout.write(txt)


main()
