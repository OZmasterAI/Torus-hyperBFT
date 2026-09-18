#!/usr/bin/env python3
"""summarize.py — turn one run-cell.sh result directory into summary.json.

Every rate below is computed from NODE Prometheus counter deltas sampled at
1 Hz (sampler.csv), never from the bench's own accounting. Headline
matched/s = window average over the bench window [t_bench0, t_bench1] on val0;
best-60s = max over all sliding 60 s windows of the whole sample (bench+drain).
Phase breakdown = per-executed-block ms from histogram _sum deltas over the
bench+drain window (so every loaded block is included), divided by the
torus_exec_block_seconds_count delta (per-phase count == block count by design).
A phase a WORKER observes (flush, under TORUS_EXEC_PIPELINE) is reported as an
OFF-CHAIN line and left out of that per-block sum: `block_ms` is the EXEC
thread's own timer, so counting worker ms into it drives residual_untimed
negative and the percentages past 100 %.
"""
import argparse, csv, json, os, statistics, sys, time
from health import assess_liveness, acceptance, DEFAULT_STALL_S

ap = argparse.ArgumentParser()
for a in ["out", "label", "worktree", "commit", "dirty", "markets", "dur", "rate", "senders",
          "t-bench0", "t-bench1", "t-drain", "drained", "bench-rc", "idle-blks", "md5-node",
          "md5-bench", "genesis-md5", "genesis-markets", "genesis-accounts", "node-env",
          "env-digests", "extra-env", "bench-cmd", "pids", "evicted", "bench-submitted",
          "block-cap", "dissem", "digest-secs", "digest-heights", "digest-quiescent",
          "drain-timeout", "markets-per-sender"]:
    ap.add_argument("--" + a, default="")
A = ap.parse_args()
OUT = A.out
t0, t1, td = int(A.t_bench0), int(A.t_bench1), int(A.t_drain)
NODE_ENV = json.loads(A.node_env) if A.node_env else {}

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
    # matched-cell-duration-parity: the first 120 s of the bench window, so a
    # 120 s cell and a 300 s cell can be compared on the SAME window. (A 300 s
    # cell's avg is dragged down by the late, deep-book regime; a 120 s cell
    # never sees it. best60 is the peak, first120 is the like-for-like.)
    for key, name in [("orders_matched_total", "matched_s"),
                      ("orders_placed_accepted_total", "placed_s"),
                      ("block_height", "blk_s")]:
        v, _ = rate(rs, key, t0, min(t1, t0 + 120))
        d[name + "_first120"] = round(v, 1)
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
# r7 state-write-build-vs-db-split: state_write is reported alongside its two
# halves — state_write_build (serializing the pending maps into the WriteBatch)
# and state_write_db (the atomic rocksdb write: WAL + memtable). build + db ==
# state_write to rounding; both are 0.0 on a pre-r7 binary.
FLUSH_SUB_R7 = ["state_write_build", "state_write_db"]
# r6 engine-untimed-attribution: the six sub-timers app.rs observes ONCE per
# native block, decomposing the engine share margin/match/settle never covered.
# phase1_actions + phase_margin + phase_match + phase_settle + post_engine_tail
# + engine_untimed == engine (by construction); settle_pass_a / settle_pass_b /
# cache_flush are NESTED inside phase_settle, so do not re-add them.
ENGINE_SUB_R6 = ["phase1_actions", "settle_pass_a", "settle_pass_b", "cache_flush",
                 "post_engine_tail", "engine_untimed"]
# bl1 exec-chain-sub-100-attribution: save_books pass 1 (journal DRAIN, reads the
# LIVE book levels -> can never leave the exec thread) vs pass 2 (overlay WRITES
# -> the only half a flush worker could take). drain + write == save_books to
# rounding; both 0.0 on a pre-bl1 binary AND on book modes 0/1 (no two-pass save).
SAVE_SUB_BL1 = ["save_books_drain", "save_books_write"]
SUB = {"engine": ["phase_margin", "phase_match", "phase_settle"] + ENGINE_SUB_R6,
       "save_books": SAVE_SUB_BL1,
       "flush": ["root", "state_write"] + FLUSH_SUB_R7 + ["evm_resync"]}


def hist_quantile(pairs, q):
    """Prometheus histogram_quantile over CUMULATIVE bucket-count deltas.

    `pairs` = [(le, cumulative_delta)] sorted ascending with +Inf last. Returns
    None when the window observed nothing (never 0.0, which would read as
    "instant" rather than "no data").
    """
    if not pairs:
        return None
    total = pairs[-1][1]
    if total <= 0:
        return None
    target = q * total
    prev_le, prev_c = 0.0, 0.0
    for le, c in pairs:
        if c >= target:
            if le == float("inf"):
                return prev_le
            if c <= prev_c:
                return le
            return prev_le + (le - prev_le) * (target - prev_c) / (c - prev_c)
        prev_le, prev_c = le, c
    return prev_le


def read_buckets(path):
    """run-cell.sh samples selected histogram BUCKETS into buckets.csv in long
    format (ts,node,metric,le,count). Returns the parsed rows as
    [(ts, node, metric, le, count)]. Missing file (pre-bl1 harness /
    resummarize of an old cell) => [] => every percentile below is None."""
    out = []
    try:
        with open(path) as f:
            for r in csv.DictReader(f):
                try:
                    ts = float(r["ts"])
                    cnt = float(r["count"])
                    le = float("inf") if r["le"].lstrip("+").lower().startswith("inf") else float(r["le"])
                except (TypeError, ValueError, KeyError, AttributeError):
                    continue
                out.append((ts, r["node"], r["metric"], le, cnt))
    except OSError:
        return []
    return out


def bucket_deltas(brows, lo, hi):
    """{(node, metric): [(le, cumulative_delta)]} over the window [lo, hi]."""
    first, last = {}, {}
    for ts, node, metric, le, cnt in brows:
        if not (lo <= ts <= hi):
            continue
        k = (node, metric, le)
        if k not in first:
            first[k] = cnt
        last[k] = cnt
    out = {}
    for (node, metric, le), c1 in last.items():
        out.setdefault((node, metric), []).append((le, c1 - first[(node, metric, le)]))
    for v in out.values():
        v.sort(key=lambda x: x[0])
    return out


def load_buckets(path, lo, hi):
    return bucket_deltas(read_buckets(path), lo, hi)


