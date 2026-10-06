#!/usr/bin/env python3
"""Benchmark progress and drain evidence, independent of state agreement."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import math
from pathlib import Path
import time
from urllib.request import urlopen

NODES = ('val0', 'val1', 'val2')
COMMITTED = 'torus_blocks_committed_total'
MEMPOOL = 'torus_mempool_native_size'
EXEC_QUEUE = 'torus_exec_queue_depth'
FLUSH = 'torus_flush_worker_depth'
FLOW = ('torus_orders_placed_accepted_total', 'torus_orders_matched_total',
        'torus_native_actions_processed_total', 'torus_orders_resting_total')
REQUIRED = (COMMITTED, MEMPOOL, EXEC_QUEUE, FLUSH) + FLOW
# s77: fills are readable over RPC only once the trade writer has written them,
# so its backlog is pending work for the drain. Optional: an older node binary
# without the gauge drains as before.
TRADE_WRITER = 'torus_trade_writer_queued_batches'
MAX_SAMPLE_GAP_S = 5
DEFAULT_STALL_S = 30
IDLE_EXEC_QUEUE_MAX = 2  # Empty blocks can be in flight while native state is quiet.
# --feed-live (run-cell ORACLE_FEED_DRAIN=1): the oracle feed keeps submitting
# through the drain. Its SubmitOraclePrices are native actions: they move
# torus_native_actions_processed_total and the native mempool, never the order
# counters. No metric splits the mempool by action kind, so "bench pool empty"
# is approximated by quiet order counters plus a mempool no larger than the
# feed's own footprint (--feed-mempool-max).
ORDER_FLOW = tuple(k for k in FLOW if k != 'torus_native_actions_processed_total')
# Exec wall per NATIVE block (observed only for blocks carrying native actions).
CHAIN_SUM, CHAIN_COUNT = 'torus_exec_chain_seconds_sum', 'torus_exec_chain_seconds_count'
DEFAULT_MAX_LAG = 2
DEFAULT_FEED_MEMPOOL_MAX = 6  # 2 rounds x 3 validators x 1 chunk (<= 256 markets)
FEED_LIVE_COLUMNS = ('elapsed_s', 'node', 'committed', 'exec_lag', 'mempool',
                     'chain_blocks', 'chain_ms', 'quiet_elapsed_s')


def complete(sample, keys):
    try:
        return (sample.get('scrape_valid', 1) == 1 and
                all(math.isfinite(float(sample[k])) and float(sample[k]) >= 0 for k in keys))
    except (KeyError, TypeError, ValueError):
        return False


def pending(sample, mempool_max=0, exec_queue_max=IDLE_EXEC_QUEUE_MAX):
    return (sample[MEMPOOL] > mempool_max or sample[EXEC_QUEUE] > exec_queue_max or
            sample[FLUSH] > 0)


def assess_liveness(rows, lo, hi, stall_s=DEFAULT_STALL_S):
    """Detect *every* observed stall, including ones followed by recovery.

    A stall is >=stall_s without a commit while work stays pending. Gaps, missing
    metrics and counter resets cannot be interpreted as evidence of health.
    This is an operational rejection threshold, not a throughput noise filter.
    """
    nodes = {}
    for node in NODES:
        samples = [r for r in rows.get(node, []) if lo <= r['ts'] <= hi]
        problems, stalls = [], []
        previous = None
        start = None
        last = None
        progress = 0

        def finish_stall():
            if start is not None and last - start >= stall_s:
                stalls.append({'start': start, 'end': last, 'seconds': last-start})

        if len(samples) < 2:
            problems.append('fewer than two samples')
        elif samples[0]['ts'] - lo > MAX_SAMPLE_GAP_S or hi - samples[-1]['ts'] > MAX_SAMPLE_GAP_S:
            problems.append('window boundaries not covered')
        for sample in samples:
            if not complete(sample, (COMMITTED, MEMPOOL, EXEC_QUEUE, FLUSH)):
                finish_stall()
                problems.append('missing or invalid progress/backlog metrics')
                previous, start, last = None, None, None
                continue
            if previous is not None:
                gap = sample['ts'] - previous['ts']
                delta = sample[COMMITTED] - previous[COMMITTED]
                if gap <= 0 or gap > MAX_SAMPLE_GAP_S or delta < 0:
                    finish_stall()
                    problems.append('sample gap, unordered time, or counter reset')
                    start, last = None, None
                elif delta > 0:
                    progress += delta
                    finish_stall()
                    start, last = None, None
                elif pending(previous) and pending(sample):
                    if start is None:
                        start = previous['ts']
                    last = sample['ts']
                else:
                    finish_stall()
                    start, last = None, None
            previous = sample
        finish_stall()
        if stalls:
            verdict = 'FAIL'
        elif problems:
            verdict = 'UNKNOWN'
        elif progress == 0:
            verdict = 'FAIL'
            problems.append('no commit progress in observed window')
        else:
            verdict = 'PASS'
        nodes[node] = {'verdict': verdict, 'stalls': stalls, 'commits_observed': progress,
                       'problems': sorted(set(problems))}
    verdicts = [v['verdict'] for v in nodes.values()]
    verdict = 'FAIL' if 'FAIL' in verdicts else 'UNKNOWN' if 'UNKNOWN' in verdicts else 'PASS'
    return {'verdict': verdict, 'window': [lo, hi], 'stall_threshold_s': stall_s,
            'max_sample_gap_s': MAX_SAMPLE_GAP_S, 'nodes': nodes}


class DrainTracker:
    """Require a wall-clock quiet interval with progress on *every* validator.

    feed_live: only the order counters must be quiet; the native mempool may
    hold up to feed_mempool_max entries and the exec lag (torus_exec_queue_depth:
    committed blocks not yet executed) must stay <= max_lag on every sample."""
    def __init__(self, quiet_s, feed_live=False, max_lag=DEFAULT_MAX_LAG,
                 feed_mempool_max=DEFAULT_FEED_MEMPOOL_MAX):
        self.quiet_s = quiet_s
        self.quiet_keys = ORDER_FLOW if feed_live else FLOW
        self.limits = (feed_mempool_max, max_lag) if feed_live else (0, IDLE_EXEC_QUEUE_MAX)
        self.base = None
        self.previous = None
        self.since = None
        self.last_time = None

    def observe(self, now, samples):
        good = len(samples) == 3 and all(complete(s, REQUIRED) for s in samples)
        reason = 'waiting for quiet counters and empty pending-work gauges'
        if not good:
            reason = 'missing or invalid metrics'
        elif self.previous and any(s[k] < p[k] for s, p in zip(samples, self.previous)
                                   for k in (COMMITTED,) + FLOW):
            good = False
            reason = 'counter reset'
        elif self.last_time is not None and not 0 < now - self.last_time <= MAX_SAMPLE_GAP_S:
            good = False
            reason = 'scrape gap'
        eligible = good and not any(pending(s, *self.limits) or s.get(TRADE_WRITER, 0) > 0
                                    for s in samples)
        same = eligible and self.base is not None and all(s[k] == b[k]
                    for s, b in zip(samples, self.base) for k in self.quiet_keys)
        if not same:
            self.base = [dict(s) for s in samples] if eligible else None
            self.since = now if eligible else None
        ready = bool(same and now-self.since >= self.quiet_s and
                     all(s[COMMITTED] > b[COMMITTED] for s, b in zip(samples, self.base)))
        if same and not ready:
            reason = 'waiting for quiet interval and commit progress on every node'
        self.previous = [dict(s) for s in samples] if good else None
        self.last_time = now
        return {'drained': ready, 'reason': 'quiet and advancing' if ready else reason,
                'quiet_elapsed_s': now-self.since if self.since is not None else 0}


def percentile(values, p):
    """Nearest-rank percentile of a non-empty list."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(p/100*len(ordered))-1)]


