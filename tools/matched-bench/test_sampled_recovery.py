import copy
import math
from pathlib import Path
import tempfile
import unittest

from health import COMMITTED, FLOW, MEMPOOL, NODES, REQUIRED
from sampled_recovery import quiet_phase, read_audit, recovery_report


def fixture(times=range(30), offsets=(0, .2, .4), request_s=.05):
    rows, audit = {node: [] for node in NODES}, []
    for node, offset in zip(NODES, offsets):
        for t in times:
            start = 100 + t + offset
            end = start + request_s
            a = dict(node=node, ts=math.floor(end + 900), started_wall=start + 900,
                     completed_wall=end + 900, started_monotonic=start, completed_monotonic=end,
                     request_seconds=end-start, scrape_valid=1, curl_returncode=0, error=None)
            audit.append(a)
            rows[node].append(dict(ts=a['ts'], scrape_valid=1, **{k: 0 for k in REQUIRED}))
            rows[node][-1][COMMITTED] = t
    return rows, audit


class SampledRecoveryTests(unittest.TestCase):
    def test_staggered_real_intersection_and_causal_confirmation(self):
        rows, audit = fixture()
        result = quiet_phase(rows, audit, [], 1000, 1030)
        self.assertEqual(result['status'], 'observed')
        c = result['confirmation']
        self.assertGreaterEqual(c['common_quiet_span_s'], 10)
        self.assertLess(c['completed_wall'], 1030)
        self.assertFalse(result['renewed_activity'])
        left, right = c['common_quiet_monotonic_window']
        for node in NODES:
            proof = c['nodes'][node]
            self.assertGreaterEqual(proof['commit_first']['started_monotonic'], left)
            self.assertLessEqual(proof['commit_last']['completed_monotonic'], right)
            self.assertLessEqual(proof['quiet_last']['completed_monotonic'], c['completed_monotonic'])
            self.assertGreater(proof['commit_delta'], 0)
        # Later input cannot move an already-established first confirmation.
        later_rows, later_audit = fixture(range(40))
        later = quiet_phase(later_rows, later_audit, [], 1000, 1040)
        self.assertEqual(later['confirmation'], c)

    def test_individual_quiet_runs_do_not_imply_common_quiet(self):
        rows, audit = fixture()
        for i, node in enumerate(NODES):
            for row in rows[node]:
                t = row['ts'] - 1000
                row[MEMPOOL] = 0 if 5*i <= t <= 15+5*i else 1
        result = quiet_phase(rows, audit, [], 1000, 1030)
        self.assertEqual(result['status'], 'not_observed_before_phase_end')
        self.assertTrue(result['censored'])

    def test_stale_node_is_not_extended_by_other_nodes(self):
        rows, audit = fixture()
        rows['val2'] = rows['val2'][:9]
        audit = [a for a in audit if a['node'] != 'val2' or a['ts'] < 1009]
        self.assertEqual(quiet_phase(rows, audit, [], 1000, 1030)['status'], 'unverified')

    def test_execution_and_flush_backlog_prevent_quiet(self):
        for metric, value in [('torus_exec_queue_depth', 3), ('torus_flush_worker_depth', 1)]:
            rows, audit = fixture()
            for row in rows['val1']:
                row[metric] = value
            self.assertEqual(quiet_phase(rows, audit, [], 1000, 1030)['status'], 'not_observed_before_phase_end')

    def test_audit_loader_refuses_missing_malformed_and_nonobject_records(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'sampler-diagnostics.jsonl'
            self.assertTrue(read_audit(path)[1])
            path.write_bytes(b'not-json\n[]\n\xff\n')
            records, errors = read_audit(path)
            self.assertEqual(records, [])
            self.assertEqual(len(errors), 3)

    def test_commit_progress_must_be_inside_intersection(self):
        for late_only in (False, True):
            rows, audit = fixture()
            for row in rows['val0']:
                t = row['ts'] - 1000
                row[COMMITTED] = int(t == 29) if late_only else min(t, 8)
            if not late_only:
                for row in rows['val2']:
                    row[MEMPOOL] = int(row['ts'] < 1010)
            with self.subTest(late_only=late_only):
                self.assertEqual(quiet_phase(rows, audit, [], 1000, 1030)['status'], 'not_observed_before_phase_end')

    def test_http_request_duration_cannot_manufacture_quiet_time(self):
        rows, audit = fixture((0, 4, 8, 12), offsets=(0, 0, 0), request_s=3.9)
        # Completion endpoints span 12s; guaranteed observation span is 8.1s.
        result = quiet_phase(rows, audit, [], 1000, 1016)
        self.assertEqual(result['status'], 'not_observed_before_phase_end')

    def test_confirmation_at_end_or_later_drain_does_not_qualify(self):
        rows, audit = fixture(range(20), offsets=(0, 0, 0), request_s=0)
        self.assertEqual(quiet_phase(rows, audit, [], 1000, 1010)['status'], 'not_observed_before_phase_end')
        self.assertEqual(quiet_phase(rows, audit, [], 1000, 1011)['status'], 'observed')

    def test_bad_evidence_is_unverified_never_false_recovery(self):
        for mutation in ('missing_audit', 'duplicate_audit', 'duplicate_row', 'missing_row', 'clock_step',
                         'gap', 'reset', 'missing_resting', 'bad_scrape', 'overlap', 'audit_error'):
            rows, audit = fixture()
            errors = []
            if mutation == 'missing_audit': audit.pop(5)
            elif mutation == 'duplicate_audit': audit.append(copy.deepcopy(audit[5]))
            elif mutation == 'duplicate_row': rows['val0'].insert(5, copy.deepcopy(rows['val0'][5]))
            elif mutation == 'missing_row': rows['val0'].pop(5)
            elif mutation == 'clock_step':
                audit[5]['started_wall'] += .2; audit[5]['completed_wall'] += .2
            elif mutation == 'gap':
                rows['val0'] = rows['val0'][:5] + rows['val0'][11:]
                audit = [a for a in audit if a['node'] != 'val0' or not 1005 <= a['ts'] < 1011]
            elif mutation == 'reset': rows['val0'][5][COMMITTED] = 0
            elif mutation == 'missing_resting': del rows['val0'][5]['torus_orders_resting_total']
            elif mutation == 'bad_scrape': rows['val0'][5]['scrape_valid'] = 0
            elif mutation == 'overlap':
                audit[5]['started_wall'] -= 2; audit[5]['started_monotonic'] -= 2
                audit[5]['request_seconds'] += 2
            else: errors.append('malformed audit JSON')
            with self.subTest(mutation=mutation):
                result = quiet_phase(rows, audit, errors, 1000, 1030)
                self.assertEqual(result['status'], 'unverified')
                self.assertIsNone(result['confirmation'])

    def test_activity_restarts_quiet_and_renewed_activity_remains_explicit(self):
        rows, audit = fixture()
        for node in NODES:
            for row in rows[node]:
                if row['ts'] >= 1005:
                    for k in FLOW: row[k] = 1
        result = quiet_phase(rows, audit, [], 1000, 1030)
        self.assertEqual(result['status'], 'observed')
        self.assertGreater(result['confirmation']['completed_wall'], 1015)
        rows, audit = fixture()
        rows['val1'][20][MEMPOOL] = 1
        for row in rows['val2'][21:]:
            for k in FLOW: row[k] = 1
        result = quiet_phase(rows, audit, [], 1000, 1030)
        self.assertEqual(result['status'], 'observed')
        self.assertTrue(result['renewed_activity'])
        self.assertEqual({e['node'] for e in result['renewed_activity_events']}, {'val1', 'val2'})

    def test_nonzero_phases_do_not_require_recovery_audit(self):
        phase = dict(start_s=0, end_s=30, rate_total=1)
        result = recovery_report([phase], [dict(planned_unix_s=1000)], {}, [], ['missing'])
        self.assertEqual(result['status'], 'not_applicable')


if __name__ == '__main__':
    unittest.main()
