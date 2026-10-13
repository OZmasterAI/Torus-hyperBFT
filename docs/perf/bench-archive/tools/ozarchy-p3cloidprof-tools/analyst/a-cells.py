"""ozarchy-p3cloid (copy of p3mw a-cells.py; + cloids, dup_cloid) / p3rss / p3g2 / p23b per-cell extraction (python3 -I). Usage: a-cells.py OUT.json PREFIX TAG:ARM ...
Per cell: throughput, CPU-s/1M, exec / commit timing from the node logs (a-p3s2g.py method), phases, RSS from the harness
cpu.csv (section 47 definition: per node peak / mean over the whole cpu.csv, mean over nodes) AND from mem-samples.txt
(5 s smaps_rollup + /proc status: bench-window peak / mean / growth, anon vs file, swap) with the memory / queue gauges."""
import datetime as dt
import gzip
import json
import math
import re
import statistics as st
import sys

V = ("val0", "val1", "val2")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z")
RE_COMMIT = re.compile(r"on_commit_block fired height=(\d+)")
RE_EXEC = re.compile(r"execution pipeline: executing finalized block height=(\d+) .*pipelined=(\w+) .*native_count=(\d+)")
RE_DONE = re.compile(r"execution pipeline: block done height=(\d+)")
MARK = {"accountValue", "freeMargin", "maintenanceMarginUsed", "totalMarginUsed", "withdrawable"}
CASH = {"cashBalance", "nativeBalance", "evmBalance", "permanentStake"}
NAN = float("nan")


def ep(s):
    return dt.datetime.fromisoformat(s).replace(tzinfo=dt.timezone.utc).timestamp()


def pct(xs, q):
    xs = sorted(xs)
    if not xs:
        return NAN
    k = (len(xs) - 1) * q
    f = math.floor(k)
    c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)


def mean(xs):
    xs = list(xs)
    return st.mean(xs) if xs else NAN


def read_log(cell, v):
    com, ex, done = {}, {}, {}
    with gzip.open(f"{cell}/{v}.log.gz", "rt", errors="replace") as fh:
        for line in fh:
            if "height" not in line:
                continue
            line = ANSI.sub("", line)
            m = TS.match(line)
            if not m:
                continue
            if "on_commit_block fired" in line and (h := RE_COMMIT.search(line)):
                com.setdefault(int(h.group(1)), ep(m.group(1)))
            elif "executing finalized block" in line and (h := RE_EXEC.search(line)):
                ex.setdefault(int(h.group(1)), (ep(m.group(1)), h.group(2) == "true", int(h.group(3))))
            elif "execution pipeline: block done" in line and (h := RE_DONE.search(line)):
                done.setdefault(int(h.group(1)), ep(m.group(1)))
    return com, ex, done


def blocks(cell, v, t0, t1):
    com, ex, done = read_log(cell, v)
    hs = sorted(h for h, t in com.items() if t0 <= t <= t1)
    ci = [1000 * (com[b] - com[a]) for a, b in zip(hs, hs[1:]) if b == a + 1]
    wall, nat, piped, lag, busy, nex = [], [], 0, [], 0.0, 0
    for h, (ts, pp, n) in ex.items():
        if not (t0 <= ts <= t1) or h not in done:
            continue
        w = done[h] - ts
        busy += w
        nex += 1
        if n > 0:
            wall.append(1000 * w)
            nat.append(n)
            piped += pp
        if h in com:
            lag.append(done[h] - com[h])
    lc = max((h for h, t in com.items() if t <= t1), default=0)
    ld = max((h for h, t in done.items() if t <= t1), default=0)
    bl, t = [], t0
    while t <= t1:  # exec backlog (committed - executed) every 5 s in the bench window
        bl.append(max((h for h, tt in com.items() if tt <= t), default=0) - max((h for h, tt in done.items() if tt <= t), default=0))
        t += 5
    return dict(commits=len(hs), ci_p50=pct(ci, .5), ci_p95=pct(ci, .95), ci_p99=pct(ci, .99), ci_mean=mean(ci),
                exec_blocks=nex, native_exec_blocks=len(wall), exec_ms_mean=mean(wall), exec_ms_p50=pct(wall, .5),
                exec_ms_p99=pct(wall, .99), busy=busy / (t1 - t0), actions_per_block=mean(nat),
                piped_frac=piped / len(wall) if wall else NAN, lag_mean_s=mean(lag), lag_max_s=max(lag) if lag else NAN,
                lag_blocks_at_end=lc - ld, backlog_blocks_mean=mean(bl), backlog_blocks_max=max(bl) if bl else NAN)


