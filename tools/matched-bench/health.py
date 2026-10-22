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
# --attest-live (run-cell STATE_HASH_ATTEST=1): every validator signs one
# AttestStateHash per 100-block checkpoint for as long as the chain commits
# (crates/torus-consensus/src/state_hash.rs submit_attestations), and idle
# blocks keep committing after the load, so torus_native_actions_processed_total
# (app.rs: every replay-guard-passing native action of an executed block) and
# the native mempool never go quiet. No metric counts EXECUTED attestations
# (torus_state_hash_attestations_submitted_total counts this node's mempool
# admissions, resubmissions included) or splits the native counter or mempool
# by kind, so:
#   * executed: the quiet keys are the order counters, and every other native
#     action (CancelOrder / CancelAllOrders move books and margin but no order
#     counter; oracle prices, transfers, ...) must not execute in the quiet
#     window either: attest_window_step reads the window's block bodies by kind
#     (attest_traffic.py; --attest-rpc) and restarts the window if one did
#     (with --feed-live the running feed's SubmitOraclePrices are allowed);
#   * pending: inside the window the native mempool may hold up to
#     --attest-mempool-max entries, but the ACCEPTING sample needs it empty on
#     every node (the feed's footprint with --feed-live): an entry there may be
#     a stranded load action. Attestations are included within a block or two,
#     so an empty sample comes within seconds (9g-hn cells: 3 samples in 4).
# The traffic over the quiet window is recorded (drain.json .attest_live).
ATTEST_SUBMITTED = 'torus_state_hash_attestations_submitted_total'
# NativeAction variant of the oracle feed's submissions (torus_getBlockBody
# serde tag; run-cell's "$BENCH" oracle-feed): allowed in the --feed-live
# --attest-live scan.
ORACLE_KIND = 'SubmitOraclePrices'
DEFAULT_ATTEST_MEMPOOL_MAX = 6  # 2 checkpoints x 3 validators
FEED_LIVE_COLUMNS = ('elapsed_s', 'node', 'committed', 'exec_lag', 'mempool',
                     'chain_blocks', 'chain_ms', 'quiet_elapsed_s')
# --to-height (s64, run-cell's paused-feed settle): the executed height. The exec
# thread sets torus_block_height after each block it executes (app.rs
# execute_committed_block_with), and torus_exec_queue_depth counts the committed
# blocks dispatched to it but not yet executed, so executed + queue is the
# node's committed height.
EXEC_HEIGHT = 'torus_block_height'
TARGET_MISSED_RC = 3


def committed_height(sample):
    """A node's committed height: executed height + blocks queued for execution."""
    return sample[EXEC_HEIGHT] + sample[EXEC_QUEUE]


def height_target(samples, n=3):
    """The highest committed height across the n nodes (3; 2 with --exclude),
    or None unless every scrape is complete (a target from a partial set could
    sit too low)."""
    if len(samples) != n or not all(complete(s, REQUIRED + (EXEC_HEIGHT,)) for s in samples):
        return None
    return int(max(committed_height(s) for s in samples))


def complete(sample, keys):
    try:
        return (sample.get('scrape_valid', 1) == 1 and
                all(math.isfinite(float(sample[k])) and float(sample[k]) >= 0 for k in keys))
    except (KeyError, TypeError, ValueError):
        return False


def pending(sample, mempool_max=0, exec_queue_max=IDLE_EXEC_QUEUE_MAX):
    return (sample[MEMPOOL] > mempool_max or sample[EXEC_QUEUE] > exec_queue_max or
            sample[FLUSH] > 0)


