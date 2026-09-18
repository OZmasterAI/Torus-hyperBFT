import copy
import unittest

from scheduled_report import COUNT_FIELDS, COUNTERS, GAUGES, node_window, scheduled_report
from workload import parse_workload, schedule_provenance


def sample(ts, value=0):
    return dict(ts=ts, scrape_valid=1, torus_orders_resting_total=0,
                **{k: value for k in COUNTERS}, **{k: 0 for k in GAUGES})


class ScheduledReportTests(unittest.TestCase):
    def fixture(self):
        workload = parse_workload('5', '.5', '.05', '0:10,10:0', 20)
        phases = workload['rate_schedule']
        observed = [dict(p, index=i, observed_elapsed_s=p['start_s'] + .01,
                         planned_unix_s=1000 + p['start_s']) for i, p in enumerate(phases)]
        provenance = schedule_provenance(workload, observed, [], '--rate-schedule 0:10,10:0', 20)
        counts = [{k: 0 for k in COUNT_FIELDS} for _ in phases]
        counts[0].update(queued_requests=2, http_started_requests=2, http_started_actions=8,
                         completed_requests=2, completed_actions=8, acked_actions=6,
                         response_error_requests=1, response_error_actions=2)
        accounting = [dict(schema=1, phases=counts, complete=True, observed_elapsed_s=22)]
        rows = {node: [sample(t, (t-1000)*factor) for t in range(1000, 1021)]
                for node, factor in [('val0', 1), ('val1', 2), ('val2', 3)]}
        audit = [dict(node=node, ts=r['ts'], started_wall=r['ts'], completed_wall=r['ts']+.1,
                      started_monotonic=r['ts']-900, completed_monotonic=r['ts']-900+.1,
                      request_seconds=.1, scrape_valid=1, curl_returncode=0, error=None)
                 for node, samples in rows.items() for r in samples]
        return workload, observed, provenance, accounting, [], rows, audit, []

    def test_cohorts_and_replicas_remain_separate(self):
        result = scheduled_report(*self.fixture())
        self.assertTrue(result['valid'])
        first, pause = result['phases']
        self.assertEqual(first['generator']['http_started_actions_s'], .8)
        self.assertEqual(first['generator']['acked_actions_per_phase_second'], .6)
        self.assertEqual(first['nodes']['val0']['rates']['matched_fill_records']['per_second'], 1)
        self.assertEqual(first['nodes']['val2']['rates']['matched_fill_records']['per_second'], 3)
        self.assertEqual(pause['generator']['http_started_actions_s'], 0)
        self.assertEqual(result['recovery']['status'], 'not_observed_before_phase_end')

    def test_missing_duplicate_malformed_and_unsettled_accounting(self):
        for mutation in ('missing', 'duplicate', 'negative', 'unsettled', 'abandoned', 'inconsistent', 'zero_starts', 'impossible_actions'):
            args = list(self.fixture())
            record = args[3][0]
            if mutation == 'missing': args[3] = []
            elif mutation == 'duplicate': args[3].append(copy.deepcopy(record))
            elif mutation == 'negative': record['phases'][0]['acked_actions'] = -1
            elif mutation == 'inconsistent': record['phases'][0]['queued_requests'] += 1
            elif mutation == 'zero_starts': record['phases'][1] = copy.deepcopy(record['phases'][0])
            elif mutation == 'impossible_actions': record['phases'][1]['completed_actions'] = 1
            else:
                record['complete'] = False
                record['phases'][0]['queued_requests'] += 1
                record['phases'][0]['outstanding_requests' if mutation == 'unsettled' else 'abandoned_requests'] += 1
            with self.subTest(mutation=mutation):
                result = scheduled_report(*args)
                self.assertFalse(result['valid'])
                self.assertEqual(result['recovery']['status'], 'unverified')

    def test_no_interpolation_and_actual_sample_span(self):
        result = node_window([sample(t, t) for t in range(1001, 1010)], 1000, 1010)
        self.assertTrue(result['valid'])
        self.assertEqual(result['sample_span_s'], 8)
        self.assertEqual(result['edge_gaps_s'], [1, 1])
        self.assertEqual(result['rates']['processed_actions']['delta'], 8)

    def test_gaps_resets_bad_scrapes_and_missing_metrics_are_not_zero_rates(self):
        good = [sample(t, t) for t in range(1000, 1011)]
        cases = [good[:1], good[:2] + good[8:], list(reversed(good))]
        reset = copy.deepcopy(good); reset[5]['torus_orders_matched_total'] = 0; cases.append(reset)
        invalid = copy.deepcopy(good); invalid[3]['scrape_valid'] = 0; cases.append(invalid)
        missing = copy.deepcopy(good); del missing[4]['torus_flush_worker_depth']; cases.append(missing)
        for rows in cases:
            result = node_window(rows, 1000, 1010)
            self.assertFalse(result['valid'])
            self.assertIsNone(result['rates'])

    def test_invalid_provenance_and_unscheduled_are_distinct(self):
        args = list(self.fixture()); args[2] = dict(required=True, valid=False)
        self.assertFalse(scheduled_report(*args)['valid'])
        self.assertIsNone(scheduled_report(None, [], dict(required=False), [], [], {}))


if __name__ == '__main__':
    unittest.main()
