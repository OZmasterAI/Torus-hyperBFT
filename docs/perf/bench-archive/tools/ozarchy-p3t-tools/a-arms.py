import json
import math
import statistics as st

R = "/home/oz/bench-results-matched/ozarchy-p3t-300m-"
C = json.load(open("/tmp/p3t-analyst/cells.json"))
ARMS = ("cd", "ad", "cm", "am")
V = ("val0", "val1", "val2")
T975 = {
    3: 3.182,
    4: 2.776,
    5: 2.571,
    6: 2.447,
    7: 2.365,
    8: 2.306,
    9: 2.262,
    10: 2.228,
    11: 2.201,
    12: 2.179,
}


def tq(df):
    return T975.get(int(round(df)), 2.179 if df > 12 else 3.182)


def rssmax(tag):
    m = {}
    for line in open(R + tag + "/cpu.csv"):
        p = line.strip().split(",")
        if len(p) >= 8 and p[5] == "torus-node":
            m[p[4]] = max(m.get(p[4], 0), int(p[7]))
    return st.mean(m.values()) / 1e6


def dupeak(tag):
    pk = 0
    try:
        for line in open(R + tag + "/data-du-samples.txt"):
            p = line.split()
            vals = [int(x) for x in p[1:4] if x.isdigit()]
            if len(vals) == 3:
                pk = max(pk, max(vals))
    except FileNotFoundError:
        return float("nan")
    return pk / 1e9


def memmin(tag):
    m = 10**12
    for line in open(R + tag + "/data-du-samples.txt"):
        for tok in line.split():
            if tok.startswith("mem_avail_kb="):
                m = min(m, int(tok.split("=")[1]))
    return m / 1e6


def fills_cpu(tag):
    s = json.load(open(R + tag + "/summary.json"))
    cpu = (
        sum(
            (
                s["proc_cpu_by_node"][v]["whole_run"]["user_ms"]
                + s["proc_cpu_by_node"][v]["whole_run"]["sys_ms"]
            )
            / 1000
            for v in V
        )
        / 3
    )
    fills = st.mean(s["proc_cpu_by_node"][v]["whole_run"]["fills"] for v in V) / 1e6
    return cpu, fills


rows = {}
for a in ARMS:
    tags = [f"r{i}-{a}" for i in range(1, 5)]
    rows[a] = dict(
        ms=[C[t]["ms"] for t in tags],
        blk=[C[t]["blk"] for t in tags],
        cpu=[C[t]["cpu"] for t in tags],
        wr=[C[t]["wr_MBps"] for t in tags],
        wchar=[C[t]["wchar_MBps"] for t in tags],
        walwc=[C[t]["wal_wchar_MBps"] for t in tags],
        rss=[rssmax(t) for t in tags],
        du=[dupeak(t) for t in tags],
        mem=[memmin(t) for t in tags],
        cf=[fills_cpu(t) for t in tags],
    )
print(
    "arm  matched/s (sd)      blk/s (sd)     CPU-s/1M (sd)   dev-wr MB/s/val  wchar MB/s/val  RSS GB  data peak GB/val  MemAvail min GB"
)
for a in ARMS:
    r = rows[a]
    print(
        f"{a}  {st.mean(r['ms']):9.0f} ({st.stdev(r['ms']):5.0f})  {st.mean(r['blk']):5.2f} ({st.stdev(r['blk']):.2f})  "
        f"{st.mean(r['cpu']):6.3f} ({st.stdev(r['cpu']):.3f})  {st.mean(r['wr']):6.1f}  {st.mean(r['wchar']):6.1f}  "
        f"{st.mean(r['rss']):.2f}  {max(r['du']):.2f}  {min(r['mem']):.1f}"
    )


def welch(
    x, y
):  # % change of mean(y) vs mean(x), 95% CI (delta method on the difference, Welch df)
    mx, my = st.mean(x), st.mean(y)
    vx, vy = st.variance(x) / len(x), st.variance(y) / len(y)
    se = math.sqrt(vx + vy)
    df = (
        (vx + vy) ** 2 / (vx**2 / (len(x) - 1) + vy**2 / (len(y) - 1))
        if vx + vy > 0
        else 6
    )
    d = my - mx
    t = tq(df)
    return 100 * d / mx, 100 * (d - t * se) / mx, 100 * (d + t * se) / mx


