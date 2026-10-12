"""Per arm mean (after - before, summed over 3 nodes) of selected latency histograms: mean ms per observation. python3 -I a-submit.py"""
import re, statistics as st
R = "/home/oz/bench-results-matched/ozarchy-p3alloc-300m-"
M = ["torus_rpc_submit_admit_seconds", "torus_rpc_submit_permit_wait_seconds", "torus_rpc_submit_verify_seconds", "torus_rpc_submit_admit_insert_seconds",
     "torus_rpc_request_duration_seconds", "torus_block_build_seconds", "torus_block_build_select_seconds", "torus_view_propose_build_seconds",
     "torus_view_propose_delay_seconds", "torus_view_proposal_arrival_seconds", "torus_view_propose_finalize_seconds"]
def read(p):
    d = {}
    for l in open(p):
        m = re.match(r"^(torus_[a-z_]+?)_(sum|count)(\{[^}]*\})? ([0-9.eE+-]+)$", l.strip())
        if m and m.group(1) in M:
            k = (m.group(1), m.group(2))
            d[k] = d.get(k, 0.0) + float(m.group(4))
    return d
out = {}
for a in ("gm", "jgm", "g2", "jg2"):
    per = {m: [] for m in M}
    for r in range(1, 5):
        c = f"{R}r{r}-{a}/"
        tot = {}
        for v in ("val0", "val1", "val2"):
            b, e = read(c + f"metrics-before-{v}.txt"), read(c + f"metrics-after-{v}.txt")
            for k in e:
                tot[k] = tot.get(k, 0) + e[k] - b.get(k, 0)
        for m in M:
            n = tot.get((m, "count"), 0)
            per[m].append(1000 * tot.get((m, "sum"), 0) / n if n else float("nan"))
    out[a] = per
print("mean ms per observation (3 nodes, cell delta; mean of 4 cells)")
print(f"{'metric':45s}" + "".join(f"{a:>10s}" for a in out))
for m in M:
    print(f"{m:45s}" + "".join(f"{st.mean(out[a][m]):10.3f}" for a in out))
