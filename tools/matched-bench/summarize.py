#!/usr/bin/env python3
"""summarize.py — turn one run-cell.sh result directory into summary.json.

Every rate below is computed from NODE Prometheus counter deltas sampled at
1 Hz (sampler.csv), never from the bench's own accounting. Headline
matched/s = window average over the bench window [t_bench0, t_bench1] on val0;
best-60s = max over all sliding 60 s windows of the whole sample (bench+drain).
Phase breakdown = per-executed-block ms from histogram _sum deltas over the
bench+drain window (so every loaded block is included), divided by the
torus_exec_block_seconds_count delta (per-phase count == block count by design).
"""
import argparse, csv, json, os, statistics, sys, time

ap = argparse.ArgumentParser()
for a in ["out", "label", "worktree", "commit", "dirty", "markets", "dur", "rate", "senders",
          "t-bench0", "t-bench1", "t-drain", "drained", "bench-rc", "idle-blks", "md5-node",
          "md5-bench", "genesis-md5", "genesis-markets", "genesis-accounts", "node-env",
          "env-digests", "extra-env", "bench-cmd", "pids", "evicted", "bench-submitted",
          "block-cap", "dissem"]:
    ap.add_argument("--" + a, default="")
A = ap.parse_args()
OUT = A.out
t0, t1, td = int(A.t_bench0), int(A.t_bench1), int(A.t_drain)

# ---------------------------------------------------------------- load sampler
rows = {"val0": [], "val1": [], "val2": []}
with open(os.path.join(OUT, "sampler.csv")) as f:
    rd = csv.DictReader(f)
    for r in rd:
        try:
            r = {k: (float(v) if k not in ("node",) else v) for k, v in r.items()}
        except (TypeError, ValueError):
            continue
        rows[r["node"]].append(r)

def m(r, k):
    return r.get("torus_" + k, 0.0) if r else 0.0

def rate(rs, key, lo, hi):
    """(counter[last<=hi] - counter[first>=lo]) / dt"""
    sel = [r for r in rs if lo <= r["ts"] <= hi]
    if len(sel) < 2:
        return 0.0, 0.0
    a, b = sel[0], sel[-1]
    dt = b["ts"] - a["ts"]
    return ((m(b, key) - m(a, key)) / dt if dt > 0 else 0.0), dt

def best60(rs, key):
    best = 0.0
    n = len(rs)
    for i in range(n):
        for j in range(i + 1, n):
            if rs[j]["ts"] - rs[i]["ts"] >= 60:
                dt = rs[j]["ts"] - rs[i]["ts"]
                best = max(best, (m(rs[j], key) - m(rs[i], key)) / dt)
                break
    return best

def worst60_blk(rs):
    worst = None
    n = len(rs)
    for i in range(n):
        for j in range(i + 1, n):
            if rs[j]["ts"] - rs[i]["ts"] >= 60:
                dt = rs[j]["ts"] - rs[i]["ts"]
                v = (m(rs[j], "block_height") - m(rs[i], "block_height")) / dt
                worst = v if worst is None else min(worst, v)
                break
    return worst or 0.0

funnel = {}
for node, rs in rows.items():
    if not rs:
        continue
    d = {}
    for key, name in [("orders_matched_total", "matched_s"), ("orders_placed_accepted_total", "placed_s"),
                      ("block_height", "blk_s"), ("blocks_committed_total", "committed_s"),
                      ("native_actions_processed_total", "actions_s"), ("orders_resting_total", "resting_s")]:
        v, dt = rate(rs, key, t0, t1)
        d[name + "_benchwin"] = round(v, 1)
        v2, dt2 = rate(rs, key, t0, td)
        d[name + "_incl_drain"] = round(v2, 1)
    d["benchwin_span_s"] = rate(rs, "orders_matched_total", t0, t1)[1]
    d["incl_drain_span_s"] = rate(rs, "orders_matched_total", t0, td)[1]
    d["matched_s_best60"] = round(best60(rs, "orders_matched_total"), 1)
    d["placed_s_best60"] = round(best60(rs, "orders_placed_accepted_total"), 1)
    d["blk_s_best60"] = round(best60(rs, "block_height"), 3)
    d["blk_s_worst60"] = round(worst60_blk(rs), 3)
    d["peak_exec_queue_depth"] = max(m(r, "exec_queue_depth") for r in rs)
    d["peak_mempool_native"] = max(m(r, "mempool_native_size") for r in rs)
    first = [r for r in rs if r["ts"] >= t0][0]
    last = rs[-1]
    for k in ["orders_matched_total", "orders_placed_accepted_total", "orders_resting_total",
              "native_actions_processed_total", "orders_rejected_margin_total", "orders_rejected_book_total",
              "orders_rejected_cancelled_total", "orders_rejected_other_total", "blocks_committed_total",
              "block_height", "consensus_timeout_total_total", "native_gossip_published_actions_total",
              "native_gossip_dropped_full_total", "member_cache_evictions_total"]:
        d["delta_" + k] = m(last, k) - m(first, k)
    d["samples"] = len(rs)
    funnel[node] = d

