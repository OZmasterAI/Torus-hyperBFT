#!/usr/bin/env python3
"""ozarchy-p3term (copy of ozarchy-p3r1 a-restart.py + SIGTERM shutdown phases) per-kill restart metrics + per-cell correctness (run: python3 -I a-restart.py PREFIX CELL_TAG...).

Per kill k (crash-kill{,-k}.json, crash-restart-tail{,-k}.log of val1; node log times are UTC, epoch from crash-kill):
  down_s            kill -> restart (crash-kill.sh; process exit + 1 s poll granularity)
  db_open_s         restart -> 'state database opened'
  C / A / gap       'crash recovery: execution gap detected' committed_height / applied_height (= durable marker) / gap;
                    no gap line -> A = C = the 'flush worker attached after replay applied=' value, gap 0
  replay_s, blk/s   gap line -> 'exec pipeline ENABLED' line
  first_replayed    first 'block done height=' after the gap line (must be A + 1); contiguous = every height A+1..C seen once
  marker_lo         val1 torus_state_hash_height in the last 1 s sample before the kill (durable running-hash height):
                    A >= marker_lo is required (the replay never starts below what was already durable)
  up_commit_s       restart -> val1's first 'on_committed_block: sending' (as p3p section 42.6)
  chain_up_commit_s restart -> val0's first commit after the restart (crash-freeze.py, val0 log); kill_commit_s = down + it
  rejoin_s          restart -> first 1 s round where val1 applied (block_height - exec_queue_depth) >= min(val0, val2)
                    applied - 2 AND val1 block_height >= min(val0, val2) block_height - 2 (both after val1's first commit)
SIGTERM additions (219f0c20 main.rs log lines, val1 log after the kill):
  sig_s / drained_s / flushed_s / complete_s   kill -> 'shutdown signal received' / 'consensus stopped, execution queue drained'
                    / 'memtables flushed, WAL synced' / 'shutdown complete' (elapsed_ms also parsed)
  exit_s            kill -> process exit (patched crash-kill.sh, 0.1 s polling); exit_forced = still alive after KILL_WAIT
  timeout           'shutdown did not finish within the limit' / 'shutdown failed' / 'second signal' lines (flags)
  over10            exit_s > 10 s (Docker default stop grace)
Per cell: summary AGREE, state digests equal, state-hash checkpoints compared (state-hashes.txt), HARD FINDING lines.
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


def ts(line):
    m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z", line)
    if not m:
        return None
    return (
        datetime.strptime(m.group(1)[:26], "%Y-%m-%dT%H:%M:%S.%f")
        .replace(tzinfo=timezone.utc)
        .timestamp()
    )


def samples(cell):
    """ckpt-samples.txt -> list of (ts, val, {metric: value})"""
    out, cur = [], None
    p = os.path.join(cell, "ckpt-samples.txt")
    if not os.path.exists(p):
        return out
    for line in open(p, errors="replace"):
        f = line.split()
        if not f:
            continue
        if f[0] == "T" and len(f) == 4:
            cur = (float(f[1]), f[2], {}) if f[3] == "ok" else None
            if cur:
                out.append(cur)
        elif (
            cur
            and len(f) == 2
            and f[0]
            in (
                "torus_block_height",
                "torus_exec_queue_depth",
                "torus_state_hash_height",
            )
        ):
            cur[2][f[0]] = float(f[1])
    return out


def rejoin(S, t_up):
    """first val1 sample after t_up whose applied/committed heights are within 2 of the others' (latest samples)"""
    last = {}
    for t, v, m in S:
        if "torus_block_height" not in m or "torus_exec_queue_depth" not in m:
            continue
        last[v] = (
            t,
            m["torus_block_height"],
            m["torus_block_height"] - m["torus_exec_queue_depth"],
        )
        if v != "val1" or t < t_up or "val0" not in last or "val2" not in last:
            continue
        o_bh = min(last["val0"][1], last["val2"][1])
        o_ap = min(last["val0"][2], last["val2"][2])
        _, bh, ap = last["val1"]
        if bh >= o_bh - 2 and ap >= o_ap - 2:
            return round(t - t_up, 2), int(bh), int(ap), int(o_bh), int(o_ap)
    return None, None, None, None, None


