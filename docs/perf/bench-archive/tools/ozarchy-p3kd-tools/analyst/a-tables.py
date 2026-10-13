"""ozarchy-p3kd summary tables (python3 -I a-tables.py): restart.txt (Part D, from kills.json; section 46 = ozarchy-p3r1 kills.json KILL rows)
and soak.txt (Part B, from series.json + cells.json; p3gate soak cells from ozarchy-p3gate-run/analyst/cells-soak.json, cross-session)."""
import json, statistics as st, sys
R = "/home/oz/bench-results-matched/"
# archive copy: optional OUTDIR (reads series.json / cells.json there, writes restart.txt / soak.txt there) and KILLS.json
# (archive smoke check); with no arguments the analyst's paths, unchanged
O = sys.argv[1].rstrip("/") + "/" if len(sys.argv) > 1 else R + "ozarchy-p3kd-run/analyst/"
K = json.load(open(sys.argv[2] if len(sys.argv) > 2 else R + "ozarchy-p3kd-run/kills.json"))
K46 = [r for r in json.load(open(R + "ozarchy-p3r1-run/kills.json")) if r["sig"] == "KILL"]
L = ["Part D: SIGKILL of val1 under load, kill -> val1 first commit = down_s + restart->val1 'on_committed_block: sending' (gate <= 60 s)",
     "chain = kill -> val0's first commit after the restart (crash-freeze.py); A = durable applied marker, C = committed at restart, gap = replayed blocks",
     "cell        k  pre_h  q | A     C     gap first contig | dbopen replay(s) blk/s | kill->v1commit kill->chain rejoin | flags"]
for r in K:
    v1 = r["down_s"] + r["up_commit_s"]
    r["kv1"] = v1
    L.append("%-11s k%d %5d %3d | %5d %5d %3d %5s %s | %5.2f %6.2f %5.2f | %6.2f %6.2f %6.2f | %s" % (r["cell"], r["kill"], r["pre_h"], r["pre_q"], r["A"], r["C"], r["gap"], r["first_replayed"],
             r["replay_contiguous"], r["db_open_s"], r["replay_s"], r["blk_s"] or 0, v1, r["kill_commit_s"], r["rejoin_s"] or -1, r["flags"] or "-"))
def agg(rs, lab):
    v = [r["down_s"] + r["up_commit_s"] for r in rs]; g = [r["gap"] for r in rs]; b = [r["blk_s"] for r in rs if r["blk_s"]]
    return "%-38s n=%d kill->v1commit mean %.1f max %.1f | gap mean %.0f max %d | replay blk/s mean %.1f | %s" % (lab, len(rs), st.mean(v), max(v), st.mean(g), max(g), st.mean(b), "PASS" if max(v) <= 60 else "FAIL")
L.append("")
for a in ("s2", "m"):
    L.append(agg([r for r in K if r["arm"] == a], "p3kd " + a))
for a, lab in (("s", "section 46 s (a44bdde6, 15 s) all 8"), ("o", "section 46 o (a44bdde6, unset) all 8")):
    L.append(agg([r for r in K46 if r["arm"] == a], lab))
    L.append(agg([r for r in K46 if r["arm"] == a and r["kill"] <= 2], lab.replace("all 8", "k1+k2 only")))
open(O + "restart.txt", "w").write("\n".join(L) + "\n")
S = json.load(open(O + "series.json")); C = json.load(open(O + "cells.json"))
G = json.load(open(R + "ozarchy-p3gate-run/analyst/cells-soak.json"))
V = ("val0", "val1", "val2")
L = ["Part B: 600 s soak, s2 c88d3073; def = jemalloc defaults, dec = MALLOC_CONF=background_thread:true,dirty_decay_ms:1000; gs2/gm = p3gate k1-s2/k1-m (cross-session, same binaries)",
     "per node GB: RSS mean min 5-10 | RSS peak | je allocated / active / resident (mean min 5-10) | resident-allocated | RSS last-5-min slope MB/min"]
cpu = {"def": C["k1-s2def"], "dec": C["k1-s2dec"], "gs2": G["k1-s2"], "gm": G["k1-m"]}
for k in ("def", "dec", "gs2", "gm"):
    A = S["cells"][k]["nodes"]; J = S["je_nodes"][k]
    for v in V:
        n = A[v]; j = J[v]
        L.append("%-3s %s RSS %.2f peak %.2f | je %.2f / %.2f / %.2f | res-alloc %.2f | slope %+.1f" % (k, v, n["rss_mean_5_10"] / 1024, n["rss_peak"] / 1024,
                 j["allocated"][0] / 1024, j["active"][0] / 1024, j["resident"][0] / 1024, (j["resident"][0] - j["allocated"][0]) / 1024, n["slope_late"][0]))
    m = lambda f: st.mean(f(v) for v in V)
    L.append("%-3s MEAN RSS %.2f peak %.2f | je alloc %.2f resident %.2f res-alloc %.2f | matched/s %.0f | node CPU-s/1M fills %.2f | blk/s %.2f" % (k,
             m(lambda v: A[v]["rss_mean_5_10"]) / 1024, m(lambda v: A[v]["rss_peak"]) / 1024, m(lambda v: J[v]["allocated"][0]) / 1024, m(lambda v: J[v]["resident"][0]) / 1024,
             m(lambda v: J[v]["resident"][0] - J[v]["allocated"][0]) / 1024, cpu[k]["ms"], cpu[k]["cpu_1m"], cpu[k]["blk"]))
d, e = cpu["dec"], cpu["def"]
L.append("dec vs def: CPU-s/1M %+.1f%%, matched/s %+.1f%%, blk/s %+.1f%% (n=1 per arm)" % (100 * (d["cpu_1m"] / e["cpu_1m"] - 1), 100 * (d["ms"] / e["ms"] - 1), 100 * (d["blk"] / e["blk"] - 1)))
L.append("dec vs gm (p3gate main, cross-session): CPU-s/1M %+.1f%%, matched/s %+.1f%%" % (100 * (d["cpu_1m"] / cpu["gm"]["cpu_1m"] - 1), 100 * (d["ms"] / cpu["gm"]["ms"] - 1)))
L.append("def vs gs2 (same arm, p3gate, reproducibility): CPU-s/1M %+.1f%%, matched/s %+.1f%%" % (100 * (e["cpu_1m"] / cpu["gs2"]["cpu_1m"] - 1), 100 * (e["ms"] / cpu["gs2"]["ms"] - 1)))
open(O + "soak.txt", "w").write("\n".join(L) + "\n")
