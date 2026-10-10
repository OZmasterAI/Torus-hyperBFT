import json,re,sys,collections,datetime
def ts(s): return datetime.datetime.strptime(s,"%Y/%m/%d-%H:%M:%S.%f").timestamp()
path=sys.argv[1]; t0=ts(sys.argv[2]) if len(sys.argv)>2 else 0; t1=ts(sys.argv[3]) if len(sys.argv)>3 else 1e12
cjob={}; fjob={}; fstart={}
C=collections.defaultdict(lambda: collections.Counter())
for line in open(path,errors='replace'):
    m=re.match(r'(\S+) \d+ ',line)
    if not m: continue
    try: t=ts(m.group(1))
    except: continue
    if 'EVENT_LOG_v1' in line:
        j=json.loads(line[line.index('{'):])
        ev=j.get('event'); job=j.get('job')
        if ev=='compaction_started': cjob[job]=(j['cf_name'],j['input_data_size'],j['compaction_reason'])
        elif ev=='compaction_finished':
            cf,inp,why=cjob.get(job,('?',0,'?'))
            if not (t0<=t<=t1): continue
            c=C[cf]; c['c_n']+=1; c['c_in']+=inp; c['c_out']+=j['total_output_size']; c['c_cpu']+=j['compaction_time_cpu_micros']; c['c_wall']+=j['compaction_time_micros']
            C['_reason:'+why]['c_cpu']+=j['compaction_time_cpu_micros']; C['_reason:'+why]['c_out']+=j['total_output_size']
            C['_outlvl:%d'%j['output_level']]['c_cpu']+=j['compaction_time_cpu_micros']
        elif ev=='flush_started': fstart[job]=(t,j['total_data_size'],j['flush_reason'])
        elif ev=='table_file_creation' and job in fstart and job not in cjob:
            st,raw,why=fstart.pop(job)
            if not (t0<=t<=t1): continue
            c=C[j['cf_name']]; c['f_n']+=1; c['f_out']+=j['file_size']; c['f_raw']+=raw; c['f_wall']+=int((t-st)*1e6)
            C['_freason:'+why]['f_out']+=j['file_size']
json.dump({k:dict(v) for k,v in C.items()},sys.stdout)
