#!/usr/bin/env python3
"""EVM load (run-cell.sh EVM_LOAD_RATE, item 7): the chain-side record of the
EVM transfers tools/tx-flood sent during a cell.

  evm_load.py scan --rpc URL --from H0 --to H1 [--interval N] [--stats FILE]
                   [--wait-s S] --out FILE
      Waits up to S seconds (default 60) until eth_blockNumber >= H1: a
      validator's eth view lags to its last checkpoint (every 15 s or 100th
      block, item 6 Phase 3). Then reads the number of EXECUTED EVM txs of
      every height H0..min(H1, served head) (eth_getBlockTransactionCountByNumber;
      the eth view lists executed txs only) and, for every sender in FILE
      (tx-flood --stats-file), its executed nonce (eth_getTransactionCount
      latest). With N (the genesis evm.block_interval) it also counts the
      blocks that carry EVM txs at a height with height % N != 0 (must be 0).
  evm_load.py summary --dir CELL --rate R --senders S --rc RC
      Merges CELL/evm-load-stats.json, CELL/evm-load-scan.json and each node's
      torus_exec_evm_seconds delta (metrics-before/after-val<i>.txt) into
      CELL/summary.json as .evm_load, with `ok` and `problems` (why the cell did
      not get the EVM load it asked for). Not ok: also folded into .validity
      (REJECT), .status and .headline.benchmark_accepted, so `health.py accept`
      rejects the cell. Idempotent: resummarize.sh runs it again.
  evm_load.py funded --genesis FILE --keys FILE
      Exit 0 when every sender of the key file (`bench-throughput gen-accounts
      --secret-keys` lines: index address key) holds EVM balance in the
      genesis `accounts`; else exit 1 and say which do not.

During a cell only tx-flood sends EVM txs (the load and the oracle feed send
native actions), so every executed EVM tx in the window is one of its.
"""

import argparse
import json
import os
import sys
import time
import urllib.request

# urllib/socket failures are OSError; bad JSON is ValueError; a missing or
# malformed result is KeyError/TypeError. Each counts as a read error.
RPC_ERRORS = (OSError, ValueError, KeyError, TypeError)
BATCH = 100
MAX_LISTED = 20


