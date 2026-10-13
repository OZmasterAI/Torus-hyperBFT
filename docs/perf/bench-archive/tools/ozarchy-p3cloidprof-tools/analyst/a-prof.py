#!/usr/bin/env python3
"""a-prof.py (ozarchy-p3cloidprof): where the cloid engine cost comes from.

Usage: a-prof.py <out_dir> <cell_dir>...   (cell dirs named ...-<round>-c0 / -c100; need prof.script.gz, w2.script.gz
       made by run-analysis.sh: perf script -F comm,tid,period[,event],ip,sym,symoff --no-inline)

W1 (perf.data, cycles:u, whole val1 process, fp call graphs):
  ms CPU per 1k fills of a sample set = share of the process' cycles:u period x val1 utime (s) over W1 / val1 fills over W1 x 1e6
  (buckets2.py method; window = prof-{stat,metrics}-w1-{before,after}).
  Thread groups: exec = the exec tid (exec-tid.txt), pool = comm torus-exec-pool (rayon market workers), other.
  Frames are inline-expanded with llvm-addr2line -i (leaf ip, return address - 1 for callers): logical stack root -> leaf.
W2 (perf-w2.data, exec thread only, 5 events): event counts (sum of sample periods) per 1k val1 fills over W2.
Cloid site of a sample = innermost logical frame matching SITES (category of candidate 1-4).
"""

import collections, gzip, json, os, re, subprocess, sys, statistics as st

ELF = "/home/oz/bench-results-matched/ozarchy-p3cloid-stage/cl-run/release/torus-node"
HASH = re.compile(r"\[[0-9a-f]{6,16}\]")
HDR = re.compile(r"^(\S.*?)\s+(\d+)\s+(\d+)\s*(?:(\S+):\s*)?$")
CLK = os.sysconf("SC_CLK_TCK")
OUT = sys.argv[1]
CELLS = sys.argv[2:]

# ---- cloid sites (candidate, label): innermost match wins. Patterns on (short func, loc) ----
# loc = crates/<crate>/src/<file>:<line>
SITES = [
    # (2) duplicate check: serial cross-market pre-pass (native_executor.rs ~6880-6935, CloidMarkets::live,
    #     duplicate_cloid, CloidMarkets::build, the claimed HashSet) and the per-book check in OrderBook::place (order_book.rs:1398-1401)
    (
        "2 dup-check",
        "prepass CloidMarkets::live",
        lambda f, l: "CloidMarkets>::live" in f or f.endswith("CloidMarkets::live"),
    ),
    (
        "2 dup-check",
        "prepass duplicate_cloid",
        lambda f, l: f.endswith("duplicate_cloid"),
    ),
    (
        "2 dup-check",
        "prepass CloidMarkets::build",
        lambda f, l: "CloidMarkets>::build" in f or f.endswith("CloidMarkets::build"),
    ),
    (
        "2 dup-check",
        "prepass serial block (claimed set, retain)",
        lambda f, l: _ln(l, "native_executor.rs", 6879, 6940),
    ),
    (
        "2 dup-check",
        "book place contains_key (order_book.rs:1397-1402)",
        lambda f, l: _ln(l, "order_book.rs", 1397, 1402),
    ),
    # (1) index insert / lookup on place: the book's (trader, cloid) map insert + log push (index_cloid),
    #     OrderBook::order_id_by_cloid lookups, and the cross-market index fed by CloidMarkets::absorb / drain_cloid_log
    (
        "1 index",
        "CloidMarkets::absorb",
        lambda f, l: "CloidMarkets>::absorb" in f or f.endswith("CloidMarkets::absorb"),
    ),
    (
        "1 index",
        "OrderBook::drain_cloid_log",
        lambda f, l: f.endswith("drain_cloid_log") or "drain_cloid_log" in f,
    ),
    ("1 index", "OrderBook::index_cloid", lambda f, l: f.endswith("index_cloid")),
    (
        "1 index",
        "OrderBook::order_id_by_cloid",
        lambda f, l: f.endswith("order_id_by_cloid"),
    ),
    (
        "1 index",
        "OrderBook::clear_cloid_log / cloid_entries",
        lambda f, l: f.endswith("clear_cloid_log") or f.endswith("cloid_entries"),
    ),
    # (3) cleanup on fill / cancel: forget_cloid(s) (cancel-all order_book.rs:2946, maker-fill removals, stops)
    ("3 cleanup", "OrderBook::forget_cloid", lambda f, l: f.endswith("forget_cloid")),
    ("3 cleanup", "OrderBook::forget_cloids", lambda f, l: f.endswith("forget_cloids")),
]


