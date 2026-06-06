#!/usr/bin/env python3
"""native-dup-factor.py — measure duplicate native INCLUSION over a block range.

For each block in [start, end] reads torus_getBlockBody and counts:
  - total native slots  = sum of nativeActionCount
  - unique actions      = distinct signed-action identities (full JSON)
duplication factor = slots / unique. ~1.0 means each action is included once
(pipeline-aware selection working); ~3.0 is the duplicate-inclusion bug.

Usage: python3 native-dup-factor.py [rpc_url] [start_block] [end_block]
"""

import json
import sys
from collections import Counter

import requests

RPC = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8645"
START = int(sys.argv[2])
END = int(sys.argv[3])


def rpc(method, params):
    body = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    return requests.post(RPC, json=body, timeout=5).json()


def main():
    slots = 0
    blocks_with_native = 0
    appearances = Counter()  # action-identity -> number of blocks it appears in
    for h in range(START, END + 1):
        r = rpc("torus_getBlockBody", [h]).get("result") or {}
        acts = r.get("nativeActions") or []
        if not acts:
            continue
        blocks_with_native += 1
        slots += len(acts)
        for a in acts:
            key = json.dumps(a, sort_keys=True, separators=(",", ":"))
            appearances[key] += 1

    unique = len(appearances)
    factor = slots / unique if unique else 0.0
    dup_actions = sum(1 for c in appearances.values() if c > 1)
    dist = Counter(appearances.values())  # {times-included: how-many-actions}

    print(f"range            : [{START}..{END}]  ({END - START + 1} blocks)")
    print(f"blocks w/ native : {blocks_with_native}")
    print(f"total slots      : {slots}")
    print(f"unique actions   : {unique}")
    print(f"DUPLICATION FACTOR = {factor:.3f}   (slots / unique)")
    print(
        f"actions in >1 blk: {dup_actions} / {unique} "
        f"({100 * dup_actions / unique:.1f}%)"
        if unique
        else "n/a"
    )
    print("inclusion distribution (times_included: num_actions):")
    for times in sorted(dist):
        print(f"   {times}x : {dist[times]}")
    print()
    if unique == 0:
        print(">>> no native actions in range")
    elif factor < 1.15:
        print(">>> DUPLICATION ELIMINATED (~1.0): pipeline-aware selection working")
    elif factor >= 2.0:
        print(">>> DUPLICATE INCLUSION present (bug)")
    else:
        print(">>> partial duplication")


if __name__ == "__main__":
    main()
