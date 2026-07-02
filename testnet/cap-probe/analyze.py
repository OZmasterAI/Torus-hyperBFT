#!/usr/bin/env python3
"""cap-probe analyzer: attribute the binding DA path per rung.

Reads out-<label>/ produced by run.sh and, for each batch-size rung, prints the
per-node PATH-1 (gossip pre-spread) vs PATH-2 (native-da recovery pull) counter
deltas, then applies a decision rule to name what actually bound throughput.

Decision rule (the whole point of the probe):
  * PATH 2 moved (pull_requests/failures up, OUTBOUND FAILURE, body-fetch-exhausted)
        -> recovery is on the critical path  -> erasure/recovery-path fix is relevant.
  * PATH 1 moved (gossip dropped_full/oversized, "dropped from pre-spread")
        -> pre-spread itself saturates       -> Option-A erasure won't help; need
                                                 ingress dispersal (Option B) / bigger budget.
  * Neither moved, blocks pinned at the cap  -> CAP-bound; DA has headroom, raise cap further.
  * Neither moved, low fill + low drop       -> load/bench-bound; push harder.

Usage: python3 testnet/cap-probe/analyze.py [out-dir]   (default: newest out-* dir)
"""

import glob, os, re, sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
HERE = os.path.dirname(os.path.abspath(__file__))

# native-DA / gossip counters we diff across a rung
P2_REQ = "torus_native_da_pull_requests_total"
P2_FAIL = "torus_native_da_pull_failures_total"
P2_REC = "torus_native_da_pull_recovered_total"
P1_DFULL = "torus_native_gossip_dropped_full_total"
P1_DOVER = "torus_native_gossip_dropped_oversized_total"
P1_PUB = "torus_native_gossip_published_actions_total"
P1_RECV = "torus_native_gossip_received_actions_total"


def load(path):
    m = {}
    if os.path.exists(path):
        for ln in open(path):
            p = ln.split()
            if len(p) >= 2:
                try:
                    m[p[0]] = float(p[1])
                except ValueError:
                    pass
    return m


def parse_log(path):
    a = dict(bcast=0, serve=0, resp=0, outf=0, inf=0, oversize=0, predrop=0, exhaust=0)
    if os.path.exists(path):
        for ln in open(path, errors="replace"):
            s = ANSI.sub("", ln)
            a["bcast"] += "broadcast pre-proposal" in s
            a["serve"] += "native-da request: serving" in s
            a["resp"] += "native-da response: queued" in s
            a["outf"] += "OUTBOUND FAILURE" in s
            a["inf"] += "INBOUND FAILURE" in s
            a["oversize"] += "oversized native action" in s
            a["predrop"] += "dropped from pre-spread" in s
            a["exhaust"] += "body fetch exhausted" in s
    return a


def rungs(d):
    seen = []
    for f in sorted(glob.glob(os.path.join(d, "m-*-before.txt"))):
        # m-<tag>-<node>-before.txt  ->  tag is everything between "m-" and the last two fields
        base = os.path.basename(f)[2 : -len("-before.txt")]
        tag = base.rsplit("-", 1)[0]
        if tag not in seen:
            seen.append(tag)
    return seen


def nodes(d, tag):
    ns = []
    for f in sorted(glob.glob(os.path.join(d, f"m-{tag}-*-before.txt"))):
        ns.append(os.path.basename(f)[len(f"m-{tag}-") : -len("-before.txt")])
    return ns


def summary_line(d, tag):
    sp = os.path.join(d, "summary.txt")
    if os.path.exists(sp):
        for ln in open(sp):
            if ln.startswith(tag + " "):
                return ln.strip()
    return ""


