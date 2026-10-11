# Restart timing per kill from val1 restart tails (crash-restart-tail.log, -2.log, -3.log) + crash-kill*.json.
import re,json,datetime,statistics as st
R='/home/oz/bench-results-matched/'
ANSI=re.compile(r'\x1b\[[0-9;]*m')
def ts(l):
    m=re.match(r'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z',l)
    return datetime.datetime.strptime(m.group(1),'%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp() if m else None
out=[]
for c in ('tw-c1','tw-c2','tw-c3'):
    for k,(tail,kj) in enumerate([('crash-restart-tail.log','crash-kill.json'),('crash-restart-tail-2.log','crash-kill-2.json'),('crash-restart-tail-3.log','crash-kill-3.json')],1):
        d=R+'ozarchy-p3s-300m-%s/'%c
        K=json.load(open(d+kj))
        ev={}; done={}; start={}; nat={}; first_commit=None; panics=[]; errors=0
        for raw in open(d+tail,errors='replace'):
            l=ANSI.sub('',raw); t=ts(l)
            if t is None: continue
            if 'starting torus-node' in l and 'start' not in ev: ev['start']=t
            elif 'loaded node key' in l and 'key' not in ev: ev['key']=t
            elif 'state database opened' in l and 'dbopen' not in ev: ev['dbopen']=t
            elif 'execution gap detected' in l:
                m=re.search(r'committed_height=(\d+) applied_height=(\d+) gap=(\d+)',l); ev['gap_t']=t; ev['committed'],ev['applied'],ev['gap']=map(int,m.groups())
            elif 'exec pipeline ENABLED' in l and 'enabled' not in ev:
                ev['enabled']=t; m=re.search(r'applied=(\d+)',l); ev['enabled_applied']=int(m.group(1)) if m else None
            elif 'executing finalized block' in l:
                h=int(re.search(r'height=(\d+)',l).group(1)); start.setdefault(h,t); nat[h]=int(re.search(r'native_count=(\d+)',l).group(1)) if 'native_count=' in l else None
                if 'pipelined=false' in l: ev.setdefault('serial_heights',[]).append(h)
            elif 'block done' in l:
                h=int(re.search(r'height=(\d+)',l).group(1)); done.setdefault(h,t)
            elif 'on_committed_block: sending' in l and first_commit is None:
                h=int(re.search(r'height=(\d+)',l).group(1)); first_commit=(h,t)
            if re.search(r'panicked|PANIC| ERROR ',l): panics.append(l.strip()[:200])
        C,A,G=ev['committed'],ev['applied'],ev['gap']
        rh=list(range(A+1,C+1))
        last_done=done.get(C)
        r=dict(cell=c,kill=k,restart_ts=K['restart_ts'],kill_ts=K['kill_ts'],gap=G,committed=C,applied=A,
               proc_start_to_key=ev['key']-ev['start'],restart_to_proc_start=ev['start']-K['restart_ts'],
               db_open_s=ev['dbopen']-ev['key'],restart_to_dbopen=ev['dbopen']-K['restart_ts'],
               dbopen_to_replay_start=ev['gap_t']-ev['dbopen'],
               replay_s_marker=ev['enabled']-ev['gap_t'],replay_s_lastdone=(last_done-ev['gap_t']) if last_done else None,
               enabled_applied=ev['enabled_applied'],all_replayed_done=all(h in done for h in rh),
               serial_replay_heights=len([h for h in ev.get('serial_heights',[]) if A<h<=C]),
               first_block_s=(done[A+1]-start[A+1]) if (A+1) in done else None,
               first_commit_h=first_commit[0] if first_commit else None,
               restart_to_first_commit=(first_commit[1]-K['restart_ts']) if first_commit else None,
               replay_end_to_first_commit=(first_commit[1]-ev['enabled']) if first_commit else None,
               native_sum=sum(nat.get(h) or 0 for h in rh),err_or_panic_lines=panics[:3])
        r['replay_blk_s']=G/r['replay_s_marker']
        if r['first_block_s'] is not None and G>1:
            r['replay_blk_s_excl_first']=(G-1)/(ev['enabled']-done[A+1])
        r['replay_ms_per_block']=1000*r['replay_s_marker']/G
        out.append(r)
json.dump(out,open(R+'ozarchy-p3s-run/analyst/restart.json','w'),indent=1)
hdr='cell kill gap C A | restart->procstart key->dbopen(DB open) restart->dbopen dbopen->replay | replay(marker) replay(lastdone) blk/s blk/s(ex1st) 1st-blk-s | end->1stcommit restart->1stcommit 1st_h | native_sum serial_n enabled_applied panics'
print(hdr)
for r in out:
    print('%s %d %d %d %d | %.3f %.3f %.3f %.3f | %.3f %.3f %.2f %.2f %.3f | %.3f %.3f %s | %d %d %s %s'%(r['cell'],r['kill'],r['gap'],r['committed'],r['applied'],r['restart_to_proc_start'],r['db_open_s'],r['restart_to_dbopen'],r['dbopen_to_replay_start'],
      r['replay_s_marker'],r['replay_s_lastdone'] or -1,r['replay_blk_s'],r.get('replay_blk_s_excl_first',-1),r['first_block_s'] or -1,r['replay_end_to_first_commit'],r['restart_to_first_commit'],r['first_commit_h'],r['native_sum'],r['serial_replay_heights'],r['enabled_applied'],r['err_or_panic_lines']))
def ms(k):
    x=[r[k] for r in out]; return '%s mean %.3f sd %.3f min %.3f max %.3f'%(k,st.mean(x),st.stdev(x),min(x),max(x))
for k in ('db_open_s','restart_to_dbopen','dbopen_to_replay_start','replay_s_marker','replay_blk_s','replay_blk_s_excl_first','replay_ms_per_block','replay_end_to_first_commit','restart_to_first_commit'):
    print(ms(k))
tot_g=sum(r['gap'] for r in out); tot_t=sum(r['replay_s_marker'] for r in out)
print('pooled replay blk/s %.3f (%d blocks / %.2f s)'%(tot_g/tot_t,tot_g,tot_t))
