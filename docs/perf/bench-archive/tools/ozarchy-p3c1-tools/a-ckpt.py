"""ozarchy-p3c1: checkpoint metrics, trigger attribution and reject comparison per arm (python3 -I)."""

import json
import re
import statistics as st

R = "/home/oz/bench-results-matched/ozarchy-p3c1-300m-"
ARMS = ("cd", "sd", "cm", "sm")
RT = {1: "r1", 2: "r2", 3: "r3", 4: "r4b"}  # round 4 = the rerun (first r4-sm excluded: harness deleted mid-cell)
V = ("val0", "val1", "val2")


def series(tag):
    """per node: list of (ts, {metric: value}) from ckpt-samples.txt (ok scrapes only)."""
    out = {v: [] for v in V}
    cur = None
    for line in open(R + tag + "/ckpt-samples.txt"):
        f = line.split()
        if not f:
            continue
        if f[0] == "T":
            cur = None
            if f[3] == "ok":
                cur = {}
                out[f[2]].append((float(f[1]), cur))
            continue
        if cur is not None and len(f) >= 2:
            try:
                cur[f[0]] = float(f[1])
            except ValueError:
                pass
    return out


def mdump(tag, which, v):
    d = {}
    for line in open(f"{R}{tag}/metrics-{which}-{v}.txt"):
        f = line.split()
        if len(f) >= 2 and not f[0].startswith("#"):
            try:
                d[f[0]] = float(f[1])
            except ValueError:
                pass
    return d


def g(d, *names):
    for n in names:
        if n in d:
            return d[n]
    return 0.0


CK = "torus_exec_checkpoints_total"
SH = "torus_exec_checkpoint_layer_shared_total"
res = {}
for a in ARMS:
    for i in range(1, 5):
        tag = f"{RT[i]}-{a}"
        s = json.load(open(R + tag + "/summary.json"))
        t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
        ser = series(tag)
        cell = {"nodes": {}}
        for v in V:
            b, e = mdump(tag, "before", v), mdump(tag, "after", v)
            n = g(e, CK) - g(b, CK)
            ws = g(e, "torus_exec_checkpoint_wait_seconds_sum") - g(
                b, "torus_exec_checkpoint_wait_seconds_sum"
            )
            lbs = g(e, "torus_exec_checkpoint_layer_bytes_sum") - g(
                b, "torus_exec_checkpoint_layer_bytes_sum"
            )
            lbn = g(e, "torus_exec_checkpoint_layer_bytes_count") - g(
                b, "torus_exec_checkpoint_layer_bytes_count"
            )
            fs = g(e, "torus_flush_worker_checkpoint_seconds_sum") - g(
                b, "torus_flush_worker_checkpoint_seconds_sum"
            )
            fn = g(e, "torus_flush_worker_checkpoint_seconds_count") - g(
                b, "torus_flush_worker_checkpoint_seconds_count"
            )
            # series: bench-window count, per-sample layer bytes per checkpoint (peak), shared max, trigger attribution
            pts = ser[v]
            inwin = [p for p in pts if t0 <= p[0] <= t1]
            nwin = (
                (g(inwin[-1][1], CK) - g(inwin[0][1], CK))
                if len(inwin) >= 2
                else float("nan")
            )
            span = (inwin[-1][0] - inwin[0][0]) if len(inwin) >= 2 else float("nan")
            peak = 0.0
            shared = max([g(p[1], SH) for p in pts] + [g(e, SH)])
            by100 = other = 0
            gaps = []
            lastck_ts = None
            for (ta, da), (tb, db) in zip(pts, pts[1:]):
                dc = g(db, CK) - g(da, CK)
                dn = g(db, "torus_exec_checkpoint_layer_bytes_count") - g(
                    da, "torus_exec_checkpoint_layer_bytes_count"
                )
                if dn > 0:
                    peak = max(
                        peak,
                        (
                            g(db, "torus_exec_checkpoint_layer_bytes_sum")
                            - g(da, "torus_exec_checkpoint_layer_bytes_sum")
                        )
                        / dn,
                    )
                if dc > 0 and "torus_block_height" in da and "torus_block_height" in db:
                    cross = (
                        int(db["torus_block_height"]) // 100
                        - int(da["torus_block_height"]) // 100
                    )
                    k = min(int(dc), max(cross, 0))
                    by100 += k
                    other += int(dc) - k
                if dc > 0 and t0 <= tb <= t1:
                    if lastck_ts is not None:
                        gaps.append(tb - lastck_ts)
                    lastck_ts = tb
            cell["nodes"][v] = dict(
                n=n,
                wait_s=ws,
                wait_per=ws / n if n else None,
                lb_mean=lbs / lbn if lbn else None,
                lb_peak=peak or None,
                flush_s=fs,
                flush_per=fs / fn if fn else None,
                nwin=nwin,
                per_min=60 * nwin / span if span == span and span > 0 else None,
                shared=shared,
                by100=by100,
                other=other,
                gap_max=max(gaps) if gaps else None,
            )
        res[tag] = cell
