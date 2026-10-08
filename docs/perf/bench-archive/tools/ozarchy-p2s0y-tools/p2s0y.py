#!/usr/bin/env python3
"""p2s0y.py: analysis of the ozarchy-p2s0y perf A/B (p2 = 2ebe1a14 vs p3 = 9e695364).

  p2s0y.py cells  <cell_dir>...            per-cell table (summary.json val0 + in-window node timers)
  p2s0y.py rec    <cell_dir> <elf> <w>     inline-expanded per-event attribution of perf-<w>.data -> <cell>/p2s0y-<w>.json
  p2s0y.py cmp    <w> <p2 cells,> <p3 cells,> [N]   arm means p2 vs p3 from the p2s0y-<w>.json files
  p2s0y.py stat   <p2 cells,> <p3 cells,>  perf stat table (perf-stat-w1/w2.txt)

Attribution (rec): exec-side samples (comm torus-execution*, i.e. the exec thread and its scoped workers) are expanded
with llvm-addr2line -i (ozarchy-margin-c7-tools/inl.py) and classed by logical frame:
  workers (stack has thread_start): match_parallel -> 'match (workers)', settle_market_results_parallel ->
  'settle pass A (workers)', else 'other workers';
  exec thread: settle_market_results_parallel at native_executor.rs line >= 7198 -> 'settle pass B' (the Pass B loop,
  7198-7346 in both arms), below -> 'settle pass A (exec thread)'; else by execute_batch_phases line: 5895-5952 phase 1,
  5959-6170 margin, 6172-6312 'match (exec thread)', 6404-6428 cache flush, 6342-6432 'settle other'.
Line ranges are identical in both arms (native_executor.rs differs only in a test module).
Event counts = sum of sample periods, per 1k fills of val0 (torus_orders_matched_total delta over the window)."""

import collections
import gzip
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl  # noqa: E402

HDR = re.compile(r"^(.*?)\s+(\d+)\s+([\d.]+):\s+(\d+)\s+(\S+?):\s*$")
PASS_B = (7198, 7346)
EBP = [
    ((5895, 5952), "phase 1"),
    ((5959, 6170), "margin"),
    ((6172, 6312), "match (exec thread)"),
    ((6404, 6428), "cache flush"),
    ((6342, 6432), "settle other"),
]
GROUPS = [
    ("exec", "torus-execution"),
    ("end_resident", "torus-end-resid"),
    ("flush", "torus-flush-wor"),
    ("rpc", "rpc-worker"),
    ("tokio", "tokio-rt-worker"),
    ("ingress", "torus-ingress-v"),
    ("rocksdb", "rocksdb"),
]


def metric(path, name):
    for line in open(path):
        if line.startswith(name + " ") or line.startswith(name + "{"):
            return float(line.split()[-1])
    return 0.0


def win(d, w):
    a, b = f"{d}/prof-metrics-{w}-before.txt", f"{d}/prof-metrics-{w}-after.txt"
    fills = metric(b, "torus_orders_matched_total") - metric(
        a, "torus_orders_matched_total"
    )
    span = float(open(f"{d}/prof-{w}-after.ts").read()) - float(
        open(f"{d}/prof-{w}-before.ts").read()
    )
    t = {}
    for k in (
        "engine",
        "phase_match",
        "phase_settle",
        "settle_pass_a",
        "settle_pass_b",
        "phase_margin",
    ):
        s = metric(b, f"torus_exec_{k}_seconds_sum") - metric(
            a, f"torus_exec_{k}_seconds_sum"
        )
        t[k + "_ms_per_1k"] = s * 1e6 / fills if fills else None

    def ut(p):
        s = open(p).read()
        f = s[s.rindex(")") + 2 :].split()
        return int(f[11]) / 100, int(f[12]) / 100

    u0, s0 = ut(f"{d}/prof-stat-{w}-before.txt")
    u1, s1 = ut(f"{d}/prof-stat-{w}-after.txt")
    return {
        "fills": fills,
        "span_s": span,
        "fills_per_s": fills / span,
        "utime_s": u1 - u0,
        "stime_s": s1 - s0,
        "timers": t,
    }