def cadence(rs, lo, hi, buckets, node):
    """Block cadence / commit-interval fields over the sampler window [lo, hi].

    Returned for THREE windows per node (see phase_by_node): `load` [t0, t1]
    (the bench is submitting), `drain` [t1, t_drain] (the chain free-runs on
    EMPTY blocks at 20-30 blk/s while the backlog empties) and `incl_drain`
    [t0, t_drain] (the pre-bl-sweep blend of both, kept for back-compat). The
    blend was the trap: bl-sweep-25-2 reads 151 ms/committed block and 2.0
    empty blk/s over incl_drain, but 231 ms / 0.29 empty blk/s under load.
    None when the window has < 2 samples.
    """
    sel = [r for r in rs if lo <= r["ts"] <= hi]
    if len(sel) < 2:
        return None
    a, b = sel[0], sel[-1]
    span = b["ts"] - a["ts"]
    nall = m(b, "exec_block_seconds_count") - m(a, "exec_block_seconds_count")
    nblk = m(b, "exec_engine_seconds_count") - m(a, "exec_engine_seconds_count")
    ncommit = m(b, "blocks_committed_total") - m(a, "blocks_committed_total")
    busy = m(b, "exec_block_seconds_sum") - m(a, "exec_block_seconds_sum")
    cic = m(b, "commit_interval_seconds_count") - m(a, "commit_interval_seconds_count")
    cis = m(b, "commit_interval_seconds_sum") - m(a, "commit_interval_seconds_sum")

    def pctl(metric, q):
        v = hist_quantile(buckets.get((node, metric)), q)
        return round(v * 1000.0, 1) if v is not None else None

    return {
        "window": [lo, hi],
        "span_s": span,
        "committed_blocks": ncommit,
        "all_exec_blocks": nall,
        "executed_native_blocks": nblk,
        "wall_ms_per_committed_block": round(span / ncommit * 1000, 1) if ncommit else None,
        "wall_ms_per_native_block": round(span / nblk * 1000, 1) if nblk else None,
        "native_blk_s": round(nblk / span, 3) if span else None,
        "empty_blk_s": round(max(nall - nblk, 0.0) / span, 3) if span else None,
        "committed_blk_s": round(ncommit / span, 3) if span else None,
        "exec_thread_busy_fraction": round(busy / span, 3) if span else None,
        "commit_interval_ms_avg": round(cis / cic * 1000, 1) if cic else None,
        "commit_interval_ms_p50": pctl("torus_commit_interval_seconds_bucket", 0.50),
        "commit_interval_ms_p95": pctl("torus_commit_interval_seconds_bucket", 0.95),
    }


CADENCE_KEYS = ["committed_blocks", "executed_native_blocks", "wall_ms_per_committed_block",
                "wall_ms_per_native_block", "native_blk_s", "empty_blk_s", "exec_thread_busy_fraction",
                "commit_interval_ms_avg", "commit_interval_ms_p50", "commit_interval_ms_p95"]


