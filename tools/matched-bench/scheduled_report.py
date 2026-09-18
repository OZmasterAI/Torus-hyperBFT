"""Descriptive scheduled-load evidence. Never changes the cell acceptance gate."""
import math

from health import MAX_SAMPLE_GAP_S, NODES
from sampled_recovery import recovery_report

COUNTERS = {
    'torus_native_actions_processed_total': 'processed_actions',
    'torus_orders_placed_accepted_total': 'accepted_placements',
    'torus_orders_matched_total': 'matched_fill_records',
    'torus_blocks_committed_total': 'commits',
}
GAUGES = {
    'torus_mempool_native_size': 'native_mempool',
    'torus_exec_queue_depth': 'execution_queue',
    'torus_flush_worker_depth': 'flush_worker',
}
COUNT_FIELDS = ('queued_requests', 'http_started_requests', 'http_started_actions',
                'completed_requests', 'completed_actions', 'acked_actions',
                'response_error_requests', 'response_error_actions',
                'skipped_expired_requests', 'skipped_expired_actions',
                'abandoned_requests', 'outstanding_requests')


def finite(value):
    return type(value) in (int, float) and math.isfinite(value)


def node_window(samples, lo, hi):
    selected = [r for r in samples if finite(r.get('ts')) and lo <= r['ts'] <= hi]
    problems = []
    result = {'valid': False, 'samples': len(selected), 'nominal_window': [lo, hi],
              'sample_window': None, 'sample_span_s': None, 'edge_gaps_s': None,
              'max_sample_gap_s': None, 'rates': None, 'backlog': None}
    if len(selected) < 2:
        result['problems'] = ['fewer than two samples']
        return result
    first, last = selected[0], selected[-1]
    span = last['ts'] - first['ts']
    gaps = [b['ts'] - a['ts'] for a, b in zip(selected, selected[1:])]
    edges = [first['ts'] - lo, hi - last['ts']]
    result.update(sample_window=[first['ts'], last['ts']], sample_span_s=span,
                  edge_gaps_s=edges, max_sample_gap_s=max(gaps))
    if span <= 0 or min(gaps) <= 0 or max(gaps + edges) > MAX_SAMPLE_GAP_S:
        problems.append('unordered samples or uncovered/gapped window')
    required = list(COUNTERS) + list(GAUGES)
    if any(r.get('scrape_valid') != 1 or
           any(not finite(r.get(k)) or r[k] < 0 for k in required) for r in selected):
        problems.append('missing or invalid metrics')
    elif any(b[k] < a[k] for a, b in zip(selected, selected[1:]) for k in COUNTERS):
        problems.append('counter reset')
    if not problems:
        result['rates'] = {name: {'delta': last[k] - first[k],
                                 'per_second': (last[k] - first[k]) / span}
                           for k, name in COUNTERS.items()}
        result['backlog'] = {name: {'first': first[k], 'peak': max(r[k] for r in selected),
                                  'last': last[k]} for k, name in GAUGES.items()}
    result.update(valid=not problems, problems=problems)
    return result


def accounting_counts(records, errors, phases):
    problems = list(errors)
    counts = None
    if len(records) != 1:
        problems.append('expected exactly one final accounting record')
    else:
        record = records[0]
        try:
            counts = record['phases']
            if type(record['schema']) is not int or record['schema'] != 1 or not isinstance(counts, list) or len(counts) != len(phases):
                raise ValueError('accounting schema or phase count mismatch')
            if type(record['complete']) is not bool or not finite(record['observed_elapsed_s']) or record['observed_elapsed_s'] < phases[-1]['end_s']:
                raise ValueError('invalid final accounting boundary')
            for phase, c in zip(phases, counts):
                if any(type(c[k]) is not int or c[k] < 0 for k in COUNT_FIELDS):
                    raise ValueError('invalid accounting counter')
                if c['queued_requests'] != sum(c[k] for k in ('completed_requests', 'skipped_expired_requests', 'abandoned_requests', 'outstanding_requests')):
                    raise ValueError('request accounting does not reconcile')
                for prefix in ('http_started', 'completed', 'response_error', 'skipped_expired'):
                    requests, actions = c[prefix + '_requests'], c[prefix + '_actions']
                    if (requests == 0) != (actions == 0) or actions < requests:
                        raise ValueError('request/action counters disagree')
                if not (c['completed_requests'] <= c['http_started_requests'] <= c['queued_requests'] and
                        c['http_started_requests'] + c['skipped_expired_requests'] <= c['queued_requests'] and
                        c['acked_actions'] <= c['completed_actions'] <= c['http_started_actions'] and
                        c['response_error_requests'] <= c['completed_requests'] and
                        c['response_error_actions'] <= c['completed_actions'] - c['acked_actions']):
                    raise ValueError('inconsistent accounting totals')
                if phase['rate_total'] == 0 and c['http_started_actions'] != 0:
                    raise ValueError('HTTP starts in a zero-rate phase')
            complete = all(c['outstanding_requests'] == 0 and c['abandoned_requests'] == 0 for c in counts)
            if record['complete'] != complete:
                raise ValueError('claimed accounting completeness differs from counters')
            if not complete:
                problems.append('outstanding or abandoned submission requests')
        except (KeyError, TypeError, ValueError, IndexError) as error:
            problems.append(str(error))
            counts = None
    return counts, problems


def scheduled_report(workload, observed, provenance, accounting, errors, rows, audit=(), audit_errors=()):
    if not provenance.get('required'):
        return None
    report = {'valid': False, 'problems': [], 'phases': [],
              'note': 'Node rates use sampled counter endpoints without interpolation; replicas are separate. '
                      'HTTP attempts are not server admission, ACK cohorts may finish in later phases, '
                      'and phase execution may process earlier submissions. Matched units are fill records.',
              'recovery': {'status': 'unverified', 'phases': [], 'reason': 'schedule provenance unverified'}}
    if not provenance.get('valid'):
        report['problems'] = ['schedule provenance unverified']
        return report
    phases = workload['rate_schedule']
    counts, problems = accounting_counts(accounting, errors, phases)
    report['problems'].extend(problems)
    recovery_errors = list(audit_errors)
    if problems:
        recovery_errors.append('scheduled submission accounting unverified')
    report['recovery'] = recovery_report(phases, observed, rows, audit, recovery_errors)
    if report['recovery']['status'] == 'unverified':
        report['problems'].append('within-phase recovery evidence unverified')
    for i, (phase, boundary) in enumerate(zip(phases, observed)):
        duration = phase['end_s'] - phase['start_s']
        lo, hi = boundary['planned_unix_s'], boundary['planned_unix_s'] + duration
        generator = None
        if counts is not None:
            generator = dict(counts[i], http_started_actions_s=counts[i]['http_started_actions'] / duration,
                             acked_actions_per_phase_second=counts[i]['acked_actions'] / duration,
                             accounting_complete=counts[i]['outstanding_requests'] == 0 and counts[i]['abandoned_requests'] == 0)
        nodes = {node: node_window(rows.get(node, []), lo, hi) for node in NODES}
        report['phases'].append(dict(phase, index=i, nominal_unix_window=[lo, hi],
                                     generator=generator, nodes=nodes))
    if any(not n['valid'] for p in report['phases'] for n in p['nodes'].values()):
        report['problems'].append('one or more node phase windows lack complete evidence')
    report['valid'] = not report['problems']
    return report
