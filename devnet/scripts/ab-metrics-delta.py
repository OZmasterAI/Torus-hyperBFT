#!/usr/bin/env python3
"""S426 BS-4a A/B analysis helpers.

Subcommands:
  fit <heights.tsv>   least-squares blocks/s over (t,height) samples -> ms/block
                      (same fit as sweep-o2-batchsize-s416.sh — never
                      endpoint-to-endpoint, S405 lesson)
  deltas <trial-dir>  metric deltas from full /metrics dumps
                      (metrics-<port>-{before,after}.txt) summed across the 4
                      validators; prints one CSV fragment:
                        p50_view_ms,p99_view_ms,views,pulls,missing_rej,handoffs,timeouts
                      A counter absent from every after-dump prints 'na'
                      (old binary lacks the BS-4a recovery counters — distinct
                      from a measured 0). A per-port negative delta means the
                      node restarted mid-trial (counter reset): the after value
                      is used as the delta.
  hist <trial-dir> [--hist NAME]
                      histogram_quantile over the before/after delta of ANY
                      Prometheus histogram (default torus_view_duration_seconds),
                      summed across the 4 validators. Prints one CSV fragment:
                        p50_ms,p99_ms,count
                      count==0 -> the histogram never fired in the window (e.g.
                      torus_state_root_compute_seconds is DEFINED but NOT wired
                      at fleet-pin 6e03294 — prints 0,0,0); callers must treat a
                      0 count as "no signal", not "0 ms". Added for the A1.6
                      large-state bake; the `deltas`/`fit` output is unchanged so
                      existing callers (ab-bs4a-s426.sh) are unaffected.
"""

import os
import re
import sys

PORTS = ["9091", "9092", "9093", "9094"]
HIST = "torus_view_duration_seconds"
COUNTERS = [
    ("pulls", "torus_native_da_pull_requests"),
    ("missing_rej", "torus_missing_action_rejections"),
    ("handoffs", "torus_native_da_recovery_handoffs"),
    ("timeouts", "torus_native_da_recovery_timeouts"),
]

LINE_RE = re.compile(r"^(\S+?)(?:\{([^}]*)\})?\s+(\S+)$")
LE_RE = re.compile(r'le="([^"]+)"')


def parse_dump(path):
    """-> (counters: base-name -> value, buckets: le-float -> cum count, hist_count)"""
    counters, buckets, hist_count = {}, {}, 0.0
    if not os.path.exists(path):
        return None
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            m = LINE_RE.match(line)
            if not m:
                continue
            name, labels, raw = m.group(1), m.group(2) or "", m.group(3)
            try:
                val = float(raw)
            except ValueError:
                continue
            if name == HIST + "_bucket":
                lm = LE_RE.search(labels)
                if lm:
                    le = float("inf") if lm.group(1) == "+Inf" else float(lm.group(1))
                    buckets[le] = buckets.get(le, 0.0) + val
            elif name == HIST + "_count":
                hist_count += val
            else:
                base = name[:-6] if name.endswith("_total") else name
                counters[base] = counters.get(base, 0.0) + val
    return counters, buckets, hist_count


def delta(after, before):
    """Counter-reset-aware delta: negative -> node restarted, use after value."""
    d = after - before
    return after if d < 0 else d


def quantile(bucket_deltas, q):
    """histogram_quantile over cumulative-bucket deltas -> seconds."""
    les = sorted(k for k in bucket_deltas if k != float("inf"))
    total = bucket_deltas.get(float("inf"), les and bucket_deltas[les[-1]] or 0.0)
    if total <= 0:
        return 0.0
    rank = q * total
    prev_le, prev_cum = 0.0, 0.0
    for le in les:
        cum = bucket_deltas[le]
        if cum >= rank:
            width, span = le - prev_le, cum - prev_cum
            frac = (rank - prev_cum) / span if span > 0 else 1.0
            return prev_le + frac * width
        prev_le, prev_cum = le, cum
    return prev_le  # rank falls in +Inf bucket: report top finite bound


