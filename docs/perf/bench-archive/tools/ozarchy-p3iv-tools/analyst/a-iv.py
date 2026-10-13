"""ozarchy-p3iv: checkpoint interval 5 s (g5) vs 15 s (g2), extra per-cell rows (python3 -I a-iv.py PREFIX BASE OTHER TAG:ARM ...).
Per cell (mean of 3 nodes unless noted), over the metrics window (metrics-before -> metrics-after):
  ckpts        torus_exec_checkpoints_total delta per node
  ckpt_per_min checkpoints per node per minute of the metrics window (bench t_bench0 -> t_bench1 length)
  layer_mb     torus_exec_checkpoint_layer_bytes sum / count delta (MB per checkpoint layer)
  layer_mb_min layer MB per node per minute (layer bytes sum delta / bench minutes)
  settle_pb    torus_exec_checkpoint_settle_wait_seconds sum / native blocks (ms per native block)
  upkeep_pb    torus_exec_checkpoint_upkeep_seconds sum / native blocks (ms per native block)
  flush_ck_s   torus_flush_worker_checkpoint_seconds sum delta per node (s)
  inline       torus_exec_checkpoint_drop_inline_total delta per node
  wr / wal_wb / rdb_wb  device write_bytes MB/s per validator: process (io-ticks, bench window), and over the
               io-threads window the RocksDB background threads (rocksdb:high/low) vs the rest (= WAL + foreground writes;
               the section 42 a-cells.py split)
Then the paired OTHER - BASE per round rK with a t(n-1) 95% CI (same as a-paired.py)."""

import json
import math
import re
import statistics as st
import sys

V = ("val0", "val1", "val2")
T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571}
MB = 1e6
K = [
    "torus_exec_native_blocks_total",
    "torus_exec_checkpoints_total",
    "torus_exec_checkpoint_drop_inline_total",
    "torus_exec_checkpoint_layer_bytes_sum",
    "torus_exec_checkpoint_layer_bytes_count",
    "torus_exec_checkpoint_settle_wait_seconds_sum",
    "torus_exec_checkpoint_upkeep_seconds_sum",
    "torus_flush_worker_checkpoint_seconds_sum",
]


def met(p):
    d = {}
    for line in open(p):
        m = re.match(r"^([a-z_]+)(\{[^}]*\})? ([0-9.eE+-]+)$", line.strip())
        if m and m.group(1) in K:
            d[m.group(1)] = d.get(m.group(1), 0.0) + float(m.group(3))
    return d


def io_proc(c, t0, t1):
    rows = {}
    for line in open(c + "/io-ticks.txt"):
        p = line.split()
        if len(p) >= 6:
            rows.setdefault(p[1], []).append((float(p[0]), int(p[3]), int(p[5])))
    wb = 0
    for rs in rows.values():
        ins = [r for r in rs if t0 <= r[0] <= t1]
        if len(ins) >= 2:
            wb += ins[-1][2] - ins[0][2]
    return wb


def thr(c, f):
    d = {}
    for line in open(c + "/" + f):
        p = line.split()
        if len(p) >= 8:
            d[(p[0], p[1])] = (p[2], int(p[4]))
    return d


