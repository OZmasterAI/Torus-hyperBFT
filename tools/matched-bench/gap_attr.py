#!/usr/bin/env python3
"""s55 gap attribution: decompose full-block interval into views/block x view ms + dead time.
Usage: gap_attr.py <cell-dir>..."""

import csv, json, os, sys

STAGES = [
    "view_duration",
    "view_propose_delay",
    "block_build",
    "view_propose_finalize",
    "view_qc_collect",
    "view_proposal_arrival",
    "view_vote_delay",
    "view_insert_persist",
    "validate_block",
    "validate_block_da_reconstruct",
    "on_committed_block",
    "mempool_remove_committed",
    "commit_persist",
]


def load_window(d, cell):
    """views, committed, timeouts per node over [t_bench0, t_bench1] from sampler.csv."""
    t0, t1 = d["timing"]["t_bench0"], d["timing"]["t_bench1"]
    out = {}
    with open(os.path.join(cell, "sampler.csv")) as f:
        rows = list(csv.DictReader(f))
    for node in sorted({r["node"] for r in rows}):
        nr = [r for r in rows if r["node"] == node and t0 <= int(r["ts"]) <= t1]
        if len(nr) < 2:
            continue
        a, b = nr[0], nr[-1]
        g = lambda r, k: float(r.get(k) or 0)
        span = int(b["ts"]) - int(a["ts"])
        out[node] = dict(
            span_s=span,
            views=g(b, "torus_consensus_view") - g(a, "torus_consensus_view"),
            committed=g(b, "torus_blocks_committed_total")
            - g(a, "torus_blocks_committed_total"),
            timeouts=g(b, "torus_consensus_timeout_total_total")
            - g(a, "torus_consensus_timeout_total_total"),
            native=g(b, "torus_exec_native_blocks_total")
            - g(a, "torus_exec_native_blocks_total"),
        )
    return out


