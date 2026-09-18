#!/usr/bin/env python3
"""Opt-in, non-atomic depth observations on one validator (never launched by default).

Example: DEPTH_OBSERVER=1 python3 sample_depth.py --url http://127.0.0.1:8645 \
  --markets 10 --start-unix <declared-load-start> --duration 300 \
  --out <fresh-directory> --parent-pid <runner-pid>

Caller launches this separately only when DEPTH_OBSERVER=1, supplies the actual
measurement start (not process-launch time), and waits for it when stopping.
SIGTERM/SIGINT or parent exit cancels and reaps in-flight curl. Observing RPCs
costs work; height brackets describe a moving interval, never an atomic snapshot.
"""
import argparse
import asyncio
import json
import math
import os
from pathlib import Path
import re
import signal
import sys
import time
from urllib.parse import urlsplit

from sample_metrics import terminate_and_reap


def parent_identity(pid):
    try:
        # Include starttime so PID reuse cannot prolong this observer's life.
        fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return None if fields[0] in ('Z', 'X') else fields[19]
    except (OSError, IndexError):
        return None


def book_result(value, market, max_levels):
    if not isinstance(value, dict) or value.get('marketId') != hex(market):
        raise ValueError('missing or mismatched marketId')
    result = {}
    for side in ('bids', 'asks'):
        levels = value.get(side)
        if not isinstance(levels, list) or len(levels) > max_levels:
            raise ValueError('missing levels or level limit exceeded')
        result[side] = []
        for level in levels:
            if not isinstance(level, dict):
                raise ValueError('invalid level')
            count = level.get('orderCount')
            if type(count) is not int or not 0 <= count <= 0xffffffff:
                raise ValueError('invalid orderCount')
            for name in ('price', 'quantity'):
                if not isinstance(level.get(name), str) or not re.fullmatch(r'0x[0-9a-fA-F]{1,64}', level[name]):
                    raise ValueError('invalid price/quantity')
            result[side].append({name: level[name] for name in ('price', 'quantity', 'orderCount')})
    result['resting_orders'] = sum(level['orderCount'] for side in ('bids', 'asks') for level in result[side])
    return result


async def rpc(url, method, params, timeout, max_bytes):
    wall, mono = time.time(), time.monotonic()
    process, error, result, size = None, None, None, 0
    creation = asyncio.create_task(asyncio.create_subprocess_exec(
        'curl', '-q', '-fsS', '--max-time', str(timeout), '--connect-timeout', str(timeout),
        '-H', 'content-type: application/json', '--data-binary',
        json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}), url,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL))
    try:
        process = await asyncio.shield(creation)
        async def read():
            nonlocal size
            chunks = []
            while True:
                chunk = await process.stdout.read(min(16384, max_bytes + 1 - size))
                if not chunk:
                    break
                size += len(chunk)
                if size > max_bytes:
                    raise ValueError('response byte limit exceeded')
                chunks.append(chunk)
            await process.wait()
            return b''.join(chunks)
        encoded = await asyncio.wait_for(read(), timeout)
        if process.returncode != 0:
            raise ValueError(f'curl exit {process.returncode}')
        payload = json.loads(encoded)
        if not isinstance(payload, dict) or payload.get('id') != 1 or payload.get('jsonrpc') != '2.0' or 'error' in payload or 'result' not in payload:
            raise ValueError('invalid RPC envelope or RPC error')
        result = payload['result']
    except asyncio.CancelledError:
        if process is None:
            try:
                process = await creation
            except OSError:
                pass
        raise
    except (OSError, ValueError, asyncio.TimeoutError) as exc:
        error = f'{type(exc).__name__}: {exc}'[:256]
    finally:
        if process is not None:
            await terminate_and_reap(process)
    return {'status': 'ok' if error is None else 'missing', 'error': error,
            'started_wall': wall, 'completed_wall': time.time(),
            'request_seconds': time.monotonic() - mono, 'response_bytes': size,
            'result': result}


def missing(reason):
    return {'status': 'missing', 'error': reason, 'result': None}


async def snapshot(args, offset, target_mono):
    started = time.time()
    deadline = target_mono + args.snapshot_budget
    records = []
    before, after = missing('not observed'), missing('not observed')
    interrupted = False

    async def request(method, params):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return missing('snapshot deadline exceeded')
        return await rpc(args.url, method, params, min(args.request_timeout, remaining), args.max_response_bytes)

    async def height():
        value = await request('eth_blockNumber', [])
        if value['status'] == 'ok':
            if not isinstance(value['result'], str) or not re.fullmatch(r'0x[0-9a-fA-F]+', value['result']):
                value.update(status='missing', error='invalid height', result=None)
        return value

    try:
        before = await height()
        for market in range(1, args.markets + 1):
            value = await request('torus_getOrderBook', [hex(market)])
            if value['status'] == 'ok':
                try:
                    value['result'] = book_result(value['result'], market, args.max_levels)
                except ValueError as exc:
                    value.update(status='missing', error=str(exc), result=None)
            records.append({'market_id': market, **value})
        after = await height()
    except asyncio.CancelledError:
        interrupted = True
    for market in range(len(records) + 1, args.markets + 1):
        records.append({'market_id': market, **missing('observer stopped')})
    regressed = (before['status'] == after['status'] == 'ok' and
                 int(after['result'], 16) < int(before['result'], 16))
    complete = not interrupted and not regressed and all(value['status'] == 'ok' for value in [before, after, *records])
    return {'offset_seconds': offset, 'target_wall': args.start_unix + offset,
            'started_wall': started, 'completed_wall': time.time(),
            'lateness_seconds': max(0, started - args.start_unix - offset),
            'status': 'complete' if complete else 'partial', 'interrupted': interrupted,
            'height_before': before, 'height_after': after, 'height_regressed': regressed, 'markets': records}


