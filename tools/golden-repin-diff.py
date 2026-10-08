#!/usr/bin/env python3
"""Compare two REPIN_DUMP files of perf_equivalence_golden.rs (s100).

Usage: golden-repin-diff.py BASE_DUMP NEW_DUMP

Lines are "<block> OUT <outputs>" or "<block> <cf id> <key hex> <value>".
Reports per category how many lines differ and the largest raw difference:
position fields (POS.<field>), balance fields (BAL.<field>), outputs whose
difference is only in decimal amounts (OUT amounts), outputs that differ in
anything else (OUT structural), rows with different keys (KEYS) and other
rows (OTHER cf <id>). Exit 1 if any structural difference is found
(different line counts, keys, other rows, or outputs beyond amounts).
"""
import collections
import re
import sys

NUM = re.compile(r"-?\d+\.\d{8}")
FIELD = re.compile(r"(\w+)=(\S+)")


def raw(text):
    return round(float(text) * 10**8)


def main(base_path, new_path):
    base = open(base_path).read().split("\n")
    new = open(new_path).read().split("\n")
    counts = collections.Counter()
    worst = collections.defaultdict(int)
    structural = len(base) != len(new)
    if structural:
        print(f"line counts differ: {len(base)} vs {len(new)}")
    for a, b in zip(base, new):
        if a == b:
            continue
        if " OUT " in a:
            if NUM.sub("N", a) != NUM.sub("N", b):
                counts["OUT structural"] += 1
                structural = True
                continue
            counts["OUT amounts"] += 1
            for x, y in zip(NUM.findall(a), NUM.findall(b)):
                worst["OUT amounts"] = max(worst["OUT amounts"], abs(raw(x) - raw(y)))
            continue
        if a.split(" ")[:3] != b.split(" ")[:3]:
            counts["KEYS"] += 1
            structural = True
            continue
        kind = "POS" if " POS " in a else "BAL" if " BAL " in a else None
        if kind is None:
            counts["OTHER cf " + a.split(" ")[1]] += 1
            structural = True
            continue
        fa, fb = dict(FIELD.findall(a)), dict(FIELD.findall(b))
        for k in fa:
            if fa[k] != fb.get(k):
                counts[f"{kind}.{k}"] += 1
                try:
                    worst[f"{kind}.{k}"] = max(worst[f"{kind}.{k}"], abs(int(fa[k]) - int(fb[k])))
                except (KeyError, ValueError):
                    structural = True
    for k in sorted(counts):
        extra = f", max {worst[k]} raw" if k in worst else ""
        print(f"{k}: {counts[k]} lines{extra}")
    if not counts and not structural:
        print("identical")
    return 1 if structural else 0


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2]))