def marker_lo(S, t_kill):
    v = None
    for t, n, m in S:
        if n == "val1" and t < t_kill and "torus_state_hash_height" in m:
            v = int(m["torus_state_hash_height"])
    return v


def kill_rows(cell):
    S = samples(cell)
    fr = {}
    try:
        for k in json.load(open(os.path.join(cell, "crash-freeze.json")))["kills"]:
            fr[k["kill_seq"]] = k
    except Exception as e:  # recorded in the row, never hidden
        fr["error"] = str(e)
    rows = []
    for k in range(1, 9):
        suf = "" if k == 1 else "-%d" % k
        kf = os.path.join(cell, "crash-kill%s.json" % suf)
        tf = os.path.join(cell, "crash-restart-tail%s.log" % suf)
        if not os.path.exists(kf):
            break
        kj = json.load(open(kf))
        t_up, t_kill = kj["restart_ts"], kj["kill_ts"]
        L = [ANSI.sub("", x) for x in open(tf, errors="replace")]

        def first(pat, after):
            for x in L:
                if pat in x:
                    t = ts(x)
                    if t and t >= after:
                        return t, x
            return None, None

        r = {
            "kill": k,
            "sig": kj.get("kill_signal"),
            "kill_ts": t_kill,
            "down_s": kj["down_s"],
            "pre_h": kj["pre_kill"]["block_height"],
            "pre_q": kj["pre_kill"]["exec_queue_depth"],
            "pre_w": kj["pre_kill"]["flush_worker_depth"],
        }
        to, _ = first("state database opened", t_up - 1)
        tk, _ = first("loaded node key", t_up - 1)
        tg, lg = first("execution gap detected", t_up - 1)
        te, le = first("exec pipeline ENABLED", t_up - 1)
        tc, lc = first("on_committed_block: sending to execution pipeline", t_up - 1)
        tsd, _ = first("shutdown signal received", t_kill - 1)
        r["shutdown_line"] = tsd is not None
        tdr, _ = first("consensus stopped, execution queue drained", t_kill - 1)
        tfl, _ = first("memtables flushed, WAL synced", t_kill - 1)
        tcm, lcm = first("shutdown complete", t_kill - 1)
        bad_stop = [x.strip()[:200] for x in L if ("did not finish within the limit" in x or "shutdown failed" in x
                                                   or "second signal" in x or "shutdown interrupted" in x)]
        rel = lambda t: round(t - t_kill, 3) if t and t < t_up + 0.5 else None  # noqa: E731
        r.update(sig_s=rel(tsd), drained_s=rel(tdr), flushed_s=rel(tfl), complete_s=rel(tcm),
                 elapsed_ms=int(re.search(r"elapsed_ms=(\d+)", lcm).group(1)) if lcm and re.search(r"elapsed_ms=(\d+)", lcm) else None,
                 exit_s=kj.get("exit_s"), exit_forced=kj.get("exit_forced"), stop_errors=bad_stop)
        r["drain_s"] = round(tdr - tsd, 3) if tdr and tsd else None
        r["flush_s"] = round(tfl - tdr, 3) if tfl and tdr else None
        r["over10"] = (kj.get("exit_s") or 0) > 10
        r["db_open_s"] = round(to - t_up, 2) if to else None
        r["keyload_open_s"] = round(to - tk, 2) if to and tk else None
        en_applied = int(re.search(r"applied=(\d+)", le).group(1)) if le else None
        if lg:
            C = int(re.search(r"committed_height=(\d+)", lg).group(1))
            A = int(re.search(r"applied_height=(\d+)", lg).group(1))
            gap = int(re.search(r"gap=(\d+)", lg).group(1))
        else:
            C = A = en_applied
            gap = 0
        r.update(C=C, A=A, gap=gap, enabled_applied=en_applied)
        r["replay_s"] = round(te - tg, 2) if tg and te else (0.0 if te else None)
        r["blk_s"] = round(gap / (te - tg), 2) if tg and te and gap else None
        # replayed heights: block done lines between the gap line and the ENABLED line
        done = []
        if tg and te:
            for x in L:
                t = ts(x)
                if t and tg <= t <= te + 0.001 and "block done" in x:
                    m = re.search(r"height=(\d+)", x)
                    if m:
                        done.append(int(m.group(1)))
        r["first_replayed"] = done[0] if done else None
        r["replayed_n"] = len(done)
        r["replay_contiguous"] = (done == list(range(A + 1, C + 1))) if gap else True
        r["marker_lo"] = marker_lo(S, t_kill)
        r["up_commit_s"] = round(tc - t_up, 2) if tc else None
        r["first_commit_h"] = (
            int(re.search(r"height=(\d+)", lc).group(1)) if lc else None
        )
        f = fr.get(k, {})
        r["chain_up_commit_s"] = f.get("restart_to_first_commit_s")
        r["kill_commit_s"] = (
            round(kj["down_s"] + f["restart_to_first_commit_s"], 2)
            if f.get("restart_to_first_commit_s") is not None
            else None
        )
        r["max_commit_gap_s"] = f.get("max_commit_gap_s")
        rj, bh, ap, obh, oap = rejoin(S, t_up)
        r.update(
            rejoin_s=rj,
            rejoin_v1_bh=bh,
            rejoin_v1_applied=ap,
            rejoin_others_bh=obh,
            rejoin_others_applied=oap,
        )
        # correctness flags for this kill
        bad = []
        if A is None or C is None:
            bad.append("no marker/committed found")
        else:
            if A > C:
                bad.append("A > C")
            if gap and r["first_replayed"] != A + 1:
                bad.append("first replayed %s != A+1 %d" % (r["first_replayed"], A + 1))
            if not r["replay_contiguous"]:
                bad.append("replay not contiguous (%d heights)" % len(done))
            if en_applied is not None and en_applied != C:
                bad.append("ENABLED applied %s != C %s" % (en_applied, C))
            if r["marker_lo"] is not None and A < r["marker_lo"]:
                bad.append(
                    "A %d below durable hash height %d sampled before the kill"
                    % (A, r["marker_lo"])
                )
        if r.get("exit_forced"):
            bad.append("stop still running at KILL_WAIT (forced SIGKILL)")
        if r.get("stop_errors"):
            bad.append("stop error lines: %s" % r["stop_errors"][:2])
        if r.get("sig") == "TERM" and gap:
            bad.append("graceful stop but replay gap %d (marker A %s vs committed C %s)" % (gap, A, C))
        r["flags"] = bad
        rows.append(r)
    return rows


