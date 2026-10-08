#!/usr/bin/env python3
"""Keep the emitted legacy phase layout aligned with its positional AWK reader."""

import csv
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

from sample_metrics import CsvSink, Response

# Independent legacy header-to-metric contract, including the trailing cache fields.
LEGACY = [
    ("committed", "torus_blocks_committed_total"),
    ("height", "torus_block_height"),
    ("placed", "torus_orders_placed_accepted_total"),
    ("matched", "torus_orders_matched_total"),
    ("resting", "torus_orders_resting_total"),
    ("exec_resting", "torus_exec_resting_orders"),
    ("lb_s", "torus_exec_load_books_seconds_sum"),
    ("lb_c", "torus_exec_load_books_seconds_count"),
    ("root_s", "torus_exec_root_seconds_sum"),
    ("root_c", "torus_exec_root_seconds_count"),
    ("sw_s", "torus_exec_state_write_seconds_sum"),
    ("sw_c", "torus_exec_state_write_seconds_count"),
    ("evm_s", "torus_exec_evm_resync_seconds_sum"),
    ("evm_c", "torus_exec_evm_resync_seconds_count"),
    ("fl_s", "torus_exec_flush_seconds_sum"),
    ("fl_c", "torus_exec_flush_seconds_count"),
    ("db_s", "torus_exec_root_dirty_buckets_sum"),
    ("db_c", "torus_exec_root_dirty_buckets_count"),
    ("execq", "torus_exec_queue_depth"),
    ("bscan", "torus_exec_root_bucket_scans_total"),
    ("mc_hit", "torus_member_cache_hits_total"),
    ("mc_miss", "torus_member_cache_misses_total"),
    ("mc_evict", "torus_member_cache_evictions_total"),
    ("mc_resident", "torus_member_cache_resident_buckets"),
]
EXTRA = [
    f"torus_exec_state_write_{stem}_{suffix}"
    for stem in ("build_seconds", "db_seconds", "batch_bytes")
    for suffix in ("sum", "count")
]


# s92 (B-blind observability): the executor's margin-cut counters, as
# torus-telemetry names them (`_total` added by the exporter).
S92_COUNTERS = [
    f"torus_sell_cuts_{pool}_{fill}_{bucket}"
    for pool in ("pool", "nonpool")
    for fill in ("zero", "partial")
    for bucket in ("t0", "t1_2", "t3_5", "t6_10", "t11_30", "t31p")
] + ["torus_maker_margin_cancels", "torus_reduce_only_cuts"]
S92_TOP_UPS = [f"torus_sell_top_ups_{k}" for k in ("full", "partial", "none")]
S92_COUNTERS += S92_TOP_UPS


