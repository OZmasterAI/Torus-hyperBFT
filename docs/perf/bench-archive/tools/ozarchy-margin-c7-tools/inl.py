#!/usr/bin/env python3
"""inl.py: inline-expanded exec-thread samples (shared loader for the margin-phase tools).

load(cell, elf, script_gz) -> (K, samples)
  K(period) -> ms CPU per 1k fills (buckets2.py method: share of process user period x val0 utime / fills).
  samples: list of (period, logical_stack) for comm torus-execution*, logical_stack root -> leaf of
  (func, file:line). Binary frames are expanded with `llvm-addr2line -i -f -C` (leaf ip; return
  address - 1 for callers); each physical frame gives its inline chain outermost first. Non-binary
  frames (libc, kernel) keep the perf symbol with loc '?'.
The addr2line results are cached next to the script as <script>.a2l.json.
"""

import sys, re, gzip, json, os, subprocess, io, contextlib, importlib.util

B2 = "/home/oz/bench-results-matched/ozarchy-14236fa-tools/buckets2.py"
HASH = re.compile(r"\[[0-9a-f]{6,16}\]")


def _k(cell):
    spec = importlib.util.spec_from_file_location("b2", B2)
    argv = sys.argv
    sys.argv = ["x", cell]
    with contextlib.redirect_stdout(io.StringIO()):
        b = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(b)
    sys.argv = argv
    tot = sum(int(l.rsplit(" ", 1)[1]) for l in open(cell + "/perf.folded"))
    return (lambda v: b.K(v / tot)), b


def _text_range(elf):
    for l in subprocess.run(
        ["readelf", "-SW", elf], capture_output=True, text=True
    ).stdout.splitlines():
        p = l.split()
        if ".text" in p:
            i = p.index(".text")
            lo = int(p[i + 2], 16)
            size = int(p[i + 4], 16)
            return lo, lo + size
    raise SystemExit("no .text")


def _anchor(elf):
    for l in subprocess.run(
        ["nm", "-C", elf], capture_output=True, text=True
    ).stdout.splitlines():
        if l.endswith(" torus_consensus::app::execution_loop"):
            return int(l.split()[0], 16)
    raise SystemExit("execution_loop not in nm")


def _raw(script_gz):
    per = 0
    fr = []
    for line in gzip.open(script_gz, "rt"):
        if not line.strip():
            if fr:
                yield per, fr
            fr = []
            continue
        if line[0] not in " \t":
            per = int(line.split()[-1])
            fr = []
            continue
        p = line.strip().split(" ", 1)
        ip = int(p[0], 16)
        sym = p[1] if len(p) > 1 else "[unknown]"
        off = 0
        m = re.match(r"^(.*)\+0x([0-9a-f]+)$", sym)
        if m:
            sym, off = m.group(1), int(m.group(2), 16)
        fr.append((ip, HASH.sub("", sym), off))  # leaf first
    if fr:
        yield per, fr


def short(f):
    f = re.sub(r"<torus_state::backend::NativeStateOverlay>", "", f)
    f = re.sub(r"::\{closure#\d+\}", "{cl}", f)
    f = (
        f.replace("torus_bridge::native_executor::", "")
        .replace("torus_core::", "")
        .replace("torus_types::", "")
    )
    f = f.replace("alloy_primitives::bits::address::", "")
    f = re.sub(r" \(\.llvm\.\d+\)", "", f)
    # drop trailing generic args: `name<...>` (inlined frames) and `<T>::name::<...>` (symbols)
    if not f.startswith("<"):
        f = f.split("<", 1)[0] or f
    f = re.sub(r"::<.*$", "", f)
    f = re.sub(r"::$", "", f)
    return f[:140]


def load(cell, elf, script_gz):
    K, b2 = _k(cell)
    lo, hi = _text_range(elf)
    ev = _anchor(elf)
    raw = list(_raw(script_gz))
    base = None
    for _, fr in raw:
        for ip, s, off in fr:
            if s == "torus_consensus::app::execution_loop":
                base = ip - off - ev
                break
        if base is not None:
            break
    assert base is not None
    want = set()
    for _, fr in raw:
        for i, (ip, s, off) in enumerate(fr):
            a = ip - base - (1 if i > 0 else 0)
            if lo <= a < hi:
                want.add(a)
    cache_f = os.path.join(os.path.dirname(os.path.abspath(__file__)), os.path.basename(script_gz) + ".a2l.json")
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
        cur = None
        k = 0
        while k < len(out):
            l = out[k]
            if l.startswith("0x"):
                cur = int(l, 16)
                chain[cur] = []
                k += 1
                continue
            fn = l
            loc = out[k + 1] if k + 1 < len(out) else "?"
            k += 2
            loc = re.sub(
                r"^.*?/(crates|library|\.cargo/registry/src/[^/]+)/", r"\1/", loc
            )
            loc = re.sub(r" \(discriminator \d+\)", "", loc)
            loc = re.sub(r":(\d+):\d+$", r":\1", loc)
            chain[cur].append((short(HASH.sub("", fn)), loc))
        json.dump({str(k): v for k, v in chain.items()}, open(cache_f, "w"))
    samples = []
    for per, fr in raw:
        st = []
        for i in range(len(fr) - 1, -1, -1):  # root -> leaf
            ip, s, off = fr[i]
            a = ip - base - (1 if i > 0 else 0)
            if a in chain:
                st.extend(tuple(x) for x in reversed(chain[a]))  # outermost first
            elif not s.startswith("[unknown]"):
                st.append((short(s), "?"))
        samples.append((per, st))
    return K, b2, samples


# execute_batch_phases phase timers (ENGINE line), by line of execute_batch_phases' body
PHASES = {
    "crab": [
        ((0, 5100), "phase 1 / pre"),
        ((5101, 5302), "margin"),
        ((5303, 5431), "match"),
        ((5432, 99999), "settle"),
    ],
    "main": [
        ((0, 3936), "phase 1 / pre"),
        ((3937, 4216), "margin"),
        ((4217, 4319), "match"),
        ((4320, 99999), "settle"),
    ],
}


def ebp_line(st):
    """Line of execute_batch_phases' body on the stack (outermost entry), or None."""
    for f, loc in st:
        if f.endswith("execute_batch_phases") or re.search(
            r"execute_batch_phases(<|$)", f
        ):
            m = re.search(r"native_executor\.rs:(\d+)", loc)
            if m:
                return int(m.group(1))
    return None


def phase_of(which, st):
    n = ebp_line(st)
    if n is None:
        return "outside execute_batch_phases"
    return next(name for (a, b), name in PHASES[which] if a <= n <= b)
