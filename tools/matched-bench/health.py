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
MAX_SAMPLE_GAP_S = 5
DEFAULT_STALL_S = 30
IDLE_EXEC_QUEUE_MAX = 2  # Empty blocks can be in flight while native state is quiet.


def complete(sample, keys):
    try:
        return (sample.get('scrape_valid', 1) == 1 and
                all(math.isfinite(float(sample[k])) and float(sample[k]) >= 0 for k in keys))
    except (KeyError, TypeError, ValueError):
        return False


def pending(sample):
    return sample[MEMPOOL] > 0 or sample[EXEC_QUEUE] > IDLE_EXEC_QUEUE_MAX or sample[FLUSH] > 0


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
    """Require a wall-clock quiet interval with progress on *every* validator."""
    def __init__(self, quiet_s):
        self.quiet_s = quiet_s
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
        eligible = good and not any(pending(s) for s in samples)
        same = eligible and self.base is not None and all(s[k] == b[k]
                    for s, b in zip(samples, self.base) for k in FLOW)
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


def parse_metrics(text):
    sample = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0] in REQUIRED:
            try:
                value = float(parts[1])
                if math.isfinite(value) and value >= 0:
                    sample[parts[0]] = value
            except ValueError:
                pass
    return sample


def fetch(url):
    try:
        with urlopen(url, timeout=3) as response:
            return parse_metrics(response.read().decode())
    except (OSError, ValueError):
        return {}


def wait_for_drain(urls, out, timeout_s, quiet_s):
    tracker = DrainTracker(quiet_s)
    start = time.monotonic()
    result = {'drained': False, 'reason': 'timeout before a complete scrape'}
    with ThreadPoolExecutor(max_workers=3) as pool, (out/'drain-samples.jsonl').open('w') as log:
        while time.monotonic()-start < timeout_s:
            samples = list(pool.map(fetch, urls))
            elapsed = time.monotonic()-start
            result = tracker.observe(elapsed, samples)
            if elapsed >= timeout_s:
                result = dict(result, drained=False, reason='drain deadline exceeded')
            log.write(json.dumps({'ts': time.time(), 'elapsed_s': elapsed, 'samples': samples,
                                  **result}, allow_nan=False)+'\n')
            log.flush()
            if result['drained']:
                break
            time.sleep(min(1, max(0, timeout_s-(time.monotonic()-start))))
    result.update(elapsed_s=time.monotonic()-start, quiet_s=quiet_s, timeout_s=timeout_s)
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
    if crash is not None:
        if crash['verdict'] == 'FAIL':
            failed.append('crash gate failed')
        elif crash['verdict'] != 'PASS':
            unknown.append('crash gate unverified')
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
    accept = sub.add_parser('accept')
    accept.add_argument('summary', type=Path)
    args = parser.parse_args()
    if args.command == 'accept':
        data = json.loads(args.summary.read_text())
        return 0 if data.get('validity', {}).get('accepted') is True else 2
    if not math.isfinite(args.timeout) or not math.isfinite(args.quiet) or min(args.timeout, args.quiet) <= 0:
        parser.error('timeout and quiet must be positive finite seconds')
    return wait_for_drain(args.urls, args.out, args.timeout, args.quiet)


if __name__ == '__main__':
    raise SystemExit(main())
