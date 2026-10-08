#!/usr/bin/env python3
"""thin-snap.py <cell-dir> <genesis> <n_thin> [interval_s]: poll val0 (8645) torus_getBalances for the LIQ_THIN senders
('bulk-test 60' .. 'bulk-test 60+n-1') + eth_blockNumber every interval_s from the harness '] bench: ' line until the
harness stop ('stopping pid') or 5 consecutive RPC failures. Writes <cell>/thin-snap.jsonl (one line per snapshot)."""

import json, sys, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

cell, gen, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
iv = float(sys.argv[4]) if len(sys.argv) > 4 else 5.0
URL = "http://127.0.0.1:8645"
runlog = f"{cell}/run.log"


def rpc(method, params):
    req = urllib.request.Request(
        URL,
        json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).encode(),
        {"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r).get("result")


def seen(s):
    try:
        return s in open(runlog).read()
    except OSError:
        return False


t0 = time.time()
while not seen("] bench: "):
    if time.time() - t0 > 900:
        sys.exit("no bench line")
    time.sleep(1)
g = json.load(open(gen))
want = {f"bulk-test {60 + i}": i for i in range(n)}
addrs = [None] * n
for row in g["native_balances"]:
    i = want.get(row.get("note"))
    if i is not None:
        addrs[i] = row["address"]
if None in addrs:
    sys.exit(f"thin senders missing in genesis: {addrs.count(None)}")
fails = 0
with open(f"{cell}/thin-snap.jsonl", "a") as out, ThreadPoolExecutor(16) as ex:
    out.write(json.dumps({"addresses": addrs}) + "\n")
    while not seen("stopping pid ") and fails < 5:
        ts = time.time()
        try:
            h = int(rpc("eth_blockNumber", []), 16)
            rows = list(ex.map(lambda a: rpc("torus_getBalances", [a]), addrs))
            out.write(
                json.dumps(
                    {
                        "ts": round(ts, 3),
                        "height": h,
                        "te": round(time.time(), 3),
                        "rows": rows,
                    }
                )
                + "\n"
            )
            out.flush()
            fails = 0
        except Exception as e:  # noqa: BLE001 (record and retry)
            fails += 1
            out.write(json.dumps({"ts": round(ts, 3), "error": str(e)[:200]}) + "\n")
        time.sleep(max(0.0, iv - (time.time() - ts)))
