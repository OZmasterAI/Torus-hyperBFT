import json, re, os
from datetime import datetime, timezone
R="/home/oz/bench-results-matched/ozarchy-p3p-300m-"
def ts(line):
    m=re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z",line)
    if not m: return None
    s=m.group(1); s=s[:26]
    return datetime.strptime(s,"%Y-%m-%dT%H:%M:%S.%f").replace(tzinfo=timezone.utc).timestamp()
for cell in ("g1-c","g2-c","g1-a","g2-a","g1-k","g2-k","x1-c","x1-a"):
    c=R+cell
    for i,suf in enumerate(("","-2","-3")):
        kf=c+f"/crash-kill{suf}.json"; tf=c+f"/crash-restart-tail{suf}.log"
        if not os.path.exists(kf): continue
        k=json.load(open(kf)); rj=json.load(open(tf+".restart.json"))
        t_up=k["restart_ts"]
        L=[re.sub(r"\x1b\[[0-9;]*m","",l) for l in open(tf,errors="replace")]
        def first(pat, after=None):
            for l in L:
                if pat in l:
                    t=ts(l)
                    if t and t>=t_up-1 and (after is None or t>=after): return t,l
            return None,None
        tk,_=first("loaded node key"); to,_=first("state database opened")
        tg,lg=first("execution gap detected"); te,le=first("exec pipeline ENABLED"); tc,lc=first("on_committed_block: sending to execution pipeline")
        ck=[(ts(l),l) for l in L if "p3proto checkpoint height" in l and ts(l) and ts(l)>=t_up-1]
        gap=re.search(r"gap=(\d+)",lg).group(1) if lg else "0"
        ap=re.search(r"applied=(\d+)",le).group(1) if le else None
        def d(x,y): return "-" if (x is None or y is None) else "%.2f"%(y-x)
        print(f"{cell} kill{i+1} sig={k.get('kill_signal')} down={k['down_s']} preH={k['pre_kill']['block_height']} q={k['pre_kill']['exec_queue_depth']} | gap={gap} committed={rj.get('committed_height_at_crash')} applied={rj.get('applied_height_at_crash')} attached={rj.get('worker_attached_applied')} | up->dbopen {d(t_up,to)} keyload->open {d(tk,to)} open->gap {d(to,tg)} replay {d(tg,te)} (gap/s {('%.2f'%(int(gap)/(te-tg))) if tg and te and int(gap) else '-'}) up->ENABLED {d(t_up,te)} up->1stcommit {d(t_up,tc)} 1stH={re.search(r'height=(\d+)',lc).group(1) if lc else None} ckpt_in_replay={[(round(a-t_up,1), re.search(r'height=Some\((\d+)\)',l).group(1) if 'Some' in l else None) for a,l in ck if te and a<=te]}")