def assess_liveness(rows, lo, hi, stall_s=DEFAULT_STALL_S, nodes=NODES):
    """Detect *every* observed stall, including ones followed by recovery.

    A stall is >=stall_s without a commit while work stays pending. Gaps, missing
    metrics and counter resets cannot be interpreted as evidence of health.
    This is an operational rejection threshold, not a throughput noise filter.
    nodes: the validators judged (run-cell KILL_NO_RESTART: the survivors; the
    killed node's scrapes fail from its kill on and say nothing about the chain).
    """
    judged = nodes
    nodes = {}
    for node in judged:
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
    committed blocks not yet executed) must stay <= max_lag on every sample.

    attest_live: as feed_live for the validators' AttestStateHash traffic: only
    the order counters must be quiet and the native mempool may hold up to
    attest_mempool_max entries (added to the feed's footprint when both run);
    the exec queue keeps its bound (idle, or max_lag with feed_live). The
    accepting sample needs the mempool within the feed's footprint alone (0
    without feed_live) on every node: nothing tells a pending attestation from
    a stranded load action. The window's other native traffic is checked by
    attest_window_step (block bodies), outside this pure tracker.

    min_height: also every node's executed height (EXEC_HEIGHT) >= min_height;
    a node without the gauge never counts as there.

    nodes: the validators observed, in the order of the samples (run-cell
    KILL_NO_RESTART: the survivors only, --exclude)."""
    def __init__(self, quiet_s, feed_live=False, max_lag=DEFAULT_MAX_LAG,
                 feed_mempool_max=DEFAULT_FEED_MEMPOOL_MAX, min_height=None,
                 attest_live=False, attest_mempool_max=DEFAULT_ATTEST_MEMPOOL_MAX,
                 nodes=NODES):
        self.nodes = tuple(nodes)
        self.quiet_s = quiet_s
        self.min_height = min_height
        self.quiet_keys = ORDER_FLOW if feed_live or attest_live else FLOW
        self.attest_live = attest_live
        self.final_mempool_max = feed_mempool_max if feed_live else 0
        self.limits = ((feed_mempool_max if feed_live else 0) +
                       (attest_mempool_max if attest_live else 0),
                       max_lag if feed_live else IDLE_EXEC_QUEUE_MAX)
        self.base = None
        self.previous = None
        self.since = None
        self.last_time = None

    def observe(self, now, samples):
        good = len(samples) == len(self.nodes) and all(complete(s, REQUIRED) for s in samples)
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
        if ready and self.attest_live and any(s[MEMPOOL] > self.final_mempool_max for s in samples):
            ready = False
            reason = ('waiting for a native mempool <= %d on every node (attestations may be '
                      'pending inside the window, a load action may not)' % self.final_mempool_max)
        behind = self.behind(samples) if good else []
        if behind:
            ready = False
            reason = 'waiting for every node to execute height %d (%s)' % (self.min_height, behind_text(behind))
        self.previous = [dict(s) for s in samples] if good else None
        self.last_time = now
        return {'drained': ready, 'reason': 'quiet and advancing' if ready else reason,
                'quiet_elapsed_s': now-self.since if self.since is not None else 0}

    def restart(self, now, samples):
        """Start a new quiet window at this (eligible) sample."""
        self.base = [dict(s) for s in samples]
        self.since = now

    def behind(self, samples):
        """[(node, executed height or None)] below min_height; [] without one."""
        if self.min_height is None:
            return []
        return [(node, s.get(EXEC_HEIGHT)) for node, s in zip(self.nodes, samples)
                if not (complete(s, (EXEC_HEIGHT,)) and s[EXEC_HEIGHT] >= self.min_height)]


def behind_text(behind):
    return ', '.join('%s at %s' % (n, 'no height' if h is None else int(h)) for n, h in behind)


def percentile(values, p):
    """Nearest-rank percentile of a non-empty list."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(p/100*len(ordered))-1)]


def feed_live_rows(elapsed, previous, samples, quiet_elapsed, nodes=NODES):
    """Per node: exec lag now, and native blocks executed since the previous
    scrape with their mean exec ms (exact per block when chain_blocks == 1)."""
    rows = []
    for node, prev, s in zip(nodes, previous, samples):
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


def attest_live_summary(base, last, attest_mempool_max, final_mempool_max=0, window=None,
                        restarts=0):
    """drain.json .attest_live: the attestations' footprint bounds, what the
    native counter and the submitted attestations (summed over the nodes) moved
    over the final quiet window (base = its first samples; None = no window),
    the last block-body scan of a window (attest_traffic.scan_window) and how
    often a non-attestation action restarted the window."""
    def delta(key):
        if base is None or last is None or not all(key in d for d in base + last):
            return None
        return [int(s[key]-b[key]) for s, b in zip(last, base)]
    native, submitted = delta(FLOW[2]), delta(ATTEST_SUBMITTED)
    return {'attest_mempool_max': attest_mempool_max, 'final_mempool_max': final_mempool_max,
            'native_actions_delta': native,
            'attestations_submitted_delta': sum(submitted) if submitted is not None else None,
            'window_scan': window, 'window_restarts': restarts}


