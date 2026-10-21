#!/usr/bin/env python3
"""Regression cases for stalled-but-equal chains and incomplete scrapes."""
import csv
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from health import (COMMITTED, MEMPOOL, EXEC_QUEUE, FLUSH, FLOW, NODES, TRADE_WRITER,
                    CHAIN_SUM, CHAIN_COUNT, EXEC_HEIGHT, DrainTracker, acceptance,
                    assess_liveness, committed_height, feed_live_rows, feed_live_summary,
                    height_target, parse_metrics)
from test_harness import BENCH_START, run_summarize, write_agreement, write_cell
from collect_logs import collect


def sample(commits=100, mempool=0, exec_queue=1, flush=0, flow=1000):
    return {COMMITTED: commits, MEMPOOL: mempool, EXEC_QUEUE: exec_queue,
            FLUSH: flush, **{k: flow for k in FLOW}}


class DrainTest(unittest.TestCase):
    def test_idle_blocks_are_allowed_but_every_validator_must_advance(self):
        tracker = DrainTracker(10)
        for t in range(12):
            nodes = [sample(100+t, exec_queue=2) for _ in NODES]
            nodes[2][COMMITTED] = 100
            self.assertFalse(tracker.observe(t, nodes)['drained'])
        nodes[2][COMMITTED] = 101
        self.assertTrue(tracker.observe(12, nodes)['drained'])

    def test_stopped_equal_chain_never_drains_even_with_empty_gauges(self):
        tracker = DrainTracker(10)
        for t in range(100):
            self.assertFalse(tracker.observe(t, [sample() for _ in NODES])['drained'])

    def test_trade_writer_backlog_blocks_drain_until_empty(self):
        # s77: fills are only readable over RPC once the trade writer has
        # written them, so a backlog is pending work; a node binary without
        # the gauge (older build) drains as before.
        tracker = DrainTracker(10)
        for t in range(30):
            nodes = [dict(sample(100+t), **{TRADE_WRITER: 5}) for _ in NODES]
            self.assertFalse(tracker.observe(t, nodes)['drained'])
        drained = [tracker.observe(t, [dict(sample(100+t), **{TRADE_WRITER: 0}) for _ in NODES])
                   for t in range(30, 42)]
        self.assertFalse(drained[0]['drained'])
        self.assertTrue(drained[-1]['drained'])

    def test_parse_metrics_keeps_trade_writer_gauge(self):
        self.assertEqual(parse_metrics(f"{TRADE_WRITER} 7\n")[TRADE_WRITER], 7.0)

    def test_pending_work_resets_quiet_interval(self):
        for field, value in [(MEMPOOL, 1), (EXEC_QUEUE, 3), (FLUSH, 1)]:
            tracker = DrainTracker(10)
            for t in range(20):
                nodes = [sample(100+t) for _ in NODES]
                if t == 9:
                    nodes[1][field] = value
                self.assertFalse(tracker.observe(t, nodes)['drained'], field)
            self.assertTrue(tracker.observe(20, [sample(120) for _ in NODES])['drained'])

    def test_counter_change_restarts_quiet_interval(self):
        tracker = DrainTracker(3)
        for t in range(6):
            result = tracker.observe(t, [sample(100+t, flow=1000 if t < 3 else 2000) for _ in NODES])
            self.assertFalse(result['drained'])
        self.assertTrue(tracker.observe(6, [sample(106, flow=2000) for _ in NODES])['drained'])

    def test_missing_nan_reset_and_scrape_gap_do_not_count_as_quiet(self):
        for fault in ['missing', 'nan', 'reset', 'gap']:
            tracker = DrainTracker(10)
            for t in range(10):
                tracker.observe(t, [sample(100+t) for _ in NODES])
            nodes = [sample(110) for _ in NODES]
            if fault == 'missing':
                del nodes[1][FLOW[0]]
            elif fault == 'nan':
                nodes[1][COMMITTED] = float('nan')
            elif fault == 'reset':
                nodes[1][COMMITTED] = 0
            self.assertFalse(tracker.observe(20 if fault == 'gap' else 10, nodes)['drained'], fault)

    def test_parser_does_not_invent_zero_metrics(self):
        self.assertEqual(parse_metrics(''), {})
        self.assertEqual(parse_metrics(f'{MEMPOOL} NaN\n{COMMITTED} inf\n'), {})
        self.assertEqual(parse_metrics(f'{MEMPOOL} 0\n'), {MEMPOOL: 0.0})


