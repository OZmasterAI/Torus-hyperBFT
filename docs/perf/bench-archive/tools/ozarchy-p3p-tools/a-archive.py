# Bench-archive rows for ozarchy-p3p (INDEX rows, cells.md section, DELETABLE block) -> /tmp/p3p-analyst/archive-*.txt (read-only on results)
import json,os,subprocess
R='/home/oz/bench-results-matched/'; O='/tmp/p3p-analyst/'
SEC="`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '42. Phase 3 checkpoint cost, measured with a throwaway prototype: all rows vs book blobs vs control (campaign ozarchy-p3p, 2026-10-11)'"
def size(p): return int(subprocess.check_output(['du','-sb','--apparent-size',p]).split()[0])
def h(n):
    if n>=1<<30: return '%.2f GiB'%(n/(1<<30))
    if n>=1<<20: return '%.1f MiB'%(n/(1<<20))
    return '%d KiB'%round(n/1024)
ARM={'c':'arm c (control, `TORUS_P3_PROTO` unset)','a':'arm a (`TORUS_P3_PROTO=all`)','k':'arm k (`TORUS_P3_PROTO=books`)',
     'cS':'arm cS (control + `STATE_HASH_ACTIVATION=1`)','aS':'arm aS (all + `STATE_HASH_ACTIVATION=1`)'}
ENV={'c':'p3 off','a':'p3 all','k':'p3 books','cS':'p3 off + state hash','aS':'p3 all + state hash'}
CELLS=[('w0','c','warm-up 60 s, excluded')]
for r in range(1,5):
    for a in ('c','a','k','cS','aS'): CELLS.append((f'r{r}-{a}',a,'throughput 120 s'))
for g in ('g1','g2'):
    for a in ('c','a','k'): CELLS.append((f'{g}-{a}',a,'graceful restart: SIGTERM val1 at bench + 60 s, 120 s (crash gate false FAIL on the INFO fail-stop config line; control SIGTERM = default action, i.e. a kill)'))
for a in ('c','a'): CELLS.append((f'x1-{a}',a,'multi-crash: SIGKILL val1 at bench + 60 / 180 / 300 s, 420 s (crash gate false FAIL on the INFO fail-stop config line; all-arm rewind beyond the exec queue by design)'))
idx=[]; cel=[]; tierA=[]; tierB=[]; dirs=[]
for t,a,what in CELLS:
    d='ozarchy-p3p-300m-'+t; dirs.append(d); s=json.load(open(R+d+'/summary.json'))
    md5=s['binaries']['torus_node_md5'][:8]
    commit='%s @ %s (node %s, branch proto/p3-coalesce @ 682a8df5)'%(os.path.basename(s['worktree']),s['commit'][:8],md5)
    for f in sorted(os.listdir(R+d)):
        if f in ('val0.log.gz','val1.log.gz','val2.log.gz','buckets.csv','tasks.txt','sampler.csv'): tierB.append(R+d+'/'+f)
    sb=sum(os.path.getsize(p) for p in tierB if p.startswith(R+d+'/'))
    idx.append('| `%s` | %s | 2026-10-11 | %s | Phase 3 checkpoint-cost prototype on tw (trade history off + WAL 2048 MiB), Classic, standard shape at 300 markets, RocksDB LOG saved; %s; %s | %s | R | - | %s |'%(d,h(size(R+d)),commit,ARM[a],what,SEC,h(sb)))
    hd=s['headline']; c=s['cell']
    env='6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; '+ENV[a]+('; SIGTERM val1 at +60 s' if t.startswith('g') else '')+('; SIGKILL val1 at +60 / +180 / +300 s' if t.startswith('x') else '')+('; warm-up' if t=='w0' else '')
    ag=hd.get('agreement_verdict')
    cel.append('| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |'.format(hd['matched_s_avg'],hd['matched_s_first120'],hd['matched_s_best60'],hd['placed_s_avg'])%(
        d,s['status'] if s['validity']['verdict']=='ACCEPT' else 'INVALID',s['generated_at'][:16].replace('T',' '),os.path.basename(s['worktree']),s['commit'][:8],md5,c['markets'],c['duration_s'],c['block_cap'],env,hd['blk_s_avg'],hd['txs_per_block_avg'],hd['chain_ms'],hd['engine_ms_per_1k_fills'],ag,s['liveness']['verdict'],s['validity']['verdict']))
for f in ('torus-node','bench-throughput'): tierA.append(R+'ozarchy-p3p-stage/n/release/'+f)
for d,what in (('ozarchy-p3p-build','md5s of the prototype node + bench built from proto/p3-coalesce 682a8df5 (md5s.txt)'),
               ('ozarchy-p3p-prelim','launcher pre-check script (prelim.py)'),
               ('ozarchy-p3p-run','p3p campaign log, done marker, launcher handoff.json and handoff-tables.txt'),
               ('ozarchy-p3p-stage','staged prototype node 6e40fa3c and bench 19f7ac94 (`n/release/`; throwaway, rebuildable from proto/p3-coalesce 682a8df5)'),
               ('ozarchy-p3p-tools','campaign driver, handoff tables script, harness copy `matched-bench/` with crash-kill.sh patched for KILL_SIG (TERM for graceful) and the original crash-kill.sh')):
    dirs.append(d); idx.append('| `%s` | %s | 2026-10-11 | 682a8df5 (6e40fa3c) | %s | %s | R | %s | - |'%(d,h(size(R+d)),what,SEC,h(sum(os.path.getsize(p) for p in tierA)) if d=='ozarchy-p3p-stage' else '-'))
SA=sum(os.path.getsize(p) for p in tierA); SB=sum(os.path.getsize(p) for p in tierB)
open(O+'archive-index.txt','w').write('\n'.join(sorted(idx))+'\n')
open(O+'archive-cells.txt','w').write('\n'.join(sorted(cel))+'\n')
blk=['Tier A, %s, %d files:'%(h(SA),len(tierA)),'','```','rm -- \\']+["  '%s' \\"%p for p in tierA]
blk[-1]=blk[-1][:-2]; blk+=['```','','Tier B, %s, %d files:'%(h(SB),len(tierB)),'','```','rm -- \\']+["  '%s' \\"%p for p in tierB]
blk[-1]=blk[-1][:-2]; blk+=['```']
open(O+'archive-deletable.txt','w').write('\n'.join(blk)+'\n')
tot=sum(size(R+d) for d in dirs)
print('TierA',SA,len(tierA),h(SA),'TierB',SB,len(tierB),h(SB),'A+B',SA+SB,h(SA+SB),'MiB A %.1f B %.1f AB %.1f'%(SA/2**20,SB/2**20,(SA+SB)/2**20),'total dirs',len(dirs),tot,h(tot))
