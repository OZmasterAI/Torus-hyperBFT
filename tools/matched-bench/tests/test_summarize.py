#!/usr/bin/env python3
"""Unit test for tools/matched-bench/summarize.py duration-parity fields.

Builds a synthetic 1 Hz sampler.csv whose val0 matched counter grows at
40 000/s for the first 60 s of the bench window and 20 000/s afterwards
(300 s bench), runs summarize.py on it and checks:

  headline.matched_s_avg       = whole bench window (unchanged semantics)  -> 24 000
  headline.matched_s_first120  = window-avg over [t_bench0, t_bench0+120]  -> 30 000
  headline.matched_s_early60   = [t_bench0, t_bench0+60]                    -> 40 000
  headline.matched_s_late60    = [t_bench1-60, t_bench1]                    -> 20 000
  headline.decay_ratio         = late60 / early60                           -> 0.5

and that a 120 s cell reports first120 == avg (same window).

Run:  python3 tools/matched-bench/tests/test_summarize.py
"""

import csv, json, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SUMMARIZE = os.path.join(HERE, "..", "summarize.py")
COLS = [
    "ts",
    "node",
    "torus_orders_matched_total",
    "torus_orders_placed_accepted_total",
    "torus_block_height",
    "torus_blocks_committed_total",
    "torus_native_actions_processed_total",
    "torus_orders_resting_total",
    "torus_exec_queue_depth",
    "torus_mempool_native_size",
]


def make_sampler(path, t0, dur, early_rate, late_rate, split_s=60, tail_s=30):
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(COLS)
        matched = {"val0": 0.0, "val1": 0.0, "val2": 0.0}
        for i in range(-5, dur + tail_s + 1):
            ts = t0 + i
            for node in ("val0", "val1", "val2"):
                # counter sampled at ts reflects the work done in (ts-1, ts]
                if 0 < i <= dur:
                    matched[node] += early_rate if i <= split_s else late_rate
                w.writerow(
                    [
                        ts,
                        node,
                        matched[node],
                        matched[node] * 1.5,
                        100 + i,
                        100 + i,
                        matched[node] * 2,
                        10,
                        0,
                        0,
                    ]
                )


def run(out, t0, dur):
    t1 = t0 + dur
    td = t1 + 20
    cmd = [
        sys.executable,
        SUMMARIZE,
        "--out",
        out,
        "--label",
        "unit",
        "--markets",
        "10",
        "--dur",
        str(dur),
        "--rate",
        "76000",
        "--senders",
        "5000",
        "--t-bench0",
        str(t0),
        "--t-bench1",
        str(t1),
        "--t-drain",
        str(td),
        "--drained",
        "1",
        "--bench-rc",
        "0",
    ]
    p = subprocess.run(
        cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    with open(os.path.join(out, "summary.json")) as f:
        return json.load(f), p.stdout


def close(a, b, tol=1.0):
    return a is not None and abs(a - b) <= tol


def main():
    fails = []
    t0 = 1_787_000_000
    with tempfile.TemporaryDirectory() as d:
        make_sampler(os.path.join(d, "sampler.csv"), t0, 300, 40000, 20000)
        s, out = run(d, t0, 300)
        h = s["headline"]
        checks = [
            ("matched_s_avg (300 s, unchanged)", h.get("matched_s_avg"), 24000),
            ("matched_s_first120", h.get("matched_s_first120"), 30000),
            ("matched_s_early60", h.get("matched_s_early60"), 40000),
            ("matched_s_late60", h.get("matched_s_late60"), 20000),
        ]
        for name, got, want in checks:
            if not close(got, want):
                fails.append(f"{name}: got {got} want {want}")
        dr = h.get("decay_ratio")
        if dr is None or abs(dr - 0.5) > 0.01:
            fails.append(f"decay_ratio: got {dr} want 0.5")
        # per-node funnel carries the same fields
        f0 = s["funnel_by_node"]["val0"]
        for k in (
            "matched_s_first120",
            "matched_s_early60",
            "matched_s_late60",
            "decay_ratio",
        ):
            if k not in f0:
                fails.append(f"funnel_by_node.val0 missing {k}")
        # every node in the synthetic run is identical -> same first120 on val1/val2
        if not close(s["funnel_by_node"]["val2"].get("matched_s_first120"), 30000):
            fails.append("val2 matched_s_first120 mismatch")
        # the fields are printed on the SUMMARY line too (round records grep it)
        if "first120=" not in out or "decay=" not in out:
            fails.append(f"SUMMARY line lacks first120=/decay=: {out.splitlines()[:1]}")

    with tempfile.TemporaryDirectory() as d:
        # a 120 s cell: first120 window == bench window -> same number as matched_s_avg
        make_sampler(os.path.join(d, "sampler.csv"), t0, 120, 40000, 20000)
        s, _ = run(d, t0, 120)
        h = s["headline"]
        if not close(h.get("matched_s_first120"), h.get("matched_s_avg") or -1):
            fails.append(
                f"120 s cell: first120 {h.get('matched_s_first120')} != avg {h.get('matched_s_avg')}"
            )
        if not close(h.get("matched_s_avg"), 30000):
            fails.append(
                f"120 s cell: matched_s_avg got {h.get('matched_s_avg')} want 30000"
            )

    if fails:
        print("FAIL")
        for x in fails:
            print("  - " + x)
        sys.exit(1)
    print(
        "PASS test_summarize: matched_s_avg unchanged; first120/early60/late60/decay_ratio correct"
    )


if __name__ == "__main__":
    main()
