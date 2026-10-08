#!/usr/bin/env python3
"""cells.py <label>...: one row per cell from summary.json, metrics-after-val0, cpu-ticks.txt."""
import json, re, sys, collections
R = '/home/oz/bench-results-matched/'
def cpu_per_1m(lab, s):
    t0, t1 = s['timing']['t_bench0'], s['timing']['t_drain']
    d = collections.defaultdict(dict)
    try:
        for l in open(R + lab + '/cpu-ticks.txt'):
            ts, p, u, st = map(int, l.split()); d[p][ts] = u + st
    except FileNotFoundError: return None
    tot = 0
    for p, m in d.items():
        ks = sorted(m); a = min(ks, key=lambda k: abs(k - t0)); b = min(ks, key=lambda k: abs(k - t1))
        tot += (m[b] - m[a]) / 100
    matched = s['funnel_by_node']['val0']['delta_orders_matched_total']
    return tot / len(d) / (matched / 1e6), tot
hdr = 'cell matched/s best60 total_matched placed/s submit_act/s blk/s commit_ms(avg/p50) chain_ms engine/1k CPU-s/1M agree live dissem oracle(stale,fresh) bp_rej pool_full'
print(hdr)
for lab in sys.argv[1:]:
    s = json.load(open(R + lab + '/summary.json')); h = s['headline']; f = s['funnel_by_node']['val0']
    met = open(R + lab + '/metrics-after-val0.txt').read()
    rej = dict(re.findall(r'torus_rpc_submit_admit_rejects_total\{reason="([a-z_]+)"\} ([0-9.e+]+)', met))
    c = cpu_per_1m(lab, s)
    of = s.get('oracle_feed', {})
    orc = f"{of.get('stale_marks_at_bench_end')},{of.get('marks_at_bench_end', {}).get('fresh')}" if of.get('oracle_feed') == 1 else 'off'
    print(lab, round(h['matched_s_avg']), round(h['matched_s_best60']), int(f['delta_orders_matched_total']), round(h['placed_s_avg']),
          round(s['ingest']['bench_submitted_actions'] / s['timing']['bench_wall_s'], 1), h['blk_s_avg'],
          f"{h['commit_interval_ms_avg']}/{h['commit_interval_ms_p50']}", h['chain_ms'], h['engine_ms_per_1k_fills'],
          round(c[0], 1) if c else None, h['agreement_verdict'], h['liveness_verdict'], h['dissemination_clean'], orc,
          int(float(rej.get('backlog_preverify', 0))), int(float(rej.get('pool_full', 0))), 'other_rej=' + str({k: v for k, v in rej.items() if k not in ('backlog_preverify', 'pool_full')}),
          'accepted=' + str(h['benchmark_accepted']), 'status=' + s['status'])
