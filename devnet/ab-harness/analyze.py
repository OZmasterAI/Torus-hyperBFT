#!/usr/bin/env python3
# Aggregate prometheus dumps across the 4 validators: before vs after.
# Usage: analyze.py <rundir> <before_label> <after_label> <window_seconds>
import sys, os, glob, json

def parse(path):
    m = {}
    try:
        for line in open(path):
            if line.startswith('#') or not line.strip():
                continue
            parts = line.split()
            if len(parts) != 2:
                continue
            name = parts[0].split('{')[0]
            try:
                v = float(parts[1])
            except ValueError:
                continue
            m[name] = m.get(name, 0.0) + v   # sum across label sets
    except FileNotFoundError:
        return None
    return m

def get(m, k):
    return m.get(k, 0.0) if m else 0.0

rundir, before, after, window = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])

# histograms: mean = (sum_after-sum_before)/(count_after-count_before) aggregated across nodes
HISTS = ['torus_commit_interval_seconds','torus_block_build_seconds',
         'torus_view_duration_seconds','torus_view_insert_persist_seconds',
         'torus_view_propose_finalize_seconds','torus_state_root_compute_seconds',
         'torus_exec_block_seconds']
COUNTERS = ['torus_consensus_timeout_total','torus_blocks_committed',
            'torus_native_actions_processed']

agg_sum = {h:0.0 for h in HISTS}
agg_cnt = {h:0.0 for h in HISTS}
ctr = {c:0.0 for c in COUNTERS}
per_node_blocks = []
views_now = []
nodes = 0
for i in range(4):
    b = parse(os.path.join(rundir, f'met-{i}-{before}.txt'))
    a = parse(os.path.join(rundir, f'met-{i}-{after}.txt'))
    if a is None:
        continue
    nodes += 1
    for h in HISTS:
        agg_sum[h] += get(a, h+'_sum') - get(b, h+'_sum')
        agg_cnt[h] += get(a, h+'_count') - get(b, h+'_count')
    for c in COUNTERS:
        ctr[c] += get(a, c) - (get(b, c) if b else 0.0)
    per_node_blocks.append(get(a,'torus_blocks_committed') - (get(b,'torus_blocks_committed') if b else 0.0))
    views_now.append(get(a,'torus_consensus_view'))

# blk/s: use median per-node committed delta / window (all validators commit same chain)
per_node_blocks.sort()
med_blocks = per_node_blocks[len(per_node_blocks)//2] if per_node_blocks else 0.0
blk_s = med_blocks / window if window>0 else 0.0

def mean(h):
    return (agg_sum[h]/agg_cnt[h]*1000.0) if agg_cnt[h]>0 else None  # ms

out = {
  'nodes': nodes,
  'window_s': window,
  'blocks_committed_median': med_blocks,
  'blk_per_s': round(blk_s,3),
  'commit_interval_ms': round(mean('torus_commit_interval_seconds'),3) if mean('torus_commit_interval_seconds') is not None else None,
  'block_build_ms': round(mean('torus_block_build_seconds'),3) if mean('torus_block_build_seconds') is not None else None,
  'view_duration_ms': round(mean('torus_view_duration_seconds'),3) if mean('torus_view_duration_seconds') is not None else None,
  'view_insert_persist_ms': round(mean('torus_view_insert_persist_seconds'),4) if mean('torus_view_insert_persist_seconds') is not None else None,
  'exec_block_ms': round(mean('torus_exec_block_seconds'),3) if mean('torus_exec_block_seconds') is not None else None,
  'timeouts_delta': int(ctr['torus_consensus_timeout_total']),
  'native_actions_processed_delta': int(ctr['torus_native_actions_processed']),
  'consensus_view_now': views_now,
}
print(json.dumps(out, indent=2))
