"""p21g.py: ozarchy p21g analysis (P2-1 cancel-all index gate: ref e934fa0e vs p21 2ecc2bdf).
Per-cell + per-arm tables from summary.json (val0), r-spread, p21 - ref deltas, the phase 1 gate (>= 1.9 ms per
native block; 1.4-2.4 ms -> 2 more cells per arm), cancel-all counters and the index gauges (all validators).
Phase 1 = engine.phase1_actions_ms. apply = end_resident_positions_ms (TraderPositions::apply timer).
Usage: p21g.py [out.txt] (also prints to stdout). Cells: every ozarchy-p21g-300m-<arm>-r<k> with a summary.json."""

import json, sys, glob, os, re, statistics as st, builtins

_out = open(sys.argv[1], "w") if len(sys.argv) > 1 else None


def print(*a, **k):
    builtins.print(*a, **k)
    if _out:
        builtins.print(*a, **k, file=_out)


R = "/home/oz/bench-results-matched/ozarchy-p21g-300m-"


def key(t):
    a, k = t.rsplit("-r", 1)
    return (int(k), a)


tags = sorted(
    (
        os.path.basename(d)[len(R) - len("/home/oz/bench-results-matched/") :]
        for d in glob.glob(R + "*-r[0-9]*")
        if os.path.isdir(d) and os.path.exists(d + "/summary.json")
    ),
    key=lambda t: os.path.getmtime(R + t + "/summary.json"),
)
K = [
    "mps",
    "blk",
    "fills",
    "eng",
    "eng1k",
    "chain",
    "ph1",
    "ph1k",
    "margin",
    "match",
    "match1k",
    "settle",
    "settle1k",
    "passB",
    "passB1k",
    "er",
    "pos",
    "pos1k",
    "erw",
    "cflush",
]


def row(t):
    s = json.load(open(R + t + "/summary.json"))
    h = s["headline"]
    p = s["phase_by_node"]["val0"]
    ph = p["phases"]
    e = ph["engine"]
    f = p["fills_per_native_block"]
    er = ph["end_resident"]
    rc = (
        open(R + t + ".cell.rc").read().strip()
        if os.path.exists(R + t + ".cell.rc")
        else "rc=?"
    )
    ca = {
        v: s["phase_by_node"][v].get("cancel_all") or {}
        for v in ("val0", "val1", "val2")
    }
    return dict(
        mps=h["matched_s_avg"],
        blk=h["native_blk_s"],
        fills=f,
        eng=e["ms"],
        eng1k=h["engine_ms_per_1k_fills"],
        chain=p["chain_ms"],
        ph1=e["phase1_actions_ms"],
        ph1k=e["phase1_actions_ms"] / f * 1000,
        margin=e["phase_margin_ms"],
        match=e["phase_match_ms"],
        match1k=e["phase_match_ms"] / f * 1000,
        settle=e["phase_settle_ms"],
        settle1k=e["phase_settle_ms"] / f * 1000,
        passB=e["settle_pass_b_ms"],
        passB1k=e["settle_pass_b_ms"] / f * 1000,
        er=er["ms"],
        pos=er["end_resident_positions_ms"],
        pos1k=er["end_resident_positions_ms"] / f * 1000,
        erw=ph["end_resident_wait"]["ms"],
        cflush=e["cache_flush_ms"],
        acc=h["benchmark_accepted"],
        live=h["liveness_verdict"],
        agree=h["agreement_verdict"],
        rc=rc,
        ca=ca,
        ph1_all={
            v: s["phase_by_node"][v]["phases"]["engine"]["phase1_actions_ms"]
            for v in ("val0", "val1", "val2")
        },
    )


