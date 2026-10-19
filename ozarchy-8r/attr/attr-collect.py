#!/usr/bin/env python3
"""8r-attr collector (archy s68). Run with python3 -I. Read-only on cell dirs.

attr-collect.py RUN_DIR ROOT OUT_JSON
Per cell (r1..r4 x A/B/C), windows from RUN_DIR/analyst/r8-analyse.json meta (whole = +0..w1, post = w0..w1,
relative to summary.json t_bench0). Per node:
  * every sampler.csv column: delta over the window (counters), mean and max over the window (gauges), end value;
  * selected metrics-1hz series not in sampler.csv (pruner, vote state write, DA flush / multiget, GC, per-CF);
  * host-1hz cpu_ticks (process CPU) and ctx switch deltas;
  * schedstat / proc CPU from summary.json (load window);
  * state DB LOG compaction / flush events per CF.
"""

import csv
import gzip
import json
import pathlib
import re
import statistics as st
import sys

NODES = ("val0", "val1", "val2")
EXTRA = frozenset(
    (
        "torus_pruner_blocks_removed_total",
        "torus_vote_state_write_seconds_sum",
        "torus_vote_state_write_seconds_count",
        "torus_validate_block_da_flush_seconds_sum",
        "torus_validate_block_da_flush_seconds_count",
        "torus_validate_block_da_multiget_seconds_sum",
        "torus_validate_block_da_multiget_seconds_count",
        "torus_validate_block_da_wait_seconds_sum",
        "torus_validate_block_da_wait_seconds_count",
        "torus_da_gc_seconds_sum",
        "torus_da_gc_seconds_count",
        "torus_da_bodies_gc_deleted_total",
        "torus_da_gc_handoff_seconds_sum",
        "torus_post_flush_writer_queued_batches",
        "torus_exec_checkpoint_live_layers",
        "torus_exec_checkpoint_live_bytes",
        "torus_jemalloc_resident_bytes",
        "torus_jemalloc_allocated_bytes",
    )
)
CFRE = re.compile(
    r'^torus_rocksdb_(l0_files|pending_compaction_bytes|memtable_bytes)\{cf="([a-z_]+)"(,db="consensus")?\}$'
)
HOSTK = ("cpu_ticks", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches", "VmRSS")


def num(x):
    try:
        return float(x)
    except (TypeError, ValueError):
        return None


def series_stats(ser, t0, t1):
    """ser sorted [(ts, v)]: delta = last<=t1 minus last<=t0; mean/max over t0<ts<=t1; end value."""
    a = b = None
    inwin = []
    for ts, v in ser:
        if ts <= t0:
            a = v
        if ts <= t1:
            b = v
        if t0 < ts <= t1:
            inwin.append(v)
    return {
        "d": (b - a) if (a is not None and b is not None) else None,
        "mean": st.mean(inwin) if inwin else None,
        "max": max(inwin) if inwin else None,
        "end": b,
    }


def add(ser, n, k, ts, v):
    if v is not None:
        ser[n].setdefault(k, []).append((ts, v))


def sampler(d):
    ser = {n: {} for n in NODES}
    with open(d / "sampler.csv") as f:
        rd = csv.DictReader(f)
        cols = [c for c in rd.fieldnames if c not in ("ts", "node")]
        rows = [r for r in rd if r["node"] in ser]
    for r in rows:
        ts = float(r["ts"])
        for col in cols:
            add(ser, r["node"], col, ts, num(r[col]))
    return ser


def m1hz_name(raw):
    m = CFRE.match(raw)
    if m:
        return f"cf:{'consensus' if m.group(3) else 'state'}:{m.group(2)}:{m.group(1)}"
    return raw if raw in EXTRA else None


def m1hz(d):
    ser = {n: {} for n in NODES}
    cur = None
    with gzip.open(d / "metrics-1hz.txt.gz", "rt", errors="replace") as f:
        for line in f:
            if line.startswith("T "):
                p = line.split()
                cur = (float(p[1]), p[2]) if len(p) > 3 and p[3] == "ok" else None
                continue
            if cur is None or not line.startswith("torus_"):
                continue
            p = line.rstrip("\n").rsplit(" ", 1)
            name = m1hz_name(p[0]) if len(p) == 2 else None
            if name is not None:
                add(ser, cur[1], name, cur[0], num(p[1]))
    return ser


def host(d):
    ser = {n: {} for n in NODES + ("host",)}
    with open(d / "host-1hz.tsv") as fh:
        rows = list(csv.DictReader(fh, delimiter="\t"))
    for r in rows:
        ts = float(r["ts"])
        for n in NODES:
            for k in HOSTK:
                add(ser, n, k, ts, num(r.get(f"{n}_{k}")))
        for k in ("Dirty_kB", "Writeback_kB"):
            add(ser, "host", k, ts, num(r.get(k)))
    return ser


def on_compaction_done(ev, jobs, out_):
    cf, insz, why = jobs.get(ev["job"], ("?", 0, "?"))
    o = out_.setdefault(
        cf,
        {
            "n": 0,
            "cpu_s": 0.0,
            "wall_s": 0.0,
            "in_mb": 0.0,
            "out_mb": 0.0,
            "rec_in": 0,
            "rec_out": 0,
            "reasons": {},
        },
    )
    o["n"] += 1
    o["cpu_s"] += ev.get("compaction_time_cpu_micros", 0) / 1e6
    o["wall_s"] += ev.get("compaction_time_micros", 0) / 1e6
    o["in_mb"] += insz / 1e6
    o["out_mb"] += ev.get("total_output_size", 0) / 1e6
    o["rec_in"] += ev.get("num_input_records", 0)
    o["rec_out"] += ev.get("num_output_records", 0)
    o["reasons"][why] = o["reasons"].get(why, 0) + 1


def on_flush(ev, flushes):
    f = flushes.setdefault(
        ev.get("cf_name", "?"), {"n": 0, "mb": 0.0, "deletes": 0, "entries": 0}
    )
    f["n"] += 1
    f["mb"] += ev.get("total_data_size", 0) / 1e6
    f["deletes"] += ev.get("num_deletes", 0)
    f["entries"] += ev.get("num_entries", 0)


def events(p):
    for line in open(p, errors="replace"):
        if "EVENT_LOG_v1" not in line:
            continue
        try:
            yield json.loads(line.split("EVENT_LOG_v1 ", 1)[1])
        except (ValueError, IndexError):
            continue


def log_compactions(p, t0, t1):
    jobs, out_, flushes = {}, {}, {}
    for ev in events(p):
        t = ev.get("time_micros", 0) / 1e6
        e = ev.get("event")
        if e == "compaction_started":
            jobs[ev["job"]] = (
                ev.get("cf_name"),
                ev.get("input_data_size", 0),
                ev.get("compaction_reason"),
            )
        elif e == "compaction_finished" and t0 <= t <= t1:
            on_compaction_done(ev, jobs, out_)
        elif e == "flush_started" and t0 <= t <= t1:
            on_flush(ev, flushes)
    return out_, flushes


def node_window(srcs, n, t0, t1):
    pn = {}
    for sk, src in srcs.items():
        for name, ser in src[n].items():
            pn[f"{sk}:{name}"] = series_stats(ser, t0, t1)
    return pn


def one_cell(root, lab, arm, rep, m):
    d = root / lab
    s = json.loads((d / "summary.json").read_text())
    tb0 = s["timing"]["t_bench0"]
    wins = {
        "whole": (tb0, tb0 + m["w1"]),
        "post": (tb0 + m["w0"], tb0 + m["w1"]),
        "pre": (tb0, tb0 + m["w0"]),
    }
    srcs = {"s": sampler(d), "x": m1hz(d), "h": host(d)}
    c = {
        "label": lab,
        "arm": arm,
        "rep": rep,
        "tb0": tb0,
        "win": {k: [a - tb0, b - tb0] for k, (a, b) in wins.items()},
    }
    for wn, (t0, t1) in wins.items():
        c[wn] = {n: node_window(srcs, n, t0, t1) for n in NODES}
        c[wn + "_hostmem"] = {
            k: series_stats(v, t0, t1) for k, v in srcs["h"]["host"].items()
        }
    c["sched"] = {n: s["sched_by_node"][n]["threads"] for n in NODES}
    c["proc"] = {n: s["proc_cpu_by_node"][n]["load"] for n in NODES}
    c["logcmp"] = {}
    for n in NODES:
        lp = d / f"rocksdb-LOG-{n}.txt"
        c["logcmp"][n] = (
            {wn: log_compactions(lp, a, b) for wn, (a, b) in wins.items()}
            if lp.exists()
            else None
        )
    return c


def main():
    run, root, out = (pathlib.Path(a) for a in sys.argv[1:4])
    meta = json.loads((run / "analyst" / "r8-analyse.json").read_text())["meta"]
    cells = list(csv.DictReader(open(run / "campaign" / "cells.tsv"), delimiter="\t"))
    res = {}
    for r in cells:
        if re.fullmatch(r"r\d+", r["rep"]):
            res[r["label"]] = one_cell(
                root, r["label"], r["arm"], r["rep"], meta[r["rep"]]
            )
            sys.stderr.write(f"done {r['label']}\n")
    out.write_text(json.dumps(res))


if __name__ == "__main__":
    main()
