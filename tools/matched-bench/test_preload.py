#!/usr/bin/env python3
"""Tests for preload.py (run-cell.sh PRELOAD=N, bench queue 8f): the
summary.json .preload record and its REJECT fold. Offline, no devnet."""

import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
PRELOAD_PY = os.path.join(HERE, "preload.py")
sys.path.insert(0, HERE)
import preload  # noqa: E402


def report(**kw):
    r = {
        "target": 20000,
        "markets": 10,
        "owners": 23,
        "owner_offset": 5060,
        "owner_end": 5083,
        "per_owner": 900,
        "batch": 900,
        "distance": 1000,
        "levels": 100,
        "actions": 23,
        "orders_placed": 20000,
        "actions_admitted": 23,
        "orders_accepted": 20000,
        "actions_failed": 0,
        "resends": 2,
        "resting": 20000,
        "resting_by_node": [20000, 20000, 20001],
        "placed_accepted_by_node": [20000, 20000, 20001],
        "resting_gauge_by_node": [20000, 20000, 20000],
        "reached": True,
        "submit_seconds": 1.5,
        "seconds": 4.2,
        "failures": [],
    }
    r.update(kw)
    return r


class PreloadRecordTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="preload-")
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name

    def write(self, name, obj):
        with open(os.path.join(self.d, name), "w") as f:
            json.dump(obj, f)

    def summary(self):
        with open(os.path.join(self.d, "summary.json")) as f:
            return json.load(f)

    def cell(self, **kw):
        c = {
            "target": 20000,
            "seconds": 7,
            "rc": 0,
            "boot_to_ready_s": 31,
            "markets": 10,
        }
        c.update(kw)
        self.write(preload.CELL_FILE, c)

    def accepted_summary(self):
        self.write(
            "summary.json",
            {
                "status": "OK",
                "label": "x",
                "validity": {
                    "verdict": "ACCEPT",
                    "accepted": True,
                    "fail_reasons": [],
                    "unverified_reasons": [],
                },
                "headline": {"benchmark_accepted": True},
            },
        )

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, PRELOAD_PY, *args],
            capture_output=True,
            text=True,
            timeout=30,
        )

    def test_reached_target_is_recorded_and_keeps_the_verdict(self):
        self.accepted_summary()
        self.cell()
        self.write(preload.REPORT_FILE, report())
        r = self.run_cli("summary", "--dir", self.d)
        self.assertEqual(r.returncode, 0, r.stderr)
        s = self.summary()
        p = s["preload"]
        self.assertTrue(p["ok"])
        self.assertEqual(p["problems"], [])
        self.assertEqual(
            (p["target"], p["resting"], p["orders_placed"], p["orders_accepted"]),
            (20000, 20000, 20000, 20000),
        )
        self.assertEqual((p["owners"], p["actions_failed"]), (23, 0))
        # The pre-load's own wall time, apart from boot -> ready.
        self.assertEqual((p["seconds"], p["boot_to_ready_s"]), (7, 31))
        self.assertEqual(s["validity"]["verdict"], "ACCEPT")
        self.assertTrue(s["validity"]["accepted"])
        self.assertEqual(s["status"], "OK")

    def test_short_of_target_rejects_the_cell(self):
        self.accepted_summary()
        self.cell(rc=1)
        self.write(
            preload.REPORT_FILE,
            report(
                resting=19000,
                reached=False,
                actions_failed=1,
                failures=["action 3 owner 5063: margin"],
            ),
        )
        self.assertEqual(self.run_cli("summary", "--dir", self.d).returncode, 0)
        s = self.summary()
        self.assertFalse(s["preload"]["ok"])
        probs = s["preload"]["problems"]
        self.assertIn("preload rc=1", probs)
        self.assertIn("resting 19000 < target 20000", probs)
        self.assertIn("1 actions failed", probs)
        v = s["validity"]
        self.assertEqual(v["verdict"], "REJECT")
        self.assertFalse(v["accepted"])
        self.assertTrue(any(f.startswith(preload.REASON) for f in v["fail_reasons"]))
        self.assertEqual(s["status"], "INVALID")
        self.assertFalse(s["headline"]["benchmark_accepted"])

    def test_fold_is_idempotent(self):
        self.accepted_summary()
        self.cell()
        self.write(preload.REPORT_FILE, report(resting=None, reached=False))
        for _ in range(2):
            self.assertEqual(self.run_cli("summary", "--dir", self.d).returncode, 0)
        v = self.summary()["validity"]
        self.assertEqual(
            sum(f.startswith(preload.REASON) for f in v["fail_reasons"]), 1
        )

    def test_missing_report_or_no_summary_yet_still_records_a_reject(self):
        # The failure path: run-cell.sh records before summarize.py ever ran.
        self.cell(rc=124)
        r = self.run_cli("summary", "--dir", self.d)
        self.assertEqual(r.returncode, 0, r.stderr)
        s = self.summary()
        self.assertFalse(s["preload"]["ok"])
        self.assertIn("no preload.json (bench report)", s["preload"]["problems"])
        self.assertEqual(s["validity"]["verdict"], "REJECT")

    def test_rc_zero_but_report_not_reached_is_not_ok(self):
        self.accepted_summary()
        self.cell()
        self.write(preload.REPORT_FILE, report(reached=False))
        self.run_cli("summary", "--dir", self.d)
        self.assertFalse(self.summary()["preload"]["ok"])

    def test_ok_predicate(self):
        self.assertEqual(
            preload.problems_of({"rc": 0, "target": 5, "report": report(resting=5)}), []
        )
        self.assertIn(
            "resting None < target 5",
            preload.problems_of({"rc": 0, "target": 5, "report": report(resting=None)}),
        )

    def test_owner_range_check(self):
        ok = preload.owner_range
        # 1M / 900 = 1112 owners at 5060..6172 in a 100k genesis (60..100060)
        self.assertEqual(ok(1_000_000, 900, 5060, 100_000, 0), (1112, 5060, 6172, None))
        self.assertIsNotNone(
            ok(1_000_000, 900, 99_000, 100_000, 0)[3]
        )  # past the funded end
        self.assertIsNotNone(
            ok(1_000_000, 900, 30, 100_000, 0)[3]
        )  # below the bulk range
        # funded spam keys take the top K of the funded range
        self.assertIsNone(ok(900, 900, 100_059, 100_000, 0)[3])
        self.assertIsNotNone(ok(900, 900, 100_059, 100_000, 1)[3])

    def test_cli_owner_range(self):
        r = self.run_cli(
            "owners",
            "--target",
            "1000000",
            "--per-owner",
            "900",
            "--owner-offset",
            "5060",
            "--bulk-count",
            "100000",
            "--spam-funded-keys",
            "0",
        )
        self.assertEqual(
            (r.returncode, r.stdout.split()), (0, ["1112", "5060", "6172"]), r.stderr
        )
        r = self.run_cli(
            "owners",
            "--target",
            "1000000",
            "--per-owner",
            "900",
            "--owner-offset",
            "99500",
            "--bulk-count",
            "100000",
            "--spam-funded-keys",
            "0",
        )
        self.assertEqual(r.returncode, 1)
        self.assertIn("not all genesis-funded", r.stderr)


if __name__ == "__main__":
    unittest.main()
