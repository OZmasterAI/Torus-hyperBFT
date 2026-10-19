#!/usr/bin/env python3
"""8r-attr report: per cell (run order) + arm means + paired B/A, C/A for the chosen rows. Run with python3 -I.
attr-report.py ATTR_JSON ROOT WINDOW"""

import datetime as dt
import json
import math
import pathlib
import statistics as st
import sys

T975 = {1: 12.706, 2: 4.303, 3: 3.182}
NODES = ("val0", "val1", "val2")
ORDER = [
    "r1-A",
    "r1-B",
    "r1-C",
    "r2-C",
    "r2-B",
    "r2-A",
    "r3-A",
    "r3-B",
    "r3-C",
    "r4-C",
    "r4-B",
    "r4-A",
]
CEST = dt.timezone(dt.timedelta(hours=2))


def g(c, win, n, key, stat="d"):
    x = c[win][n].get(key)
    return None if x is None else x[stat]


def per_s(c, win, key, scale=1.0):
    w = c["win"][win]
    v = [g(c, win, n, key) for n in NODES]
    return None if None in v else st.mean(v) / (w[1] - w[0]) * scale


def ratio_ms(c, win, s_key, c_key, scale=1000.0):
    v = []
    for n in NODES:
        a, b = g(c, win, n, s_key), g(c, win, n, c_key)
        if a is None or not b:
            return None
        v.append(a / b * scale)
    return st.mean(v)


def gmax(c, win, key):
    v = [g(c, win, n, key, "max") for n in NODES]
    return None if None in v else st.mean(v)


def gmean(c, win, key):
    v = [g(c, win, n, key, "mean") for n in NODES]
    return None if None in v else st.mean(v)


def gend(c, win, key):
    v = [g(c, win, n, key, "end") for n in NODES]
    return None if None in v else st.mean(v)


def sched(c, thr, k):
    return st.mean(c["sched"][n][thr][k] for n in NODES) / 1000.0


def vmstat(root, lab, t0, t1):
    rows = []
    for line in open(root / lab / "vmstat-1hz.txt"):
        p = line.split()
        if len(p) < 19 or not p[0].isdigit():
            continue
        ts = (
            dt.datetime.strptime(p[-2] + " " + p[-1], "%Y-%m-%d %H:%M:%S")
            .replace(tzinfo=CEST)
            .timestamp()
        )
        if t0 < ts <= t1:
            rows.append((float(p[0]), float(p[14])))
    return st.mean(r[0] for r in rows), st.mean(r[1] for r in rows)


