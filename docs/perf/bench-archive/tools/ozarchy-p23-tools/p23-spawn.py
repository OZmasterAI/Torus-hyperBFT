"""p23-spawn.py <cell_dir>...: thread spawns per native block on val0 (perf PERF_RECORD_FORK/EXIT in the 45 s window),
spawn bursts (scopes) per parent thread, user CPU in spawn/join code (cycles:u samples), exited-thread CPU (process minus live
threads from /proc), spawn sites (outermost torus frame of each child thread's samples)."""

import collections
import os
import re
import statistics as st
import subprocess
import sys

FORK = re.compile(
    r"^\s*(.*?)\s+(\d+)\s+([\d.]+):\s+PERF_RECORD_FORK\((\d+):(\d+)\):\((\d+):(\d+)\)"
)
EXIT = re.compile(r"^\s*(.*?)\s+(\d+)\s+([\d.]+):\s+PERF_RECORD_EXIT\((\d+):(\d+)\)")
SPAWN_LEAF = re.compile(
    r"pthread_create|create_thread|allocate_stack|clone3|__clone|start_thread|__nptl|pthread_join|clockjoin|timedjoin|"
    r"\bmunmap\b|\bmmap\b|mprotect|madvise|sigaltstack|_dl_allocate_tls|_dl_deallocate_tls|__libc_thread_freeres|call_tls_dtors|"
    r"Builder::spawn|spawn_unchecked|unix::thread::Thread::new|thread_start|stack_overflow|thread::scoped|ScopeData|JoinInner|"
    r"Thread::new|set_name|prctl|thread_local.*(dtor|destroy)|guard::|current::set_current|Thread::id|ThreadId::new"
)
TICK = 100.0


def metric(path, name):
    for line in open(path):
        if line.startswith(name + " ") or line.startswith(name + "{"):
            return float(line.split()[-1])
    return None


def tasks(path):
    d = {}
    for line in open(path):
        p = line.split()
        if len(p) >= 4:
            d[p[0]] = (p[1], int(p[2]), int(p[3]))
    return d


def stat(path):
    f = open(path).read()
    rest = f[f.rindex(")") + 2 :].split()
    return int(rest[11]), int(rest[12])  # utime, stime (fields 14, 15)


def outer_site(frames):
    # frames outer -> leaf; skip std/libc thread start glue, return the first frame from a torus crate
    for fr in frames:
        if "torus_" in fr or "hotstuff" in fr:
            return re.sub(r"::\{\{closure\}\}.*", "", fr)[:110]
    return frames[-1][:110] if frames else "?"


