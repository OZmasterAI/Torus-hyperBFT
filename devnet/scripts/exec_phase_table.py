#!/usr/bin/env python3
"""Exec-ceiling probe (s351): decompose exec_block_seconds into phases.

Usage: exec_phase_table.py <baseline-metrics.txt> <post-metrics.txt>

Reads two OpenMetrics snapshots, prints per-phase delta table:
ms/native-block, share of exec_block_seconds, and the unattributed residual.
"""

import sys

PHASES = [
    "verify",
    "replay_guard",
    "engine",
    "phase_margin",
    "phase_match",
    "phase_settle",
    "save_books",
    "flush",
]


def parse(path):
    vals = {}
    with open(path) as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            parts = line.split()
            if len(parts) >= 2:
                try:
                    vals[parts[0]] = float(parts[1])
                except ValueError:
                    pass
    return vals


def delta(base, post, key):
    return post.get(key, 0.0) - base.get(key, 0.0)


def main():
    base, post = parse(sys.argv[1]), parse(sys.argv[2])
    blk_sum = delta(base, post, "torus_exec_block_seconds_sum")
    blk_cnt = delta(base, post, "torus_exec_block_seconds_count")
    rows, phase_total = [], 0.0
    for p in PHASES:
        s = delta(base, post, f"torus_exec_{p}_seconds_sum")
        c = delta(base, post, f"torus_exec_{p}_seconds_count")
        phase_total += s
        rows.append((p, s, c))

    print(f"{'phase':<14}{'Δsum s':>10}{'Δcount':>8}{'ms/blk':>10}{'share%':>8}")
    for p, s, c in rows:
        ms = (s / c * 1000) if c else 0.0
        share = (s / blk_sum * 100) if blk_sum else 0.0
        print(f"{p:<14}{s:>10.3f}{c:>8.0f}{ms:>10.2f}{share:>8.1f}")
    resid = blk_sum - phase_total
    print(
        f"{'residual':<14}{resid:>10.3f}{'':>8}{'':>10}"
        f"{(resid / blk_sum * 100) if blk_sum else 0:>8.1f}"
    )
    print(
        f"{'TOTAL block':<14}{blk_sum:>10.3f}{blk_cnt:>8.0f}"
        f"{(blk_sum / blk_cnt * 1000) if blk_cnt else 0:>10.2f}{'100.0':>8}"
    )

    acts = delta(base, post, "torus_native_actions_processed_total")
    commits = delta(base, post, "torus_blocks_committed_total")
    bb_sum = delta(base, post, "torus_block_build_seconds_sum")
    bb_cnt = delta(base, post, "torus_block_build_seconds_count")
    print(
        f"\nactions processed: {acts:.0f} | blocks committed: {commits:.0f}"
        f" | actions/native-blk: {(acts / blk_cnt) if blk_cnt else 0:.0f}"
    )
    print(
        f"block_build: {bb_cnt:.0f} obs, "
        f"{(bb_sum / bb_cnt * 1000) if bb_cnt else 0:.2f} ms/blk (leader-only)"
    )
    print(f"exec_queue_depth now: {post.get('torus_exec_queue_depth', 0):.0f}")


if __name__ == "__main__":
    main()