# ---------------------------------------------------------------- phase breakdown
PHASES = ["evm", "verify", "replay_guard", "load_books", "engine", "save_books", "flush", "body_persist"]
SUB = {"engine": ["phase_margin", "phase_match", "phase_settle"], "flush": ["root", "state_write", "evm_resync"]}
phase = {}
for node, rs in rows.items():
    if not rs:
        continue
    sel = [r for r in rs if t0 <= r["ts"] <= td]
    if len(sel) < 2:
        continue
    a, b = sel[0], sel[-1]
    nall = m(b, "exec_block_seconds_count") - m(a, "exec_block_seconds_count")   # every committed block
    nblk = m(b, "exec_engine_seconds_count") - m(a, "exec_engine_seconds_count")  # native (loaded) blocks only
    ncommit = m(b, "blocks_committed_total") - m(a, "blocks_committed_total")
    span = b["ts"] - a["ts"]
    if nblk <= 0:
        continue
    def dsum(k):
        return m(b, "exec_" + k + "_seconds_sum") - m(a, "exec_" + k + "_seconds_sum")
    def per_blk(k):
        return dsum(k) / nblk * 1000.0
    tot = per_blk("block")
    p = {"executed_native_blocks": nblk, "all_exec_blocks": nall, "committed_blocks": ncommit, "span_s": span,
         "wall_ms_per_committed_block": round(span / ncommit * 1000, 1) if ncommit else None,
         "wall_ms_per_native_block": round(span / nblk * 1000, 1),
         "block_ms": round(tot, 2),
         "exec_thread_busy_fraction": round(dsum("block") / span, 3) if span else None,
         "note": "ms are per NATIVE block (engine_count delta); block_ms includes the tiny cost of empty blocks in the window"}
    ph = {}
    acc = 0.0
    for k in PHASES:
        v = per_blk(k); acc += v
        ph[k] = {"ms": round(v, 2), "pct_of_block": round(100 * v / tot, 1) if tot else None,
                 "pct_of_wall": round(100 * dsum(k) / span, 1) if span else None}
        for s_ in SUB.get(k, []):
            ph[k][s_ + "_ms"] = round(per_blk(s_), 2)
    ph["residual_untimed"] = {"ms": round(tot - acc, 2), "pct_of_block": round(100 * (tot - acc) / tot, 1) if tot else None}
    p["phases"] = ph
    # early vs late (first / last 60 s of the LOADED window = until matched stops moving)
    loaded = [r for r in sel if m(r, "orders_matched_total") < m(sel[-1], "orders_matched_total")]
    if loaded:
        t_end = loaded[-1]["ts"]
        early = [r for r in sel if r["ts"] <= sel[0]["ts"] + 60]
        late = [r for r in sel if t_end - 60 <= r["ts"] <= t_end]
        def win(rs):
            if len(rs) < 2:
                return None
            a2, b2 = rs[0], rs[-1]
            n2 = m(b2, "exec_engine_seconds_count") - m(a2, "exec_engine_seconds_count")
            if n2 <= 0:
                return None
            d = {k: round((m(b2, "exec_" + k + "_seconds_sum") - m(a2, "exec_" + k + "_seconds_sum")) / n2 * 1000, 1)
                 for k in ["block"] + PHASES + ["phase_margin", "phase_match", "phase_settle", "root", "state_write"]}
            d["native_blocks"] = n2
            d["orders_placed_per_block"] = round((m(b2, "orders_placed_accepted_total") - m(a2, "orders_placed_accepted_total")) / n2)
            d["resting_orders_end"] = m(b2, "exec_resting_orders")
            d["span_s"] = b2["ts"] - a2["ts"]
            return d
        p["early_60s"] = win(early)
        p["late_60s"] = win(late)
    p["orders_placed_per_exec_block"] = round((m(b, "orders_placed_accepted_total") - m(a, "orders_placed_accepted_total")) / nblk, 1)
    p["orders_matched_per_exec_block"] = round((m(b, "orders_matched_total") - m(a, "orders_matched_total")) / nblk, 1)
    p["actions_per_exec_block"] = round((m(b, "native_actions_processed_total") - m(a, "native_actions_processed_total")) / nblk, 1)
    dbc = m(b, "exec_root_dirty_buckets_count") - m(a, "exec_root_dirty_buckets_count")
    p["dirty_buckets_per_flush"] = round((m(b, "exec_root_dirty_buckets_sum") - m(a, "exec_root_dirty_buckets_sum")) / dbc, 1) if dbc else None
    cic = m(b, "commit_interval_seconds_count") - m(a, "commit_interval_seconds_count")
    p["commit_interval_ms_avg"] = round((m(b, "commit_interval_seconds_sum") - m(a, "commit_interval_seconds_sum")) / cic * 1000, 1) if cic else None
    btc = m(b, "block_transactions_count_count") - m(a, "block_transactions_count_count")
    p["txs_per_block_avg"] = round((m(b, "block_transactions_count_sum") - m(a, "block_transactions_count_sum")) / btc, 1) if btc else None
    phase[node] = p