class Rpc:
    def __init__(self, url, timeout=10):
        self.url = url
        self.timeout = timeout
        self.batch_ok = True

    def _post(self, body):
        req = urllib.request.Request(
            self.url, json.dumps(body).encode(), {"content-type": "application/json"}
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as r:
            return json.load(r)

    def call(self, method, params):
        reply = self._post(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        )
        if "error" in reply:
            raise ValueError("rpc error: %s" % reply["error"])
        return reply["result"]

    def calls(self, method, params_list):
        """One result per params (None where the call failed), in order.
        JSON-RPC batches of BATCH; single calls if the server refuses a batch."""
        out = [None] * len(params_list)
        for start in range(0, len(params_list), BATCH):
            chunk = params_list[start : start + BATCH]
            if self.batch_ok:
                try:
                    reply = self._post(
                        [
                            {
                                "jsonrpc": "2.0",
                                "id": start + i,
                                "method": method,
                                "params": p,
                            }
                            for i, p in enumerate(chunk)
                        ]
                    )
                    if not isinstance(reply, list):
                        raise ValueError("batch refused: %s" % str(reply)[:200])
                    for item in reply:
                        i = item.get("id")
                        if (
                            isinstance(i, int)
                            and start <= i < start + len(chunk)
                            and "error" not in item
                        ):
                            out[i] = item.get("result")
                    continue
                except RPC_ERRORS:
                    self.batch_ok = False
            for i, p in enumerate(chunk):
                try:
                    out[start + i] = self.call(method, p)
                except RPC_ERRORS:
                    out[start + i] = None
        return out


def hex_int(v):
    """0x-hex string -> int; anything else -> None."""
    if isinstance(v, str) and v.startswith("0x"):
        try:
            return int(v, 16)
        except ValueError:
            return None
    return None


def wait_head(rpc, target, wait_s, sleep=1.0):
    """Poll eth_blockNumber until it reaches `target` or `wait_s` passes.
    Returns (served head or None, seconds waited)."""
    t0 = time.monotonic()
    head = None
    while True:
        try:
            head = hex_int(rpc.call("eth_blockNumber", []))
        except RPC_ERRORS:
            pass
        waited = time.monotonic() - t0
        if (head is not None and head >= target) or waited >= wait_s:
            return head, round(waited, 1)
        time.sleep(sleep)


def scan(rpc, h0, h1, interval, stats, wait_s):
    head, waited = wait_head(rpc, h1, wait_s)
    out = {
        "from": h0,
        "to": h1,
        "interval": interval,
        "served_head": head,
        "waited_s": waited,
        "complete": head is not None and head >= h1,
    }
    top = h1 if head is None else min(h1, head)
    heights = list(range(h0, top + 1)) if top >= h0 else []
    counts = rpc.calls(
        "eth_getBlockTransactionCountByNumber", [[hex(h)] for h in heights]
    )
    evm = []  # [height, executed EVM txs] for heights that carry any
    unread = []
    for h, c in zip(heights, counts, strict=True):
        n = hex_int(c)
        if n is None:
            unread.append(h)
        elif n > 0:
            evm.append([h, n])
    off = [h for h, _ in evm if interval and h % interval != 0]
    out.update(
        {
            "scanned_to": top if heights else None,
            "blocks_scanned": len(heights),
            "read_errors": len(unread),
            "unread_heights": unread[:MAX_LISTED],
            "evm_blocks": len(evm),
            "evm_txs": sum(n for _, n in evm),
            "evm_txs_per_block_max": max((n for _, n in evm), default=0),
            "cadence_heights": (
                sum(1 for h in heights if h % interval == 0) if interval else None
            ),
            "off_cadence_blocks": len(off) if interval else None,
            "off_cadence_heights": off[:MAX_LISTED] if interval else None,
            "evm_block_txs": evm,
        }
    )
    senders = (stats or {}).get("per_sender") or []
    if senders:
        nonces = rpc.calls(
            "eth_getTransactionCount", [[s.get("address"), "latest"] for s in senders]
        )
        rows, landed, unlanded, unread_n = [], 0, 0, 0
        for s, n in zip(senders, nonces, strict=True):
            chain = hex_int(n)
            start, nxt = s.get("start_nonce"), s.get("next_nonce")
            rows.append(
                {
                    "address": s.get("address"),
                    "start_nonce": start,
                    "next_nonce": nxt,
                    "chain_nonce": chain,
                }
            )
            if chain is None or not isinstance(start, int) or not isinstance(nxt, int):
                unread_n += 1
                continue
            landed += max(0, chain - start)
            unlanded += max(0, nxt - chain)
        out.update(
            {
                "senders": rows,
                "landed_by_nonce": landed,
                "accepted_not_landed": unlanded,
                "sender_nonce_read_errors": unread_n,
            }
        )
    return out


def metric(path, name):
    """Value of an unlabelled Prometheus sample in a scrape file, else None."""
    try:
        with open(path) as f:
            for line in f:
                fields = line.split()
                if len(fields) >= 2 and fields[0] == name:
                    return float(fields[1])
    except OSError:
        return None
    return None


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def summary(cell, rate, senders, rc):
    stats = load(os.path.join(cell, "evm-load-stats.json")) or {}
    sc = load(os.path.join(cell, "evm-load-scan.json")) or {}
    evm_blocks = sc.get("evm_blocks")
    nodes = {}
    for i in range(3):
        d = {}
        for part in ("sum", "count"):
            name = "torus_exec_evm_seconds_" + part
            b = metric(os.path.join(cell, "metrics-before-val%d.txt" % i), name)
            a = metric(os.path.join(cell, "metrics-after-val%d.txt" % i), name)
            d["delta_" + part] = None if a is None or b is None else a - b
        s = d["delta_sum"]
        d["ms_per_evm_block"] = (
            round(s * 1000 / evm_blocks, 3) if s is not None and evm_blocks else None
        )
        nodes["val%d" % i] = d
    elapsed = stats.get("elapsed_s")
    accepted = stats.get("accepted")
    e = {
        "evm_load": 1,
        "rate": rate,
        "senders": senders,
        "rc": int(rc) if rc.lstrip("-").isdigit() else None,
        "stats_present": bool(stats),
        "scan_present": bool(sc),
        "fanout": stats.get("fanout"),
        "sender_offset": stats.get("sender_offset"),
        "stopped_by": stats.get("stopped_by"),
        "elapsed_s": elapsed,
        "sent": stats.get("sent"),
        "accepted": accepted,
        "rejected": stats.get("rejected"),
        "reject_reasons": stats.get("reject_reasons"),
        "throttled": stats.get("throttled"),
        "missed": stats.get("missed"),
        "unfinished": stats.get("unfinished"),
        "accepted_per_s": (
            round(accepted / elapsed, 2) if accepted is not None and elapsed else None
        ),
        "landed": sc.get("evm_txs"),
        "landed_by_nonce": sc.get("landed_by_nonce"),
        "accepted_not_landed": sc.get("accepted_not_landed"),
        "evm_blocks": evm_blocks,
        "evm_txs_per_block_max": sc.get("evm_txs_per_block_max"),
        "interval": sc.get("interval"),
        "cadence_heights": sc.get("cadence_heights"),
        "off_cadence_blocks": sc.get("off_cadence_blocks"),
        "scan_from": sc.get("from"),
        "scan_to": sc.get("scanned_to"),
        "scan_complete": sc.get("complete"),
        "scan_read_errors": sc.get("read_errors"),
        "exec_evm_seconds": nodes,
    }
    e["problems"] = problems_of(e)
    e["ok"] = not e["problems"]
    return e


def problems_of(e):
    """Why a cell that asked for EVM load did not get it ([] = it did). Any
    problem fails the cell (run-cell.sh), whatever the native verdict."""
    p = []
    if e["rc"] != 0:
        p.append("tx-flood rc=%s" % e["rc"])
    if not e["stats_present"]:
        p.append("no evm-load-stats.json")
    elif not e["accepted"]:
        p.append("no EVM tx accepted")
    if not e["scan_present"]:
        p.append("no evm-load-scan.json")
        return p
    if e["scan_complete"] is not True:
        p.append("scan incomplete: val0's eth head stayed below the committed head")
    if e["scan_read_errors"]:
        p.append("%s heights unreadable" % e["scan_read_errors"])
    if e["off_cadence_blocks"]:
        p.append(
            "%s blocks carry EVM txs off the N=%s cadence"
            % (e["off_cadence_blocks"], e["interval"])
        )
    if e["accepted"] and not e["landed"]:
        p.append("no EVM tx landed")
    return p


EVM_REASON = "EVM load failed: "


def fold_verdict(d):
    """A cell that asked for EVM load and did not get it is rejected in the
    canonical verdict too (validity, status, headline.benchmark_accepted), so
    `health.py accept` and every reader of summary.json see it. Idempotent
    (resummarize.sh runs it again)."""
    e = d.get("evm_load") or {}
    if e.get("ok") is True:
        return
    v = d.setdefault("validity", {})
    fails = [f for f in v.get("fail_reasons") or [] if not str(f).startswith(EVM_REASON)]
    v["fail_reasons"] = fails + [EVM_REASON + "; ".join(e.get("problems") or ["no .evm_load"])]
    v["verdict"] = "REJECT"
    v["accepted"] = False
    d["status"] = "INVALID"
    d.setdefault("headline", {})["benchmark_accepted"] = False


def funded(genesis_path, keys_path):
    """(ok, message): does every key-file sender hold EVM wei in the genesis?"""
    try:
        with open(keys_path) as f:
            want = [ln.split()[1].lower() for ln in f if len(ln.split()) >= 2]
    except OSError as e:
        return False, "cannot read %s: %s" % (keys_path, e)
    g = load(genesis_path)
    if not isinstance(g, dict):
        return False, "cannot parse %s" % genesis_path
    bal = {}
    for a in g.get("accounts") or []:
        try:
            v = str(a.get("balance", "0"))
            bal[str(a["address"]).lower()] = (
                int(v, 16) if v.startswith("0x") else int(v)
            )
        except (KeyError, ValueError, TypeError, AttributeError):
            continue
    if not want:
        return False, "%s lists no sender" % keys_path
    missing = [w for w in want if bal.get(w, 0) <= 0]
    if missing:
        return False, "%d of %d EVM load senders hold no EVM balance in %s, e.g. %s" % (
            len(missing),
            len(want),
            genesis_path,
            " ".join(missing[:3]),
        )
    return True, "%d/%d EVM load senders funded in the genesis (min %d wei)" % (
        len(want),
        len(want),
        min(bal[w] for w in want),
    )


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("scan")
    s.add_argument("--rpc", required=True)
    s.add_argument("--from", dest="h0", type=int, required=True)
    s.add_argument("--to", dest="h1", type=int, required=True)
    s.add_argument(
        "--interval",
        default="",
        help="genesis evm.block_interval (N); empty = no cadence check",
    )
    s.add_argument("--stats", help="tx-flood --stats-file")
    s.add_argument("--wait-s", type=float, default=60)
    s.add_argument("--out", required=True)
    m = sub.add_parser("summary")
    m.add_argument("--dir", required=True)
    m.add_argument("--rate", required=True)
    m.add_argument("--senders", type=int, required=True)
    m.add_argument("--rc", default="")
    fu = sub.add_parser("funded")
    fu.add_argument("--genesis", required=True)
    fu.add_argument("--keys", required=True)
    a = p.parse_args(argv)
    if a.cmd == "funded":
        ok, msg = funded(a.genesis, a.keys)
        print(msg)
        return 0 if ok else 1
    if a.cmd == "scan":
        interval = (
            int(a.interval) if a.interval.isdigit() and int(a.interval) > 0 else None
        )
        stats = load(a.stats) if a.stats else None
        out = scan(Rpc(a.rpc), a.h0, a.h1, interval, stats, a.wait_s)
        with open(a.out, "w") as f:
            json.dump(out, f, indent=1)
        print(
            "evm scan %s..%s (served %s, waited %ss, complete=%s): %d blocks, %d EVM blocks, "
            "%d EVM txs executed, off-cadence blocks %s (N=%s), read errors %d%s"
            % (
                out["from"],
                out["scanned_to"],
                out["served_head"],
                out["waited_s"],
                out["complete"],
                out["blocks_scanned"],
                out["evm_blocks"],
                out["evm_txs"],
                out["off_cadence_blocks"],
                interval,
                out["read_errors"],
                (
                    ", landed by nonce %s, accepted not landed %s"
                    % (out["landed_by_nonce"], out["accepted_not_landed"])
                    if "landed_by_nonce" in out
                    else ""
                ),
            )
        )
        return 0
    path = os.path.join(a.dir, "summary.json")
    with open(path) as f:
        d = json.load(f)
    try:
        d["evm_load"] = summary(a.dir, a.rate, a.senders, a.rc)
    except Exception as e:  # a broken artifact must not leave the cell unjudged
        d["evm_load"] = {
            "evm_load": 1,
            "rate": a.rate,
            "senders": a.senders,
            "rc": a.rc,
            "ok": False,
            "problems": ["evm_load summary failed: %r" % e],
        }
    fold_verdict(d)
    with open(path, "w") as f:
        json.dump(d, f, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