def feed(t, commits=100, mempool=None, exec_queue=None, flow=1000):
    """A live oracle feed: native actions climb, the mempool holds 0..3 oracle
    chunks, the exec queue runs 0..2 behind commit; order counters stay put."""
    s = sample(commits + t, mempool=t % 4 if mempool is None else mempool,
               exec_queue=t % 3 if exec_queue is None else exec_queue, flow=flow)
    s[FLOW[2]] = 5000 + 3*t
    return s


class FeedLiveDrainTest(unittest.TestCase):
    """ORACLE_FEED_DRAIN=1: the feed keeps submitting through the drain, so
    native actions, the mempool and the exec queue never go quiet. Done = order
    counters quiet, mempool within the feed's own footprint, no flush or
    trade-writer work, exec lag <= max_lag, commits on every node."""

    def test_live_feed_drains_only_in_feed_live_mode(self):
        live, legacy = DrainTracker(10, feed_live=True, feed_mempool_max=6), DrainTracker(10)
        for t in range(10):
            nodes = [feed(t) for _ in NODES]
            self.assertFalse(legacy.observe(t, nodes)['drained'])
            self.assertFalse(live.observe(t, nodes)['drained'])
        nodes = [feed(10) for _ in NODES]
        self.assertFalse(legacy.observe(10, nodes)['drained'])
        self.assertTrue(live.observe(10, nodes)['drained'])

    def test_exec_lag_above_bound_restarts_the_window(self):
        for max_lag, drained in ((2, False), (4, True)):
            tracker = DrainTracker(10, feed_live=True, feed_mempool_max=6, max_lag=max_lag)
            results = [tracker.observe(t, [feed(t, exec_queue=3 if t == 5 else None)
                                           for _ in NODES]) for t in range(16)]
            self.assertEqual(results[-1]['drained'], drained, max_lag)

    def test_order_flow_mempool_flush_or_trade_writer_restart_the_window(self):
        for fault in ('flow', 'mempool', 'flush', 'trade_writer'):
            tracker = DrainTracker(10, feed_live=True, feed_mempool_max=6)
            for t in range(16):
                nodes = [feed(t) for _ in NODES]
                if t == 9:
                    if fault == 'flow':
                        nodes[1] = feed(t, flow=1001)
                    elif fault == 'mempool':
                        nodes[1][MEMPOOL] = 7
                    elif fault == 'flush':
                        nodes[1][FLUSH] = 1
                    else:
                        nodes[1][TRADE_WRITER] = 1
                self.assertFalse(tracker.observe(t, nodes)['drained'], (fault, t))

    def test_live_mode_still_needs_commit_progress_on_every_node(self):
        tracker = DrainTracker(10, feed_live=True, feed_mempool_max=6)
        for t in range(30):
            nodes = [feed(t) for _ in NODES]
            nodes[2][COMMITTED] = 100
            self.assertFalse(tracker.observe(t, nodes)['drained'])

    def test_summary_reports_max_lag_and_native_block_exec_ms(self):
        rows = [{'elapsed_s': t, 'node': 'val0', 'exec_lag': lag, 'chain_blocks': blocks,
                 'chain_ms': ms} for t, lag, blocks, ms in
                [(1, 9, 1, 999.0), (2, 1, 1, 40.0), (3, 2, 2, 50.0), (4, 0, 0, None),
                 (5, 1, 1, 60.0), (6, 2, 1, 200.0)]]
        out = feed_live_summary(rows, since=1)
        self.assertEqual(out['max_exec_lag'], 2)
        self.assertEqual(out['native_intervals'], 4)
        self.assertEqual(out['single_block_intervals'], 3)
        self.assertEqual(out['native_blocks'], 5)
        self.assertEqual(out['chain_ms'], {'p50': 50.0, 'p95': 200.0, 'max': 200.0})
        self.assertIsNone(feed_live_summary(rows, since=None)['max_exec_lag'])
        self.assertIsNone(feed_live_summary(rows, since=6)['chain_ms'])

    def test_sample_that_started_the_window_never_leaks_into_the_summary(self):
        # Review of 37ff2af: rows rounded elapsed to ms while tracker.since is
        # raw, so the window's first sample (its interval precedes the window:
        # the last bench block, 900 ms here) leaked in whenever rounding went up.
        def chain(count, secs):
            return [dict(feed(0), **{CHAIN_COUNT: count, CHAIN_SUM: secs}) for _ in NODES]
        for since in (5.0006, 5.0004, 5.0005, 7.123456789):
            tracker_rows = feed_live_rows(since - 1, [{}]*3, chain(10, 1.0), 0)
            tracker_rows += feed_live_rows(since, chain(10, 1.0), chain(11, 1.9), 0)
            for k in range(1, 4):
                tracker_rows += feed_live_rows(since + k, chain(10+k, 1.9 + 0.04*(k-1)),
                                               chain(11+k, 1.9 + 0.04*k), k)
            out = feed_live_summary(tracker_rows, since)
            self.assertEqual(out['chain_ms'], {'p50': 40.0, 'p95': 40.0, 'max': 40.0}, since)
            self.assertEqual((out['native_intervals'], out['native_blocks']), (9, 9), since)

    def test_parse_metrics_keeps_exec_chain_series_only_when_asked(self):
        text = f'{CHAIN_SUM} 1.5\n{CHAIN_COUNT} 3\n{MEMPOOL} 0\n'
        self.assertEqual(parse_metrics(text), {MEMPOOL: 0.0})
        self.assertEqual(parse_metrics(text, (CHAIN_SUM, CHAIN_COUNT)),
                         {MEMPOOL: 0.0, CHAIN_SUM: 1.5, CHAIN_COUNT: 3.0})


