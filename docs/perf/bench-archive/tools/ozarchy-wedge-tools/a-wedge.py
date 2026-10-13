#!/usr/bin/env python3
"""ozarchy-wedge kill-cell analysis (run: python3 -I a-wedge.py PREFIX OUT_JSON CELL_TAG...).

Per kill k (crash-kill{,-k}.json; node log times are UTC; chain-wide = val0, never killed):
  down_s              kill -> restarted process (crash-kill.sh)
  kill_chain_s        kill -> first val0 commit after the RESTART (section 52 / p3term kill->commit definition)
  kill_chain_raw_s    kill -> first val0 commit after the kill (can be a block already voted before the kill)
  kill_node_s         kill -> the restarted node's first 'on_committed_block' line after its restart
  max_gap_s           longest stretch between consecutive val0 commits whose END lies in [kill_k, kill_{k+1}) (last: bench end),
                      the stretch straddling the kill included (crash-freeze.py definition, recomputed here)
  nopeers[v]          'Failed to publish consensus message: NoPeersSubscribedToTopic' lines per validator in [kill_k, kill_{k+1})
  txfail[v], nbfail[v] 'Failed to publish tx:' / 'failed to publish native batch:' lines, same window
  wd[v], stale[v]     torus_mesh_watchdog_disconnects_total / torus_mesh_stale_conn_disconnects_total increase in the window
                      (ckpt-samples.txt, 1 s; the restarted node's process starts at 0)
  sd_n, sd_sum, sd_max_le   torus_consensus_subscribe_delay_seconds count / sum increase over all nodes in the window, and the
                      largest bucket bound that gained an observation (wf only)
  sub_ok_s            kill -> first scrape round from which all 3 validators report torus_consensus_subscribed_validators == 2
                      (after the restart; the restarted node's first ok scrape counts)
  sub_low[v]          seconds in the window with a scrape of subscribed_validators < 2 (per node, ok scrapes only, 1 s each)
"""

import collections
import gzip
import json
import os
import re
import statistics
import sys
from datetime import datetime, timezone

R = "/home/oz/bench-results-matched/"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TSRE = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z")
COMMIT = "on_committed_block: sending to execution pipeline"
PATS = {
    "nopeers": "Failed to publish consensus message: NoPeersSubscribedToTopic",
    "txfail": "Failed to publish tx:",
    "nbfail": "failed to publish native batch:",
}
NETUP = "p2p network started"


def ts(line):
    m = TSRE.match(line)
    if not m:
        return None
    return (
        datetime.strptime(m.group(1)[:26], "%Y-%m-%dT%H:%M:%S.%f")
        .replace(tzinfo=timezone.utc)
        .timestamp()
    )


def scan_log(path):
    """-> commits [(t, h)] first line per height, events {pat: [t]}"""
    commits, seen, ev = [], set(), {k: [] for k in list(PATS) + ["netup"]}
    with gzip.open(path, "rt", errors="replace") as f:
        for line in f:
            if COMMIT not in line and "ublish" not in line and NETUP not in line:
                continue
            line = ANSI.sub("", line)
            t = ts(line)
            if t is None:
                continue
            if COMMIT in line:
                m = re.search(r"height=(\d+)", line)
                if m:
                    h = int(m.group(1))
                    commits.append((t, h, h in seen))
                    seen.add(h)
                continue
            if NETUP in line:
                ev["netup"].append(t)
                continue
            for k, p in PATS.items():
                if p in line:
                    ev[k].append(t)
    return commits, ev


def samples(cell):
    """ckpt-samples.txt -> {val: [(t, {metric: value})]}"""
    out = collections.defaultdict(list)
    cur = None
    p = os.path.join(cell, "ckpt-samples.txt")
    if not os.path.exists(p):
        return out
    for line in open(p, errors="replace"):
        f = line.split()
        if not f:
            continue
        if f[0] == "T" and len(f) == 4:
            cur = None
            if f[3] == "ok":
                cur = {}
                out[f[2]].append((float(f[1]), cur))
        elif cur is not None and len(f) == 2:
            try:
                cur[f[0]] = float(f[1])
            except ValueError:
                pass
    return out


