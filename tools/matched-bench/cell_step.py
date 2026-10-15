#!/usr/bin/env python3
"""cell_step.py — the per-cell write-up step of a matched-bench campaign (python3 stdlib, run with -I).

The campaign calls it (through cell-step.sh's `cell_step`) right after each cell, in the
foreground, before the next cell starts. It validates the finished cell, runs the
campaign's archived analysis on it, and appends one row to <RUN_DIR>/cells.tsv.

  cell_step.py record --run-dir RUN --cell-dir DIR --label LABEL --tag TAG --arm ARM --rep REP
                      --staged-md5 MD5 [--spec SPEC] [--rc-file F] [--campaign-log F]
                      [--analysis-cmd TEMPLATE] [--analysis-key TEMPLATE] [--analysis-metric KEY]
                      [--analysis-timeout S] [--extra-reasons R1,R2] [--no-summary-md5]
  cell_step.py pending-reruns --run-dir RUN

cells.tsv (fixed contract): header written once, then one row per finished cell,
tab-separated:  label arm rep rc agree md5_ok metric flag
  rc      the cell wrapper's exit code from <label>.cell.rc ("missing" if there is none)
  agree   yes iff summary.json validity.verdict == ACCEPT and agreement_verdict == AGREE (the RAW
          result: a known-artifact cell has agree=no and flag=ok)
  md5_ok  yes iff every recorded node binary md5 of the cell equals the staged md5 of its arm:
          each running process's exe_md5 (node-environ-trie.txt, 3 nodes; node-restart-env.txt,
          a restarted node) — at least one process record is required — and summary.json
          binaries.torus_node_md5 (full) when present (optional: a missing one is skipped; not checked with
          --no-summary-md5: run-cell.sh hashes the STAGED file, a wrapper script on wrapper arms)
  metric  matched/s: the analysis row's --analysis-metric value, else summary.json
          headline.matched_s_avg, else 0 (no summary); plain number
  flag    ok | bad:<reason>[,<reason>...]   reasons, in this order:
          md5, no-summary, death (node-death.txt), rc-missing | rc=<n>, disagree |
          agreement-<verdict>, verdict-<verdict> (rc 0 but not ACCEPT), the --extra-reasons (the campaign's
          own failures of the cell: a failed hard check "check", a STOP site's reason), analysis |
          analysis-timeout (cell-step.sh writes bad:step-timeout / bad:step-error itself when the step
          ends without a row)

Known artifacts (stay ok; named on stdout and in the evidence json):
  crash-gate-info-failstop-line  restart cell, rc 2: the crash gate counted the restarted node's INFO
          config echo 'running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false' as a fail-stop.
          Excused only when EVERY panic/fail-stop pattern line (crash-kill.sh's pattern) in the
          crash-restart-tail*.log files is that INFO line.
  mark-only-digest               DISAGREE whose only evidence is a state digest difference in the 5 mark
          fields (accountValue freeMargin maintenanceMarginUsed totalMarginUsed withdrawable) on the
          differing node(s), with block hash, header state root and survivor counters equal, no
          panic lines and height spread <= 5 (state-digest-val*.txt, same classification as a-cells.py).
  restart-liveness-unverified    a restart cell's 'liveness unverified' (validity.unverified_reasons).
  A non-zero rc is excused only when it is 2 (health.py accept: not ACCEPT) and every validity fail /
  unverified reason is excused by the above.

Bad cell: the row is appended with flag bad:<reasons>, the line "CELL-FLAG <label> <reasons>" is appended
to the campaign log (O_APPEND; default RUN/campaign.log, the driver's own detach.sh log), and
"<label>-rerun" is queued in RUN/rerun-queue.tsv (rerun_label label arm rep spec) for the campaign's
end. A label ending in "-rerun" is never requeued (a bad rerun is flagged, and stays flagged).
Rows are never rewritten or deleted.

Analysis check: --analysis-cmd is the campaign's archived analysis for one cell, e.g.
  "python3 -I $ANALYST/a-cells.py {out} {prefix} {tag}:{arm}"
Placeholders: {out} (RUN/cell-step/<label>.analysis.json), {prefix} (cell dir minus the tag, a-cells.py's
PREFIX), {tag} {arm} {label} {cell} {run}. It must exit 0 within --analysis-timeout (default 600 s) and
write a JSON object holding the key --analysis-key (default "{tag}"); with --analysis-metric, that row
must carry a finite number under it. Anything else (an unparsable command, e.g. unbalanced quotes,
included) flags bad:analysis (timeout: bad:analysis-timeout;
the analysis process group is SIGKILLed). Output: RUN/cell-step/<label>.analysis.log. Policy: an
analysis failure is a BAD cell (rerun queued): an end-of-campaign table must not silently lose the cell.
If the analysis script itself is broken every cell flags bad:analysis — fix the script and re-run the
analysis by hand; the reruns can be skipped by emptying the queue before the campaign's end.

Idempotency: a label that already has a row is refused (exit 3, "CELL-STEP DUPLICATE <label> ..." in the
campaign log, nothing written). A rerun is a different label (<label>-rerun) and gets its own row.

Exit: 0 ok row, 1 bad row, 2 usage / foreign cells.tsv header, 3 duplicate, 5 unexpected error (traceback on
stderr; cell-step.sh then writes the bad:step-error row), 124 terminated (SIGTERM: the analysis child is
killed first; cell-step.sh then writes the bad:step-timeout row).
"""

