"""c2h.py: ozarchy c2h campaign analysis (b/base/fix/sav/both). Per-cell + per-arm tables from summary.json (val0),
r1/r2/r3 spread, ratios vs b and vs base. Apply = end_resident_positions_ms (TraderPositions::apply timer).
sav-r1 excluded (rc=2, liveness UNKNOWN: host stall). Usage: c2h.py [out.txt]  (also prints to stdout)."""
import json, sys, statistics as st, builtins
_out = open(sys.argv[1], "w") if len(sys.argv) > 1 else None
def print(*a, **k):
    builtins.print(*a, **k)
    if _out: builtins.print(*a, **k, file=_out)
R="/home/oz/bench-results-matched/ozarchy-c2h-300m-"
tags="b-r1 base-r1 fix-r1 sav-r1 both-r1 both-r2 sav-r2 fix-r2 base-r2 b-r2 sav-r3 base-r3".split()
EXCL={"sav-r1"}
K=["mps","blk","fills","eng1k","chain","settle","passB","passB1k","match","er","pos","pos1k","erw","sw_db","flush"]
def row(t):
    s=json.load(open(R+t+"/summary.json")); h=s["headline"]; p=s["phase_by_node"]["val0"]; ph=p["phases"]; e=ph["engine"]; f=p["fills_per_native_block"]
    er=ph["end_resident"]
    return dict(mps=h["matched_s_avg"],blk=h["native_blk_s"],fills=f,eng1k=h["engine_ms_per_1k_fills"],chain=p["chain_ms"],
        settle=e["phase_settle_ms"],passB=e["settle_pass_b_ms"],passB1k=e["settle_pass_b_ms"]/f*1000,match=e["phase_match_ms"],
        er=er["ms"],pos=er["end_resident_positions_ms"],pos1k=er["end_resident_positions_ms"]/f*1000,erw=ph["end_resident_wait"]["ms"],
        sw_db=ph["flush"]["state_write_db_ms"],flush=ph["flush"]["ms"],acc=h["benchmark_accepted"],live=h["liveness_verdict"],agree=h["agreement_verdict"])
fmt={"mps":"{:,.0f}","fills":"{:,.0f}","blk":"{:.3f}","passB1k":"{:.3f}","pos1k":"{:.3f}","eng1k":"{:.2f}"}
F=lambda k,v: fmt.get(k,"{:.2f}").format(v)
rows={t:row(t) for t in tags}
print("| cell | "+" | ".join(K)+" | verdicts |"); print("|---"*(len(K)+2)+"|")
for t in tags:
    r=rows[t]; print(f"| {t} | "+" | ".join(F(k,r[k]) for k in K)+f" | {r['agree']}/{r['live']}/acc={r['acc']} |")
arms=["b","base","fix","sav","both"]
M={}
for a in arms:
    ts=[t for t in tags if t.startswith(a+"-") and t not in EXCL]
    M[a]={k:st.mean(rows[t][k] for t in ts) for k in K}
    M[a]["n"]=len(ts)
    M[a]["spread"]={k:(max(rows[t][k] for t in ts)/min(rows[t][k] for t in ts)-1)*100 for k in K} if len(ts)>1 else None
print("\nper-arm means (sav-r1 excluded)")
print("| arm | n | "+" | ".join(K)+" |"); print("|---"*(len(K)+2)+"|")
for a in arms: print(f"| {a} | {M[a]['n']} | "+" | ".join(F(k,M[a][k]) for k in K)+" |")
print("\nspread % over valid cells (max/min-1)")
for a in arms:
    if M[a]["spread"]: print(a, " ".join(f"{k}={M[a]['spread'][k]:.1f}" for k in K))
for ref in ["b","base"]:
    print(f"\nratios vs {ref}")
    for a in arms:
        if a==ref: continue
        print(f"| {a}/{ref} | "+" | ".join(f"{k}={M[a][k]/M[ref][k]:.3f}" for k in K)+" |")