BROWS = read_buckets(os.path.join(OUT, "buckets.csv"))
BUCKETS = bucket_deltas(BROWS, t0, td)          # exec-phase percentiles (bench+drain)
BUCKETS_LOAD = bucket_deltas(BROWS, t0, t1)     # cadence under load
BUCKETS_DRAIN = bucket_deltas(BROWS, t1, td)    # cadence while draining
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
    # bl3 worker-aware accounting. `block_ms` (and every _sum below) is the
    # EXEC thread's timer; a phase the flush WORKER observes is wall time on
    # ANOTHER thread. Sum it into the per-block table and the accounting stops
    # closing: on bl2 on-10m-r2 (TORUS_EXEC_PIPELINE=1) flush was 252 ms of W
    # time against a 618 ms exec block, so residual_untimed read -212 ms and the
    # phase percentages summed to 134 %. Off-chain phases keep their ms (that is
    # what W cost) and their share of WALL time, but have no share of the exec
    # block and are excluded from the per-block sum.
    worker_present = (
        m(b, "flush_worker_seconds_count") - m(a, "flush_worker_seconds_count")
    ) > 0
    off_chain_phases = ["flush"] if worker_present else []
    # Cadence is reported per WINDOW (see cadence()). The top-level cadence
    # fields are the LOAD window [t0, t1]; `drain` and `incl_drain` sit beside
    # them. The exec-phase ms table below (block_ms, phases, chain ruler) stays
    # on [t0, t_drain] so every loaded block is included.
    cad_load = cadence(rs, t0, t1, BUCKETS_LOAD, node)
    cad_drain = cadence(rs, t1, td, BUCKETS_DRAIN, node)
    cad_all = cadence(rs, t0, td, BUCKETS, node)
    p = {"cadence_window": "load [t_bench0, t_bench1]",
         "block_ms": round(tot, 2),
         "phase_window": "bench+drain [t_bench0, t_drain]",
         "phase_window_s": span,
         "phase_window_native_blocks": nblk,
         "phase_window_all_exec_blocks": nall,
         "phase_window_committed_blocks": ncommit,
         "note": "cadence/commit fields (committed_blocks, native_blk_s, empty_blk_s, wall_ms_per_*, "
                 "commit_interval_ms_*, exec_thread_busy_fraction) are the LOAD window; `drain` / "
                 "`incl_drain` hold the same fields over [t_bench1, t_drain] / [t_bench0, t_drain]. "
                 "Phase ms are per NATIVE block over the bench+drain window (engine_count delta); "
                 "block_ms includes the tiny cost of empty blocks in that window"}
    for k in CADENCE_KEYS:
        p[k] = cad_load[k] if cad_load else None
    p["span_s"] = cad_load["span_s"] if cad_load else None
    p["all_exec_blocks"] = cad_load["all_exec_blocks"] if cad_load else None
    p["drain"] = cad_drain
    p["incl_drain"] = cad_all
    ph = {}
    acc = 0.0
    for k in PHASES:
        v = per_blk(k)
        off = k in off_chain_phases
        if not off:
            acc += v
        ph[k] = {"ms": round(v, 2),
                 "off_chain": off,
                 "pct_of_block": None if off else (round(100 * v / tot, 1) if tot else None),
                 "pct_of_wall": round(100 * dsum(k) / span, 1) if span else None}
        for s_ in SUB.get(k, []):
            ph[k][s_ + "_ms"] = round(per_blk(s_), 2)
    # r7: batch size handed to RocksDB per native block, and the db-write
    # throughput it implies (None on a pre-r7 binary).
    bbc = m(b, "exec_state_write_batch_bytes_count") - m(a, "exec_state_write_batch_bytes_count")
    bbs = m(b, "exec_state_write_batch_bytes_sum") - m(a, "exec_state_write_batch_bytes_sum")
    ph["flush"]["state_write_batch_kb"] = round(bbs / bbc / 1024, 1) if bbc else None
    dbs = dsum("state_write_db")
    ph["flush"]["state_write_db_mb_per_s"] = round(bbs / dbs / 1e6, 1) if dbs > 0 else None
    ph["residual_untimed"] = {"ms": round(tot - acc, 2),
                              "off_chain": False,
                              "pct_of_block": round(100 * (tot - acc) / tot, 1) if tot else None,
                              "note": "block_ms minus the phases timed ON THE EXEC THREAD"
                                      + (" (flush excluded: observed by the flush worker)" if worker_present else "")}
    p["off_chain_phases"] = off_chain_phases
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
                 for k in ["block"] + PHASES + ["phase_margin", "phase_match", "phase_settle", "root",
                                                "state_write"] + FLUSH_SUB_R7 + ENGINE_SUB_R6}
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

    # ------------------------------------------------ bl1 exec-chain ruler
    # The campaign's PRIMARY number is the exec CRITICAL CHAIN per NATIVE
    # block, not `block_ms` (which is diluted by empty blocks) and not
    # wall/native (which includes idle). This section reports it, splits it
    # from whatever a flush worker took off the exec thread, and gives the
    # denominator (fills/native block) without which any chain number is
    # unreadable — engine ms scales with fills, so a thinner block reads as a
    # chain win.
    #
    #   chain_ms         exec-thread wall per NATIVE block (E)
    #   pipelined_ms     flush-worker wall per native block (W); 0.0 when the
    #                    binary has no worker, None on a pre-bl1 binary
    #   handoff_wait_ms  E blocked handing a job to W; 0.0 on a serial binary
    #
    # Everything is None (never 0.0) when the series is ABSENT: a 0 ms chain
    # would read as a spectacular — and false — win, and None is also the
    # "your node binary is stale" tell.
    def draw(name):
        return m(b, name + "_sum") - m(a, name + "_sum")

    def craw(name):
        return m(b, name + "_count") - m(a, name + "_count")

    def pctl(metric, q):
        v = hist_quantile(BUCKETS.get((node, metric)), q)
        return round(v * 1000.0, 1) if v is not None else None

    nchain = craw("exec_chain_seconds")
    nworker = craw("flush_worker_seconds")
    bl1 = nchain > 0
    chain_ms = round(draw("exec_chain_seconds") / nblk * 1000.0, 2) if bl1 else None
    p["chain_ms"] = chain_ms
    p["pipelined_ms"] = round(draw("flush_worker_seconds") / nblk * 1000.0, 2) if nworker > 0 else (0.0 if bl1 else None)
    p["handoff_wait_ms"] = round(draw("exec_handoff_wait_seconds") / nblk * 1000.0, 2) if bl1 else None
    assert bool(nworker > 0) == worker_present, "worker_present must be single-sourced"
    p["worker_present"] = worker_present
    p["flush_worker_depth_max"] = max(m(r, "flush_worker_depth") for r in sel)
    # E time spent on NON-native (empty) blocks, expressed per native block:
    # the whole difference between block_ms and chain_ms by construction.
    p["empty_block_ms"] = round(tot - chain_ms, 2) if bl1 else None
    p["gap_to_100ms"] = round(chain_ms - 100.0, 2) if bl1 else None
    # Independent witness for nblk (which comes from exec_engine_seconds_count):
    # if the two ever disagree, one observe site is on the wrong predicate and
    # every ms-per-native-block column is skewed.
    p["native_blocks_counter"] = (
        (m(b, "exec_native_blocks_total") - m(a, "exec_native_blocks_total"))
        if bl1
        else None
    )
    # MANDATORY next to any chain number (see above).
    fills = m(b, "orders_matched_total") - m(a, "orders_matched_total")
    p["fills_per_native_block"] = round(fills / nblk, 1)
    p["engine_ms_per_1k_fills"] = round(ph["engine"]["ms"] / fills * nblk * 1000.0, 2) if fills > 0 else None
    p["chain_ms_p50"] = pctl("torus_exec_chain_seconds_bucket", 0.50)
    p["chain_ms_p95"] = pctl("torus_exec_chain_seconds_bucket", 0.95)
    p["handoff_wait_ms_p95"] = pctl("torus_exec_handoff_wait_seconds_bucket", 0.95)
    p["pipelined_ms_p95"] = pctl("torus_flush_worker_seconds_bucket", 0.95)
    # The ruler's own per-node-cell acceptance gate:
    #   * the chain must COVER every phase still running on the exec thread
    #     (on a serial binary that includes flush);
    #   * the chain can never exceed the block wall (the empty-block share is
    #     the whole difference).
    e_phases = ["verify", "replay_guard", "load_books", "engine", "save_books"]
    if not p["worker_present"]:
        e_phases.append("flush")
    e_sum = round(sum(ph[k]["ms"] for k in e_phases), 2)
    p["chain_identity"] = {
        "e_phases": e_phases,
        "off_chain_phases": off_chain_phases,
        "e_phase_sum_ms": e_sum,
        "chain_minus_e_phases_ms": round(chain_ms - e_sum, 2) if bl1 else None,
        "chain_covers_e_phases": (chain_ms + 0.5 >= e_sum) if bl1 else None,
        "chain_le_block": (chain_ms <= tot + 0.5) if bl1 else None,
        "block_minus_chain_ms": p["empty_block_ms"],
        "save_split_covers_save_books": (
            round(ph["save_books"]["save_books_drain_ms"] + ph["save_books"]["save_books_write_ms"], 2)
            <= round(ph["save_books"]["ms"], 2) + 0.5
        ),
    }
    # r4 commit-persist: consensus-thread commit-time durable persist (whole call /
    # body-record encode / WriteBatch write), ms per commit.
    cpc = m(b, "commit_persist_seconds_count") - m(a, "commit_persist_seconds_count")
    for name in ("commit_persist", "commit_body_encode", "commit_persist_write"):
        p[name + "_ms_avg"] = round((m(b, name + "_seconds_sum") - m(a, name + "_seconds_sum")) / cpc * 1000, 2) if cpc else None
    btc = m(b, "block_transactions_count_count") - m(a, "block_transactions_count_count")
    p["txs_per_block_avg"] = round((m(b, "block_transactions_count_sum") - m(a, "block_transactions_count_sum")) / btc, 1) if btc else None
    # r3 exec-write-stall-attribution: the two split write timers (ms per call) and
    # the DB-wide RocksDB picture over the window (rates from cumulative tickers;
    # gauges as window mean/max). All 0/None on a pre-r3 binary.
    def per_call(k):
        c = m(b, k + "_seconds_count") - m(a, k + "_seconds_count")
        return round((m(b, k + "_seconds_sum") - m(a, k + "_seconds_sum")) / c * 1000, 2) if c else None
    p["exec_body_persist_put_ms_per_call"] = per_call("exec_body_persist_write")
    p["commit_persist_ms_per_call"] = per_call("commit_persist")
    def rk(k):
        return (m(b, "rocksdb_" + k) - m(a, "rocksdb_" + k)) / span if span else 0.0
    def gstat(k, scale=1.0):
        vals = [m(r, k) * scale for r in sel]
        return {"mean": round(sum(vals) / len(vals), 1), "max": round(max(vals), 1)} if vals else None
    dbw = m(b, "rocksdb_db_write_count") - m(a, "rocksdb_db_write_count")
    ws = m(b, "rocksdb_write_stall_count") - m(a, "rocksdb_write_stall_count")
    p["rocksdb"] = {
        "stall_ms_per_s": round(rk("stall_micros") / 1000.0, 2),
        "stall_ms_per_native_block": round((m(b, "rocksdb_stall_micros") - m(a, "rocksdb_stall_micros")) / 1000.0 / nblk, 2),
        "writes_per_s_self": round(rk("write_self"), 1),
        "writes_per_s_other": round(rk("write_other"), 1),
        "wal_mb_per_s": round(rk("wal_bytes") / 1e6, 2),
        "bytes_written_mb_per_s": round(rk("bytes_written") / 1e6, 2),
        "flush_write_mb_per_s": round(rk("flush_write_bytes") / 1e6, 2),
        "compact_read_mb_per_s": round(rk("compact_read_bytes") / 1e6, 2),
        "compact_write_mb_per_s": round(rk("compact_write_bytes") / 1e6, 2),
        "compaction_cpu_cores": round(rk("compaction_cpu_micros") / 1e6, 3),
        "db_write_ms_avg": round((m(b, "rocksdb_db_write_sum_micros") - m(a, "rocksdb_db_write_sum_micros")) / dbw / 1000.0, 3) if dbw else None,
        "write_stall_ms_avg": round((m(b, "rocksdb_write_stall_sum_micros") - m(a, "rocksdb_write_stall_sum_micros")) / ws / 1000.0, 3) if ws else None,
        "db_write_p99_ms_last": round(m(b, "rocksdb_db_write_p99_micros") / 1000.0, 3),
        "memtable_mb": gstat("rocksdb_memtable_bytes_all", 1e-6),
        "immutable_memtables": gstat("rocksdb_immutable_memtables_all"),
        "l0_files_max": gstat("rocksdb_l0_files_max"),
        "pending_compaction_mb": gstat("rocksdb_pending_compaction_bytes_all", 1e-6),
        "delayed_write_rate": gstat("rocksdb_delayed_write_rate"),
        "write_stopped": gstat("rocksdb_write_stopped"),
        "running_compactions": gstat("rocksdb_running_compactions"),
        "trade_writer_queued_batches": gstat("trade_writer_queued_batches"),
    }
    phase[node] = p