import argparse
import fcntl
import glob
import json
import math
import os
import re
import shlex
import signal
import subprocess
import sys
import time
import traceback

HEADER = ("label", "arm", "rep", "rc", "agree", "md5_ok", "metric", "flag")
QUEUE = "rerun-queue.tsv"
TSV = "cells.tsv"
V = ("val0", "val1", "val2")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
# crash-kill.sh crash_scan_restart_tail: panics=$(cnt 'panicked|FAIL-STOP|fail-stop|conflicting blocks')
PANIC_RE = re.compile(r"panicked|FAIL-STOP|fail-stop|conflicting blocks")
BENIGN_FAILSTOP_RE = re.compile(
    r" INFO \S+: running state hash fail-stop \(TORUS_STATE_HASH_FAILSTOP\) on=false\s*$"
)
MARK = {
    "accountValue",
    "freeMargin",
    "maintenanceMarginUsed",
    "totalMarginUsed",
    "withdrawable",
}
CASH = {"cashBalance", "nativeBalance", "evmBalance", "permanentStake"}
MD5_RE = re.compile(r"exe_md5=(\S*)")

CHILD = None  # the running analysis process (own session), killed on SIGTERM


def on_term(signum, _frame):
    if CHILD is not None and CHILD.poll() is None:
        try:
            os.killpg(CHILD.pid, signal.SIGKILL)
        except OSError:
            pass
    print(
        "[cell-step] terminated by signal %d (analysis child killed)" % signum,
        flush=True,
    )
    os._exit(124)


def die(code, msg):
    print("cell_step.py: " + msg, file=sys.stderr)
    sys.exit(code)


def slug(s):
    return re.sub(r"[^a-z0-9]+", "-", str(s).lower()).strip("-") or "none"


def fmt_metric(x):
    try:
        x = float(x)
    except (TypeError, ValueError):
        return "0"
    if not math.isfinite(x) or x < 0:
        return "0"
    if x.is_integer():
        return str(int(x))
    return ("%.6f" % x).rstrip("0").rstrip(".")


def read_rc(path):
    try:
        txt = open(path).read().strip()
    except OSError:
        return "missing", None
    m = re.fullmatch(r"rc=(-?\d+)", txt)
    if m:
        return m.group(1), int(m.group(1))
    return slug(txt.replace("rc=", "")) or "unparsable", None


def load_json(path):
    try:
        with open(path) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return None


# ---------------------------------------------------------------- md5


def md5_check(cell, summary, staged, use_summary=True):
    staged = staged.strip().lower()
    ev = {"staged": staged, "records": [], "problems": []}
    if not re.fullmatch(r"[0-9a-f]{8,32}", staged):
        ev["problems"].append("staged md5 %r is not 8-32 hex digits" % staged)
        return False, ev

    def same(v):
        v = v.strip().lower()
        n = min(len(v), len(staged))
        return bool(re.fullmatch(r"[0-9a-f]{8,32}", v)) and v[:n] == staged[:n]

    procs = 0
    for fn in ("node-environ-trie.txt", "node-restart-env.txt"):
        try:
            lines = open(os.path.join(cell, fn), errors="replace").read().splitlines()
        except OSError:
            continue
        for ln in lines:
            m = MD5_RE.search(ln)
            if not m:
                continue
            procs += fn == "node-environ-trie.txt"
            ev["records"].append([fn, m.group(1)])
            if not same(m.group(1)):
                ev["problems"].append("%s exe_md5=%s" % (fn, m.group(1)))
    if procs == 0:
        ev["problems"].append("no running-process md5 record (node-environ-trie.txt)")
    b = ((summary or {}).get("binaries") or {}).get("torus_node_md5")
    if b and not use_summary:
        ev["records"].append(["summary.json binaries.torus_node_md5 (not checked: --no-summary-md5)", b])
    elif b:
        ev["records"].append(["summary.json binaries.torus_node_md5", b])
        if not same(b):
            ev["problems"].append("summary.json torus_node_md5=%s" % b)
    return not ev["problems"], ev