def metrics(path):
    d = {}
    try:
        for line in open(path):
            f = line.split()
            if len(f) >= 2 and not f[0].startswith("#"):
                try:
                    d[f[0]] = float(f[1])
                except ValueError:
                    pass
    except OSError:
        pass
    return d


def classify(a, b):
    kinds, fields, nl = set(), set(), 0
    if len(a) != len(b):
        kinds.add("line-count")
    for x, y in zip(a, b):
        if x == y:
            continue
        nl += 1
        try:
            ox, oy = json.loads(x), json.loads(y)
        except ValueError:
            kinds.add("nonjson")
            continue
        if "asks" in ox or "bids" in ox:
            kinds.add("book")
            fields.add("book")
        elif "longOi" in ox:
            kinds.add("oi")
            fields.add("oi")
        else:
            for k in set(ox) | set(oy):
                if ox.get(k) != oy.get(k):
                    fields.add(k)
                    kinds.add("mark" if k in MARK else ("cash" if k in CASH else "other"))
    cls = "mark-only" if kinds == {"mark"} else ("HARD:" + ",".join(sorted(kinds - {"mark"})) if kinds - {"mark"} else "none")
    return nl, sorted(fields), cls


def digest(cell, s):
    lines = {v: open(f"{cell}/state-digest-{v}.txt").read().splitlines() for v in V}
    ag = {j["node"]: j for j in map(json.loads, open(f"{cell}/agreement.jsonl"))}
    dg = {v: ag[v]["state_digest"] for v in V}
    out = {"verdict": s["headline"].get("agreement_verdict"), "digest_s": [ag[v].get("digest_seconds") for v in V], "diffs": {}}
    vals = list(dg.values())
    maj = max(set(vals), key=vals.count)
    ref = next(v for v in V if dg[v] == maj)
    for v in V:
        if dg[v] == maj:
            continue
        nl, fields, cls = classify(lines[ref], lines[v])
        eq = 0
        for line in lines[v]:
            if '"accountValue"' in line:
                o = json.loads(line)
                eq += o.get("accountValue") == o.get("cashBalance")
        out["diffs"][v] = dict(vs=ref, lines=nl, fields=fields, cls=cls, accountValue_eq_cash=eq)
    return out


def parse_mem(cell):
    M, G = {v: [] for v in V}, {v: [] for v in V}
    for line in open(f"{cell}/mem-samples.txt"):
        f = line.split()
        if len(f) < 4:
            continue
        if f[0] == "M":
            M[f[2]].append((float(f[1]), {k.rstrip(":"): float(x) for k, x in (kv.split("=") for kv in f[4:] if "=" in kv)}))
        elif f[0] == "G":
            d = {}
            for kv in f[3:]:
                k, _, x = kv.rpartition("=")
                try:
                    d[k] = float(x)
                except ValueError:
                    pass
            G[f[2]].append((float(f[1]), d))
    return M, G


def slope(win):
    if len(win) < 3:
        return NAN
    xs = [t for t, _ in win]
    ys = [d["VmRSS"] / 1e6 for _, d in win]
    mx, my = st.mean(xs), st.mean(ys)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs) * 60