def histories(height=lambda t: t, backlog=lambda t: 1):
    return {n: [dict(sample(height(t), mempool=backlog(t)), ts=t) for t in range(101)] for n in NODES}


class LivenessTest(unittest.TestCase):
    def test_grow_stop_and_resume_is_rejected(self):
        data = histories(lambda t: t if t < 20 else 20 if t < 70 else t-50)
        result = assess_liveness(data, 0, 100)
        self.assertEqual(result['verdict'], 'FAIL')
        self.assertEqual(result['nodes']['val0']['stalls'][0]['seconds'], 50)
        self.assertGreater(data['val0'][-1][COMMITTED]-data['val0'][0][COMMITTED], 5)

    def test_terminal_stall_is_rejected(self):
        result = assess_liveness(histories(lambda t: min(t, 40)), 0, 100)
        self.assertEqual(result['verdict'], 'FAIL')

    def test_one_stalled_validator_rejects_the_fleet(self):
        data = histories()
        data['val2'] = histories(lambda t: min(t, 40))['val2']
        self.assertEqual(assess_liveness(data, 0, 100)['verdict'], 'FAIL')

    def test_short_pause_does_not_claim_a_stall(self):
        data = histories(lambda t: t if t < 20 else 20 if t < 30 else t-10)
        self.assertEqual(assess_liveness(data, 0, 100)['verdict'], 'PASS')

    def test_idle_chain_without_any_progress_is_not_healthy(self):
        self.assertEqual(assess_liveness(histories(lambda t: 1, lambda t: 0), 0, 100)['verdict'], 'FAIL')

    def test_empty_blocks_advancing_are_healthy(self):
        self.assertEqual(assess_liveness(histories(backlog=lambda t: 0), 0, 100)['verdict'], 'PASS')

    def test_data_gaps_missing_fields_resets_and_invalid_scrapes_are_unknown(self):
        for fault in ['gap', 'missing', 'reset', 'invalid_scrape', 'boundary', 'node']:
            data = histories()
            if fault == 'gap':
                data['val0'] = [r for r in data['val0'] if not 40 < r['ts'] < 60]
            elif fault == 'missing':
                del data['val0'][50][MEMPOOL]
            elif fault == 'reset':
                data['val0'][50][COMMITTED] = 0
            elif fault == 'invalid_scrape':
                data['val0'][50]['scrape_valid'] = 0
            elif fault == 'boundary':
                data['val0'] = data['val0'][10:]
            else:
                del data['val0']
            self.assertEqual(assess_liveness(data, 0, 100)['verdict'], 'UNKNOWN', fault)


