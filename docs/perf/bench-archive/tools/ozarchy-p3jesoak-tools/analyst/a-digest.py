"""ozarchy-p3jesoak: classify state-digest disagreements per cell (part A's a-cells.py classify(), unchanged logic).
python3 -I a-digest.py CELLDIR... -> per cell: verdict, digest heights / seconds, per minority node the differing fields and class
(mark-only = only the 5 oracle-mark-derived account fields differ)."""
import json
import sys

V = ("val0", "val1", "val2")
MARK = {"accountValue", "freeMargin", "maintenanceMarginUsed", "totalMarginUsed", "withdrawable"}
CASH = {"cashBalance", "nativeBalance", "evmBalance", "permanentStake"}


def classify(a, b):
    kinds, fields, nl = set(), set(), 0
    if len(a) != len(b):
        kinds.add("line-count")
    for x, y in zip(a, b):
        if x == y:
            continue
        nl += 1
        try:
            ox, oy = json.loads(x), json.loads(y)
        except ValueError:
            kinds.add("nonjson")
            continue
        if "asks" in ox or "bids" in ox:
            kinds.add("book"); fields.add("book")
        elif "longOi" in ox:
            kinds.add("oi"); fields.add("oi")
        else:
            for k in set(ox) | set(oy):
                if ox.get(k) != oy.get(k):
                    fields.add(k)
                    kinds.add("mark" if k in MARK else ("cash" if k in CASH else "other"))
    cls = "mark-only" if kinds == {"mark"} else ("HARD:" + ",".join(sorted(kinds - {"mark"})) if kinds - {"mark"} else "none")
    return nl, sorted(fields), cls


for cell in sys.argv[1:]:
    s = json.load(open(cell + "/summary.json"))
    ag = {j["node"]: j for j in map(json.loads, open(cell + "/agreement.jsonl"))}
    lines = {v: open(f"{cell}/state-digest-{v}.txt").read().splitlines() for v in V}
    dg = {v: ag[v]["state_digest"] for v in V}
    vals = list(dg.values())
    maj = max(set(vals), key=vals.count)
    ref = next(v for v in V if dg[v] == maj)
    print(cell.rstrip("/").split("/")[-1], s["validity"]["verdict"], s["headline"].get("agreement_verdict"),
          "cmp_height", [ag[v].get("cmp_height") for v in V], "block_hash equal", len({ag[v]["block_hash"] for v in V}) == 1,
          "digest_height", [ag[v].get("digest_height") for v in V], "digest_s", [ag[v].get("digest_seconds") for v in V],
          "distinct digests", len(set(vals)))
    for v in V:
        if dg[v] == maj and vals.count(maj) > 1:
            continue
        nl, fields, cls = classify(lines[ref], lines[v])
        print(f"  {v} vs {ref}: {nl} lines differ, fields {fields}, class {cls}")