def rows_for(c, win, root):
    w = c["win"][win]
    tb0 = c["tb0"]
    vr, vid = vmstat(root, c["label"], tb0 + w[0], tb0 + w[1])
    R = {}
    R["native blocks/s"] = per_s(c, win, "s:torus_exec_native_blocks_total")
    R["matched/s (k)"] = per_s(c, win, "s:torus_orders_matched_total", 1e-3)
    for db, p in (
        ("state", "s:torus_rocksdb_"),
        ("cons", "s:torus_rocksdb_consensus_"),
    ):
        R[f"{db} compaction CPU s/s"] = per_s(c, win, p + "compaction_cpu_micros", 1e-6)
        R[f"{db} compaction wall s/s"] = per_s(
            c, win, p + "compaction_sum_micros", 1e-6
        )
        R[f"{db} compactions /min"] = per_s(c, win, p + "compaction_count", 60)
        R[f"{db} compact read MB/s"] = per_s(c, win, p + "compact_read_bytes", 1e-6)
        R[f"{db} compact write MB/s"] = per_s(c, win, p + "compact_write_bytes", 1e-6)
        R[f"{db} flushes /min"] = per_s(c, win, p + "flush_count", 60)
        R[f"{db} flush write MB/s"] = per_s(c, win, p + "flush_write_bytes", 1e-6)
        R[f"{db} WAL MB/s"] = per_s(c, win, p + "wal_bytes", 1e-6)
        R[f"{db} stall micros (cum. end)"] = gend(c, win, p + "stall_micros")
        R[f"{db} write stalls (cum. end)"] = gend(c, win, p + "write_stall_count")
        R[f"{db} write stopped max"] = gmax(c, win, p + "write_stopped")
        R[f"{db} delayed write rate max"] = gmax(c, win, p + "delayed_write_rate")
        R[f"{db} L0 files max (any CF)"] = gmax(c, win, p + "l0_files_max")
        R[f"{db} pending compaction MB max"] = (
            gmax(c, win, p + "pending_compaction_bytes_all") or 0
        ) / 1e6
        R[f"{db} running compactions mean"] = gmean(c, win, p + "running_compactions")
        R[f"{db} DB writes /s"] = per_s(c, win, p + "db_write_count")
        R[f"{db} DB write mean us"] = ratio_ms(
            c, win, p + "db_write_sum_micros", p + "db_write_count", 1.0
        )
        R[f"{db} DB write p99 us (cum. end)"] = gend(c, win, p + "db_write_p99_micros")
        R[f"{db} DB write max ms (cum. end)"] = (
            gend(c, win, p + "db_write_max_micros") or 0
        ) / 1e3
        R[f"{db} write-group followers /s"] = per_s(c, win, p + "write_other")
    R["state cf_block_bodies L0 mean"] = gmean(
        c, win, "x:cf:state:cf_block_bodies:l0_files"
    )
    R["cons cf_native_shards L0 mean"] = gmean(
        c, win, "x:cf:consensus:cf_native_shards:l0_files"
    )
    R["cons cf_native_pending L0 mean"] = gmean(
        c, win, "x:cf:consensus:cf_native_pending:l0_files"
    )
    R["process CPU cores"] = per_s(c, win, "h:cpu_ticks", 0.01)
    R["host runqueue r (vmstat)"] = vr
    R["host idle % (vmstat)"] = vid
    R["nonvol ctx sw /s"] = per_s(c, win, "h:nonvoluntary_ctxt_switches")
    for nm, s_key, c_key in (
        (
            "exec block ms",
            "torus_exec_block_seconds_sum",
            "torus_exec_block_seconds_count",
        ),
        (
            "exec engine ms",
            "torus_exec_engine_seconds_sum",
            "torus_exec_engine_seconds_count",
        ),
        (
            "exec flush ms",
            "torus_exec_flush_seconds_sum",
            "torus_exec_flush_seconds_count",
        ),
        (
            "exec handoff wait ms",
            "torus_exec_handoff_wait_seconds_sum",
            "torus_exec_handoff_wait_seconds_count",
        ),
        (
            "exec chain ms",
            "torus_exec_chain_seconds_sum",
            "torus_exec_chain_seconds_count",
        ),
        (
            "exec body persist ms",
            "torus_exec_body_persist_seconds_sum",
            "torus_exec_block_seconds_count",
        ),
        (
            "exec body persist write ms",
            "torus_exec_body_persist_write_seconds_sum",
            "torus_exec_body_persist_write_seconds_count",
        ),
        (
            "exec state write db ms",
            "torus_exec_state_write_db_seconds_sum",
            "torus_exec_state_write_db_seconds_count",
        ),
        (
            "exec save books ms",
            "torus_exec_save_books_seconds_sum",
            "torus_exec_save_books_seconds_count",
        ),
        (
            "exec checkpoint wait ms",
            "torus_exec_checkpoint_wait_seconds_sum",
            "torus_exec_block_seconds_count",
        ),
        (
            "flush worker ms",
            "torus_flush_worker_seconds_sum",
            "torus_flush_worker_seconds_count",
        ),
        (
            "view duration ms",
            "torus_view_duration_seconds_sum",
            "torus_view_duration_seconds_count",
        ),
        (
            "view insert persist ms",
            "torus_view_insert_persist_seconds_sum",
            "torus_view_insert_persist_seconds_count",
        ),
        (
            "view qc->advance ms",
            "torus_view_qc_to_advance_seconds_sum",
            "torus_view_qc_to_advance_seconds_count",
        ),
        (
            "commit persist ms",
            "torus_commit_persist_seconds_sum",
            "torus_commit_persist_seconds_count",
        ),
        (
            "commit persist write ms",
            "torus_commit_persist_write_seconds_sum",
            "torus_commit_persist_seconds_count",
        ),
        (
            "commit body encode ms",
            "torus_commit_body_encode_seconds_sum",
            "torus_commit_persist_seconds_count",
        ),
        (
            "on_committed_block ms",
            "torus_on_committed_block_seconds_sum",
            "torus_on_committed_block_seconds_count",
        ),
        (
            "validate block ms",
            "torus_validate_block_seconds_sum",
            "torus_validate_block_seconds_count",
        ),
        (
            "commit interval ms",
            "torus_commit_interval_seconds_sum",
            "torus_commit_interval_seconds_count",
        ),
    ):
        R[nm] = ratio_ms(c, win, "s:" + s_key, "s:" + c_key)
    R["vote state write ms"] = ratio_ms(
        c,
        win,
        "x:torus_vote_state_write_seconds_sum",
        "x:torus_vote_state_write_seconds_count",
    )
    R["DA GC s/s (any thread)"] = per_s(c, win, "x:torus_da_gc_seconds_sum")
    R["exec block busy s/s"] = per_s(c, win, "s:torus_exec_block_seconds_sum")
    R["exec chain busy s/s"] = per_s(c, win, "s:torus_exec_chain_seconds_sum")
    R["exec flush busy s/s"] = per_s(c, win, "s:torus_exec_flush_seconds_sum")
    R["flush worker busy s/s"] = per_s(c, win, "s:torus_flush_worker_seconds_sum")
    R["on_committed busy s/s"] = per_s(c, win, "s:torus_on_committed_block_seconds_sum")
    fills = per_s(c, win, "s:torus_orders_matched_total")
    R["exec block us per fill"] = R["exec block busy s/s"] / fills * 1e6
    R["exec queue depth mean"] = gmean(c, win, "s:torus_exec_queue_depth")
    R["pruner blocks removed /s"] = per_s(c, win, "x:torus_pruner_blocks_removed_total")
    # load-window thread CPU (schedstat; not windowed)
    R["[load] exec on-CPU s"] = sched(c, "torus-execution", "on_cpu_ms")
    R["[load] exec runqueue wait s"] = sched(c, "torus-execution", "runqueue_wait_ms")
    R["[load] hotstuff on-CPU s"] = sched(c, "hotstuff-algo", "on_cpu_ms")
    R["[load] hotstuff runqueue wait s"] = sched(c, "hotstuff-algo", "runqueue_wait_ms")
    pu = st.mean(
        (c["proc"][n]["user_ms"] + c["proc"][n]["sys_ms"]) / 1000.0 for n in NODES
    )
    R["[load] process CPU s"] = pu
    R["[load] other threads CPU s"] = (
        pu - R["[load] exec on-CPU s"] - R["[load] hotstuff on-CPU s"]
    )
    R["[load] fills (M)"] = st.mean(c["proc"][n]["fills"] for n in NODES) / 1e6
    return R