class AcceptanceTest(unittest.TestCase):
    def test_agreement_is_insufficient(self):
        self.assertFalse(acceptance({'verdict': 'FAIL'}, True, 0, 'AGREE', True)['accepted'])
        self.assertFalse(acceptance({'verdict': 'UNKNOWN'}, True, 0, 'AGREE', True)['accepted'])
        self.assertFalse(acceptance({'verdict': 'PASS'}, False, 0, 'AGREE', True)['accepted'])
        self.assertFalse(acceptance({'verdict': 'PASS'}, True, 0, 'AGREE', False)['accepted'])
        self.assertFalse(acceptance({'verdict': 'PASS'}, True, 0, 'AGREE', None)['accepted'])
        self.assertTrue(acceptance({'verdict': 'PASS'}, True, 0, 'AGREE', True)['accepted'])

    def test_summary_preserves_equal_state_but_rejects_stall_and_exit_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            write_cell(directory, 1000, 1000, dur=120)
            write_agreement(directory, ['same']*3)
            path = Path(directory)/'sampler.csv'
            with path.open() as f:
                reader = csv.DictReader(f)
                fields = reader.fieldnames + [FLUSH]
                data = list(reader)
            for row in data:
                t = int(row['ts'])-BENCH_START
                row[COMMITTED] = str(10*min(t, 30))
                row[MEMPOOL] = '15000'
                row[FLUSH] = '0'
            with path.open('w') as f:
                writer = csv.DictWriter(f, fields)
                writer.writeheader()
                writer.writerows(data)
            summary, _ = run_summarize(directory, dur=120, extra=['--digest-quiescent', '1'])
            self.assertEqual(summary['headline']['agreement_verdict'], 'AGREE')
            self.assertEqual(summary['liveness']['verdict'], 'FAIL')
            self.assertEqual(summary['status'], 'INVALID')
            self.assertTrue(summary['timing']['drained_reported'])
            self.assertFalse(summary['timing']['drained'])
            result = subprocess.run(['python3', str(Path(__file__).with_name('health.py')),
                                     'accept', str(Path(directory)/'summary.json')])
            self.assertEqual(result.returncode, 2)

    def test_complete_clean_summary_accepts_and_partial_dissem_is_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            write_cell(directory, 1000, 1000, dur=120)
            write_agreement(directory, ['same']*3)
            path = Path(directory)/'sampler.csv'
            with path.open() as f:
                reader = csv.DictReader(f)
                fields, data = reader.fieldnames + [FLUSH], list(reader)
            for row in data:
                row[FLUSH] = '0'
            with path.open('w') as f:
                writer = csv.DictWriter(f, fields)
                writer.writeheader()
                writer.writerows(data)
            counts = 'exhausted=0,sync_fallback=0,da_outbound_fail=0,starvation=0'
            clean = ' '.join(n+':'+counts for n in NODES)
            summary, _ = run_summarize(directory, dur=120, extra=['--dissem', clean])
            self.assertEqual(summary['validity']['verdict'], 'ACCEPT')
            result = subprocess.run(['python3', str(Path(__file__).with_name('health.py')),
                                     'accept', str(Path(directory)/'summary.json')])
            self.assertEqual(result.returncode, 0)
            for partial in ['val0:'+counts, clean.replace('starvation=0', 'starvation=oops'),
                            clean.replace('starvation=0', 'starvation=-1')]:
                summary, _ = run_summarize(directory, dur=120, extra=['--dissem', partial])
                self.assertIsNone(summary['headline']['dissemination_clean'])
                self.assertEqual(summary['validity']['verdict'], 'UNVERIFIED')


