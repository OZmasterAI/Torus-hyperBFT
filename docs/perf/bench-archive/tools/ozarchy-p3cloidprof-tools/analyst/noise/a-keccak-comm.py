#!/usr/bin/env python3
"""a-keccak-comm.py: Keccak self time (leaf __KeccakF1600 / KECCAK_ASM_SHA3_* / keccak256 wrappers) by thread (comm, exec tid)
per cell, W1 val1 cycles:u, ms CPU per 1k val1 W1 fills (a-prof.py normalisation) and cores. Also callchain depth of Keccak
leaves (user frames below the leaf). Read-only: reads <cell>/prof.script.gz, <cell>/exec-tid.txt, ../prof-cells.json.
Usage: a-keccak-comm.py <prof-cells.json> <bench-results-matched dir>"""
import collections, gzip, json, re, sys, statistics as st
d = json.load(open(sys.argv[1])); B = sys.argv[2]
P = "ozarchy-p3cloidprof-300m-"; R = ("r1", "r2", "r3"); T2 = 4.302653
HDR = re.compile(r"^(\S.*?)\s+(\d+)\s+(\d+)\s*$")
KEC = re.compile(r"^(__KeccakF1600|KECCAK_ASM_SHA3_\w+|alloy_primitives::utils::keccak256\w*|keccak256)\b|sha3|tiny_keccak")
VER = re.compile(r"verify_one_action_with")
GOS = re.compile(r"add_native_action_from_gossip")
def cls(comm, tid, et):
    if tid == et: return "exec thread"
    for p, n in (("torus-gossip-ve", "torus-gossip-verify (gossip ingest ecrecover)"), ("torus-ingress-v", "torus-ingress-verify (RPC submit verify)"),
                 ("hotstuff-algo", "hotstuff-algo (consensus)"), ("rpc-worker", "rpc-worker"), ("torus-execution", "torus-execution (non-exec tids)")):
        if comm.startswith(p): return n
    return "other: " + comm
res = {}
for r in R:
    for a in ("c0", "c100"):
        name = P + r + "-" + a; cell = f"{B}/{name}"; et = open(f"{cell}/exec-tid.txt").read().strip()
        w1 = d[name]["w1"]; tot = 0; kec = collections.Counter(); depth = collections.Counter(); ksym = collections.Counter()
        ver_comm = collections.Counter(); gos_comm = collections.Counter(); comm_all = collections.Counter()
        hdr = None; fr = []
        def flush():
            global tot
            if hdr is None: return
            comm, tid, per = hdr; tot += per; c = cls(comm, tid, et); comm_all[c] += per
            user = [s for ip, s in fr if not ip.startswith("ffffffff")]
            if user and KEC.search(user[0]):
                kec[c] += per; ksym[user[0]] += per; depth[min(len(user) - 1, 5)] += 1
            txt = " ".join(user)
            if VER.search(txt): ver_comm[c] += per
            if GOS.search(txt): gos_comm[c] += per
        for line in gzip.open(f"{cell}/prof.script.gz", "rt", errors="replace"):
            if not line.strip():
                flush(); hdr = None; fr = []; continue
            if line[0] not in " \t":
                m = HDR.match(line.rstrip("\n")); hdr = (m.group(1).strip(), m.group(2), int(m.group(3))) if m else None; fr = []; continue
            p = line.strip().split(" ", 1)
            s = re.sub(r"\+0x[0-9a-f]+$", "", p[1]) if len(p) > 1 else "[unknown]"
            fr.append((p[0], s))
        flush()
        k = w1["utime"] / w1["fills"] * 1e6 / tot; fps = w1["fills"] / w1["span"]
        res[(r, a)] = dict(kec={c: v * k for c, v in kec.items()}, ver={c: v * k for c, v in ver_comm.items()},
                           gos={c: v * k for c, v in gos_comm.items()}, comm={c: v * k for c, v in comm_all.items()},
                           depth=dict(depth), ksym={s: v * k for s, v in ksym.items()}, fps=fps)
def table(title, key, cores=False):
    print(f"\n== {title} ({'cores = ms/1k x W1 fills/s / 1e6' if cores else 'ms CPU per 1k val1 W1 fills'})")
    cs = sorted(set(c for v in res.values() for c in v[key]), key=lambda c: -res[("r1", "c100")][key].get(c, 0))
    print(f"{'thread':50} " + " ".join(f"{r+'-'+a:>8}" for a in ("c0", "c100") for r in R) + f" {'c0 mean±sd':>14} {'c100 mean±sd':>14} {'delta':>7} {'95% CI t(2)':>17}  res")
    rows = cs + ["TOTAL"]
    for c in rows:
        def g(r, a):
            v = res[(r, a)][key]; x = sum(v.values()) if c == "TOTAL" else v.get(c, 0.0)
            return x * res[(r, a)]["fps"] / 1e6 if cores else x
        c0 = [g(r, "c0") for r in R]; c1 = [g(r, "c100") for r in R]
        if max(c0 + c1) < (0.0005 if cores else 0.005): continue
        dl = [y - x for x, y in zip(c0, c1)]; m = st.mean(dl); h = T2 * st.stdev(dl) / 3 ** 0.5
        print(f"{c:50} " + " ".join(f"{x:8.3f}" for x in c0 + c1) + f" {st.mean(c0):6.3f}±{st.stdev(c0):5.3f} {st.mean(c1):6.3f}±{st.stdev(c1):5.3f} {m:+7.3f} [{m-h:+7.3f},{m+h:+7.3f}]  {'yes' if abs(m) > h else 'no'}")
table("Keccak self by thread", "kec"); table("Keccak self by thread", "kec", cores=True)
table("samples whose user stack contains verify_one_action_with, by thread (inclusive)", "ver")
table("samples whose user stack contains add_native_action_from_gossip, by thread (inclusive)", "gos")
table("all samples by thread", "comm"); table("all samples by thread", "comm", cores=True)
print("\n== Keccak leaf samples by number of user frames below the leaf (0 = bare leaf, no caller)")
for (r, a), v in sorted(res.items()):
    n = sum(v["depth"].values()); print(f"{r}-{a}: " + " ".join(f"{k}:{v['depth'].get(k,0)}" for k in range(6)) + f"  bare {v['depth'].get(0,0)/n*100:.2f}%")
print("\n== Keccak self by leaf symbol, mean per arm (ms/1k)")
syms = sorted(set(s for v in res.values() for s in v["ksym"]))
for s in syms:
    c0 = st.mean(res[(r, "c0")]["ksym"].get(s, 0) for r in R); c1 = st.mean(res[(r, "c100")]["ksym"].get(s, 0) for r in R)
    print(f"  {s[:70]:70} c0 {c0:.3f} c100 {c1:.3f} delta {c1-c0:+.3f}")