def main(cell):
    d = json.load(open(os.path.join(cell, "summary.json")))
    h, c = d["headline"], d["cell"]
    print(
        f"\n===== {d['label']}  commit={d.get('commit')}  cap={c['block_cap']}  idle_blk_s={d.get('idle_blk_s')}  agree={h.get('agreement_verdict')}"
    )
    g = lambda k: h.get(k, "n/a")
    print(
        f"  matched/s {g('matched_s_avg')}  native_blk_s {g('native_blk_s')}  actions/blk {g('actions_per_exec_block')}  "
        f"chain_ms {g('chain_ms')}  wall/committed {g('wall_ms_per_committed_block')}  "
        f"commit_int avg/p50/p95 {g('commit_interval_ms_avg')}/{g('commit_interval_ms_p50')}/{g('commit_interval_ms_p95')}  "
        f"timeouts(whole) {g('consensus_timeouts')}"
    )
    lw = load_window(d, cell)
    print("  LOAD WINDOW (sampler):")
    for n, v in lw.items():
        vpb = v["views"] / v["committed"] if v["committed"] else float("nan")
        ms_per_view = 1000 * v["span_s"] / v["views"] if v["views"] else float("nan")
        ms_per_commit = (
            1000 * v["span_s"] / v["committed"] if v["committed"] else float("nan")
        )
        print(
            f"    {n}: span {v['span_s']}s views {v['views']:.0f} committed {v['committed']:.0f} native {v['native']:.0f} "
            f"timeouts {v['timeouts']:.0f} | views/committed {vpb:.3f} | ms/view {ms_per_view:.1f} | ms/committed {ms_per_commit:.1f}"
        )
    cb = d.get("consensus_by_node") or {}
    if cb:
        print(f"  VIEW STAGES ms (window: {cb['val0'].get('window')}):")
        print("    " + "stage".ljust(30) + "".join(n.rjust(10) for n in sorted(cb)))
        for s in STAGES:
            print(
                "    "
                + s.ljust(30)
                + "".join(
                    f"{cb[n].get(s + '_ms', float('nan')):10.1f}" for n in sorted(cb)
                )
            )
        print(
            "    "
            + "views_per_committed(whole)".ljust(30)
            + "".join(
                f"{cb[n].get('views_per_committed_block', float('nan')):10.3f}"
                for n in sorted(cb)
            )
        )
        print(
            "    "
            + "cons_thread_ms/view_est".ljust(30)
            + "".join(
                f"{cb[n].get('consensus_thread_ms_per_view_est', float('nan')):10.1f}"
                for n in sorted(cb)
            )
        )
        # Load-window per-view estimate: strip idle/drain views at the idle view time.
        idle_view_ms = 1000.0 / d["idle_blk_s"] if d.get("idle_blk_s") else None
        if idle_view_ms:
            print(
                f"  LOAD-WINDOW VIEW MS EST (idle views stripped at {idle_view_ms:.0f} ms each):"
            )
            for n in sorted(cb):
                tot_views = cb[n].get("view_duration_count") or 0
                tot_ms = tot_views * cb[n].get("view_duration_ms", 0)
                lv = lw.get(n, {}).get("views", 0)
                if lv and tot_views > lv:
                    est = (tot_ms - (tot_views - lv) * idle_view_ms) / lv
                    print(
                        f"    {n}: ~{est:.0f} ms/view under load (whole-run {cb[n]['view_duration_ms']:.1f}, {tot_views:.0f} views total, {lv:.0f} in load)"
                    )
    sb = d.get("sched_by_node") or {}
    if sb:
        print("  HOTSTUFF THREAD schedstat (per committed block, load window):")
        for n in sorted(sb):
            t = sb[n]["threads"].get("hotstuff-algo", {})
            print(
                f"    {n}: on_cpu {t.get('on_cpu_ms_per_committed_block')} ms  rq_wait {t.get('runqueue_wait_ms_per_committed_block')} ms"
            )
    pb = d.get("phase_by_node", {}).get("val0", {})
    keys = [
        "block_ms",
        "chain_ms",
        "engine_ms",
        "save_books_ms",
        "flush_ms",
        "pipelined_ms",
        "body_persist_ms",
        "exec_thread_busy_fraction",
    ]
    print("  EXEC val0: " + "  ".join(f"{k}={pb[k]}" for k in keys if k in pb))


for cell in sys.argv[1:]:
    main(cell)


def hist_tail(cell, name="torus_view_duration_seconds"):
    import re
    def h(path):
        b, s, c = {}, None, None
        for line in open(path):
            if line.startswith(name + "_bucket"):
                le = float(re.search(r'le="([^"]+)"', line).group(1).replace("+Inf", "inf"))
                b[le] = float(line.split()[-1])
            elif line.startswith(name + "_sum "):
                s = float(line.split()[-1])
            elif line.startswith(name + "_count "):
                c = float(line.split()[-1])
        return b, s, c
    for node in ["val0", "val1", "val2"]:
        try:
            b0, s0, c0 = h(os.path.join(cell, f"metrics-before-{node}.txt"))
            b1, s1, c1 = h(os.path.join(cell, f"metrics-after-{node}.txt"))
        except FileNotFoundError:
            return
        n = c1 - c0
        if not n:
            return
        cum = {k: b1[k] - b0.get(k, 0) for k in b1}
        gt512 = n - cum.get(0.512, n); gt1024 = n - cum.get(1.024, n); gt2048 = n - cum.get(2.048, n)
        # lower-bound wall share of slow views: each >512 view costs >=512 ms etc.
        lb_ms = 0.512 * (gt512 - gt1024) * 1000 + 1.024 * (gt1024 - gt2048) * 1000 + 2.048 * gt2048 * 1000
        print(f"  VIEW TAIL {node} (whole run): views {n:.0f} mean {1000*(s1-s0)/n:.0f} ms | >512ms {gt512:.0f} ({100*gt512/n:.1f}%) | >1.024s {gt1024:.0f} | >2.048s {gt2048:.0f} | slow-view wall >= {lb_ms/1000:.1f} s of {(s1-s0):.0f} s total view time")


for cell in sys.argv[1:]:
    hist_tail(cell)
