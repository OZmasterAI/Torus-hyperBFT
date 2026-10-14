"""Per-validator commit-interval timeline (python3 -I). Usage: tl.py CELL_DIR VAL T0 T1 OUT.csv
One row per commit interval fire(h-1)->fire(h) inside [T0,T1] (on_commit_block fired, the a-cells.py definition).
Splits each interval on the HotStuff thread: persist (fire -> 'sending' log, durable body write),
send_block (sending log -> next HotStuff-thread log line: blocked exec_tx.send on the 64-deep channel),
cons (rest: consensus rounds until the next commit). Adds what exec was doing and consensus events."""
import bisect, csv, datetime as dt, gzip, re, sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z")
def ep(s): return dt.datetime.fromisoformat(s).replace(tzinfo=dt.timezone.utc).timestamp()
R = {
 'fire': re.compile(r"on_commit_block fired height=(\d+)"),
 'send': re.compile(r"sending to execution pipeline height=(\d+) evm_txs=(\d+) native=(\d+)"),
 'xs': re.compile(r"executing finalized block height=(\d+) has_evm=(\w+) has_native=(\w+) pipelined=(\w+).*native_count=(\d+)"),
 'xd': re.compile(r"execution pipeline: block done height=(\d+)"),
 'prod': re.compile(r"produce_block called \(CTE\) parent_height=(\d+)"),
 'val': re.compile(r"validate_block: (?:TorusBlock deserialized|CompactBlock fallback) height=(\d+)"),
 'msg': re.compile(r"on_receive_msg: type=(\w+), view=(\d+)"),
 'rskip': re.compile(r"round-skip: entered view (\d+) from (\d+) on (\w+)"),
 'lskip': re.compile(r"leader-skip:"),
 'epoch': re.compile(r"validator inflation rewards distributed"),
 'sel': re.compile(r"selected actions for block evm_txs=(\d+) native_actions=(\d+)"),
}
HS = ('fire', 'prod', 'val', 'msg', 'rskip', 'lskip', 'sel')  # lines from the HotStuff/app thread

def parse(path):
    ev = []
    with gzip.open(path, 'rt', errors='replace') as fh:
        for line in fh:
            line = ANSI.sub('', line)
            m = TS.match(line)
            if not m: continue
            for k, rx in R.items():
                g = rx.search(line)
                if g:
                    ev.append((ep(m.group(1)), k, g.groups())); break
    return ev

def main(cell, v, t0, t1, out):
    ev = parse(f"{cell}/{v}.log.gz")
    fire = {}; send = {}; xs = {}; xd = {}; hs_t = []; xs_t = []; xs_h = []
    rs = []; ls = []; prod = []; epoch = []; views = []; val = []
    for t, k, g in ev:
        if k == 'fire': fire.setdefault(int(g[0]), t)
        elif k == 'send': send.setdefault(int(g[0]), (t, int(g[2]), int(g[1])))
        elif k == 'xs':
            h = int(g[0])
            if h not in xs:
                xs[h] = (t, g[3] == 'true', int(g[4])); xs_t.append(t); xs_h.append(h)
        elif k == 'xd': xd.setdefault(int(g[0]), t)
        elif k == 'rskip': rs.append(t)
        elif k == 'lskip': ls.append(t)
        elif k == 'prod': prod.append(t)
        elif k == 'epoch': epoch.append(t)
        elif k == 'val': val.append(t)
        if k == 'msg': views.append((t, int(g[1])))
        if k in HS: hs_t.append(t)
    hs_t.sort()
    vt = [t for t, _ in views]; vv = [x for _, x in views]
    def cnt(a, lo, hi): return bisect.bisect_right(a, hi) - bisect.bisect_left(a, lo)
    def view_at(t):
        i = bisect.bisect_right(vt, t) - 1
        return vv[i] if i >= 0 else 0
    hs_sorted = sorted(h for h, t in fire.items() if t0 <= t <= t1)
    rows = []
    for a, b in zip(hs_sorted, hs_sorted[1:]):
        if b != a + 1: continue
        ta, tb = fire[a], fire[b]
        k = a + 1  # the 'sending' height logged right after fire(a)
        st = send.get(k)
        if st and ta <= st[0] <= tb:
            ts_, nat, evm = st
            j = bisect.bisect_right(hs_t, ts_)
            tres = hs_t[j] if j < len(hs_t) else tb
            tres = min(tres, tb)
        else:
            ts_, nat, evm, tres = ta, -1, -1, ta
        persist = ts_ - ta; blocked = tres - ts_; cons = tb - tres
        # exec block that started right at/just before the resume (it freed the slot), and the one it followed
        i = bisect.bisect_right(xs_t, tres + 0.0005) - 1
        xh = xs_h[i] if i >= 0 else -1
        xstart_gap = (tres - xs_t[i]) if i >= 0 else -1  # resume - last exec start (small => send unblocked by that start)
        prev = xh - 1
        pd = xd.get(prev); px = xs.get(prev)
        prev_ms = (pd - px[0]) * 1000 if pd and px else float('nan')
        prev_nat = px[2] if px else -1
        prev_pip = int(px[1]) if px else -1
        idle_ms = (xs_t[i] - pd) * 1000 if (i >= 0 and pd) else float('nan')  # exec thread gap: done(prev) -> start(next)
        # exec blocks started inside the blocked window, and their summed wall
        lo = bisect.bisect_left(xs_t, ts_); hi = bisect.bisect_right(xs_t, tres + 0.0005)
        depth = k - (xs_h[lo - 1] if lo > 0 else 0)  # sent-but-not-started at send time (incl. this one)
        rows.append(dict(h=b, t=round(tb, 6), ci_ms=round((tb - ta) * 1000, 3), persist_ms=round(persist * 1000, 3),
            blocked_ms=round(blocked * 1000, 3), cons_ms=round(cons * 1000, 3), send_native=nat, send_evm=evm, depth_at_send=depth,
            unblock_xh=xh, unblock_gap_ms=round(xstart_gap * 1000, 3), prev_exec_ms=round(prev_ms, 3), prev_native=prev_nat,
            prev_pipelined=prev_pip, exec_idle_ms=round(idle_ms, 3), xstarts_in_block=hi - lo,
            views=view_at(tb) - view_at(ta), rskip=cnt(rs, ta, tb), lskip=cnt(ls, ta, tb), produced=cnt(prod, ta, tb),
            validates=cnt(val, ta, tb), epoch=cnt(epoch, ta - 0.0, tb)))
    with open(out, 'w', newline='') as fh:
        w = csv.DictWriter(fh, fieldnames=list(rows[0])); w.writeheader(); w.writerows(rows)

if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4]), sys.argv[5])