print()
for base, arm, what in (
    ("cd", "ad", "a vs c, shared NVMe"),
    ("cm", "am", "a vs c, tmpfs UPPER BOUND"),
    ("cd", "cm", "control: tmpfs vs disk"),
    ("ad", "am", "proto: tmpfs vs disk"),
):
    out = []
    for k in ("ms", "blk", "cpu", "wchar", "rss"):
        p, lo, hi = welch(rows[base][k], rows[arm][k])
        out.append(f"{k} {p:+.2f}% [{lo:+.2f},{hi:+.2f}]")
    print(f"{arm} vs {base} ({what}): " + "; ".join(out))
# interaction: (am-cm) - (ad-cd) gain in % points, per-round paired
for k in ("ms", "blk", "cpu"):
    gd = [100 * (rows["ad"][k][i] / rows["cd"][k][i] - 1) for i in range(4)]
    gm = [100 * (rows["am"][k][i] / rows["cm"][k][i] - 1) for i in range(4)]
    dd = [gm[i] - gd[i] for i in range(4)]
    se = st.stdev(dd) / 2
    print(
        f"{k}: per-round a/c gain disk {[round(x, 1) for x in gd]} mean {st.mean(gd):+.2f}; tmpfs {[round(x, 1) for x in gm]} mean {st.mean(gm):+.2f}; "
        f"tmpfs-disk {st.mean(dd):+.2f} pp [{st.mean(dd) - 3.182 * se:+.2f},{st.mean(dd) + 3.182 * se:+.2f}] (paired by round, df 3)"
    )
# equal-fills ANCOVA, common within-arm slope over the 4 arms (p3p a-ancova method)
sxx = sxy = 0
for a in ARMS:
    xs = rows[a]["cf"]
    mx = st.mean(f for _, f in xs)
    my = st.mean(c for c, _ in xs)
    for c, f in xs:
        sxx += (f - mx) ** 2
        sxy += (f - mx) * (c - my)
beta = sxy / sxx
res = []
for a in ARMS:
    xs = rows[a]["cf"]
    mx = st.mean(f for _, f in xs)
    my = st.mean(c for c, _ in xs)
    res += [c - my - beta * (f - mx) for c, f in xs]
dfr = len(res) - len(ARMS) - 1
s2 = sum(e * e for e in res) / dfr
print(
    f"\nANCOVA common slope {beta:.2f} +- {math.sqrt(s2 / sxx):.2f} CPU-s per 1M fills; residual sd {math.sqrt(s2):.2f} CPU-s/node (df {dfr})"
)
for a in ARMS:
    print(
        a,
        "CPU-s/node mean %.1f, fills/node M %.3f"
        % (st.mean(c for c, _ in rows[a]["cf"]), st.mean(f for _, f in rows[a]["cf"])),
    )
for base, arm in (("cd", "ad"), ("cm", "am"), ("cd", "cm"), ("ad", "am")):
    b = rows[base]["cf"]
    x = rows[arm]["cf"]
    mb = (st.mean(c for c, _ in b), st.mean(f for _, f in b))
    mxx = (st.mean(c for c, _ in x), st.mean(f for _, f in x))
    dx = mxx[1] - mb[1]
    adj = mxx[0] - (mb[0] + beta * dx)
    ref = mb[0] + beta * dx
    se = math.sqrt(s2 * (0.5) + dx**2 * s2 / sxx)
    t = tq(dfr)
    print(
        f"{arm} vs {base} equal fills: {adj:+.1f} CPU-s/node = {100 * adj / ref:+.2f}% [{100 * (adj - t * se) / ref:+.2f},{100 * (adj + t * se) / ref:+.2f}] ; raw CPU-s/node diff {mxx[0] - mb[0]:+.1f}"
    )
print(
    "\np3p reference (n=4, shared NVMe): c 24.996 a 23.770 CPU-s/1M (-4.90%), matched/s +4.7%, blocks/s +6.1%, equal fills -2.1%"
)