class PhaseMappingTests(unittest.TestCase):
    def test_emitted_phase_rows_match_legacy_header_and_actual_awk(self):
        here = Path(__file__).parent
        script = (here / "run-cell.sh").read_text()
        phase = re.search(r'^PHASE_COLS="([^"]+)"', script, re.M).group(1).split()
        wide = re.search(r'^WIDE_COLS="([^"]+)"', script, re.M).group(1).split()
        header = re.search(
            r'echo "(ts,committed,[^"]+)" > "\$OUT/phase-val\$i.csv"', script
        ).group(1)
        self.assertEqual(phase, [metric for _, metric in LEGACY])
        self.assertEqual(header.split(","), ["ts"] + [label for label, _ in LEGACY])
        self.assertTrue(all(metric in wide and metric not in phase for metric in EXTRA))
        # Every field has a distinct offset and slope; shifted columns cannot
        # accidentally look like a correct timer, count, queue or cache value.
        slopes = {metric: i for i, (_, metric) in enumerate(LEGACY, 1)}
        slopes.update({metric: 100 + i for i, metric in enumerate(EXTRA)})
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            path = out / "phase-val0.csv"
            path.write_text(header + "\n")
            with CsvSink(out, wide, [], phase, [], ["val0"]) as sink:
                for elapsed in (0, 60, 120, 180):
                    body = "".join(
                        f"{metric} {slope * (1000 + elapsed)}\n"
                        for metric, slope in slopes.items()
                    )
                    sink.emit(
                        "val0",
                        Response(
                            body,
                            True,
                            1000 + elapsed,
                            elapsed,
                            1000 + elapsed,
                            elapsed,
                            0,
                            None,
                            None,
                        ),
                    )
            with path.open() as file:
                rows = list(csv.reader(file))
            self.assertTrue(all(len(row) == len(rows[0]) == 25 for row in rows))
            for elapsed, row in zip((0, 60, 120, 180), rows[1:]):
                self.assertEqual(
                    row,
                    [str(1000 + elapsed)]
                    + [str(slopes[metric] * (1000 + elapsed)) for _, metric in LEGACY],
                )
            wide_row = next(csv.reader((out / "sampler.csv").read_text().splitlines()))
            for metric in EXTRA:
                self.assertEqual(
                    wide_row[2 + wide.index(metric)], str(slopes[metric] * 1000)
                )
            output = subprocess.run(
                ["awk", "-f", str(here / "phase60.awk"), str(path)],
                capture_output=True,
                text=True,
                check=True,
                timeout=5,
            ).stdout
        for name, sum_index, count_index in (
            ("load_books", 7, 8),
            ("root", 9, 10),
            ("state_write", 11, 12),
            ("evm_resync", 13, 14),
            ("flush", 15, 16),
        ):
            values = next(
                line.split()
                for line in output.splitlines()
                if line.startswith(name + " ")
            )
            expected = f"{1000 * sum_index / count_index:.2f}"
            self.assertEqual(values[1:3], [expected, expected], name)
        dirty = next(
            line.split()
            for line in output.splitlines()
            if line.startswith("dirty_buckets/obs")
        )
        self.assertEqual(dirty[1:3], [f"{17 / 18:.1f}"] * 2)
        self.assertIn(f"peak_execq={19 * 1180}", output)

    def test_wide_cols_carry_every_end_resident_series_summarize_reads(self):
        """Item 6 steps 1 / 2: summarize.py reads end_resident, its two subs
        and the join wait (per native block, plus the wait's count to tell a
        step 2 binary, plus its buckets for p50 / p90). A series missing from
        WIDE_COLS reads as 0.0 in summary.json (ozarchy, 5524646)."""
        script = (Path(__file__).parent / "run-cell.sh").read_text()
        wide = re.search(r'^WIDE_COLS="([^"]+)"', script, re.M).group(1).split()
        buckets = re.search(r'^BUCKET_METRICS="([^"]+)"', script, re.M).group(1).split()
        for name in (
            "end_resident",
            "end_resident_rows",
            "end_resident_positions",
            "end_resident_wait",
        ):
            self.assertIn(f"torus_exec_{name}_seconds_sum", wide, name)
        for name in ("end_resident", "end_resident_wait"):
            self.assertIn(f"torus_exec_{name}_seconds_count", wide, name)
        self.assertIn("torus_exec_end_resident_wait_seconds_bucket", buckets)

    def test_margin_configs_split_of_load_books_is_sampled_and_summarized(self):
        """Item 6 E4: the context's margin-config load, a part of load_books,
        is sampled (sum, count) and summarize.py reports it under load_books."""
        here = Path(__file__).parent
        wide = (
            re.search(r'^WIDE_COLS="([^"]+)"', (here / "run-cell.sh").read_text(), re.M)
            .group(1)
            .split()
        )
        for suffix in ("sum", "count"):
            self.assertIn(f"torus_exec_margin_configs_seconds_{suffix}", wide)
        sub = re.search(
            r'"load_books":\s*\[([^\]]*)\]', (here / "summarize.py").read_text()
        )
        self.assertIsNotNone(sub, "SUB has no load_books entry")
        self.assertIn('"margin_configs"', sub.group(1))

    def test_action_status_phase_is_sampled_and_summarized(self):
        """Item 6 cut 1: the action status (native failures mapped to body
        positions, exec thread, after the engine) is sampled (sum, count) and
        summarize.py reports it as an exec-thread phase in the chain identity,
        so residual_untimed no longer holds it."""
        here = Path(__file__).parent
        wide = (
            re.search(r'^WIDE_COLS="([^"]+)"', (here / "run-cell.sh").read_text(), re.M)
            .group(1)
            .split()
        )
        for suffix in ("sum", "count"):
            self.assertIn(f"torus_exec_action_status_seconds_{suffix}", wide)
        text = (here / "summarize.py").read_text()
        phases = re.search(r"^PHASES = \[([^\]]*)\]", text, re.M)
        self.assertIsNotNone(phases)
        self.assertIn('"action_status"', phases.group(1))
        e_phases = re.search(r"e_phases = \[([^\]]*)\]", text)
        self.assertIsNotNone(e_phases)
        self.assertIn('"action_status"', e_phases.group(1))

    def test_s92_margin_cut_counters_are_sampled_and_summarized(self):
        """s92 (B-blind observability): the sell-cut counters (pool / non-pool
        x zero-fill / partial x tick bucket), maker margin cancels and
        reduce-only cuts are sampled, and summarize.py reports each one's
        delta over the bench window per node."""
        here = Path(__file__).parent
        wide = (
            re.search(r'^WIDE_COLS="([^"]+)"', (here / "run-cell.sh").read_text(), re.M)
            .group(1)
            .split()
        )
        names = S92_COUNTERS
        self.assertEqual(len(names), 26 + len(S92_TOP_UPS))
        for name in names:
            self.assertIn(f"{name}_total", wide, name)
        import test_summarize as ts

        saved = dict(ts.COUNTERS)
        ts.COUNTERS.update(
            {f"{n[len('torus_') :]}_total": 3 + i for i, n in enumerate(names)}
        )
        try:
            with tempfile.TemporaryDirectory() as out:
                ts.write_fixture(out)
                f0 = ts.run(out)["funnel_by_node"]["val0"]
        finally:
            ts.COUNTERS.clear()
            ts.COUNTERS.update(saved)
        for i, n in enumerate(names):
            self.assertEqual(f0.get(f"delta_{n[len('torus_') :]}_total"), 3 + i, n)

    def test_phase2_step0_series_are_sampled(self):
        """Item 6 Phase 2 step 0.2: the cancel-all / by-id counters, the
        per-site spawn gauges (names as torus-telemetry EXEC_SPAWN_SITES) and
        the oracle-only block histogram (sum, count, buckets) are sampled, and
        run-cell.sh snapshots /proc/<pid>/stat into procstat.raw."""
        script = (Path(__file__).parent / "run-cell.sh").read_text()
        wide = re.search(r'^WIDE_COLS="([^"]+)"', script, re.M).group(1).split()
        buckets = re.search(r'^BUCKET_METRICS="([^"]+)"', script, re.M).group(1).split()
        lib = (Path(__file__).parents[2] / "crates/torus-telemetry/src/lib.rs").read_text()
        sites = re.findall(r'"([a-z_]+)"', re.search(r"EXEC_SPAWN_SITES: \[&str; 9\] = \[([^\]]*)\]", lib).group(1))
        self.assertEqual(len(sites), 9)
        summarize = (Path(__file__).parent / "summarize.py").read_text()
        ours = re.search(r"^SPAWN_SITES = \[([^\]]*)\]", summarize, re.M)
        self.assertIsNotNone(ours, "summarize.py has no SPAWN_SITES")
        self.assertEqual(re.findall(r'"([a-z_]+)"', ours.group(1)), sites)
        for name in ["torus_exec_cancel_all_total", "torus_exec_cancel_all_books_visited_total",
                     "torus_exec_cancel_all_books_hit_total", "torus_exec_by_id_actions_total",
                     "torus_exec_by_id_books_probed_total", "torus_exec_oracle_only_block_seconds_sum",
                     "torus_exec_oracle_only_block_seconds_count"] + [f"torus_exec_thread_spawns_{s}" for s in sites]:
            self.assertIn(name, wide)
        self.assertIn("torus_exec_oracle_only_block_seconds_bucket", buckets)
        self.assertIn('procstat.raw', script)


if __name__ == "__main__":
    unittest.main()
