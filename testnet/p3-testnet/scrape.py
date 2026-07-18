#!/usr/bin/env python3
"""Parse a Prometheus text dump into the counters/histograms the p3 throughput
leg needs. Emits JSON on stdout. VERBATIM copy of the devnet mission harness
scrape.py so testnet numbers fold identically to the devnet baseline.

prometheus_client suffixes counters with _total; histograms carry _sum/_count.
Labeled series are summed across labels except resting_depth which is kept
per-market (for determinism agreement)."""

import sys, json, re

# name (as registered) -> how to fold
COUNTERS = [
    "torus_session_owner_cache_hits",
    "torus_session_owner_cache_misses",
    "torus_session_owner_cache_evictions",
    "torus_verified_sender_cache_hits",
    "torus_verified_sender_cache_misses",
    "torus_exec_verify_skipped",
    "torus_orders_placed",
    "torus_orders_matched",
    "torus_orders_rejected",
    "torus_blocks_committed",
    "torus_native_actions_processed",
]
HIST_SUMS = [
    "torus_exec_verify_seconds",
    "torus_exec_block_seconds",
    "torus_exec_engine_seconds",
    "torus_exec_replay_guard_seconds",
    "torus_exec_phase_margin_seconds",
    "torus_exec_phase_match_seconds",
    "torus_exec_phase_settle_seconds",
    "torus_exec_flush_seconds",
    "torus_exec_load_books_seconds",
    "torus_exec_save_books_seconds",
]
GAUGES = ["torus_block_height", "torus_consensus_view", "torus_mempool_native_size"]


def parse(path):
    counters = {c: 0.0 for c in COUNTERS}
    hist = {h: {"sum": 0.0, "count": 0.0} for h in HIST_SUMS}
    gauges = {g: None for g in GAUGES}
    resting = {}  # market -> depth
    with open(path) as f:
        for line in f:
            if line.startswith("#"):
                continue
            line = line.strip()
            if not line:
                continue
            parts = line.rsplit(" ", 1)
            if len(parts) != 2:
                continue
            key, val = parts
            try:
                v = float(val)
            except ValueError:
                continue
            base = key.split("{", 1)[0]
            # counters (prometheus_client _total)
            cbase = base[:-6] if base.endswith("_total") else base
            if cbase in counters:
                counters[cbase] += v
                continue
            # histograms
            if base.endswith("_sum"):
                h = base[:-4]
                if h in hist:
                    hist[h]["sum"] += v
                continue
            if base.endswith("_count"):
                h = base[:-6]
                if h in hist:
                    hist[h]["count"] += v
                continue
            if base in gauges:
                gauges[base] = v
                continue
            if base == "torus_native_resting_depth":
                m = re.search(r'market="?([^",}]+)"?', key)
                mk = m.group(1) if m else "?"
                resting[mk] = resting.get(mk, 0.0) + v
    return {"counters": counters, "hist": hist, "gauges": gauges, "resting": resting}


if __name__ == "__main__":
    print(json.dumps(parse(sys.argv[1])))
