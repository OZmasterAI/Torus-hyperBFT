# per native block exec phases over the LOAD window [t_bench0,t_bench1] (and ramp/full split), val0..2 mean
import csv,json,sys
R='/home/oz/bench-results-matched/'
P=["block","verify","replay_guard","load_books","engine","save_books","body_persist","flush","handoff_wait","chain"]
E=["phase1_actions","phase_margin","phase_match","phase_settle","post_engine_tail","engine_untimed","cache_flush"]
def g(r,k):
    try: return float(r[k])
    except: return 0.0
def near(rows,t): return min(rows,key=lambda r:abs(float(r['ts'])-t))
def seg(rows,a,b):
    ra,rb=near(rows,a),near(rows,b); d=lambda k:g(rb,k)-g(ra,k)
    n=d('torus_exec_engine_seconds_count'); o={}
    o['n']=n; o['dt']=float(rb['ts'])-float(ra['ts'])
    o['fills/blk']=d('torus_orders_matched_total')/n/1000
    for k in P+E: o[k]=1000*d(f'torus_exec_{k}_seconds_sum')/n
    o['resid']=o['block']-sum(o[k] for k in ["verify","replay_guard","load_books","engine","save_books","body_persist"])
    o['eng_resid']=o['engine']-sum(o[k] for k in E if k!='cache_flush')
    return o
which=sys.argv[1]
for c in sys.argv[2:]:
    t=json.load(open(R+c+'/summary.json'))['timing']
    acc={}
    for node in ['val0','val1','val2']:
        rows=[r for r in csv.DictReader(open(R+c+'/sampler.csv')) if r['node']==node]
        fill=next((float(r['ts']) for r in rows if float(r['ts'])>=t['t_bench0'] and g(r,'torus_exec_queue_depth')>=64),t['t_bench1'])
        a,b={'load':(t['t_bench0'],t['t_bench1']),'ramp':(t['t_bench0']+20,fill-5),'full':(fill+5,t['t_bench1'])}[which]
        o=seg(rows,a,b)
        for k,v in o.items(): acc.setdefault(k,[]).append(v)
    print(c.replace('ozarchy-',''), ' '.join(f"{k}={sum(v)/3:.1f}" for k,v in acc.items()))
