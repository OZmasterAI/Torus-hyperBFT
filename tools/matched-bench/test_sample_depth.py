#!/usr/bin/env python3
"""Bounded observer fixtures; local HTTP and synthetic schedules only."""
import asyncio
import json
import os
import signal
import subprocess
import sys
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import sample_depth as depth


def config(out, **overrides):
    values = dict(out=out, url='http://127.0.0.1:1', markets=2,
                  start_unix=time.time(), duration=1, offsets=[0], parent_pid=os.getpid(),
                  request_timeout=0.2, snapshot_budget=1, max_response_bytes=4096,
                  max_output_bytes=65536, max_levels=5)
    values.update(overrides)
    return SimpleNamespace(**values)


def result(value):
    return dict(status='ok', error=None, started_wall=time.time(), completed_wall=time.time(),
                request_seconds=0.01, response_bytes=10, result=value)


def book(market, count=0):
    return {'marketId': hex(market), 'bids': [] if count == 0 else
            [{'price': '0x10', 'quantity': '0x20', 'orderCount': count}], 'asks': []}


async def successful_rpc(_url, method, params, *_):
    return result('0x10' if method == 'eth_blockNumber' else book(int(params[0], 16), 7 if params[0] == '0x1' else 0))


class SnapshotTests(unittest.IsolatedAsyncioTestCase):
    async def test_counts_empty_markets_timing_and_height_brackets(self):
        args = config(Path('/unused'))
        with patch.object(depth, 'rpc', successful_rpc):
            value = await depth.snapshot(args, 0, time.monotonic())
        self.assertEqual(value['status'], 'complete')
        self.assertEqual([v['market_id'] for v in value['markets']], [1, 2])
        self.assertEqual([v['result']['resting_orders'] for v in value['markets']], [7, 0])
        self.assertEqual(value['markets'][1]['result']['bids'], [])
        self.assertEqual(value['height_before']['result'], '0x10')
        self.assertEqual(value['height_after']['result'], '0x10')
        self.assertGreaterEqual(value['completed_wall'], value['started_wall'])
        self.assertEqual(value['markets'][0]['request_seconds'], 0.01)

    async def test_expired_snapshot_is_missing_not_zero_and_issues_no_rpc(self):
        with patch.object(depth, 'rpc') as call:
            value = await depth.snapshot(config(Path('/unused')), 0, time.monotonic() - 2)
        call.assert_not_called()
        self.assertEqual(value['status'], 'partial')
        self.assertTrue(all(v['status'] == 'missing' and v['result'] is None for v in value['markets']))

    async def test_one_bad_market_or_height_keeps_other_raw_evidence(self):
        for fault in ('market', 'height'):
            async def partial(url, method, params, *bounds):
                if (fault == 'market' and params == ['0x2']) or (fault == 'height' and method == 'eth_blockNumber'):
                    return depth.missing('fixture unavailable')
                return await successful_rpc(url, method, params, *bounds)
            with patch.object(depth, 'rpc', partial):
                value = await depth.snapshot(config(Path('/unused')), 0, time.monotonic())
            self.assertEqual(value['status'], 'partial')
            self.assertEqual(value['markets'][0]['result']['resting_orders'], 7)

    async def test_regressing_height_is_partial(self):
        heights = iter(['0x10', '0xf'])
        async def regressing(url, method, params, *bounds):
            if method == 'eth_blockNumber':
                return result(next(heights))
            return await successful_rpc(url, method, params, *bounds)
        with patch.object(depth, 'rpc', regressing):
            value = await depth.snapshot(config(Path('/unused')), 0, time.monotonic())
        self.assertEqual(value['status'], 'partial')
        self.assertTrue(value['height_regressed'])

    async def test_manifest_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            args = config(Path(directory) / 'new')
            with patch.object(depth, 'rpc', successful_rpc):
                self.assertEqual(await depth.observe(args), 0)
                with self.assertRaises(FileExistsError):
                    await depth.observe(args)
            manifest = json.loads((args.out / 'manifest.json').read_text())
            self.assertEqual(manifest['status'], 'complete')
            self.assertEqual(manifest['snapshots_written'], 1)
            self.assertFalse(manifest['atomic'])
            self.assertEqual(manifest['output_bytes'], (args.out / 'depth.jsonl').stat().st_size)

    async def test_output_limit_is_reported_without_partial_json_record(self):
        with tempfile.TemporaryDirectory() as directory:
            args = config(Path(directory) / 'new', max_output_bytes=1)
            with patch.object(depth, 'rpc', successful_rpc):
                self.assertEqual(await depth.observe(args), 2)
            self.assertEqual((args.out / 'depth.jsonl').read_bytes(), b'')
            self.assertIn('output byte limit', json.loads((args.out / 'manifest.json').read_text())['error'])

    async def test_parent_exit_stops_future_schedule(self):
        with tempfile.TemporaryDirectory() as directory:
            args = config(Path(directory) / 'new', start_unix=time.time() + 30)
            with patch.object(depth, 'parent_identity', side_effect=['original', 'replacement']):
                self.assertEqual(await asyncio.wait_for(depth.observe(args), 2), 2)
            manifest = json.loads((args.out / 'manifest.json').read_text())
            self.assertEqual(manifest['stop_reason'], 'parent exited')
            self.assertEqual(manifest['snapshots_written'], 0)