def counter_inc(S, v, name, t0, t1):
    """increase of a counter on node v over [t0, t1); a drop = process restart (new process counts from 0)"""
    prev, inc = None, 0.0
    for t, m in S.get(v, []):
        if name not in m:
            continue
        x = m[name]
        if t < t0:
            prev = x
            continue
        if t >= t1:
            break
        if prev is None:
            prev = (
                x if t < t0 + 0.5 else 0.0
            )  # first sample of a restarted process: counts from 0
        if x >= prev:
            inc += x - prev
        else:
            inc += x
        prev = x
    return inc


def hist_inc(S, t0, t1):
    """subscribe-delay histogram increase summed over nodes: count, sum, largest bucket bound with new observations"""
    n = s = 0.0
    maxle = None
    for v in ("val0", "val1", "val2"):
        n += counter_inc(S, v, "torus_consensus_subscribe_delay_seconds_count", t0, t1)
        s += counter_inc(S, v, "torus_consensus_subscribe_delay_seconds_sum", t0, t1)
        les = set()
        for t, m in S.get(v, []):
            for k in m:
                if k.startswith("torus_consensus_subscribe_delay_seconds_bucket"):
                    les.add(k)
        # per-bucket increase; the bucket that gains observations beyond the next lower bound holds the observations
        bounds = []
        for k in les:
            mm = re.search(r'le="([^"]+)"', k)
            if mm:
                bounds.append(
                    (float("inf") if mm.group(1) == "+Inf" else float(mm.group(1)), k)
                )
        bounds.sort()
        prev_inc = 0.0
        for le, k in bounds:
            inc = counter_inc(S, v, k, t0, t1)
            if inc > prev_inc + 1e-9:
                maxle = le if maxle is None else max(maxle, le)
            prev_inc = inc
    return n, s, maxle


def sub_metrics(S, t_kill, t_up, t1):
    """sub_ok_s: first time >= t_up from which every node's latest ok scrape shows subscribed_validators == 2 (until t1);
    sub_low: per node, ok scrapes in [t_kill, t1) with value < 2"""
    name = "torus_consensus_subscribed_validators"
    ev = []
    for v in ("val0", "val1", "val2"):
        for t, m in S.get(v, []):
            if name in m and t_kill - 3 <= t < t1:
                ev.append((t, v, m[name]))
    ev.sort()
    last = {}
    ok_since = None
    for t, v, x in ev:
        last[v] = x
        allok = len(last) == 3 and all(last[w] >= 2 for w in last)
        if t >= t_up and allok:
            if ok_since is None:
                ok_since = t
        elif t >= t_up:
            ok_since = None
    low = {
        v: sum(1 for t, w, x in ev if w == v and t >= t_kill and x < 2)
        for v in ("val0", "val1", "val2")
    }
    mins = {
        v: min([x for t, w, x in ev if w == v and t >= t_kill] or [None])
        if any(w == v and t >= t_kill for t, w, x in ev)
        else None
        for v in ("val0", "val1", "val2")
    }
    return (round(ok_since - t_kill, 2) if ok_since else None), low, mins


def cell_checks(cell, label, runlog):
    out = {}
    try:
        s = json.load(open(os.path.join(cell, "summary.json")))
        nodes = s["agreement"]["nodes"]
        out["digests_equal"] = len({n["state_digest"] for n in nodes}) == 1
        out["agree"] = s["agreement"].get("agreement_verdict")
        out["block_hash_equal"] = s["agreement"].get("block_hash_equal")
        out["verdict"] = s.get("validity", {}).get("verdict")
        out["matched_s"] = s["headline"]["matched_s_avg"]
        out["blk_s"] = s["headline"]["native_blk_s"]
        out["t_bench1"] = s.get("timing", {}).get("t_bench1")
    except Exception as e:
        out["summary_error"] = str(e)
    try:
        out["rc"] = open(cell + ".cell.rc").read().strip()
    except OSError:
        out["rc"] = None
    out["hard_findings"] = (
        [
            x.strip()
            for x in open(runlog, errors="replace")
            if x.startswith("HARD FINDING " + label + ":")
        ]
        if os.path.exists(runlog)
        else []
    )
    out["check_fail"] = (
        [
            x.strip()
            for x in open(runlog, errors="replace")
            if x.startswith("CHECK FAIL " + label)
        ]
        if os.path.exists(runlog)
        else []
    )
    return out