def feed_live_rows(elapsed, previous, samples, quiet_elapsed):
    """Per node: exec lag now, and native blocks executed since the previous
    scrape with their mean exec ms (exact per block when chain_blocks == 1)."""
    rows = []
    for node, prev, s in zip(NODES, previous, samples):
        blocks = ms = None
        if all(k in d for d in (prev, s) for k in (CHAIN_SUM, CHAIN_COUNT)):
            blocks = int(s[CHAIN_COUNT]-prev[CHAIN_COUNT])
            if blocks > 0:
                ms = round(1000*(s[CHAIN_SUM]-prev[CHAIN_SUM])/blocks, 3)
        # elapsed_s stays raw: feed_live_summary compares it with the tracker's
        # raw `since`; only the TSV writer rounds.
        rows.append({'elapsed_s': elapsed, 'node': node,
                     'committed': s.get(COMMITTED), 'exec_lag': s.get(EXEC_QUEUE),
                     'mempool': s.get(MEMPOOL), 'chain_blocks': blocks, 'chain_ms': ms,
                     'quiet_elapsed_s': quiet_elapsed})
    return rows


def feed_live_summary(rows, since):
    """Over the final quiet window (rows after `since`), where every native
    block is an oracle-only block: max exec lag, and p50/p95/max of the
    per-interval mean exec ms per native block."""
    window = [r for r in rows if since is not None and r['elapsed_s'] > since]
    lags = [r['exec_lag'] for r in window if r['exec_lag'] is not None]
    native = [r for r in window if r['chain_blocks']]
    ms = [r['chain_ms'] for r in native]
    return {'window_start_s': since, 'max_exec_lag': max(lags) if lags else None,
            'native_intervals': len(native),
            'single_block_intervals': sum(r['chain_blocks'] == 1 for r in native),
            'native_blocks': sum(r['chain_blocks'] for r in native),
            'chain_ms': {'p50': percentile(ms, 50), 'p95': percentile(ms, 95),
                         'max': max(ms)} if ms else None}


def parse_metrics(text, extra=()):
    sample = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and (parts[0] in REQUIRED or parts[0] == TRADE_WRITER or
                                parts[0] in extra):
            try:
                value = float(parts[1])
                if math.isfinite(value) and value >= 0:
                    sample[parts[0]] = value
            except ValueError:
                pass
    return sample