# ---------------------------------------------------------------- known artifacts


def restart_tail_benign(cell):
    """True iff the restart tails exist and every panic/fail-stop pattern line is the INFO config echo."""
    files = sorted(f for f in glob.glob(os.path.join(cell, "crash-restart-tail*.log")))
    if not files:
        return False, "no crash-restart-tail*.log"
    benign, other = 0, []
    for f in files:
        with open(f, errors="replace") as fh:
            for ln in fh:
                ln = ANSI.sub("", ln.rstrip("\n"))
                if not PANIC_RE.search(ln):
                    continue
                if BENIGN_FAILSTOP_RE.search(ln):
                    benign += 1
                else:
                    other.append(ln[:240])
    if other:
        return False, "real panic/fail-stop line(s): %s" % other[:2]
    if benign == 0:
        return (
            False,
            "no panic/fail-stop line in the restart tail (gate evidence inconsistent)",
        )
    return (
        True,
        "%d INFO 'state hash fail-stop ... on=false' line(s), nothing else" % benign,
    )


def classify(a, b):
    """a-cells.py classify(): differing digest lines -> (n lines, fields, class)."""
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
        if not isinstance(ox, dict) or not isinstance(oy, dict):
            kinds.add("nonjson")
        elif "asks" in ox or "bids" in ox:
            kinds.add("book")
            fields.add("book")
        elif "longOi" in ox:
            kinds.add("oi")
            fields.add("oi")
        else:
            for k in set(ox) | set(oy):
                if ox.get(k) != oy.get(k):
                    fields.add(k)
                    kinds.add(
                        "mark" if k in MARK else ("cash" if k in CASH else "other")
                    )
    if kinds == {"mark"}:
        cls = "mark-only"
    elif kinds - {"mark"}:
        cls = "HARD:" + ",".join(sorted(kinds - {"mark"}))
    else:
        cls = "none"
    return nl, sorted(fields), cls


def mark_only_digest(cell, summary):
    a = (summary or {}).get("agreement") or {}
    need = {
        "block_hash_equal": True,
        "header_state_root_equal": True,
        "counters_equal": True,
    }
    for k, want in need.items():
        if a.get(k) is not want:
            return False, "agreement.%s=%r" % (k, a.get(k))
    if a.get("state_digest_equal") is not False:
        return False, "state digests are not the difference"
    if int(a.get("panic_or_failstop_lines") or 0) != 0:
        return False, "panic/fail-stop lines in the 3-node run"
    if a.get("height_spread") is None or a["height_spread"] > 5:
        return False, "height_spread=%r" % a.get("height_spread")
    nodes = {n.get("node"): n.get("state_digest") for n in a.get("nodes") or []}
    if set(nodes) != set(V):
        return False, "agreement nodes %s" % sorted(nodes)
    try:
        lines = {
            v: open(os.path.join(cell, "state-digest-%s.txt" % v)).read().splitlines()
            for v in V
        }
    except OSError as e:
        return False, "digest files: %s" % e
    vals = list(nodes.values())
    maj = max(set(vals), key=vals.count)
    ref = next(v for v in V if nodes[v] == maj)
    diffs = {}
    for v in V:
        if nodes[v] == maj:
            continue
        nl, fields, cls = classify(lines[ref], lines[v])
        diffs[v] = {"vs": ref, "lines": nl, "fields": fields, "cls": cls}
    if not diffs or any(d["cls"] != "mark-only" for d in diffs.values()):
        return False, "digest diff %s" % json.dumps(diffs)
    return True, "digest diff mark fields only: %s" % json.dumps(diffs)


# ---------------------------------------------------------------- judgement