def parse_hist(path, hist_name):
    """-> (buckets: le-float -> cum count, hist_count) for an arbitrary histogram.

    Independent of parse_dump's hardcoded HIST so `deltas` keeps its exact output.
    Returns None when the dump file is missing (port down at snap time)."""
    buckets, hist_count = {}, 0.0
    if not os.path.exists(path):
        return None
    bkt_name = hist_name + "_bucket"
    cnt_name = hist_name + "_count"
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            m = LINE_RE.match(line)
            if not m:
                continue
            name, labels, raw = m.group(1), m.group(2) or "", m.group(3)
            try:
                val = float(raw)
            except ValueError:
                continue
            if name == bkt_name:
                lm = LE_RE.search(labels)
                if lm:
                    le = float("inf") if lm.group(1) == "+Inf" else float(lm.group(1))
                    buckets[le] = buckets.get(le, 0.0) + val
            elif name == cnt_name:
                hist_count += val
    return buckets, hist_count


def cmd_hist(trial_dir, hist_name):
    bucket_deltas = {}
    count = 0.0
    for port in PORTS:
        before = parse_hist(
            os.path.join(trial_dir, f"metrics-{port}-before.txt"), hist_name
        )
        after = parse_hist(
            os.path.join(trial_dir, f"metrics-{port}-after.txt"), hist_name
        )
        if before is None or after is None:
            continue  # port never dumped (node down at snap time) — skip pair
        b_bkt, b_cnt = before
        a_bkt, a_cnt = after
        count += delta(a_cnt, b_cnt)
        for le, av in a_bkt.items():
            bucket_deltas[le] = bucket_deltas.get(le, 0.0) + delta(
                av, b_bkt.get(le, 0.0)
            )
    p50 = round(quantile(bucket_deltas, 0.50) * 1000.0, 1)
    p99 = round(quantile(bucket_deltas, 0.99) * 1000.0, 1)
    print(f"{p50},{p99},{int(count)}")


def cmd_fit(path):
    pts = []
    for line in open(path):
        f = line.split()
        if len(f) == 2 and int(f[1]) >= 0:
            pts.append((float(f[0]), int(f[1])))
    if len(pts) < 3:
        print(0)
        return
    mt = sum(t for t, _ in pts) / len(pts)
    mh = sum(h for _, h in pts) / len(pts)
    num = sum((t - mt) * (h - mh) for t, h in pts)
    den = sum((t - mt) ** 2 for t, _ in pts)
    bps = num / den if den else 0.0
    print(round(1000.0 / bps, 1) if bps > 0 else 0)


def cmd_deltas(trial_dir):
    counter_deltas = {}
    seen_in_after = set()
    bucket_deltas = {}
    views = 0.0
    for port in PORTS:
        before = parse_dump(os.path.join(trial_dir, f"metrics-{port}-before.txt"))
        after = parse_dump(os.path.join(trial_dir, f"metrics-{port}-after.txt"))
        if before is None or after is None:
            continue  # port never dumped (node down at snap time) — skip pair
        b_ctr, b_bkt, b_cnt = before
        a_ctr, a_bkt, a_cnt = after
        seen_in_after.update(a_ctr.keys())
        for _, base in COUNTERS:
            if base in a_ctr:
                counter_deltas[base] = counter_deltas.get(base, 0.0) + delta(
                    a_ctr[base], b_ctr.get(base, 0.0)
                )
        views += delta(a_cnt, b_cnt)
        for le, av in a_bkt.items():
            bucket_deltas[le] = bucket_deltas.get(le, 0.0) + delta(
                av, b_bkt.get(le, 0.0)
            )

    p50 = round(quantile(bucket_deltas, 0.50) * 1000.0, 1)
    p99 = round(quantile(bucket_deltas, 0.99) * 1000.0, 1)
    fields = [str(p50), str(p99), str(int(views))]
    for _, base in COUNTERS:
        if base in seen_in_after:
            fields.append(str(int(counter_deltas.get(base, 0.0))))
        else:
            fields.append("na")
    print(",".join(fields))


def main():
    argv = sys.argv[1:]
    if len(argv) < 2 or argv[0] not in ("fit", "deltas", "hist"):
        sys.exit(__doc__)
    cmd, target = argv[0], argv[1]
    if cmd == "fit":
        cmd_fit(target)
    elif cmd == "deltas":
        cmd_deltas(target)
    else:  # hist [--hist NAME]
        hist_name = HIST
        rest = argv[2:]
        if rest:
            if rest[0] == "--hist" and len(rest) == 2:
                hist_name = rest[1]
            else:
                sys.exit(__doc__)
        cmd_hist(target, hist_name)


if __name__ == "__main__":
    main()
