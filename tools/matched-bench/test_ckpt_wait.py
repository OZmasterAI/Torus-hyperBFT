#!/usr/bin/env python3
"""ckpt_wait.py + run-cell.sh digest_ckpt_wait: the state digest waits for a
checkpoint submitted after the drain to be durable on every live node.

Item 6 Phase 3 coalesced checkpoints (TORUS_CHECKPOINT_INTERVAL_MS, 15 s on the
standard load): validator RPC reads RocksDB only and lags up to one checkpoint
window by design (docs/plans/item6-phase3-design.md). The drain's quiet window
is 10 s, so without the wait the digest can read a durable state from before
the last state change (ozarchy-9k-smokek-smoke-s0: books flipped stale ->
correct mid-digest, a false DISAGREE).

Offline: fake /metrics endpoints on 127.0.0.1 whose counters advance by
REQUEST COUNT, never on a timer: the helper's T0 baseline is always the first
answered scrape, so a loaded host cannot make the baseline read an advanced
counter (s137 review: a 100 ms timer could beat the baseline, target C0 + 1
then unreachable). No assertion has an upper bound on wall time; a timeout is
short only where the timeout itself is under test. No node, no cargo.

    python3 -m pytest -q tools/matched-bench/test_ckpt_wait.py
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
CKPT_WAIT = HERE / "ckpt_wait.py"
RUN_CELL = HERE / "run-cell.sh"
SUBMITS = "torus_exec_checkpoints_total"
WRITES = "torus_exec_checkpoint_db_write_seconds_count"
ON = json.dumps({"TORUS_EXEC_PIPELINE": "1", "TORUS_CHECKPOINT_INTERVAL_MS": "15000"})
# Where a timeout is NOT the thing under test: generous, so a loaded host
# never turns a pass into a timeout (a passing wait returns as soon as it can).
LONG = ["--timeout-s", "60"]


class FakeNode:
    """A /metrics endpoint driven by request count. `steps`: [(n, submits,
    writes)]; the answered scrape with index i (0 = the first, the helper's T0
    baseline) serves the last step with n <= i. submits/writes None = metric
    absent from the body. `fail_first`: the first N requests get HTTP 503 (not
    answered, so they do not advance the steps); `fail`: every request does.
    `raw`: metric lines served verbatim instead of the steps on every answer
    with index >= `raw_from` (malformed-counter tests: exact text such as
    -1e-999 or duplicates)."""

    def __init__(self, steps, fail=False, fail_first=0, raw=None, raw_from=0):
        self.raw = raw
        self.raw_from = raw_from
        self.steps = sorted(steps, key=lambda e: e[0])
        self.fail = fail
        self.fail_first = fail_first
        self.hits = 0
        self.answered = 0
        self.lock = threading.Lock()
        node = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                body = node.next_body()
                if body is None:
                    self.send_response(503)
                    self.end_headers()
                    return
                body = body.encode()
                self.send_response(200)
                self.send_header("content-type", "text/plain")
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_port
        self.url = "http://127.0.0.1:%d/metrics" % self.port
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def next_body(self):
        with self.lock:
            self.hits += 1
            if self.fail or self.hits <= self.fail_first:
                return None
            i = self.answered
            self.answered += 1
        submits = writes = None
        for n, s, w in self.steps:
            if n <= i:
                submits, writes = s, w
        lines = ["# HELP torus_block_height x", "torus_block_height 1600"]
        if self.raw is not None and i >= self.raw_from:
            return "\n".join(lines + list(self.raw)) + "\n# EOF\n"
        if submits is not None:
            lines.append("# TYPE torus_exec_checkpoints counter")
            lines.append("%s %d" % (SUBMITS, submits))
        if writes is not None:
            lines.append('torus_exec_checkpoint_db_write_seconds_bucket{le="0.001"} 0')
            lines.append("torus_exec_checkpoint_db_write_seconds_sum 1.5")
            lines.append("%s %d" % (WRITES, writes))
        return "\n".join(lines) + "\n# EOF\n"

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def run_wait(nodes, node_env=ON, extra=(), book_mode="classic", timeout=120):
    """nodes: [(name, FakeNode)] -> (rc, stdout, stderr, json record, wall s)."""
    with tempfile.TemporaryDirectory() as d:
        out = Path(d, "digest-ckpt-wait.json")
        cmd = [
            sys.executable,
            "-I",
            str(CKPT_WAIT),
            "--node-env",
            node_env,
            "--genesis-book-mode",
            book_mode,
            "--out",
            str(out),
            "--poll-s",
            "0.05",
        ]
        for name, node in nodes:
            cmd += ["--metrics", "%s=%s" % (name, node.url)]
        cmd += list(extra)
        t = time.monotonic()
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        wall = time.monotonic() - t
        rec = json.loads(out.read_text()) if out.exists() else None
        return r.returncode, r.stdout, r.stderr, rec, wall


class WaitTests(unittest.TestCase):
    def test_waits_for_a_checkpoint_submitted_after_t0_on_every_node(self):
        # At T0 val0's last submit (21) is already durable, val1's is still in
        # flight (writes 20). Neither counts: only write >= C0 + 1 is a
        # checkpoint SUBMITTED after T0. val0 gets there on its 4th scrape
        # (poll round 3), val1 on its 7th (poll round 6).
        n0 = FakeNode([(0, 21, 21), (3, 22, 22)])
        n1 = FakeNode([(0, 21, 20), (1, 21, 21), (3, 22, 21), (6, 22, 22)])
        with n0, n1:
            rc, out, err, rec, _ = run_wait([("val0", n0), ("val1", n1)])
        self.assertEqual(rc, 0, err)
        self.assertEqual(rec["outcome"], "durable")
        self.assertEqual(rec["interval_ms"], 15000)
        self.assertEqual(rec["timeout_s"], 60.0)
        self.assertEqual(rec["nodes"]["val0"]["c0"], 21)
        self.assertEqual(rec["nodes"]["val0"]["write_count"], 22)
        self.assertEqual(rec["nodes"]["val1"]["c0"], 21)
        self.assertEqual(rec["nodes"]["val1"]["write_count"], 22)
        self.assertTrue(all(n["durable"] for n in rec["nodes"].values()))
        self.assertEqual((n0.answered, n1.answered), (4, 7))
        # waited_s counts from the helper's own T0; val1 became durable three
        # poll sleeps after val0, and six 0.05 s sleeps is a lower bound.
        self.assertGreater(
            rec["nodes"]["val1"]["waited_s"], rec["nodes"]["val0"]["waited_s"]
        )
        self.assertGreaterEqual(rec["waited_s"], 0.29)
        lines = out.strip().splitlines()
        self.assertEqual(len(lines), 1, out)
        self.assertRegex(
            lines[0],
            r"^digest checkpoint wait: val0 ckpt 21->22 durable, "
            r"val1 ckpt 21->22 durable, waited \d+\.\ds \(interval 15000 ms\)$",
        )

    def test_only_the_given_live_nodes_are_waited_on(self):
        v0 = FakeNode([(0, 5, 5), (1, 6, 6)])
        v2 = FakeNode([(0, 5, 5), (1, 6, 6)])
        stuck = FakeNode([(0, 5, 5)])
        with v0, v2, stuck:
            rc, out, err, rec, _ = run_wait([("val0", v0), ("val2", v2)])
        self.assertEqual(rc, 0, err)
        self.assertEqual(sorted(rec["nodes"]), ["val0", "val2"])
        self.assertEqual(stuck.hits, 0)
        self.assertNotIn("val1", out)

    def test_checkpoints_off_by_env_skip_with_one_line(self):
        for env, why in (
            ({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}, "TORUS_CHECKPOINT_INTERVAL_MS"),
            ({"TORUS_CHECKPOINT_INTERVAL_MS": "  "}, None),
            ({"TORUS_CHECKPOINT_INTERVAL_MS": "-1"}, "TORUS_CHECKPOINT_INTERVAL_MS"),
            (
                {"TORUS_EXEC_PIPELINE": "0", "TORUS_CHECKPOINT_INTERVAL_MS": "15000"},
                "TORUS_EXEC_PIPELINE",
            ),
        ):
            with self.subTest(env=env):
                # blank = unset = the validator default (15 s) on classic: a
                # mode-3 genesis turns that default off.
                mode = "level_authority_chunked" if why is None else "classic"
                node = FakeNode([(0, 0, 0)])
                with node:
                    rc, out, err, rec, _ = run_wait(
                        [("val0", node)], json.dumps(env), book_mode=mode
                    )
                self.assertEqual(rc, 0, err)
                # No wait: the baseline scrape and nothing after it.
                self.assertEqual(node.hits, 1)
                self.assertEqual(rec["outcome"], "skipped")
                self.assertIsNone(rec["interval_ms"])
                lines = out.strip().splitlines()
                self.assertEqual(len(lines), 1, out)
                self.assertTrue(
                    lines[0].startswith("digest checkpoint wait: skipped"), lines[0]
                )
                self.assertIn("checkpoints off", lines[0])

    def test_unset_interval_is_the_15s_validator_default_on_classic(self):
        node = FakeNode([(0, 3, 3), (1, 4, 4)])
        with node:
            rc, out, err, rec, _ = run_wait([("val0", node)], json.dumps({}))
        self.assertEqual(rc, 0, err)
        self.assertEqual(rec["outcome"], "durable")
        self.assertEqual(rec["interval_ms"], 15000)

    def test_env_on_zero_counters_wait_for_the_first_checkpoint(self):
        """s137 review [P2]: checkpoints ON and both counters 0 at T0 = the
        first window has not flushed yet (blocks are memory-only Digest jobs
        until the first checkpoint). Skipping would accept a stale state:
        wait for the first durable checkpoint (target C0 + 1 = 1)."""
        n0 = FakeNode([(0, 0, 0), (2, 1, 1)])
        n1 = FakeNode([(0, 0, 0), (1, 1, 0), (4, 1, 1)])
        with n0, n1:
            rc, out, err, rec, _ = run_wait([("val0", n0), ("val1", n1)], extra=LONG)
        self.assertEqual(rc, 0, out + err)
        self.assertEqual(rec["outcome"], "durable")
        for name in ("val0", "val1"):
            self.assertEqual(rec["nodes"][name]["c0"], 0)
            self.assertEqual(rec["nodes"][name]["target"], 1)
            self.assertEqual(rec["nodes"][name]["write_count"], 1)
            self.assertTrue(rec["nodes"][name]["durable"])
        self.assertGreaterEqual(n1.answered, 5)
        self.assertIn("val0 ckpt 0->1 durable, val1 ckpt 0->1 durable", out)

    def test_env_on_zero_counters_never_flushed_is_a_timeout_not_a_skip(self):
        """Codex probe: writes [0, 0, 0], never durable -> must not ACCEPT."""
        node = FakeNode([(0, 0, 0)])
        with node:
            rc, out, err, rec, _ = run_wait(
                [("val0", node)], extra=["--timeout-s", "0.5"]
            )
        self.assertEqual(rc, 3, out + err)
        self.assertEqual(rec["outcome"], "timeout")
        self.assertFalse(rec["nodes"]["val0"]["durable"])
        self.assertIn("val0 ckpt 0->0 NOT durable", out)

    def test_env_on_mixed_fleet_zero_counters_are_still_held_to_the_first_checkpoint(
        self,
    ):
        """An older binary elsewhere does not excuse a metric-reporting node
        whose first checkpoint never landed."""
        old = FakeNode([(0, None, None)])
        zero = FakeNode([(0, 0, 0)])
        with old, zero:
            rc, out, err, rec, _ = run_wait(
                [("val0", old), ("val1", zero)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.2", "--timeout-s", "1.0"],
            )
        self.assertEqual(rc, 3, out + err)
        self.assertEqual(rec["outcome"], "timeout")
        self.assertIn("val1 ckpt 0->0 NOT durable", out)

    def test_env_off_but_node_checkpoints_waits_anyway(self):
        """The node is the truth: a resolution that disagrees with it must not
        skip the wait silently."""
        node = FakeNode([(0, 7, 7), (3, 8, 8)])
        with node:
            rc, out, err, rec, _ = run_wait(
                [("val0", node)], json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"})
            )
        self.assertEqual(rc, 0, err)
        self.assertEqual(rec["outcome"], "durable")
        self.assertEqual(node.answered, 4)
        self.assertIn("WARNING", rec["note"])

    def test_metric_absent_falls_back_to_sleeping_interval_plus_margin(self):
        old = FakeNode([(0, None, None)])
        half = FakeNode([(0, 4, None)])
        for node in (old, half):
            with self.subTest(node=node.steps), node:
                rc, out, err, rec, wall = run_wait(
                    [("val0", node)],
                    json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                    extra=["--fallback-margin-s", "0.4"],
                )
                self.assertEqual(rc, 0, err)
                self.assertEqual(rec["outcome"], "fallback_sleep")
                self.assertGreaterEqual(wall, 0.7)
                self.assertGreaterEqual(rec["waited_s"], 0.7)
                # A node without the metrics is slept on, not polled.
                self.assertEqual(node.hits, 1)
                self.assertTrue(rec["nodes"]["val0"]["missing"])
                self.assertIn("absent", out)
                self.assertIn("slept", out)
                self.assertIn("no durable evidence", rec["note"])

    def test_mixed_fleet_still_checks_the_nodes_that_report_the_metrics(self):
        """Codex stop review: an older binary on one node must not excuse a
        stalled checkpoint on another: val1 (metrics, write stuck below C0+1)
        is a timeout even though val0 can only be slept on."""
        old = FakeNode([(0, None, None)])
        stalled = FakeNode([(0, 9, 8)])
        with old, stalled:
            rc, out, err, rec, _ = run_wait(
                [("val0", old), ("val1", stalled)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.2", "--timeout-s", "1.0"],
            )
        self.assertEqual(rc, 3, out + err)
        self.assertEqual(rec["outcome"], "timeout")
        self.assertFalse(rec["nodes"]["val1"]["durable"])
        self.assertIn("val1 ckpt 9->8 NOT durable", out)

    def test_mixed_fleet_waits_for_both_the_sleep_and_the_durable_checkpoint(self):
        old = FakeNode([(0, None, None)])
        slow = FakeNode([(0, 9, 9), (5, 10, 10)])
        with old, slow:
            rc, out, err, rec, wall = run_wait(
                [("val0", old), ("val1", slow)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.2"] + LONG,
            )
        self.assertEqual(rc, 0, out + err)
        self.assertEqual(rec["outcome"], "fallback_sleep")
        self.assertTrue(rec["nodes"]["val1"]["durable"])
        self.assertEqual(slow.answered, 6)
        self.assertGreaterEqual(wall, 0.5)
        self.assertIn("val1 ckpt 9->10 durable", out)
        self.assertIn("absent on val0", out)

    def test_metric_absent_with_checkpoints_off_skips_without_sleeping(self):
        old = FakeNode([(0, None, None)])
        with old:
            rc, out, err, rec, _ = run_wait(
                [("val0", old)], json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"})
            )
        self.assertEqual(rc, 0, err)
        self.assertEqual(rec["outcome"], "skipped")
        # One scrape and no fallback sleep (that would be >= 15 s here); the
        # bound is loose on purpose: a loaded host may take a while to scrape.
        self.assertEqual(old.hits, 1)
        self.assertLess(rec["waited_s"], 10.0)

    def test_checkpoints_off_but_writes_counted_is_activity_not_a_skip(self):
        # s77 re-review (Codex P2): SUBMITS absent, WRITES=9, env resolves
        # off: the writes are evidence that checkpoints ran, so this is not
        # "checkpoints off". The missing SUBMITS leaves the node unverifiable
        # (fallback_sleep, unpinned digest), never a skip. The negative margin
        # only shortens the 15 s default interval's fallback sleep.
        node = FakeNode([(0, None, 9)])
        with node:
            rc, out, err, rec, _ = run_wait(
                [("val0", node)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
                extra=["--fallback-margin-s", "-14.8"] + LONG,
            )
        self.assertEqual(rc, 0, out + err)
        self.assertEqual(rec["outcome"], "fallback_sleep", out)
        self.assertFalse(rec["nodes"]["val0"]["durable"])

    def test_checkpoints_off_unreadable_node_with_writes_elsewhere_is_waited_on(self):
        # Same evidence rule in the retry decision: WRITES>0 on an answering
        # node keeps asking the unreadable one instead of skipping.
        ran = FakeNode([(0, None, 9)])
        dead = FakeNode([(0, 0, 0)], fail=True)
        with ran, dead:
            rc, out, err, rec, _ = run_wait(
                [("val0", ran), ("val1", dead)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
                extra=["--timeout-s", "1"],
            )
        self.assertNotEqual(rec["outcome"], "skipped", out)
        self.assertGreater(dead.hits, 1)

    def test_counter_validation_invalid_sample_with_checkpoints_off_never_skips(self):
        # 18c msg 121 negative-off: interval off, SUBMITS=-1, WRITES=0 was
        # "skipped" (ACCEPT). An invalid sample makes the node's evidence
        # invalid for the whole scrape: fallback_sleep (UNVERIFIED), no skip.
        # The negative margin only shortens the 15 s default fallback sleep.
        bad = FakeNode([], raw=["%s -1" % SUBMITS, "%s 0" % WRITES])
        with bad:
            rc, out, err, rec, _ = run_wait(
                [("val0", bad)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
                extra=["--fallback-margin-s", "-14.8"] + LONG,
            )
        self.assertEqual(rec["outcome"], "fallback_sleep", out + err)
        self.assertFalse(rec["nodes"]["val0"]["durable"])
        self.assertTrue(rec["nodes"]["val0"]["invalid"])

    def test_counter_validation_tiny_negative_is_not_rounded_to_zero(self):
        # 18c msg 121 underflow-on: float("-1e-999") == -0.0 passed as 0, so
        # SUBMITS=0, WRITES=1 was durable. Exact decimal parsing rejects it.
        bad = FakeNode([], raw=["%s -1e-999" % SUBMITS, "%s 1" % WRITES])
        with bad:
            rc, out, err, rec, _ = run_wait(
                [("val0", bad)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.1"] + LONG,
            )
        self.assertEqual(rec["outcome"], "fallback_sleep", out + err)
        self.assertFalse(rec["nodes"]["val0"]["durable"])

    def test_counter_validation_duplicate_samples_invalidate_the_scrape(self):
        # 18c msg 121 duplicate-invalid-on: SUBMITS 0 then SUBMITS -1 kept the
        # valid 0 (durable). Any duplicate, valid or not, invalidates the
        # node's evidence for that scrape.
        for second in ("-1", "0"):
            with self.subTest(second=second):
                bad = FakeNode([], raw=["%s 0" % SUBMITS, "%s %s" % (SUBMITS, second),
                                        "%s 1" % WRITES])
                with bad:
                    rc, out, err, rec, _ = run_wait(
                        [("val0", bad)],
                        json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                        extra=["--fallback-margin-s", "0.1"] + LONG,
                    )
                self.assertEqual(rec["outcome"], "fallback_sleep", out + err)
                self.assertFalse(rec["nodes"]["val0"]["durable"])

    def test_counter_validation_malformed_or_labelled_lines_count_as_samples(self):
        # Codex stop review: a counter line without a value, or a labelled
        # series of the same metric, was skipped before duplicate detection,
        # so "SUBMITS 0" next to it stayed the single valid sample (durable).
        # Every line of a counter is a sample; a malformed one is invalid.
        for raw in (["%s 0" % SUBMITS, SUBMITS, "%s 1" % WRITES],
                    ["%s 0" % SUBMITS, '%s{x="y"} 5' % SUBMITS, "%s 1" % WRITES],
                    ["%s 0" % SUBMITS, "%s 1" % WRITES, WRITES],
                    [SUBMITS, "%s 1" % WRITES],
                    ['%s{x="y"} 0' % SUBMITS, "%s 1" % WRITES]):
            with self.subTest(raw=raw):
                bad = FakeNode([], raw=raw)
                with bad:
                    rc, out, err, rec, _ = run_wait(
                        [("val0", bad)],
                        json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                        extra=["--fallback-margin-s", "0.1"] + LONG,
                    )
                self.assertEqual(rec["outcome"], "fallback_sleep", out + err)
                self.assertFalse(rec["nodes"]["val0"]["durable"])
                self.assertTrue(rec["nodes"]["val0"]["invalid"])

    def test_counter_validation_rejects_fractions_nan_inf_and_out_of_range(self):
        for value in ("0.5", "NaN", "+Inf", "inf", "18446744073709551616", "1e400", "0x10", ""):
            with self.subTest(value=value):
                bad = FakeNode([], raw=["%s %s" % (SUBMITS, value), "%s 1" % WRITES])
                with bad:
                    rc, out, err, rec, _ = run_wait(
                        [("val0", bad)],
                        json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                        extra=["--fallback-margin-s", "0.1"] + LONG,
                    )
                self.assertNotEqual(rec["outcome"], "durable", out + err)
                self.assertNotEqual(rec["outcome"], "skipped", out + err)

    def test_counter_validation_invalid_rescrape_never_makes_a_node_durable(self):
        # The poll path: a valid T0 (9, 9) then rescrapes whose WRITES would
        # reach the target 10 but whose scrape holds an invalid sample. The
        # whole scrape is void, so the node stays NOT durable (timeout).
        for raw in (["%s 9" % SUBMITS, "%s 10" % WRITES, "%s 10" % WRITES],
                    ["%s -1" % SUBMITS, "%s 10" % WRITES]):
            with self.subTest(raw=raw):
                node = FakeNode([(0, 9, 9)], raw=raw, raw_from=1)
                with node:
                    rc, out, err, rec, _ = run_wait(
                        [("val0", node)],
                        json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                        extra=["--timeout-s", "3"],
                    )
                self.assertEqual(rec["outcome"], "timeout", out + err)
                self.assertFalse(rec["nodes"]["val0"]["durable"])
                self.assertEqual(rec["nodes"]["val0"]["write_count"], 9)
                self.assertGreater(node.answered, 1)

    def test_counter_validation_invalid_node_is_reported_as_invalid_not_absent(self):
        # Both metrics were served, so "absent (older binary)" is wrong for
        # this node: it is named as invalid, with the offending samples.
        bad = FakeNode([], raw=["%s 0" % SUBMITS, "%s 0" % SUBMITS, "%s 1" % WRITES])
        with bad:
            rc, out, err, rec, _ = run_wait(
                [("val0", bad)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.1"] + LONG,
            )
        self.assertEqual(rec["outcome"], "fallback_sleep", out + err)
        for text in (out, rec["note"]):
            self.assertIn("invalid counter samples on val0 (%s: 2 samples)" % SUBMITS, text)
            self.assertNotIn("older binary", text)
            self.assertIn("no durable evidence on val0", text)

    def test_counter_validation_invalid_and_unreadable_with_checkpoints_off(self):
        # Checkpoints off, val0 invalid, val1 unreadable: no skip (timeout on
        # the unreadable node), and the note names the invalid node instead of
        # an empty "but  ran checkpoints" warning.
        bad = FakeNode([], raw=["%s -1" % SUBMITS, "%s 0" % WRITES])
        dead = FakeNode([], fail=True)
        with bad, dead:
            rc, out, err, rec, _ = run_wait(
                [("val0", bad), ("val1", dead)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
                extra=["--timeout-s", "1.5"],
            )
        self.assertEqual(rec["outcome"], "timeout", out + err)
        self.assertNotIn("ran checkpoints", rec["note"])
        self.assertIn("invalid counter samples on val0 (%s=-1)" % SUBMITS, rec["note"])
        self.assertIn("metrics unreadable on val1", out)

    def test_counter_validation_rejects_non_ascii_digits(self):
        # Decimal() accepts any Unicode digit; a Prometheus counter is ASCII.
        bad = FakeNode([], raw=["%s 0" % SUBMITS, "%s \u0661" % WRITES])
        with bad:
            rc, out, err, rec, _ = run_wait(
                [("val0", bad)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.1"] + LONG,
            )
        self.assertEqual(rec["outcome"], "fallback_sleep", out + err)
        self.assertFalse(rec["nodes"]["val0"]["durable"])

    def test_counter_validation_accepts_plain_and_exponent_integers(self):
        # Valid encodings a Prometheus exporter may emit: integral decimals
        # (including 1e1 and 21.0) still count.
        for submits, writes in (("9", "10"), ("9.0", "1e1"), ("+9", "10")):
            with self.subTest(submits=submits, writes=writes):
                node = FakeNode([], raw=["%s %s" % (SUBMITS, submits), "%s %s" % (WRITES, writes)])
                with node:
                    rc, out, err, rec, _ = run_wait([("val0", node)], ON, extra=LONG)
                self.assertEqual(rec["outcome"], "durable", out + err)

    def test_negative_counters_are_not_durable_proof(self):
        # s77 re-review (Codex P3): SUBMITS=-1, WRITES=0 gave target 0 and an
        # immediate "durable". A counter must be a finite non-negative
        # integer; anything else is unusable, so the node is unverifiable.
        bad = FakeNode([(0, -1, 0)])
        with bad:
            rc, out, err, rec, _ = run_wait(
                [("val0", bad)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "300"}),
                extra=["--fallback-margin-s", "0.1"] + LONG,
            )
        self.assertNotEqual(rec["outcome"], "durable", out)
        self.assertFalse(rec["nodes"]["val0"]["durable"])
        self.assertIn(rec["outcome"], ("fallback_sleep", "timeout"))

    def test_checkpoints_off_and_unreadable_metrics_skip_without_retrying(self):
        dead = FakeNode([(0, 0, 0)], fail=True)
        with dead:
            rc, out, err, rec, _ = run_wait(
                [("val0", dead)], json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"})
            )
        self.assertEqual(rc, 0, err)
        self.assertEqual(rec["outcome"], "skipped")
        self.assertEqual(dead.hits, 1)

    def test_env_off_but_a_readable_node_checkpoints_does_not_skip_an_unreadable_one(
        self,
    ):
        """Codex stop review: env off, val1 unreadable, val0 ran checkpoints.
        The node is the truth (checkpoints are on): val1 must be retried and,
        unreadable until the deadline, the wait is a timeout, never skipped."""
        good = FakeNode([(0, 7, 7), (1, 8, 8)])
        dead = FakeNode([(0, 7, 7)], fail=True)
        with good, dead:
            rc, out, err, rec, _ = run_wait(
                [("val0", good), ("val1", dead)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
                extra=["--timeout-s", "0.6"],
            )
        self.assertEqual(rc, 3, out + err)
        self.assertEqual(rec["outcome"], "timeout")
        # The first pass and at least one retry pass (the retry pass scrapes
        # before it checks the deadline).
        self.assertGreater(dead.hits, 1)

    def test_env_off_unreadable_node_that_recovers_is_waited_on(self):
        good = FakeNode([(0, 7, 7), (1, 8, 8)])
        # 503 for its first 3 requests, then the baseline (4, 4), then (5, 5).
        flaky = FakeNode([(0, 4, 4), (1, 5, 5)], fail_first=3)
        with good, flaky:
            rc, out, err, rec, _ = run_wait(
                [("val0", good), ("val1", flaky)],
                json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
                extra=LONG,
            )
        self.assertEqual(rc, 0, out + err)
        self.assertEqual(rec["outcome"], "durable")
        self.assertEqual(rec["nodes"]["val1"]["c0"], 4)
        self.assertTrue(rec["nodes"]["val1"]["durable"])
        self.assertEqual(flaky.hits, 5)
        self.assertIn("WARNING", rec["note"])

    def test_default_timeout_is_two_intervals_plus_30s(self):
        node = FakeNode([(0, 1, 1), (1, 2, 2)])
        with node:
            _, _, err, rec, _ = run_wait(
                [("val0", node)], json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "2000"})
            )
        self.assertEqual(rec["timeout_s"], 34.0, err)

    def test_timeout_is_a_recorded_failure_not_a_pass(self):
        node = FakeNode([(0, 9, 9)])
        good = FakeNode([(0, 9, 9), (1, 10, 10)])
        with node, good:
            # val0 needs one poll after its baseline: 4 s leaves room for a
            # loaded host; val1 never gets there.
            rc, out, err, rec, wall = run_wait(
                [("val0", good), ("val1", node)], extra=["--timeout-s", "4"]
            )
        self.assertEqual(rc, 3, err)
        self.assertEqual(rec["outcome"], "timeout")
        self.assertFalse(rec["nodes"]["val1"]["durable"])
        self.assertTrue(rec["nodes"]["val0"]["durable"])
        self.assertEqual(rec["nodes"]["val1"]["write_count"], 9)
        self.assertGreaterEqual(wall, 4)
        self.assertIn("TIMEOUT", out)
        self.assertIn("val1 ckpt 9->9 NOT durable", out)

    def test_unreadable_metrics_until_the_deadline_is_a_timeout(self):
        dead = FakeNode([(0, 1, 1)], fail=True)
        with dead:
            rc, out, err, rec, _ = run_wait(
                [("val0", dead)], extra=["--timeout-s", "0.5"]
            )
        self.assertEqual(rc, 3, err)
        self.assertEqual(rec["outcome"], "timeout")
        self.assertIsNone(rec["nodes"]["val0"]["c0"])

    def test_resolution_mirrors_the_node(self):
        sys.path.insert(0, str(HERE))
        try:
            import ckpt_wait
        finally:
            sys.path.pop(0)
        r = ckpt_wait.resolve_interval_ms
        self.assertEqual(r({}, "classic"), 15000)
        self.assertEqual(r({}, ""), 15000)
        self.assertIsNone(r({}, "level_authority"))
        self.assertIsNone(r({}, "level_authority_chunked"))
        self.assertEqual(r({}, "order_rows"), 15000)
        self.assertIsNone(r({"TORUS_BOOK_ROWS": " 3 "}, ""))
        self.assertEqual(r({"TORUS_BOOK_ROWS": "3"}, "classic"), 15000)
        self.assertEqual(
            r({"TORUS_CHECKPOINT_INTERVAL_MS": "  9000\t"}, "classic"), 9000
        )
        self.assertEqual(r({"TORUS_CHECKPOINT_INTERVAL_MS": "+500"}, "classic"), 500)
        self.assertEqual(
            r({"TORUS_CHECKPOINT_INTERVAL_MS": "20000"}, "level_authority"), 20000
        )
        for off in ("0", "-5", "abc", "1.5", "99999999999999999999"):
            self.assertIsNone(r({"TORUS_CHECKPOINT_INTERVAL_MS": off}, "classic"), off)
        self.assertIsNone(r({"TORUS_EXEC_PIPELINE": " 0 "}, "classic"))


# ------------------------------------------------------------- run-cell.sh
def run_cell_function(live, mets, node_env=ON, extra_env=None):
    """Execute run-cell.sh's real digest_ckpt_wait() with LIVE_IDX / METS."""
    script = RUN_CELL.read_text()
    start = script.index("digest_ckpt_wait() {\n")
    end = script.index("\n}\n", start) + 3
    with tempfile.TemporaryDirectory() as d:
        command = """set -uo pipefail
OUT=%s
SELF_DIR=%s
LIVE_IDX=(%s)
METS=(%s)
NODE_ENV_JSON=%s
GEN_BOOK_MODE_GOT=classic
log() { printf '[t] %%s\\n' "$*" | tee -a "$OUT/run.log"; }
%s
digest_ckpt_wait
printf 'OUTCOME=%%s\\n' "$DIGEST_CKPT_WAIT"
""" % (
            d,
            HERE,
            " ".join(map(str, live)),
            " ".join(map(str, mets)),
            "'" + node_env + "'",
            script[start:end],
        )
        env = dict(os.environ, **(extra_env or {}))
        r = subprocess.run(
            ["bash", "-c", command], capture_output=True, text=True, timeout=120, env=env
        )
        rec_path = Path(d, "digest-ckpt-wait.json")
        rec = json.loads(rec_path.read_text()) if rec_path.exists() else None
        return r, rec


