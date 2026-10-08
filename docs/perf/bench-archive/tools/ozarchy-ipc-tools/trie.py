#!/usr/bin/env python3
"""trie.py <label>...: trie-off comparison row per cell (summary.json, threads.py, cells.py CPU-s/1M, flush metrics)."""

import json, sys, importlib.util

T = "/home/oz/bench-results-matched/ozarchy-14236fa-tools/"
R = "/home/oz/bench-results-matched/"


def load(name):
    spec = importlib.util.spec_from_file_location(name, T + name + ".py")
    m = importlib.util.module_from_spec(spec)
    src = open(T + name + ".py").read().split("\nhdr =")[0].split("\nif __name__")[0]
    exec(compile(src, name, "exec"), m.__dict__)
    return m


th = load("threads")
ce = load("cells")


def metric(lab, name):
    for l in open(R + lab + "/metrics-after-val0.txt"):
        if l.startswith(name + " "):
            return float(l.split()[-1])
    return 0.0


print(
    "cell matched/s best60 engine/1k exec+exited/1k (exec,exited) flush_thr/blk flush_ph_ms root_ms state_write_ms "
    "exec_root_s/blk flush_s/blk handoff_ms commit_avg/p50 CPU-s/1M blocks"
)
for lab in sys.argv[1:]:
    s = json.load(open(R + lab + "/summary.json"))
    h = s["headline"]
    f = s["funnel_by_node"]["val0"]
    t = th.row(lab)
    c = ce.cpu_per_1m(lab, s)
    blocks = f["delta_blocks_committed_total"]
    fills = f["delta_orders_matched_total"]
    fl = s["phase_by_node"]["val0"]["phases"]["flush"]
    rc, rs = (
        metric(lab, "torus_exec_root_seconds_count"),
        metric(lab, "torus_exec_root_seconds_sum"),
    )
    fc, fs = (
        metric(lab, "torus_exec_flush_seconds_count"),
        metric(lab, "torus_exec_flush_seconds_sum"),
    )
    print(
        lab,
        round(h["matched_s_avg"]),
        round(h["matched_s_best60"]),
        h["engine_ms_per_1k_fills"],
        round(t["exec"] + t["exited"], 2),
        f"({t['exec']},{t['exited']})",
        round(t["flush"] * fills / 1000 / blocks, 1),
        fl.get("ms"),
        fl.get("root_ms"),
        fl.get("state_write_ms"),
        round(rs / rc * 1000, 2) if rc else None,
        round(fs / fc * 1000, 1) if fc else None,
        h.get("handoff_wait_ms"),
        f"{h['commit_interval_ms_avg']}/{h['commit_interval_ms_p50']}",
        round(c[0], 1) if c else None,
        blocks,
    )