def ci(xs):
    v = [math.log(x) for x in xs]
    m = st.mean(v)
    hw = T975[len(v) - 1] * st.stdev(v) / math.sqrt(len(v))
    return math.exp(m), math.exp(m - hw), math.exp(m + hw)


def dci(xs):
    m = st.mean(xs)
    hw = T975[len(xs) - 1] * st.stdev(xs) / math.sqrt(len(xs))
    return m, m - hw, m + hw


def fmt(x):
    if x is None:
        return "n/a"
    a = abs(x)
    return (
        f"{x:,.0f}"
        if a >= 1000
        else f"{x:.1f}"
        if a >= 100
        else f"{x:.2f}"
        if a >= 1
        else f"{x:.3f}"
        if a >= 0.01
        else f"{x:.4f}"
    )


def paired(vals, reps, arm):
    a = [vals[(r, "A")] for r in reps]
    b = [vals[(r, arm)] for r in reps]
    up = sum(y > x for x, y in zip(a, b))
    if all(x and x > 0 for x in a) and all(y and y > 0 for y in b):
        m, lo, hi = ci([y / x for x, y in zip(a, b)])
        return f"{m:.3f} [{lo:.3f}, {hi:.3f}] {up}/4"
    m, lo, hi = dci([y - x for x, y in zip(a, b)])
    return f"diff {fmt(m)} [{fmt(lo)}, {fmt(hi)}] {up}/4"


def main():
    d = json.load(open(sys.argv[1]))
    root = pathlib.Path(sys.argv[2])
    win = sys.argv[3]
    by = {c["label"].split("300m-")[1]: c for c in d.values()}
    R = {k: rows_for(by[k], win, root) for k in ORDER}
    names = list(R[ORDER[0]].keys())
    reps = ["r1", "r2", "r3", "r4"]
    print(f"### window {win} (node mean per cell; columns in run order)")
    print("| metric | " + " | ".join(ORDER) + " |")
    print("|---" * (len(ORDER) + 1) + "|")
    for nm in names:
        print(f"| {nm} | " + " | ".join(fmt(R[k][nm]) for k in ORDER) + " |")
    print(
        f"\n### arm means (sd) and paired ratios, window {win}; 'k/4' = pairs where the arm is higher than A"
    )
    print("| metric | A | B | C | B/A [95% CI] | C/A [95% CI] |")
    print("|---|---|---|---|---|---|")
    for nm in names:
        vals = {(k[:2], k[3]): R[k][nm] for k in ORDER}
        if any(v is None for v in vals.values()):
            continue
        cols = []
        for arm in "ABC":
            xs = [vals[(r, arm)] for r in reps]
            cols.append(f"{fmt(st.mean(xs))} ({fmt(st.stdev(xs))})")
        print(
            f"| {nm} | "
            + " | ".join(cols)
            + f" | {paired(vals, reps, 'B')} | {paired(vals, reps, 'C')} |"
        )


if __name__ == "__main__":
    main()