class RunCellTests(unittest.TestCase):
    def test_dead_node_is_not_waited_on(self):
        v0 = FakeNode([(0, 5, 5), (1, 6, 6)])
        v2 = FakeNode([(0, 5, 5), (1, 6, 6)])
        stuck = FakeNode([(0, 5, 5)])
        with v0, v2, stuck:
            r, rec = run_cell_function(
                [0, 2],
                [v0.port, stuck.port, v2.port],
                extra_env={"DIGEST_CKPT_POLL_S": "0.05"},
            )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("OUTCOME=durable", r.stdout)
        self.assertEqual(sorted(rec["nodes"]), ["val0", "val2"])
        self.assertEqual(stuck.hits, 0)
        self.assertRegex(
            r.stdout,
            r"digest checkpoint wait: val0 ckpt 5->6 durable, val2 ckpt 5->6 durable",
        )
        self.assertNotIn("WARNING", r.stdout)

    def test_timeout_logs_loudly_and_sets_the_outcome(self):
        stuck = FakeNode([(0, 5, 5)])
        with stuck:
            r, rec = run_cell_function(
                [0],
                [stuck.port, stuck.port, stuck.port],
                extra_env={
                    "DIGEST_CKPT_TIMEOUT_S": "0.5",
                    "DIGEST_CKPT_POLL_S": "0.05",
                },
            )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("OUTCOME=timeout", r.stdout)
        self.assertIn("WARNING", r.stdout)
        self.assertEqual(rec["outcome"], "timeout")

    def test_fallback_sleep_logs_a_warning_too(self):
        """s137 review [P2]: sleeping on an older binary is no durable
        evidence; run.log says so like a timeout."""
        old = FakeNode([(0, None, None)])
        with old:
            r, rec = run_cell_function(
                [0],
                [old.port] * 3,
                node_env=json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "100"}),
            )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("OUTCOME=fallback_sleep", r.stdout)
        self.assertEqual(rec["outcome"], "fallback_sleep")
        self.assertIn("WARNING: digest checkpoint wait outcome=fallback_sleep", r.stdout)
        self.assertIn("no durable evidence", r.stdout)

    def test_helper_failure_is_recorded_as_error(self):
        node = FakeNode([(0, 5, 5)])
        with node:
            r, rec = run_cell_function(
                [0], [node.port, node.port, node.port], node_env="not json"
            )
        self.assertIn("OUTCOME=error", r.stdout)
        self.assertIn("WARNING", r.stdout)
        self.assertEqual(rec["outcome"], "error")

    def test_checkpoints_off_is_one_line_and_no_wait(self):
        node = FakeNode([(0, 0, 0)])
        with node:
            r, rec = run_cell_function(
                [0, 1, 2],
                [node.port] * 3,
                node_env=json.dumps({"TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
            )
        self.assertIn("OUTCOME=skipped", r.stdout)
        # One baseline scrape per live node, no polling.
        self.assertEqual(node.hits, 3)
        self.assertEqual(r.stdout.count("digest checkpoint wait"), 1, r.stdout)
        self.assertNotIn("WARNING", r.stdout)

    def test_wait_sits_after_the_drain_and_settle_and_before_the_digest(self):
        script = RUN_CELL.read_text()
        call = script.index("\ndigest_ckpt_wait\n")
        self.assertLess(script.index('log "drained=$DRAINED'), call)
        self.assertLess(
            script.index('log "oracle feed paused (SIGSTOP) for the digest; settle'),
            call,
        )
        self.assertLess(call, script.index("if Q_BEFORE=$(funnel_snapshot)"))
        self.assertLess(call, script.index('"$TOOLS_DIR/digest-node.sh" "${RPCS[$i]}"'))
        self.assertLess(
            call, script.index('rpc "${RPCS[$i]}" torus_getLiquidatorVault')
        )
        self.assertLess(call, script.index('agree_cmp_height "$HCMP"'))


# ------------------------------------------------------------- summarize.py
sys.path.insert(0, str(HERE))
from test_harness import run_summarize, write_agreement, write_cell  # noqa: E402

sys.path.pop(0)


class SummarizeTests(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="ckpt-wait-sum-")

    def tearDown(self):
        import shutil

        shutil.rmtree(self.d, ignore_errors=True)

    def write_wait(self, outcome, rec=None):
        if rec is None:
            rec = {
                "outcome": outcome,
                "interval_ms": 15000,
                "timeout_s": 60.0,
                "waited_s": 60.1 if outcome == "timeout" else 3.4,
                "nodes": {
                    "val0": {"c0": 21, "write_count": 21, "durable": False, "missing": []}
                },
            }
        with open(os.path.join(self.d, "digest-ckpt-wait.json"), "w") as f:
            json.dump(rec, f)

    def summarize(self, digests):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, digests)
        r = run_summarize(self.d, extra=["--digest-quiescent", "1"])
        return r

    def ckpt_reasons(self, s):
        return [
            x
            for x in s["validity"]["unverified_reasons"] + s["validity"]["fail_reasons"]
            if "checkpoint wait" in x
        ]

    def test_timeout_with_unequal_digests_is_unverified_not_a_fork(self):
        self.write_wait("timeout")
        s, out = self.summarize(["a", "b", "b"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertIsNone(s["headline"]["validators_agree"])
        self.assertEqual(s["agreement"]["state_digest_ckpt_wait"]["outcome"], "timeout")
        self.assertEqual(s["headline"]["digest_ckpt_wait"], "timeout")
        self.assertIn("digest_ckpt=timeout", out)

    def test_timeout_with_equal_digests_is_not_accepted(self):
        self.write_wait("error")
        s, _ = self.summarize(["same"] * 3)
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertFalse(s["validity"]["accepted"])
        self.assertTrue(self.ckpt_reasons(s), s["validity"])

    def test_durable_wait_keeps_the_verdicts(self):
        for outcome in ("durable", "skipped"):
            with self.subTest(outcome=outcome):
                self.write_wait(outcome)
                s, out = self.summarize(["a", "b", "b"])
                self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
                s, out = self.summarize(["same"] * 3)
                self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
                self.assertFalse(self.ckpt_reasons(s), s["validity"])
                self.assertIn("digest_ckpt=%s" % outcome, out)

    def test_fallback_sleep_does_not_pin_the_digest(self):
        """s137 review [P2]: a node without the metrics (older binary) was
        only slept on; its checkpoints may never have advanced. No durable
        evidence -> digest unpinned, cell UNVERIFIED (never ACCEPT)."""
        rec = {
            "outcome": "fallback_sleep",
            "interval_ms": 15000,
            "nodes": {
                "val0": {"c0": None, "write_count": None, "durable": False,
                         "missing": [SUBMITS, WRITES]},
                "val1": {"c0": 9, "write_count": 10, "durable": True, "missing": []},
                "val2": {"c0": 4, "write_count": None, "durable": False,
                         "missing": [WRITES]},
            },
        }
        self.write_wait(None, rec)
        s, out = self.summarize(["a", "b", "b"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertIn("digest_ckpt=fallback_sleep", out)
        s, _ = self.summarize(["same"] * 3)
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertFalse(s["validity"]["accepted"])
        self.assertEqual(s["validity"]["verdict"], "UNVERIFIED")
        self.assertIn(
            "state digest checkpoint wait fallback_sleep: no durable evidence on "
            "val0 val2 (older binary)",
            self.ckpt_reasons(s),
        )

    def test_record_without_an_outcome_is_a_failed_wait(self):
        """s137 review [P3]: an existing {} record is not "no record"."""
        for rec in ({}, {"note": "x"}, {"outcome": "bogus"}, {"outcome": None}):
            with self.subTest(rec=rec):
                self.write_wait(None, rec)
                s, out = self.summarize(["same"] * 3)
                self.assertFalse(s["validity"]["accepted"], s["validity"])
                self.assertEqual(s["headline"]["digest_ckpt_wait"], "error")
                self.assertIn("digest_ckpt=error", out)
                self.assertTrue(self.ckpt_reasons(s), s["validity"])
                s, _ = self.summarize(["a", "b", "b"])
                self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")

    def test_cell_without_the_record_is_unchanged(self):
        s, out = self.summarize(["same"] * 3)
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertIsNone(s["agreement"]["state_digest_ckpt_wait"])
        self.assertIsNone(s["headline"]["digest_ckpt_wait"])
        self.assertIn("digest_ckpt=None", out)


if __name__ == "__main__":
    unittest.main()
