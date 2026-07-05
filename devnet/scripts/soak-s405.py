#!/usr/bin/env python3
"""S405 BS-3 soak sampler: correlate the accumulating leader propose cost with
RocksDB runtime state on a fresh devnet.

Samples validator-0 (:9091) every INTERVAL seconds and appends one CSV row of
window-delta averages (histograms) and gauge snapshots. The propose split
(build = produce+insert, finalize = update/commit+broadcast) counts only fresh
proposals, so view churn / re-proposals cannot pollute it.

Usage: python3 devnet/scripts/soak-s405.py [out.csv] [interval_s]
Stop with SIGTERM/Ctrl-C; partial CSV is valid.
"""

import re
import sys
import time
import urllib.request

URL = "http://127.0.0.1:9091/metrics"
OUT = sys.argv[1] if len(sys.argv) > 1 else "devnet/soak-s405.csv"
INTERVAL = int(sys.argv[2]) if len(sys.argv) > 2 else 60

HISTS = [
    "torus_view_propose_delay_seconds",
    "torus_view_propose_build_seconds",
    "torus_view_propose_finalize_seconds",
    "torus_view_insert_persist_seconds",
    "torus_view_duration_seconds",
]
GAUGES = [
    "torus_block_height",
    "torus_consensus_view",
    "torus_rocksdb_block_cache_bytes",
]
# Per-CF families: cf label -> column name suffix.
CF_FAMS = [
    (
        "torus_rocksdb_l0_files",
        ["cf_consensus_meta", "cf_trie_accounts", "cf_block_headers"],
    ),
    (
        "torus_rocksdb_memtable_bytes",
        ["cf_consensus_meta", "cf_trie_accounts", "cf_block_headers"],
    ),
    ("torus_rocksdb_pending_compaction_bytes", ["cf_consensus_meta"]),
]

LINE = re.compile(r"^(\S+?)(\{[^}]*\})?\s+([0-9.eE+-]+)$")


def scrape():
    vals = {}
    with urllib.request.urlopen(URL, timeout=5) as r:
        for raw in r.read().decode().splitlines():
            if raw.startswith("#"):
                continue
            m = LINE.match(raw.strip())
            if m:
                vals[m.group(1) + (m.group(2) or "")] = float(m.group(3))
    return vals


def hist_delta_ms(cur, prev, name):
    s = cur.get(f"{name}_sum", 0.0) - prev.get(f"{name}_sum", 0.0)
    c = cur.get(f"{name}_count", 0.0) - prev.get(f"{name}_count", 0.0)
    return round(1000.0 * s / c, 2) if c > 0 else ""


def cf_val(cur, fam, cf):
    for k, v in cur.items():
        if k.startswith(fam + "{") and f'cf="{cf}"' in k:
            return int(v)
    return ""


header = ["ts", "uptime_s"]
header += [h.replace("torus_view_", "").replace("_seconds", "") + "_ms" for h in HISTS]
header += [g.replace("torus_", "") for g in GAUGES]
for fam, cfs in CF_FAMS:
    for cf in cfs:
        header.append(fam.replace("torus_rocksdb_", "") + ":" + cf.replace("cf_", ""))

t0 = time.time()
prev = scrape()
with open(OUT, "a") as f:
    f.write(",".join(header) + "\n")
    while True:
        time.sleep(INTERVAL)
        try:
            cur = scrape()
        except Exception as e:
            print(f"scrape failed: {e}", file=sys.stderr)
            continue
        row = [time.strftime("%H:%M:%S"), str(int(time.time() - t0))]
        row += [str(hist_delta_ms(cur, prev, h)) for h in HISTS]
        row += [str(int(cur.get(g, 0))) for g in GAUGES]
        for fam, cfs in CF_FAMS:
            for cf in cfs:
                row.append(str(cf_val(cur, fam, cf)))
        f.write(",".join(row) + "\n")
        f.flush()
        prev = cur
