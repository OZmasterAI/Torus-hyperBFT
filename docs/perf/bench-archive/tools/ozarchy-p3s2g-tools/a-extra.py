"""ozarchy-p3c1: per arm exec / chain / load-generator context (python3 -I)."""

import json
import re
import statistics as st

R = "/home/oz/bench-results-matched/"
RT = {1: "r1", 2: "r2", 3: "r3", 4: "r4b"}
ARMS = ("cd", "sd", "cm", "sm")
V = ("val0", "val1", "val2")
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def bench_stalls(tag):
    """bench.log progress lines (~2 per s): seconds with no new submission (runs of >= 3 equal lines), and the RPC head steps."""
    last = None
    run = 0
    stall_lines = 0
    heads = []
    for line in open(R + tag + "/bench.log", errors="replace"):
        line = ANSI.sub("", line)
        m = re.match(r"^\[(\d+)s\] submitted: ([\d,]+) actions .*blk: #(\d+)", line)
        if not m:
            continue
        n = int(m.group(2).replace(",", ""))
        heads.append(int(m.group(3)))
        if n == last:
            run += 1
        else:
            if run >= 3:
                stall_lines += run
            run = 0
        last = n
    if run >= 3:
        stall_lines += run
    steps = [b - a for a, b in zip(heads, heads[1:]) if b != a]
    return stall_lines / 2.0, (st.median(steps) if steps else None)


SETS = {
    "p3s2g cd (commit, c97f56f9)": ["ozarchy-p3s2g-300m-%s-cd" % r for r in ("r1", "r2", "r3", "r4")],
    "p3s2g sd (commit, step 2)": ["ozarchy-p3s2g-300m-%s-sd" % r for r in ("r1", "r2", "r3", "r4")],
    "p3c1 cd (head, 2725e321)": ["ozarchy-p3c1-300m-%s-cd" % r for r in ("r1", "r2", "r3", "r4b")],
    "p3c1 sd (head, step 1)": ["ozarchy-p3c1-300m-%s-sd" % r for r in ("r1", "r2", "r3", "r4b")],
    "ict head (step 2 12e11c7e, 1 cell)": ["ozarchy-ict-smoke-300m-head"],
    "ict commit (step 2 12e11c7e, 1 cell)": ["ozarchy-ict-smoke-300m-commit"],
}
rows = {}
for a, tags in SETS.items():
    r = rows[a] = {k: [] for k in ("busy", "chain", "txb", "ci", "sub", "stall", "step", "inflight_end", "hand", "pipe", "eng", "cpu", "ms", "blk", "apb", "fpb")}
    for t in tags:
        s = json.load(open(R + t + "/summary.json"))
        h = s["headline"]
        r["busy"].append(st.mean(s["phase_by_node"][v]["exec_thread_busy_fraction"] for v in V))
        r["chain"].append(h.get("chain_ms")); r["txb"].append(h.get("txs_per_block_avg")); r["ci"].append(h.get("commit_interval_ms_avg"))
        r["hand"].append(h.get("handoff_wait_ms")); r["pipe"].append(h.get("pipelined_ms")); r["sub"].append(s["ingest"]["bench_submit_rate"])
        r["inflight_end"].append(s["ingest"]["in_flight"]["in_flight_at_end"]); r["eng"].append(h["engine_ms_per_1k_fills"])
        r["ms"].append(h["matched_s_avg"]); r["blk"].append(h["native_blk_s"]); r["apb"].append(h["actions_per_exec_block"]); r["fpb"].append(h["fills_per_native_block"])
        r["cpu"].append(st.mean((s["proc_cpu_by_node"][v]["whole_run"]["user_ms"] + s["proc_cpu_by_node"][v]["whole_run"]["sys_ms"]) / 1000 / (s["proc_cpu_by_node"][v]["whole_run"]["fills"] / 1e6) for v in V))
        sl, stp = bench_stalls(t)
        r["stall"].append(sl); r["step"].append(stp)
print("set | exec busy | chain_ms | actions/blk | fills/blk | engine ms/1k fills | CPU-s/1M | matched/s | native blk/s | submit/s | submit-stall s/cell | head step median | in-flight end")
for a, r in rows.items():
    m = lambda k: st.mean(r[k])
    print(f"{a} | {m('busy'):.3f} | {m('chain'):.1f} | {m('apb'):.1f} | {m('fpb'):.0f} | {m('eng'):.2f} | {m('cpu'):.2f} | {m('ms'):.0f} | {m('blk'):.2f} | {m('sub'):.0f} | {m('stall'):.1f} {r['stall']} | {r['step']} | {m('inflight_end'):.0f}")
json.dump(rows, open("/home/oz/bench-results-matched/ozarchy-p3s2g-run/analyst/extra.json", "w"), indent=1)
