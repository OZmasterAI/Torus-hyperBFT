#!/usr/bin/env python3
"""Independent bounded node scrapes; one parser and CSV writer per process.

Column order and CSV headers belong to run-cell.sh. CSV timestamps remain
integer seconds for existing consumers, but are taken AFTER each response.
The JSONL audit retains precise wall/monotonic times and request failures.
"""
import argparse
import asyncio
from contextlib import ExitStack
from dataclasses import dataclass
import json
import math
from pathlib import Path
import re
import signal
import time

REQUIRED = (
    "torus_blocks_committed_total", "torus_mempool_native_size",
    "torus_exec_queue_depth", "torus_flush_worker_depth",
    "torus_orders_placed_accepted_total", "torus_orders_matched_total",
    "torus_native_actions_processed_total", "torus_orders_resting_total",
)
NUMBER = re.compile(r"^[0-9]+([.][0-9]+)?([eE][+-]?[0-9]+)?$")
LE = re.compile(r'le="([^"]*)"')


def parse_metrics(text, bucket_names):
    """Match legacy extract's names, last-value wins and optional zero defaults."""
    values, buckets = {}, []
    for line in text.splitlines():
        fields = line.split()
        if not fields or fields[0].startswith("#"):
            continue
        # Like awk, an empty duplicate must overwrite the earlier value;
        # otherwise a malformed required series could retain stale validity.
        name, value = fields[0], fields[1] if len(fields) > 1 else ""
        values[name] = value
        base, brace, _ = name.partition("{")
        bound = LE.search(name) if brace and base in bucket_names else None
        if bound:
            buckets.append((base, bound.group(1), value))
    valid = all(NUMBER.fullmatch(values.get(key, "")) and
                math.isfinite(float(values[key])) for key in REQUIRED)
    values["scrape_valid"] = "1" if valid else "0"
    return values, buckets


@dataclass
class Response:
    text: str
    success: bool
    started_wall: float
    started_mono: float
    completed_wall: float
    completed_mono: float
    returncode: int | None
    error: str | None
    pid: int | None


async def terminate_and_reap(process):
    if process.returncode is None:
        try:
            process.terminate()
        except ProcessLookupError:
            pass
        try:
            # Cancellation may have left a full PIPE buffer. Drain while
            # waiting: wait() alone can hang even after the child exits.
            await asyncio.wait_for(process.communicate(), 1.0)
        except asyncio.TimeoutError:
            try:
                process.kill()
            except ProcessLookupError:
                pass
            await process.communicate()
    else:
        # The child may already have exited with unread buffered output.
        await process.communicate()


async def fetch(url, timeout):
    started_wall, started_mono = time.time(), time.monotonic()
    process, text, success, error, returncode = None, "", False, None, None
    # Shield process creation so cancellation cannot orphan a spawned curl
    # before the coroutine receives its Process handle.
    creation = asyncio.create_task(asyncio.create_subprocess_exec(
        "curl", "-fsS", "--max-time", str(timeout), url,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE))
    try:
        process = await asyncio.shield(creation)
        stdout, stderr = await asyncio.wait_for(process.communicate(), timeout + 1.0)
        returncode = process.returncode
        if returncode == 0:
            text = stdout.decode("utf-8")
            success = True
        else:
            # Never salvage a partial body from a timed-out/failed response.
            error = stderr.decode("utf-8", errors="replace")[:2048]
    except asyncio.CancelledError:
        if process is None:
            try:
                process = await creation
            except OSError:
                pass
        raise
    except (OSError, UnicodeError, asyncio.TimeoutError) as exc:
        error = f"{type(exc).__name__}: {exc}"
    finally:
        if process is not None:
            await terminate_and_reap(process)
    return Response(text, success, started_wall, started_mono, time.time(),
                    time.monotonic(), returncode, error,
                    process.pid if process is not None else None)


