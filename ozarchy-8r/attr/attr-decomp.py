#!/usr/bin/env python3
"""Per replicate: blocks/s difference (arm - A, post window, 1 Hz node-seconds) split into within-class terms
(class = state DB compaction running / consensus-only compaction / neither; weighted at the arm's shares) and the mix
term (share shift at A's per-class rates). attr-decomp.py ROOT RUN_DIR (imports attr-1hz.py helpers)."""
import json
import pathlib
import statistics as st
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
H = __import__("attr-1hz".replace("-", "_")) if False else None
src = (pathlib.Path(__file__).resolve().parent / "attr-1hz.py").read_text().replace('if __name__ == "__main__":\n    main()', "")
ns = {}
exec(compile(src, "attr-1hz.py", "exec"), ns)
CL = {
    "none": lambda x: not x["scomp"] and not x["ccomp"],
    "state": lambda x: x["scomp"],
    "cons": lambda x: x["ccomp"] and not x["scomp"],
}


def classes(root, meta, k):
    d = root / f"ozarchy-8r-300m-{k}"
    tb0 = json.loads((d / "summary.json").read_text())["timing"]["t_bench0"]
    m = meta[k[:2]]
    rows = ns["load"](d)
    xs = []
    for n in ns["NODES"]:
        xs += ns["per_second"](rows[n], tb0 + m["w0"], tb0 + m["w1"], {"slow": [], "prune": []})
    out = {}
    for c, f in CL.items():
        sel = [x for x in xs if f(x)]
        out[c] = (len(sel) / len(xs), st.mean(x["blk"] for x in sel))
    out["all"] = (1.0, st.mean(x["blk"] for x in xs))
    return out


def main():
    root, run = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    meta = json.loads((run / "analyst" / "r8-analyse.json").read_text())["meta"]
    print("| rep | arm | total diff blk/s | within state-comp | within none | within cons-only | mix |")
    print("|---|---|---|---|---|---|---|")
    acc = {}
    for rep in ("r1", "r2", "r3", "r4"):
        a = classes(root, meta, f"{rep}-A")
        for arm in "BC":
            b = classes(root, meta, f"{rep}-{arm}")
            w = {c: b[c][0] * (b[c][1] - a[c][1]) for c in CL}
            mix = sum((b[c][0] - a[c][0]) * a[c][1] for c in CL)
            tot = b["all"][1] - a["all"][1]
            acc.setdefault(arm, []).append((tot, w["state"], w["none"], w["cons"], mix))
            print(f"| {rep} | {arm} | {tot:+.2f} | {w['state']:+.2f} | {w['none']:+.2f} | {w['cons']:+.2f} | {mix:+.2f} |")
    for arm, v in acc.items():
        cols = [st.mean(x[i] for x in v) for i in range(5)]
        neg = [sum(1 for x in v if x[i] < 0) for i in range(5)]
        print(f"| mean | {arm} | " + " | ".join(f"{c:+.2f} ({n}/4 < 0)" for c, n in zip(cols, neg)) + " |")


if __name__ == "__main__":
    main()
