#!/usr/bin/env python3
"""compare.py: crab vs main table from ipc-w1.json / ipc-w2.json. Per 1k fills (W1 fills_sampled);
W2 events expressed per 1k instructions (MPKI) because W2 crossed the bench end."""
import json
R = "/home/oz/bench-results-matched/ozarchy-ipc-"
def ld(c, w): return json.load(open(f"{R}{c}/ipc-{w}.json"))
def ev(j, kind, g, n=None):
    return j["groups"].get(g, {}) if kind == "g" else j[kind]["process" if g is None else g].get(n, {})
cols = "Mcyc Minst IPC L1dMPKI DRAMpk BrMPKI | L3hitMPKI dTLBMPKI ICMPKI IPCw2"
def line(lbl, c1, c2, k):
    cy, ins = c1.get("cycles", 0), c1.get("instructions", 0)
    if not cy: return f"{lbl:34s} -"
    mp = lambda e, d: 1000 * d.get(e, 0) / max(d.get("instructions", 0), 1)
    return (f"{lbl:34s} {cy*k/1e6:7.2f} {ins*k/1e6:7.2f} {ins/cy:5.2f} {mp('L1-dcache-load-misses',c1):6.1f} "
            f"{mp('ls_dmnd_fills_from_sys.mem_io_local',c1):6.2f} {mp('branch-misses',c1):5.2f} | "
            f"{mp('ls_dmnd_fills_from_sys.ext_cache_local',c2):6.2f} {mp('ls_l1_d_tlb_miss.all',c2):6.2f} "
            f"{mp('ic_tag_hit_miss.instruction_cache_miss',c2):6.2f} {c2.get('instructions',0)/max(c2.get('cycles',1),1):5.2f}")
J = {c: (ld(c, "w1"), ld(c, "w2")) for c in ("crab", "main")}
print("per 1k fills (Mcyc, Minst); MPKI = per 1k instructions; DRAMpk = DRAM demand fills per 1k instr")
print(f"{'':34s} {cols}")
for g in ("process", "exec", "flush", "rpc"):
    for c in ("crab", "main"):
        a, b = J[c]; print(line(f"{g} {c}", ev(a, "g", g), ev(b, "g", g), 1000 / a["fills_sampled"]))
names = list(J["crab"][0]["incl"]["process"].keys() | J["main"][0]["incl"]["process"].keys())
order = ["match_market","match_at_level","insert_order","place_order_with_accounts","settle_market_results_parallel","compute_market_settle_plan","execute_batch_phases","sort_native_actions","cancel_all_many","drain_book","PositionCache::flush_all","get_cf_raw","positions_for_trader","maker_fill_fits"]
for kind in ("incl", "self"):
    for n in order:
        for c in ("crab", "main"):
            a, b = J[c]; print(line(f"{kind} {n[:22]} {c}", a[kind]["process"].get(n, {}), b[kind]["process"].get(n, {}), 1000 / a["fills_sampled"]))