def judge(cell, summary, rc_num, md5ok):
    reasons, artifacts, notes = [], [], {}
    if not md5ok:
        reasons.append("md5")
    if summary is None:
        reasons.append("no-summary")
    if os.path.exists(os.path.join(cell, "node-death.txt")):
        reasons.append("death")
    rc_reason = None
    if rc_num is None:
        rc_reason = "rc-missing"
    agree_reason = verdict_reason = None
    if summary is not None:
        val = summary.get("validity") or {}
        verdict = val.get("verdict")
        av = (summary.get("headline") or {}).get("agreement_verdict") or (
            summary.get("agreement") or {}
        ).get("agreement_verdict")
        crash = summary.get("crash")
        notes.update(
            verdict=verdict,
            agreement=av,
            fail_reasons=val.get("fail_reasons"),
            unverified_reasons=val.get("unverified_reasons"),
            crash_fail_reasons=(crash or {}).get("fail_reasons"),
        )
        mark_ok = False
        if av == "DISAGREE":
            mark_ok, notes["mark_only_digest"] = mark_only_digest(cell, summary)
            if mark_ok:
                artifacts.append("mark-only-digest")
            else:
                agree_reason = "disagree"
        elif av != "AGREE":
            agree_reason = "agreement-" + slug(av or "missing")
        explained = True
        tail_ok = None
        for f in val.get("fail_reasons") or []:
            if f == "validator agreement failed" and mark_ok:
                continue
            if f == "crash gate failed" and crash:
                for cf in crash.get("fail_reasons") or []:
                    if cf == "panic/fail-stop after the restart":
                        if tail_ok is None:
                            tail_ok, notes["restart_tail"] = restart_tail_benign(cell)
                            if tail_ok:
                                artifacts.append("crash-gate-info-failstop-line")
                        explained = explained and tail_ok
                    elif (
                        cf == "state digest differs across the 3 nodes (fork)"
                        and mark_ok
                    ):
                        continue
                    else:
                        explained = False
                continue
            explained = False
        for u in val.get("unverified_reasons") or []:
            if u == "liveness unverified" and crash:
                artifacts.append("restart-liveness-unverified")
                continue
            explained = False
        notes["validity_explained"] = explained
        if rc_num is not None and rc_num != 0:
            if not (rc_num == 2 and verdict in ("REJECT", "UNVERIFIED") and explained):
                rc_reason = "rc=%d" % rc_num
        elif rc_num == 0 and verdict != "ACCEPT":
            verdict_reason = "verdict-" + slug(verdict or "missing")
    elif rc_num is not None and rc_num != 0:
        rc_reason = "rc=%d" % rc_num
    for r in (rc_reason, agree_reason, verdict_reason):
        if r:
            reasons.append(r)
    return reasons, artifacts, notes


# ---------------------------------------------------------------- analysis


def fill(tok, ph):
    for k, v in ph.items():
        tok = tok.replace("{" + k + "}", v)
    return tok


def run_analysis(a, cell, step_dir):
    global CHILD
    out = os.path.join(step_dir, a.label + ".analysis.json")
    logp = os.path.join(step_dir, a.label + ".analysis.log")
    if os.path.exists(out):
        os.remove(out)
    cell_n = os.path.normpath(cell)
    if not cell_n.endswith(a.tag):
        return (
            "fail",
            None,
            "cell dir %s does not end with the tag %s (no {prefix})" % (cell_n, a.tag),
        )
    ph = {
        "out": out,
        "prefix": cell_n[: -len(a.tag)],
        "tag": a.tag,
        "arm": a.arm,
        "label": a.label,
        "cell": cell_n,
        "run": os.path.normpath(a.run_dir),
    }
    try:
        argv = [fill(t, ph) for t in shlex.split(a.analysis_cmd)]
    except ValueError as e:  # e.g. "No closing quotation": a broken command is a failed analysis, not a lost row
        return "fail", None, "cannot parse the analysis command %r: %s" % (a.analysis_cmd, e)
    if not argv:
        return "fail", None, "empty analysis command"
    t0 = time.time()
    with open(logp, "w") as lf:
        lf.write("# %s\n" % " ".join(shlex.quote(x) for x in argv))
        lf.flush()
        try:
            CHILD = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=lf,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        except OSError as e:
            return "fail", None, "cannot start: %s" % e
        try:
            rc = CHILD.wait(timeout=a.analysis_timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(CHILD.pid, signal.SIGKILL)
            except OSError:
                pass
            CHILD.wait()
            CHILD = None
            return (
                "timeout",
                None,
                "killed after %.0f s (log %s)" % (a.analysis_timeout, logp),
            )
        CHILD = None
    took = time.time() - t0
    if rc != 0:
        return "fail", None, "exit %d after %.1f s (log %s)" % (rc, took, logp)
    d = load_json(out)
    key = fill(a.analysis_key, ph)
    if not isinstance(d, dict) or not isinstance(d.get(key), dict):
        return "fail", None, "no row %r in %s" % (key, out)
    metric = None
    if a.analysis_metric:
        metric = d[key].get(a.analysis_metric)
        if (
            isinstance(metric, bool)
            or not isinstance(metric, (int, float))
            or not math.isfinite(metric)
        ):
            return (
                "fail",
                None,
                "row %r has no finite %r (%r)" % (key, a.analysis_metric, metric),
            )
    return "ok", metric, "exit 0 in %.1f s, row %r" % (took, key)


# ---------------------------------------------------------------- files


def check_header(lines, path):
    if lines and lines[0] != "\t".join(HEADER):
        die(
            2,
            "%s has a foreign header %r (want %r); refusing to append"
            % (path, lines[0], "\t".join(HEADER)),
        )


def labels_in(lines):
    return {ln.split("\t", 1)[0] for ln in lines[1:] if ln}


def append_line(path, line):
    with open(path, "a") as fh:
        fh.write(line + "\n")
        fh.flush()
        os.fsync(fh.fileno())


def queue_rerun(run, a):
    rl = a.label + "-rerun"
    p = os.path.join(run, QUEUE)
    with open(p, "a+") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)
        fh.seek(0)
        if any(ln.split("\t", 1)[0] == rl for ln in fh.read().splitlines()):
            return rl, False
        fh.write("\t".join((rl, a.label, a.arm, a.rep, a.spec)) + "\n")
        fh.flush()
        os.fsync(fh.fileno())
    return rl, True


