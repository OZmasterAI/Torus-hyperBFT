"""ozarchy-p3s2g analysis: per cell + per arm. Usage: python3 -I a-p3s2g.py [PREFIX] [TAGS...]
Writes cells.json beside this script (when PREFIX is the campaign prefix) and prints tables."""
import gzip, json, math, re, statistics as st, sys, os, datetime as dt
R = "/home/oz/bench-results-matched"
P = sys.argv[1] if len(sys.argv) > 1 else R + "/ozarchy-p3s2g-300m-"
TAGS = sys.argv[2:] or ["r1-cd", "r1-sd", "r2-sd", "r2-cd", "r3-cd", "r3-sd", "r4-sd", "r4-cd"]
V = ("val0", "val1", "val2")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z")
RE_COMMIT = re.compile(r"on_commit_block fired height=(\d+)")
RE_EXEC = re.compile(r"execution pipeline: executing finalized block height=(\d+) .*pipelined=(\w+) .*native_count=(\d+)")
RE_DONE = re.compile(r"execution pipeline: block done height=(\d+)")
MARK = {"accountValue", "freeMargin", "maintenanceMarginUsed", "totalMarginUsed", "withdrawable"}
CASH = {"cashBalance", "nativeBalance", "evmBalance", "permanentStake"}


def ep(s):
    return dt.datetime.fromisoformat(s).replace(tzinfo=dt.timezone.utc).timestamp()


def pct(xs, q):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = (len(xs) - 1) * q
    f = math.floor(k)
    c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)


def blocks(cell, v, t0, t1):
    com, ex, done = {}, {}, {}
    with gzip.open(f"{cell}/{v}.log.gz", "rt", errors="replace") as fh:
        for line in fh:
            if "height" not in line:
                continue
            line = ANSI.sub("", line)
            m = TS.match(line)
            if not m:
                continue
            t = None
            if "on_commit_block fired" in line:
                h = RE_COMMIT.search(line)
                if h:
                    com.setdefault(int(h.group(1)), ep(m.group(1)))
            elif "executing finalized block" in line:
                h = RE_EXEC.search(line)
                if h:
                    ex.setdefault(int(h.group(1)), (ep(m.group(1)), h.group(2) == "true", int(h.group(3))))
            elif "execution pipeline: block done" in line:
                h = RE_DONE.search(line)
                if h:
                    done.setdefault(int(h.group(1)), ep(m.group(1)))
    hs = sorted(h for h, t in com.items() if t0 <= t <= t1)
    ci = [1000 * (com[b] - com[a]) for a, b in zip(hs, hs[1:]) if b == a + 1]
    wall, nat, piped, lag, busy = [], [], 0, [], 0.0
    nex = 0
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
    # exec lag at bench end: last committed height vs last executed height at t1
    lc = max((h for h, t in com.items() if t <= t1), default=0)
    ld = max((h for h, t in done.items() if t <= t1), default=0)
    return dict(commits=len(hs), ci_p50=pct(ci, .5), ci_p95=pct(ci, .95), ci_p99=pct(ci, .99), ci_mean=st.mean(ci) if ci else float("nan"),
                exec_blocks=nex, native_exec_blocks=len(wall), exec_ms_mean=st.mean(wall) if wall else float("nan"),
                exec_ms_p50=pct(wall, .5), exec_ms_p99=pct(wall, .99), busy=busy / (t1 - t0), actions_per_block=st.mean(nat) if nat else float("nan"),
                piped_frac=piped / len(wall) if wall else float("nan"), lag_mean_s=st.mean(lag) if lag else float("nan"),
                lag_max_s=max(lag) if lag else float("nan"), lag_blocks_at_end=lc - ld)


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


def digest(cell, s):
    """per node digest line diffs vs the majority, classified."""
    lines = {v: open(f"{cell}/state-digest-{v}.txt").read().splitlines() for v in V}
    ag = {j["node"]: j for j in map(json.loads, open(f"{cell}/agreement.jsonl"))}
    dg = {v: ag[v]["state_digest"] for v in V}
    out = {"verdict": s["headline"].get("agreement_verdict"), "digest_s": [ag[v]["digest_seconds"] for v in V],
           "digest_h": [ag[v]["digest_height"] for v in V], "diffs": {}}
    vals = list(dg.values())
    maj = max(set(vals), key=vals.count)
    ref = next(v for v in V if dg[v] == maj)
    for v in V:
        if dg[v] == maj and vals.count(maj) > 1:
            continue
        if vals.count(maj) == 1 and v == ref:
            continue
        a, b = lines[ref], lines[v]
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
        out["diffs"][v] = dict(vs=ref, lines=nl, fields=sorted(fields), cls="mark-only" if kinds == {"mark"} else ("HARD:" + ",".join(sorted(kinds - {"mark"})) if kinds - {"mark"} else "none"))
    # oracle price age at each node's digest end: oracle feed paused at run.log 'oracle feed paused' (local time) -> use the
    # oracle-feed-stats.json last accept if present; else the pause time
    return out