# ---------------------------------------------------------------- consensus (metrics-before/after)
# Per-view consensus-thread means from the WHOLE-RUN metrics-before/after
# snapshots (they bracket idle + load + drain; the sampler never carried the
# torus_view_* series, so no per-window slicing is possible here).
VIEW_HISTS = ["torus_view_duration_seconds", "torus_view_propose_delay_seconds",
              "torus_view_propose_build_seconds", "torus_view_propose_finalize_seconds",
              "torus_view_qc_collect_seconds", "torus_view_proposal_arrival_seconds",
              "torus_view_insert_persist_seconds", "torus_view_vote_delay_seconds",
              "torus_commit_persist_seconds", "torus_block_build_seconds",
              # Producer stages: same proposal counts; parent decode is outside total.
              "torus_block_build_parent_decode_seconds",
              "torus_block_build_selection_seconds",
              "torus_block_build_mirror_seconds",
              "torus_block_build_attestation_seconds",
              "torus_block_build_assemble_seconds",
              "torus_block_build_encode_seconds",
              "torus_block_build_bookkeeping_seconds",
              "torus_block_build_epoch_seconds",
              # added to the node concurrently with this summarizer: None when absent
              "torus_validate_block_seconds", "torus_validate_block_decode_seconds",
              "torus_validate_block_da_reconstruct_seconds", "torus_validate_block_attest_seconds",
              "torus_validate_block_custody_seconds", "torus_on_committed_block_seconds",
              "torus_mempool_remove_committed_seconds"]


def read_metrics(path):
    """Unlabelled `name value` lines of a Prometheus text scrape -> {name: float}."""
    out = {}
    try:
        with open(path) as f:
            for line in f:
                if not line or line[0] == "#":
                    continue
                parts = line.split()
                if len(parts) != 2 or "{" in parts[0]:
                    continue
                try:
                    out[parts[0]] = float(parts[1])
                except ValueError:
                    pass
    except OSError:
        return {}
    return out


consensus = {}
for node in ("val0", "val1", "val2"):
    before = read_metrics(os.path.join(OUT, "metrics-before-%s.txt" % node))
    after = read_metrics(os.path.join(OUT, "metrics-after-%s.txt" % node))
    if not before or not after:
        continue

    def delta(k):
        return after.get(k, 0.0) - before.get(k, 0.0)

    def mean_ms(h):
        if h + "_count" not in after:
            return None
        c = delta(h + "_count")
        return round(delta(h + "_sum") / c * 1000, 2) if c > 0 else None

    c = {"window": "whole run (metrics-before -> metrics-after: idle + bench + drain)"}
    for h in VIEW_HISTS:
        short = h[len("torus_"):-len("_seconds")]
        c[short + "_ms"] = mean_ms(h)
        c[short + "_count"] = delta(h + "_count") if h + "_count" in after else None
    views = delta("torus_consensus_view")
    committed = delta("torus_blocks_committed_total")
    c["views"] = views
    c["committed_blocks"] = committed
    c["views_per_committed_block"] = round(views / committed, 3) if committed else None
    # Consensus-thread busy estimate per view with 3 EQUAL validators: a node
    # proposes 1 view in 3 (propose_delay) and inserts the other 2 in 3
    # (insert_persist).
    pd_, ip_ = c.get("view_propose_delay_ms"), c.get("view_insert_persist_ms")
    est = (pd_ or 0.0) / 3 + (ip_ or 0.0) * 2 / 3 if (pd_ is not None or ip_ is not None) else None
    c["consensus_thread_ms_per_view_est"] = round(est, 2) if est is not None else None
    c["consensus_thread_ms_per_committed_block_est"] = (
        round(est * views / committed, 2) if est is not None and committed else None)
    consensus[node] = c

