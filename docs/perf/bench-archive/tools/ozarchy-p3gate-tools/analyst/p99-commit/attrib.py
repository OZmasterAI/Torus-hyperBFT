"""Tail attribution of commit intervals (python3 -I). Usage: attrib.py OUT_PREFIX LABEL:CELLDIR:ARM:REGIME ...
Reads fl/<LABEL>.csv (fleet.py) and <CELLDIR>/sampler.csv (1 Hz, per node).
Checkpoint write window per node: from the second the exec thread submits a checkpoint
(torus_exec_checkpoint_wait_seconds_count steps) to the second the flush worker's state write ends
(torus_exec_state_write_db_seconds_count steps, within 6 s): window (submit-1, end] in sampler seconds.
Control: the m cells get pseudo-windows = the s2 (or d3) cell's windows of the same regime at the same offsets from
the first interval, so the 'remove the window intervals' counterfactual is compared like for like.
Tags per interval (first match): ckpt (overlaps a checkpoint write window on any node), timeout
(a view timeout on any node in the interval's seconds, or a round/leader skip on this node),
epoch (epoch-reward line within the interval or 1 s before), execbp (fleet blocked-on-send union
>= 50% of the interval), other."""
import csv, json, math, sys, collections

def pct(xs, q):
    xs = sorted(xs); k = (len(xs) - 1) * q; f = math.floor(k); c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)

def windows(cell):
    rows = list(csv.DictReader(open(f'{cell}/sampler.csv')))
    by = collections.defaultdict(list)
    for x in rows: by[x['node']].append(x)
    W = []; TO = collections.Counter()
    for v, xs in by.items():
        prev = None; subs = []; ends = []
        for x in xs:
            t = int(x['ts'])
            g = lambda c: float(x.get(c) or 0)
            if prev:
                if g('torus_exec_checkpoint_wait_seconds_count') > prev[0]: subs.append(t)
                if g('torus_exec_state_write_db_seconds_count') > prev[1]: ends.append(t)
                if g('torus_consensus_timeout_total_total') > prev[2]: TO[t] += 1
            prev = (g('torus_exec_checkpoint_wait_seconds_count'), g('torus_exec_state_write_db_seconds_count'), g('torus_consensus_timeout_total_total'))
        for s in subs:
            e = min([e for e in ends if s <= e <= s + 6], default=s)
            W.append((s - 1, e, v))  # sampler second t covers (t-1, t]: write ran inside (s-1, e]
    return W, TO

def tag(x, W, TO):
    t1 = float(x['t']); t0 = t1 - float(x['ci_ms']) / 1000
    if any(a < t1 and b > t0 for a, b, _ in W): return 'ckpt'
    if any(TO.get(s) for s in range(int(t0), int(t1) + 2)) or int(x['rskip']) > 0: return 'timeout'
    if int(x['epoch_near']): return 'epoch'
    if float(x['union_blocked_ms']) >= 0.5 * float(x['ci_ms']): return 'execbp'
    return 'other'

