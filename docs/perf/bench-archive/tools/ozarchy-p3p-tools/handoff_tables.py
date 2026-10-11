# Read-only extraction of compact numbers from the ozarchy-p3p cell dirs (no conclusions).
# usage: python3 -I handoff_tables.py <results_root> <prefix>  -> prints tables to stdout
import glob, json, os, re, sys
root, prefix = sys.argv[1], sys.argv[2]
def load(p):
    try:
        return json.load(open(p))
    except Exception:
        return None
def md5s(d):
    try:
        vals = re.findall(r"exe_md5=([0-9a-f]{8})", open(os.path.join(d, "node-environ-trie.txt")).read())
        return ",".join(sorted(set(vals))) + " (%d)" % len(vals)
    except Exception:
        return "?"
def cpu(s, key):
    out = []
    for v in ("val0", "val1", "val2"):
        w = (((s or {}).get("proc_cpu_by_node") or {}).get(v) or {}).get(key) or {}
        out.append((w.get("user_ms", 0) + w.get("sys_ms", 0)) / 1000.0)
    return out
def rc_of(label):
    try:
        return open(os.path.join(root, prefix + "-" + label + ".cell.rc")).read().strip().replace("rc=", "", 1)
    except Exception:
        return "?"
cells = sorted(glob.glob(os.path.join(root, prefix + "-*")))
cells = [c for c in cells if os.path.isdir(c)]
print("# per cell: label | rc | agree | liveness | accepted | matched/s avg | matched/s first120 | native blk/s | matched total val0 | node md5 | cpu-s load val0,val1,val2 | cpu-s whole val0,val1,val2")
rows = {}
for d in cells:
    label = os.path.basename(d)[len(prefix) + 1:]
    s = load(os.path.join(d, "summary.json"))
    h = (s or {}).get("headline") or {}
    fv = (((s or {}).get("funnel_by_node") or {}).get("val0") or {})
    lo = cpu(s, "load"); wh = cpu(s, "whole_run")
    row = dict(label=label, rc=rc_of(label), agree=h.get("agreement_verdict"), live=h.get("liveness_verdict"),
               acc=h.get("benchmark_accepted"), mps=h.get("matched_s_avg"), mps120=h.get("matched_s_first120"),
               nblk=h.get("native_blk_s"), mtot=fv.get("delta_orders_matched_total"), md5=md5s(d),
               cpu_load=lo, cpu_whole=wh)
    rows[label] = row
    f = lambda x: ("%.2f" % x) if isinstance(x, (int, float)) else str(x)
    print("%s | rc=%s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s" % (
        label, row["rc"], row["agree"], row["live"], row["acc"], f(row["mps"]), f(row["mps120"]), f(row["nblk"]),
        row["mtot"], row["md5"], ",".join(f(x) for x in lo), ",".join(f(x) for x in wh)))
print()
print("# per arm (throughput cells r1-r4): matched/s avg per round, mean, max-min spread; cpu-s load val0-2 sum; matched total val0")
arms = ["c", "a", "k", "cS", "aS"]
for a in arms:
    vals = []; cpus = []; mt = []
    for r in ("r1", "r2", "r3", "r4"):
        row = rows.get("%s-%s" % (r, a))
        if row:
            vals.append(row["mps"]); cpus.append(sum(row["cpu_load"])); mt.append(row["mtot"])
    if not vals:
        continue
    vv = [x for x in vals if isinstance(x, (int, float))]
    mean = sum(vv) / len(vv) if vv else None
    spread = (max(vv) - min(vv)) if vv else None
    print("arm %-3s matched/s per round %s | mean %s | spread %s | cpu-s load sum per round %s | matched_total val0 per round %s" % (
        a, [round(x, 1) if isinstance(x, (int, float)) else x for x in vals],
        round(mean, 1) if mean else None, round(spread, 1) if spread is not None else None,
        [round(x, 1) for x in cpus], mt))
print()
print("# restart cells: restart-metrics.json, crash-kill.json signal/pre_kill, crash.json restart gap, crash gate lines")
for d in cells:
    label = os.path.basename(d)[len(prefix) + 1:]
    if not (label.startswith("g") or label.startswith("x")):
        continue
    rm = load(os.path.join(d, "restart-metrics.json"))
    ck = load(os.path.join(d, "crash-kill.json"))
    cr = load(os.path.join(d, "crash.json"))
    print("## " + label)
    keys = ["down_s", "db_open_s", "restart_to_db_open_s", "restart_to_first_commit_s", "max_commit_gap_s",
            "max_gap_from_kill_s", "rewind_blocks", "wal_files_at_kill", "wal_bytes_at_kill", "wal_files_replayed",
            "wal_bytes_replayed", "wal_replayed_missing_from_sample"]
    print("  restart-metrics: " + (" ".join("%s=%s" % (k, rm.get(k)) for k in keys) if rm else "none"))
    if ck:
        print("  crash-kill: signal=%s kill_seq=%s killed_pid=%s down_s=%s pre_kill=%s" % (
            ck.get("kill_signal", "KILL(default)"), ck.get("kill_seq"), ck.get("killed_pid"), ck.get("down_s"), json.dumps(ck.get("pre_kill"))))
    if cr:
        print("  crash.json restart: " + json.dumps(cr.get("restart"), separators=(",", ":"))[:400])
    try:
        lines = [l.rstrip()[:220] for l in open(os.path.join(d, "run.log"), errors="replace") if "crash gate" in l or "crash-kill" in l]
        for l in lines[:12]:
            if "crash gate" not in l and "VALIDITY" not in l and "crash-kill:" not in l:
                continue
            print("  run.log: " + re.sub(r"\x1b\[[0-9;]*m", "", l))
    except Exception:
        pass