def analyse(prefix, tag, runlog):
    label = prefix + tag
    cell = R + label
    cc = cell_checks(cell, label, runlog)
    logs = {}
    for v in (0, 1, 2):
        logs["val%d" % v] = scan_log(os.path.join(cell, "val%d.log.gz" % v))
    S = samples(cell)
    kills = []
    for k in range(1, 9):
        kf = os.path.join(cell, "crash-kill%s.json" % ("" if k == 1 else "-%d" % k))
        if not os.path.exists(kf):
            break
        kills.append(json.load(open(kf)))
    t_end = cc.get("t_bench1") or max(t for t, h, d in logs["val0"][0])
    c0 = [(t, h) for t, h, dup in logs["val0"][0] if not dup]
    rows = []
    for i, kj in enumerate(kills):
        t_kill, t_up = kj["kill_ts"], kj["restart_ts"]
        t1 = kills[i + 1]["kill_ts"] if i + 1 < len(kills) else t_end
        kn = kj.get("kill_node") or "val%s" % kj.get("kill_idx")
        r = {
            "cell": tag,
            "arm": tag.split("-")[1],
            "kill": i + 1,
            "kill_node": kn,
            "sig": kj.get("kill_signal"),
            "down_s": kj["down_s"],
            "pre_h": kj["pre_kill"]["block_height"],
        }
        fa = next((t for t, h in c0 if t > t_up), None)
        fr = next((t for t, h in c0 if t > t_kill), None)
        r["kill_chain_s"] = round(fa - t_kill, 2) if fa else None
        r["kill_chain_raw_s"] = round(fr - t_kill, 2) if fr else None
        kc = [(t, h) for t, h, dup in logs[kn][0] if t > t_up]
        r["kill_node_s"] = round(kc[0][0] - t_kill, 2) if kc else None
        r["node_first_h"] = kc[0][1] if kc else None
        gap, gstart = 0.0, None
        prev = None
        for t, h in c0:
            if prev is not None and t_kill <= t < t1 and t - prev > gap:
                gap, gstart = t - prev, prev
            prev = t
        last_before_end = max([t for t, h in c0 if t < t1] or [t_kill])
        if t1 - last_before_end > gap:
            gap, gstart = t1 - last_before_end, last_before_end
        r["max_gap_s"] = round(gap, 2)
        gend = next((t for t, h in c0 if gstart is not None and t > gstart + gap - 1e-6), None)
        r["resume_s"] = round(gend - t_kill, 2) if gend else None  # kill -> the val0 commit that ends the longest gap
        nu = [t for t in logs[kn][1]["netup"] if t > t_up]
        r["netup_s"] = round(nu[0] - t_kill, 2) if nu else None  # kill -> restarted node's p2p network started
        drops = [t for v in ("val0", "val1", "val2") if v != kn for t, m in S.get(v, [])
                 if t_kill <= t < t1 and m.get("torus_consensus_subscribed_validators", 2) < 2]
        r["surv_drop_s"] = round(min(drops) - t_kill, 2) if drops else None  # first survivor scrape with subscribed < 2
        r["max_gap_from_kill_s"] = (
            round(gstart - t_kill, 2) if gstart is not None else None
        )
        for p in PATS:
            r[p] = {v: sum(1 for t in logs[v][1][p] if t_kill <= t < t1) for v in logs}
        r["wd"] = {
            v: counter_inc(S, v, "torus_mesh_watchdog_disconnects_total", t_kill, t1)
            for v in ("val0", "val1", "val2")
        }
        r["stale"] = {
            v: counter_inc(S, v, "torus_mesh_stale_conn_disconnects_total", t_kill, t1)
            for v in ("val0", "val1", "val2")
        }
        n, s, mle = hist_inc(S, t_kill, t1)
        r["sd_n"], r["sd_sum"], r["sd_max_le"] = n, round(s, 3), mle
        r["sub_ok_s"], r["sub_low"], r["sub_min"] = sub_metrics(S, t_kill, t_up, t1)
        rows.append(r)
    # cell-wide totals (whole log, incl. pre-kill)
    cc["totals"] = {p: {v: len(logs[v][1][p]) for v in logs} for p in PATS}
    cc["scrapes_ok"] = {v: len(S.get(v, [])) for v in ("val0", "val1", "val2")}
    return cc, rows