def _ln(loc, fname, a, b):
    m = re.search(re.escape(fname) + r":(\d+)$", loc)
    return bool(m) and a <= int(m.group(1)) <= b


def short(f):
    f = re.sub(r"<torus_state::backend::NativeStateOverlay>", "", f)
    f = re.sub(r"::\{closure#\d+\}", "{cl}", f)
    for p in (
        "torus_bridge::native_executor::",
        "torus_core::order_book::",
        "torus_core::",
        "torus_types::",
        "alloy_primitives::bits::address::",
        "torus_consensus::app::",
    ):
        f = f.replace(p, "")
    f = re.sub(r" \(\.llvm\.\d+\)", "", f)
    if not f.startswith("<"):
        f = f.split("<", 1)[0] or f
    f = re.sub(r"::<.*$", "", f)
    f = re.sub(r"::$", "", f)
    return f[:150]


def hb_short(f):
    """hashbrown / core::hash helpers: keep the operation name (find, reserve_rehash, insert, remove ...)."""
    return f


def met(p):
    d = {}
    for l in open(p):
        if l.startswith("#") or not l.strip():
            continue
        k, v = l.rsplit(" ", 1)
        try:
            d[k] = d.get(k, 0) + float(v)
        except ValueError:
            pass
    return d


def mval(d, name):
    return sum(v for k, v in d.items() if k == name or k.startswith(name + "{"))


def utime(p):
    s = open(p).read().split(")")[1].split()
    return int(s[11]) / CLK, int(s[12]) / CLK


def window(cell, w):
    a, b = (
        met(f"{cell}/prof-metrics-{w}-before.txt"),
        met(f"{cell}/prof-metrics-{w}-after.txt"),
    )
    ua, sa = utime(f"{cell}/prof-stat-{w}-before.txt")
    ub, sb = utime(f"{cell}/prof-stat-{w}-after.txt")
    t0, t1 = (
        float(open(f"{cell}/prof-{w}-before.ts").read()),
        float(open(f"{cell}/prof-{w}-after.ts").read()),
    )
    fills = mval(b, "torus_orders_matched_total") - mval(
        a, "torus_orders_matched_total"
    )
    blk = mval(b, "torus_exec_native_blocks_total") - mval(
        a, "torus_exec_native_blocks_total"
    )
    ph = {
        k: mval(b, f"torus_exec_{k}_seconds_sum")
        - mval(a, f"torus_exec_{k}_seconds_sum")
        for k in (
            "engine",
            "engine_untimed",
            "phase1_actions",
            "phase_margin",
            "phase_match",
            "phase_settle",
        )
    }
    return dict(fills=fills, blk=blk, span=t1 - t0, utime=ub - ua, stime=sb - sa, ph=ph)


def tasks_cpu(cell, w, tid):
    def rd(p):
        r = {}
        for l in open(p):
            x = l.split()
            if len(x) >= 4:
                r[x[0]] = (x[1], int(x[2]) / CLK, int(x[3]) / CLK)
        return r

    a, b = (
        rd(f"{cell}/prof-tasks-{w}-before.txt"),
        rd(f"{cell}/prof-tasks-{w}-after.txt"),
    )
    if tid in a and tid in b:
        return b[tid][1] - a[tid][1]
    return None


def raw(script_gz, with_event=False):
    comm = tid = ev = None
    per = 0
    fr = []
    for line in gzip.open(script_gz, "rt", errors="replace"):
        if not line.strip():
            if comm is not None:
                yield comm, tid, per, ev, fr
            comm = None
            fr = []
            continue
        if line[0] not in " \t":
            m = HDR.match(line.rstrip("\n"))
            if not m:
                comm = None
                continue
            comm, tid, per, ev = (
                m.group(1).strip(),
                m.group(2),
                int(m.group(3)),
                m.group(4),
            )
            fr = []
            continue
        p = line.strip().split(" ", 1)
        try:
            ip = int(p[0], 16)
        except ValueError:
            continue
        sym = p[1] if len(p) > 1 else "[unknown]"
        off = 0
        m = re.match(r"^(.*)\+0x([0-9a-f]+)$", sym)
        if m:
            sym, off = m.group(1), int(m.group(2), 16)
        fr.append((ip, HASH.sub("", sym), off))
    if comm is not None:
        yield comm, tid, per, ev, fr


