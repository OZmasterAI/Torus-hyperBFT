#!/usr/bin/env python3
"""compare.py — one-line-per-cell table over run-cell.sh result dirs.

    compare.py <result-dir>...   (dirs holding summary.json; globs welcome)

Columns are NODE-counter numbers only (matched/s window avg + best-60s on val0),
the val0 per-native-block phase ms that the r5 root-and-save-workers sweep
targets (root, state_write, save_books, block), the worker counts that
ACTUALLY engaged (torus_exec_root_bucket_hash_workers /
torus_exec_save_books_workers, last/max over the window), member-cache hit
ratio, box load1 max, and the 3-validator agreement verdict — a cell with
agree=False is REJECTED whatever its matched/s. Cells are grouped by label
with the trailing -rN stripped and a mean over reps is printed per group.
"""

import glob, json, os, statistics, sys


def load(d):
    p = os.path.join(d, "summary.json")
    try:
        with open(p) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def g(d, *ks, default=None):
    for k in ks:
        if not isinstance(d, dict) or k not in d or d[k] is None:
            return default
        d = d[k]
    return d


def fmt(v, w=8, nd=1):
    if v is None:
        return "-".rjust(w)
    if isinstance(v, float):
        return f"{v:.{nd}f}".rjust(w)
    return str(v).rjust(w)


dirs = []
for a in sys.argv[1:]:
    dirs.extend(sorted(glob.glob(a)) or [a])
rows = []
for d in dirs:
    d = d.rstrip("/")
    s = load(d)
    if not s:
        continue
    h = s.get("headline", {})
    p0 = g(s, "phase_by_node", "val0", default={}) or {}
    ph = p0.get("phases", {}) or {}
    w = p0.get("workers", {}) or {}
    mc = p0.get("member_cache", {}) or {}
    rows.append(
        {
            "label": s.get("label") or os.path.basename(d),
            "status": s.get("status"),
            "matched": h.get("matched_s_avg"),
            "best60": h.get("matched_s_best60"),
            "blk_s": h.get("blk_s_avg"),
            "block_ms": p0.get("block_ms"),
            "root_ms": g(ph, "flush", "root_ms"),
            "sw_ms": g(ph, "flush", "state_write_ms"),
            "save_ms": g(ph, "save_books", "ms"),
            "dirtyb": p0.get("dirty_buckets_per_flush"),
            "rw": f"{w.get('root_bucket_hash_last', '-')}/{w.get('root_bucket_hash_max', '-')}",
            "sw": f"{w.get('save_books_last', '-')}/{w.get('save_books_max', '-')}",
            "mc_hit": mc.get("hit_ratio"),
            "load1": g(s, "cpu", "load1_max"),
            "agree": h.get("validators_agree"),
            "extra": g(s, "cell", "extra_env", default=""),
        }
    )

hdr = f"{'label':40} {'status':8} {'matched/s':>9} {'best60':>8} {'blk/s':>6} {'block_ms':>8} {'root_ms':>8} {'sw_ms':>7} {'save_ms':>8} {'dirtyb':>7} {'rootW':>6} {'saveW':>6} {'mc_hit':>6} {'load1':>6} agree  extra_env"
print(hdr)
print("-" * len(hdr))
groups = {}
for r in rows:
    print(
        f"{r['label']:40} {str(r['status']):8} {fmt(r['matched'], 9)} {fmt(r['best60'], 8)} {fmt(r['blk_s'], 6, 2)} "
        f"{fmt(r['block_ms'], 8)} {fmt(r['root_ms'], 8)} {fmt(r['sw_ms'], 7)} {fmt(r['save_ms'], 8)} {fmt(r['dirtyb'], 7, 0)} "
        f"{r['rw']:>6} {r['sw']:>6} {fmt(r['mc_hit'], 6, 3)} {fmt(r['load1'], 6)} {str(r['agree']):5}  {r['extra']}"
    )
    base = r["label"]
    if "-r" in base and base.rsplit("-r", 1)[1].isdigit():
        base = base.rsplit("-r", 1)[0]
    groups.setdefault(base, []).append(r)

print()
print(
    f"{'cell (mean over reps)':40} {'n':>2} {'matched/s':>9} {'best60':>8} {'root_ms':>8} {'save_ms':>8} {'block_ms':>8} all_agree"
)
for base, rs in groups.items():

    def mean(k):
        vs = [r[k] for r in rs if isinstance(r[k], (int, float))]
        return statistics.mean(vs) if vs else None

    print(
        f"{base:40} {len(rs):>2} {fmt(mean('matched'), 9)} {fmt(mean('best60'), 8)} {fmt(mean('root_ms'), 8)} "
        f"{fmt(mean('save_ms'), 8)} {fmt(mean('block_ms'), 8)} {str(all(r['agree'] for r in rs))}"
    )
