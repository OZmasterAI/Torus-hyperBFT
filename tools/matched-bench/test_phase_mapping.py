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
    ('committed', 'torus_blocks_committed_total'),
    ('height', 'torus_block_height'),
    ('placed', 'torus_orders_placed_accepted_total'),
    ('matched', 'torus_orders_matched_total'),
    ('resting', 'torus_orders_resting_total'),
    ('exec_resting', 'torus_exec_resting_orders'),
    ('lb_s', 'torus_exec_load_books_seconds_sum'),
    ('lb_c', 'torus_exec_load_books_seconds_count'),
    ('root_s', 'torus_exec_root_seconds_sum'),
    ('root_c', 'torus_exec_root_seconds_count'),
    ('sw_s', 'torus_exec_state_write_seconds_sum'),
    ('sw_c', 'torus_exec_state_write_seconds_count'),
    ('evm_s', 'torus_exec_evm_resync_seconds_sum'),
    ('evm_c', 'torus_exec_evm_resync_seconds_count'),
    ('fl_s', 'torus_exec_flush_seconds_sum'),
    ('fl_c', 'torus_exec_flush_seconds_count'),
    ('db_s', 'torus_exec_root_dirty_buckets_sum'),
    ('db_c', 'torus_exec_root_dirty_buckets_count'),
    ('execq', 'torus_exec_queue_depth'),
    ('bscan', 'torus_exec_root_bucket_scans_total'),
    ('mc_hit', 'torus_member_cache_hits_total'),
    ('mc_miss', 'torus_member_cache_misses_total'),
    ('mc_evict', 'torus_member_cache_evictions_total'),
    ('mc_resident', 'torus_member_cache_resident_buckets'),
]
EXTRA = [f'torus_exec_state_write_{stem}_{suffix}'
         for stem in ('build_seconds', 'db_seconds', 'batch_bytes')
         for suffix in ('sum', 'count')]


class PhaseMappingTests(unittest.TestCase):
    def test_emitted_phase_rows_match_legacy_header_and_actual_awk(self):
        here = Path(__file__).parent
        script = (here / 'run-cell.sh').read_text()
        phase = re.search(r'^PHASE_COLS="([^"]+)"', script, re.M).group(1).split()
        wide = re.search(r'^WIDE_COLS="([^"]+)"', script, re.M).group(1).split()
        header = re.search(r'echo "(ts,committed,[^"]+)" > "\$OUT/phase-val\$i.csv"', script).group(1)
        self.assertEqual(phase, [metric for _, metric in LEGACY])
        self.assertEqual(header.split(','), ['ts'] + [label for label, _ in LEGACY])
        self.assertTrue(all(metric in wide and metric not in phase for metric in EXTRA))
        # Every field has a distinct offset and slope; shifted columns cannot
        # accidentally look like a correct timer, count, queue or cache value.
        slopes = {metric: i for i, (_, metric) in enumerate(LEGACY, 1)}
        slopes.update({metric: 100 + i for i, metric in enumerate(EXTRA)})
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            path = out / 'phase-val0.csv'
            path.write_text(header + '\n')
            with CsvSink(out, wide, [], phase, [], ['val0']) as sink:
                for elapsed in (0, 60, 120, 180):
                    body = ''.join(f'{metric} {slope * (1000 + elapsed)}\n'
                                   for metric, slope in slopes.items())
                    sink.emit('val0', Response(body, True, 1000 + elapsed, elapsed,
                                              1000 + elapsed, elapsed, 0, None, None))
            with path.open() as file:
                rows = list(csv.reader(file))
            self.assertTrue(all(len(row) == len(rows[0]) == 25 for row in rows))
            for elapsed, row in zip((0, 60, 120, 180), rows[1:]):
                self.assertEqual(row, [str(1000 + elapsed)] +
                                 [str(slopes[metric] * (1000 + elapsed)) for _, metric in LEGACY])
            wide_row = next(csv.reader((out / 'sampler.csv').read_text().splitlines()))
            for metric in EXTRA:
                self.assertEqual(wide_row[2 + wide.index(metric)], str(slopes[metric] * 1000))
            output = subprocess.run(['awk', '-f', str(here / 'phase60.awk'), str(path)],
                                    capture_output=True, text=True, check=True, timeout=5).stdout
        for name, sum_index, count_index in (
                ('load_books', 7, 8), ('root', 9, 10), ('state_write', 11, 12),
                ('evm_resync', 13, 14), ('flush', 15, 16)):
            values = next(line.split() for line in output.splitlines() if line.startswith(name + ' '))
            expected = f'{1000 * sum_index / count_index:.2f}'
            self.assertEqual(values[1:3], [expected, expected], name)
        dirty = next(line.split() for line in output.splitlines() if line.startswith('dirty_buckets/obs'))
        self.assertEqual(dirty[1:3], [f'{17 / 18:.1f}'] * 2)
        self.assertIn(f'peak_execq={19 * 1180}', output)


if __name__ == '__main__':
    unittest.main()
