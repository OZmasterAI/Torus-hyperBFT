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

A non-attestation action counts unless its status is "skipped" on EVERY node
(failed the replay guard / signature check at execution: no state change, and
not counted by torus_native_actions_processed_total, app.rs). The status is
per node: a height whose first answering node skipped a non-attestation action
is read from every other node too, and the action is excused only if each one
returns the same body with that action "skipped"; a node that does not answer,
has not executed the block yet (status null) or executed it counts it.
Statuses that differ between nodes are listed (status_split_heights): the
execution is deterministic, so a split is itself a fault worth a look.
"failed" / "rejected" passed the replay guard and count, like the counter. A
block not executed on the first node yet (status null) counts every action.
An action whose kind cannot be read counts as non-attestation: only a readable
AttestStateHash (or an `allowed` kind) is excused.

allowed (scan_window only; the CLI allows nothing): kinds that are expected
traffic in the window, counted apart ("allowed") and not as non-attestation.
health.py drain --feed-live --attest-live passes SubmitOraclePrices: the oracle
feed keeps running through that drain. The settle drain after the feed is
paused and the digest window allow nothing.

The body is written at COMMIT, in the same batch as the header (app.rs
persist_committed_block_durably_timed), before execution, so every committed
height has a readable body.

  attest_traffic.py window --from LO --to HI --rpc URL [URL ...]
      Scan blocks LO..HI (inclusive). A height a node does not return (null,
      error, malformed) is read from the next node; a skipped non-attestation
      action is checked on every node (see above). Prints the scan as JSON.
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


def body_actions(body):
    """[(kind, status or None)] of one torus_getBlockBody result, status None
    when the block is not executed on that node yet. ValueError on a malformed
    body."""
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
    return [
        (action_kind(signed), None if status is None else status[i])
        for i, signed in enumerate(actions)
    ]


def classify_body(body, allowed=()):
    """(attestations, other) native actions of one torus_getBlockBody result
    that count on this one node (see the module doc; allowed kinds are in
    neither). ValueError on a malformed body."""
    attest = other = 0
    for kind, status in body_actions(body):
        if status == "skipped":
            continue
        if kind == ATTEST_KIND:
            attest += 1
        elif kind not in allowed:
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
    """{height: body_actions(body)} for the heights `url` answers with a well
    formed body; a refused batch is retried one body at a time."""
    got = {}

    def take(h, reply):
        result = reply.get("result") if isinstance(reply, dict) else None
        if result is not None:
            try:
                got[h] = body_actions(result)
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


def suspect(actions, allowed):
    """A non-attestation, non-allowed action skipped on this node: excused
    only if every node skipped it."""
    return any(
        status == "skipped" and kind != ATTEST_KIND and kind not in allowed
        for kind, status in actions
    )


def count_height(actions, peers, allowed):
    """(attest, allowed, other, split) of one height: `actions` from the first
    node that answered, `peers` = every node's actions (None: no answer).
    split: the nodes that answered with a status disagree on one action."""
    kinds = [k for k, _ in actions]
    attest = allow = other = 0
    split = False
    for i, (kind, status) in enumerate(actions):
        if kind == ATTEST_KIND or kind in allowed:
            if status != "skipped":
                attest += kind == ATTEST_KIND
                allow += kind != ATTEST_KIND
            continue
        if status != "skipped":
            other += 1
            continue
        seen = [
            p[i][1] if p is not None and [k for k, _ in p] == kinds else None
            for p in peers
        ]
        split = split or len({x for x in seen if x is not None}) > 1
        other += not all(x == "skipped" for x in seen)
    return attest, allow, other, split


def scan_window(rpcs, lo, hi, allowed=()):
    """Blocks lo..hi over the nodes' RPCs, in order (a height one node does not
    answer is read from the next; a height with a skipped non-attestation
    action from every node). clean = every body read and no counted
    non-attestation (non-allowed) action."""
    allowed = tuple(sorted(set(allowed) - {ATTEST_KIND}))
    out = {
        "from": lo,
        "to": hi,
        "blocks": 0,
        "attest": 0,
        "allowed": 0,
        "allowed_kinds": list(allowed),
        "other": 0,
        "other_heights": [],
        "status_split": 0,
        "status_split_heights": [],
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
    missing, read, source = list(range(lo, hi + 1)), {}, {}
    for i, url in enumerate(rpcs):
        if not missing:
            break
        got = read_bodies(url, missing)
        read.update(got)
        source.update((h, i) for h in got)
        missing = [h for h in missing if h not in read]
    # every node's view of the heights the first one skipped something on
    suspects = sorted(h for h, a in read.items() if suspect(a, allowed))
    views = {h: [None] * len(rpcs) for h in suspects}
    for h in suspects:
        views[h][source[h]] = read[h]
    for i, url in enumerate(rpcs):
        ask = [h for h in suspects if source[h] != i]
        for h, actions in (read_bodies(url, ask) if ask else {}).items():
            views[h][i] = actions
    counted = {
        h: count_height(a, views.get(h, [a]), allowed) for h, a in read.items()
    }
    others = sorted(h for h, c in counted.items() if c[2])
    splits = sorted(h for h, c in counted.items() if c[3])
    out.update(
        blocks=len(counted),
        attest=sum(c[0] for c in counted.values()),
        allowed=sum(c[1] for c in counted.values()),
        other=sum(c[2] for c in counted.values()),
        other_heights=others[:LISTED],
        status_split=len(splits),
        status_split_heights=splits[:LISTED],
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