def fetch(url, extra=()):
    try:
        with urlopen(url, timeout=3) as response:
            return parse_metrics(response.read().decode(), extra)
    except (OSError, ValueError):
        return {}


def wait_for_drain(urls, out, timeout_s, quiet_s, feed_live=False, max_lag=DEFAULT_MAX_LAG,
                   feed_mempool_max=DEFAULT_FEED_MEMPOOL_MAX):
    tracker = DrainTracker(quiet_s, feed_live, max_lag, feed_mempool_max)
    extra = (CHAIN_SUM, CHAIN_COUNT) if feed_live else ()
    rows, previous = [], [{} for _ in urls]
    start = time.monotonic()
    result = {'drained': False, 'reason': 'timeout before a complete scrape'}
    with ThreadPoolExecutor(max_workers=3) as pool, (out/'drain-samples.jsonl').open('w') as log:
        while time.monotonic()-start < timeout_s:
            samples = list(pool.map(lambda url: fetch(url, extra), urls))
            elapsed = time.monotonic()-start
            result = tracker.observe(elapsed, samples)
            if elapsed >= timeout_s:
                result = dict(result, drained=False, reason='drain deadline exceeded')
            log.write(json.dumps({'ts': time.time(), 'elapsed_s': elapsed, 'samples': samples,
                                  **result}, allow_nan=False)+'\n')
            log.flush()
            if feed_live:
                rows += feed_live_rows(elapsed, previous, samples, result['quiet_elapsed_s'])
                previous = samples
            if result['drained']:
                break
            time.sleep(min(1, max(0, timeout_s-(time.monotonic()-start))))
    result.update(elapsed_s=time.monotonic()-start, quiet_s=quiet_s, timeout_s=timeout_s)
    if feed_live:
        with (out/'drain-feed-live.tsv').open('w') as tsv:
            tsv.write('\t'.join(FEED_LIVE_COLUMNS)+'\n')
            tsv.writelines('\t'.join('' if r[k] is None else
                                     str(round(r[k], 3) if isinstance(r[k], float) else r[k])
                                     for k in FEED_LIVE_COLUMNS) + '\n' for r in rows)
        result['feed_live'] = dict(feed_live_summary(rows, tracker.since), max_lag_bound=max_lag,
                                   feed_mempool_max=feed_mempool_max)
    (out/'drain.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result))
    return 0 if result['drained'] else 1


def acceptance(liveness, drained, bench_rc, agreement, dissemination_clean, crash=None):
    failed, unknown = [], []
    if bench_rc != 0:
        failed.append('load generator failed')
    if not drained:
        failed.append('drain not established')
    if liveness['verdict'] == 'FAIL':
        failed.append('liveness failed')
    elif liveness['verdict'] != 'PASS':
        unknown.append('liveness unverified')
    if agreement in ('DISAGREE', 'INCOMPLETE'):
        failed.append('validator agreement failed')
    elif agreement != 'AGREE':
        unknown.append('validator agreement unverified')
    if dissemination_clean is False:
        failed.append('dissemination failures')
    elif dissemination_clean is None:
        unknown.append('dissemination unverified')
    if crash is not None and crash['verdict'] != 'PASS':
        failed.append('crash gate failed')
    verdict = 'REJECT' if failed else 'UNVERIFIED' if unknown else 'ACCEPT'
    return {'verdict': verdict, 'accepted': verdict == 'ACCEPT',
            'fail_reasons': failed, 'unverified_reasons': unknown}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    drain = sub.add_parser('drain')
    drain.add_argument('--urls', nargs=3, required=True)
    drain.add_argument('--out', type=Path, required=True)
    drain.add_argument('--timeout', type=float, required=True)
    drain.add_argument('--quiet', type=float, default=10)
    drain.add_argument('--feed-live', action='store_true',
                       help='an oracle feed keeps running: tolerate its traffic (see ORDER_FLOW)')
    drain.add_argument('--max-lag', type=int, default=DEFAULT_MAX_LAG,
                       help='--feed-live: max torus_exec_queue_depth on every sample')
    drain.add_argument('--feed-mempool-max', type=int, default=DEFAULT_FEED_MEMPOOL_MAX,
                       help="--feed-live: max native mempool size (the feed's own entries)")
    accept = sub.add_parser('accept')
    accept.add_argument('summary', type=Path)
    args = parser.parse_args()
    if args.command == 'accept':
        data = json.loads(args.summary.read_text())
        return 0 if data.get('validity', {}).get('accepted') is True else 2
    if not math.isfinite(args.timeout) or not math.isfinite(args.quiet) or min(args.timeout, args.quiet) <= 0:
        parser.error('timeout and quiet must be positive finite seconds')
    if min(args.max_lag, args.feed_mempool_max) < 0:
        parser.error('max-lag and feed-mempool-max must be >= 0')
    return wait_for_drain(args.urls, args.out, args.timeout, args.quiet, args.feed_live,
                          args.max_lag, args.feed_mempool_max)


if __name__ == '__main__':
    raise SystemExit(main())