def node_mem(rs, gs, t0, t1):
    win = [(t, d) for t, d in rs if t0 <= t <= t1]
    gw = [(t, d) for t, d in gs if t0 <= t <= t1]

    def kb(k, xs):
        return [d.get(k, 0) / 1e6 for _, d in xs]

    def gm(k):
        return mean(d.get(k, 0) for _, d in gw)

    def gx(k):
        return max((d.get(k, 0) for _, d in gw), default=NAN)

    lb = NAN
    if len(gw) >= 2:
        a, b = gw[0][1], gw[-1][1]
        n = b.get("torus_exec_checkpoint_layer_bytes_count", 0) - a.get("torus_exec_checkpoint_layer_bytes_count", 0)
        if n > 0:
            lb = (b["torus_exec_checkpoint_layer_bytes_sum"] - a["torus_exec_checkpoint_layer_bytes_sum"]) / n / 1e6
    return dict(
        rss_peak_cell=max(kb("VmRSS", rs)), rss_peak_win=max(kb("VmRSS", win), default=NAN), rss_mean_win=mean(kb("VmRSS", win)),
        rss_first15=mean(d["VmRSS"] / 1e6 for t, d in win if t <= t0 + 15), rss_last15=mean(d["VmRSS"] / 1e6 for t, d in win if t >= t1 - 15),
        rss_slope_gb_min=slope(win), hwm=max(kb("VmHWM", rs)), anon_mean_win=mean(kb("RssAnon", win)), file_mean_win=mean(kb("RssFile", win)),
        anon_peak=max(kb("RssAnon", rs)), swap_max=max(kb("VmSwap", rs)), thp_mean_win=mean(kb("AnonHugePages", win)),
        threads_max=max(d.get("Threads", 0) for _, d in rs),
        block_cache_mb=gm("torus_rocksdb_block_cache_bytes") / 1e6, memtable_mb=gm("torus_rocksdb_memtable_bytes_all") / 1e6,
        memtable_mb_max=gx("torus_rocksdb_memtable_bytes_all") / 1e6, mempool_mean=gm("torus_mempool_native_size"),
        mempool_max=gx("torus_mempool_native_size"), execq_mean=gm("torus_exec_queue_depth"), execq_max=gx("torus_exec_queue_depth"),
        flushq_mean=gm("torus_flush_worker_depth"), resident_rows_mb=gm("torus_exec_resident_rows_bytes") / 1e6,
        resting_mean=gm("torus_exec_resting_orders"), cai_entries=gm("torus_exec_cancel_all_index_entries"), layer_mb_per_ckpt=lb)


def memsamples(cell, t0, t1):
    try:
        M, G = parse_mem(cell)
    except OSError:
        return None
    return {v: node_mem(M[v], G[v], t0, t1) for v in V if M[v]}