def main():
    prefix, outj, runlog = sys.argv[1], sys.argv[2], sys.argv[3]
    allrows, cells = [], {}
    keys = ["down_s", "netup_s", "surv_drop_s", "resume_s", "max_gap_s", "max_gap_from_kill_s", "kill_chain_s", "kill_chain_raw_s",
            "kill_node_s", "node_first_h", "nopeers", "txfail", "nbfail", "wd", "stale", "sd_n", "sd_sum", "sd_max_le", "sub_ok_s",
            "sub_low", "sub_min"]
    for tag in sys.argv[4:]:
        cc, rows = analyse(prefix, tag, runlog)
        cells[tag] = cc
        print("CELL %s %s" % (tag, json.dumps({k: v for k, v in cc.items() if k != "t_bench1"})))
        for r in rows:
            allrows.append(r)
            print("KILL %s k%d %s %s" % (r["cell"], r["kill"], r["kill_node"], r["sig"]),
                  " ".join("%s=%s" % (k, json.dumps(r[k], separators=(",", ":"))) for k in keys))
    json.dump({"cells": cells, "kills": allrows}, open(outj, "w"), indent=1)
    g = collections.defaultdict(list)
    for r in allrows:
        g[r["arm"]].append(r)

    def mmm(rs, k):
        v = [r[k] for r in rs if r.get(k) is not None]
        return "%.2f/%.2f/%.2f/%.2f" % (statistics.mean(v), statistics.median(v), min(v), max(v)) if v else "-"

    print("\nAGG (mean/median/min/max)")
    for arm in sorted(g):
        rs = g[arm]
        print("AGG %s n=%d resume %s | maxgap %s | kill->node %s | netup %s | surv_drop %s | sub_ok %s | kill->chain(p3term) %s"
              % (arm, len(rs), mmm(rs, "resume_s"), mmm(rs, "max_gap_s"), mmm(rs, "kill_node_s"), mmm(rs, "netup_s"),
                 mmm(rs, "surv_drop_s"), mmm(rs, "sub_ok_s"), mmm(rs, "kill_chain_s")))
        tot = lambda k: int(sum(sum(r[k].values()) for r in rs))  # noqa: E731
        print("AGG %s nopeers=%d (kills with >0: %d/%d; per node %s) txfail=%d nbfail=%d wd=%d stale=%d sd_n=%d sd_sum=%.2f sd_max_le=%s gap>15s=%d gap>30s=%d"
              % (arm, tot("nopeers"), sum(1 for r in rs if sum(r["nopeers"].values()) > 0), len(rs),
                 {v: sum(r["nopeers"][v] for r in rs) for v in ("val0", "val1", "val2")}, tot("txfail"), tot("nbfail"), tot("wd"), tot("stale"),
                 sum(r["sd_n"] for r in rs), sum(r["sd_sum"] for r in rs), max([r["sd_max_le"] for r in rs if r["sd_max_le"] is not None] or [None]) if any(r["sd_max_le"] is not None for r in rs) else None,
                 sum(1 for r in rs if r["max_gap_s"] > 15), sum(1 for r in rs if r["max_gap_s"] > 30)))
        for kk in sorted({r["kill"] for r in rs}):
            sub = [r for r in rs if r["kill"] == kk]
            print("  AGG %s kill%d n=%d resume %s maxgap %s netup %s" % (arm, kk, len(sub), mmm(sub, "resume_s"), mmm(sub, "max_gap_s"), mmm(sub, "netup_s")))
        for kn in ("val1", "val2"):
            sub = [r for r in rs if r["kill_node"] == kn]
            if sub:
                print("  AGG %s %s n=%d resume %s maxgap %s" % (arm, kn, len(sub), mmm(sub, "resume_s"), mmm(sub, "max_gap_s")))


main()