def split_script(d, w, data):
    """perf script of perf-<w>.data, exec comms, split per event into <cell>/<w>.<ev>.inl.script.gz (inl.py format)."""
    outs = {}
    have = [
        f
        for f in os.listdir(d)
        if f.startswith(f"{w}.") and f.endswith(".inl.script.gz")
    ]
    if have and os.path.exists(f"{d}/{w}.split.ok"):
        return {f[len(w) + 1 : -len(".inl.script.gz")]: f"{d}/{f}" for f in have}
    p = subprocess.Popen(
        [
            "perf",
            "script",
            "-F",
            "comm,tid,time,period,event,ip,sym,symoff",
            "--no-inline",
            "--comms",
            "torus-execution",
            "-i",
            data,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        errors="replace",
        bufsize=1 << 20,
    )
    cur = None
    for line in p.stdout:
        if line[:1] in (" ", "\t"):
            if cur is not None:
                cur.write(line)
            continue
        if not line.strip():
            if cur is not None:
                cur.write("\n")
            continue
        m = HDR.match(line.rstrip("\n"))
        if not m:
            cur = None
            continue
        ev = m.group(5).replace(":u", "")
        if ev not in outs:
            outs[ev] = gzip.open(f"{d}/{w}.{ev}.inl.script.gz", "wt", compresslevel=1)
        cur = outs[ev]
        cur.write(f"{m.group(1)} {m.group(2)} {m.group(3)}: {m.group(4)}\n")
    p.wait()
    for f in outs.values():
        f.close()
    open(f"{d}/{w}.split.ok", "w").write("ok\n")
    return {ev: f"{d}/{w}.{ev}.inl.script.gz" for ev in outs}


def groups(d, w, data):
    """whole-process event totals per thread group (no call graph)."""
    g = collections.defaultdict(lambda: collections.defaultdict(int))
    p = subprocess.Popen(
        [
            "perf",
            "script",
            "-F",
            "comm,tid,period,event",
            "-G",
            "-i",
            data,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        errors="replace",
        bufsize=1 << 20,
    )
    rx = re.compile(r"^(.*?)\s+(\d+)\s+(\d+)\s+(\S+?):\s*$")
    for line in p.stdout:
        m = rx.match(line.rstrip("\n"))
        if not m:
            continue
        comm, per, ev = (
            m.group(1).strip(),
            int(m.group(3)),
            m.group(4).replace(":u", ""),
        )
        g["process"][ev] += per
        for name, pfx in GROUPS:
            if comm.startswith(pfx):
                g[name][ev] += per
                break
        else:
            g["other"][ev] += per
    p.wait()
    return {k: dict(v) for k, v in g.items()}


def expand(elf, script_gz):
    """inl.load without its K (needs no perf.folded): list of (period, logical stack root->leaf of (func, loc))."""
    lo, hi = inl._text_range(elf)
    ev = inl._anchor(elf)
    raw = list(inl._raw(script_gz))
    base = None
    for _, fr in raw:
        for ip, s, off in fr:
            if s == "torus_consensus::app::execution_loop":
                base = ip - off - ev
                break
        if base is not None:
            break
    assert base is not None, "execution_loop not on any stack"
    want = set()
    for _, fr in raw:
        for i, (ip, s, off) in enumerate(fr):
            a = ip - base - (1 if i > 0 else 0)
            if lo <= a < hi:
                want.add(a)
    cache_f = script_gz + ".a2l.json"
    chain = {}
    if os.path.exists(cache_f):
        chain = {int(k): v for k, v in json.load(open(cache_f)).items()}
    todo = sorted(a for a in want if a not in chain)
    if todo:
        out = subprocess.run(
            ["llvm-addr2line", "-a", "-i", "-f", "-C", "-e", elf],
            input="\n".join(hex(a) for a in todo),
            capture_output=True,
            text=True,
        ).stdout.splitlines()
        cur, k = None, 0
        while k < len(out):
            line = out[k]
            if line.startswith("0x"):
                cur = int(line, 16)
                chain[cur] = []
                k += 1
                continue
            fn, loc = line, (out[k + 1] if k + 1 < len(out) else "?")
            k += 2
            loc = re.sub(
                r"^.*?/(crates|library|\.cargo/registry/src/[^/]+)/", r"\1/", loc
            )
            loc = re.sub(r" \(discriminator \d+\)", "", loc)
            loc = re.sub(r":(\d+):\d+$", r":\1", loc)
            chain[cur].append((inl.short(inl.HASH.sub("", fn)), loc))
        json.dump({str(k): v for k, v in chain.items()}, open(cache_f, "w"))
    samples = []
    for per, fr in raw:
        st = []
        for i in range(len(fr) - 1, -1, -1):
            ip, s, off = fr[i]
            a = ip - base - (1 if i > 0 else 0)
            if a in chain:
                st.extend(tuple(x) for x in reversed(chain[a]))
            elif not s.startswith("[unknown]"):
                st.append((inl.short(s), "?"))
        samples.append((per, st))
    return samples


def line_in(loc, fname="native_executor.rs"):
    m = re.search(re.escape(fname) + r":(\d+)$", loc)
    return int(m.group(1)) if m else None


def classify(st):
    """-> (class, index of the anchor frame: callees of the class are st[idx+1:])"""
    worker = not any("execution_loop" in f for f, _ in st)
    if worker:
        for i, (f, _) in enumerate(st):
            if "match_parallel" in f:
                return "match (workers)", i
            if "settle_market_results_parallel" in f:
                return "settle pass A (workers)", i
        return "other workers", 0
    idx = [
        i for i, (f, _) in enumerate(st) if f.endswith("settle_market_results_parallel")
    ]
    if idx:
        i = idx[-1]
        n = line_in(st[i][1])
        if n is not None and PASS_B[0] <= n <= PASS_B[1]:
            return "settle pass B", i
        return "settle pass A (exec thread)", i
    for i, (f, loc) in enumerate(st):
        if f.endswith("execute_batch_phases"):
            n = line_in(loc)
            if n is None:
                continue
            for (a, b), name in EBP:
                if a <= n <= b:
                    return name, i
            return "exec other (execute_batch_phases)", i
    return "exec other", 0


def rec(d, elf, w, data=None):
    data = data or f"{d}/perf-{w}.data"
    info = win(d, w)
    scripts = split_script(d, w, data)
    cls = collections.defaultdict(lambda: collections.defaultdict(int))
    slf = collections.defaultdict(
        lambda: collections.defaultdict(lambda: collections.defaultdict(int))
    )
    inc = collections.defaultdict(
        lambda: collections.defaultdict(lambda: collections.defaultdict(int))
    )
    lines = collections.defaultdict(
        lambda: collections.defaultdict(lambda: collections.defaultdict(int))
    )
    for ev, sg in sorted(scripts.items()):
        for per, st in expand(elf, sg):
            if not st:
                continue
            c, i = classify(st)
            cls[c][ev] += per
            cls["exec (all)"][ev] += per
            if c not in (
                "settle pass B",
                "match (workers)",
                "match (exec thread)",
                "settle pass A (workers)",
            ):
                continue
            key = "match" if c.startswith("match") else c
            leaf = st[-1]
            slf[key][leaf[0]][ev] += per
            lines[key][f"{leaf[0]} @ {leaf[1]}"][ev] += per
            # inclusive over logical callees of the anchor; for pass B the first callee frame carries the
            # pass B body line it is called from
            seen = set()
            for f, loc in st[i + 1 :]:
                if f not in seen:
                    seen.add(f)
                    inc[key][f][ev] += per
            if c == "settle pass B":
                n = st[i][1]
                inc[key]["@ pass B line " + n.rsplit(":", 1)[-1]][ev] += per
    k = 1000.0 / info["fills"]
    norm = lambda dd: {ev: v * k for ev, v in dd.items()}  # noqa: E731
    out = {
        "cell": d,
        "window": w,
        **info,
        "groups_per_1k": {g: norm(v) for g, v in groups(d, w, data).items()},
        "class_per_1k": {c: norm(v) for c, v in cls.items()},
        "self_per_1k": {c: {f: norm(v) for f, v in m.items()} for c, m in slf.items()},
        "incl_per_1k": {c: {f: norm(v) for f, v in m.items()} for c, m in inc.items()},
        "lines_per_1k": {
            c: {f: norm(v) for f, v in m.items()} for c, m in lines.items()
        },
    }
    json.dump(out, open(f"{d}/p2s0y-{os.path.basename(data)}.json", "w"), indent=1)
    print(
        f"{d} {w}: fills {info['fills']:.0f} in {info['span_s']:.1f}s, events {sorted(scripts)}"
    )


def mean(xs):
    xs = [x for x in xs if x is not None]
    return sum(xs) / len(xs) if xs else None


def cmp_(w, c2, c3, N=25):
    J2 = [json.load(open(f"{c}/p2s0y-{w}.json")) for c in c2]
    J3 = [json.load(open(f"{c}/p2s0y-{w}.json")) for c in c3]
    evs = sorted({e for j in J2 + J3 for v in j["class_per_1k"].values() for e in v})
    evs = [e for e in ("cycles", "instructions") if e in evs] + [
        e for e in evs if e not in ("cycles", "instructions")
    ]
    print(
        f"### window {w}: events per 1k fills (mean of the cells; cycles / instructions in M, misses in k)"
    )
    for J, lab in ((J2, "p2"), (J3, "p3")):
        for j in J:
            print(
                f"  {lab} {os.path.basename(j['cell'])}: fills {j['fills']:.0f} {j['fills_per_s']:.0f}/s, utime {j['utime_s']:.1f}s, "
                "timers ms/1k "
                + " ".join(
                    f"{k.replace('_ms_per_1k', '')} {v:.3f}"
                    for k, v in j["timers"].items()
                    if v is not None
                )
            )

    def get(j, sec, *ks):
        x = j[sec]
        for kk in ks:
            x = x.get(kk, {}) if isinstance(x, dict) else {}
        return x

    def sc(e, v):
        return v / 1e6 if e in ("cycles", "instructions") else v / 1e3

    def table(title, sec, keys, sub=None):
        print(f"\n{title}")
        print(
            "  "
            + "".ljust(46)
            + "".join(f"{e[:22]:>24s}" for e in evs)
            + "     IPC p2 / p3"
        )
        for key in keys:
            row = "  " + key[:46].ljust(46)
            ipc = []
            for e in evs:
                a = mean(
                    [
                        get(j, sec, *(sub + [key] if sub else [key])).get(e, 0)
                        for j in J2
                    ]
                )
                b = mean(
                    [
                        get(j, sec, *(sub + [key] if sub else [key])).get(e, 0)
                        for j in J3
                    ]
                )
                r = f"{b / a:.3f}" if a else "  -  "
                row += f"{sc(e, a):8.3f}>{sc(e, b):8.3f} {r:>6s}"
            for J in (J2, J3):
                cy = mean(
                    [
                        get(j, sec, *(sub + [key] if sub else [key])).get("cycles", 0)
                        for j in J
                    ]
                )
                ins = mean(
                    [
                        get(j, sec, *(sub + [key] if sub else [key])).get(
                            "instructions", 0
                        )
                        for j in J
                    ]
                )
                ipc.append(ins / cy if cy else 0)
            print(row + f"   {ipc[0]:.2f} / {ipc[1]:.2f}")

    gk = [
        "process",
        "exec",
        "end_resident",
        "flush",
        "rpc",
        "tokio",
        "ingress",
        "rocksdb",
        "other",
    ]
    table(
        "thread groups (whole process, p2 > p3 ratio)",
        "groups_per_1k",
        [g for g in gk if g in J2[0]["groups_per_1k"]],
    )
    ck = sorted(
        {c for j in J2 + J3 for c in j["class_per_1k"]},
        key=lambda c: -J2[0]["class_per_1k"].get(c, {}).get("cycles", 0),
    )
    table("exec classes", "class_per_1k", ck)
    for key in ("settle pass B", "match"):
        for sec, what in (
            ("self_per_1k", "self (leaf logical function)"),
            ("incl_per_1k", "inclusive (logical callees of the anchor)"),
            ("lines_per_1k", "leaf line"),
        ):
            pool = collections.Counter()
            for j in J2 + J3:
                for f, v in get(j, sec, key).items():
                    pool[f] += 0  # make sure it exists

            # rank by |delta cycles| and by size
            def cyc(J, f):
                return mean([get(j, sec, key, f).get("cycles", 0) for j in J]) or 0

            fs = list(pool)
            top = sorted(fs, key=lambda f: -max(cyc(J2, f), cyc(J3, f)))[:N]
            mov = sorted(fs, key=lambda f: -abs(cyc(J3, f) - cyc(J2, f)))[:N]
            table(f"{key}: {what}, top {N} by size", sec, top, [key])
            table(f"{key}: {what}, top {N} movers (|p3 - p2| cycles)", sec, mov, [key])


def stat(c2, c3):
    def rd(c, w):
        r = {}
        for line in open(f"{c}/perf-stat-{w}.txt"):
            if line.startswith("#") or not line.strip():
                continue
            p = line.strip().split(",")
            try:
                r[p[2].replace(":u", "")] = (float(p[0]), p[4] if len(p) > 4 else "")
            except ValueError:
                r[p[2].replace(":u", "")] = (None, p[0])
        return r

    for w in [w for w in ("w1", "w2") if os.path.exists(f"{c2[0]}/perf-stat-{w}.txt")]:
        rows = {}
        for lab, cs in (("p2", c2), ("p3", c3)):
            for c in cs:
                s = rd(c, w)
                info = win(c, w)
                k = 1000.0 / info["fills"]
                d = {e: v[0] * k for e, v in s.items() if v[0] is not None}
                d["_pct"] = {e: v[1] for e, v in s.items()}
                d["_fills_per_s"] = info["fills_per_s"]
                d["_passb"] = info["timers"]["settle_pass_b_ms_per_1k"]
                d["_match"] = info["timers"]["phase_match_ms_per_1k"]
                d["_engine"] = info["timers"]["engine_ms_per_1k"]
                rows.setdefault(lab, []).append((os.path.basename(c), d))
        evs = [e for e in rows["p2"][0][1] if not e.startswith("_")]
        print(
            f"\n### perf stat {w} (val0 whole process, user space, per 1k fills; counter run % {rows['p2'][0][1]['_pct']})"
        )
        hdr = (
            "  cell".ljust(32)
            + "fills/s".rjust(10)
            + "".join(f"{e[:24]:>26s}" for e in evs)
            + "     IPC  passB  match engine (ms/1k, node timers)"
        )
        print(hdr)
        for lab in ("p2", "p3"):
            for name, d in rows[lab]:
                print(
                    f"  {name[-12:]:30s}{d['_fills_per_s']:10.0f}"
                    + "".join(f"{d[e]:26.1f}" for e in evs)
                    + f"  {d['instructions'] / d['cycles']:6.3f} {d['_passb']:6.3f} {d['_match']:6.3f} {d['_engine']:6.3f}"
                )
        m = {
            lab: {
                e: mean([d[e] for _, d in rows[lab]])
                for e in evs + ["_passb", "_match", "_engine"]
            }
            for lab in rows
        }
        sp = {
            lab: {
                e: (
                    abs(rows[lab][0][1][e] - rows[lab][1][1][e]) / m[lab][e]
                    if m[lab][e]
                    else 0
                )
                for e in evs
            }
            for lab in rows
        }
        print(
            "  mean p2".ljust(42)
            + "".join(f"{m['p2'][e]:26.1f}" for e in evs)
            + f"  {m['p2']['instructions'] / m['p2']['cycles']:6.3f}"
        )
        print(
            "  mean p3".ljust(42)
            + "".join(f"{m['p3'][e]:26.1f}" for e in evs)
            + f"  {m['p3']['instructions'] / m['p3']['cycles']:6.3f}"
        )
        print(
            "  p3/p2".ljust(42)
            + "".join(f"{m['p3'][e] / m['p2'][e]:26.3f}" for e in evs)
            + f"  {(m['p3']['instructions'] / m['p3']['cycles']) / (m['p2']['instructions'] / m['p2']['cycles']):6.3f}"
            + f" passB {m['p3']['_passb'] / m['p2']['_passb']:.3f} match {m['p3']['_match'] / m['p2']['_match']:.3f} engine {m['p3']['_engine'] / m['p2']['_engine']:.3f}"
        )
        print(
            "  r1/r2 spread p2".ljust(42)
            + "".join(f"{sp['p2'][e] * 100:25.1f}%" for e in evs)
        )
        print(
            "  r1/r2 spread p3".ljust(42)
            + "".join(f"{sp['p3'][e] * 100:25.1f}%" for e in evs)
        )
        per_ins = [e for e in evs if e not in ("cycles", "instructions")]
        print(
            "  per 1k instructions: "
            + "; ".join(
                f"{e} p2 {m['p2'][e] / m['p2']['instructions'] * 1000:.3f} p3 {m['p3'][e] / m['p3']['instructions'] * 1000:.3f}"
                for e in per_ins
            )
        )


def cells(ds):
    print(
        "| cell | node md5 | matched/s | fills/blk | engine ms/1k | settle | settle pass B | match | pass B ms/1k | match ms/1k | perf window: pass B / match / engine ms/1k |"
    )
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for d in ds:
        s = json.load(open(f"{d}/summary.json"))
        h = s["headline"]
        p = s["phase_by_node"]["val0"]
        e = p["phases"]["engine"]
        f = p["fills_per_native_block"]
        md5 = sorted(
            {
                m
                for m in re.findall(
                    r"exe_md5=(\w+)", open(f"{d}/node-environ-trie.txt").read()
                )
            }
        )
        wt = ""
        if os.path.exists(f"{d}/prof-w1-before.ts"):
            t = win(d, "w1")["timers"]
            wt = f"{t['settle_pass_b_ms_per_1k']:.3f} / {t['phase_match_ms_per_1k']:.3f} / {t['engine_ms_per_1k']:.2f}"
        print(
            f"| {os.path.basename(d).replace('ozarchy-p2s0y-300m-', '')} | {','.join(md5)} | {h['matched_s_avg']:,.0f} | {f:,.0f} | "
            f"{h['engine_ms_per_1k_fills']:.2f} | {e['phase_settle_ms']:.1f} | {e['settle_pass_b_ms']:.1f} | {e['phase_match_ms']:.1f} | "
            f"{e['settle_pass_b_ms'] / f * 1000:.3f} | {e['phase_match_ms'] / f * 1000:.3f} | {wt} |"
        )


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "cells":
        cells(sys.argv[2:])
    elif cmd == "rec":
        rec(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5] if len(sys.argv) > 5 else None)
    elif cmd == "cmp":
        cmp_(
            sys.argv[2],
            sys.argv[3].split(","),
            sys.argv[4].split(","),
            int(sys.argv[5]) if len(sys.argv) > 5 else 25,
        )
    elif cmd == "stat":
        stat(sys.argv[2].split(","), sys.argv[3].split(","))