def cell_checks(cell, label):
    out = {}
    try:
        s = json.load(open(os.path.join(cell, "summary.json")))
        nodes = s["agreement"]["nodes"]
        out["digests_equal"] = len({n["state_digest"] for n in nodes}) == 1
        out["agree"] = s["agreement"].get("agreement_verdict")
        out["block_hash_equal"] = s["agreement"].get("block_hash_equal")
        out["digest_quiescent"] = s["agreement"].get("state_digest_quiescent")
        out["digest_heights"] = [n.get("digest_height") for n in nodes]
        out["matched_s"] = s["headline"]["matched_s_avg"]
        out["blk_s"] = s["headline"]["native_blk_s"]
    except Exception as e:
        out["summary_error"] = str(e)
    d = collections.defaultdict(dict)
    p = os.path.join(cell, "state-hashes.txt")
    if os.path.exists(p):
        for x in open(p):
            f = x.split(" ", 3)
            try:
                j = json.loads(f[3])
            except Exception:
                continue
            if isinstance(j, dict) and "localHash" in j:
                d[int(f[2][2:])][f[1]] = j["localHash"]
    full = [h for h in d if len(d[h]) == 3]
    out["sh_heights"] = len(d)
    out["sh_with3"] = len(full)
    out["sh_disagree"] = sorted(h for h in full if len(set(d[h].values())) > 1)
    out["sh_max"] = max(d) if d else None
    # p3gate archive copy: was R + "ozarchy-p3term-run/campaign.log" (HARD FINDING lines of the wrong campaign; section 61)
    log = R + "ozarchy-p3gate-run/campaign.log"
    out["hard_findings"] = (
        [
            x.strip()
            for x in open(log, errors="replace")
            if x.startswith("HARD FINDING " + label + ":")
        ]
        if os.path.exists(log)
        else []
    )
    return out