def runlog_times(cell):
    """local HH:MM:SS of 'oracle feed paused' and 'state digest:' start, as seconds since midnight"""
    r = {}
    for line in open(f"{cell}/run.log"):
        m = re.match(r"^\[(\d\d):(\d\d):(\d\d)\] (.*)", line)
        if not m:
            continue
        sec = int(m.group(1)) * 3600 + int(m.group(2)) * 60 + int(m.group(3))
        if "oracle feed paused" in m.group(4):
            r["pause"] = sec
        elif m.group(4).startswith("state digest:"):
            r["digest_start"] = sec
    return r


out = {}
for tag in TAGS:
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
    dlt = lambda k: [ma[v].get(k, 0) - mb[v].get(k, 0) for v in V]
    ing = s["ingest"]
    rt = runlog_times(c)
    dgi = digest(c, s)
    out[tag] = dict(
        rc=open(P + tag + ".cell.rc").read().strip(), valid=s["validity"]["verdict"], agree=h.get("agreement_verdict"),
        ms=h["matched_s_avg"], blk=h["native_blk_s"], placed=h["placed_s_avg"], submit=ing.get("bench_submit_rate"),
        chain_ms=h.get("chain_ms"), ci_avg=h.get("commit_interval_ms_avg"), ci_p50=h.get("commit_interval_ms_p50"), ci_p95=h.get("commit_interval_ms_p95"),
        fills_blk=h.get("fills_per_native_block"), eng_1k=h.get("engine_ms_per_1k_fills"), apb=h.get("actions_per_exec_block"),
        peak_q=h.get("peak_exec_queue_depth"), timeouts=h.get("consensus_timeouts"),
        cpu_1m=st.mean(a / f for a, f in cpu), cpu_node=st.mean(a for a, _ in cpu), fills_node=st.mean(f for _, f in cpu),
        cpu_1k_fills_ms=st.mean(1000 * a / (f * 1000) for a, f in cpu),
        rss_peak=st.mean(max(x) for x in rss.values()), rss_mean=st.mean(st.mean(x) for x in rss.values()),
        rss_peak_max=max(max(x) for x in rss.values()),
        blocks=bl, econ=ing.get("econ_mix"), in_flight=ing.get("in_flight"), nonce_evict=ing.get("mempool_nonce_expired_evictions_per_node"),
        val0_processed=ing.get("val0_actions_processed"), submitted=ing.get("bench_submitted_actions"),
        cancel_all_exec=dlt("torus_exec_cancel_all_total"), action_fail=dlt("torus_exec_action_failures_total"),
        action_rej=dlt("torus_exec_action_rejections_total"), rej_open_limit=dlt("torus_orders_rejected_open_limit_total"),
        rej_margin=dlt("torus_orders_rejected_margin_total"), rej_book=dlt("torus_orders_rejected_book_total"),
        rej_cancelled=dlt("torus_orders_rejected_cancelled_total"), rej_other=dlt("torus_orders_rejected_other_total"),
        ckpt={k: dlt(k) for k in ("torus_exec_checkpoints_total", "torus_exec_checkpoint_layer_shared_total", "torus_exec_checkpoint_wait_seconds_sum",
                                  "torus_exec_checkpoint_layer_bytes_sum", "torus_exec_checkpoint_layer_bytes_count", "torus_flush_worker_checkpoint_seconds_sum")},
        digest=dgi, runlog=rt, t0=t0, t1=t1,
    )
here = os.path.dirname(os.path.abspath(__file__))
if P.endswith("ozarchy-p3s2g-300m-"):
    json.dump(out, open(here + "/cells.json", "w"), indent=0)
for t, x in out.items():
    b = x["blocks"]
    print(f"{t} rc={x['rc']} {x['valid']} {x['agree']} ms={x['ms']:.0f} blk={x['blk']:.2f} submit={x['submit']} cpu1M={x['cpu_1m']:.3f} rss pk/mean={x['rss_peak']:.2f}/{x['rss_mean']:.2f} chain={x['chain_ms']} apb={x['apb']} fills/blk={x['fills_blk']} eng1k={x['eng_1k']}")
    for v in V:
        print("   ", v, {k: (round(y, 3) if isinstance(y, float) else y) for k, y in b[v].items()})
    print("   cancel-all sent", (x["econ"] or {}).get("sent_cancel_all"), "exec", x["cancel_all_exec"], "fail", x["action_fail"], "rej", x["action_rej"], "open_limit", x["rej_open_limit"])
    print("   digest", json.dumps(x["digest"]), x["runlog"])
