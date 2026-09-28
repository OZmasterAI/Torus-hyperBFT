#!/usr/bin/env python3
"""crash-freeze.py — per-kill chain freeze of a CRASH_KILL_AT_S cell (s75).

    crash-freeze.py <cell-results-dir> [--json]

Kill 1 = crash.json, kill k>=2 = crash-<k>.json (the bare crash-kill[-<k>].json
record when the post-cell scan is missing). Kill k's window is
[kill_ts_k, kill_ts_{k+1}); the last one ends at bench end (summary.json
timing.t_bench1). Per kill:

  down_s                     SIGKILL -> restarted process (crash-kill.sh)
  restart_to_first_commit_s  restart -> first val0 commit after it (None: none
                             inside the window)
  max_commit_gap_s           longest stretch without a val0 commit whose END
                             lies in the window, measured from the commit
                             before it — so the stretch that straddles the kill
                             is kill k's (n=3: the chain stalls while val1 is
                             down). A stretch still open at the window end
                             counts up to the window end.
  max_gap_from_kill_s        where that stretch began, relative to the kill

Commit times, best resolution first: val0.log.gz `on_committed_block: sending
to execution pipeline height=N` (microseconds; first line of each new height),
else sampler.csv val0 torus_blocks_committed_total (1 Hz integer ts — the
source s74 analyze.py's `freeze` uses, so the two agree to within ~1 s; that
counter also lags the log line by ~1-2 s, so restart_to_first_commit_s is only
trustworthy from the log). s74-rv-off-r1: log 35.94 s, sampler/analyze.py 35 s.
"""

import csv, gzip, json, os, re, statistics, sys
from datetime import datetime

COMMIT = "on_committed_block: sending to execution pipeline"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
LINE = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z.*?\bheight=(\d+)")


def load_json(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def kills(cell):
    seqs = {1}
    for name in os.listdir(cell):
        m = re.fullmatch(r"crash(?:-kill)?-(\d+)\.json", name)
        if m and int(m.group(1)) >= 2:
            seqs.add(int(m.group(1)))
    out = []
    for k in sorted(seqs):
        names = ("crash.json", "crash-kill.json") if k == 1 else (
            "crash-%d.json" % k, "crash-kill-%d.json" % k)
        rec = next((r for r in (load_json(os.path.join(cell, n)) for n in names) if r), None)
        if rec and rec.get("kill_ts"):
            out.append(dict(rec, kill_seq=rec.get("kill_seq") or k))
    return out


def log_commits(path):
    commits, top = [], -1
    with gzip.open(path, "rt", errors="replace") as f:
        for line in f:
            if COMMIT not in line:
                continue
            m = LINE.match(ANSI.sub("", line))
            if not m or int(m.group(2)) <= top:
                continue
            top = int(m.group(2))
            commits.append(datetime.fromisoformat(m.group(1) + "+00:00").timestamp())
    return commits


def sampler_commits(path):
    commits, top = [], None
    with open(path) as f:
        for row in csv.DictReader(f):
            if row["node"] != "val0" or row.get("scrape_valid", "1") != "1":
                continue
            c = int(float(row["torus_blocks_committed_total"]))
            if top is None or c > top:
                if top is not None:
                    commits.append(float(row["ts"]))
                top = c
    return commits


def freeze(k, end, commits, res):
    kt, rt = k["kill_ts"], k.get("restart_ts")
    before = [t for t in commits if t < kt]
    inside = [t for t in commits if kt <= t < end]
    best, at, prev = 0.0, None, before[-1] if before else None
    for t in inside + [end]:
        if prev is not None and t - prev > best:
            best, at = t - prev, prev - kt
        prev = t
    # a commit observed at t happened in (t - res, t]: only one that is
    # certainly after the restart counts (matters at 1 Hz, not for the log)
    first = next((t for t in inside if rt is not None and t - res >= rt), None)
    r3 = lambda v: None if v is None else round(v, 3)
    return {
        "kill_seq": k["kill_seq"], "kill_at_s": k.get("kill_at_s"),
        "kill_ts": kt, "restart_ts": rt, "window_end_ts": end,
        "down_s": k.get("down_s"),
        "restart_to_first_commit_s": r3(first - rt) if first is not None else None,
        "max_commit_gap_s": r3(best), "max_gap_from_kill_s": r3(at),
        "commits_in_window": len(inside),
    }


def main():
    args = [a for a in sys.argv[1:] if a != "--json"]
    if len(args) != 1:
        sys.exit(__doc__.split("\n\n")[1])
    cell = args[0]
    ks = kills(cell)
    if not ks:
        sys.exit("crash-freeze: no crash.json / crash-kill.json with kill_ts in %s" % cell)
    if os.path.exists(os.path.join(cell, "val0.log.gz")):
        source, res, commits = "val0.log.gz", 1e-6, log_commits(os.path.join(cell, "val0.log.gz"))
    else:
        source, res, commits = "sampler.csv", 1.0, sampler_commits(os.path.join(cell, "sampler.csv"))
    t1 = ((load_json(os.path.join(cell, "summary.json")) or {}).get("timing") or {}).get("t_bench1")
    bench_end = float(t1) if t1 else (commits[-1] if commits else ks[-1]["kill_ts"])
    ends = [k["kill_ts"] for k in ks[1:]] + [bench_end]
    rows = [freeze(k, e, commits, res) for k, e in zip(ks, ends)]
    mean = lambda f: (round(statistics.mean(r[f] for r in rows if r[f] is not None), 3)
                      if any(r[f] is not None for r in rows) else None)
    out = {"cell": cell, "source": source, "resolution_s": res, "bench_end_ts": bench_end,
           "kills": rows, "mean_max_commit_gap_s": mean("max_commit_gap_s"),
           "mean_restart_to_first_commit_s": mean("restart_to_first_commit_s")}
    if "--json" in sys.argv:
        json.dump(out, sys.stdout, indent=1)
        print()
        return
    cols = ["kill_seq", "kill_at_s", "down_s", "restart_to_first_commit_s",
            "max_commit_gap_s", "max_gap_from_kill_s", "commits_in_window"]
    print("\t".join(cols))
    for r in rows:
        print("\t".join(str(r[c]) for c in cols))
    print("# source=%s resolution=%ss kills=%d mean_max_commit_gap_s=%s "
          "mean_restart_to_first_commit_s=%s" % (source, res, len(rows),
                                                 out["mean_max_commit_gap_s"],
                                                 out["mean_restart_to_first_commit_s"]))


if __name__ == "__main__":
    main()