class DrainHttpTest(unittest.TestCase):
    def test_cli_distinguishes_quiet_stalled_and_quiet_advancing_nodes(self):
        class Metrics(BaseHTTPRequestHandler):
            advancing = True
            counts = {}
            def log_message(self, *_args):
                pass
            def do_GET(self):
                self.counts[self.path] = self.counts.get(self.path, 100) + int(self.advancing)
                body = ''.join(f'{k} {v}\n' for k, v in sample(self.counts[self.path]).items()).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Metrics)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                command = ['python3', str(Path(__file__).with_name('health.py')), 'drain',
                           '--out', directory, '--timeout', '2', '--quiet', '0.5', '--urls',
                           *[f'http://127.0.0.1:{server.server_port}/{n}' for n in NODES]]
                result = subprocess.run(command, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(json.loads((Path(directory)/'drain.json').read_text())['drained'])
                Metrics.advancing = False
                result = subprocess.run(command, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(json.loads((Path(directory)/'drain.json').read_text())['drained'])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


class FeedLiveHttpTest(unittest.TestCase):
    def test_cli_drains_under_a_live_feed_and_records_exec_lag_and_chain_ms(self):
        class Metrics(BaseHTTPRequestHandler):
            ticks = {}
            def log_message(self, *_args):
                pass
            def do_GET(self):
                t = self.ticks[self.path] = self.ticks.get(self.path, 0) + 1
                # one oracle-only native block per scrape, 40 ms each
                values = dict(feed(t), **{CHAIN_SUM: 0.04*t, CHAIN_COUNT: t})
                body = ''.join(f'{k} {v}\n' for k, v in values.items()).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Metrics)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                command = ['python3', str(Path(__file__).with_name('health.py')), 'drain',
                           '--out', directory, '--timeout', '6', '--quiet', '2', '--urls',
                           *[f'http://127.0.0.1:{server.server_port}/{n}' for n in NODES]]
                legacy = subprocess.run(command, capture_output=True, text=True, timeout=20)
                self.assertEqual(legacy.returncode, 1, legacy.stderr)
                self.assertFalse((Path(directory)/'drain-feed-live.tsv').exists())
                self.assertNotIn('feed_live', json.loads((Path(directory)/'drain.json').read_text()))
                live = subprocess.run(command + ['--feed-live', '--feed-mempool-max', '6',
                                                 '--max-lag', '2'],
                                      capture_output=True, text=True, timeout=20)
                self.assertEqual(live.returncode, 0, live.stderr)
                drain = json.loads((Path(directory)/'drain.json').read_text())
                self.assertTrue(drain['drained'])
                summary = drain['feed_live']
                self.assertEqual((summary['max_lag_bound'], summary['feed_mempool_max']), (2, 6))
                self.assertLessEqual(summary['max_exec_lag'], 2)
                self.assertEqual(summary['chain_ms']['p50'], 40.0)
                self.assertEqual(summary['chain_ms']['max'], 40.0)
                lines = (Path(directory)/'drain-feed-live.tsv').read_text().splitlines()
                self.assertEqual(lines[0].split('\t'), ['elapsed_s', 'node', 'committed',
                                                        'exec_lag', 'mempool', 'chain_blocks',
                                                        'chain_ms', 'quiet_elapsed_s'])
                self.assertGreater(len(lines), 3*2)
                self.assertIn('feed_live', live.stdout)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


ATTEST_SUBMITTED = 'torus_state_hash_attestations_submitted_total'


def attest(t, commits=100, mempool=None, flow=1000, every=4):
    """STATE_HASH_ATTEST=1 after the load (9g-hn smoke 2026-10-21): idle blocks
    keep committing, each node attests every 100-block checkpoint (~4 s), so
    torus_native_actions_processed_total gains +3 every `every` samples and the
    native mempool holds 0..3 attestations; order counters stay put."""
    s = sample(commits + t, mempool=(t % 4) if mempool is None else mempool, exec_queue=t % 3,
               flow=flow)
    s[FLOW[2]] = 5000 + 3*(t // every)
    s[ATTEST_SUBMITTED] = 10 + t // every
    return s


class AttestLiveDrainTest(unittest.TestCase):
    """--attest-live: the validators' own AttestStateHash actions move the
    native counter and mempool forever; only the order counters must be quiet
    and the native mempool within the attestations' footprint."""

    def test_attestations_drain_only_in_attest_live_mode(self):
        live, legacy = DrainTracker(10, attest_live=True), DrainTracker(10)
        results = []
        for t in range(40):
            nodes = [attest(t) for _ in NODES]
            self.assertFalse(legacy.observe(t, nodes)['drained'], t)
            results.append(live.observe(t, nodes)['drained'])
        self.assertTrue(any(results))
        self.assertFalse(any(results[:10]))

    def test_moving_order_counters_never_drain_in_attest_live_mode(self):
        for key in ('torus_orders_placed_accepted_total', 'torus_orders_matched_total',
                    'torus_orders_resting_total'):
            tracker = DrainTracker(10, attest_live=True)
            for t in range(40):
                nodes = [attest(t) for _ in NODES]
                nodes[t % 3][key] = 1000 + t
                self.assertFalse(tracker.observe(t, nodes)['drained'], (key, t))

    def test_mempool_above_the_attest_footprint_or_pending_work_restarts_the_window(self):
        for fault in ('mempool', 'flush', 'trade_writer', 'exec_queue'):
            tracker = DrainTracker(10, attest_live=True, attest_mempool_max=6)
            for t in range(40):
                nodes = [attest(t) for _ in NODES]
                if t % 5 == 0:
                    if fault == 'mempool':
                        nodes[1][MEMPOOL] = 7
                    elif fault == 'flush':
                        nodes[1][FLUSH] = 1
                    elif fault == 'trade_writer':
                        nodes[1][TRADE_WRITER] = 1
                    else:
                        nodes[1][EXEC_QUEUE] = 3
                self.assertFalse(tracker.observe(t, nodes)['drained'], (fault, t))

    def test_attest_live_still_needs_commit_progress_on_every_node(self):
        tracker = DrainTracker(10, attest_live=True)
        for t in range(40):
            nodes = [attest(t) for _ in NODES]
            nodes[2][COMMITTED] = 100
            self.assertFalse(tracker.observe(t, nodes)['drained'])

    def test_feed_and_attest_live_add_their_mempool_footprints(self):
        both = DrainTracker(10, feed_live=True, feed_mempool_max=6, attest_live=True,
                            attest_mempool_max=6)
        feed_only = DrainTracker(10, feed_live=True, feed_mempool_max=6)
        for t in range(12):
            nodes = [attest(t, mempool=10) for _ in NODES]
            r_both, r_feed = both.observe(t, nodes), feed_only.observe(t, nodes)
        self.assertTrue(r_both['drained'])
        self.assertFalse(r_feed['drained'])

    def test_counter_reset_of_the_native_counter_is_still_a_reset(self):
        tracker = DrainTracker(10, attest_live=True)
        for t in range(10):
            tracker.observe(t, [attest(t) for _ in NODES])
        nodes = [attest(10) for _ in NODES]
        nodes[0][FLOW[2]] = 0
        self.assertEqual(tracker.observe(10, nodes)['reason'], 'counter reset')


class AttestLiveHttpTest(unittest.TestCase):
    def test_cli_drains_with_attestations_and_records_their_traffic(self):
        class Metrics(BaseHTTPRequestHandler):
            ticks = {}
            def log_message(self, *_args):
                pass
            def do_GET(self):
                t = self.ticks[self.path] = self.ticks.get(self.path, 0) + 1
                body = ''.join(f'{k} {v}\n' for k, v in attest(t, every=2).items()).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Metrics)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                command = ['python3', str(Path(__file__).with_name('health.py')), 'drain',
                           '--out', directory, '--timeout', '8', '--quiet', '3', '--urls',
                           *[f'http://127.0.0.1:{server.server_port}/{n}' for n in NODES]]
                legacy = subprocess.run(command, capture_output=True, text=True, timeout=30)
                self.assertEqual(legacy.returncode, 1, legacy.stderr)
                self.assertNotIn('attest_live', json.loads((Path(directory)/'drain.json').read_text()))
                live = subprocess.run(command + ['--attest-live', '--attest-mempool-max', '6'],
                                      capture_output=True, text=True, timeout=30)
                self.assertEqual(live.returncode, 0, live.stderr)
                drain = json.loads((Path(directory)/'drain.json').read_text())
                self.assertTrue(drain['drained'])
                info = drain['attest_live']
                self.assertEqual(info['attest_mempool_max'], 6)
                # the quiet window moved the native counter (attestations only)
                self.assertEqual(len(info['native_actions_delta']), 3)
                self.assertTrue(all(d > 0 for d in info['native_actions_delta']), info)
                self.assertGreater(info['attestations_submitted_delta'], 0)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_negative_attest_mempool_max_is_a_usage_error(self):
        r = subprocess.run(['python3', str(Path(__file__).with_name('health.py')), 'drain',
                            '--out', '/nonexistent', '--timeout', '1', '--attest-live',
                            '--attest-mempool-max', '-1', '--urls', 'a', 'b', 'c'],
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 2)


def hsample(height, queue=1, commits=None, flow=1000):
    """sample() plus the executed height (torus_block_height, set by the exec
    thread after each block); commits default to the height."""
    s = sample(height if commits is None else commits, exec_queue=queue, flow=flow)
    s[EXEC_HEIGHT] = height
    return s


class SettleToHeightTest(unittest.TestCase):
    """s64: the paused-feed settle (run-cell feed-stop-settle/) waits for a
    height, not a fixed 60 s: target = the highest committed height across the
    nodes at feed stop (executed + exec queue); done once every node has
    executed it and the chain is quiet; rc != 0 if not within the max."""

    def test_target_is_the_highest_committed_height_across_nodes(self):
        nodes = [hsample(500, queue=60), hsample(530, queue=2), hsample(520, queue=0)]
        self.assertEqual(height_target(nodes), 560)
        self.assertEqual(committed_height(nodes[1]), 532)

    def test_target_needs_a_complete_trio(self):
        nodes = [hsample(500), hsample(510), hsample(505)]
        del nodes[1][EXEC_HEIGHT]
        self.assertIsNone(height_target(nodes))
        self.assertIsNone(height_target(nodes[:2]))
        nodes = [hsample(500), hsample(510), dict(hsample(505), scrape_valid=0)]
        self.assertIsNone(height_target(nodes))

    def test_keeps_waiting_while_any_node_is_behind_the_target(self):
        tracker = DrainTracker(10, min_height=300)
        # quiet and advancing for far longer than the quiet window, but val1
        # executes 1 block/s from 200: below the target until t=100
        for t in range(100):
            nodes = [hsample(300 + t), hsample(200 + t), hsample(300 + t)]
            r = tracker.observe(t, nodes)
            self.assertFalse(r['drained'], t)
        self.assertIn('height 300', r['reason'])
        self.assertIn('val1', r['reason'])

    def test_drains_once_every_node_reached_the_target_and_is_quiet(self):
        tracker = DrainTracker(10, min_height=300)
        results = [tracker.observe(t, [hsample(290 + t) for _ in NODES]) for t in range(30)]
        drained = [t for t, r in enumerate(results) if r['drained']]
        # heights reach 300 at t=10; quiet since t=0, so done right then
        self.assertEqual(drained[0], 10)

    def test_reaching_the_target_alone_is_not_enough(self):
        tracker = DrainTracker(10, min_height=100)
        for t in range(30):
            # past the target but the exec queue is still deep: not quiet
            self.assertFalse(tracker.observe(t, [hsample(200 + t, queue=40) for _ in NODES])['drained'])

    def test_missing_height_metric_never_counts_as_reached(self):
        tracker = DrainTracker(10, min_height=100)
        for t in range(30):
            nodes = [hsample(200 + t) for _ in NODES]
            del nodes[2][EXEC_HEIGHT]
            self.assertFalse(tracker.observe(t, nodes)['drained'])

    def test_without_a_target_the_drain_is_unchanged(self):
        tracker = DrainTracker(10)
        results = [tracker.observe(t, [sample(100 + t) for _ in NODES]) for t in range(12)]
        self.assertTrue(results[10]['drained'])

    def test_parse_metrics_keeps_the_exec_height_only_when_asked(self):
        text = f'{EXEC_HEIGHT} 42\n'
        self.assertNotIn(EXEC_HEIGHT, parse_metrics(text))
        self.assertEqual(parse_metrics(text, (EXEC_HEIGHT,))[EXEC_HEIGHT], 42.0)


class SettleToHeightHttpTest(unittest.TestCase):
    """The CLI end to end: `drain --to-height auto` records the target at the
    first complete scrape and drains (rc 0) once it is executed; a target the
    chain cannot reach within --timeout fails with rc 3 and says so."""

    def serve(self):
        class Metrics(BaseHTTPRequestHandler):
            counts = {}
            def log_message(self, *_args):
                pass
            def do_GET(self):
                # executed height +1 per scrape from 100 (val1 from 90), queue 2
                h = self.counts[self.path] = self.counts.get(
                    self.path, 90 if self.path.endswith('val1') else 100) + 1
                body = ''.join(f'{k} {v}\n' for k, v in hsample(h, queue=2).items()).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Metrics)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def stop():
            server.shutdown()
            server.server_close()
            thread.join()
        self.addCleanup(stop)
        return [f'http://127.0.0.1:{server.server_port}/{n}' for n in NODES]

    def run_drain(self, directory, urls, to_height, timeout):
        command = ['python3', str(Path(__file__).with_name('health.py')), 'drain',
                   '--out', directory, '--timeout', str(timeout), '--quiet', '0.5',
                   '--to-height', to_height, '--urls', *urls]
        result = subprocess.run(command, capture_output=True, text=True, timeout=60)
        return result, json.loads((Path(directory)/'drain.json').read_text())

    def test_auto_target_is_reached_and_recorded(self):
        urls = self.serve()
        with tempfile.TemporaryDirectory() as directory:
            result, drain = self.run_drain(directory, urls, 'auto', 30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(drain['drained'])
            # first scrape: val0/val2 executed 101 + queue 2 -> committed 103
            self.assertEqual(drain['target_height'], 103)
            self.assertTrue(drain['target_reached'])
            self.assertEqual(sorted(drain['target_committed_by_node']), list(NODES))
            # val1 started 10 behind: it had to execute up to 103 first
            self.assertGreaterEqual(min(drain['exec_heights'].values()), 103)

    def test_unreachable_target_fails_with_rc_3_after_the_max(self):
        urls = self.serve()
        with tempfile.TemporaryDirectory() as directory:
            result, drain = self.run_drain(directory, urls, '100000', 2)
            self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
            self.assertFalse(drain['drained'])
            self.assertFalse(drain['target_reached'])
            self.assertEqual(drain['target_height'], 100000)
            self.assertIn('height target 100000 not reached', drain['reason'])
            self.assertGreaterEqual(drain['elapsed_s'], 2)

    def test_bad_to_height_is_a_usage_error(self):
        for bad in ('x', '-1', '0'):
            r = subprocess.run(['python3', str(Path(__file__).with_name('health.py')), 'drain',
                                '--out', '/nonexistent', '--timeout', '1', '--to-height', bad,
                                '--urls', 'a', 'b', 'c'], capture_output=True, text=True, timeout=30)
            self.assertEqual(r.returncode, 2, bad)
            self.assertIn('--to-height', r.stderr)


class CollectionTest(unittest.TestCase):
    def test_streaming_log_counts_keep_line_semantics_and_ansi(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'node.log'
            path.write_bytes(b'\x1b[32mFULL-BODY push\x1b[0m bytes=9 bytes=19\n'
                             b'FULL-BODY push bytes=13\n'
                             b'body fetch exhausted; body fetch exhausted; falling back to sync\n'
                             b'body fetch has no remaining targets\n'
                             b'block-data OUTBOUND FAILURE\n'
                             b'header-first body starvation\n'
                             b'Send Queue full '+b'x'*1000000+b'\n')
            result = collect(path)
            self.assertEqual(result['counts']['body_push'], 2)
            self.assertEqual(result['counts']['body_push_max_bytes'], 19)
            self.assertEqual(result['counts']['exhausted'], 2)
            self.assertEqual(result['counts']['sync_fallback'], 1)
            self.assertEqual(result['counts']['da_outbound_fail'], 1)
            self.assertEqual(result['counts']['starvation'], 1)
            self.assertEqual(result['send_queue_full_lines'], 1)
            self.assertGreater(result['send_queue_full_bytes'], 1000000)

    def test_sampler_marks_missing_metrics_instead_of_silent_zero_health(self):
        from sample_metrics import parse_metrics as parse_sampler_metrics
        values = sample()
        text = ''.join(f'{k} {v}\n' for k, v in values.items())
        good, _ = parse_sampler_metrics(text, set())
        bad, _ = parse_sampler_metrics('', set())
        self.assertEqual(good['scrape_valid'], '1')
        self.assertEqual(bad['scrape_valid'], '0')
        self.assertEqual({k: float(good[k]) for k in values}, values)
        self.assertTrue(all(k not in bad for k in values))



if __name__ == '__main__':
    unittest.main()
