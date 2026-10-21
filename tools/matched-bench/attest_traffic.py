#!/usr/bin/env python3
"""STATE_HASH_ATTEST=1: tell the validators' AttestStateHash actions apart from
every other native action, by kind, over a window of blocks.

Every validator signs one AttestStateHash per 100-block checkpoint for as long
as the chain commits (crates/torus-consensus/src/state_hash.rs
submit_attestations), so after the load torus_native_actions_processed_total
keeps moving on the idle chain. The node has no metric that counts executed
attestations or splits the native counter / mempool by action kind, but every
committed block's body is readable by kind: torus_getBlockBody returns each
SignedNativeAction as serde JSON (NativeAction is externally tagged:
{"action": {"AttestStateHash": {...}}, ...}, a unit variant as the bare
string) and, once executed on that node, nativeActionStatus per action.

A non-attestation action counts unless its status is "skipped" (failed the
replay guard / signature check at execution: no state change, and not counted
by torus_native_actions_processed_total, app.rs). "failed" / "rejected"
passed the replay guard and count, like the counter. A block not executed on
the node yet (status null) counts every action. An action whose kind cannot be
read counts as non-attestation: only a readable AttestStateHash is excused.

The body is written at COMMIT, in the same batch as the header (app.rs
persist_committed_block_durably_timed), before execution, so every committed
height has a readable body.

  attest_traffic.py window --from LO --to HI --rpc URL [URL ...]
      Scan blocks LO..HI (inclusive). A height a node does not return (null,
      error, malformed) is read from the next node. Prints the scan as JSON.
      Exit 0: every body read and no non-attestation action; 1: a
      non-attestation action executed in the window; 2: a body unreadable on
      every node, a bad range, or a usage error (unavailable evidence).
"""

import argparse
import json
import sys
from urllib.request import Request, urlopen

ATTEST_KIND = "AttestStateHash"
METHOD = "torus_getBlockBody"
BATCH = 20  # bodies per JSON-RPC batch (a load block can be ~200 KB of JSON)
MAX_BLOCKS = 50_000  # ~33 min at 25 blk/s: a larger window is a caller bug
TIMEOUT_S = 10
LISTED = 10  # heights listed per problem in the JSON (counts are complete)


def action_kind(signed):
    """The NativeAction variant name of one SignedNativeAction (serde JSON), or
    None when the shape is not the externally tagged enum."""
    action = signed.get("action") if isinstance(signed, dict) else None
    if isinstance(action, str):
        return action
    if isinstance(action, dict) and len(action) == 1:
        return next(iter(action))
    return None


def classify_body(body):
    """(attestations, other) native actions of one torus_getBlockBody result
    that count (see the module doc). ValueError on a malformed body."""
    if not isinstance(body, dict) or not isinstance(body.get("nativeActions"), list):
        raise ValueError("not a block body")
    actions = body["nativeActions"]
    count = body.get("nativeActionCount", len(actions))
    if count != len(actions):
        raise ValueError("nativeActionCount %r != %d actions" % (count, len(actions)))
    status = body.get("nativeActionStatus")
    if status is not None and (
        not isinstance(status, list) or len(status) != len(actions)
    ):
        raise ValueError("nativeActionStatus does not match the actions")
    attest = other = 0
    for i, signed in enumerate(actions):
        if status is not None and status[i] == "skipped":
            continue
        if action_kind(signed) == ATTEST_KIND:
            attest += 1
        else:
            other += 1
    return attest, other


def post(url, payload):
    request = Request(
        url,
        data=json.dumps(payload).encode(),
        headers={"content-type": "application/json"},
    )
    with urlopen(request, timeout=TIMEOUT_S) as response:
        return json.loads(response.read().decode())


def call(h):
    return {"jsonrpc": "2.0", "id": h, "method": METHOD, "params": [h]}


def read_bodies(url, heights):
    """{height: (attest, other)} for the heights `url` answers with a well
    formed body; a refused batch is retried one body at a time."""
    got = {}

    def take(h, reply):
        result = reply.get("result") if isinstance(reply, dict) else None
        if result is not None:
            try:
                got[h] = classify_body(result)
            except ValueError:
                pass

    for i in range(0, len(heights), BATCH):
        chunk = heights[i : i + BATCH]
        try:
            replies = post(url, [call(h) for h in chunk])
            if not isinstance(replies, list):
                raise ValueError("batch reply is not a list")
            by_id = {r.get("id"): r for r in replies if isinstance(r, dict)}
            for h in chunk:
                take(h, by_id.get(h))
        except (OSError, ValueError):
            for h in chunk:
                try:
                    take(h, post(url, call(h)))
                except (OSError, ValueError):
                    pass
    return got


def scan_window(rpcs, lo, hi):
    """Blocks lo..hi over the nodes' RPCs, in order (a height one node does not
    answer is read from the next). clean = every body read and no counted
    non-attestation action."""
    out = {
        "from": lo,
        "to": hi,
        "blocks": 0,
        "attest": 0,
        "other": 0,
        "other_heights": [],
        "unread": 0,
        "unread_heights": [],
        "clean": False,
    }
    if (
        not rpcs
        or not isinstance(lo, int)
        or not isinstance(hi, int)
        or lo < 0
        or hi < lo
        or hi - lo + 1 > MAX_BLOCKS
    ):
        out["error"] = "no RPC or bad window %r..%r" % (lo, hi)
        return out
    missing, counted = list(range(lo, hi + 1)), {}
    for url in rpcs:
        if not missing:
            break
        counted.update(read_bodies(url, missing))
        missing = [h for h in missing if h not in counted]
    others = sorted(h for h, (_, o) in counted.items() if o)
    out.update(
        blocks=len(counted),
        attest=sum(a for a, _ in counted.values()),
        other=sum(o for _, o in counted.values()),
        other_heights=others[:LISTED],
        unread=len(missing),
        unread_heights=missing[:LISTED],
    )
    out["clean"] = out["other"] == 0 and out["unread"] == 0
    return out


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="command", required=True)
    window = sub.add_parser("window")
    window.add_argument("--from", dest="lo", type=int, required=True)
    window.add_argument("--to", dest="hi", type=int, required=True)
    window.add_argument("--rpc", nargs="+", required=True)
    args = parser.parse_args(argv)
    result = scan_window(args.rpc, args.lo, args.hi)
    print(json.dumps(result, sort_keys=True))
    if result["other"] > 0:
        return 1
    return 0 if result["clean"] else 2


if __name__ == "__main__":
    sys.exit(main())