def main():
    prefix = sys.argv[1]
    allrows = []
    for tag in sys.argv[2:]:
        label = prefix + tag
        cell = R + label
        if not os.path.isdir(cell):
            print("MISSING", label)
            continue
        arm = tag.split("-")[1] if tag.count("-") >= 2 else "?"
        cc = cell_checks(cell, label)
        print("CELL %s %s" % (tag, json.dumps(cc)))
        for r in kill_rows(cell):
            r.update(cell=tag, arm=arm)
            allrows.append(r)
            print(
                "KILL %s k%d %s down=%.2f pre_h=%s q=%s | C=%s A=%s gap=%s marker_lo=%s first=%s n=%s contig=%s | dbopen=%s replay=%s (%s blk/s) | up->commit(v1)=%s h=%s | up->commit(chain)=%s kill->commit=%s | rejoin=%s | shutdown_line=%s | exit=%s forced=%s sig/drained/flushed/complete=%s/%s/%s/%s elapsed_ms=%s over10=%s | flags=%s"
                % (
                    tag,
                    r["kill"],
                    r["sig"],
                    r["down_s"],
                    r["pre_h"],
                    r["pre_q"],
                    r["C"],
                    r["A"],
                    r["gap"],
                    r["marker_lo"],
                    r["first_replayed"],
                    r["replayed_n"],
                    r["replay_contiguous"],
                    r["db_open_s"],
                    r["replay_s"],
                    r["blk_s"],
                    r["up_commit_s"],
                    r["first_commit_h"],
                    r["chain_up_commit_s"],
                    r["kill_commit_s"],
                    r["rejoin_s"],
                    r["shutdown_line"],
                    r["exit_s"], r["exit_forced"], r["sig_s"], r["drained_s"], r["flushed_s"], r["complete_s"], r["elapsed_ms"], r["over10"],
                    r["flags"],
                )
            )
    # p3gate archive copy: was R + "ozarchy-p3term-run/kills.json", which overwrote p3term's file (section 61)
    json.dump(allrows, open(R + "ozarchy-p3gate-run/analyst/kills.json", "w"), indent=1)
    print(
        "\nAGG arm sig n | down mean/max | dbopen | gap mean/max | replay_s | up->commit(v1) | up->commit(chain) mean/max | kill->commit mean/max | rejoin mean/max | exit mean/max | drain mean/max | flush mean/max | exit p95 | over10"
    )
    g = collections.defaultdict(list)
    for r in allrows:
        g[(r["arm"], r["sig"])].append(r)

    def mm(rs, k):
        v = [r[k] for r in rs if r.get(k) is not None]
        return "%.2f/%.2f" % (statistics.mean(v), max(v)) if v else "-"

    for key in sorted(g):
        rs = g[key]
        print(
            "AGG",
            key[0],
            key[1],
            len(rs),
            mm(rs, "down_s"),
            mm(rs, "db_open_s"),
            mm(rs, "gap"),
            mm(rs, "replay_s"),
            mm(rs, "up_commit_s"),
            mm(rs, "chain_up_commit_s"),
            mm(rs, "kill_commit_s"),
            mm(rs, "rejoin_s"),
            mm(rs, "exit_s"), mm(rs, "drain_s"), mm(rs, "flush_s"),
            "%.2f" % sorted(r["exit_s"] for r in rs if r.get("exit_s") is not None)[max(0, int(round(0.95 * (len(rs) - 1))))] if any(r.get("exit_s") is not None for r in rs) else "-",
            sum(1 for r in rs if r.get("over10")),
        )


main()