# ---------------------------------------------------------------- consensus, LOAD window (s58)
# The block above reads metrics-before/after, which bracket idle + load + drain.
# Idle views are fast and numerous -- the ramp alone commits ~100 near-empty
# blocks -- so every per-view stage mean is pulled toward the idle value and an
# A/B reads as a no-op: across the s55 fast/slow pair view_duration differed
# 16 % while matched/s differed 2.54x. When the sampler carries the torus_view_*
# series (added to WIDE_COLS in s58) the same means can be sliced to [t0, t1].
# Cells recorded before that report None -- never 0.0, which would read as the
# arrival wait having been eliminated rather than never measured.
def consensus_window(rs, lo, hi):
    sel = [r for r in rs if lo <= r["ts"] <= hi]
    if len(sel) < 2:
        return None
    a, b = sel[0], sel[-1]
    if "torus_view_duration_seconds_count" not in a:
        return None
    out = {"window": [lo, hi], "span_s": b["ts"] - a["ts"]}
    for h in VIEW_HISTS:
        short = h[len("torus_"):-len("_seconds")]
        ck = short + "_seconds_count"
        if ("torus_" + ck) not in a:
            out[short + "_ms"] = None
            out[short + "_count"] = None
            continue
        c = m(b, ck) - m(a, ck)
        tot = m(b, short + "_seconds_sum") - m(a, short + "_seconds_sum")
        out[short + "_ms"] = round(tot / c * 1000, 2) if c > 0 else None
        out[short + "_count"] = c
    views = m(b, "consensus_view") - m(a, "consensus_view")
    committed = m(b, "blocks_committed_total") - m(a, "blocks_committed_total")
    out["views"] = views
    out["committed_blocks"] = committed
    out["views_per_committed_block"] = round(views / committed, 3) if committed else None
    return out


for _node, _rs in rows.items():
    if not _rs:
        continue
    _entry = consensus.setdefault(
        _node, {"window": "sampler only (no metrics-before/after scrape)"})
    _entry["load"] = consensus_window(_rs, t0, t1)

# ---------------------------------------------------------------- schedstat (run-cell.sh B.2)
# $OUT/schedstat.json: {"val0": {"hotstuff-algo": {"tid": N, "before": [on_cpu_ns,
# runqueue_wait_ns, timeslices], "bench_end": [...], "after": [...]}, ...}, ...}
# (thread missing => null). The LOAD window here is before -> bench_end, i.e.
# ~3 s wider than [t0, t1] on the front; per-committed-block uses the load
# cadence's committed_blocks.
sched = {}
try:
    with open(os.path.join(OUT, "schedstat.json")) as f:
        SCHED_RAW = json.load(f)
except (OSError, ValueError):
    SCHED_RAW = {}
for node, threads in (SCHED_RAW or {}).items():
    if not isinstance(threads, dict):
        continue
    ncommit = (phase.get(node) or {}).get("committed_blocks")
    d = {"window": "load (schedstat before -> bench_end snapshots)",
         "committed_blocks": ncommit, "threads": {}}
    for tname, t in threads.items():
        if not t or not t.get("before") or not t.get("bench_end"):
            d["threads"][tname] = None
            continue
        b0, b1 = t["before"], t["bench_end"]
        on_cpu = (b1[0] - b0[0]) / 1e6
        rq = (b1[1] - b0[1]) / 1e6
        e = {"tid": t.get("tid"),
             "on_cpu_ms": round(on_cpu, 1),
             "runqueue_wait_ms": round(rq, 1),
             "timeslices": b1[2] - b0[2],
             "on_cpu_ms_per_committed_block": round(on_cpu / ncommit, 3) if ncommit else None,
             "runqueue_wait_ms_per_committed_block": round(rq / ncommit, 3) if ncommit else None}
        if t.get("after"):
            e["on_cpu_ms_whole_run"] = round((t["after"][0] - b0[0]) / 1e6, 1)
            e["runqueue_wait_ms_whole_run"] = round((t["after"][1] - b0[1]) / 1e6, 1)
        d["threads"][tname] = e
    sched[node] = d

# ---------------------------------------------------------------- agreement
def _nums(raw, cast=float):
    out = []
    for tok in (raw or "").split():
        try:
            out.append(cast(tok))
        except ValueError:
            pass
    return out


# Digest provenance from run-cell.sh. `digest_quiescent` = the funnel counters
# were UNCHANGED on all 3 nodes across the whole (concurrent) digest window, so
# the three digests describe the same state. Absent (older cells) = assume
# quiescent, which keeps their verdicts exactly as they were.
digest_quiescent = A.digest_quiescent != "0"


def crash_record(path):
    """The raw crash.json (crash-kill.sh's record + run-cell.sh's log scan), or
    None when this cell never ran the kill -9 gate. Read BEFORE the agreement
    verdict because who was killed changes how the funnel counters are read."""
    try:
        with open(path) as f:
            raw = json.load(f)
    except (OSError, ValueError):
        return None
    return raw if raw.get("enabled") else None


CRASH_RAW = crash_record(os.path.join(OUT, "crash.json"))
# Prometheus counters are PROCESS lifetime: the SIGKILLed node restarts them at
# 0, so on a crash cell it can never match the two survivors no matter how
# perfectly it reconverged. Exclude it from the counter comparison; its state is
# judged on block hash + header root + state digest instead (crash_gate below).
KILLED_NODE = None
if CRASH_RAW is not None:
    KILLED_NODE = CRASH_RAW.get("kill_node")
    if not KILLED_NODE and CRASH_RAW.get("kill_idx") is not None:
        KILLED_NODE = "val%d" % int(CRASH_RAW["kill_idx"])

