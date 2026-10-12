"""ozarchy-p3c1: per arm exec / chain / load-generator context (python3 -I)."""

import json
import re
import statistics as st

R = "/home/oz/bench-results-matched/ozarchy-p3c1-300m-"
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


rows = {}
for a in ARMS:
    r = rows[a] = {
        k: []
        for k in (
            "busy",
            "chain",
            "txb",
            "ci",
            "sub",
            "stall",
            "step",
            "inflight_end",
            "hand",
            "pipe",
        )
    }
    for i in range(1, 5):
        t = f"{RT[i]}-{a}"
        s = json.load(open(R + t + "/summary.json"))
        h = s["headline"]
        r["busy"].append(
            st.mean(s["phase_by_node"][v]["exec_thread_busy_fraction"] for v in V)
        )
        r["chain"].append(h.get("chain_ms"))
        r["txb"].append(h.get("txs_per_block_avg"))
        r["ci"].append(h.get("commit_interval_ms_avg"))
        r["hand"].append(h.get("handoff_wait_ms"))
        r["pipe"].append(h.get("pipelined_ms"))
        r["sub"].append(s["ingest"]["bench_submit_rate"])
        r["inflight_end"].append(s["ingest"]["in_flight"]["in_flight_at_end"])
        sl, stp = bench_stalls(t)
        r["stall"].append(sl)
        r["step"].append(stp)
print(
    "arm  exec busy  chain_ms  pipelined_ms  handoff_ms  txs/blk  commit_ms  submit/s  submit-stall s/cell  RPC head step (median)  in-flight at end"
)
for a in ARMS:
    r = rows[a]
    print(
        f"{a}  {st.mean(r['busy']):.3f}  {st.mean(r['chain']):6.1f}  {st.mean(r['pipe']):6.2f}  {st.mean(r['hand']):5.2f}  {st.mean(r['txb']):6.1f}  "
        f"{st.mean(r['ci']):6.1f}  {st.mean(r['sub']):7.0f}  {st.mean(r['stall']):5.1f} {r['stall']}  {r['step']}  {st.mean(r['inflight_end']):7.0f}"
    )
json.dump(
    rows,
    open("/home/oz/bench-results-matched/ozarchy-p3c1-run/analyst/extra.json", "w"),
    indent=1,
)