# ---------------------------------------------------------------- agreement
agree_rows = []
try:
    with open(os.path.join(OUT, "agreement.jsonl")) as f:
        for line in f:
            line = line.strip()
            if line:
                agree_rows.append(json.loads(line))
except (OSError, ValueError) as e:
    agree_rows = []
agreement = {"nodes": agree_rows}
if len(agree_rows) == 3:
    hs = [r["height"] for r in agree_rows]
    agreement["height_spread"] = max(hs) - min(hs)
    agreement["block_hash_equal"] = len({r["block_hash"] for r in agree_rows}) == 1 and agree_rows[0]["block_hash"] != "ERR"
    agreement["header_state_root_equal"] = len({r["header_state_root"] for r in agree_rows}) == 1
    agreement["state_digest_equal"] = len({r["state_digest"] for r in agree_rows}) == 1
    agreement["counters_equal"] = all(len({r[k] for r in agree_rows}) == 1 for k in ("matched", "placed", "resting", "actions"))
    agreement["panic_or_failstop_lines"] = sum(r["panic_or_failstop_lines"] for r in agree_rows)
    agreement["error_lines"] = sum(r["error_lines"] for r in agree_rows)
    agreement["validators_agree"] = bool(agreement["height_spread"] <= 5 and agreement["block_hash_equal"]
                                         and agreement["state_digest_equal"] and agreement["counters_equal"]
                                         and agreement["panic_or_failstop_lines"] == 0 and A.drained == "1")
else:
    agreement["validators_agree"] = False

# ---------------------------------------------------------------- dissemination / pacing
# run-cell.sh passes "val0:manifest=N,exhausted=N,sync_fallback=N,da_outbound_fail=N,starvation=N,pacing=N val1:... val2:..."
# (node log line counts). Block-cap-raise sweep gate: a raised cap is REJECTED when
# bodies stop disseminating (exhausted / sync_fallback / da_outbound_fail > 0 under
# load), whatever matched/s says.
def parse_dissem(raw):
    out = {}
    for tok in (raw or "").split():
        if ":" not in tok:
            continue
        node, kvs = tok.split(":", 1)
        d = {}
        for kv in kvs.split(","):
            if "=" in kv:
                k, v = kv.split("=", 1)
                try:
                    d[k] = int(v)
                except ValueError:
                    d[k] = None
        out[node] = d
    return out

dissem = parse_dissem(A.dissem)
if dissem:
    dissem["raw"] = A.dissem.strip()   # re-fed verbatim by resummarize.sh
    fail_keys = ("exhausted", "sync_fallback", "da_outbound_fail", "starvation")
    dissem["total_failures"] = sum((d.get(k) or 0) for n, d in dissem.items() if n.startswith("val") for k in fail_keys)
    dissem["total_manifest_pushes"] = sum((d.get("manifest") or 0) for n, d in dissem.items() if n.startswith("val"))
    dissem["total_pacing_lines"] = sum((d.get("pacing") or 0) for n, d in dissem.items() if n.startswith("val"))
    dissem["dissemination_clean"] = dissem["total_failures"] == 0

