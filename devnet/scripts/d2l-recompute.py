#!/usr/bin/env python3
"""S458: recompute ab-d2l-s458.csv delta columns from saved per-trial metric dumps.

The first in-run parser missed prometheus_client's `_total` counter suffix, so
all counter deltas read 0. Raw before/after dumps are saved per trial — this
re-emits the corrected CSV. Usage:
    d2l-recompute.py <outroot> <old_csv>
Keeps the first 8 columns (trial..wedged) from the old CSV, recomputes the rest.
"""

import csv
import os
import re
import sys

COUNTERS = [
    "torus_native_gossip_published_actions",
    "torus_native_gossip_dropped_full",
    "torus_native_gossip_dropped_oversized",
    "torus_native_gossip_received_actions",
    "torus_rpc_submit_admit_forward_seconds_count",
    "torus_direct_send_failures_untracked",
    "torus_native_da_pull_requests",
    "torus_native_da_pull_recovered",
    "torus_native_da_pull_failures",
    "torus_native_da_recovery_handoffs",
    "torus_native_da_recovery_timeouts",
]


def read(path):
    vals = {}
    try:
        for line in open(path):
            if line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) == 2:
                name = parts[0].split("{")[0]
                if name.endswith("_total"):
                    name = name[: -len("_total")]
                try:
                    vals[name] = vals.get(name, 0.0) + float(parts[1])
                except ValueError:
                    pass
    except FileNotFoundError:
        pass
    return vals


def deltas(trial_dir):
    tot = {c: 0.0 for c in COUNTERS}
    v0_proc = blocks = mempool_after = 0.0
    for port in ["9091", "9092", "9093", "9094"]:
        b = read(os.path.join(trial_dir, f"metrics-{port}-before.txt"))
        a = read(os.path.join(trial_dir, f"metrics-{port}-after.txt"))
        for c in COUNTERS:
            tot[c] += a.get(c, 0.0) - b.get(c, 0.0)
        mempool_after += a.get("torus_mempool_native_size", 0.0)
        if port == "9091":
            v0_proc = a.get("torus_native_actions_processed", 0.0) - b.get(
                "torus_native_actions_processed", 0.0
            )
            blocks = a.get("torus_blocks_committed", 0.0) - b.get(
                "torus_blocks_committed", 0.0
            )
    return [str(int(tot[c])) for c in COUNTERS] + [
        str(int(v0_proc)),
        str(int(blocks)),
        str(int(mempool_after)),
    ]


def main():
    outroot, old_csv = sys.argv[1], sys.argv[2]
    rows = list(csv.reader(open(old_csv)))
    print(",".join(rows[0]))
    for row in rows[1:]:
        trial, gossip, phase = row[0], row[1], row[2]
        matches = [
            d
            for d in os.listdir(outroot)
            if re.fullmatch(rf"trial-{trial}-gossip{gossip}-{phase}", d)
        ]
        if not matches:
            print(",".join(row))  # keep as-is (e.g. never-started trials)
            continue
        print(",".join(row[:8] + deltas(os.path.join(outroot, matches[0]))))


if __name__ == "__main__":
    main()
