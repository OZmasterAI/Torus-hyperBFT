#!/usr/bin/env python3
"""test_harness.py — unit tests for the matched-bench harness itself.

    python3 tools/matched-bench/test_harness.py

Runs offline (no devnet, no cargo, no network beyond 127.0.0.1 loopback stubs)
and in a few seconds. Two things are pinned here, both of which cost real bench
time when they broke:

1. `digest-node.sh` (the 3-validator state digest) must produce a stream in the
   SAME deterministic order as the pre-r6 serial loop even though it fans the
   per-market RPCs out `-P` ways, or two honest validators hash differently and
   a good candidate reads as a fork.
2. `summarize.py` must (a) report a `first120` window so 120 s and 300 s cells
   are comparable, and (b) never call `validators_agree=true` without an equal
   state digest, while NOT calling a cell a fork when the only thing that
   differs is a digest taken while the chain was still moving.
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
DIGEST_SH = os.path.join(HERE, "digest-node.sh")
SUMMARIZE = os.path.join(HERE, "summarize.py")


def jq_compact(obj):
    """Byte-identical to `jq -cS` for the flat string maps this stub returns."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


class StubRpc(BaseHTTPRequestHandler):
    """Deterministic JSON-RPC stub with per-request latency skew, so a digest
    that concatenates in completion order (rather than market order) fails."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *_a):
        pass

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        method, params = req.get("method", ""), req.get("params", [])
        arg = params[0] if params else ""
        # Skew: high market ids answer FIRST, so completion order != id order.
        try:
            skew = max(0, 40 - int(str(arg), 16)) if str(arg).startswith("0x") else 5
        except ValueError:
            skew = 5
        import time

        time.sleep(skew / 1000.0)
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": req.get("id", 1),
                "result": {"m": method, "a": str(arg)},
            }
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class DigestOrderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), StubRpc)
        cls.url = "http://127.0.0.1:%d" % cls.srv.server_address[1]
        cls.t = threading.Thread(target=cls.srv.serve_forever, daemon=True)
        cls.t.start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()

    def expected_stream(self, markets, accounts):
        lines = []
        for m in range(1, markets + 1):
            hx = "0x%x" % m
            lines.append(jq_compact({"m": "torus_getOrderBook", "a": hx}))
            lines.append(jq_compact({"m": "torus_getOpenInterest", "a": hx}))
        for a in accounts:
            lines.append(jq_compact({"m": "torus_getBalances", "a": a}))
        return "".join(line + "\n" for line in lines)

    def run_digest(self, markets, accounts, par):
        d = tempfile.mkdtemp(prefix="digest-test-")
        try:
            acct = os.path.join(d, "accounts.txt")
            with open(acct, "w") as f:
                f.write("".join(a + "\n" for a in accounts))
            out = os.path.join(d, "state-digest.txt")
            r = subprocess.run(
                ["bash", DIGEST_SH, self.url, str(markets), acct, out, str(par), "10"],
                capture_output=True,
                text=True,
                timeout=180,
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(out) as f:
                return f.read(), r.stdout.strip()
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_parallel_digest_matches_serial_order_and_hash(self):
        accounts = ["0xaa%02d" % i for i in range(7)]
        want = self.expected_stream(40, accounts)
        want_sha = hashlib.sha256(want.encode()).hexdigest()

        serial, out1 = self.run_digest(40, accounts, 1)
        self.assertEqual(serial, want, "serial (-P 1) stream must be id-ordered")

        par, out8 = self.run_digest(40, accounts, 8)
        self.assertEqual(par, serial, "-P 8 must not reorder the digest stream")
        self.assertIn(want_sha, out1)
        self.assertIn(want_sha, out8, "parallel digest must hash identically")

    def test_digest_is_stable_across_repeats(self):
        accounts = ["0xbb01", "0xbb02"]
        a, sa = self.run_digest(12, accounts, 6)
        b, sb = self.run_digest(12, accounts, 6)
        self.assertEqual(a, b)
        self.assertEqual(sa.split()[0], sb.split()[0])

    def test_reports_wall_seconds(self):
        _, out = self.run_digest(6, ["0xcc01"], 4)
        parts = out.split()
        self.assertEqual(len(parts), 2, "stdout must be '<sha256> <wall_seconds>'")
        self.assertEqual(len(parts[0]), 64)
        float(parts[1])


# ------------------------------------------------------------------ summarize.py
BENCH_START = 1_700_000_000


def write_cell(d, matched_first120_rate, matched_tail_rate, dur=300):
    """Synthetic 1 Hz sampler for one node trio: `matched` climbs at
    matched_first120_rate for 120 s, then at matched_tail_rate."""
    cols = [
        "torus_orders_matched_total",
        "torus_orders_placed_accepted_total",
        "torus_block_height",
        "torus_blocks_committed_total",
        "torus_native_actions_processed_total",
        "torus_orders_resting_total",
        "torus_exec_queue_depth",
        "torus_mempool_native_size",
    ]
    with open(os.path.join(d, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for s in range(dur + 1):
            matched = matched_first120_rate * min(s, 120) + matched_tail_rate * max(
                0, s - 120
            )
            for n in ("val0", "val1", "val2"):
                vals = [matched, matched * 2, 10 * s, 10 * s, matched, 5, 1, 0]
                f.write(
                    "%d,%s,%s\n"
                    % (BENCH_START + s, n, ",".join(str(int(v)) for v in vals))
                )
    with open(os.path.join(d, "bench.log"), "w") as f:
        f.write("Submitted (load-gen accepted): 1,000\n")


def write_agreement(d, digests, hashes=None, counters_equal=True):
    hashes = hashes or ["0xdead"] * 3
    with open(os.path.join(d, "agreement.jsonl"), "w") as f:
        for i in range(3):
            f.write(
                json.dumps(
                    {
                        "node": "val%d" % i,
                        "height": 1000 + i,
                        "cmp_height": 995,
                        "block_hash": hashes[i],
                        "header_state_root": "0x0",
                        "state_digest": digests[i],
                        "matched": 100,
                        "placed": 200,
                        "resting": 5 if counters_equal or i == 0 else 6,
                        "actions": 100,
                        "panic_or_failstop_lines": 0,
                        "error_lines": 0,
                    }
                )
                + "\n"
            )


def run_summarize(d, dur=300, drained="1", extra=()):
    cmd = [
        sys.executable,
        SUMMARIZE,
        "--out",
        d,
        "--label",
        "t",
        "--markets",
        "300",
        "--dur",
        str(dur),
        "--rate",
        "76000",
        "--senders",
        "5000",
        "--t-bench0",
        str(BENCH_START),
        "--t-bench1",
        str(BENCH_START + dur),
        "--t-drain",
        str(BENCH_START + dur),
        "--drained",
        drained,
        "--bench-rc",
        "0",
        *extra,
    ]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if r.returncode != 0:
        raise AssertionError(r.stderr)
    with open(os.path.join(d, "summary.json")) as f:
        return json.load(f), r.stdout


class SummarizeTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="summ-test-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def test_first120_window_is_reported_and_distinct_from_avg(self):
        write_cell(self.d, 50_000, 10_000, dur=300)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        h = s["headline"]
        self.assertAlmostEqual(h["matched_s_first120"], 50_000, delta=500)
        self.assertAlmostEqual(h["matched_s_avg"], 26_000, delta=500)
        self.assertAlmostEqual(
            s["funnel_by_node"]["val1"]["matched_s_first120"], 50_000, delta=500
        )

    def test_first120_equals_avg_on_a_120s_cell(self):
        write_cell(self.d, 40_000, 0, dur=120)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d, dur=120)
        self.assertAlmostEqual(
            s["headline"]["matched_s_first120"], s["headline"]["matched_s_avg"], delta=1
        )

    def test_equal_digest_agrees(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertIs(s["headline"]["validators_agree"], True)

    def test_unequal_digest_while_quiescent_is_a_fork(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["a", "b", "b"])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertIs(s["headline"]["validators_agree"], False)

    def test_unequal_digest_while_chain_moved_is_unverified_not_a_fork(self):
        """r6-base-300m-r1: equal hash + equal counters, digest taken 4 min apart
        per node while the chain still moved. That must NOT read agree=False."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["a", "b", "b"])
        s, _ = run_summarize(self.d, drained="0", extra=["--digest-quiescent", "0"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertIsNone(
            s["headline"]["validators_agree"],
            "digest-unverified must be null, neither true nor false",
        )

    def test_never_agrees_without_an_equal_digest(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["a", "b", "c"])
        for q in ("0", "1"):
            s, _ = run_summarize(self.d, extra=["--digest-quiescent", q])
            self.assertIsNot(s["headline"]["validators_agree"], True)

    def test_hash_mismatch_is_always_a_fork(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3, hashes=["0xa", "0xb", "0xb"])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "0"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertIs(s["headline"]["validators_agree"], False)

    def test_digest_provenance_is_recorded(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(
            self.d,
            extra=[
                "--digest-quiescent",
                "1",
                "--digest-secs",
                "41 39 40",
                "--digest-heights",
                "1000 1001 1000",
                "--drain-timeout",
                "780",
                "--markets-per-sender",
                "3",
            ],
        )
        a = s["agreement"]
        self.assertEqual(a["state_digest_seconds_per_node"], [41.0, 39.0, 40.0])
        self.assertEqual(a["state_digest_heights"], [1000, 1001, 1000])
        self.assertTrue(a["state_digest_quiescent"])
        self.assertEqual(s["timing"]["drain_timeout_s"], 780)
        self.assertEqual(s["cell"]["markets_per_sender"], 3)

    def test_legacy_cell_without_digest_flags_still_summarizes(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertIsNone(s["cell"]["markets_per_sender"])


if __name__ == "__main__":
    if not os.path.exists(DIGEST_SH):
        print("NOTE: %s missing — digest tests will fail" % DIGEST_SH)
    unittest.main(verbosity=2)
