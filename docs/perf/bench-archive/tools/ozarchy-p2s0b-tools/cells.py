#!/usr/bin/env python3
"""cells.py <cell_dir>...: p2s0b per-cell table from summary.json (val0 unless noted) + fds.txt.
Phase ms are per native block over the harness's bench+drain window (summary note); proc CPU over the load window."""

import json
import os
import statistics as S
import sys


def g(d, *ks):
    for k in ks:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def fds(c):
    try:
        rows = [line.split() for line in open(c + "/fds.txt")]
    except OSError:
        return None
    m = 0
    for r in rows:
        for x in r[1:]:
            if x != "-":
                m = max(m, int(x))
    return m


cells = sys.argv[1:]
rows = []
for c in cells:
    s = json.load(open(c + "/summary.json"))
    h, p = s["headline"], s["phase_by_node"]["val0"]
    e = p["phases"]["engine"]
    fpb = h["fills_per_native_block"]
    pc = g(s, "proc_cpu_by_node", "val0", "load") or {}
    ts = p.get("thread_spawns") or {}
    ca = p.get("cancel_all") or {}
    bi = p.get("by_id") or {}
    oo = p.get("oracle_only_blocks") or {}
    r = {
        "cell": os.path.basename(c).replace("ozarchy-p2s0b-300m-", ""),
        "verdict": f"{h['agreement_verdict']}/{h['liveness_verdict']}/{'acc' if h['benchmark_accepted'] else 'REJ'}",
        "matched_s": h["matched_s_avg"],
        "best60": h["matched_s_best60"],
        "native_blk_s": h["native_blk_s"],
        "fills_blk": fpb,
        "engine": e["ms"],
        "engine_1k": h["engine_ms_per_1k_fills"],
        "phase1": e.get("phase1_actions_ms"),
        "margin": e.get("phase_margin_ms"),
        "match": e.get("phase_match_ms"),
        "settle": e.get("phase_settle_ms"),
        "cache_flush": e.get("cache_flush_ms"),
        "save_books": g(p, "phases", "save_books", "ms"),
        "verify": g(p, "phases", "verify", "ms"),
        "flush_worker": g(p, "phases", "flush", "ms"),
        "chain": p.get("chain_ms"),
        "ca_blk": ca.get("per_native_block"),
        "ca_visit": ca.get("books_visited_per_cancel_all"),
        "ca_hit": ca.get("books_hit_per_cancel_all"),
        "byid_blk": bi.get("per_native_block"),
        "byid_probe": bi.get("books_probed_per_action"),
        "spawn_blk": ts.get("total_per_native_block"),
        "spawn_min": ts.get("total_per_minute"),
        "spawn_sites": {
            k: v for k, v in (ts.get("per_native_block") or {}).items() if v
        },
        "user_blk": pc.get("user_ms_per_native_block"),
        "user_1k": pc.get("user_ms_per_1k_fills"),
        "sys_blk": pc.get("sys_ms_per_native_block"),
        "sys_1k": pc.get("sys_ms_per_1k_fills"),
        "oo_n": oo.get("blocks"),
        "oo_avg": oo.get("ms_avg"),
        "oo_p50": oo.get("ms_p50"),
        "oo_p95": oo.get("ms_p95"),
        "fds_max": fds(c),
        "ingest_byid": g(s, "ingest", "cancel_by_id"),
        "stale": g(s, "oracle_feed", "stale_marks_at_bench_end"),
    }
    rows.append(r)

f = lambda v, n=1: (
    "-" if v is None else (f"{v:,.{n}f}" if isinstance(v, (int, float)) else str(v))
)  # noqa: E731
print(
    "cell | verdict | matched/s | best60 | native blk/s | fills/blk | engine ms/blk | engine ms/1k | phase1 | margin | match | settle | cache flush | save books | verify | flush worker | chain"
)
for r in rows:
    print(
        " | ".join(
            [
                r["cell"],
                r["verdict"],
                f(r["matched_s"], 0),
                f(r["best60"], 0),
                f(r["native_blk_s"], 3),
                f(r["fills_blk"], 0),
                f(r["engine"]),
                f(r["engine_1k"], 2),
                f(r["phase1"]),
                f(r["margin"]),
                f(r["match"]),
                f(r["settle"]),
                f(r["cache_flush"]),
                f(r["save_books"]),
                f(r["verify"]),
                f(r["flush_worker"]),
                f(r["chain"]),
            ]
        )
    )
print()
print(
    "cell | cancel-alls/blk | books visited/call | books hit/call | by-id/blk | probed/by-id | spawns/blk | spawns/min | user ms/blk | user ms/1k | sys ms/blk | sys ms/1k | oracle-only n avg p50 p95 | fds max | stale"
)
for r in rows:
    print(
        " | ".join(
            [
                r["cell"],
                f(r["ca_blk"], 2),
                f(r["ca_visit"]),
                f(r["ca_hit"], 2),
                f(r["byid_blk"], 2),
                f(r["byid_probe"]),
                f(r["spawn_blk"], 2),
                f(r["spawn_min"], 0),
                f(r["user_blk"]),
                f(r["user_1k"], 2),
                f(r["sys_blk"], 2),
                f(r["sys_1k"], 3),
                f"{f(r['oo_n'], 0)} {f(r['oo_avg'], 2)} {f(r['oo_p50'], 2)} {f(r['oo_p95'], 2)}",
                f(r["fds_max"], 0),
                f(r["stale"], 0),
            ]
        )
    )
print()
for r in rows:
    print(
        f"{r['cell']} spawn sites per native block: {json.dumps(r['spawn_sites'])}"
        + (
            f"  ingest.cancel_by_id {json.dumps(r['ingest_byid'])}"
            if r["ingest_byid"]
            else ""
        )
    )

# overhead check: s-r* vs b-r*
by = {r["cell"]: r for r in rows}
sr = [by[k] for k in ("s-r1", "s-r2") if k in by]
br = [by[k] for k in ("b-r1", "b-r2") if k in by]
if sr and br:
    print()
    for key, lab in (
        ("matched_s", "matched/s"),
        ("engine_1k", "engine ms/1k"),
        ("engine", "engine ms/blk"),
        ("phase1", "phase1 ms/blk"),
        ("chain", "chain ms/blk"),
        ("user_1k", "user CPU ms/1k"),
        ("sys_1k", "sys CPU ms/1k"),
    ):
        a, b = (
            [x[key] for x in sr if x[key] is not None],
            [x[key] for x in br if x[key] is not None],
        )
        if a and b:
            print(
                f"overhead {lab}: step0 {S.mean(a):,.3f} ({', '.join(f'{v:,.3f}' for v in a)}) / base {S.mean(b):,.3f} "
                f"({', '.join(f'{v:,.3f}' for v in b)}) = {S.mean(a) / S.mean(b):.4f}x"
            )
