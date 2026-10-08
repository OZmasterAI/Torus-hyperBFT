#!/usr/bin/env python3
"""positions.py <accounts.txt> <markets> <rpc>: torus_getPosition for each account x market; prints summary JSON."""
import json, sys, time, urllib.request
from concurrent.futures import ThreadPoolExecutor
accts = [l.strip() for l in open(sys.argv[1]) if l.strip()]
n, rpc = int(sys.argv[2]), sys.argv[3]
def call(a, mk):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "torus_getPosition", "params": [a, hex(mk)]}).encode()
    req = urllib.request.Request(rpc, body, {"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            d = json.load(r)
        if "error" in d: return (a, mk, "err", str(d["error"])[:120])
        res = d.get("result")
        return (a, mk, "pos" if res and float(res.get("size", "0") or 0) != 0 else "none", None)
    except Exception as e:
        return (a, mk, "err", str(e)[:120])
t0 = time.time()
with ThreadPoolExecutor(16) as ex:
    out = list(ex.map(lambda x: call(*x), [(a, m) for a in accts for m in range(1, n + 1)]))
per = {}
errs = [o for o in out if o[2] == "err"]
for a, mk, s, _ in out:
    per.setdefault(a, 0)
    if s == "pos": per[a] += 1
v = sorted(per.values())
print(json.dumps({"accounts": len(accts), "markets": n, "calls": len(out), "errors": len(errs), "first_error": errs[0][3] if errs else None,
  "avg_positions_per_account": round(sum(v) / len(v), 3) if v else None, "min": v[0] if v else None, "median": v[len(v)//2] if v else None,
  "max": v[-1] if v else None, "accounts_with_any": sum(1 for x in v if x), "hist": {str(k): v.count(k) for k in sorted(set(v))}, "secs": round(time.time() - t0, 1)}))