# A digest sampled a block or two off its peers moves the ACTION counter alone —
# metrics-after is one scrape, the three digests are concurrent. Wider than this
# is not a scrape skew, it is two chains.
MAX_DIGEST_HEIGHT_SKEW = 2
COUNTER_KEYS = ("matched", "placed", "resting", "actions")
SETTLED_COUNTER_KEYS = ("matched", "placed", "resting")

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
    agreement["counters_equal_all_nodes"] = all(len({r[k] for r in agree_rows}) == 1 for k in COUNTER_KEYS)
    scored_rows = [r for r in agree_rows if r["node"] != KILLED_NODE]
    agreement["counters_excluded_node"] = KILLED_NODE if len(scored_rows) < len(agree_rows) else None
    agreement["counters_compared_nodes"] = [r["node"] for r in scored_rows]
    agreement["counters_equal"] = all(len({r[k] for r in scored_rows}) == 1 for k in COUNTER_KEYS)
    agreement["panic_or_failstop_lines"] = sum(r["panic_or_failstop_lines"] for r in agree_rows)
    agreement["error_lines"] = sum(r["error_lines"] for r in agree_rows)
    # bl1 resident-books-untouched-advance: full O(resting depth) reloads of
    # the rank8 holder per validator (torus_exec_resident_rebuilds). Exactly 1
    # per process = the cold start; more = a mid-run "resident books stale"
    # stall landed in the cell. Absent on pre-candidate cells -> None.
    agreement["resident_rebuilds_per_node"] = [r.get("resident_rebuilds") for r in agree_rows]
    agreement["state_digest_quiescent"] = digest_quiescent
    agreement["state_digest_seconds_per_node"] = _nums(A.digest_secs)
    agreement["state_digest_heights"] = _nums(A.digest_heights, int)
    dh = agreement["state_digest_heights"]
    agreement["digest_height_spread"] = (max(dh) - min(dh)) if len(dh) == len(agree_rows) else None
    # The digests were NOT taken at one pinned instant of one pinned height.
    digest_unpinned = bool(not digest_quiescent or A.drained != "1"
                           or (agreement["digest_height_spread"] or 0) > 0)
    # ...and the only counter apart is the action counter, by no more than the
    # scrape skew that explains it. Every settled-state counter still agrees.
    agreement["action_counter_skew_only"] = bool(
        not agreement["counters_equal"]
        and all(len({r[k] for r in scored_rows}) == 1 for k in SETTLED_COUNTER_KEYS)
        and digest_unpinned
        and (agreement["digest_height_spread"] or 0) <= MAX_DIGEST_HEIGHT_SKEW)
    # Consensus evidence that does NOT depend on when the digest was taken.
    consensus_ok = bool(agreement["height_spread"] <= 5 and agreement["block_hash_equal"]
                        and agreement["counters_equal"]
                        and agreement["panic_or_failstop_lines"] == 0)
    if consensus_ok and agreement["state_digest_equal"]:
        verdict = "AGREE"
    elif (agreement["action_counter_skew_only"] and agreement["height_spread"] <= 5
          and agreement["block_hash_equal"] and agreement["header_state_root_equal"]
          and agreement["state_digest_equal"]
          and agreement["panic_or_failstop_lines"] == 0):
        # Same hash, same header root, same digest on all three; only
        # torus_native_actions_processed_total apart, and the digests were not
        # taken at one pinned height. A sampling artifact, not a fork — and not
        # proof of agreement either.
        verdict = "DIGEST_UNVERIFIED"
    elif consensus_ok and not (digest_quiescent and A.drained == "1"):
        # r6-base-300m-r1 shape: equal block hash + equal counters, but the
        # digests were sampled while the chain still moved (drain timed out, or
        # the counters moved during the digest window). That is a HARNESS
        # artifact, not a fork — and it is equally not proof of agreement.
        verdict = "DIGEST_UNVERIFIED"
    else:
        verdict = "DISAGREE"
    agreement["agreement_verdict"] = verdict
    # Tri-state: True only with an EQUAL digest, False only for real evidence of
    # divergence, None when the digest could not be taken at a pinned state.
    agreement["validators_agree"] = {"AGREE": True, "DIGEST_UNVERIFIED": None}.get(verdict, False)
else:
    agreement["agreement_verdict"] = "INCOMPLETE"
    agreement["validators_agree"] = False

# ---------------------------------------------------------------- crash gate
# bl3: a CRASH_KILL_AT_S cell SIGKILLs one devnet validator mid-load and restarts
# it from the same data dir (tools/matched-bench/crash-kill.sh). run-cell.sh drops
# the record + the post-run log scan in crash.json; this turns it into a verdict.
#
# What the gate proves for TORUS_EXEC_PIPELINE: the applied-height marker is the
# crash fence, so the restarted node must replay from it, converge to the SAME
# state as the two survivors, and rewind no further than the work that was
# already committed-but-unexecuted at the kill.
#
#   rewind_blocks             committed - applied at the crash (the replay gap)
#   exec_queue_depth_at_kill  committed-but-unexecuted blocks already queued on E
#   rewind_beyond_exec_queue  what the PIPELINE cost on top: depth-1 hand-off
#                             plus the in-flight batch => <= 2
#
# Absent crash.json => `crash` is null and the headline gate is null (NOT "FAIL"):
# an ordinary cell simply did not run the gate.
MAX_REWIND_BEYOND_EXEC_QUEUE = 2


def crash_gate(raw, agreement, node_env):
    if raw is None:
        return None
    r = raw.get("restart") or {}
    pre = raw.get("pre_kill") or {}
    out = dict(raw)
    gap = int(r.get("gap") or 0)
    q = pre.get("exec_queue_depth")
    q = int(q) if q is not None else None
    out["rewind_blocks"] = gap
    out["exec_queue_depth_at_kill"] = q
    out["rewind_beyond_exec_queue"] = (gap - q) if q is not None else None
    out["max_rewind_beyond_exec_queue"] = MAX_REWIND_BEYOND_EXEC_QUEUE
    # The gate exists to unblock the FLAG: if the restarted node came back
    # without the flush worker, it crash-tested the serial path and proves
    # nothing. (A flag-off crash cell is a legitimate serial control.)
    flag_expected = node_env.get("TORUS_EXEC_PIPELINE") == "1"
    out["pipeline_flag_expected"] = flag_expected
    out["pipeline_flag_confirmed"] = bool(r.get("pipeline_enabled_line")) if flag_expected else None

    fail = []
    if not raw.get("restarted_pid"):
        fail.append("killed node did not restart")
    if out["rewind_beyond_exec_queue"] is None:
        fail.append("no exec_queue_depth sampled at the kill — rewind unbounded")
    elif out["rewind_beyond_exec_queue"] > MAX_REWIND_BEYOND_EXEC_QUEUE:
        fail.append("rewind %d blocks beyond the exec queue (max %d)"
                    % (out["rewind_beyond_exec_queue"], MAX_REWIND_BEYOND_EXEC_QUEUE))
    if int(r.get("panic_or_failstop_lines") or 0) > 0:
        fail.append("panic/fail-stop after the restart")
    if int(r.get("hole_lines") or 0) > 0:
        fail.append("unhealed execution hole after the restart")
    # Fork evidence is MANDATORY for all THREE nodes, the killed one included:
    # only its process-lifetime COUNTERS are excused (they reset on restart),
    # never its state. Checked here by name rather than via the one-word verdict
    # so the gate keeps its own teeth if the verdict rules ever loosen.
    out["counters_excluded_node"] = agreement.get("counters_excluded_node")
    out["counters_compared_nodes"] = agreement.get("counters_compared_nodes")
    for key, what in (("block_hash_equal", "block hash"),
                      ("header_state_root_equal", "header state root"),
                      ("state_digest_equal", "state digest")):
        if not agreement.get(key):
            fail.append("%s differs across the 3 nodes (fork)" % what)
    if not agreement.get("state_digest_quiescent"):
        fail.append("state digest was not taken at a quiescent chain")
    if int(agreement.get("panic_or_failstop_lines") or 0) > 0:
        fail.append("panic/fail-stop somewhere in the 3-node run")
    # The survivors must still agree with each other on the funnel; only the
    # killed node is exempt.
    if not agreement.get("counters_equal"):
        fail.append("funnel counters differ among %s"
                    % (agreement.get("counters_compared_nodes") or "the nodes"))
    if agreement.get("agreement_verdict") == "INCOMPLETE":
        fail.append("3-node agreement INCOMPLETE")
    if flag_expected and not r.get("pipeline_enabled_line"):
        fail.append("flush worker was NOT attached after the restart")
    out["fail_reasons"] = fail
    out["verdict"] = "FAIL" if fail else "PASS"
    return out


