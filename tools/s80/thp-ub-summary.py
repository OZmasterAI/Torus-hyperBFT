#!/usr/bin/env python3
"""Summarise thp-heap proxy runs: per label the MEDIAN engine ms/1k fills, run list, THP share; per arm the mean."""
import glob, re, sys, os, statistics as st
R = "/home/oz/projects/wt/novel-s80/proto/results"
pfx = sys.argv[1]
CONF = {"D": "-", "G": "-", "N": "thp:never", "A": "thp:always,metadata_thp:always", "H": "hpa:true",
        "E": "hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152"}
rows = {}
for log in sorted(glob.glob(f"{R}/ub-thp-heap-{pfx}-*.log")):
    lab = os.path.basename(log)[len("ub-thp-heap-"):-4]
    m = re.search(r"MEDIAN engine_ms/1k_fills=([\d.]+) runs=\[([^\]]*)\]", open(log).read())
    if not m:
        continue
    runs = [float(x) for x in m.group(2).split(",")]
    parts = re.findall(r"exec/1k=([\d.]+) margin/1k=([\d.]+) match/1k=([\d.]+) settle/1k=([\d.]+) tail\(liq\)/1k=([\d.]+)", open(log).read())
    thp = None
    tsv = f"{R}/thp-ub-{lab}.tsv"
    if os.path.exists(tsv):
        # a sampler started before its ub.sh got the lock can also catch another arm's process (same binary):
        # keep only rows whose MALLOC_CONF is this arm's
        want = CONF.get(re.sub(r"\d+$", "", lab.split("-")[-1]), None)
        v = [l.split("\t") for l in open(tsv).read().splitlines()[1:]]
        v = [x for x in v if want is None or x[6] == want]
        if v:
            thp = (st.mean(float(x[4]) for x in v), min(float(x[4]) for x in v), st.mean(int(x[2]) for x in v) / 1024, len(v))
    rows[lab] = (float(m.group(1)), runs, thp, parts)
arms = {}
for lab, (med, runs, thp, parts) in rows.items():
    arm = re.sub(r"\d+$", "", lab.split("-")[-1])
    arms.setdefault(arm, []).append((lab, med, thp))
    t = f"thp mean {thp[0]:.1f}% min {thp[1]:.1f}% anon {thp[2]:.0f} MB (n={thp[3]})" if thp else "thp -"
    print(f"{lab:10s} median {med:.3f} runs {[round(x,3) for x in runs]} {t}")
print()
base = None
for arm in sorted(arms, key=lambda a: "DGNAHE".find(a)):
    meds = [x[1] for x in arms[arm]]
    th = [x[2][0] for x in arms[arm] if x[2]]
    an = [x[2][2] for x in arms[arm] if x[2]]
    mean = st.mean(meds)
    if arm == "D":
        base = mean
    d = f" vs D {100*(mean-base)/base:+.1f}%" if base and arm != "D" else ""
    print(f"arm {arm}: n={len(meds)} medians {meds} mean {mean:.3f}{d}  THP mean {st.mean(th) if th else float('nan'):.1f}%  anon {st.mean(an) if an else float('nan'):.0f} MB")
