# Bench-archive rows for ozarchy-p3s (INDEX.md rows, cells.md section, DELETABLE.md block) -> analyst/archive-*.txt
import json,os,subprocess
R='/home/oz/bench-results-matched/'; A=R+'ozarchy-p3s-run/analyst/'
SEC="`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '41. Phase 3 step 0 sizing on tw: coalescing factor, layer size, restart budget, CPU ceiling (campaign ozarchy-p3s, 2026-10-11)'"
def size(p):
    return int(subprocess.check_output(['du','-sb','--apparent-size',p]).split()[0])
def h(n):
    if n>=1<<30: return '%.2f GiB'%(n/(1<<30))
    if n>=1<<20: return '%.1f MiB'%(n/(1<<20))
    return '%d KiB'%round(n/1024)
CELLS=[('tw-c1','multi-crash: SIGKILL val1 at bench + 60 / 180 / 300 s, 420 s (crash gate false FAIL on the INFO fail-stop config line)'),
       ('tw-c2','multi-crash: SIGKILL val1 at bench + 60 / 180 / 300 s, 420 s (crash gate false FAIL on the INFO fail-stop config line)'),
       ('tw-c3','multi-crash: SIGKILL val1 at bench + 60 / 180 / 300 s, 420 s (crash gate false FAIL on the INFO fail-stop config line)'),
       ('tw-old1','same-day control on the acc3 node 2ede76eb (bench a33d82f5), no perf'),
       ('tw-p1','perf (cycles:u 499 Hz, val0, 45 s from bench + 35 s)'),('tw-p2','perf (cycles:u 499 Hz, val0, 45 s from bench + 35 s)'),
       ('tw-r1','no perf; val0 WAL kept in wal-val0-keep/ (118 files, 13.62 GiB, decoded for section 41.1)'),('tw-r2','no perf')]
idx=[]; cel=[]; tierA=[]; tierB=[]
for t,what in CELLS:
    d='ozarchy-p3s-300m-'+t; s=json.load(open(R+d+'/summary.json'))
    md5=s['binaries']['torus_node_md5'][:8]
    br='branch main @ fd9e5dfa' if md5=='fb460a0c' else 'branch perf/append-cf-compaction @ fa8b646f'
    commit='%s @ %s (node %s, %s)'%(os.path.basename(s['worktree']),s['commit'][:8],md5,br)
    tb=0
    for f in sorted(os.listdir(R+d)):
        p=R+d+'/'+f
        if f=='perf.data': tierA.append(p)
        elif f in ('val0.log.gz','val1.log.gz','val2.log.gz','buckets.csv','tasks.txt','sampler.csv'): tierB.append(p)
    if t=='tw-r1':
        for f in sorted(os.listdir(R+d+'/wal-val0-keep')):
            if f.endswith('.log'): tierB.append(R+d+'/wal-val0-keep/'+f)
    sa=sum(os.path.getsize(p) for p in tierA if p.startswith(R+d+'/'))
    sb=sum(os.path.getsize(p) for p in tierB if p.startswith(R+d+'/'))
    idx.append('| `%s` | %s | 2026-10-11 | %s | Phase 3 step 0 sizing on tw (`TORUS_TRADE_HISTORY=0` + `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048`), Classic, standard shape at 300 markets, RocksDB LOG saved; %s | %s | R | %s | %s |'%(d,h(size(R+d)),commit,what,SEC,h(sa) if sa else '-',h(sb) if sb else '-'))
    hd=s['headline']; c=s['cell']
    env='6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB'+('; crash kill val1 at +60 / +180 / +300 s' if '-c' in t else '')+('; perf val0' if '-p' in t else '')+('; acc3 node' if t=='tw-old1' else '')+('; val0 WAL kept' if t=='tw-r1' else '')
    ag='AGREE' if s['agreement'].get('validators_agree') or s['agreement'].get('agreement_verdict')=='AGREE' else str(s['agreement'].get('agreement_verdict'))
    cel.append('| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |'.format(hd['matched_s_avg'],hd['matched_s_first120'],hd['matched_s_best60'],hd['placed_s_avg'])%(
        d,s['status'],s['generated_at'][:16].replace('T',' '),os.path.basename(s['worktree']),s['commit'][:8],md5,c['markets'],c['duration_s'],c['block_cap'],env,hd['blk_s_avg'],hd['txs_per_block_avg'],hd['chain_ms'],hd['engine_ms_per_1k_fills'],ag,s['liveness']['verdict'],s['validity']['verdict']))
for d,what in (('ozarchy-p3s-build','build of node + bench from main fd9e5dfa (build.log, md5s.txt)'),('ozarchy-p3s-run','p3s campaign log, done marker, launcher handoff and tables, analyst outputs (`analyst/`: decoded WAL tables `wal/batches.bin` / `entries.bin`, coalesce / threads / ceiling / restart / budget JSON, perf folded stacks)'),
               ('ozarchy-p3s-stage','staged node fb460a0c and bench af556aec (`n/release/`; kept: Phase 3 baseline node)'),('ozarchy-p3s-tools','launcher scripts (build, part1-3, handoff tables) and analyst scripts (`a-*.py`, `waldec/waldec.c` WAL decoder)')):
    idx.append('| `%s` | %s | 2026-10-11 | fd9e5dfa (fb460a0c) | %s | %s | R | - | - |'%(d,h(size(R+d)),what,SEC))
# folded stacks in run/analyst are derived perf dumps (Tier A like *-tools dumps)
for f in ('folded-tw-p1.txt','folded-tw-p2.txt'): tierA.append(A+f)
SA=sum(os.path.getsize(p) for p in tierA); SB=sum(os.path.getsize(p) for p in tierB)
open(A+'archive-index.txt','w').write('\n'.join(sorted(idx))+'\n')
open(A+'archive-cells.txt','w').write('\n'.join(sorted(cel))+'\n')
blk=['Tier A, %s, %d files:'%(h(SA),len(tierA)),'','```','rm -- \\']+["  '%s' \\"%p for p in tierA]
blk[-1]=blk[-1][:-2]; blk+=['```','','Tier B, %s, %d files:'%(h(SB),len(tierB)),'','```','rm -- \\']+["  '%s' \\"%p for p in tierB]
blk[-1]=blk[-1][:-2]; blk+=['```']
open(A+'archive-deletable.txt','w').write('\n'.join(blk)+'\n')
tot=sum(size(R+d) for d in ['ozarchy-p3s-300m-'+t for t,_ in CELLS]+['ozarchy-p3s-build','ozarchy-p3s-run','ozarchy-p3s-stage','ozarchy-p3s-tools'])
print('TierA',SA,len(tierA),h(SA),'TierB',SB,len(tierB),h(SB),'A+B',SA+SB,h(SA+SB),'total dirs',tot,h(tot))