fmt = {
    "mps": "{:,.0f}",
    "fills": "{:,.0f}",
    "blk": "{:.3f}",
    "eng1k": "{:.2f}",
    "ph1k": "{:.3f}",
    "match1k": "{:.3f}",
    "settle1k": "{:.3f}",
    "passB1k": "{:.3f}",
    "pos1k": "{:.3f}",
}
F = lambda k, v: fmt.get(k, "{:.2f}").format(v)
rows = {t: row(t) for t in tags}
print(
    "ms per native block (val0, bench + drain); *1k = ms per 1k fills; ph1 = phase 1 (phase1_actions_ms)"
)
print("| cell | " + " | ".join(K) + " | rc / verdicts |")
print("|---" * (len(K) + 2) + "|")
for t in tags:
    r = rows[t]
    print(
        f"| {t} | "
        + " | ".join(F(k, r[k]) for k in K)
        + f" | {r['rc']} {r['agree']}/{r['live']}/acc={r['acc']} |"
    )
print("\nphase 1 ms per native block on every validator")
for t in tags:
    print(
        f"| {t} | "
        + " | ".join(f"{v} {x:.2f}" for v, x in rows[t]["ph1_all"].items())
        + " |"
    )
print(
    "\ncancel-all per validator: cancel-alls/blk, books visited / hit per cancel-all, index entries / traders at end"
)
for t in tags:
    print(
        f"| {t} | "
        + " | ".join(
            f"{v} {c.get('per_native_block')}, {c.get('books_visited_per_cancel_all')} / {c.get('books_hit_per_cancel_all')}, "
            f"{c.get('index_entries_end')} / {c.get('index_traders_end')}"
            for v, c in rows[t]["ca"].items()
        )
        + " |"
    )
arms = ["ref", "p21"]
M = {}
for a in arms:
    ts = [t for t in tags if t.startswith(a + "-r")]
    if not ts:
        continue
    M[a] = {k: st.mean(rows[t][k] for t in ts) for k in K}
    M[a]["n"] = len(ts)
    M[a]["ts"] = ts
    M[a]["spread"] = (
        {
            k: (max(rows[t][k] for t in ts) / min(rows[t][k] for t in ts) - 1) * 100
            for k in K
        }
        if len(ts) > 1
        else None
    )
print("\nper-arm means (warm cells excluded)")
print("| arm | n | " + " | ".join(K) + " |")
print("|---" * (len(K) + 2) + "|")
for a in M:
    print(f"| {a} | {M[a]['n']} | " + " | ".join(F(k, M[a][k]) for k in K) + " |")
print("\nspread % within arm (max/min-1)")
for a in M:
    if M[a]["spread"]:
        print(a, " ".join(f"{k}={M[a]['spread'][k]:.1f}" for k in K))
if "ref" in M and "p21" in M:
    print("\np21 vs ref: ratio | delta")
    print(
        " ".join(
            f"{k}={M['p21'][k] / M['ref'][k]:.3f}|{M['p21'][k] - M['ref'][k]:+.2f}"
            for k in K
        )
    )
    d = M["ref"]["ph1"] - M["p21"]["ph1"]
    rr = [rows[t]["ph1"] for t in M["ref"]["ts"]]
    pp = [rows[t]["ph1"] for t in M["p21"]["ts"]]
    print(
        f"\nGATE phase 1 drop (ref - p21, ms per native block, val0): {d:.2f} (ref {M['ref']['ph1']:.2f} [{min(rr):.2f}-{max(rr):.2f}], "
        f"p21 {M['p21']['ph1']:.2f} [{min(pp):.2f}-{max(pp):.2f}]); pairwise min..max drop {min(rr) - max(pp):.2f}..{max(rr) - min(pp):.2f}"
    )
    print(
        "gate >= 1.9:",
        "MET" if d >= 1.9 else "NOT MET",
        "| within 1.4-2.4 (extra cells rule):",
        "YES" if 1.4 <= d <= 2.4 else "no",
    )
    d3 = st.mean(
        st.mean(rows[t]["ph1_all"].values()) for t in M["ref"]["ts"]
    ) - st.mean(st.mean(rows[t]["ph1_all"].values()) for t in M["p21"]["ts"])
    print(f"phase 1 drop, mean of the 3 validators: {d3:.2f}")
