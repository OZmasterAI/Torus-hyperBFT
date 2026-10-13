"""Per kill: on each survivor, kill -> first 'peer disconnected' and first 'peer connected' of the killed node's peer id
(ghost connection = the new connection arrives before the old one is dropped). Usage: a-conn.py PREFIX TAG..."""
import gzip, json, os, re, sys
from datetime import datetime, timezone
R = "/home/oz/bench-results-matched/"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
def ts(l):
    return datetime.strptime(l[:26], "%Y-%m-%dT%H:%M:%S.%f").replace(tzinfo=timezone.utc).timestamp()
tot = {}
for tag in sys.argv[2:]:
    c = R + sys.argv[1] + tag
    ks = [json.load(open(os.path.join(c, f))) for f in sorted(os.listdir(c), key=lambda f: (len(f), f)) if re.fullmatch(r"crash-kill(-\d)?\.json", f)]
    kn = ks[0]["kill_node"]
    pid = None
    for l in gzip.open(os.path.join(c, kn + ".log.gz"), "rt", errors="replace"):
        m = re.search(r"local_peer_id=(\w+)", ANSI.sub("", l))
        if m: pid = m.group(1); break
    ev = {}
    for v in ("val0", "val1", "val2"):
        if v == kn: continue
        ev[v] = []
        for l in gzip.open(os.path.join(c, v + ".log.gz"), "rt", errors="replace"):
            if pid not in l or "peer " not in l: continue
            l = ANSI.sub("", l)
            if "peer connected" in l: ev[v].append((ts(l), "C", re.search(r"num_established=(\d+)", l).group(1)))
            elif "peer disconnected" in l: ev[v].append((ts(l), "D", ""))
    for i, k in enumerate(ks):
        t0 = k["kill_ts"]; t1 = ks[i + 1]["kill_ts"] if i + 1 < len(ks) else t0 + 1e9
        out = []
        for v, e in ev.items():
            d = next((t - t0 for t, x, n in e if x == "D" and t0 <= t < t1), None)
            cc = next(((t - t0, n) for t, x, n in e if x == "C" and t0 <= t < t1), None)
            ghost = cc is not None and (d is None or cc[0] < d)
            out.append("%s disc=%s conn=%s est=%s ghost=%s" % (v, "%.2f" % d if d is not None else None, "%.2f" % cc[0] if cc else None, cc[1] if cc else None, ghost))
            tot.setdefault(tag.split("-")[1], []).append(ghost)
        print(tag, "k%d" % (i + 1), " | ".join(out))
for a, g in tot.items():
    print("TOTAL", a, "survivor-kill pairs", len(g), "ghost", sum(g))
