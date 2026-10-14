#!/usr/bin/env python3
"""Read-only per-cell extraction for ozarchy-g3 (analyst). Writes only to analyst/cells.json."""
import json, os, re, sys, csv, statistics as st
B = "/home/oz/bench-results-matched"
RUN = B + "/ozarchy-g3-run"
EXP = {"goff": "d6b05d8e", "gjoff": "d6b05d8e", "moff": "c2fda80f", "mon": "c2fda80f"}
rows = [l.rstrip("\n").split("\t") for l in open(RUN + "/cells.tsv")][1:]

def hq(pairs, q):
    if not pairs: return None
    total = pairs[-1][1]
    if total <= 0: return None
    t = q * total; pl, pc = 0.0, 0.0
    for le, c in pairs:
        if c >= t:
            if le == float("inf"): return pl
            if c <= pc: return le
            return pl + (le - pl) * (t - pc) / (c - pc)
        pl, pc = le, c
    return pl

out = []
for r in rows:
    lab, arm = r[0], r[1]
    d = f"{B}/{lab}"
    c = {"label": lab.replace("ozarchy-g3-300m-", ""), "arm": arm, "rep": r[2]}
    c["rc_file"] = open(d + ".cell.rc").read().strip()
    s = json.load(open(d + "/summary.json"))
    h = s["headline"]
    c["status"] = s.get("status")
    c["matched_s"] = h["matched_s_avg"]
    c["agree"] = h["agreement_verdict"]; c["live"] = h["liveness_verdict"]
    c["accepted"] = h["benchmark_accepted"]; c["validity"] = s["validity"]["verdict"]
    c["fail_reasons"] = s["validity"].get("fail_reasons")
    c["summary_md5"] = s["binaries"]["torus_node_md5"][:8]
    c["bench_md5"] = s["binaries"]["bench_throughput_md5"][:8]
    c["t0"], c["t1"] = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    # agreement.jsonl
    ag = [json.loads(l) for l in open(d + "/agreement.jsonl") if l.strip()]
    c["ag_n"] = len(ag)
    c["bh_equal"] = len({a["block_hash"] for a in ag}) == 1
    c["sd_equal"] = len({a["state_digest"] for a in ag}) == 1
    c["panic"] = sum(a.get("panic_or_failstop_lines", 0) for a in ag)
    c["errlines"] = sum(a.get("error_lines", 0) for a in ag)
    c["rebuilds"] = [a.get("resident_rebuilds") for a in ag]
    sds = [open(f"{d}/state-digest-val{i}.txt").read() for i in range(3)]
    c["sdfile_equal"] = sds[0] == sds[1] == sds[2]
    # exe md5
    ex = re.findall(r"exe_md5=(\w+)", open(d + "/node-environ-trie.txt").read())
    c["exe_md5"] = ex
    c["exe_ok"] = len(ex) == 3 and all(e == EXP[arm] for e in ex)
    pre = open(d + "/node-preload.txt").read()
    mp = open(d + "/node-maps-jemalloc.txt").read()
    c["preload_n"] = pre.count("LD_PRELOAD=/usr/lib/libjemalloc.so.2")
    c["mallocconf_n"] = pre.count("MALLOC_CONF=")
    c["maps_je_n"] = mp.count("/usr/lib/libjemalloc.so.2")
    c["pre_lines"] = len([l for l in pre.splitlines() if l.strip()])
    # cpu
    pc = s["proc_cpu_by_node"]
    cs = []
    for n in ("val0", "val1", "val2"):
        L = pc[n]["load"]
        cs.append((L["user_ms"] + L["sys_ms"]) / 1000.0 / (L["fills"] / 1e6))
    c["cpu_s_per_1M"] = st.mean(cs)
    c["cpu_s_per_1M_nodes"] = cs
    W = pc["val0"]["whole_run"]
    c["cpu_s_per_1M_whole"] = st.mean([(pc[n]["whole_run"]["user_ms"] + pc[n]["whole_run"]["sys_ms"]) / 1000.0 / (pc[n]["whole_run"]["fills"] / 1e6) for n in ("val0","val1","val2")])
    c["node_pcpu"] = st.mean(v for k, v in s["cpu"]["pcpu_avg_by_proc"].items() if k.startswith("torus-node"))
    c["load1_avg_summary"] = s["cpu"]["load1_avg"]; c["load1_max_summary"] = s["cpu"]["load1_max"]
    # cpu.csv: load1 and rss within bench window
    lo, hi = c["t0"], c["t1"]
    loads = {}; rss = {}
    for row in csv.DictReader(open(d + "/cpu.csv")):
        t = int(row["ts"])
        if lo <= t <= hi:
            loads[t] = float(row["load1"])
            if row["comm"] == "torus-node":
                rss.setdefault(row["pid"], []).append(int(row["rss_kb"]))
    lv = list(loads.values())
    c["load1_mean"] = st.mean(lv) if lv else None; c["load1_max"] = max(lv) if lv else None
    c["load1_n"] = len(lv)
    c["rss_peak_mb"] = max(max(v) for v in rss.values()) / 1024 if rss else None
    c["rss_mean_mb"] = st.mean(st.mean(v) for v in rss.values()) / 1024 if rss else None
    # mem-samples VmHWM at end (max over samples per node)
    hwm = {}
    for l in open(d + "/mem-samples.txt"):
        if l.startswith("M "):
            p = l.split(); m = re.search(r"VmHWM:=(\d+)", l)
            if m: hwm[p[2]] = max(hwm.get(p[2], 0), int(m.group(1)))
    c["vmhwm_mb"] = st.mean(hwm.values()) / 1024 if hwm else None
    # jemalloc metrics after (M arms only)
    al = []; rs = []
    for i in range(3):
        t = open(f"{d}/metrics-after-val{i}.txt").read()
        m = re.search(r"^torus_jemalloc_allocated_bytes (\S+)", t, re.M)
        if m: al.append(float(m.group(1)))
        m = re.search(r"^torus_jemalloc_resident_bytes (\S+)", t, re.M)
        if m: rs.append(float(m.group(1)))
    c["je_alloc_mb"] = st.mean(al) / 2**20 if al else None
    c["je_resident_mb"] = st.mean(rs) / 2**20 if rs else None
    # block time from buckets.csv over load window, per node, check against summary p50/p95
    bk = {}
    for row in csv.DictReader(open(d + "/buckets.csv")):
        if row["metric"] != "torus_commit_interval_seconds_bucket": continue
        le = float("inf") if row["le"] == "+Inf" else float(row["le"])
        bk.setdefault(row["node"], {}).setdefault(int(row["ts"]), {})[le] = float(row["count"])
    q = {}
    for n, ser in bk.items():
        ts = sorted(ser)
        a = max([t for t in ts if t <= lo], default=ts[0]); b = min([t for t in ts if t >= hi], default=ts[-1])
        pairs = sorted((le, ser[b][le] - ser[a].get(le, 0)) for le in ser[b])
        q[n] = {k: (hq(pairs, v) or 0) * 1000 for k, v in (("p50", .5), ("p95", .95), ("p99", .99))}
    c["bt"] = q
    c["bt_p50"] = st.mean(v["p50"] for v in q.values()); c["bt_p95"] = st.mean(v["p95"] for v in q.values()); c["bt_p99"] = st.mean(v["p99"] for v in q.values())
    c["sum_p50_val0"] = h["commit_interval_ms_p50"]; c["sum_p95_val0"] = h["commit_interval_ms_p95"]
    c["ci_avg"] = h["commit_interval_ms_avg"]
    c["wall_ms_per_block"] = h["wall_ms_per_committed_block"]
    c["block_ms"] = s["phase_by_node"]["val0"]["block_ms"]
    c["mix"] = s["ingest"]["econ_mix"]; c["submitted"] = s["ingest"]["bench_submitted_actions"]
    c["evict"] = s["ingest"]["mempool_nonce_expired_evictions_per_node"]
    c["fills_per_blk"] = h["fills_per_native_block"]; c["native_blk_s"] = h["native_blk_s"]
    c["engine_ms_1k"] = h["engine_ms_per_1k_fills"]
    out.append(c)
json.dump(out, open(RUN + "/analyst/cells.json", "w"), indent=1)
for c in out:
    print("\t".join(str(x) for x in [c["label"], c["arm"], c["rc_file"], c["status"], c["agree"], c["live"], c["validity"], c["bh_equal"], c["sd_equal"], c["sdfile_equal"], c["panic"], c["errlines"], c["rebuilds"], "".join(sorted(set(c["exe_md5"]))), len(c["exe_md5"]), c["exe_ok"], c["summary_md5"], c["bench_md5"], c["preload_n"], c["mallocconf_n"], c["maps_je_n"], c["matched_s"]]))