class CsvSink:
    """Only the event-loop thread writes. No await can interleave a row."""
    def __init__(self, out, wide, funnel, phase, buckets, nodes):
        self.out, self.columns = out, (wide, funnel, phase)
        self.bucket_names, self.nodes = set(buckets), nodes
        self.stack = ExitStack()

    def __enter__(self):
        try:
            def opened(name):
                return self.stack.enter_context((self.out / name).open("a", buffering=1))
            self.wide = opened("sampler.csv")
            self.buckets = opened("buckets.csv")
            self.audit = opened("sampler-diagnostics.jsonl")
            self.funnels = {node: opened(f"funnel-{node}.csv") for node in self.nodes}
            self.phases = {node: opened(f"phase-{node}.csv") for node in self.nodes}
        except BaseException:
            self.stack.close()
            raise
        return self

    def __exit__(self, *_):
        self.stack.close()

    def emit(self, node, response):
        values, buckets = parse_metrics(response.text if response.success else "", self.bucket_names)
        ts = int(response.completed_wall)
        def row(columns):
            return ",".join(values.get(name, "0") for name in columns)
        self.wide.write(f"{ts},{node},{row(self.columns[0])}\n")
        self.funnels[node].write(f"{ts},{row(self.columns[1])}\n")
        self.phases[node].write(f"{ts},{row(self.columns[2])}\n")
        if buckets:
            self.buckets.write("".join(f"{ts},{node},{name},{le},{count}\n"
                                       for name, le, count in buckets))
        self.audit.write(json.dumps({
            "node": node, "ts": ts, "started_wall": response.started_wall,
            "completed_wall": response.completed_wall,
            "started_monotonic": response.started_mono,
            "completed_monotonic": response.completed_mono,
            "request_seconds": response.completed_mono - response.started_mono,
            "curl_pid": response.pid, "curl_returncode": response.returncode,
            "scrape_valid": int(values["scrape_valid"]), "error": response.error,
        }, separators=(",", ":"), allow_nan=False) + "\n")


async def sample_node(node, url, stop, emit, interval, timeout):
    while not stop.is_set():
        response = await fetch(url, timeout)
        if stop.is_set():
            break
        emit(node, response)
        # No catch-up bursts or synthetic points. Normal requests keep their
        # own cadence. Completion seconds must advance for legacy int readers;
        # a backwards wall-clock jump remains visible, never clamped away.
        next_start = max(response.started_mono + interval,
                         response.completed_mono +
                         (math.floor(response.completed_wall) + 1 - response.completed_wall))
        delay = max(0, next_start - time.monotonic())
        try:
            await asyncio.wait_for(stop.wait(), delay)
        except asyncio.TimeoutError:
            pass


async def collect(urls, stop, emit, interval=1.0, timeout=3.0):
    workers = [asyncio.create_task(sample_node(node, url, stop, emit, interval, timeout))
               for node, url in urls.items()]
    stopping = asyncio.create_task(stop.wait())
    try:
        done, _ = await asyncio.wait([stopping, *workers], return_when=asyncio.FIRST_COMPLETED)
        for task in done:
            if task is not stopping:
                task.result()  # A writer/worker failure must not silently drop a node.
    finally:
        stop.set()
        stopping.cancel()
        for worker in workers:
            worker.cancel()
        await asyncio.gather(stopping, *workers, return_exceptions=True)


async def run(args):
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    urls = {f"val{i}": url for i, url in enumerate(args.urls)}
    try:
        with CsvSink(args.out, args.wide.split(), args.funnel.split(), args.phase.split(),
                     args.buckets.split(), urls) as sink:
            await collect(urls, stop, sink.emit, args.interval, args.timeout)
    finally:
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.remove_signal_handler(sig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    for name in ("wide", "funnel", "phase", "buckets"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("urls", nargs=3)
    args = parser.parse_args()
    if not all(math.isfinite(v) and v > 0 for v in (args.interval, args.timeout)):
        parser.error("interval and timeout must be finite and positive")
    asyncio.run(run(args))


if __name__ == "__main__":
    main()