def cell(P, tag, arm):
    c = P + tag
    s = json.load(open(c + "/summary.json"))
    h = s["headline"]
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    cpu = []
    for v in V:
        w = s["proc_cpu_by_node"][v]["whole_run"]
        cpu.append(((w["user_ms"] + w["sys_ms"]) / 1000, w["fills"] / 1e6))
    rss = {}
    for line in open(c + "/cpu.csv"):
        p = line.strip().split(",")
        if len(p) >= 8 and p[5] == "torus-node":
            rss.setdefault(p[4], []).append(int(p[7]) / 1e6)
    bl = {v: blocks(c, v, t0, t1) for v in V}
    mb = {v: metrics(f"{c}/metrics-before-{v}.txt") for v in V}
    ma = {v: metrics(f"{c}/metrics-after-{v}.txt") for v in V}

    def dlt(k):
        return [ma[v].get(k, 0) - mb[v].get(k, 0) for v in V]

    ing = s["ingest"]
    ph = {v: s["phase_by_node"][v] for v in V}
    phases = {k: mean(ph[v]["phases"][k]["ms"] for v in V) for k in ph["val0"]["phases"]}
    sub = {}
    if "checkpoint_upkeep" in ph["val0"]["phases"]:
        sub["checkpoint_wait"] = mean(ph[v]["phases"]["checkpoint_upkeep"].get("checkpoint_wait_ms", 0) for v in V)
    q = {k: [ph[v].get(k) for v in V] for k in ("checkpoint_settle_wait_ms_p50", "checkpoint_settle_wait_ms_p99", "checkpoint_upkeep_ms_p50",
                                                 "checkpoint_upkeep_ms_p99", "chain_ms_p50", "chain_ms_p95")}
    try:
        dgi = digest(c, s)
    except Exception as e:  # noqa: BLE001
        dgi = {"error": repr(e)}
    ck = ("torus_exec_checkpoints_total", "torus_exec_checkpoint_layer_shared_total", "torus_exec_checkpoint_wait_seconds_sum",
          "torus_exec_checkpoint_layer_bytes_sum", "torus_exec_checkpoint_layer_bytes_count", "torus_flush_worker_checkpoint_seconds_sum",
          "torus_exec_checkpoint_settle_wait_seconds_sum", "torus_exec_checkpoint_settle_wait_seconds_count",
          "torus_exec_checkpoint_upkeep_seconds_sum", "torus_exec_checkpoint_upkeep_seconds_count")
    return dict(
        arm=arm, rc=open(c + ".cell.rc").read().strip(), valid=s["validity"]["verdict"], agree=h.get("agreement_verdict"),
        ms=h["matched_s_avg"], blk=h["native_blk_s"], placed=h["placed_s_avg"], submit=ing.get("bench_submit_rate"),
        chain_ms=h.get("chain_ms"), fills_blk=h.get("fills_per_native_block"), eng_1k=h.get("engine_ms_per_1k_fills"), apb=h.get("actions_per_exec_block"),
        peak_q=h.get("peak_exec_queue_depth"), busy_summary=mean(ph[v]["exec_thread_busy_fraction"] or 0 for v in V),
        block_ms=mean(ph[v]["block_ms"] for v in V), phases=phases, phase_sub=sub, quant=q,
        cpu_1m=mean(a / f for a, f in cpu), cpu_node=mean(a for a, _ in cpu), fills_node=mean(f for _, f in cpu),
        rss_peak=mean(max(x) for x in rss.values()), rss_mean=mean(st.mean(x) for x in rss.values()),
        mem=memsamples(c, t0, t1), blocks=bl, econ=ing.get("econ_mix"), in_flight=ing.get("in_flight"),
        nonce_evict=ing.get("mempool_nonce_expired_evictions_per_node"),
        cancel_all_exec=dlt("torus_exec_cancel_all_total"), action_fail=dlt("torus_exec_action_failures_total"),
        action_rej=dlt("torus_exec_action_rejections_total"),
        spawns={k: dlt("torus_exec_thread_spawns_" + k) for k in ("match", "settle", "end_resident", "save_books", "flush_digest")},
        native_blocks=dlt("torus_exec_native_blocks_total"), cloids=ing.get("cloids"), dup_cloid=dlt('torus_orders_rejected_reason_total{reason="duplicate_cloid"}'),
        dup_cloid_after=[ma[v].get('torus_orders_rejected_reason_total{reason="duplicate_cloid"}') for v in V], ckpt={k: dlt(k) for k in ck}, digest=dgi, t0=t0, t1=t1)


def main():
    outp, P = sys.argv[1], sys.argv[2]
    out = {}
    for ta in sys.argv[3:]:
        tag, arm = ta.split(":")
        x = out[tag] = cell(P, tag, arm)
        b = x["blocks"]
        print(f"{tag} {arm} rc={x['rc']} {x['valid']} {x['agree']} ms={x['ms']:.0f} blk={x['blk']:.2f} cpu1M={x['cpu_1m']:.2f} "
              f"rss pk/mean={x['rss_peak']:.2f}/{x['rss_mean']:.2f} exec p50/p99={mean(b[v]['exec_ms_p50'] for v in V):.0f}/"
              f"{mean(b[v]['exec_ms_p99'] for v in V):.0f} backlog={mean(b[v]['backlog_blocks_mean'] for v in V):.0f} "
              f"resid={x['phases'].get('residual_untimed', NAN):.1f} evict={x['nonce_evict']}", flush=True)
    json.dump(out, open(outp, "w"), indent=0)


main()