def text_range():
    for l in subprocess.run(
        ["readelf", "-SW", ELF], capture_output=True, text=True
    ).stdout.splitlines():
        p = l.split()
        if ".text" in p:
            i = p.index(".text")
            return int(p[i + 2], 16), int(p[i + 2], 16) + int(p[i + 4], 16)


NMD = {}
for l in subprocess.run(
    ["nm", "-C", ELF], capture_output=True, text=True
).stdout.splitlines():
    x = l.split(" ", 2)
    if len(x) == 3 and x[1] in ("t", "T", "w", "W") and x[0]:
        NMD.setdefault(HASH.sub("", x[2]), int(x[0], 16))
LO, HI = text_range()
CACHE = os.path.join(OUT, "a2l-cache.json")
CHAIN = (
    {int(k): v for k, v in json.load(open(CACHE)).items()}
    if os.path.exists(CACHE)
    else {}
)


def base_of(samples):
    """load bias: ip - symoff - nm address of a known binary symbol (perf demangled name)."""
    votes = collections.Counter()
    for _, _, _, _, fr in samples[:20000]:
        for ip, s, off in fr:
            if s in NMD:
                votes[ip - off - NMD[s]] += 1
    return votes.most_common(1)[0][0]


def a2l(addrs):
    todo = sorted(a for a in addrs if a not in CHAIN)
    for i in range(0, len(todo), 20000):
        chunk = todo[i : i + 20000]
        out = subprocess.run(
            ["llvm-addr2line", "-a", "-i", "-f", "-C", "-e", ELF],
            input="\n".join(hex(a) for a in chunk),
            capture_output=True,
            text=True,
        ).stdout.splitlines()
        cur = None
        k = 0
        while k < len(out):
            l = out[k]
            if l.startswith("0x"):
                cur = int(l, 16)
                CHAIN[cur] = []
                k += 1
                continue
            fn, loc = l, (out[k + 1] if k + 1 < len(out) else "?")
            k += 2
            loc = re.sub(
                r"^.*?/(crates|library|\.cargo/registry/src/[^/]+)/", r"\1/", loc
            )
            loc = re.sub(r" \(discriminator \d+\)", "", loc)
            loc = re.sub(r":(\d+):\d+$", r":\1", loc)
            CHAIN[cur].append((short(HASH.sub("", fn)), loc))
    json.dump({str(k): v for k, v in CHAIN.items()}, open(CACHE, "w"))


def logical(samples):
    b = base_of(samples)
    want = set()
    for _, _, _, _, fr in samples:
        for i, (ip, s, off) in enumerate(fr):
            a = ip - b - (1 if i > 0 else 0)
            if LO <= a < HI:
                want.add(a)
    a2l(want)
    res = []
    for comm, tid, per, ev, fr in samples:
        stck = []
        for i in range(len(fr) - 1, -1, -1):
            ip, s, off = fr[i]
            a = ip - b - (1 if i > 0 else 0)
            if a in CHAIN:
                stck.extend(tuple(x) for x in reversed(CHAIN[a]))
            elif not s.startswith("[unknown]"):
                stck.append((short(s), "?"))
        res.append((comm, tid, per, ev, stck))
    return res


def site(stck):
    for f, l in reversed(stck):
        for cand, lab, fn in SITES:
            if fn(f, l):
                return cand, lab
    return None, None


def group(comm, tid, et):
    if tid == et:
        return "exec"
    if comm.startswith("torus-exec-poo"):
        return "pool"
    return "other"


def leafname(stck):
    if not stck:
        return "[unknown]"
    f, l = stck[-1]
    fl = re.sub(r":\d+$", "", l)
    fl = re.sub(r"^\.cargo/registry/src/", "", fl)
    fl = re.sub(r"^(crates/[^/]+)/src/", r"\1/", fl)
    return f"{f} [{fl}]"


