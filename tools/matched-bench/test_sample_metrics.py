#!/usr/bin/env python3
"""Offline collector tests: local HTTP only; no validators or benchmark binaries."""
import asyncio
import csv
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import sample_metrics as sampler

BODY = "".join(f"{name} {i + 1}\n" for i, name in enumerate(sampler.REQUIRED))
BUCKET = "torus_exec_chain_seconds_bucket"
WIDE = [*sampler.REQUIRED, "optional_missing", "scrape_valid"]


def response(text=BODY, success=True, start=100.1, end=102.9):
    return sampler.Response(text, success, start, 10.0, end, 12.8,
                            0 if success else 28, None, 123)


class ParserTests(unittest.TestCase):
    def test_legacy_awk_equivalence_and_optional_defaults(self):
        # Independent reference is the old wide extractor, with fixed columns.
        fixture = ("# HELP ignored\n" + BODY +
                   f"{sampler.REQUIRED[0]} 2e3\n" +
                   f'{BUCKET}{{le="0.1"}} 7\n{BUCKET}{{le="+Inf"}} 9\n')
        reference = r'''BEGIN {
            n=split(names,a," "); for(i=1;i<=n;i++) want[a[i]]=1;
            nr=split(required,req," ");
        }
        ($1 in want){v[$1]=$2}
        END {
            valid=1; for(i=1;i<=nr;i++) if(!(req[i] in v) || v[req[i]] !~ /^[0-9]+([.][0-9]+)?([eE][+-]?[0-9]+)?$/) valid=0;
            v["scrape_valid"]=valid;
            for(i=1;i<=n;i++) printf "%s%s",(i>1?",":""),(a[i] in v ? v[a[i]] : 0); printf "\n";
        }'''
        for body in (fixture, "", fixture.replace(f"{sampler.REQUIRED[2]} 3", ""),
                     fixture + sampler.REQUIRED[0] + "\n"):
            with self.subTest(body=body):
                old = subprocess.run(["awk", "-v", "names=" + " ".join(WIDE),
                                      "-v", "required=" + " ".join(sampler.REQUIRED),
                                      reference], input=body, text=True, capture_output=True, check=True).stdout
                values, _ = sampler.parse_metrics(body, {BUCKET})
                self.assertEqual(",".join(values.get(k, "0") for k in WIDE) + "\n", old)
        values, buckets = sampler.parse_metrics(fixture, {BUCKET})
        self.assertEqual(values[sampler.REQUIRED[0]], "2e3")
        self.assertEqual(buckets, [(BUCKET, "0.1", "7"), (BUCKET, "+Inf", "9")])

    def test_missing_and_nonfinite_required_metrics_are_invalid(self):
        for value in ("NaN", "+Inf", "-1", "1e999", "junk"):
            values, _ = sampler.parse_metrics(BODY + f"{sampler.REQUIRED[0]} {value}\n", set())
            self.assertEqual(values["scrape_valid"], "0", value)
        self.assertEqual(sampler.parse_metrics("", set())[0]["scrape_valid"], "0")

    def test_complete_rows_response_timestamp_and_failed_body_discard(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            with sampler.CsvSink(out, WIDE, [sampler.REQUIRED[0]],
                                 ["optional_missing"], [BUCKET], ["val0", "val1"]) as sink:
                for node in ("val0", "val1"):
                    sink.emit(node, response(BODY + f'{BUCKET}{{le="+Inf"}} 9\n'))
                # Even a complete-looking partial response must not count.
                sink.emit("val0", response(BODY, success=False, end=105.1))
            rows = list(csv.reader(io.StringIO((out / "sampler.csv").read_text())))
            self.assertEqual([r[:2] for r in rows], [["102", "val0"], ["102", "val1"], ["105", "val0"]])
            self.assertTrue(all(len(r) == len(WIDE) + 2 for r in rows))
            self.assertEqual(rows[-1][2:], ["0"] * len(WIDE))
            self.assertEqual((out / "funnel-val0.csv").read_text(), "102,1\n105,0\n")
            self.assertEqual((out / "phase-val1.csv").read_text(), "102,0\n")
            self.assertEqual(len((out / "buckets.csv").read_text().splitlines()), 2)
            audit = [json.loads(line) for line in (out / "sampler-diagnostics.jsonl").read_text().splitlines()]
            self.assertEqual(audit[0]["completed_wall"], 102.9)
            self.assertAlmostEqual(audit[0]["request_seconds"], 2.8)
            self.assertEqual(audit[-1]["scrape_valid"], 0)
            # gap_attr.py's integer parser remains supported.
            self.assertEqual([int(row[0]) for row in rows], [102, 102, 105])


class CollectorTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.connections = set()
        self.handlers = {}
        self.server = await asyncio.start_server(self.handle, "127.0.0.1", 0)
        self.base = f"http://127.0.0.1:{self.server.sockets[0].getsockname()[1]}"

    async def asyncTearDown(self):
        self.server.close()
        # Python 3.12 wait_closed() also waits for active client connections.
        # Stop blocked handlers first so their finally blocks close transports.
        connections = tuple(self.connections)
        for task in connections:
            task.cancel()
        await asyncio.gather(*connections, return_exceptions=True)
        await asyncio.wait_for(self.server.wait_closed(), 3)

    async def handle(self, reader, writer):
        task = asyncio.current_task()
        self.connections.add(task)
        try:
            request = await reader.readuntil(b"\r\n\r\n")
            route = request.split()[1].decode()
            handler = self.handlers.get(route)
            if handler:
                await handler(writer)
            else:
                await self.send(writer, BODY)
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except ConnectionError:
                pass
            self.connections.discard(task)

    async def send(self, writer, body, status="200 OK", extra_length=0):
        payload = body.encode()
        writer.write(f"HTTP/1.1 {status}\r\nContent-Length: {len(payload) + extra_length}\r\nConnection: close\r\n\r\n".encode() + payload)
        await writer.drain()

    async def test_delayed_endpoint_does_not_stall_other_nodes_and_timestamps_advance(self):
        slow_entered, release = asyncio.Event(), asyncio.Event()
        async def slow(writer):
            slow_entered.set()
            await release.wait()
            await self.send(writer, BODY)
        self.handlers["/slow"] = slow
        stop, rows, ready = asyncio.Event(), [], asyncio.Event()
        def emit(node, item):
            rows.append((node, item))
            if all(sum(n == fast for n, _ in rows) >= 2 for fast in ("val0", "val2")):
                ready.set()
        task = asyncio.create_task(sampler.collect(
            {"val0": self.base + "/fast", "val1": self.base + "/slow", "val2": self.base + "/fast"},
            stop, emit, timeout=10))
        try:
            await asyncio.wait_for(slow_entered.wait(), 2)
            await asyncio.wait_for(ready.wait(), 4)
            self.assertFalse(any(n == "val1" for n, _ in rows))
            for node in ("val0", "val2"):
                stamps = [int(r.completed_wall) for n, r in rows if n == node]
                self.assertTrue(all(b > a for a, b in zip(stamps, stamps[1:])))
        finally:
            stop.set()
            await asyncio.wait_for(task, 3)
            release.set()

    async def test_timeout_discards_partial_response_and_marks_invalid(self):
        async def partial(writer):
            await self.send(writer, BODY, extra_length=100)
            await asyncio.Event().wait()
        self.handlers["/partial"] = partial
        item = await sampler.fetch(self.base + "/partial", 0.15)
        self.assertFalse(item.success)
        self.assertEqual(item.text, "")
        self.assertNotEqual(item.returncode, 0)
        self.assertLess(item.completed_mono - item.started_mono, 2)
        self.assertEqual(sampler.parse_metrics(item.text, set())[0]["scrape_valid"], "0")

    async def test_http_error_cannot_be_a_valid_sample(self):
        async def failed(writer):
            await self.send(writer, BODY, status="503 Unavailable")
        self.handlers["/failed"] = failed
        item = await sampler.fetch(self.base + "/failed", 1)
        self.assertFalse(item.success)
        self.assertEqual(item.text, "")

    async def test_timestamp_is_after_response_not_before_request(self):
        released = []
        async def slow(writer):
            await asyncio.sleep(0.05)
            released.append(time.time())
            await self.send(writer, BODY)
        self.handlers["/slow"] = slow
        item = await sampler.fetch(self.base + "/slow", 1)
        self.assertTrue(item.success)
        self.assertLessEqual(item.started_wall, released[0])
        self.assertLessEqual(released[0], item.completed_wall)
        self.assertGreater(item.completed_mono, item.started_mono)

    async def test_stop_cancels_and_reaps_all_inflight_curl_children(self):
        requests, processes, entered = 0, [], asyncio.Event()
        async def blocked(writer):
            nonlocal requests
            requests += 1
            if requests == 3:
                entered.set()
            await asyncio.Event().wait()
        self.handlers["/blocked"] = blocked
        create = asyncio.create_subprocess_exec
        async def tracked(*args, **kwargs):
            process = await create(*args, **kwargs)
            processes.append(process)
            return process
        stop = asyncio.Event()
        with patch.object(sampler.asyncio, "create_subprocess_exec", tracked):
            task = asyncio.create_task(sampler.collect(
                {f"val{i}": self.base + "/blocked" for i in range(3)}, stop,
                lambda *_: self.fail("stopped request emitted a sample"), timeout=30))
            try:
                await asyncio.wait_for(entered.wait(), 3)
            finally:
                stop.set()
                await asyncio.wait_for(task, 3)
        self.assertEqual(len(processes), 3)
        for process in processes:
            self.assertIsNotNone(process.returncode)
            with self.assertRaises(ProcessLookupError):
                os.kill(process.pid, 0)

    async def test_shutdown_drains_full_pipes_before_reaping(self):
        # This child deliberately fills stdout while no communicate() reader
        # runs, reproducing cancellation's possible full-buffer state.
        process = await asyncio.create_subprocess_exec(
            sys.executable, "-c",
            "import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "os.write(2,b'ready'); os.write(1,b'x'*1048576); os.write(2,b'y'*1048576); time.sleep(30)",
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        try:
            self.assertEqual(await asyncio.wait_for(process.stderr.readexactly(5), 3), b"ready")
            await asyncio.sleep(0.05)
            await asyncio.wait_for(sampler.terminate_and_reap(process), 3)
            self.assertIsNotNone(process.returncode)
            with self.assertRaises(ProcessLookupError):
                os.kill(process.pid, 0)
        finally:
            if process.returncode is None:
                process.kill()
            await asyncio.wait_for(process.communicate(), 3)

    async def test_cli_sigterm_reaps_children_and_exits_cleanly(self):
        requests, pids, entered = 0, [], asyncio.Event()
        async def blocked(writer):
            nonlocal requests
            requests += 1
            if requests == 3:
                entered.set()
            await asyncio.Event().wait()
        self.handlers["/blocked"] = blocked
        with tempfile.TemporaryDirectory() as directory:
            process = await asyncio.create_subprocess_exec(
                sys.executable, str(Path(sampler.__file__)), "--out", directory,
                "--wide", " ".join(WIDE), "--funnel", sampler.REQUIRED[0],
                "--phase", sampler.REQUIRED[0], "--buckets", BUCKET, "--timeout", "30",
                *([self.base + "/blocked"] * 3),
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
            try:
                await asyncio.wait_for(entered.wait(), 4)
                # Linux devnet host: capture the sampler's direct curl children.
                children = Path(f"/proc/{process.pid}/task/{process.pid}/children")
                pids = [int(pid) for pid in children.read_text().split()]
                self.assertEqual(len(pids), 3)
                process.send_signal(signal.SIGTERM)
                _, stderr = await asyncio.wait_for(process.communicate(), 3)
                self.assertEqual(process.returncode, 0, stderr.decode())
                for pid in pids:
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid, 0)
            finally:
                await sampler.terminate_and_reap(process)
                for pid in pids:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass


if __name__ == "__main__":
    unittest.main()
