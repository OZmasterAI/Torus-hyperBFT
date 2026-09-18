#!/usr/bin/env python3
"""Execute the runner's real snapshot functions and quiescence decision in Bash."""
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import unittest

COUNTERS = ('torus_orders_placed_accepted_total', 'torus_orders_matched_total',
            'torus_native_actions_processed_total', 'torus_orders_resting_total')
GOOD = ''.join(f'{name} {i + 1}\n' for i, name in enumerate(COUNTERS))


class FunnelSnapshotTests(unittest.TestCase):
    def run_snapshot_pair(self, overrides=None, failed=(), remove_helper=None):
        script = Path(__file__).with_name('run-cell.sh').read_text()
        functions = script[script.index('funnel_counters() {\n'):
                           script.index('declare -a DHGT DIGSHA DIGSECS\n')]
        # These are the actual production capture/status and comparison lines;
        # do not duplicate the condition and accidentally test a safer version.
        def line(prefix):
            return next(item for item in script.splitlines() if item.startswith(prefix))
        before = line('Q_BEFORE_OK=') + '\n' + line('if Q_BEFORE=')
        after = line('Q_AFTER_OK=') + '\n' + line('if Q_AFTER=')
        compare = line('if [ "$Q_BEFORE_OK"')
        with tempfile.TemporaryDirectory() as directory:
            for phase in ('before', 'after'):
                for node in ('0', '1', '2'):
                    key = phase + '-' + node
                    Path(directory, key).write_text((overrides or {}).get(key, GOOD))
                    if key in failed:
                        Path(directory, key + '.fail').touch()
            command = '''set -uo pipefail
METS=(0 1 2)
scrape_one() {
    cat "$FIXTURES/$PHASE-$1"
    [ ! -e "$FIXTURES/$PHASE-$1.fail" ]
}
''' + functions
            if remove_helper:
                command += f'\nunset -f {remove_helper}\n'
            command += '\nPHASE=before\n' + before + '\nPHASE=after\n' + after + '\n' + compare
            command += '\nprintf "%s %s %s\\n" "$DIGEST_QUIESCENT" "$Q_BEFORE_OK" "$Q_AFTER_OK"\n'
            result = subprocess.run(['bash', '-c', command],
                                    env={**os.environ, 'FIXTURES': directory},
                                    capture_output=True, text=True, timeout=10, check=True)
            return result.stdout.strip()

    def test_complete_equal_snapshots_are_quiescent_including_real_zero_counters(self):
        self.assertEqual(self.run_snapshot_pair(), '1 1 1')
        zero = ''.join(f'{name} 0\n' for name in COUNTERS)
        self.assertEqual(self.run_snapshot_pair(
            {f'{phase}-{node}': zero for phase in ('before', 'after') for node in range(3)}), '1 1 1')

    def test_changed_complete_snapshot_is_not_quiescent(self):
        self.assertEqual(self.run_snapshot_pair({'after-1': GOOD + 'unrelated_metric 9\n'}), '1 1 1')
        changed = GOOD.replace(COUNTERS[0] + ' 1', COUNTERS[0] + ' 5')
        self.assertEqual(self.run_snapshot_pair({'after-1': changed}), '0 1 1')

    def test_empty_missing_malformed_and_duplicate_counters_are_unavailable(self):
        bad_bodies = ['', '\n'.join(GOOD.splitlines()[1:]), GOOD + COUNTERS[0] + '\n',
                      GOOD + COUNTERS[0] + ' 1\n']
        for value in ('NaN', '+Inf', '-1', '1e999', 'garbage'):
            bad_bodies.append(GOOD.replace(COUNTERS[0] + ' 1', COUNTERS[0] + ' ' + value))
        for body in bad_bodies:
            with self.subTest(body=body):
                self.assertEqual(self.run_snapshot_pair({'before-0': body, 'after-0': body}), '0 0 0')

    def test_failed_http_with_complete_body_is_unavailable_for_each_node(self):
        for node in range(3):
            with self.subTest(node=node):
                self.assertEqual(self.run_snapshot_pair(failed=(f'before-{node}',)), '0 0 1')
                self.assertEqual(self.run_snapshot_pair(failed=(f'after-{node}',)), '0 1 0')
        self.assertEqual(self.run_snapshot_pair(failed=('before-0', 'after-0')), '0 0 0')

    def test_real_scrape_helper_rejects_http_error_with_valid_looking_body(self):
        class Metrics(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.send_response(503)
                self.end_headers()
                self.wfile.write(GOOD.encode())

        server = ThreadingHTTPServer(('127.0.0.1', 0), Metrics)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            script = Path(__file__).with_name('run-cell.sh').read_text()
            helper = next(line for line in script.splitlines() if line.startswith('scrape_one() {'))
            result = subprocess.run(['bash', '-c', helper + '\nscrape_one "$1"',
                                     'test', str(server.server_port)],
                                    capture_output=True, text=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, '')
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_missing_parser_or_snapshot_helper_cannot_prove_quiescence(self):
        for helper in ('funnel_counters', 'funnel_snapshot'):
            with self.subTest(helper=helper):
                self.assertEqual(self.run_snapshot_pair(remove_helper=helper), '0 0 0')


if __name__ == '__main__':
    unittest.main()