def arm_of(c):
    return "c100" if c.endswith("-c100") else "c0"


def mean(x):
    return sum(x) / len(x) if x else 0.0


rows = {}  # cell -> dict
fold = {"c0": collections.Counter(), "c100": collections.Counter()}
for cell in CELLS:
    name = os.path.basename(cell.rstrip("/"))
    et = open(f"{cell}/exec-tid.txt").read().strip()
    w1, w2 = window(cell, "w1"), window(cell, "w2")
    smp = logical(list(raw(f"{cell}/prof.script.gz")))
    tot = sum(s[2] for s in smp)
    K = lambda per: per / tot * w1["utime"] / w1["fills"] * 1e6
    self_f = collections.defaultdict(float)  # (group, leaf func) -> ms/1k
    self_loc = collections.defaultdict(float)  # (group, leaf func @ loc) -> ms/1k
    incl = collections.defaultdict(
        float
    )  # (group, func) -> ms/1k inclusive (once per sample)
    cat = collections.defaultdict(float)  # (group, cand, label) -> ms/1k
    hb = collections.defaultdict(
        float
    )  # (group, hashbrown/hash leaf, cloid cand or none) -> ms/1k
    grp = collections.defaultdict(float)
    for comm, tid, per, ev, stck in smp:
        g = group(comm, tid, et)
        v = K(per)
        grp[g] += v
        lf = leafname(stck)
        self_f[(g, lf)] += v
        self_loc[(g, lf + " @ " + (stck[-1][1] if stck else "?"))] += v
        for f in set(f for f, _ in stck):
            incl[(g, f)] += v
        cand, lab = site(stck)
        cat[(g, cand or "none", lab or "-")] += v
        if re.search(r"hashbrown|foldhash|core::hash|BuildHasher|Hasher", lf):
            hb[(g, lf, cand or "none")] += v
        fold[arm_of(name)][
            (g + ";" + ";".join(f for f, _ in stck)).replace(" ", "_")
        ] += per / tot * w1["utime"] / w1["fills"] * 1e6
    et_cpu = tasks_cpu(cell, "w1", et)
    # W2: exec thread events
    ev_tot = collections.defaultdict(float)
    ev_cat = collections.defaultdict(float)
    ev_leaf = collections.defaultdict(float)
    if os.path.exists(f"{cell}/w2.script.gz"):
        s2 = logical(list(raw(f"{cell}/w2.script.gz")))
        for comm, tid, per, ev, stck in s2:
            e = (
                (ev or "?")
                .replace(":u", "")
                .replace("ls_dmnd_fills_from_sys.", "fill.")
            )
            x = per / max(w2["fills"], 1) * 1000.0
            ev_tot[e] += x
            cand, lab = site(stck)
            ev_cat[(e, cand or "none")] += x
            ev_leaf[(e, leafname(stck))] += x
    rows[name] = dict(
        arm=arm_of(name),
        w1=w1,
        w2=w2,
        et=et,
        et_cpu=et_cpu,
        samples=len(smp),
        grp=dict(grp),
        self_f={"|".join(k): v for k, v in self_f.items()},
        self_loc={"|".join(k): v for k, v in self_loc.items()},
        incl={"|".join(k): v for k, v in incl.items()},
        cat={"|".join(k): v for k, v in cat.items()},
        hb={"|".join(k): v for k, v in hb.items()},
        ev_tot=dict(ev_tot),
        ev_cat={"|".join(k): v for k, v in ev_cat.items()},
        ev_leaf={"|".join(k): v for k, v in ev_leaf.items()},
    )
    print(
        f"loaded {name}: {len(smp)} samples, fills W1 {w1['fills']:.0f}, utime {w1['utime']:.1f}s",
        file=sys.stderr,
    )

json.dump(rows, open(os.path.join(OUT, "prof-cells.json"), "w"), indent=1)
for a in fold:
    n = sum(1 for r in rows.values() if r["arm"] == a)
    with open(os.path.join(OUT, f"folded-{a}.txt"), "w") as fo:
        for k, v in fold[a].most_common():
            fo.write(
                f"{k} {v / max(n, 1) * 1000:.0f}\n"
            )  # unit: microseconds CPU per 1k fills (mean over the arm's cells)

