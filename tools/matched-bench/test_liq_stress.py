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
    "scrape_valid",
]


def write_stress_cell(d, liq_col=True, shock=True, vault=True):
    """60 s at 10 blk/s. Shock at T0+20; liquidations +5/s on T0+21..T0+30
    (last increase at height 300, shock at height 200 -> 100 blocks). Post
    engine tail 2 ms/block before the shock, 10 ms/block in the window. Exec
    lag 1 before, peaking at 7 in the window."""
    cols = COLS if liq_col else [c for c in COLS if "liquid" not in c]
    tail_s, tail_n = 0.0, 0
    with open(os.path.join(d, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for s in range(61):
            ts = T0 + s
            in_window = 20 < s <= 30
            if s > 0:
                tail_n += 10
                tail_s += 10 * (0.010 if in_window else 0.002)
            liqs = 5 * min(max(s - 20, 0), 10)
            row = {
                "torus_block_height": 10 * s,
                "torus_exec_queue_depth": 7 if s == 25 else (3 if in_window else 1),
                "torus_exec_post_engine_tail_seconds_sum": "%.6f" % tail_s,
                "torus_exec_post_engine_tail_seconds_count": tail_n,
                "torus_liquidations_triggered_total": liqs,
                "torus_liquidator_vault_deficit": "12.5" if s > 25 else "0",
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
        with open(os.path.join(d, "metrics-after-val%d.txt" % i), "w") as f:
            f.write(
                "torus_liquidations_triggered_total 50\ntorus_liquidator_vault_deficit 12.5\n"
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
        # What row 76 still cannot get from this cell is named, never guessed.
        joined = " ".join(r["missing"])
        self.assertIn("liquidation step timer", joined)
        self.assertIn("no liquidatable account", joined)
        self.assertIn("ADL", joined)

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