def main():
    out = sys.argv[1]; specs = [s.split(':') for s in sys.argv[2:]]
    res = {}
    for lab, cell, arm, reg in specs:
        W, TO = windows(cell)
        rows = list(csv.DictReader(open(f'fl/{lab}.csv')))
        per = collections.defaultdict(list)
        for x in rows: per[x['val']].append(x)
        span = {v: float(xs[-1]['t']) - float(xs[0]['t']) + float(xs[0]['ci_ms']) / 1000 for v, xs in per.items()}
        # time covered by checkpoint windows (union over nodes) inside the cell's interval span
        lo = min(float(x['t']) for x in rows); hi = max(float(x['t']) for x in rows)
        iv = sorted((max(a, lo), min(b, hi)) for a, b, _ in W if b > lo and a < hi)
        cov, ce = 0.0, lo
        for a, b in iv:
            if b > ce: cov += b - max(a, ce); ce = b
        res[lab] = dict(arm=arm, reg=reg, per=per, span=span, W=W, TO=TO, ckcov=cov / (hi - lo), nck=len(W) / 3)
        for v in per:
            for x in per[v]: x['tag'] = tag(x, W, TO)
    json.dump({k: dict(arm=r['arm'], reg=r['reg'], ckcov=r['ckcov'], nck=r['nck'], ckwin=r['W']) for k, r in res.items()}, open(out + '-windows.json', 'w'), indent=0)
    with open(out + '-tagged.csv', 'w', newline='') as fh:
        w = None
        for lab, r in res.items():
            for v, xs in r['per'].items():
                for x in xs:
                    if w is None: w = csv.DictWriter(fh, fieldnames=['label'] + list(x)); w.writeheader()
                    w.writerow(dict(label=lab, **x))
    # pseudo-windows for baseline cells: shift the matching Phase-3 cell's windows to the m cell's start
    def first_t(r): return min(float(x['t']) - float(x['ci_ms']) / 1000 for xs in r['per'].values() for x in xs)
    for lab, r in res.items():
        if r['arm'] != 'm': continue
        src = [l for l in res if res[l]['reg'] == r['reg'] and res[l]['arm'] == 's2' and l.split('-')[0] == lab.split('-')[0]]
        if not src: continue
        sr = res[src[0]]; off = first_t(r) - first_t(sr)
        r['PW'] = [(a + off, b + off, v) for a, b, v in sr['W']]
    def inwin(x, W):
        t1 = float(x['t']); t0 = t1 - float(x['ci_ms']) / 1000
        return any(a < t1 and b > t0 for a, b, _ in W)
    def cover(r, W):
        lo = first_t(r); hi = max(float(x['t']) for xs in r['per'].values() for x in xs)
        iv = sorted((max(a, lo), min(b, hi)) for a, b, _ in W if b > lo and a < hi); cov, ce = 0.0, lo
        for a, b in iv:
            if b > ce: cov += b - max(a, ce); ce = b
        return cov, hi - lo
    tags = ['ckpt', 'timeout', 'epoch', 'execbp', 'other']
    L = []
    P = lambda *a: L.append(' '.join(str(x) for x in a))
    for reg in ('SAT', 'soak'):
        labs = [l for l in res if res[l]['reg'] == reg]
        mlabs = [l for l in labs if res[l]['arm'] == 'm']
        if not mlabs: continue
        thr = sum(pct([float(x['ci_ms']) for x in res[l]['per'][v]], .99) for l in mlabs for v in res[l]['per']) / (3 * len(mlabs))
        P(f'== {reg}: threshold T = baseline (m) mean per-validator p99 = {thr:.0f} ms')
        P(f"{'cell':7} {'arm':3} {'p99':>5} {'p99 no-ckpt':>11} {'ckpt time%':>10} {'ckpts':>5} | intervals > T per validator per 100 s: total " + ' '.join(f'{t:>7}' for t in tags) + ' | share of time >T in ckpt windows')
        agg = collections.defaultdict(lambda: collections.defaultdict(list))
        for l in labs:
            r = res[l]; p99 = []; p99n = []; cnt = collections.Counter(); tot = 0; ckt = 0.0; allt = 0.0
            for v, xs in r['per'].items():
                ci = [float(x['ci_ms']) for x in xs]
                p99.append(pct(ci, .99)); p99n.append(pct([float(x['ci_ms']) for x in xs if x['tag'] != 'ckpt'], .99))
                for x in xs:
                    c = float(x['ci_ms'])
                    if c > thr:
                        cnt[x['tag']] += 100 / r['span'][v] / 3; tot += 100 / r['span'][v] / 3
                        allt += c - thr; ckt += (c - thr) if x['tag'] == 'ckpt' else 0
            P(f"{l:7} {r['arm']:3} {sum(p99)/3:5.0f} {sum(p99n)/3:11.0f} {100*r['ckcov']:9.1f}% {r['nck']:5.1f} | {'':39}{tot:5.2f} " + ' '.join(f'{cnt[t]:7.2f}' for t in tags) + f" | {100*ckt/allt if allt else 0:5.1f}%")
            agg[r['arm']]['p99'].append(sum(p99) / 3); agg[r['arm']]['p99n'].append(sum(p99n) / 3); agg[r['arm']]['tot'].append(tot)
            for t in tags: agg[r['arm']][t].append(cnt[t])
        P('per arm mean (sd over cells where n>1):')
        for arm, a in agg.items():
            sd = lambda xs: (sum((y - sum(xs)/len(xs))**2 for y in xs) / (len(xs) - 1)) ** .5 if len(xs) > 1 else float('nan')
            P(f"  {arm:3} n={len(a['p99'])} p99 {sum(a['p99'])/len(a['p99']):5.0f} (sd {sd(a['p99']):4.0f})  p99 excl. ckpt-window intervals {sum(a['p99n'])/len(a['p99n']):5.0f} (sd {sd(a['p99n']):4.0f})  >T/100s {sum(a['tot'])/len(a['tot']):5.2f} (sd {sd(a['tot']):4.2f}) " + ' '.join(f"{t} {sum(a[t])/len(a[t]):5.2f}" for t in tags))
    for reg in ('SAT', 'soak'):
        labs = [l for l in res if res[l]['reg'] == reg]
        mlabs = [l for l in labs if res[l]['arm'] == 'm']
        if not mlabs: continue
        thr = sum(pct([float(x['ci_ms']) for x in res[l]['per'][v]], .99) for l in mlabs for v in res[l]['per']) / (3 * len(mlabs))
        P(f'== {reg}: rate of intervals > T={thr:.0f} ms inside vs outside checkpoint write windows (per validator per 100 s of window / non-window time);')
        P('   m cells use the matching s2 cell\'s windows (pseudo) as the control for chance coincidence')
        P(f"{'cell':7} {'arm':3} {'win time%':>9} {'rate in':>8} {'rate out':>8} {'in/out':>6} {'p99 all':>7} {'p99 excl win':>12}")
        for l in labs:
            r = res[l]; W = r['W'] if r['arm'] != 'm' else r.get('PW', [])
            if not W: continue
            cov, span = cover(r, W)
            nin = sum(1 for xs in r['per'].values() for x in xs if float(x['ci_ms']) > thr and inwin(x, W)) / 3
            nout = sum(1 for xs in r['per'].values() for x in xs if float(x['ci_ms']) > thr and not inwin(x, W)) / 3
            rin = 100 * nin / cov if cov else float('nan'); rout = 100 * nout / (span - cov)
            p99a = sum(pct([float(x['ci_ms']) for x in xs], .99) for xs in r['per'].values()) / 3
            p99e = sum(pct([float(x['ci_ms']) for x in xs if not inwin(x, W)], .99) for xs in r['per'].values()) / 3
            P(f"{l:7} {r['arm']:3} {100*cov/span:8.1f}% {rin:8.2f} {rout:8.2f} {rin/rout if rout else float('inf'):6.1f} {p99a:7.0f} {p99e:12.0f}")
    open(out + '.txt', 'w').write('\n'.join(L) + '\n'); print('\n'.join(L))

main()