class ShapeTests(unittest.TestCase):
    def test_cli_is_disabled_by_default_without_output_or_required_arguments(self):
        environment = dict(os.environ)
        environment.pop('DEPTH_OBSERVER', None)
        result = subprocess.run([sys.executable, str(Path(depth.__file__))], env=environment,
                                capture_output=True, text=True, timeout=3)
        self.assertEqual(result.returncode, 0)
        self.assertIn('disabled', result.stderr)

    def test_invalid_book_fields_and_oversized_levels_are_not_empty_books(self):
        cases = [{}, book(2), {**book(1), 'asks': None}]
        for count in (-1, True, 1.5, '7'):
            value = book(1, 1)
            value['bids'][0]['orderCount'] = count
            cases.append(value)
        value = book(1, 1)
        value['bids'] *= 6
        cases.append(value)
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                depth.book_result(value, 1, 5)


class RpcTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.connections = set()
        self.entered = asyncio.Event()
        self.server = await asyncio.start_server(self.handle, '127.0.0.1', 0)
        self.base = f'http://127.0.0.1:{self.server.sockets[0].getsockname()[1]}'

    async def asyncTearDown(self):
        self.server.close()
        pending = tuple(self.connections)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pending, return_exceptions=True)
        await asyncio.wait_for(self.server.wait_closed(), 2)

    async def handle(self, reader, writer):
        task = asyncio.current_task()
        self.connections.add(task)
        try:
            headers = await reader.readuntil(b'\r\n\r\n')
            route = headers.split()[1]
            length = next(int(line.split(b':')[1]) for line in headers.split(b'\r\n') if line.lower().startswith(b'content-length:'))
            body = json.loads(await reader.readexactly(length))
            self.assertEqual(body['method'], 'eth_blockNumber')
            self.entered.set()
            if route == b'/blocked':
                await asyncio.Event().wait()
            payload = json.dumps({'jsonrpc': '2.0', 'id': 1, 'result': '0x20'}).encode()
            if route == b'/large':
                payload = b'x' * 4096
            elif route == b'/bad':
                payload = b'{'
            status = b'503 Unavailable' if route == b'/error' else b'200 OK'
            writer.write(b'HTTP/1.1 ' + status + b'\r\nContent-Length: ' + str(len(payload)).encode() + b'\r\nConnection: close\r\n\r\n' + payload)
            await writer.drain()
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except ConnectionError:
                pass
            self.connections.discard(task)

    async def test_response_bounds_http_errors_parse_errors_and_timeout(self):
        good = await depth.rpc(self.base + '/ok', 'eth_blockNumber', [], 1, 128)
        self.assertEqual(good['status'], 'ok')
        self.assertEqual(good['result'], '0x20')
        self.assertGreaterEqual(good['completed_wall'], good['started_wall'])
        for route in ('large', 'bad', 'error', 'blocked'):
            with self.subTest(route=route):
                bad = await depth.rpc(self.base + '/' + route, 'eth_blockNumber', [], 0.1, 128)
                self.assertEqual(bad['status'], 'missing')
                self.assertIsNone(bad['result'])
                self.assertLess(bad['request_seconds'], 2)

    async def test_cli_sigterm_records_partial_manifest_and_reaps_curl(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'observations'
            process = await asyncio.create_subprocess_exec(
                sys.executable, str(Path(depth.__file__)), '--url', self.base + '/blocked',
                '--markets', '2', '--start-unix', str(time.time()), '--duration', '1',
                '--offsets', '0,end', '--request-timeout', '10', '--out', str(out),
                '--parent-pid', str(os.getpid()), stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE, env={**os.environ, 'DEPTH_OBSERVER': '1'})
            try:
                await asyncio.wait_for(self.entered.wait(), 3)
                children = [int(pid) for pid in Path(
                    f'/proc/{process.pid}/task/{process.pid}/children').read_text().split()]
                self.assertEqual(len(children), 1)
                process.send_signal(signal.SIGTERM)
                _, stderr = await asyncio.wait_for(process.communicate(), 3)
                self.assertEqual(process.returncode, 2, stderr.decode())
                manifest = json.loads((out / 'manifest.json').read_text())
                self.assertEqual(manifest['status'], 'partial')
                self.assertEqual(manifest['stop_reason'], 'SIGTERM')
                record = json.loads((out / 'depth.jsonl').read_text())
                self.assertTrue(record['interrupted'])
                self.assertEqual(len(record['markets']), 2)
                self.assertTrue(all(value['status'] == 'missing' for value in record['markets']))
                for pid in children:
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid, 0)
            finally:
                await depth.terminate_and_reap(process)

    async def test_cancel_reaps_request_child(self):
        processes = []
        original = asyncio.create_subprocess_exec
        async def tracked(*args, **kwargs):
            process = await original(*args, **kwargs)
            processes.append(process)
            return process
        with patch.object(depth.asyncio, 'create_subprocess_exec', tracked):
            task = asyncio.create_task(depth.rpc(self.base + '/blocked', 'eth_blockNumber', [], 10, 128))
            await asyncio.wait_for(self.entered.wait(), 2)
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(task, 2)
        self.assertEqual(len(processes), 1)
        self.assertIsNotNone(processes[0].returncode)
        with self.assertRaises(ProcessLookupError):
            os.kill(processes[0].pid, 0)


if __name__ == '__main__':
    unittest.main()