async def observe(args):
    # New directory only: no overwrites, append-to-prior-run, or stale manifest.
    args.out.mkdir(parents=False, exist_ok=False)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    reason = {'value': None}
    def stopped(why):
        reason['value'] = why
        stop.set()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stopped, sig.name)
    identity = parent_identity(args.parent_pid)
    start_mono = time.monotonic() + args.start_unix - time.time()
    count, size, complete, failure = 0, 0, True, None

    async def watch_parent():
        while not stop.is_set():
            if identity is None or parent_identity(args.parent_pid) != identity:
                stopped('parent exited')
                return
            try:
                await asyncio.wait_for(stop.wait(), 0.5)
            except asyncio.TimeoutError:
                pass

    async def collect(file):
        nonlocal count, size, complete
        for offset in args.offsets:
            delay = max(0, start_mono + offset - time.monotonic())
            try:
                await asyncio.wait_for(stop.wait(), delay)
            except asyncio.TimeoutError:
                pass
            if stop.is_set():
                break
            value = await snapshot(args, offset, start_mono + offset)
            encoded = (json.dumps(value, separators=(',', ':'), allow_nan=False) + '\n').encode()
            if size + len(encoded) > args.max_output_bytes:
                raise ValueError('output byte limit exceeded')
            file.write(encoded)
            file.flush()
            size += len(encoded)
            count += 1
            complete = complete and value['status'] == 'complete'
            if stop.is_set():
                break

    worker = watcher = stopper = None
    try:
        with (args.out / 'depth.jsonl').open('xb') as file:
            worker = asyncio.create_task(collect(file))
            watcher = asyncio.create_task(watch_parent())
            stopper = asyncio.create_task(stop.wait())
            done, _ = await asyncio.wait([worker, stopper, watcher], return_when=asyncio.FIRST_COMPLETED)
            if worker in done:
                worker.result()
            else:
                worker.cancel()
                await asyncio.gather(worker, return_exceptions=True)
    except Exception as exc:
        failure = f'{type(exc).__name__}: {exc}'[:256]
    finally:
        stop.set()
        for task in (worker, watcher, stopper):
            if task is not None and not task.done():
                task.cancel()
        await asyncio.gather(*(t for t in (worker, watcher, stopper) if t is not None), return_exceptions=True)
        for sig in (signal.SIGTERM, signal.SIGINT):
            loop.remove_signal_handler(sig)
        manifest = {'status': 'complete' if complete and count == len(args.offsets) and failure is None else 'partial',
                    'error': failure, 'stop_reason': reason['value'], 'completed_wall': time.time(),
                    'url': args.url, 'parent_pid': args.parent_pid, 'start_unix': args.start_unix,
                    'duration': args.duration, 'offsets': args.offsets, 'markets': args.markets,
                    'snapshots_written': count, 'output_bytes': size,
                    'bounds': {k: getattr(args, k) for k in ('request_timeout', 'snapshot_budget',
                              'max_response_bytes', 'max_output_bytes', 'max_levels')},
                    'atomic': False}
        with (args.out / 'manifest.json').open('x') as file:
            json.dump(manifest, file, indent=2, allow_nan=False)
    return 0 if manifest['status'] == 'complete' else 2


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', required=True)
    parser.add_argument('--markets', type=int, required=True)
    parser.add_argument('--start-unix', type=float, required=True)
    parser.add_argument('--duration', type=float, required=True)
    parser.add_argument('--offsets', default='0,100,200,end')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--parent-pid', type=int, default=os.getppid())
    parser.add_argument('--request-timeout', type=float, default=2)
    parser.add_argument('--snapshot-budget', type=float, default=30)
    parser.add_argument('--max-response-bytes', type=int, default=262144)
    parser.add_argument('--max-output-bytes', type=int, default=16 * 1024 * 1024)
    parser.add_argument('--max-levels', type=int, default=256)
    args = parser.parse_args()
    try:
        args.offsets = [args.duration if token == 'end' else float(token) for token in args.offsets.split(',')]
        url = urlsplit(args.url)
        valid = (url.scheme in ('http', 'https') and url.hostname is not None and not url.username and not url.password
                 and 1 <= args.markets <= 300 and args.parent_pid > 0
                 and math.isfinite(args.start_unix) and args.start_unix <= time.time() + 300
                 and 0 < args.duration <= 86400 and 1 <= len(args.offsets) <= 8
                 and all(math.isfinite(t) and 0 <= t <= args.duration for t in args.offsets)
                 and args.offsets == sorted(set(args.offsets))
                 and 0 < args.request_timeout <= 10 and 0 < args.snapshot_budget <= 60
                 and 1 <= args.max_response_bytes <= 1048576
                 and 1 <= args.max_output_bytes <= 64 * 1024 * 1024 and 1 <= args.max_levels <= 1024)
        if not valid:
            raise ValueError('invalid URL, schedule or bounds')
    except ValueError as exc:
        parser.error(str(exc))
    return args


if __name__ == '__main__':
    if os.environ.get('DEPTH_OBSERVER', '0') != '1':
        print('depth observer disabled; set DEPTH_OBSERVER=1 to enable', file=sys.stderr)
        raise SystemExit(0)
    raise SystemExit(asyncio.run(observe(arguments())))