arms = {
    "c0": [r for r in rows.values() if r["arm"] == "c0"],
    "c100": [r for r in rows.values() if r["arm"] == "c100"],
}


def am(arm, key, k):
    return mean([r[key].get(k, 0.0) for r in arms[arm]])


def table(key, keys, title, fo, top=25, groups=("exec", "pool", "other", "all")):
    allk = set()
    for r in rows.values():
        allk |= set(r[key])
    for g in groups:
        ks = sorted(
            set(k.split("|", 1)[1] for k in allk if g == "all" or k.startswith(g + "|"))
        )

        def val(arm, kk):
            if g == "all":
                return sum(
                    am(arm, key, f"{gg}|{kk}") for gg in ("exec", "pool", "other")
                )
            return am(arm, key, f"{g}|{kk}")

        tc0 = mean(
            [
                sum(v for kk, v in r["grp"].items() if g == "all" or kk == g)
                for r in arms["c0"]
            ]
        )
        tc1 = mean(
            [
                sum(v for kk, v in r["grp"].items() if g == "all" or kk == g)
                for r in arms["c100"]
            ]
        )
        d = tc1 - tc0
        fo.write(
            f"\n== {title} -- thread group {g}: total c0 {tc0:.3f} c100 {tc1:.3f} delta {d:+.3f} ms CPU/1k fills ({(d / tc0 * 100 if tc0 else 0):+.1f}%)\n"
        )
        fo.write(f"{'c0':>8} {'c100':>8} {'delta':>8} {'%dlt':>6}  frame\n")
        vals = [(val("c0", kk), val("c100", kk), kk) for kk in ks]
        vals.sort(key=lambda x: -abs(x[1] - x[0]))
        for c0, c1, kk in vals[:top]:
            fo.write(
                f"{c0:8.3f} {c1:8.3f} {c1 - c0:+8.3f} {((c1 - c0) / d * 100 if d else 0):6.1f}  {kk}\n"
            )
        fo.write("top by c100 self:\n")
        vals.sort(key=lambda x: -x[1])
        for c0, c1, kk in vals[:12]:
            fo.write(f"{c0:8.3f} {c1:8.3f} {c1 - c0:+8.3f}  {kk}\n")


with open(os.path.join(OUT, "frames-self.txt"), "w") as fo:
    fo.write(
        "W1 cycles:u, val1, self time (inline-expanded leaf function) in ms CPU per 1k fills, mean over cells per arm\n"
    )
    fo.write("cells: " + " ".join(f"{k}({r['arm']})" for k, r in rows.items()) + "\n")
    table("self_f", None, "SELF by function", fo, top=30)
with open(os.path.join(OUT, "frames-self-loc.txt"), "w") as fo:
    fo.write("W1 self time by leaf function @ file:line, ms CPU/1k fills\n")
    table(
        "self_loc",
        None,
        "SELF by function@line",
        fo,
        top=40,
        groups=("exec", "pool", "all"),
    )
with open(os.path.join(OUT, "frames-children.txt"), "w") as fo:
    fo.write(
        "W1 inclusive (children) time per logical frame, ms CPU/1k fills (counted once per sample)\n"
    )
    table(
        "incl", None, "CHILDREN by function", fo, top=40, groups=("exec", "pool", "all")
    )
with open(os.path.join(OUT, "cloid-sites.txt"), "w") as fo:
    fo.write(
        "W1 samples by innermost cloid site on the logical stack (inclusive of callees: hashbrown, memcpy...), ms CPU/1k fills\n"
    )
    table("cat", None, "CLOID SITE (candidate|label)", fo, top=30)
    fo.write(
        "\nhashbrown / hash leaf frames by cloid site of the sample (candidate 'none' = not under a cloid site)\n"
    )
    table(
        "hb", None, "HASH LEAF (leaf|cand)", fo, top=30, groups=("exec", "pool", "all")
    )