def attest_window_step(tracker, now, samples, result, scan):
    """--attest-live, once the tracker drained: no native action other than
    AttestStateHash may have executed in the quiet window. scan(lo, hi) reads
    blocks lo..hi by kind (attest_traffic.scan_window): lo = the lowest
    executed height at the window's first samples, hi = the highest committed
    height now (executed + exec queue), so it covers every block any node can
    have executed inside the window. A non-attestation action restarts the
    window here (the legacy drain's native counter would have); an unreadable
    height or body keeps it open (not drained). Returns (result, scan or None)."""
    if not result['drained']:
        return result, None
    if not all(complete(s, (EXEC_HEIGHT,)) for s in tracker.base + samples):
        return dict(result, drained=False,
                    reason='attest-live: executed height (%s) unavailable, the quiet window '
                           'cannot be scanned' % EXEC_HEIGHT), None
    lo = int(min(s[EXEC_HEIGHT] for s in tracker.base))
    hi = int(max(committed_height(s) for s in samples))
    window = scan(lo, hi)
    if window['other'] > 0:
        tracker.restart(now, samples)
        return dict(result, drained=False, quiet_elapsed_s=0,
                    reason='attest-live: %d non-attestation native action(s) executed in blocks '
                           '%d..%d (at %s): quiet window restarted'
                           % (window['other'], lo, hi, window.get('other_heights'))), window
    if not window['clean']:
        return dict(result, drained=False,
                    reason='attest-live: block bodies unreadable in %d..%d (%d heights, e.g. %s)'
                           % (lo, hi, window['unread'], window.get('unread_heights'))), window
    return result, window


