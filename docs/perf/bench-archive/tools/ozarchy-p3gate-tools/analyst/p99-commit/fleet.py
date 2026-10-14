"""Fleet-level commit-interval decomposition (python3 -I). Usage: fleet.py CELL_DIR T0 T1 OUT.csv
Commit interval = on_commit_block fired(h-1) -> fired(h) per validator (a-cells.py definition).
HotStuff (consensus) thread busy segments per node, from the INFO log:
  persist : fired(h-1) -> 'sending to execution pipeline' h   (materialize + durable header/body write)
  blocked : 'sending' h -> exec start of h-64                   (blocking send on the 64-deep exec channel)
  produce : 'produce_block called' -> 'pre-proposal push sent'   (leader block build + push)
Quorum is 3 of 3 (power 1e6 each, f=999999), so a commit waits for the slowest node:
for each interval on node v, busy time of every node u inside the window is summed per kind;
the critical node is the one with the most busy time; residual = window - its busy (network/consensus).
For the blocked part, what the exec thread of the critical node was doing is recorded."""
import bisect, csv, datetime as dt, gzip, re, sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z")
def ep(s): return dt.datetime.fromisoformat(s).replace(tzinfo=dt.timezone.utc).timestamp()
R = [
 ('fire', re.compile(r"on_commit_block fired height=(\d+)")),
 ('send', re.compile(r"sending to execution pipeline height=(\d+) evm_txs=(\d+) native=(\d+)")),
 ('xs', re.compile(r"executing finalized block height=(\d+) has_evm=(\w+) has_native=(\w+) pipelined=(\w+).*native_count=(\d+)")),
 ('xd', re.compile(r"execution pipeline: block done height=(\d+)")),
 ('prod', re.compile(r"produce_block called \(CTE\) parent_height=(\d+)")),
 ('push', re.compile(r"pre-proposal push sent count=(\d+) height=(\d+)")),
 ('sel', re.compile(r"selected actions for block evm_txs=(\d+) native_actions=(\d+)")),
 ('rskip', re.compile(r"round-skip: entered view (\d+) from (\d+) on (\w+)")),
 ('epoch', re.compile(r"validator inflation rewards distributed")),
 ('msg', re.compile(r"on_receive_msg: type=(\w+), view=(\d+)")),
]
V = ('val0', 'val1', 'val2')
DEPTH = 64

def parse(path):
    n = dict(fire={}, send={}, xs={}, xd={}, prod=[], push=[], rskip=[], epoch=[], views=[])
    with gzip.open(path, 'rt', errors='replace') as fh:
        for line in fh:
            if 'INFO' not in line and 'WARN' not in line: continue
            line = ANSI.sub('', line)
            m = TS.match(line)
            if not m: continue
            for k, rx in R:
                g = rx.search(line)
                if not g: continue
                t = ep(m.group(1))
                if k == 'fire': n['fire'].setdefault(int(g[1]), t)
                elif k == 'send': n['send'].setdefault(int(g[1]), (t, int(g[3])))
                elif k == 'xs': n['xs'].setdefault(int(g[1]), (t, g[4] == 'true', int(g[5])))
                elif k == 'xd': n['xd'].setdefault(int(g[1]), t)
                elif k == 'prod': n['prod'].append(t)
                elif k == 'push': n['push'].append(t)
                elif k == 'rskip': n['rskip'].append(t)
                elif k == 'epoch': n['epoch'].append(t)
                elif k == 'msg': n['views'].append((t, int(g[2])))
                break
    return n

def segments(n):
    """busy segments on the consensus thread: list of (start, end, kind, info)."""
    seg = []
    fire = n['fire']; xs = n['xs']; xd = n['xd']
    for k, (ts, nat) in n['send'].items():
        tf = fire.get(k - 1)
        if tf is not None and tf <= ts and ts - tf < 5:
            seg.append((tf, ts, 'persist', dict(h=k, nat=nat)))
        x = xs.get(k - DEPTH)
        if x and x[0] > ts:
            # what exec ran while the send was blocked: the block before k-64 (it held the slot)
            hb = k - DEPTH - 1; xb = xs.get(hb); db = xd.get(hb)
            seg.append((ts, x[0], 'blocked', dict(h=k, ex_h=hb, ex_nat=xb[2] if xb else -1,
                       ex_pip=int(xb[1]) if xb else -1, ex_ms=(db - xb[0]) * 1000 if (xb and db) else float('nan'),
                       idle_ms=(x[0] - db) * 1000 if db else float('nan'))))
    pu = sorted(n['push'])
    for tp in n['prod']:
        j = bisect.bisect_left(pu, tp)
        if j < len(pu) and pu[j] - tp < 0.5:
            seg.append((tp, pu[j], 'produce', {}))  # only leader turns that pushed a body (native blocks)
    seg.sort()
    return seg