crash = crash_gate(CRASH_RAW, agreement, NODE_ENV)

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
    # r4: full-body (direct) pushes vs manifest pushes = which dissemination path
    # the proposals took; body_push_max_bytes vs the 8 MB direct-push floor.
    dissem["total_body_pushes"] = sum((d.get("body_push") or 0) for n, d in dissem.items() if n.startswith("val"))
    dissem["body_push_max_bytes"] = max([(d.get("body_push_max_bytes") or 0) for n, d in dissem.items() if n.startswith("val")] or [0])
    dissem["total_pacing_lines"] = sum((d.get("pacing") or 0) for n, d in dissem.items() if n.startswith("val"))
    evidence_complete = all(
        isinstance(dissem.get(n, {}).get(k), int) and dissem[n][k] >= 0
        for n in ('val0', 'val1', 'val2') for k in fail_keys)
    dissem["dissemination_clean"] = (False if dissem["total_failures"] > 0 else
                                     True if evidence_complete else None)

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
liveness = assess_liveness(rows, t0, td, DEFAULT_STALL_S)
# Keep the runner's historical observation for audit, but a known stall cannot
# count as a successfully drained performance cell. Agreement stays independent.
drained = A.drained == "1" and liveness['verdict'] != 'FAIL'
validity = acceptance(liveness, drained, int(A.bench_rc or -1),
                      agreement.get('agreement_verdict'),
                      dissem.get('dissemination_clean'), crash)
summary = {
    "status": "OK" if validity['accepted'] else "INVALID" if validity['verdict'] == 'REJECT' else "UNVERIFIED",
    "label": A.label, "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
    "worktree": A.worktree, "commit": A.commit, "dirty_files": int(A.dirty or 0),
    "binaries": {"torus_node_md5": A.md5_node, "bench_throughput_md5": A.md5_bench},
    "genesis": {"md5": A.genesis_md5, "markets": int(A.genesis_markets or 0), "native_balances": int(A.genesis_accounts or 0)},
    "cell": {"markets": int(A.markets), "duration_s": int(A.dur), "rate_total": int(A.rate), "senders": int(A.senders),
             "block_cap": int(A.block_cap) if A.block_cap else None,
             "markets_per_sender": int(A.markets_per_sender) if A.markets_per_sender else None,
             "extra_env": A.extra_env, "node_env": NODE_ENV,
             "env_digests_per_node": A.env_digests.split(), "bench_cmd": A.bench_cmd, "node_pids": A.pids.split()},
    "timing": {"t_bench0": t0, "t_bench1": t1, "t_drain": td, "bench_wall_s": t1 - t0, "drain_s": td - t1,
               "drained": drained, "drained_reported": A.drained == "1", "drain_timeout_s": int(A.drain_timeout) if A.drain_timeout else None,
               "bench_rc": int(A.bench_rc or -1)},
    "idle_blk_s": float(A.idle_blks or 0),
    "headline": {
        "matched_s_avg": v0.get("matched_s_benchwin"),
        "matched_s_first120": v0.get("matched_s_first120"),
        "matched_s_best60": v0.get("matched_s_best60"),
        "matched_s_incl_drain": v0.get("matched_s_incl_drain"),
        "placed_s_avg": v0.get("placed_s_benchwin"),
        "blk_s_avg": v0.get("blk_s_benchwin"),
        "blk_s_worst60": v0.get("blk_s_worst60"),
        "peak_exec_queue_depth": v0.get("peak_exec_queue_depth"),
        "txs_per_block_avg": phase.get("val0", {}).get("txs_per_block_avg"),
        # bl1 exec-chain ruler headline: the campaign's PRIMARY number and the
        # denominator it must always be read next to.
        "chain_ms": phase.get("val0", {}).get("chain_ms"),
        "gap_to_100ms": phase.get("val0", {}).get("gap_to_100ms"),
        "pipelined_ms": phase.get("val0", {}).get("pipelined_ms"),
        "handoff_wait_ms": phase.get("val0", {}).get("handoff_wait_ms"),
        "fills_per_native_block": phase.get("val0", {}).get("fills_per_native_block"),
        "engine_ms_per_1k_fills": phase.get("val0", {}).get("engine_ms_per_1k_fills"),
        # cadence fields are the LOAD window [t_bench0, t_bench1] (bl-sweep);
        # earlier summaries blended in the empty-block drain (see
        # phase_by_node.<node>.incl_drain for the old values).
        "cadence_window": "load",
        "wall_ms_per_committed_block": phase.get("val0", {}).get("wall_ms_per_committed_block"),
        "native_blk_s": phase.get("val0", {}).get("native_blk_s"),
        "empty_blk_s": phase.get("val0", {}).get("empty_blk_s"),
        "commit_interval_ms_avg": phase.get("val0", {}).get("commit_interval_ms_avg"),
        "commit_interval_ms_p50": phase.get("val0", {}).get("commit_interval_ms_p50"),
        "commit_interval_ms_p95": phase.get("val0", {}).get("commit_interval_ms_p95"),
        "consensus_thread_ms_per_committed_block_est": consensus.get("val0", {}).get("consensus_thread_ms_per_committed_block_est"),
        "actions_per_exec_block": phase.get("val0", {}).get("actions_per_exec_block"),
        "consensus_timeouts": v0.get("delta_consensus_timeout_total_total"),
        "dissemination_clean": dissem.get("dissemination_clean") if dissem else None,
        "validators_agree": agreement.get("validators_agree"),
        "agreement_verdict": agreement.get("agreement_verdict"),
        "liveness_verdict": liveness['verdict'],
        "benchmark_accepted": validity['accepted'],
        "exec_resident_rebuilds": agreement.get("resident_rebuilds_per_node"),
        # bl3 crash gate: None on a cell that did not run it.
        "crash_gate": crash.get("verdict") if crash else None,
    },
    "ingest": {"bench_submitted_actions": int(A.bench_submitted or 0),
               "val0_actions_processed": int(v0.get("delta_native_actions_processed_total", 0)),
               "mempool_nonce_expired_evictions_per_node": [int(x) for x in A.evicted.split()],
               "note": "NONCE_WINDOW_MS=60s: backlog older than 60 s is evicted silently; submitted-processed gap = expiry"},
    "funnel_by_node": funnel,
    "phase_by_node": phase,
    "consensus_by_node": consensus,
    "sched_by_node": sched,
    "agreement": agreement,
    "liveness": liveness,
    "validity": validity,
    "crash": crash,
    "dissemination": dissem,
    "cpu": cpu,
    "bench_log_tail": bench_tail,
}
with open(os.path.join(OUT, "summary.json"), "w") as f:
    json.dump(summary, f, indent=1)
