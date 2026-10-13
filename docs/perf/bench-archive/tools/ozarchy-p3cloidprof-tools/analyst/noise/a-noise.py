#!/usr/bin/env python3
"""a-noise.py: per-cell values, per-arm mean/SD and paired c100-c0 delta with 95% CI t(2) for the section-60 frames.
Reads ../prof-cells.json (written by a-prof.py: W1 cycles:u val1, ms CPU per 1k val1 W1 fills, inline-expanded). Read-only.
Usage: a-noise.py <prof-cells.json>"""
import json, re, sys, statistics as st
d = json.load(open(sys.argv[1]))
P = "ozarchy-p3cloidprof-300m-"
R = ("r1", "r2", "r3")
T2 = 4.302653
def sumk(r, key, pred): return sum(v for k, v in r[key].items() if pred(k))
def grp(g): return lambda r: sum(v for k, v in r["grp"].items() if g == "all" or k == g)
def cat(g, lab): return lambda r: sumk(r, "cat", lambda k: k.startswith(g + "|") and k.endswith("|" + lab))
def incl(g, name): return lambda r: r["incl"].get(f"{g}|{name}", 0.0)
def self_(g, rx):
    gs = ("exec", "pool", "other") if g == "all" else (g,)
    return lambda r: sumk(r, "self_f", lambda k: k.split("|", 1)[0] in gs and re.search(rx, k.split("|", 1)[1]))
def add(*fs): return lambda r: sum(f(r) for f in fs)
KEC = r"^(__KeccakF1600|KECCAK_ASM_SHA3_|keccak256 |alloy_primitives::utils::keccak256|<sha3|<tiny_keccak|<keccak)"
ROWS = [
 ("group exec total", grp("exec")), ("group pool total", grp("pool")), ("group other total", grp("other")), ("group all", grp("all")),
 ("exec site CloidMarkets::absorb (excl. drain)", cat("exec", "CloidMarkets::absorb")),
 ("exec site OrderBook::drain_cloid_log", cat("exec", "OrderBook::drain_cloid_log")),
 ("exec incl CloidMarkets::absorb", incl("exec", "<CloidMarkets>::absorb")),
 ("exec incl CloidMarkets::live", incl("exec", "live")),
 ("exec site duplicate_cloid", cat("exec", "prepass duplicate_cloid")),
 ("exec site prepass serial block (claimed set, retain)", cat("exec", "prepass serial block (claimed set, retain)")),
 ("exec incl claimed-set reserve_rehash RawTable<((Address,u128),())>", incl("exec", "<hashbrown::raw::RawTable<((Address, u128), ())>>::reserve_rehash")),
 ("exec site forget_cloid", cat("exec", "OrderBook::forget_cloid")),
 ("pool site forget_cloid", cat("pool", "OrderBook::forget_cloid")),
 ("pool site book contains_key (order_book.rs:1397-1402)", cat("pool", "book place contains_key (order_book.rs:1397-1402)")),
 ("pool site index_cloid", cat("pool", "OrderBook::index_cloid")),
 ("exec+pool all cloid sites", lambda r: sumk(r, "cat", lambda k: k.split("|")[0] in ("exec", "pool") and k.split("|")[1] != "none")),
 ("exec incl hashbrown find_inner", incl("exec", "find_inner")),
 ("pool incl hashbrown find_inner", incl("pool", "find_inner")),
 ("exec incl make_hash", incl("exec", "make_hash")),
 ("pool incl make_hash", incl("pool", "make_hash")),
 ("exec self _mm_movemask_epi8 (probe)", self_("exec", r"^_mm_movemask_epi8 ")),
 ("pool self _mm_movemask_epi8 (probe)", self_("pool", r"^_mm_movemask_epi8 ")),
 ("exec self spec_eq (key compare)", self_("exec", r"^spec_eq ")),
 ("pool self spec_eq (key compare)", self_("pool", r"^spec_eq ")),
 ("exec self foldhash (all foldhash leaves)", self_("exec", r"foldhash")),
 ("pool self foldhash (all foldhash leaves)", self_("pool", r"foldhash")),
 ("exec self foldhash::hash_bytes_long", self_("exec", r"^foldhash::hash_bytes_long ")),
 ("exec self Keccak (F1600+absorb/squeeze+wrappers)", self_("exec", KEC)),
 ("pool self Keccak", self_("pool", KEC)),
 ("other self Keccak", self_("other", KEC)),
 ("all self __KeccakF1600 only", self_("all", r"^__KeccakF1600 ")),
 ("all self Keccak (F1600+absorb/squeeze+wrappers)", self_("all", KEC)),
 ("other incl verify_one_action_with", incl("other", "torus_rpc::torus::verify_one_action_with")),
]
def ci(xs):
    m = st.mean(xs); s = st.stdev(xs); h = T2 * s / len(xs) ** 0.5
    return m, s, m - h, m + h
out = []
hdr = f"{'frame':62} " + " ".join(f"{r+'-'+a:>9}" for a in ("c0", "c100") for r in R) + f" {'c0 mean±sd':>15} {'c100 mean±sd':>15} {'delta':>8} {'95% CI t(2)':>19} {'d/sd_d':>6}  resolved"
out.append("W1 cycles:u val1, ms CPU per 1k val1 W1 fills (a-prof.py normalisation: period share x val1 W1 utime / W1 fills), per cell; paired by round r1-r3\n")
out.append(hdr)
for name, f in ROWS:
    v = {(r, a): f(d[P + r + "-" + a]) for r in R for a in ("c0", "c100")}
    c0 = [v[(r, "c0")] for r in R]; c1 = [v[(r, "c100")] for r in R]
    dl = [b - a for a, b in zip(c0, c1)]
    m, s, lo, hi = ci(dl)
    res = "yes" if (lo > 0 or hi < 0) else "no"
    out.append(f"{name:62} " + " ".join(f"{x:9.3f}" for x in c0 + c1)
               + f" {st.mean(c0):7.3f}±{st.stdev(c0):6.3f} {st.mean(c1):7.3f}±{st.stdev(c1):6.3f} {m:+8.3f} [{lo:+8.3f},{hi:+8.3f}] {abs(m)/s if s else float('inf'):6.1f}  {res}")
print("\n".join(out))
