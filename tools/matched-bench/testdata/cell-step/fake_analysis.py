#!/usr/bin/env python3
"""Stand-in for a campaign analyst script (shape of a-cells.py: OUT.json PREFIX TAG:ARM).

Used only by test_cell_step.py. FAKE_ANALYSIS_MODE selects the behaviour:
  ok     write {tag: {"ms": <summary.json headline.matched_s_avg + FAKE_ANALYSIS_DELTA>}}, exit 0
  fail   print a traceback-like line, exit 1
  norow  write {} (exit 0 but no row for the cell)
  sleep  write the pid to FAKE_ANALYSIS_PIDFILE, then sleep 120 s (a hung analysis)
FAKE_ANALYSIS_CALLS (optional): one line "<prefix><tag> <arm>" is appended per call.
"""

import json
import os
import sys
import time

out, prefix, ta = sys.argv[1], sys.argv[2], sys.argv[3]
tag, arm = ta.split(":")
mode = os.environ.get("FAKE_ANALYSIS_MODE", "ok")
calls = os.environ.get("FAKE_ANALYSIS_CALLS")
if calls:
    with open(calls, "a") as fh:
        fh.write("%s%s %s\n" % (prefix, tag, arm))
if mode == "fail":
    print(
        "Traceback (most recent call last): KeyError: 'phase_by_node'", file=sys.stderr
    )
    sys.exit(1)
if mode == "norow":
    json.dump({}, open(out, "w"))
    sys.exit(0)
if mode == "sleep":
    with open(os.environ["FAKE_ANALYSIS_PIDFILE"], "w") as fh:
        fh.write(str(os.getpid()))
    time.sleep(120)
    sys.exit(0)
s = json.load(open(prefix + tag + "/summary.json"))
ms = s["headline"]["matched_s_avg"] + float(os.environ.get("FAKE_ANALYSIS_DELTA", "0"))
json.dump({tag: {"arm": arm, "ms": ms}}, open(out, "w"))