def run(cell):
    t0 = float(open(cell + "/prof-before.ts").read())
    t1 = float(open(cell + "/prof-after.ts").read())
    blk = metric(
        cell + "/prof-metrics-after.txt", "torus_exec_native_blocks_total"
    ) - metric(cell + "/prof-metrics-before.txt", "torus_exec_native_blocks_total")
    wall = t1 - t0
    ev = subprocess.run(
        [
            "perf",
            "script",
            "-i",
            cell + "/perf.data",
            "--show-task-events",
            "-F",
            "comm,tid,time",
        ],
        capture_output=True,
        text=True,
    ).stdout
    forks = []
    exits = {}
    pcomm = {}
    for line in ev.splitlines():
        m = FORK.match(line)
        if m:
            forks.append(
                (float(m.group(3)), int(m.group(7)), int(m.group(5)))
            )  # time, parent tid, child tid
            pcomm[int(m.group(7))] = m.group(1)
            continue
        m = EXIT.match(line)
        if m:
            exits[int(m.group(5))] = float(m.group(3))
    children = {c for _, _, c in forks}
    life = [exits[c] - t for t, _, c in forks if c in exits]
    byparent = collections.Counter(pcomm[p] for _, p, _ in forks)
    # bursts: forks by one parent tid with < 2 ms between consecutive forks = one scope
    bursts = collections.defaultdict(list)
    last = {}
    for t, p, c in sorted(forks):
        if p in last and t - last[p][0] < 0.002:
            last[p][1].append(c)
        else:
            last[p] = [t, [c]]
            bursts[p].append(last[p][1])
        last[p][0] = t
    scopes = {
        pcomm[p]: (len(b), collections.Counter(len(x) for x in b))
        for p, b in bursts.items()
    }
    # samples: user stacks (cycles:u), period weighted
    sc = subprocess.run(
        [
            "perf",
            "script",
            "-i",
            cell + "/perf.data",
            "-F",
            "comm,tid,period,ip,sym",
            "--no-inline",
        ],
        capture_output=True,
        text=True,
    ).stdout
    tot = 0
    spawn_user = collections.Counter()
    spawn_user_exec = 0
    exec_tot = 0
    child_tot = 0
    site = collections.Counter()
    site_tids = collections.defaultdict(set)
    comm = tid = per = None
    frames = []

    def flush():
        nonlocal tot, spawn_user_exec, exec_tot, child_tot
        if comm is None:
            return
        tot += per
        outer = list(reversed(frames))
        leaf = frames[0] if frames else "?"
        if comm.startswith("torus-execution") and tid not in children:
            exec_tot += per
        if SPAWN_LEAF.search(leaf):
            spawn_user[re.sub(r"<.*?>|\[.*?\]", "", leaf)[:60]] += per
            if comm.startswith("torus-execution") and tid not in children:
                spawn_user_exec += per
        if tid in children:
            child_tot += per
            s = outer_site(outer)
            site[s] += per
            site_tids[s].add(tid)

    for line in sc.splitlines():
        if not line.strip():
            flush()
            comm = None
            frames = []
            continue
        if line[0] != "\t":
            m = re.match(r"^(.*?)\s+(\d+)\s+(\d+)\s*$", line.strip())
            if m:
                comm, tid, per = m.group(1), int(m.group(2)), int(m.group(3))
            continue
        p = line.strip().split(" ", 1)
        if len(p) > 1 and not p[1].startswith("[unknown]"):
            frames.append(re.sub(r"\[[0-9a-f]{6,16}\]", "", p[1]))
    flush()
    # /proc: process utime/stime delta and live-thread deltas -> CPU of threads not alive at both snapshots
    u0, s0 = stat(cell + "/prof-stat-before.txt")
    u1, s1 = stat(cell + "/prof-stat-after.txt")
    a, b = tasks(cell + "/prof-tasks-before.txt"), tasks(cell + "/prof-tasks-after.txt")
    lu = sum(b[k][1] - a[k][1] for k in b if k in a)
    ls_ = sum(b[k][2] - a[k][2] for k in b if k in a)
    exu, exs = (u1 - u0 - lu) / TICK, (s1 - s0 - ls_) / TICK
    pu, ps = (u1 - u0) / TICK, (s1 - s0) / TICK
    execu = (
        sum(
            b[k][1] - a[k][1]
            for k in b
            if k in a and b[k][0].startswith("torus-execution")
        )
        / TICK
    )
    execs = (
        sum(
            b[k][2] - a[k][2]
            for k in b
            if k in a and b[k][0].startswith("torus-execution")
        )
        / TICK
    )
    n = len(forks)
    print(
        f"== {os.path.basename(cell)}: window {wall:.1f} s, native blocks {blk:.0f} ({blk / wall:.2f}/s)"
    )
    print(
        f"  forks {n} = {n / blk:.1f} per native block ({n / wall:.0f}/s); exits seen {len(exits)}; child lifetime ms median {1e3 * st.median(life):.3f} p90 {1e3 * sorted(life)[int(0.9 * len(life))]:.3f}"
    )
    print("  forks by parent comm:", dict(byparent.most_common(8)))
    for k, (nb, dist) in sorted(scopes.items(), key=lambda x: -x[1][0])[:6]:
        print(
            f"  parent {k}: {nb} bursts = {nb / blk:.2f}/blk; threads per burst {dict(sorted(dist.items()))}"
        )
    print(
        f"  process CPU in window: user {pu:.2f} s sys {ps:.2f} s; threads not alive at both snapshots: user {exu:.2f} s sys {exs:.2f} s "
        f"(= {1e6 * exu / n:.1f} us user, {1e6 * exs / n:.1f} us sys per spawned thread)"
    )
    print(
        f"  exec thread(s) live: user {execu:.2f} s sys {execs:.2f} s = {1e3 * (execu + execs) / blk:.2f} ms CPU/blk"
    )
    print(
        f"  user samples (period-weighted): spawn/join-glue leaf share {100 * sum(spawn_user.values()) / tot:.3f}% of all user cycles; "
        f"exec thread spawn-glue {100 * spawn_user_exec / max(exec_tot, 1):.3f}% of exec-thread cycles; spawned children {100 * child_tot / tot:.2f}% of all"
    )
    print(
        "  top spawn-glue leaves:",
        [(k, round(100 * v / tot, 3)) for k, v in spawn_user.most_common(8)],
    )
    print(
        "  spawn sites (children's outermost torus frame: % of child cycles, children seen):"
    )
    for k, v in site.most_common(10):
        print(f"     {100 * v / max(child_tot, 1):5.1f}%  {len(site_tids[k]):5d}  {k}")
    return dict(
        blk=blk,
        wall=wall,
        forks=n,
        exu=exu,
        exs=exs,
        pu=pu,
        ps=ps,
        glue=sum(spawn_user.values()) / tot,
        glue_exec=spawn_user_exec / max(exec_tot, 1),
        bursts={k: v[0] for k, v in scopes.items()},
    )


for c in sys.argv[1:]:
    run(c)
