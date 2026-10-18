#!/usr/bin/env python3
"""Pre-load (run-cell.sh PRELOAD=N, bench queue 8f): the summary.json record
of the resting orders `bench-throughput preload` put on the books between node
ready and the load start.

  preload.py summary --dir CELL
      Merges CELL/preload.json (the bench's JSON report) and
      CELL/preload-cell.json (run-cell.sh: target, wall seconds, rc,
      boot_to_ready_s) into CELL/summary.json as .preload, with `ok` and
      `problems`. Not ok (rc != 0, target not reached on every node, a failed
      action, no report): also folded into .validity (REJECT), .status and
      .headline.benchmark_accepted, so `health.py accept` rejects the cell.
      Creates summary.json when absent (run-cell.sh records a failed pre-load
      before it dies). Idempotent: resummarize.sh runs it again.
  preload.py owners --target N --per-owner P --owner-offset O --bulk-count B
                    --spam-funded-keys K
      Prints "owners first end" (end exclusive) for the pre-load owner range
      and exits 0 when it lies inside the genesis-funded bulk range
      60..60+B and below the top K indices (`consensus --spam-cancel-funded`
      keys); else exit 1 and say why.

The pre-load runs before the metrics-before snapshot, the samplers and the
load, so none of its blocks count toward the cell's load numbers.
"""

import argparse
import json
import os
import sys

REPORT_FILE = "preload.json"
CELL_FILE = "preload-cell.json"
REASON = "pre-load failed: "
FUNDED_BULK_START = 60

# Report fields copied into .preload as they are.
REPORT_KEYS = (
    "markets",
    "owners",
    "owner_offset",
    "owner_end",
    "per_owner",
    "batch",
    "distance",
    "levels",
    "actions",
    "orders_placed",
    "actions_admitted",
    "orders_accepted",
    "actions_failed",
    "resends",
    "resting",
    "resting_by_node",
    "placed_accepted_by_node",
    "resting_gauge_by_node",
    "reached",
    "submit_seconds",
    "failures",
)


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def problems_of(p):
    """Why the pre-load did not deliver its target ([] = it did)."""
    out = []
    rep = p.get("report")
    if p.get("rc") != 0:
        out.append("preload rc=%s" % p.get("rc"))
    if not isinstance(rep, dict):
        out.append("no %s (bench report)" % REPORT_FILE)
        return out
    target, resting = p.get("target"), rep.get("resting")
    if not isinstance(resting, int) or not isinstance(target, int) or resting < target:
        out.append("resting %s < target %s" % (resting, target))
    if rep.get("reached") is not True and not out:
        out.append("bench reports the target not reached")
    if rep.get("actions_failed"):
        out.append("%s actions failed" % rep["actions_failed"])
    return out


def fold_verdict(d, problems):
    v = d.setdefault("validity", {})
    fails = [f for f in v.get("fail_reasons") or [] if not str(f).startswith(REASON)]
    if not problems:
        v["fail_reasons"] = fails
        return
    v["fail_reasons"] = fails + [REASON + "; ".join(problems)]
    v["verdict"] = "REJECT"
    v["accepted"] = False
    d["status"] = "INVALID"
    d.setdefault("headline", {})["benchmark_accepted"] = False


def summary(cell_dir):
    cell = load(os.path.join(cell_dir, CELL_FILE)) or {}
    rep = load(os.path.join(cell_dir, REPORT_FILE))
    rc = cell.get("rc")
    p = {
        "target": cell.get("target"),
        "seconds": cell.get("seconds"),
        "boot_to_ready_s": cell.get("boot_to_ready_s"),
        "rc": rc,
        "cmd": cell.get("cmd"),
        "report_present": isinstance(rep, dict),
    }
    for k in REPORT_KEYS:
        p[k] = rep.get(k) if isinstance(rep, dict) else None
    if p["markets"] is None:
        p["markets"] = cell.get("markets")
    p["bench_seconds"] = rep.get("seconds") if isinstance(rep, dict) else None
    p["problems"] = problems_of({"rc": rc, "target": p["target"], "report": rep})
    p["ok"] = not p["problems"]
    path = os.path.join(cell_dir, "summary.json")
    d = load(path)
    if not isinstance(d, dict):
        d = {}
    d["preload"] = p
    fold_verdict(d, p["problems"])
    tmp = path + ".preload.tmp"
    with open(tmp, "w") as f:
        json.dump(d, f, indent=1)
    os.replace(tmp, path)
    return p


def owner_range(target, per_owner, offset, bulk_count, spam_funded_keys):
    """(owners, first, end, why-not-or-None) for the pre-load owner range."""
    owners = -(-target // per_owner)
    end = offset + owners
    funded_end = FUNDED_BULK_START + bulk_count
    usable_end = funded_end - spam_funded_keys
    why = None
    if offset < FUNDED_BULK_START or end > funded_end:
        why = (
            "pre-load owners %d..%d are not all genesis-funded (%d..%d = 60 + BULK_COUNT)"
            % (offset, end, FUNDED_BULK_START, funded_end)
        )
    elif end > usable_end:
        why = (
            "pre-load owners %d..%d overlap the %d funded spam keys %d..%d (the top of the funded range)"
            % (offset, end, spam_funded_keys, usable_end, funded_end)
        )
    return owners, offset, end, why


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("summary")
    s.add_argument("--dir", required=True)
    o = sub.add_parser("owners")
    for name in (
        "--target",
        "--per-owner",
        "--owner-offset",
        "--bulk-count",
        "--spam-funded-keys",
    ):
        o.add_argument(name, type=int, required=True)
    a = ap.parse_args(argv)
    if a.cmd == "summary":
        p = summary(a.dir)
        print(
            json.dumps(
                {
                    "ok": p["ok"],
                    "problems": p["problems"],
                    "resting": p["resting"],
                    "target": p["target"],
                    "seconds": p["seconds"],
                }
            )
        )
        return 0
    if a.target < 1 or a.per_owner < 1:
        print("--target and --per-owner must be >= 1", file=sys.stderr)
        return 2
    owners, first, end, why = owner_range(
        a.target, a.per_owner, a.owner_offset, a.bulk_count, a.spam_funded_keys
    )
    if why:
        print(why, file=sys.stderr)
        return 1
    print(owners, first, end)
    return 0


if __name__ == "__main__":
    sys.exit(main())