h = summary["headline"]
print(f"SUMMARY {A.label}: matched/s avg={h['matched_s_avg']} first120={h['matched_s_first120']} "
      f"best60={h['matched_s_best60']} "
      f"placed/s={h['placed_s_avg']} blk/s={h['blk_s_avg']} txs/blk={h['txs_per_block_avg']} "
      f"timeouts={h['consensus_timeouts']} dissem_clean={h['dissemination_clean']} "
      f"agree={h['agreement_verdict']} ({h['validators_agree']}) "
      f"resident_rebuilds={h['exec_resident_rebuilds']} "
      f"digest_s={agreement.get('state_digest_seconds_per_node')} "
      f"drained={summary['timing']['drained']} bench_rc={A.bench_rc}")
print(f"VALIDITY {A.label}: {validity['verdict']} liveness={liveness['verdict']} "
      f"reasons={validity['fail_reasons'] + validity['unverified_reasons']}")
if crash:
    print(f"CRASH val{crash.get('kill_idx')}: verdict={crash['verdict']} "
          f"kill_at={crash.get('kill_at_s')}s down={crash.get('down_s')}s "
          f"rewind={crash['rewind_blocks']} blk (exec_queue_at_kill="
          f"{crash['exec_queue_depth_at_kill']} beyond_queue="
          f"{crash['rewind_beyond_exec_queue']}/{crash['max_rewind_beyond_exec_queue']}) "
          f"worker_attached={crash.get('pipeline_flag_confirmed')} "
          f"agree={agreement.get('agreement_verdict')} "
          f"(counters over {crash.get('counters_compared_nodes')}, "
          f"val{crash.get('kill_idx')} judged on digest/hash/root) "
          f"reasons={crash['fail_reasons'] or 'none'}")
p0 = phase.get("val0", {})
if p0:
    print(f"PHASE val0: block_ms={p0['block_ms']} wall/committed(load)={p0['wall_ms_per_committed_block']} "
          f"(incl_drain={(p0['incl_drain'] or {}).get('wall_ms_per_committed_block')}) " +
          " ".join(f"{k}={v['ms']}("
                   + ("off-chain" if v["pct_of_block"] is None else f"{v['pct_of_block']}%")
                   + ")" for k, v in p0["phases"].items()))

    # bl1 exec-chain ruler: the critical chain, what came off it, and the
    # denominator. chain_ms=None means the node binary predates bl1.
    ci = p0["chain_identity"]
    sb = p0["phases"]["save_books"]
    print(f"CHAIN val0: chain_ms={p0['chain_ms']} (p50={p0['chain_ms_p50']} p95={p0['chain_ms_p95']}) "
          f"gap_to_100ms={p0['gap_to_100ms']} pipelined_ms={p0['pipelined_ms']} "
          f"handoff_wait_ms={p0['handoff_wait_ms']} worker={p0['worker_present']} "
          f"empty_block_ms={p0['empty_block_ms']} | fills/blk={p0['fills_per_native_block']} "
          f"engine_ms/1k_fills={p0['engine_ms_per_1k_fills']} | "
          f"LOAD native_blk/s={p0['native_blk_s']} empty_blk/s={p0['empty_blk_s']} "
          f"commit_ms avg/p50/p95={p0['commit_interval_ms_avg']}/{p0['commit_interval_ms_p50']}/{p0['commit_interval_ms_p95']} | "
          f"save_books={sb['ms']}(drain={sb['save_books_drain_ms']} write={sb['save_books_write_ms']}) | "
          f"identity covers_e={ci['chain_covers_e_phases']} le_block={ci['chain_le_block']} "
          f"chain-e_phases={ci['chain_minus_e_phases_ms']}")

    # r6 engine-untimed-attribution: engine internals on one line (all 0.0 on a
    # pre-r6 node binary, which is itself the "binary is stale" tell).
    e = p0["phases"]["engine"]
    print("ENGINE val0: total=" + str(e["ms"]) + " = phase1_actions=" + str(e["phase1_actions_ms"]) +
          " margin=" + str(e["phase_margin_ms"]) + " match=" + str(e["phase_match_ms"]) +
          " settle=" + str(e["phase_settle_ms"]) + "(passA=" + str(e["settle_pass_a_ms"]) +
          " passB=" + str(e["settle_pass_b_ms"]) + " cache_flush=" + str(e["cache_flush_ms"]) + ")" +
          " tail=" + str(e["post_engine_tail_ms"]) + " untimed=" + str(e["engine_untimed_ms"]))

class _CTol(dict):
    """Missing whole-run stage keys print as None (sampler-only nodes)."""
    def __missing__(self, k):
        return None


for node, c0 in consensus.items():
    sc = (sched.get(node) or {}).get("threads") or {}
    hs = sc.get("hotstuff-algo") or {}
    c = _CTol(c0)
    if "view_duration_ms" in c0:
        print(f"CONSENSUS {node} (whole run): view_ms={c['view_duration_ms']} views={c['views']} "
              f"views/committed={c['views_per_committed_block']} | propose delay={c['view_propose_delay_ms']} "
              f"build={c['view_propose_build_ms']} finalize={c['view_propose_finalize_ms']} "
              f"qc_collect={c['view_qc_collect_ms']} | arrival={c['view_proposal_arrival_ms']} "
              f"insert_persist={c['view_insert_persist_ms']} vote_delay={c['view_vote_delay_ms']} | "
              f"commit_persist={c['commit_persist_ms']} block_build={c['block_build_ms']} "
              f"validate={c['validate_block_ms']}(decode={c['validate_block_decode_ms']} "
              f"da={c['validate_block_da_reconstruct_ms']} attest={c['validate_block_attest_ms']} "
              f"custody={c['validate_block_custody_ms']}) on_committed={c['on_committed_block_ms']} "
              f"mempool_rm={c['mempool_remove_committed_ms']} | "
              f"thread_est/view={c['consensus_thread_ms_per_view_est']} "
              f"/committed={c['consensus_thread_ms_per_committed_block_est']}"
              + (f" | sched hotstuff-algo on_cpu/blk={hs.get('on_cpu_ms_per_committed_block')} "
                 f"rq_wait/blk={hs.get('runqueue_wait_ms_per_committed_block')}" if hs else ""))
    ld = c0.get("load")
    if ld:
        print(f"CONSENSUS {node} (LOAD window): view_ms={ld.get('view_duration_ms')} "
              f"views={ld.get('views')} views/committed={ld.get('views_per_committed_block')} | "
              f"arrival={ld.get('view_proposal_arrival_ms')} "
              f"propose delay={ld.get('view_propose_delay_ms')} "
              f"build={ld.get('view_propose_build_ms')} "
              f"qc_collect={ld.get('view_qc_collect_ms')} "
              f"insert_persist={ld.get('view_insert_persist_ms')}")