def extra_reasons(txt):
    """--extra-reasons "check,stop-fd": the campaign's own failures of this cell, slugged, in order, deduped."""
    out = []
    for x in (txt or "").split(","):
        if x.strip() and slug(x) not in out:
            out.append(slug(x))
    return out


def record(a):
    run = a.run_dir
    for name, v in (
        ("label", a.label),
        ("tag", a.tag),
        ("arm", a.arm),
        ("rep", a.rep),
        ("spec", a.spec),
    ):
        if re.search(r"[\t\n]", v) or (
            name != "spec" and (not v or re.search(r"\s", v))
        ):
            die(
                2,
                "--%s %r must be non-empty without whitespace (spec: no tab/newline)"
                % (name, v),
            )
    os.makedirs(run, exist_ok=True)
    step_dir = os.path.join(run, "cell-step")
    os.makedirs(step_dir, exist_ok=True)
    tsv = os.path.join(run, TSV)
    log = a.campaign_log or os.path.join(run, "campaign.log")
    lines = open(tsv).read().splitlines() if os.path.exists(tsv) else []
    check_header(lines, tsv)
    if a.label in labels_in(lines):
        msg = "CELL-STEP DUPLICATE %s: already has a row in %s; nothing written" % (
            a.label,
            tsv,
        )
        append_line(log, msg)
        print(msg, file=sys.stderr)
        return 3
    t0 = time.time()
    cell = a.cell_dir
    rc_file = a.rc_file or os.path.normpath(cell) + ".cell.rc"
    rc_txt, rc_num = read_rc(rc_file)
    summary = load_json(os.path.join(cell, "summary.json"))
    md5ok, md5ev = md5_check(cell, summary, a.staged_md5, not a.no_summary_md5)
    reasons, artifacts, notes = judge(cell, summary, rc_num, md5ok)
    for x in extra_reasons(a.extra_reasons):
        if x not in reasons:
            reasons.append(x)
    an_status, an_metric, an_detail = "skipped", None, "no --analysis-cmd"
    if a.analysis_cmd:
        try:
            an_status, an_metric, an_detail = run_analysis(a, cell, step_dir)
        except Exception as e:  # noqa: BLE001 — any analysis-path error flags the cell, the row is still written
            an_status, an_metric, an_detail = (
                "fail",
                None,
                "analysis step error %s: %s" % (type(e).__name__, e),
            )
        if an_status == "fail":
            reasons.append("analysis")
        elif an_status == "timeout":
            reasons.append("analysis-timeout")
    h = (summary or {}).get("headline") or {}
    if an_metric is not None:
        metric, msrc = an_metric, "analysis:%s" % a.analysis_metric
    elif summary is not None and h.get("matched_s_avg") is not None:
        metric, msrc = h.get("matched_s_avg"), "summary.json headline.matched_s_avg"
    else:
        metric, msrc = 0, "none"
    agree = (
        "yes"
        if summary is not None
        and (summary.get("validity") or {}).get("verdict") == "ACCEPT"
        and h.get("agreement_verdict") == "AGREE"
        else "no"
    )
    flag = "bad:" + ",".join(reasons) if reasons else "ok"
    row = dict(
        zip(
            HEADER,
            (
                a.label,
                a.arm,
                a.rep,
                rc_txt,
                agree,
                "yes" if md5ok else "no",
                fmt_metric(metric),
                flag,
            ),
        )
    )
    is_rerun = a.label.endswith("-rerun")
    queued = None
    ev = {
        "label": a.label,
        "tag": a.tag,
        "arm": a.arm,
        "rep": a.rep,
        "spec": a.spec,
        "cell": cell,
        "rc_file": rc_file,
        "row": row,
        "reasons": reasons,
        "artifacts": artifacts,
        "notes": notes,
        "md5": md5ev,
        "analysis": {"status": an_status, "detail": an_detail, "metric": an_metric},
        "metric_source": msrc,
        "step_seconds": round(time.time() - t0, 2),
        "written_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    if reasons and not is_rerun:
        queued = queue_rerun(run, a)
        ev["rerun_queued"] = queued[0]
    with open(os.path.join(step_dir, a.label + ".json"), "w") as fh:
        json.dump(ev, fh, indent=1)
    if reasons:
        append_line(log, "CELL-FLAG %s %s" % (a.label, ",".join(reasons)))
    with open(tsv, "a+") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)
        fh.seek(0)
        cur = fh.read().splitlines()
        check_header(cur, tsv)
        if a.label in labels_in(
            cur
        ):  # raced by another writer: never two rows for one label
            die(3, "CELL-STEP DUPLICATE %s (appeared while the step ran)" % a.label)
        if not cur:
            fh.write("\t".join(HEADER) + "\n")
        fh.write("\t".join(row[k] for k in HEADER) + "\n")
        fh.flush()
        os.fsync(fh.fileno())
    tail = ""
    if reasons and is_rerun:
        tail = "; rerun bad again: not requeued"
    elif queued:
        tail = "; rerun %s %s" % (
            "queued as" if queued[1] else "already queued:",
            queued[0],
        )
    print(
        "[cell-step] %s rc=%s agree=%s md5_ok=%s metric=%s flag=%s; artifacts: %s; analysis: %s (%s)%s"
        % (
            a.label,
            row["rc"],
            row["agree"],
            row["md5_ok"],
            row["metric"],
            flag,
            ",".join(artifacts) or "none",
            an_status,
            an_detail,
            tail,
        ),
        flush=True,
    )
    return 1 if reasons else 0


