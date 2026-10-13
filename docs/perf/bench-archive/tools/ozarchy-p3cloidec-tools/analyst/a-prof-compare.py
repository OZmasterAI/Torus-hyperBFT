"""a-prof-compare.py (ozarchy-p3cloidec analyst): F c100 vs B c100 (and B c100 vs B c0) from the per-arm a-prof outputs
(prof-F/prof-cells.json, prof-B/prof-cells.json). ms CPU per 1k fills (W1 cycles:u, val1) by thread group and cloid site,
hash leaf frames; W2 exec-thread events per 1k fills. Mean (sd) per set, delta, pooled sd, step/sd."""
import json, math, statistics as st, collections
A = "/home/oz/bench-results-matched/ozarchy-p3cloidec-run/analyst/"
F = json.load(open(A + "prof-F/prof-cells.json"))
B = json.load(open(A + "prof-B/prof-cells.json"))
sets = {"F100": [r for r in F.values() if r["arm"] == "c100"], "B100": [r for r in B.values() if r["arm"] == "c100"],
        "B0": [r for r in B.values() if r["arm"] == "c0"]}
def sd(x): return st.stdev(x) if len(x) > 1 else 0.0
def pooled(a, b):
    df = len(a) + len(b) - 2
    return math.sqrt(((len(a) - 1) * sd(a) ** 2 + (len(b) - 1) * sd(b) ** 2) / df) if df > 0 else float("nan")
def line(name, get):
    v = {k: [get(r) for r in rs] for k, rs in sets.items()}
    out = f"{name[:62]:62}" + "".join(f"{st.mean(v[k]):9.3f}({sd(v[k]):5.3f})" for k in ("F100", "B100", "B0"))
    for x, y in (("B100", "F100"), ("B0", "B100")):
        d = st.mean(v[y]) - st.mean(v[x]); p = pooled(v[x], v[y])
        out += f" | {y}-{x} {d:+7.3f} s/sd {d / p if p else float('nan'):+6.1f}"
    print(out)
print("W1 cycles:u val1, ms CPU/1k fills: mean(sd) F100 B100 B0 | F100-B100 | B100-B0 (pooled sd of one cell)")
for g in ("exec", "pool", "other"):
    line(f"GROUP {g} total", lambda r, g=g: r["grp"].get(g, 0.0))
line("GROUP all", lambda r: sum(r["grp"].values()))
for g in ("exec", "pool"):
    print(f"\n-- cloid sites, group {g} (innermost site on the logical stack, inclusive of callees)")
    labs = sorted({k.split("|", 1)[1] for rs in sets.values() for r in rs for k in r["cat"] if k.startswith(g + "|")})
    for lab in labs:
        line(lab, lambda r, k=f"{g}|{lab}": r["cat"].get(k, 0.0))
    line("SUM of cloid sites (not none)", lambda r, g=g: sum(v for k, v in r["cat"].items() if k.startswith(g + "|") and not k.startswith(g + "|none")))
    for cand in ("1 index", "2 dup-check", "3 cleanup"):
        line(f"candidate {cand}", lambda r, g=g, c=cand: sum(v for k, v in r["cat"].items() if k.startswith(f"{g}|{c}|")))
print("\n-- hash leaf frames (leaf | cloid cand), top by |F100-B100| over exec+pool")
keys = collections.Counter()
for rs in sets.values():
    for r in rs:
        for k in r["hb"]:
            if k.split("|")[0] in ("exec", "pool"):
                keys[k] += 0
def mk(k, s): return st.mean([r["hb"].get(k, 0.0) for r in sets[s]])
for k in sorted(keys, key=lambda k: -abs(mk(k, "F100") - mk(k, "B100")))[:14]:
    line(k, lambda r, k=k: r["hb"].get(k, 0.0))
print("\n-- self by leaf function (exec+pool), top 16 by |F100-B100|")
keys = {k for rs in sets.values() for r in rs for k in r["self_f"] if k.split("|")[0] in ("exec", "pool")}
def ms(k, s): return st.mean([r["self_f"].get(k, 0.0) for r in sets[s]])
for k in sorted(keys, key=lambda k: -abs(ms(k, "F100") - ms(k, "B100")))[:16]:
    line(k, lambda r, k=k: r["self_f"].get(k, 0.0))
print("\n-- W2 exec thread, events per 1k fills")
evs = sorted({e for rs in sets.values() for r in rs for e in r["ev_tot"]})
for e in evs:
    v = {k: [r["ev_tot"].get(e, 0.0) for r in rs] for k, rs in sets.items()}
    d = st.mean(v["F100"]) - st.mean(v["B100"]); p = pooled(v["F100"], v["B100"]); d2 = st.mean(v["B100"]) - st.mean(v["B0"])
    print(f"{e:32} F100 {st.mean(v['F100']):12.0f}({sd(v['F100']):8.0f}) B100 {st.mean(v['B100']):12.0f}({sd(v['B100']):8.0f}) B0 {st.mean(v['B0']):12.0f} "
          f"| F-B {100 * d / st.mean(v['B100']):+6.1f}% s/sd {d / p:+5.1f} | B100-B0 {100 * d2 / st.mean(v['B0']):+6.1f}%")
    for c in ("1 index", "2 dup-check", "3 cleanup", "none"):
        w = {k: [r["ev_cat"].get(f"{e}|{c}", 0.0) for r in rs] for k, rs in sets.items()}
        print(f"    {c:12} F100 {st.mean(w['F100']):12.0f} B100 {st.mean(w['B100']):12.0f} B0 {st.mean(w['B0']):12.0f}  F-B {st.mean(w['F100']) - st.mean(w['B100']):+12.0f}")
print("\nIPC (instructions/cycles) per cell, W2 exec thread:")
for k, rs in sets.items():
    ipc = [r["ev_tot"].get("instructions", 0) / r["ev_tot"]["cycles"] for r in rs]
    print(f"  {k}: " + " ".join(f"{x:.3f}" for x in ipc) + f"  mean {st.mean(ipc):.3f} sd {sd(ipc):.3f}")
print("\nIPC at cloid sites (W2 exec thread): instructions/cycles by candidate")
for k, rs in sets.items():
    o = []
    for c in ("1 index", "2 dup-check", "3 cleanup", "none"):
        i = sum(r["ev_cat"].get(f"instructions|{c}", 0) for r in rs); cy = sum(r["ev_cat"].get(f"cycles|{c}", 0) for r in rs)
        o.append(f"{c} {i / cy if cy else float('nan'):.2f}")
    print(f"  {k}: " + ", ".join(o))
