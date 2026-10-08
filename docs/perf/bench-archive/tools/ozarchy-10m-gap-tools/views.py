import json,sys
R='/home/oz/bench-results-matched/'
cells=sys.argv[1:]
K=['view_duration','view_propose_delay','block_build','block_build_select','block_build_mirror','block_build_attest','view_qc_collect','view_proposal_arrival','validate_block_da_reconstruct','view_insert_persist','view_vote_delay','view_vote_gather','view_qc_to_advance','on_committed_block','mempool_remove_committed','commit_persist']
for v in ['val0','val1','val2']:
  print('##',v)
  print('metric(mean ms; count)', *[c.replace('ozarchy-','') for c in cells], sep=' | ')
  data={c:json.load(open(R+c+'/summary.json')) for c in cells}
  for k in K:
    row=[]
    for c in cells:
      L=data[c]['consensus_by_node'][v]['load']
      row.append(f"{L.get(k+'_ms')} ({L.get(k+'_count'):.0f})")
    print(k,*row,sep=' | ')
  for k in ['views','committed_blocks','views_per_committed_block','span_s','view_timeout_no_proposal_total']:
    print(k,*[data[c]['consensus_by_node'][v]['load'].get(k) for c in cells],sep=' | ')
  for th in ['hotstuff-algo','torus-execution']:
    for f in ['on_cpu_ms_per_committed_block','runqueue_wait_ms_per_committed_block']:
      print(th,f,*[ (data[c]['sched_by_node'][v]['threads'].get(th) or {}).get(f) for c in cells],sep=' | ')