def wait_for_drain(urls, out, timeout_s, quiet_s, feed_live=False, max_lag=DEFAULT_MAX_LAG,
                   feed_mempool_max=DEFAULT_FEED_MEMPOOL_MAX, to_height=None,
                   attest_live=False, attest_mempool_max=DEFAULT_ATTEST_MEMPOOL_MAX,
                   attest_rpcs=None, scan=None, nodes=NODES, excluded=()):
    """to_height: None (no height condition), an int, or 'auto' = the
    height_target() of the first complete scrape (the chain at the call).
    attest_live: scan (lo, hi) -> attest_traffic.scan_window result; default:
    over attest_rpcs. nodes: the validators behind `urls`, in order (--exclude
    drops the killed one); excluded: recorded in drain.json when not empty."""
    nodes = tuple(nodes)
    if len(nodes) != len(urls):
        raise ValueError('%d urls for nodes %s' % (len(urls), nodes))
    tracker = DrainTracker(quiet_s, feed_live, max_lag, feed_mempool_max,
                           to_height if isinstance(to_height, int) else None,
                           attest_live, attest_mempool_max, nodes)
    extra = (CHAIN_SUM, CHAIN_COUNT) if feed_live else ()
    if attest_live:
        extra += (ATTEST_SUBMITTED,)
        if scan is None:
            from attest_traffic import scan_window
            # --feed-live: the oracle feed keeps submitting through this
            # drain, so its own kind is expected traffic like the
            # attestations. Without it (the settle drain after the feed is
            # paused) and in run-cell's digest window, nothing is allowed.
            allowed = (ORACLE_KIND,) if feed_live else ()
            scan = lambda lo, hi: scan_window(list(attest_rpcs or ()), lo, hi, allowed)
    if to_height is not None or attest_live:
        extra += (EXEC_HEIGHT,)
    last = None
    window, restarts = None, 0
    target_by_node, heights = None, {n: None for n in nodes}
    rows, previous = [], [{} for _ in urls]
    start = time.monotonic()
    result = {'drained': False, 'reason': 'timeout before a complete scrape'}
    with ThreadPoolExecutor(max_workers=3) as pool, (out/'drain-samples.jsonl').open('w') as log:
        while time.monotonic()-start < timeout_s:
            samples = list(pool.map(lambda url: fetch(url, extra), urls))
            elapsed = time.monotonic()-start
            if to_height == 'auto' and tracker.min_height is None:
                tracker.min_height = height_target(samples, len(nodes))
                if tracker.min_height is not None:
                    target_by_node = {n: int(committed_height(s)) for n, s in zip(nodes, samples)}
            for n, s in zip(nodes, samples):
                if EXEC_HEIGHT in s:
                    heights[n] = int(s[EXEC_HEIGHT])
            result = tracker.observe(elapsed, samples)
            last = samples
            if to_height == 'auto' and tracker.min_height is None:
                result = dict(result, drained=False,
                              reason='height target not recorded yet: no complete scrape')
            if elapsed >= timeout_s:
                result = dict(result, drained=False, reason='drain deadline exceeded')
            if attest_live:
                result, scanned = attest_window_step(tracker, elapsed, samples, result, scan)
                if scanned is not None:
                    window = scanned
                    restarts += scanned['other'] > 0
            log.write(json.dumps({'ts': time.time(), 'elapsed_s': elapsed, 'samples': samples,
                                  **result}, allow_nan=False)+'\n')
            log.flush()
            if feed_live:
                rows += feed_live_rows(elapsed, previous, samples, result['quiet_elapsed_s'], nodes)
                previous = samples
            if result['drained']:
                break
            time.sleep(min(1, max(0, timeout_s-(time.monotonic()-start))))
    result.update(elapsed_s=time.monotonic()-start, quiet_s=quiet_s, timeout_s=timeout_s)
    if excluded:
        # run-cell KILL_NO_RESTART: the killed validator is not a drain participant
        result.update(nodes=list(nodes), excluded_nodes=sorted(excluded))
    target_missed = False
    if to_height is not None:
        target = tracker.min_height
        behind = [(n, h) for n, h in heights.items() if h is None or target is None or h < target]
        target_missed = not result['drained'] and (target is None or bool(behind))
        result.update(to_height=to_height, target_height=target, target_reached=not target_missed,
                      target_committed_by_node=target_by_node, exec_heights=heights)
        if target_missed:
            result['reason'] = ('height target not recorded: no complete scrape' if target is None else
                                'height target %d not reached (%s)' % (target, behind_text(behind)))
    if feed_live:
        with (out/'drain-feed-live.tsv').open('w') as tsv:
            tsv.write('\t'.join(FEED_LIVE_COLUMNS)+'\n')
            tsv.writelines('\t'.join('' if r[k] is None else
                                     str(round(r[k], 3) if isinstance(r[k], float) else r[k])
                                     for k in FEED_LIVE_COLUMNS) + '\n' for r in rows)
        result['feed_live'] = dict(feed_live_summary(rows, tracker.since), max_lag_bound=max_lag,
                                   feed_mempool_max=feed_mempool_max)
    if attest_live:
        result['attest_live'] = attest_live_summary(
            tracker.base if result['drained'] else None, last, attest_mempool_max,
            tracker.final_mempool_max, window, restarts)
    (out/'drain.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result))
    return 0 if result['drained'] else TARGET_MISSED_RC if target_missed else 1


DISSEM_FAIL_KEYS = ('exhausted', 'sync_fallback', 'da_outbound_fail', 'starvation')


def failure_targets(record, peer_nodes):
    """The nodes a dissemination-failure line (a collect_logs.py record) is
    reported toward: the OUTBOUND FAILURE peer, else the fetched block's
    proposers (report only: a fetch rotates across every other validator
    before it falls back to sync); [] when the evidence names none (or names
    an unknown peer id)."""
    if record.get('peer_id') is not None:
        node = peer_nodes.get(record['peer_id'])
        return [node] if node else []
    return list(record.get('proposers') or [])


def kill_dissemination(log_summary, killed, kill_ts, counts=None):
    """KILL_NO_RESTART dissemination verdict from per-peer evidence
    (collect_logs.py's log-summary.json); None when no node was killed (that
    cell is judged on its counts, as always).

    A failure line is EXEMPT only when the line itself names the killed node
    as its peer (an OUTBOUND FAILURE toward its peer id) and is dated at or
    after kill_ts. Everything else counts: failures between the survivors,
    toward the killed node before its kill, the killed node's own lines (all
    from before its SIGKILL), lines that name no peer, every body / justify
    fetch fallback (the fetch rotates across all other validators before it
    falls back to sync, so a survivor failed too, whoever proposed the block;
    its proposer is reported in live_by_node, never exempted), and counted
    lines without a per-line record
    (truncated, or a log-summary.json from before the records). counts
    (summarize.py's --dissem, per node) must equal the log summary's own,
    else nothing is exempt.
    -> {clean, killed_node, kill_ts, exempt_toward_killed, live_failures,
        live_by_node: {node: {lines, toward: {node|'unknown': n}}}, evidence}"""
    if not killed:
        return None
    nodes = ('val0', 'val1', 'val2')
    entries = {n: log_summary.get(n) or {} for n in nodes}
    peer_nodes = {e['local_peer_id']: n for n, e in entries.items() if e.get('local_peer_id')}
    exempt, live, notes = 0, {}, {}

    def count_live(node, toward, lines=1):
        row = live.setdefault(node, {'lines': 0, 'toward': {}})
        row['lines'] += lines
        for t in toward:
            row['toward'][t] = row['toward'].get(t, 0) + lines

    for n, entry in entries.items():
        given = (counts or {}).get(n) or {}
        node_counts = entry.get('counts') or given
        if counts is not None and any((given.get(k) or 0) != (node_counts.get(k) or 0)
                                      for k in DISSEM_FAIL_KEYS):
            notes.setdefault('mismatch', '%s: --dissem counts differ from log-summary.json' % n)
        records = entry.get('failures')
        if records is None:
            records = []
            notes.setdefault('counts only', '%s: no per-line records' % n)
        tally = dict.fromkeys(DISSEM_FAIL_KEYS, 0)
        for r in records:
            for k in r.get('kinds') or ():
                tally[k] = tally.get(k, 0) + 1
            targets = failure_targets(r, peer_nodes)
            ts = r.get('ts')
            toward_killed = r.get('peer_id') is not None and peer_nodes.get(r['peer_id']) == killed
            if (n != killed and toward_killed and ts is not None
                    and kill_ts is not None and ts >= kill_ts):
                exempt += 1
            else:
                count_live(n, targets or ['unknown'])
        # counted failures no record covers: unattributed, so they count
        missing = sum(max(0, (node_counts.get(k) or 0) - tally[k]) for k in DISSEM_FAIL_KEYS)
        if missing:
            notes.setdefault('incomplete', '%s: %d counted failures without a record' % (n, missing))
            count_live(n, ['unknown'], missing)
    if 'mismatch' in notes:
        exempt, clean = 0, False
    else:
        clean = not live
    evidence = '; '.join('%s (%s)' % kv for kv in notes.items()) or 'per-line'
    return {'clean': clean, 'killed_node': killed, 'kill_ts': kill_ts,
            'exempt_toward_killed': exempt,
            'live_failures': sum(r['lines'] for r in live.values()),
            'live_by_node': live, 'evidence': evidence}


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
    drain.add_argument('--attest-live', action='store_true',
                       help='STATE_HASH_ATTEST=1: tolerate the validators\' AttestStateHash traffic '
                            '(see ATTEST_SUBMITTED)')
    drain.add_argument('--attest-mempool-max', type=int, default=DEFAULT_ATTEST_MEMPOOL_MAX,
                       help='--attest-live: max native mempool size from attestations inside the '
                            'quiet window (added to --feed-mempool-max with --feed-live)')
    drain.add_argument('--attest-rpc', nargs=3, default=None,
                       help="--attest-live (required): the validators' JSON-RPC URLs; the quiet "
                            'window\'s block bodies are read by action kind (attest_traffic.py)')
    drain.add_argument('--exclude', action='append', default=[], choices=NODES[1:],
                       help='run-cell KILL_NO_RESTART: a validator killed without restart; its '
                            '--urls / --attest-rpc entry is dropped and only the others must drain')
    drain.add_argument('--to-height', type=to_height_arg, default=None,
                       help="also wait until every node executed this height ('auto': the highest "
                            "committed height across the nodes at the first complete scrape); "
                            "exit %d if it is not reached within --timeout" % TARGET_MISSED_RC)
    accept = sub.add_parser('accept')
    accept.add_argument('summary', type=Path)
    args = parser.parse_args()
    if args.command == 'accept':
        data = json.loads(args.summary.read_text())
        return 0 if data.get('validity', {}).get('accepted') is True else 2
    if not math.isfinite(args.timeout) or not math.isfinite(args.quiet) or min(args.timeout, args.quiet) <= 0:
        parser.error('timeout and quiet must be positive finite seconds')
    if min(args.max_lag, args.feed_mempool_max, args.attest_mempool_max) < 0:
        parser.error('max-lag, feed-mempool-max and attest-mempool-max must be >= 0')
    if args.attest_live and not args.attest_rpc:
        parser.error('--attest-live needs --attest-rpc (3 URLs): without the block bodies the '
                     'quiet window cannot tell attestations from other native actions')
    excluded = sorted(set(args.exclude))
    if len(excluded) > 1:
        parser.error('--exclude: at most one validator (two survivors of three)')
    keep = [i for i, n in enumerate(NODES) if n not in excluded]
    nodes = tuple(NODES[i] for i in keep)
    urls = [args.urls[i] for i in keep]
    attest_rpc = [args.attest_rpc[i] for i in keep] if args.attest_rpc else args.attest_rpc
    return wait_for_drain(urls, args.out, args.timeout, args.quiet, args.feed_live,
                          args.max_lag, args.feed_mempool_max, args.to_height,
                          args.attest_live, args.attest_mempool_max, attest_rpc,
                          nodes=nodes, excluded=excluded)


def to_height_arg(value):
    if value == 'auto':
        return value
    try:
        height = int(value)
    except ValueError:
        height = 0
    if height <= 0:
        raise argparse.ArgumentTypeError("'auto' or a positive height (got %r)" % value)
    return height


if __name__ == '__main__':
    raise SystemExit(main())