def pending(a):
    run = a.run_dir
    q = os.path.join(run, QUEUE)
    if not os.path.exists(q):
        return 0
    tsv = os.path.join(run, TSV)
    done = labels_in(open(tsv).read().splitlines()) if os.path.exists(tsv) else set()
    seen = set()
    for ln in open(q).read().splitlines():
        f = ln.split("\t")
        if len(f) != 5 or f[0] in seen or f[0] in done:
            continue
        seen.add(f[0])
        print(ln)
    return 0


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("record")
    r.add_argument("--run-dir", required=True)
    r.add_argument("--cell-dir", required=True)
    r.add_argument("--label", required=True)
    r.add_argument("--tag", required=True)
    r.add_argument("--arm", required=True)
    r.add_argument("--rep", required=True)
    r.add_argument("--staged-md5", required=True)
    r.add_argument("--spec", default="-")
    r.add_argument("--rc-file")
    r.add_argument("--campaign-log")
    r.add_argument("--analysis-cmd", default="")
    r.add_argument("--analysis-key", default="{tag}")
    r.add_argument("--analysis-metric", default="")
    r.add_argument("--analysis-timeout", type=float, default=600.0)
    r.add_argument("--extra-reasons", default="")
    r.add_argument("--no-summary-md5", action="store_true")
    p = sub.add_parser("pending-reruns")
    p.add_argument("--run-dir", required=True)
    a = ap.parse_args()
    signal.signal(signal.SIGTERM, on_term)
    signal.signal(signal.SIGHUP, on_term)
    if a.cmd == "record":
        if not (a.analysis_timeout > 0):
            die(2, "--analysis-timeout must be > 0")
        try:
            return record(a)
        except Exception:  # noqa: BLE001 — distinct code; cell-step.sh writes the bad:step-error row
            traceback.print_exc()
            return 5
    return pending(a)


if __name__ == "__main__":
    sys.exit(main())
