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
    "view_vote_gather",
    "view_qc_to_advance",
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


def _kv(text):
    return {k: (None if v == "-" else int(v)) for k, v in (f.split("=", 1) for f in text.split())}


def _node_logs(cell):
    for sub in ("run", ""):
        paths = {n: os.path.join(cell, sub, n + ".log") for n in ("val0", "val1", "val2")}
        if all(os.path.exists(p) for p in paths.values()):
            return paths
    return None


def _stats(xs):
    xs = sorted(xs)
    pick = lambda q: xs[min(len(xs) - 1, int(q * len(xs)))]
    return dict(n=len(xs), p50=pick(0.5), p90=pick(0.9), mean=round(sum(xs) / len(xs), 2))


def view_join(cell, window=None):
    """s70 proposal->QC split: join each validator's `view_close` trace line
    (TORUS_BODY_FETCH_TRACE=1) per view into critical-path segments, ms.
    L = the view's leader (the node that proposed), N = the collector (the node
    whose PC is for that view; under rotation the next leader). `window` is
    (t0, t1) unix seconds on L's view start. None when the logs are absent."""
    paths = _node_logs(cell)
    if not paths:
        return None
    closes, votes_at = {}, {}  # node -> view -> record; (node, view) -> [admission unix_us]
    for node, path in paths.items():
        closes[node] = {}
        with open(path, errors="replace") as f:
            for line in f:
                if "body_fetch_diag view_close: " in line:
                    r = _kv(line.split("body_fetch_diag view_close: ", 1)[1])
                    closes[node][r["view"]] = r
                elif "body_fetch_diag admission: " in line and " kind=vote " in line:
                    a = dict(x.split("=", 1) for x in line.split("admission: ", 1)[1].split())
                    votes_at.setdefault((node, int(a["view"])), []).append(int(a["unix_us"]))
    pcs = {r["pc_view"]: (node, r) for node, vs in closes.items() for r in vs.values() if r["pc_view"] is not None}
    segs = {k: [] for k in ("propose", "header_rx_last", "vote_sent_last", "vote_to_pc", "vote_queue",
                            "vote_gather", "pc_to_advance", "advance_to_leader", "cycle")}
    joined = no_pc = 0
    last_voter = {}
    ms = lambda a, b: (b - a) / 1000.0
    for view in sorted({v for vs in closes.values() for v in vs}):
        lead = [n for n in closes if closes[n].get(view, {}).get("propose_us") is not None]
        if len(lead) != 1:
            continue
        L, lr = lead[0], closes[lead[0]][view]
        if window and not window[0] <= lr["start_us"] / 1e6 <= window[1]:
            continue
        if view not in pcs:
            no_pc += 1
            continue
        N, nr = pcs[view]
        recs = {n: closes[n].get(view) for n in closes}
        rx = [r["proposal_rx_us"] for n, r in recs.items() if n != L and r and r["proposal_rx_us"] is not None]
        sent = {n: r["vote_us"] for n, r in recs.items() if r and r["vote_us"] is not None}
        if not rx or not sent:
            continue
        joined += 1
        last = max(sent, key=sent.get)
        role = "leader" if last == L else "collector" if last == N else "follower"
        last_voter[role] = last_voter.get(role, 0) + 1
        segs["propose"].append(ms(lr["start_us"], lr["propose_us"]))
        segs["header_rx_last"].append(ms(lr["propose_us"], max(rx)))
        segs["vote_sent_last"].append(ms(lr["propose_us"], sent[last]))
        segs["vote_to_pc"].append(ms(sent[last], nr["pc_us"]))
        if votes_at.get((N, view)):
            segs["vote_queue"].append(ms(max(votes_at[(N, view)]), nr["pc_us"]))
        if nr["first_vote_rx_us"] is not None:
            segs["vote_gather"].append(ms(nr["first_vote_rx_us"], nr["pc_us"]))
        segs["pc_to_advance"].append(ms(nr["pc_us"], nr["end_us"]))
        if L != N:
            segs["advance_to_leader"].append(ms(nr["end_us"], lr["end_us"]))
        segs["cycle"].append(ms(lr["start_us"], nr["end_us"]))
    return dict(joined=joined, no_pc=no_pc, last_voter=last_voter,
                segments={k: _stats(v) for k, v in segs.items() if v})


def print_view_join(cell):
    window = None
    try:
        t = json.load(open(os.path.join(cell, "summary.json")))["timing"]
        window = (t["t_bench0"], t["t_bench1"])
    except (OSError, ValueError, KeyError):
        pass
    j = view_join(cell, window)
    if j is None:
        return
    print(f"  PROPOSAL->QC JOIN ms ({'load window' if window else 'whole run'}; "
          f"{j['joined']} views joined, {j['no_pc']} without a PC; last voter {j['last_voter']}):")
    for k, v in j["segments"].items():
        print(f"    {k.ljust(20)} p50 {v['p50']:8.1f}  p90 {v['p90']:8.1f}  mean {v['mean']:8.1f}  n {v['n']}")


if __name__ == "__main__":
    for cell in sys.argv[1:]:
        main(cell)
    for cell in sys.argv[1:]:
        hist_tail(cell)
    for cell in sys.argv[1:]:
        print_view_join(cell)