def analyze(d):
    print(f"### cap-probe analysis: {d} ###\n")
    for tag in rungs(d):
        print("=" * 68)
        print(tag, " ", summary_line(d, tag))
        print("=" * 68)
        agg = dict(
            p2req=0,
            p2fail=0,
            dfull=0,
            dover=0,
            serve=0,
            outf=0,
            predrop=0,
            oversize=0,
            exhaust=0,
        )
        for n in nodes(d, tag):
            b = load(os.path.join(d, f"m-{tag}-{n}-before.txt"))
            a = load(os.path.join(d, f"m-{tag}-{n}-after.txt"))
            dv = lambda k: int(a.get(k, 0) - b.get(k, 0))
            lg = parse_log(os.path.join(d, f"log-{tag}-{node_logname(d, tag, n)}.txt"))
            print(
                f"  [{n:>4}] P1 pre-spread: pub +{dv(P1_PUB):<6} recv +{dv(P1_RECV):<6} "
                f"dropFull +{dv(P1_DFULL)} dropOversz +{dv(P1_DOVER)} | predrop_log {lg['predrop']} oversz_log {lg['oversize']}"
            )
            print(
                f"         P2 recovery : req +{dv(P2_REQ):<6} recovered +{dv(P2_REC):<6} "
                f"fail +{dv(P2_FAIL)} | serve {lg['serve']} resp {lg['resp']} OUTBOUND_FAIL {lg['outf']} bodyFetchExhaust {lg['exhaust']}"
            )
            agg["p2req"] += dv(P2_REQ)
            agg["p2fail"] += dv(P2_FAIL)
            agg["dfull"] += dv(P1_DFULL)
            agg["dover"] += dv(P1_DOVER)
            agg["serve"] += lg["serve"]
            agg["outf"] += lg["outf"]
            agg["predrop"] += lg["predrop"]
            agg["oversize"] += lg["oversize"]
            agg["exhaust"] += lg["exhaust"]

        # --- decision rule ---
        wedged = "WEDGE" in summary_line(d, tag) or agg["exhaust"] > 0
        path2 = (
            agg["p2req"] > 0
            or agg["p2fail"] > 0
            or agg["outf"] > 0
            or agg["exhaust"] > 0
        )
        path1 = (
            agg["dfull"] > 0
            or agg["dover"] > 0
            or agg["predrop"] > 0
            or agg["oversize"] > 0
        )
        print("  " + "-" * 64)
        if path2 and not path1:
            v = "PATH 2 (native-da RECOVERY pull) binds -> erasure / recovery-path fix IS relevant"
        elif path1 and not path2:
            v = "PATH 1 (gossip PRE-SPREAD) binds -> Option-A erasure won't help; need ingress dispersal (Option B) / bigger pre-spread budget"
        elif path1 and path2:
            v = "BOTH paths stressed -> pre-spread saturates AND recovery is failing to cover; the wedge is dissemination-wide"
        elif wedged:
            v = "WEDGE but NO DA-path stress on counters -> look elsewhere (consensus/view timeout, mempool, exec lag), not DA"
        else:
            v = "NO DA stress -> throughput is CAP-bound or load-bound (raise cap / push harder), DA has headroom"
        print(f"  VERDICT: {v}")
        if wedged:
            print("  (chain wedged/stalled this rung — see summary.txt + bench-*.txt)")
        print()


# logs are named per docker service (validator-0..) or "node" for file mode;
# metrics nodes are v0/v1.. — map best-effort so the analyzer stays robust if names differ.
_LOGCACHE = {}


def node_logname(d, tag, metrics_node):
    if (d, tag) not in _LOGCACHE:
        _LOGCACHE[(d, tag)] = [
            os.path.basename(f)[len(f"log-{tag}-") : -4]
            for f in glob.glob(os.path.join(d, f"log-{tag}-*.txt"))
        ]
    logs = _LOGCACHE[(d, tag)]
    # exact suffix/prefix match (v0 <-> validator-0), else positional, else single "node"
    for lg in logs:
        if metrics_node.lstrip("v").rstrip("c") and lg.endswith(metrics_node[-1]):
            return lg
    return logs[0] if len(logs) == 1 else metrics_node


if __name__ == "__main__":
    if len(sys.argv) > 1:
        d = sys.argv[1]
        if not os.path.isabs(d):
            d = os.path.join(HERE, d) if os.path.exists(os.path.join(HERE, d)) else d
    else:
        cand = sorted(glob.glob(os.path.join(HERE, "out-*")), key=os.path.getmtime)
        d = cand[-1] if cand else "."
    analyze(d)