with open(os.path.join(OUT, "llc.txt"), "w") as fo:
    fo.write(
        "W2 exec thread only (perf record -t <exec tid> --no-inherit, 20 s), event counts per 1k val1 fills over W2, mean per arm\n"
    )
    fo.write(
        "fill.mem_io_local = demand data-cache fills from local DRAM (= LLC miss; LLC-load-misses unsupported on Zen 3)\n"
    )
    fo.write("fill.ext_cache_local = demand fills from the other CCD's L3\n\n")
    evs = sorted(set(k for r in rows.values() for k in r["ev_tot"]))
    fo.write(
        f"{'event':32} {'c0':>14} {'c100':>14} {'delta%':>8}   per-cell c0 | c100\n"
    )
    for e in evs:
        c0, c1 = am("c0", "ev_tot", e), am("c100", "ev_tot", e)
        fo.write(
            f"{e:32} {c0:14.0f} {c1:14.0f} {((c1 - c0) / c0 * 100 if c0 else 0):+8.1f}   "
            + " ".join(f"{r['ev_tot'].get(e, 0):.0f}" for r in arms["c0"])
            + " | "
            + " ".join(f"{r['ev_tot'].get(e, 0):.0f}" for r in arms["c100"])
            + "\n"
        )
    for e in evs:
        fo.write(f"\n{e} by cloid site (per 1k fills)\n")
        ks = sorted(
            set(
                k.split("|", 1)[1]
                for r in rows.values()
                for k in r["ev_cat"]
                if k.startswith(e + "|")
            )
        )
        for kk in ks:
            c0, c1 = am("c0", "ev_cat", f"{e}|{kk}"), am("c100", "ev_cat", f"{e}|{kk}")
            fo.write(
                f"  {kk:16} c0 {c0:12.0f} c100 {c1:12.0f} delta {c1 - c0:+12.0f}\n"
            )
        ks = sorted(
            set(
                k.split("|", 1)[1]
                for r in rows.values()
                for k in r["ev_leaf"]
                if k.startswith(e + "|")
            )
        )
        vals = sorted(
            (
                (
                    am("c0", "ev_leaf", f"{e}|{kk}"),
                    am("c100", "ev_leaf", f"{e}|{kk}"),
                    kk,
                )
                for kk in ks
            ),
            key=lambda x: -abs(x[1] - x[0]),
        )
        fo.write(f"  top leaf frames by |delta| ({e})\n")
        for c0, c1, kk in vals[:12]:
            fo.write(
                f"    c0 {c0:12.0f} c100 {c1:12.0f} delta {c1 - c0:+12.0f}  {kk}\n"
            )
    # derived ratios
    fo.write("\nderived (exec thread, W2): ")
    for a in ("c0", "c100"):
        cyc, ins, dram, l1 = (
            am(a, "ev_tot", "cycles"),
            am(a, "ev_tot", "instructions"),
            am(a, "ev_tot", "fill.mem_io_local"),
            am(a, "ev_tot", "L1-dcache-load-misses"),
        )
        fo.write(
            f"{a}: IPC {ins / cyc if cyc else 0:.3f}, DRAM fills per 1k instr {dram / ins * 1000 if ins else 0:.3f}, L1 miss per 1k instr {l1 / ins * 1000 if ins else 0:.2f}; "
        )
    fo.write("\n")
with open(os.path.join(OUT, "windows.txt"), "w") as fo:
    fo.write(
        "profile windows on val1 (W1 = perf.data, W2 = perf-w2.data): fills, blocks, utime, phase sums\n"
    )
    for k, r in rows.items():
        for w in ("w1", "w2"):
            x = r[w]
            fo.write(
                f"{k:30} {w} span {x['span']:.1f}s fills {x['fills']:.0f} ({x['fills'] / max(x['span'], 1e-9):.0f}/s) blk {x['blk']:.0f} utime {x['utime']:.1f}s "
                f"stime {x['stime']:.1f}s engine/blk {x['ph']['engine'] / max(x['blk'], 1) * 1000:.1f}ms "
                + " ".join(
                    f"{p}/1k {v / max(x['fills'], 1) * 1e6:.3f}ms" for p, v in x["ph"].items()
                )
                + "\n"
            )
        fo.write(
            f"{k:30} exec tid {r['et']} utime W1 {r['et_cpu']}s; W1 samples {r['samples']}; group ms/1k "
            + " ".join(f"{g} {v:.3f}" for g, v in sorted(r["grp"].items()))
            + "\n"
        )
print("done", file=sys.stderr)
