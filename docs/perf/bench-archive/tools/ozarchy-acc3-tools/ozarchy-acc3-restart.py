#!/usr/bin/env python3
# ozarchy-acc2-restart.py CELL_DIR  (run as: python3 -I ozarchy-acc2-restart.py CELL_DIR).
# Restart metrics of one ozarchy-acc2 restart cell (SIGKILL val1 at bench+60 s, restart from the same data dir).
# Read-only on the cell; writes restart-metrics.json into CELL_DIR and prints the same JSON on one line.
#   db_open_s          val1 'loaded node key' -> 'state database opened' of the restarted process (s74 definition,
#                      both lines in crash-restart-tail.log, node log timestamps)
#   restart_to_db_open_s   restart -> 'state database opened'
#   wal_files_replayed / wal_replay_numbers   'Recovering log #N' of the restarted process (rocksdb-LOG-val1.txt)
#   wal_bytes_replayed     sizes of those WAL files in the last 1 s sample of val1's *.log before the SIGKILL (val1-wal-samples.txt)
#   restart_to_first_commit_s, max_commit_gap_s, max_gap_from_kill_s   crash-freeze.py (val0 commit log lines), kill 1
#   rewind_blocks      crash.json restart.gap (blocks the restarted node replayed from its applied-height marker)
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

FREEZE = "/home/oz/projects/wt/p3s0r-9b7e29b2/tools/matched-bench/crash-freeze.py"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z")
cell = sys.argv[1]


def ts_of(line):
    m = TS.match(ANSI.sub("", line))
    if not m:
        return None
    return datetime.fromisoformat(m.group(1)).replace(tzinfo=timezone.utc).timestamp()


def first_ts(path, needle):
    with open(path, errors="replace") as f:
        for line in f:
            if needle in line:
                t = ts_of(line)
                if t is not None:
                    return t
    return None


def load(name):
    with open(os.path.join(cell, name)) as f:
        return json.load(f)


out = {"cell": os.path.basename(cell.rstrip("/"))}
try:
    kill = load("crash-kill.json")
    out.update(
        down_s=kill.get("down_s"),
        killed_pid=kill.get("killed_pid"),
        restarted_pid=kill.get("restarted_pid"),
        kill_ts=kill.get("kill_ts"),
        restart_ts=kill.get("restart_ts"),
        pre_kill=kill.get("pre_kill"),
    )
    tail = os.path.join(cell, "crash-restart-tail.log")
    t_load = first_ts(tail, "loaded node key")
    t_open = first_ts(tail, "state database opened")
    out["db_open_s"] = round(t_open - t_load, 3) if t_load and t_open else None
    out["restart_to_db_open_s"] = (
        round(t_open - kill["restart_ts"], 3) if t_open else None
    )
    replay = []
    for line in open(os.path.join(cell, "rocksdb-LOG-val1.txt"), errors="replace"):
        m = re.search(r"Recovering log #(\d+)", line)
        if m and int(m.group(1)) not in replay:
            replay.append(int(m.group(1)))
    out["wal_files_replayed"] = len(replay)
    out["wal_replay_numbers"] = replay
    # last complete 1 s sample taken before the SIGKILL: "T <ts>" then "F <name> <bytes>" per WAL file
    last_ts, last_files, cur_ts, cur = None, None, None, {}
    with open(os.path.join(cell, "val1-wal-samples.txt")) as f:
        for line in f:
            p = line.split()
            if p and p[0] == "T" and len(p) == 2:
                if cur_ts is not None and cur_ts < kill["kill_ts"]:
                    last_ts, last_files = cur_ts, cur
                cur_ts, cur = float(p[1]), {}
            elif p and p[0] == "F" and len(p) == 3 and cur_ts is not None:
                cur[int(p[1].split(".")[0])] = int(p[2])
    if cur_ts is not None and cur_ts < kill["kill_ts"]:
        last_ts, last_files = cur_ts, cur
    if last_files is not None:
        out["wal_sample_lag_s"] = round(kill["kill_ts"] - last_ts, 3)
        out["wal_files_at_kill"] = len(last_files)
        out["wal_bytes_at_kill"] = sum(last_files.values())
        out["wal_bytes_replayed"] = sum(last_files.get(n, 0) for n in replay)
        out["wal_replayed_missing_from_sample"] = [
            n for n in replay if n not in last_files
        ]
    else:
        out["wal_bytes_replayed"] = None
    crash = load("crash.json")
    out["rewind_blocks"] = (crash.get("restart") or {}).get("gap")
    fr = subprocess.run(
        [sys.executable, "-I", FREEZE, cell, "--json"],
        capture_output=True,
        text=True,
        check=True,
    )
    k1 = json.loads(fr.stdout)["kills"][0]
    out.update(
        restart_to_first_commit_s=k1.get("restart_to_first_commit_s"),
        max_commit_gap_s=k1.get("max_commit_gap_s"),
        max_gap_from_kill_s=k1.get("max_gap_from_kill_s"),
    )
except Exception as e:  # a missing or unparsable file is recorded, never hidden
    out["error"] = "%s: %s" % (type(e).__name__, e)
with open(os.path.join(cell, "restart-metrics.json"), "w") as f:
    json.dump(out, f, indent=1)
print(json.dumps(out))
