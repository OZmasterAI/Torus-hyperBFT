#!/usr/bin/env python3
"""Load-window ([t_bench0,t_bench1]) and bench+drain fills per native block and chain ms from sampler.csv, val0."""
import csv, json, sys
R = '/home/oz/bench-results-matched/'
def at(rows, t):
    return min(rows, key=lambda r: abs(int(r['ts']) - t))
print('| cell | load: native blocks | load: fills/native blk | load: chain ms | bench+drain: native blocks | bench+drain: fills/native blk | bench+drain: chain ms |')
print('|---|---|---|---|---|---|---|')
for lab in sys.argv[1:]:
    s = json.load(open(R + lab + '/summary.json'))['timing']
    rows = [r for r in csv.DictReader(open(R + lab + '/sampler.csv')) if r['node'] == 'val0']
    out = []
    for t1 in (s['t_bench1'], s['t_drain']):
        a, b = at(rows, s['t_bench0']), at(rows, t1)
        d = lambda k: float(b[k]) - float(a[k])
        nb = d('torus_exec_native_blocks_total')
        out += [f"{nb:.0f}", f"{d('torus_orders_matched_total')/nb/1e3:.1f}k", f"{d('torus_exec_chain_seconds_sum')/d('torus_exec_chain_seconds_count')*1e3:.0f}"]
    print(f"| {lab.split('300m-')[1]} | " + ' | '.join(out) + ' |')
