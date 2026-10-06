#!/usr/bin/env python3
"""Row 76 liquidation-stress analysis (liq_stress.py) on synthetic cell dirs."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from liq_stress import analyze  # noqa: E402

T0 = 1_000
COLS = [
    "torus_block_height",
    "torus_exec_queue_depth",
    "torus_exec_post_engine_tail_seconds_sum",
    "torus_exec_post_engine_tail_seconds_count",
    "torus_liquidations_triggered_total",
    "torus_liquidator_vault_deficit",
    "torus_liquidation_step_seconds_sum",
    "torus_liquidation_step_seconds_count",
    "torus_liquidations_stage1_total",
    "torus_liquidations_backstop_total",
    "torus_liquidations_adl_total",
    "torus_liquidation_scanned_total",
    "torus_liquidation_acted_total",
    "torus_liquidation_pending",
    "torus_liquidation_deferred",
    "scrape_valid",
]
# The liquidation-telemetry columns (feat/liq-telemetry); older binaries lack them.
TEL = [c for c in COLS if c.startswith("torus_liquidation_") or c.startswith("torus_liquidations_") and c != "torus_liquidations_triggered_total"]
# Pending after each sample s (val0 and the others alike): 0 before the shock,
# 40 at s=21 falling by 4 per second, 0 again from s=30 (height 300).
PENDING = {s: 40 - 4 * (s - 21) for s in range(21, 30)}


def write_stress_cell(d, liq_col=True, shock=True, vault=True, tel_col=True, pending_tail=None):

    """60 s at 10 blk/s. Shock at T0+20; liquidations +5/s on T0+21..T0+30
    (last increase at height 300, shock at height 200 -> 100 blocks). Post
    engine tail 2 ms/block before the shock, 10 ms/block in the window. Exec
    lag 1 before, peaking at 7 in the window. Liquidation step 0.5 ms/block
    before, 4 ms/block in the window except 8 ms/block in second 22; per
    window second 3 stage-1, 1 backstop, 1 ADL, 20 scanned, 5 acted; pending
    from PENDING (pending_tail: the value from s=30 on, default 0); deferred
    12 in seconds 21..22."""
    cols = COLS if liq_col else [c for c in COLS if "liquid" not in c]
    if not tel_col:
        cols = [c for c in cols if c not in TEL]
    tail_s, tail_n = 0.0, 0
    step_s = 0.0
    with open(os.path.join(d, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for s in range(61):
            ts = T0 + s
            in_window = 20 < s <= 30
            if s > 0:
                tail_n += 10
                tail_s += 10 * (0.010 if in_window else 0.002)
                step_s += 10 * ((0.008 if s == 22 else 0.004) if in_window else 0.0005)
            liqs = 5 * min(max(s - 20, 0), 10)
            k = min(max(s - 20, 0), 10)
            pend = PENDING.get(s, 0)
            if s >= 30 and pending_tail is not None:
                pend = pending_tail
            row = {
                "torus_block_height": 10 * s,
                "torus_exec_queue_depth": 7 if s == 25 else (3 if in_window else 1),
                "torus_exec_post_engine_tail_seconds_sum": "%.6f" % tail_s,
                "torus_exec_post_engine_tail_seconds_count": tail_n,
                "torus_liquidations_triggered_total": liqs,
                "torus_liquidator_vault_deficit": "12.5" if s > 25 else "0",
                "torus_liquidation_step_seconds_sum": "%.6f" % step_s,
                "torus_liquidation_step_seconds_count": tail_n,
                "torus_liquidations_stage1_total": 3 * k,
                "torus_liquidations_backstop_total": k,
                "torus_liquidations_adl_total": k,
                "torus_liquidation_scanned_total": 20 * k,
                "torus_liquidation_acted_total": 5 * k,
                "torus_liquidation_pending": pend,
                "torus_liquidation_deferred": 12 if s in (21, 22) else 0,
                "scrape_valid": 1,
            }
            for n in ("val0", "val1", "val2"):
                f.write("%d,%s,%s\n" % (ts, n, ",".join(str(row[c]) for c in cols)))
            if s == 40:  # a failed scrape: zeros everywhere, must be ignored
                f.write("%d,val0,%s\n" % (ts, ",".join("0" for _ in cols)))
    for i in range(3):
        with open(os.path.join(d, "metrics-before-val%d.txt" % i), "w") as f:
            f.write(
                "# HELP x\ntorus_liquidations_triggered_total 0\ntorus_liquidator_vault_deficit 0\n"
            )
            if tel_col:
                f.write(
                    "torus_liquidations_stage1_total 0\ntorus_liquidations_backstop_total 0\n"
                    "torus_liquidations_adl_total 0\ntorus_liquidation_scanned_total 7\n"
                    "torus_liquidation_acted_total 0\n"
                )
        with open(os.path.join(d, "metrics-after-val%d.txt" % i), "w") as f:
            f.write(
                "torus_liquidations_triggered_total 50\ntorus_liquidator_vault_deficit 12.5\n"
            )
            if tel_col:
                f.write(
                    "torus_liquidations_stage1_total 30\ntorus_liquidations_backstop_total 10\n"
                    "torus_liquidations_adl_total 10\ntorus_liquidation_scanned_total 207\n"
                    "torus_liquidation_acted_total 50\ntorus_liquidation_step_seconds_count 600\n"
                )
        if vault:
            with open(os.path.join(d, "vault-val%d.json" % i), "w") as f:
                json.dump(
                    {
                        "address": "0x746f",
                        "availableBalance": "-12.5",
                        "deficit": "12.5",
                        "openPositions": 4,
                    },
                    f,
                )
    with open(os.path.join(d, "oracle-feed.log"), "w") as f:
        f.write("[oracle-feed] 10 markets at 30000 TRS\n")
        if shock:
            f.write(
                "[oracle-feed] shock round 10 at %d ms: odd markets +400 bp, even markets -400 bp\n"
                % ((T0 + 20) * 1000 + 300)
            )
    summary = {
        "cell": {"liq_thin": 200, "liq_thin_avail": "1000000.0"},
        "oracle_feed": {
            "shock_bp": 400 if shock else 0,
            "shock_round": 10 if shock else 0,
        },
    }
    with open(os.path.join(d, "summary.json"), "w") as f:
        json.dump(summary, f)


class LiqStressTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="liqstress-")

    def tearDown(self):
        shutil.rmtree(self.d)

    def test_full_stress_cell(self):
        write_stress_cell(self.d)
        r = analyze(self.d)
        self.assertEqual(
            (r["liq_thin"], r["liq_thin_avail"], r["shock_bp"], r["shock_round"]),
            (200, "1000000.0", 400, 10),
        )
        for n in ("val0", "val1", "val2"):
            self.assertEqual(
                r["liquidations"][n], {"before": 0, "after": 50, "delta": 50}
            )
            self.assertEqual(r["vault_deficit_after"][n], 12.5)
            self.assertEqual(r["vault"][n]["openPositions"], 4)
        self.assertEqual(r["shock"], {"round": 10, "unix_ms": (T0 + 20) * 1000 + 300})
        self.assertTrue(r["per_sample"])
        self.assertEqual(
            r["shock_height"], 210
        )  # first val0 sample at/after the shock ms
        self.assertEqual(r["first_liquidation"], {"ts": T0 + 21, "height": 210})
        self.assertEqual(r["last_liquidation"], {"ts": T0 + 30, "height": 300})
        self.assertEqual(r["blocks_shock_to_last_liquidation"], 90)
        tail = r["post_engine_tail_ms_per_block"]["val0"]
        self.assertAlmostEqual(tail["baseline"], 2.0, places=3)
        self.assertAlmostEqual(tail["window"], 10.0, places=3)
        lag = r["exec_lag"]["val0"]
        self.assertEqual(
            (lag["baseline_max"], lag["window_max"], lag["window_p50"]), (1, 7, 3)
        )
        # Liquidation telemetry: per-class counts from the snapshots.
        for n in ("val0", "val1", "val2"):
            self.assertEqual(
                r["liquidations_by_class"][n],
                {"stage1": 30, "backstop": 10, "adl": 10, "scanned": 200, "acted": 50},
            )
        # Step ms per block from the histogram deltas: 0.5 before; window
        # 10 blocks at 8 ms + 90 at 4 ms over s=21..30 = 4.4; max 8 (s=22).
        step = r["liquidation_step_ms_per_block"]["val0"]
        self.assertAlmostEqual(step["baseline"], 0.5, places=3)
        self.assertAlmostEqual(step["window"], 4.4, places=3)
        self.assertAlmostEqual(step["window_max"], 8.0, places=3)
        # Pending: peak 40 at s=21, back to 0 at s=30 (height 300): 90 blocks.
        p = r["pending"]
        self.assertEqual(p["peak"], {"ts": T0 + 21, "height": 210, "pending": 40})
        self.assertEqual(p["zero"], {"ts": T0 + 30, "height": 300})
        self.assertEqual(r["blocks_shock_to_pending_zero"], 90)
        tl = r["pending_timeline"]
        self.assertEqual(tl[0]["ts"], T0 + 20)  # the sample before the first pending > 0
        self.assertEqual(tl[-1], {"ts": T0 + 30, "height": 300, "pending": 0, "deferred": 0, "step_ms": 4.0})
        self.assertEqual(tl[1]["deferred"], 12)
        self.assertEqual(tl[2]["step_ms"], 8.0)
        # The node now exports the timer and the pending gauge; only the ADL
        # counterparty list (node log lines) stays outside this tool.
        joined = " ".join(r["missing"])
        self.assertNotIn("liquidation step timer", joined)
        self.assertNotIn("no liquidatable account", joined)
        self.assertIn("ADL", joined)

    def test_older_binary_without_liquidation_telemetry(self):
        write_stress_cell(self.d, tel_col=False)
        r = analyze(self.d)
        self.assertIsNone(r["blocks_shock_to_pending_zero"])
        self.assertEqual(r["pending_timeline"], [])
        self.assertIsNone(r["liquidation_step_ms_per_block"]["val0"])
        self.assertEqual(
            r["liquidations_by_class"]["val0"],
            {"stage1": None, "backstop": None, "adl": None, "scanned": None, "acted": None},
        )
        joined = " ".join(r["missing"])
        self.assertIn("liquidation step timer", joined)
        self.assertIn("no liquidatable account", joined)
        # The lower bound from the triggered counter is still there.
        self.assertEqual(r["blocks_shock_to_last_liquidation"], 90)

    def test_zero_filled_columns_from_an_older_binary_are_not_telemetry(self):
        # sample_metrics.py writes "0" for a metric the node does not export, so
        # the columns alone prove nothing: the after-snapshots decide.
        write_stress_cell(self.d)
        for i in range(3):
            with open(os.path.join(self.d, "metrics-after-val%d.txt" % i), "w") as f:
                f.write("torus_liquidations_triggered_total 50\n")
        r = analyze(self.d)
        self.assertEqual(r["pending_timeline"], [])
        self.assertIsNone(r["blocks_shock_to_pending_zero"])
        self.assertIsNone(r["liquidation_step_ms_per_block"]["val0"])
        joined = " ".join(r["missing"])
        self.assertIn("liquidation step timer", joined)
        self.assertIn("no liquidatable account", joined)

    def test_pending_that_never_returns_to_zero_is_reported(self):
        write_stress_cell(self.d, pending_tail=3)
        r = analyze(self.d)
        self.assertIsNone(r["pending"]["zero"])
        self.assertIsNone(r["blocks_shock_to_pending_zero"])
        self.assertEqual(r["pending"]["last"], {"ts": T0 + 60, "height": 600, "pending": 3})
        self.assertIn("pending never returned to 0", " ".join(r["missing"]))

    def test_default_cell_reports_absence_not_zeros(self):
        write_stress_cell(self.d, liq_col=False, shock=False, vault=False)
        r = analyze(self.d)
        self.assertFalse(r["per_sample"])
        self.assertIsNone(r["shock"])
        self.assertIsNone(r["blocks_shock_to_last_liquidation"])
        self.assertIsNone(r["first_liquidation"])
        self.assertEqual(r["vault"], {"val0": None, "val1": None, "val2": None})
        joined = " ".join(r["missing"])
        self.assertIn("sampler.csv has no torus_liquidations_triggered_total", joined)
        self.assertIn("vault-val", joined)
        # Before/after counters still come from the metric snapshots.
        self.assertEqual(r["liquidations"]["val0"]["delta"], 50)
        # Without a shock the window opens at the first liquidation.
        self.assertIsNotNone(r["post_engine_tail_ms_per_block"]["val0"]["baseline"])

    def test_cli_writes_liq_stress_json(self):
        write_stress_cell(self.d)
        p = subprocess.run(
            [sys.executable, os.path.join(HERE, "liq_stress.py"), self.d],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(p.returncode, 0, p.stderr)
        with open(os.path.join(self.d, "liq-stress.json")) as f:
            on_disk = json.load(f)
        self.assertEqual(json.loads(p.stdout), on_disk)
        self.assertEqual(on_disk["blocks_shock_to_last_liquidation"], 90)


if __name__ == "__main__":
    unittest.main(verbosity=2)