# ---------------------------------------------------------------- cpu
cpu = {}
try:
    with open(os.path.join(OUT, "cpu.csv")) as f:
        rd = csv.DictReader(f)
        per = {}
        loads = []
        for r in rd:
            per.setdefault(r["comm"] + ":" + r["pid"], []).append(float(r["pcpu"]))
            loads.append(float(r["load1"]))
        cpu = {"load1_max": max(loads) if loads else None,
               "load1_avg": round(statistics.mean(loads), 1) if loads else None,
               "ncpu": os.cpu_count(),
               "pcpu_avg_by_proc": {k: round(statistics.mean(v), 1) for k, v in per.items()}}
except (OSError, ValueError, KeyError):
    cpu = {}

# ---------------------------------------------------------------- bench log tail
bench_tail = ""
try:
    with open(os.path.join(OUT, "bench.log"), errors="replace") as f:
        bench_tail = "".join(f.readlines()[-25:])
except OSError:
    pass

v0 = funnel.get("val0", {})
summary = {
    "status": "OK" if (A.bench_rc == "0" and v0) else "DEGRADED",
    "label": A.label, "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
    "worktree": A.worktree, "commit": A.commit, "dirty_files": int(A.dirty or 0),
    "binaries": {"torus_node_md5": A.md5_node, "bench_throughput_md5": A.md5_bench},
    "genesis": {"md5": A.genesis_md5, "markets": int(A.genesis_markets or 0), "native_balances": int(A.genesis_accounts or 0)},
    "cell": {"markets": int(A.markets), "duration_s": int(A.dur), "rate_total": int(A.rate), "senders": int(A.senders),
             "block_cap": int(A.block_cap) if A.block_cap else None,
             "extra_env": A.extra_env, "node_env": json.loads(A.node_env) if A.node_env else {},
             "env_digests_per_node": A.env_digests.split(), "bench_cmd": A.bench_cmd, "node_pids": A.pids.split()},
    "timing": {"t_bench0": t0, "t_bench1": t1, "t_drain": td, "bench_wall_s": t1 - t0, "drain_s": td - t1,
               "drained": A.drained == "1", "bench_rc": int(A.bench_rc or -1)},
    "idle_blk_s": float(A.idle_blks or 0),
    "headline": {
        "matched_s_avg": v0.get("matched_s_benchwin"),
        "matched_s_best60": v0.get("matched_s_best60"),
        "matched_s_incl_drain": v0.get("matched_s_incl_drain"),
        "placed_s_avg": v0.get("placed_s_benchwin"),
        "blk_s_avg": v0.get("blk_s_benchwin"),
        "blk_s_worst60": v0.get("blk_s_worst60"),
        "peak_exec_queue_depth": v0.get("peak_exec_queue_depth"),
        "txs_per_block_avg": phase.get("val0", {}).get("txs_per_block_avg"),
        "actions_per_exec_block": phase.get("val0", {}).get("actions_per_exec_block"),
        "consensus_timeouts": v0.get("delta_consensus_timeout_total_total"),
        "dissemination_clean": dissem.get("dissemination_clean") if dissem else None,
        "validators_agree": agreement.get("validators_agree"),
    },
    "ingest": {"bench_submitted_actions": int(A.bench_submitted or 0),
               "val0_actions_processed": int(v0.get("delta_native_actions_processed_total", 0)),
               "mempool_nonce_expired_evictions_per_node": [int(x) for x in A.evicted.split()],
               "note": "NONCE_WINDOW_MS=60s: backlog older than 60 s is evicted silently; submitted-processed gap = expiry"},
    "funnel_by_node": funnel,
    "phase_by_node": phase,
    "agreement": agreement,
    "dissemination": dissem,
    "cpu": cpu,
    "bench_log_tail": bench_tail,
}
with open(os.path.join(OUT, "summary.json"), "w") as f:
    json.dump(summary, f, indent=1)
h = summary["headline"]
print(f"SUMMARY {A.label}: matched/s avg={h['matched_s_avg']} best60={h['matched_s_best60']} "
      f"placed/s={h['placed_s_avg']} blk/s={h['blk_s_avg']} txs/blk={h['txs_per_block_avg']} "
      f"timeouts={h['consensus_timeouts']} dissem_clean={h['dissemination_clean']} agree={h['validators_agree']} "
      f"drained={summary['timing']['drained']} bench_rc={A.bench_rc}")
p0 = phase.get("val0", {})
if p0:
    print(f"PHASE val0: block_ms={p0['block_ms']} wall/committed={p0['wall_ms_per_committed_block']} " +
          " ".join(f"{k}={v['ms']}({v['pct_of_block']}%)" for k, v in p0["phases"].items()))