json.dump(
    res,
    open("/home/oz/bench-results-matched/ozarchy-p3c1-run/analyst/ckpt.json", "w"),
    indent=1,
)


def m(xs):
    xs = [x for x in xs if x is not None and x == x]
    return st.mean(xs) if xs else float("nan")


print(
    "arm  ckpt/node/cell(before->after)  ckpt/min(bench)  wait s/cell  wait ms/ckpt  layer MB mean  layer MB peak  flush s/cell  flush s/ckpt  shared max  trig h%100/other  max gap s"
)
for a in ARMS:
    nd = [res[f"{RT[i]}-{a}"]["nodes"][v] for i in range(1, 5) for v in V]
    print(
        f"{a}  {m([x['n'] for x in nd]):6.1f}  {m([x['per_min'] for x in nd]):6.2f}  {m([x['wait_s'] for x in nd]):6.3f}  "
        f"{1000 * m([x['wait_per'] for x in nd]):7.1f}  {m([x['lb_mean'] for x in nd]) / 1e6:7.1f}  "
        f"{max([x['lb_peak'] or 0 for x in nd]) / 1e6:7.1f}  {m([x['flush_s'] for x in nd]):6.3f}  {m([x['flush_per'] for x in nd]):6.3f}  "
        f"{max(x['shared'] for x in nd):.0f}  {sum(x['by100'] for x in nd)}/{sum(x['other'] for x in nd)}  "
        f"{max([x['gap_max'] or 0 for x in nd]):.1f}"
    )
for tag, c in res.items():
    print(
        tag,
        " ".join(
            f"{v}:n={x['n']:.0f},win={x['nwin']},100/o={x['by100']}/{x['other']},sh={x['shared']:.0f}"
            for v, x in c["nodes"].items()
        ),
    )

# ---- rejects per arm (mean over 4 cells, per node for node counters)
print("\nrejects (mean per cell; node counters mean over the 3 nodes)")
keys = set()
P = {}
for a in ARMS:
    for i in range(1, 5):
        P[f"{RT[i]}-{a}"] = json.load(open(R + f"{RT[i]}-{a}/p3c1-post.json"))
        for v in V:
            keys |= {k for k, x in P[f"{RT[i]}-{a}"]["rejects"][f"node_{v}"].items() if x}
for k in sorted(keys):
    print(
        f"  {k:70s} "
        + "  ".join(
            f"{a} {m([P[f'{RT[i]}-{a}']['rejects'][f'node_{v}'].get(k, 0) for i in range(1, 5) for v in V]):10.1f}"
            for a in ARMS
        )
    )
bk = [
    "submitted",
    "val0_actions_processed",
    "sent_minus_accepted_place",
    "sent_minus_accepted_cancel_all",
    "in_flight_released_refused",
    "in_flight_released_timeout",
    "in_flight_at_end",
    "tail_errors",
    "tail_missed",
    "oracle_rejected",
    "oracle_stale_marks_at_bench_end",
    "dissemination_total_failures",
    "agreement_error_lines",
    "agreement_panic_or_failstop_lines",
]
for k in bk:
    print(
        f"  bench.{k:62s} "
        + "  ".join(
            f"{a} {m([P[f'{RT[i]}-{a}']['rejects']['bench'][k] for i in range(1, 5)]):10.1f}"
            for a in ARMS
        )
    )
print(
    "  bench.nonce_expired_evictions        "
    + "  ".join(
        f"{a} {sum(sum(P[f'{RT[i]}-{a}']['rejects']['bench']['mempool_nonce_expired_evictions_per_node']) for i in range(1, 5))}"
        for a in ARMS
    )
)
print(
    "  bench.log error-like kinds           "
    + "  ".join(
        f"{a} {sum(len(P[f'{RT[i]}-{a}']['rejects']['bench_log_lines'] or {}) for i in range(1, 5))}"
        for a in ARMS
    )
)
# node log WARN/ERROR categories: totals per arm, normalised (sender/height numbers already N)
cats = {}
for a in ARMS:
    for i in range(1, 5):
        for v in V:
            for k, x in (
                P[f"{RT[i]}-{a}"]["rejects"]["node_log_warn_error"][v] or {}
            ).items():
                k2 = re.sub(r"sender=\S+", "sender=X", k)[:110]
                cats.setdefault(k2, {}).setdefault(a, 0)
                cats[k2][a] += x
print("\nnode log WARN/ERROR categories (sum over 4 cells x 3 nodes)")
for k, d in sorted(cats.items(), key=lambda kv: -sum(kv[1].values()))[:25]:
    print("  " + "  ".join(f"{a}={d.get(a, 0):5d}" for a in ARMS) + "  " + k)