def cell(P, tag):
    c = P + tag
    s = json.load(open(c + "/summary.json"))
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    mins = (t1 - t0) / 60
    rows = []
    for v in V:
        b, a = met(f"{c}/metrics-before-{v}.txt"), met(f"{c}/metrics-after-{v}.txt")
        d = {k: a.get(k, float("nan")) - b.get(k, 0.0) for k in K}
        nb = d["torus_exec_native_blocks_total"]
        rows.append(
            dict(
                ckpts=d["torus_exec_checkpoints_total"],
                ckpt_per_min=d["torus_exec_checkpoints_total"] / mins,
                layer_mb=d["torus_exec_checkpoint_layer_bytes_sum"]
                / d["torus_exec_checkpoint_layer_bytes_count"]
                / MB,
                layer_mb_min=d["torus_exec_checkpoint_layer_bytes_sum"] / MB / mins,
                settle_pb=1000
                * d["torus_exec_checkpoint_settle_wait_seconds_sum"]
                / nb,
                upkeep_pb=1000 * d["torus_exec_checkpoint_upkeep_seconds_sum"] / nb,
                flush_ck_s=d["torus_flush_worker_checkpoint_seconds_sum"],
                inline=d["torus_exec_checkpoint_drop_inline_total"],
            )
        )
    r = {k: st.mean(x[k] for x in rows) for k in rows[0]}
    r["ckpts_nodes"] = [x["ckpts"] for x in rows]
    r["wr"] = io_proc(c, t0, t1) / 3 / (t1 - t0) / MB
    a, b = thr(c, "io-threads-start.txt"), thr(c, "io-threads-end.txt")
    ts0 = float(open(c + "/io-threads-start.txt.ts").read().split()[0])
    ts1 = float(open(c + "/io-threads-end.txt.ts").read().split()[0])
    rdb = sum(
        w - a.get(k, (nm, 0))[1]
        for k, (nm, w) in b.items()
        if nm.startswith("rocksdb:high") or nm.startswith("rocksdb:low")
    )
    tot = io_proc(c, ts0 - 0.5, ts1 + 0.5)
    r["rdb_wb"] = rdb / 3 / (ts1 - ts0) / MB
    r["wal_wb"] = (tot - rdb) / 3 / (ts1 - ts0) / MB
    return r


def main():
    P, base, other = sys.argv[1:4]
    C = {}
    for ta in sys.argv[4:]:
        tag, arm = ta.split(":")
        C[tag] = cell(P, tag)
        x = C[tag]
        print(
            f"{tag} {arm} ckpts/node={x['ckpts_nodes']} per_min={x['ckpt_per_min']:.2f} layer_mb={x['layer_mb']:.1f} "
            f"layer_mb/min={x['layer_mb_min']:.0f} settle ms/blk={x['settle_pb']:.2f} upkeep ms/blk={x['upkeep_pb']:.2f} "
            f"flush_ckpt_s={x['flush_ck_s']:.2f} inline={x['inline']:.0f} wr={x['wr']:.1f} wal_wb={x['wal_wb']:.1f} rdb_wb={x['rdb_wb']:.1f} MB/s"
        )
    rounds = sorted({t.split("-")[0] for t in C if t.startswith("r")})
    pairs = [
        (r, C[f"{r}-{base}"], C[f"{r}-{other}"])
        for r in rounds
        if f"{r}-{base}" in C and f"{r}-{other}" in C
    ]
    print(
        f"\npaired rounds: {[p[0] for p in pairs]}  ({other} - {base}); CI = mean +- t(n-1) * sd / sqrt(n)"
    )
    print(
        f"{'metric':12s} {base + ' mean':>12s} {other + ' mean':>12s} {'delta':>10s} {'95% CI':>22s} {'%':>8s} {'% CI':>20s}  per-round deltas"
    )
    for k in [k for k in pairs[0][1] if k != "ckpts_nodes"]:
        a = [p[1][k] for p in pairs]
        b = [p[2][k] for p in pairs]
        if any(v != v for v in a + b):
            continue
        d = [y - x for x, y in zip(a, b)]
        n = len(d)
        md = st.mean(d)
        h = T975[n - 1] * st.stdev(d) / math.sqrt(n) if n > 1 else float("nan")
        ma = st.mean(a)
        pc = (lambda v: 100 * v / ma) if ma else (lambda v: float("nan"))
        print(
            f"{k:12s} {ma:12.3f} {st.mean(b):12.3f} {md:+10.3f} [{md - h:+9.3f}, {md + h:+9.3f}] {pc(md):+7.2f}% "
            f"[{pc(md - h):+7.2f}, {pc(md + h):+7.2f}]  {' '.join(f'{v:+.3f}' for v in d)}"
        )


main()
