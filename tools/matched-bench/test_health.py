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

from health import (COMMITTED, MEMPOOL, EXEC_QUEUE, FLUSH, FLOW, NODES,
                    DrainTracker, acceptance, assess_liveness, parse_metrics)
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
        script = Path(__file__).with_name('run-cell.sh').read_text()
        function = script[script.index('extract() {'):script.index('# bl1 exec-chain-sub-100-attribution: histogram BUCKET')]
        values = sample()
        columns = ' '.join(values) + ' scrape_valid'
        command = function + '\nextract "$1"\n'
        text = ''.join(f'{k} {v}\n' for k, v in values.items())
        good = subprocess.run(['bash', '-c', command, 'test', columns], input=text, text=True, capture_output=True, check=True)
        bad = subprocess.run(['bash', '-c', command, 'test', columns], input='', text=True, capture_output=True, check=True)
        self.assertEqual(good.stdout.strip().split(',')[-1], '1')
        self.assertEqual(bad.stdout.strip().split(',')[-1], '0')


if __name__ == '__main__':
    unittest.main()
