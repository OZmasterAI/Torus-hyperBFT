"""a-prof-paired.py (ozarchy-p3cloidec, written for section 63): ABBA paired F100 - B100 (r1-r2, r4-r3, r6-r5, r7-r8) of
the W1 profile figures in prof-F/prof-cells.json and prof-B/prof-cells.json (a-prof-arm.py output), val1, ms CPU per 1k
val1 fills: thread-group totals, cloid candidates (exec, pool) and their sum. 95% CI t(3).
Usage: python3 -I a-prof-paired.py [ANALYST_DIR]   (stdout = prof-paired.txt)"""
import json, math, statistics as st, sys
A = (sys.argv[1].rstrip("/") + "/") if len(sys.argv) > 1 else "/home/oz/bench-results-matched/ozarchy-p3cloidec-run/analyst/"
F = json.load(open(A + "prof-F/prof-cells.json"))
B = json.load(open(A + "prof-B/prof-cells.json"))
PAIRS = (("ec-r1-c100", "ec-r2-c100"), ("ec-r4-c100", "ec-r3-c100"), ("ec-r6-c100", "ec-r5-c100"), ("ec-r7-c100", "ec-r8-c100"))
def cand(r, g, c):
    return sum(v for k, v in r["cat"].items() if k.startswith(f"{g}|{c}"))
def sites(r, g):
    return sum(v for k, v in r["cat"].items() if k.startswith(g + "|") and not k.startswith(g + "|none"))
ROWS = [("exec thread total", lambda r: r["grp"].get("exec", 0.0)),
        ("pool (market workers) total", lambda r: r["grp"].get("pool", 0.0)),
        ("other threads total", lambda r: r["grp"].get("other", 0.0)),
        ("whole process", lambda r: sum(r["grp"].values())),
        ("exec all cloid sites", lambda r: sites(r, "exec")),
        ("exec 1 index", lambda r: cand(r, "exec", "1 index")),
        ("exec 2 dup-check", lambda r: cand(r, "exec", "2 dup-check")),
        ("exec 3 cleanup", lambda r: cand(r, "exec", "3 cleanup")),
        ("exec not at a cloid site", lambda r: cand(r, "exec", "none")),
        ("pool all cloid sites", lambda r: sites(r, "pool")),
        ("pool 1 index", lambda r: cand(r, "pool", "1 index")),
        ("pool 2 dup-check", lambda r: cand(r, "pool", "2 dup-check")),
        ("pool 3 cleanup", lambda r: cand(r, "pool", "3 cleanup")),
        ("exec + pool all cloid sites", lambda r: sites(r, "exec") + sites(r, "pool"))]
print("W1 cycles:u val1, ms CPU per 1k val1 fills; ABBA paired F100 - B100 (pairs " + ", ".join(f"{f[3:5]}-{b[3:5]}" for f, b in PAIRS) + "), 95% CI t(3)")
print(f"{'figure':32s} {'B100 mean':>10s} {'F100 mean':>10s} {'delta':>8s} {'95% CI':>20s}  per-pair deltas")
for name, get in ROWS:
    d = [get(F[f]) - get(B[b]) for f, b in PAIRS]
    m, h = st.mean(d), 3.182 * st.stdev(d) / math.sqrt(len(d))
    print(f"{name:32s} {st.mean(get(B[b]) for _, b in PAIRS):10.3f} {st.mean(get(F[f]) for f, _ in PAIRS):10.3f} {m:+8.3f} [{m - h:+8.3f}, {m + h:+8.3f}]  "
          + " ".join(f"{x:+.3f}" for x in d))
