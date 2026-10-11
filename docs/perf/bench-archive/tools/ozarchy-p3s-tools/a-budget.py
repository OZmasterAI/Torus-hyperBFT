# Restart budget per interval: DB open + serial replay of the window's blocks (+ optional today's exec-queue rewind) + replay end -> first commit.
import json,statistics as st
A='/home/oz/bench-results-matched/ozarchy-p3s-run/analyst/'
R=json.load(open(A+'restart.json'))
nb=[6.369,6.293]  # tw-r1, tw-r2 native blk/s (summary.json headline.native_blk_s)
rate=st.mean(nb)
G=sum(r['gap'] for r in R); Tt=sum(r['replay_s_marker'] for r in R)
pooled=G/Tt
first=[r['first_block_s'] for r in R]
steady=(G-len(R))/(Tt-sum(first))
slow_steady=min(r['replay_blk_s_excl_first'] for r in R)
db=[r['db_open_s']+(r['restart_to_proc_start'])+r['dbopen_to_replay_start'] for r in R]  # restart -> replay start
e2c=[r['replay_end_to_first_commit'] for r in R]
maxgap=max(r['gap'] for r in R)
print('native blk/s tw-r1/r2 mean %.3f; pooled serial replay %.3f blk/s (%d blocks / %.2f s); cold first block mean %.2f s max %.2f; steady excl first pooled %.3f blk/s, slowest kill %.3f'%(rate,pooled,G,Tt,st.mean(first),max(first),steady,slow_steady))
print('restart->replay start (proc start + DB open + to gap line) mean %.2f max %.2f; replay end -> first commit mean %.2f max %.2f'%(st.mean(db),max(db),st.mean(e2c),max(e2c)))
out={}
for name,blocks in (('15 s',15*rate),('30 s',30*rate),('100 blocks',100),('min(30 s,100)',min(30*rate,100))):
    for extra_name,extra in (('window only',0),('+ max rewind today (%d)'%maxgap,maxgap)):
        n=blocks+extra
        central=st.mean(db)+n/pooled+st.mean(e2c)
        model=st.mean(db)+st.mean(first)+(n-1)/steady+st.mean(e2c)
        pess=max(db)+max(first)+(n-1)/slow_steady+max(e2c)
        out[name+' | '+extra_name]=dict(blocks=n,replay_pooled_s=n/pooled,total_central=central,total_model=model,total_pessimistic=pess)
        print('%-14s %-26s blocks %6.1f | replay at pooled %.1f s | total central %.1f s, cold+steady model %.1f s, pessimistic %.1f s | gate 60 s'%(name,extra_name,n,n/pooled,central,model,pess))
json.dump(dict(rate=rate,pooled=pooled,steady=steady,slow_steady=slow_steady,first_mean=st.mean(first),db_mean=st.mean(db),db_max=max(db),e2c_mean=st.mean(e2c),e2c_max=max(e2c),budget=out),open(A+'budget.json','w'),indent=1)