def overlap(seg, lo, hi):
    out = {}
    for s, e, k, info in seg:
        if e <= lo or s >= hi: continue
        d = min(e, hi) - max(s, lo)
        out.setdefault(k, [0.0, []])
        out[k][0] += d; out[k][1].append((d, info))
    return out

def main(cell, t0, t1, out):
    N = {v: parse(f"{cell}/{v}.log.gz") for v in V}
    S = {v: segments(N[v]) for v in V}
    starts = {v: [s[0] for s in S[v]] for v in V}
    rows = []
    for v in V:
        fire = N[v]['fire']; rs = sorted(N[v]['rskip']); epo = sorted(N[v]['epoch'])
        vt = [t for t, _ in N[v]['views']]; vv = [x for _, x in N[v]['views']]
        def view_at(t):
            i = bisect.bisect_right(vt, t) - 1
            return vv[i] if i >= 0 else 0
        hs = sorted(h for h, t in fire.items() if t0 <= t <= t1)
        for a, b in zip(hs, hs[1:]):
            if b != a + 1: continue
            lo, hi = fire[a], fire[b]
            ci = (hi - lo) * 1000
            per = {}
            for u in V:
                # only segments that could overlap: start < hi
                j = bisect.bisect_left(starts[u], hi)
                cand = [s for s in S[u][max(0, j - 400):j]]
                per[u] = overlap(cand, lo, hi)
            busy = {u: sum(x[0] for x in per[u].values()) for u in V}
            # union over nodes of blocked-on-send time inside the window (exec back-pressure anywhere in the fleet)
            ivs = sorted((max(s, lo), min(e, hi)) for u in V for s, e, k, _ in S[u][max(0, bisect.bisect_left(starts[u], hi) - 400):bisect.bisect_left(starts[u], hi)] if k == 'blocked' and e > lo and s < hi)
            ub, ce = 0.0, lo
            for s_, e_ in ivs:
                if e_ > ce:
                    ub += e_ - max(s_, ce); ce = e_
            crit = max(V, key=lambda u: busy[u])
            cb = per[crit]
            kinds = {k: cb.get(k, [0.0])[0] * 1000 for k in ('persist', 'blocked', 'produce')}
            # blocked detail on the critical node: the largest blocked piece
            bl = max(cb.get('blocked', [0, []])[1], default=None, key=lambda x: x[0])
            own = per[v]
            nat_send = N[v]['send'].get(b + 0, (0, -1))[1]
            rows.append(dict(cell=cell.rsplit('-300m-', 1)[-1], val=v, h=b, t=round(hi, 3), ci_ms=round(ci, 2),
                crit=crit, crit_busy_ms=round(busy[crit] * 1000, 2),
                c_persist=round(kinds['persist'], 2), c_blocked=round(kinds['blocked'], 2), c_produce=round(kinds['produce'], 2),
                resid_ms=round(ci - busy[crit] * 1000, 2), union_blocked_ms=round(ub * 1000, 2),
                own_persist=round(own.get('persist', [0])[0] * 1000, 2), own_blocked=round(own.get('blocked', [0])[0] * 1000, 2),
                own_produce=round(own.get('produce', [0])[0] * 1000, 2),
                max_persist_any=round(max(per[u].get('persist', [0])[0] for u in V) * 1000, 2),
                bl_ex_h=bl[1]['ex_h'] if bl else -1, bl_ex_nat=bl[1]['ex_nat'] if bl else -1,
                bl_ex_pip=bl[1]['ex_pip'] if bl else -1, bl_ex_ms=round(bl[1]['ex_ms'], 2) if bl else '',
                bl_idle_ms=round(bl[1]['idle_ms'], 2) if bl else '',
                views=view_at(hi) - view_at(lo), rskip=bisect.bisect_right(rs, hi) - bisect.bisect_left(rs, lo),
                epoch_near=int(any(lo - 1 <= e <= hi for e in epo)), send_nat=nat_send))
    with open(out, 'w', newline='') as fh:
        w = csv.DictWriter(fh, fieldnames=list(rows[0])); w.writeheader(); w.writerows(rows)

if __name__ == '__main__':
    main(sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4])
