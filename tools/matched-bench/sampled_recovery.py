"""Conservative, descriptive quiet intervals from independent real scrapes.

Never carry another node's value forward to a new timestamp. Request intervals
bound when metrics could have been observed; completion alone is not enough.
"""
import json
import math

from health import COMMITTED, FLOW, MAX_SAMPLE_GAP_S, NODES, REQUIRED, pending

QUIET_S = 10
# Collector wall/monotonic calls are adjacent. A larger offset change makes
# generator-wall phase placement ambiguous; reject rather than repair clocks.
CLOCK_TOLERANCE_S = 0.05


def finite(value):
    return type(value) in (int, float) and math.isfinite(value)


def read_audit(path):
    records, errors = [], []
    try:
        with open(path, encoding='utf-8', errors='replace') as source:
            for number, line in enumerate(source, 1):
                try:
                    record = json.loads(line)
                    if not isinstance(record, dict):
                        raise ValueError('expected object')
                    records.append(record)
                except ValueError:
                    errors.append(f'invalid sampler audit record at line {number}')
    except OSError:
        errors.append('sampler diagnostics unavailable')
    return records, errors


def observations(rows, audit, lo, hi):
    """Join unique real rows and validate clocks/coverage before any inference."""
    indexed, offsets, problems = {}, [], []
    for a in audit:
        try:
            if a['node'] not in NODES or not finite(a['ts']):
                raise ValueError('invalid sampler audit identity')
            keys = ('started_wall', 'completed_wall', 'started_monotonic', 'completed_monotonic', 'request_seconds')
            if any(not finite(a[k]) for k in keys):
                raise ValueError('invalid sampler audit clock')
            if (a['started_wall'] > a['completed_wall'] or
                    a['started_monotonic'] > a['completed_monotonic'] or
                    a['ts'] != math.floor(a['completed_wall']) or
                    abs(a['request_seconds'] - (a['completed_monotonic'] - a['started_monotonic'])) > 1e-6):
                raise ValueError('inconsistent sampler audit interval')
            offsets.extend((a['started_wall'] - a['started_monotonic'],
                            a['completed_wall'] - a['completed_monotonic']))
            indexed.setdefault((a['node'], a['ts']), []).append(a)
        except (KeyError, TypeError, ValueError) as error:
            problems.append(str(error))
    if not offsets or max(offsets) - min(offsets) > CLOCK_TOLERANCE_S:
        problems.append('missing or ambiguous wall/monotonic clock mapping')

    events = []
    for node in NODES:
        seen, selected = set(), []
        for row in rows.get(node, []):
            if not finite(row.get('ts')):
                problems.append(f'{node}: invalid sample timestamp')
                continue
            if not math.floor(lo) <= row['ts'] <= math.floor(hi):
                continue
            key = (node, row['ts'])
            matches = indexed.get(key, [])
            if key in seen or len(matches) != 1:
                problems.append(f'{node}: missing or ambiguous sample/audit join')
                continue
            seen.add(key)
            a = matches[0]
            # A straddling request cannot support a within-phase observation.
            if a['started_wall'] < lo or a['completed_wall'] >= hi:
                continue
            if (row.get('scrape_valid') != 1 or a.get('scrape_valid') != 1 or
                    a.get('curl_returncode') != 0 or a.get('error') is not None or
                    any(not finite(row.get(k)) or row[k] < 0 for k in REQUIRED)):
                problems.append(f'{node}: invalid recovery metrics/scrape')
                continue
            selected.append(dict(a, row=row))
        # An audit event without its CSV row cannot silently disappear.
        for key, matches in indexed.items():
            if key[0] == node and any(lo <= a['started_wall'] and a['completed_wall'] < hi for a in matches) and key not in seen:
                problems.append(f'{node}: audit has no matching sample')
        if len(selected) < 2:
            problems.append(f'{node}: insufficient within-phase samples')
            continue
        if selected[0]['started_wall'] - lo > MAX_SAMPLE_GAP_S or hi - selected[-1]['completed_wall'] > MAX_SAMPLE_GAP_S:
            problems.append(f'{node}: phase boundaries not covered')
        for a, b in zip(selected, selected[1:]):
            gap = b['completed_monotonic'] - a['completed_monotonic']
            if not 0 < gap <= MAX_SAMPLE_GAP_S or b['started_monotonic'] < a['completed_monotonic']:
                problems.append(f'{node}: sample gap, overlap, or unordered time')
            if any(b['row'][k] < a['row'][k] for k in (COMMITTED,) + FLOW):
                problems.append(f'{node}: counter reset')
        events.extend(selected)
    return sorted(events, key=lambda a: a['completed_monotonic']), sorted(set(problems))


