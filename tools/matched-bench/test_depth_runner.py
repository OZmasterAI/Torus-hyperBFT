#!/usr/bin/env python3
"""Execute the actual runner hook/helpers with a harmless mock observer."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

MOCK = r'''
import json, os, pathlib, signal, sys, time
args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
out = pathlib.Path(args['--out'])
out.mkdir()
def stopped(*_):
    (out / 'manifest.json').write_text(json.dumps({'status':'partial'}))
    raise SystemExit(2)
signal.signal(signal.SIGTERM, stopped)
(out.parent / 'invocation.json').write_text(json.dumps({
    'args':args, 'env':os.environ.get('DEPTH_OBSERVER'), 'pid':os.getpid()}))
if os.environ.get('MOCK_BLOCK') == '1':
    time.sleep(20)
code = int(os.environ.get('MOCK_RC', '0'))
(out / 'manifest.json').write_text(json.dumps({'status':'complete' if code == 0 else 'partial'}))
raise SystemExit(code)
'''


class DepthRunnerTests(unittest.TestCase):
    def run_hook(self, enabled=None, duration=300, rc=0, finish='wait'):
        source = Path(__file__).with_name('run-cell.sh').read_text()
        # Extract actual source through finish_fail, including its embedded Python.
        helpers = source[source.index('start_depth_observer() {\n'):
                         source.index("trap 'log \"interrupted\"; finish_fail; exit 130' INT TERM")]
        default = next(line for line in source.splitlines() if line.startswith('DEPTH_OBSERVER=${'))
        exit_trap = next(line for line in source.splitlines() if line.startswith("trap 'finish_depth_observer"))
        # The real launch location ties this to T_BENCH0 and starts the generator
        # before the optional observer, without executing the real generator.
        hook = source[source.index('T_BENCH0=$(date +%s)\n'):
                      source.index('cpusampler & CPU_PID=$!')]
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / 'sample_depth.py').write_text(MOCK)
            stopper = out / 'stop-3val.sh'
            stopper.write_text('#!/usr/bin/env bash\nexit 0\n')
            stopper.chmod(0o755)
            command = '''set -uo pipefail
SELF_DIR="$FIXTURE"
OUT="$FIXTURE"
WSL="$FIXTURE"
RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)
MARKETS=50
DUR="$MOCK_DURATION"
DEPTH_PID=""; CPU_PID=""; BENCH_PID=""; CRASH_PID=""
BENCH_CMD=(true)
log() { :; }
stop_sampler() { :; }
date() { printf '1700000000\\n'; }
''' + default + '\n' + helpers + '\n' + exit_trap + '\n' + hook
            command += '\nwait "$BENCH_PID"\nBENCH_PID=""\n'
            if enabled == 1:
                command += '''for attempt in {1..200}; do
    [ -s "$OUT/invocation.json" ] && break
    sleep 0.01
done
[ -s "$OUT/invocation.json" ] || exit 99
'''
            if finish == 'exit':
                command += 'exit 7\n'
            elif finish == 'failure':
                command += 'finish_fail\n[ -z "$DEPTH_PID" ] || exit 98\n'
            else:
                command += 'finish_depth_observer wait\n[ -z "$DEPTH_PID" ] || exit 98\n'
            environment = {**os.environ, 'FIXTURE': directory, 'MOCK_DURATION': str(duration),
                           'MOCK_RC': str(rc), 'MOCK_BLOCK': '1' if finish in ('failure', 'exit') else '0'}
            environment.pop('DEPTH_OBSERVER', None)
            if enabled is not None:
                environment['DEPTH_OBSERVER'] = str(enabled)
            result = subprocess.run(['bash', '-c', command], env=environment,
                                    capture_output=True, text=True, timeout=8)
            self.assertEqual(result.returncode, 7 if finish == 'exit' else 0, result.stderr)
            files = {file.name: json.loads(file.read_text()) for file in out.glob('*.json')}
            if 'invocation.json' in files:
                with self.assertRaises(ProcessLookupError):
                    os.kill(files['invocation.json']['pid'], 0)
            return files

    def test_default_off_records_provenance_without_launch(self):
        files = self.run_hook()
        self.assertFalse(files['depth-observer.json']['enabled'])
        self.assertNotIn('invocation.json', files)
        self.assertNotIn('depth-observer-exit.json', files)

    def test_enabled_uses_declared_start_val1_parent_and_nominal_offsets(self):
        for duration, offsets in ((300, '0,100,200,end'), (200, '0,100,end'), (120, '0,100,end')):
            with self.subTest(duration=duration):
                files = self.run_hook(enabled=1, duration=duration)
                provenance, invocation = files['depth-observer.json'], files['invocation.json']
                self.assertTrue(provenance['enabled'])
                self.assertFalse(provenance['affects_acceptance'])
                self.assertEqual(provenance['nominal_end_unix'], 1700000000 + duration)
                self.assertEqual(invocation['env'], '1')
                self.assertEqual(invocation['args']['--url'], 'http://127.0.0.1:8646')
                self.assertEqual(invocation['args']['--start-unix'], '1700000000')
                self.assertEqual(invocation['args']['--duration'], str(duration))
                self.assertEqual(invocation['args']['--offsets'], offsets)
                self.assertEqual(invocation['args']['--markets'], '50')
                self.assertEqual(int(invocation['args']['--parent-pid']), provenance['parent_pid'])
                self.assertTrue(invocation['args']['--out'].endswith('/depth'))
                self.assertEqual(files['depth-observer-exit.json']['exit_code'], 0)

    def test_partial_observer_exit_is_recorded_without_failing_cell(self):
        files = self.run_hook(enabled=1, rc=2)
        self.assertEqual(files['depth-observer-exit.json']['exit_code'], 2)
        self.assertFalse(files['depth-observer-exit.json']['affects_acceptance'])

    def test_actual_failure_cleanup_stops_and_reaps_observer(self):
        files = self.run_hook(enabled=1, finish='failure')
        self.assertEqual(files['depth-observer-exit.json']['finish_mode'], 'stop')
        self.assertEqual(files['depth-observer-exit.json']['exit_code'], 2)
        self.assertEqual(files['summary.json']['status'], 'FAILED')

    def test_exit_trap_stops_observer_and_preserves_original_exit_code(self):
        files = self.run_hook(enabled=1, finish='exit')
        self.assertEqual(files['depth-observer-exit.json']['finish_mode'], 'stop')
        self.assertEqual(files['depth-observer-exit.json']['exit_code'], 2)


if __name__ == '__main__':
    unittest.main()