def evidence(event):
    return {k: event[k] for k in ('ts', 'started_wall', 'completed_wall', 'started_monotonic', 'completed_monotonic')} | {
        'committed': event['row'][COMMITTED]}


def quiet_phase(rows, audit, errors, lo, hi):
    result = {'status': 'unverified', 'nominal_unix_window': [lo, hi],
              'quiet_s': QUIET_S, 'confirmation': None, 'censored': False,
              'renewed_activity': None, 'renewed_activity_events': [], 'problems': []}
    events, problems = observations(rows, audit, lo, hi)
    result['problems'] = sorted(set(list(errors) + problems))
    if result['problems']:
        return result
    runs = {node: [] for node in NODES}
    found_flow, renewed_nodes = None, set()
    for event in events:
        node, row = event['node'], event['row']
        eligible = not pending(row)
        if found_flow is not None:
            if node not in renewed_nodes and (not eligible or any(row[k] != found_flow[node][k] for k in FLOW)):
                renewed_nodes.add(node)
                result['renewed_activity_events'].append(dict(node=node, **evidence(event)))
            continue
        run = runs[node]
        if not eligible:
            runs[node] = []
        elif run and all(row[k] == run[-1]['row'][k] for k in FLOW):
            run.append(event)
        else:
            runs[node] = [event]
        if not all(runs.values()):
            continue
        left = max(run[0]['completed_monotonic'] for run in runs.values())
        right = min(run[-1]['started_monotonic'] for run in runs.values())
        if right - left < QUIET_S:
            continue
        contributors = {}
        for other, run in runs.items():
            inside = [a for a in run if a['started_monotonic'] >= left and a['completed_monotonic'] <= right]
            if len(inside) < 2 or inside[-1]['row'][COMMITTED] <= inside[0]['row'][COMMITTED]:
                break
            contributors[other] = {'quiet_first': evidence(run[0]), 'quiet_last': evidence(run[-1]),
                                   'commit_first': evidence(inside[0]), 'commit_last': evidence(inside[-1]),
                                   'commit_delta': inside[-1]['row'][COMMITTED] - inside[0]['row'][COMMITTED]}
        if len(contributors) != len(NODES):
            continue
        result['confirmation'] = {'completed_wall': event['completed_wall'],
                                  'completed_monotonic': event['completed_monotonic'],
                                  'delay_from_pause_start_s': event['completed_wall'] - lo,
                                  'common_quiet_monotonic_window': [left, right],
                                  'common_quiet_span_s': right - left, 'nodes': contributors}
        found_flow = {n: {k: run[-1]['row'][k] for k in FLOW} for n, run in runs.items()}
    result['status'] = 'observed' if found_flow is not None else 'not_observed_before_phase_end'
    result['censored'] = found_flow is None
    result['renewed_activity'] = bool(renewed_nodes) if found_flow is not None else None
    return result


def recovery_report(phases, observed, rows, audit, errors):
    reports = []
    for index, (phase, boundary) in enumerate(zip(phases, observed)):
        if phase['rate_total'] != 0:
            continue
        lo = boundary['planned_unix_s']
        hi = lo + phase['end_s'] - phase['start_s']
        reports.append(dict(quiet_phase(rows, audit, errors, lo, hi), index=index))
    statuses = [p['status'] for p in reports]
    status = ('not_applicable' if not reports else 'unverified' if 'unverified' in statuses
              else 'not_observed_before_phase_end' if 'not_observed_before_phase_end' in statuses else 'observed')
    return {'status': status, 'phases': reports, 'quiet_s': QUIET_S,
            'clock_tolerance_s': CLOCK_TOLERANCE_S,
            'note': 'Sampled quiet evidence only: no synthetic timestamps or values; common request-bounded '
                    'interval plus real commit progress on every node. Confirmation must precede phase end. '
                    'Renewed activity is separate; observation does not prove permanent drainage, absence '
                    'of in-flight network requests, or achievement of the preceding requested burst.'}
