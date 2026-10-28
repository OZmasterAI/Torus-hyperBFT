#!/usr/bin/env python3
"""test_cell_step.py — fixture tests for the per-cell write-up step (cell_step.py + cell-step.sh).

Runs the real cell_step.py (and the bash helper) over copies of three real
ozarchy-p3gate cells' small files (testdata/cell-step/, summary.json trimmed to
the keys the step reads, values verbatim):

  * ozarchy-p3gate-300m-r1-m        rc 0, ACCEPT AGREE           -> ok
  * ozarchy-p3gate-300m-t1-s2-TERM  rc 2, REJECT AGREE: crash gate FAIL only on the restarted val1's
                                    INFO line 'running state hash fail-stop ... on=false'   -> ok (known artifact)
  * ozarchy-p3gate-300m-t1-m-TERM   rc 2, REJECT DISAGREE: the same INFO line + the mark-only digest
                                    (val0 differs in 5 mark fields only, block hash equal)   -> ok (known artifact)

and asserts the cells.tsv contract (header once; label arm rep rc agree md5_ok
metric flag; tab-separated), each bad reason, the CELL-FLAG line, the rerun
queue (<label>-rerun, bad row kept, a bad rerun is not requeued), duplicate
refusal, the analysis timeout and the helper's outer timeout. The analysis
step is exercised with testdata/cell-step/fake_analysis.py (a-cells.py shape).

Usage: python3 tools/matched-bench/test_cell_step.py   (also collected by pytest)
"""

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
STEP = os.path.join(HERE, "cell_step.py")
HELPER = os.path.join(HERE, "cell-step.sh")
TEMPLATE = os.path.join(HERE, "campaign-template.sh")
FIX = os.path.join(HERE, "testdata", "cell-step")
FAKE = os.path.join(FIX, "fake_analysis.py")

P = "ozarchy-p3gate-300m-"
OK, TS2, TM = P + "r1-m", P + "t1-s2-TERM", P + "t1-m-TERM"
MD5 = {
    "m": "f10815747be79f22f502b51a8cd466eb",
    "s2": "4e0d8b84f54f53d5b6db2d5e696cd51c",
}
HEADER = "label\tarm\trep\trc\tagree\tmd5_ok\tmetric\tflag"
ANALYSIS = "python3 -I %s {out} {prefix} {tag}:{arm}" % FAKE


class Env:
    """One throwaway results root (R) + campaign run dir (RUN) per test."""

    def __init__(self, *labels):
        self.tmp = tempfile.mkdtemp(prefix="cell-step-test-")
        self.R = os.path.join(self.tmp, "res")
        self.RUN = os.path.join(self.tmp, "ozarchy-test-run")
        os.makedirs(self.R)
        os.makedirs(self.RUN)
        for lb in labels:
            self.add(lb)

    def add(self, src, dst=None):
        dst = dst or src
        shutil.copytree(os.path.join(FIX, src), os.path.join(self.R, dst))
        shutil.copy(
            os.path.join(FIX, src + ".cell.rc"), os.path.join(self.R, dst + ".cell.rc")
        )
        return os.path.join(self.R, dst)

    def path(self, *p):
        return os.path.join(self.R, *p)

    def run_path(self, *p):
        return os.path.join(self.RUN, *p)

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def step(
        self,
        label,
        arm,
        rep,
        md5=None,
        analysis=ANALYSIS,
        env=None,
        extra=(),
        tag=None,
        spec=None,
        timeout=60,
    ):
        tag = tag or label[len(P) :]
        cmd = [
            sys.executable,
            "-I",
            STEP,
            "record",
            "--run-dir",
            self.RUN,
            "--cell-dir",
            self.path(label),
            "--label",
            label,
            "--tag",
            tag,
            "--arm",
            arm,
            "--rep",
            rep,
            "--staged-md5",
            md5 or MD5[arm],
            "--spec",
            spec or "%s:%s:120:none:" % (tag, arm),
        ]
        if analysis:
            cmd += ["--analysis-cmd", analysis, "--analysis-metric", "ms"]
        cmd += list(extra)
        e = dict(os.environ)
        e.update(env or {})
        return subprocess.run(
            cmd, capture_output=True, text=True, env=e, timeout=timeout
        )

    def rows(self):
        p = self.run_path("cells.tsv")
        if not os.path.exists(p):
            return []
        return open(p).read().splitlines()

    def table(self):
        lines = self.rows()
        assert lines and lines[0] == HEADER, lines[:1]
        return [dict(zip(HEADER.split("\t"), ln.split("\t"))) for ln in lines[1:]]

    def log(self):
        p = self.run_path("campaign.log")
        return open(p).read().splitlines() if os.path.exists(p) else []

    def flags(self):
        return [ln for ln in self.log() if ln.startswith("CELL-FLAG ")]

    def pending(self):
        r = subprocess.run(
            [sys.executable, "-I", STEP, "pending-reruns", "--run-dir", self.RUN],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert r.returncode == 0, r.stderr
        return [ln.split("\t") for ln in r.stdout.splitlines() if ln]


def with_env(*labels):
    def deco(fn):
        def run():
            e = Env(*labels)
            try:
                fn(e)
            finally:
                e.close()

        run.__name__ = fn.__name__
        run.__doc__ = fn.__doc__
        return run

    return deco


def set_rc(e, label, text):
    with open(e.path(label + ".cell.rc"), "w") as fh:
        fh.write(text)


def edit_json(path, fn):
    d = json.load(open(path))
    fn(d)
    json.dump(d, open(path, "w"), indent=1)


def flag_of(row):
    return row["flag"]


def bad_reasons(flag):
    assert flag.startswith("bad:"), flag
    return flag[4:].split(",")


# ---------------------------------------------------------------- contract format


@with_env(OK, TS2)
def test_header_written_once(e):
    """the header is the first line, written once for any number of rows"""
    assert e.step(OK, "m", "r1").returncode == 0
    assert e.step(TS2, "s2", "t1").returncode == 0
    lines = e.rows()
    assert lines[0] == HEADER, lines[0]
    assert sum(1 for ln in lines if ln == HEADER) == 1, lines
    assert len(lines) == 3, lines


@with_env(OK)
def test_row_columns_order_and_tabs(e):
    """one row: 8 tab-separated columns in contract order, plain-number metric, no stray whitespace"""
    r = e.step(OK, "m", "r1")
    assert r.returncode == 0, r.stdout + r.stderr
    line = e.rows()[1]
    cols = line.split("\t")
    assert len(cols) == 8, cols
    assert all(c == c.strip() and c and " " not in c for c in cols), cols
    assert cols[:6] == [OK, "m", "r1", "0", "yes", "yes"], cols
    assert re.fullmatch(r"\d+(\.\d+)?", cols[6]), cols[6]
    assert cols[6] == "199153.9", cols[
        6
    ]  # fake analysis = summary headline.matched_s_avg of the real r1-m
    assert cols[7] == "ok", cols


@with_env(OK)
def test_existing_foreign_header_is_refused(e):
    """a cells.tsv with another header is never appended to"""
    with open(e.run_path("cells.tsv"), "w") as fh:
        fh.write("label\tarm\tmetric\n")
    r = e.step(OK, "m", "r1")
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert "header" in r.stderr, r.stderr
    assert open(e.run_path("cells.tsv")).read() == "label\tarm\tmetric\n"


# ---------------------------------------------------------------- ok cell and metric


@with_env(OK)
def test_ok_cell(e):
    """rc 0, ACCEPT AGREE, md5 equal, analysis row present -> ok; nothing flagged or queued"""
    calls = e.run_path("calls.txt")
    r = e.step(OK, "m", "r1", env={"FAKE_ANALYSIS_CALLS": calls})
    assert r.returncode == 0, r.stdout + r.stderr
    (row,) = e.table()
    assert row == {
        "label": OK,
        "arm": "m",
        "rep": "r1",
        "rc": "0",
        "agree": "yes",
        "md5_ok": "yes",
        "metric": row["metric"],
        "flag": "ok",
    }, row
    assert e.flags() == [], e.log()
    assert e.pending() == []
    # the analysis ran once, on this cell (prefix + tag = the cell dir), with its arm
    assert open(calls).read().split() == [e.path(OK), "m"]


@with_env(OK)
def test_metric_from_analysis_row(e):
    """the metric is the analysis row's value when the analysis produces one"""
    s = json.load(open(e.path(OK, "summary.json")))["headline"]["matched_s_avg"]
    r = e.step(OK, "m", "r1", env={"FAKE_ANALYSIS_DELTA": "1000"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert float(e.table()[0]["metric"]) == s + 1000, (e.table(), s)


@with_env(OK)
def test_metric_falls_back_to_summary(e):
    """no analysis configured -> metric = summary.json headline.matched_s_avg, still ok"""
    s = json.load(open(e.path(OK, "summary.json")))["headline"]["matched_s_avg"]
    r = e.step(OK, "m", "r1", analysis=None)
    assert r.returncode == 0, r.stdout + r.stderr
    row = e.table()[0]
    assert float(row["metric"]) == s and row["flag"] == "ok", row


# ---------------------------------------------------------------- known artifacts stay ok


@with_env(TS2)
def test_artifact_crash_gate_info_line_ok(e):
    """rc 2 whose only failure is the crash gate counting the INFO 'state hash fail-stop ... on=false' line -> ok"""
    r = e.step(TS2, "s2", "t1")
    assert r.returncode == 0, r.stdout + r.stderr
    (row,) = e.table()
    assert row["rc"] == "2" and row["flag"] == "ok", row
    assert row["agree"] == "no", row  # raw verdict: REJECT AGREE is not ACCEPT/AGREE
    assert row["md5_ok"] == "yes", row
    assert e.flags() == [] and e.pending() == []
    assert "crash-gate-info-failstop-line" in r.stdout, r.stdout


@with_env(TS2)
def test_artifact_crash_gate_info_line_on_true_ok(e):
    """s136: fail-stop is on by default, so the INFO config echo reads on=true -> still the artifact"""
    p = e.path(TS2, "crash-restart-tail.log")
    with open(p) as fh:
        tail = fh.read()
    assert "(TORUS_STATE_HASH_FAILSTOP) on=false" in tail
    with open(p, "w") as fh:
        fh.write(
            tail.replace(
                "(TORUS_STATE_HASH_FAILSTOP) on=false",
                "(TORUS_STATE_HASH_FAILSTOP) on=true",
            )
        )
    r = e.step(TS2, "s2", "t1")
    assert r.returncode == 0, r.stdout + r.stderr
    (row,) = e.table()
    assert row["rc"] == "2" and row["flag"] == "ok", row
    assert "crash-gate-info-failstop-line" in r.stdout, r.stdout


@with_env(TM)
def test_artifact_mark_only_digest_ok(e):
    """rc 2, REJECT DISAGREE where val0 differs in mark fields only (block hash equal) + the INFO line -> ok"""
    r = e.step(TM, "m", "t1")
    assert r.returncode == 0, r.stdout + r.stderr
    (row,) = e.table()
    assert row["rc"] == "2" and row["agree"] == "no" and row["flag"] == "ok", row
    assert "mark-only-digest" in r.stdout, r.stdout
    assert e.flags() == [] and e.pending() == []


# ---------------------------------------------------------------- each bad reason


@with_env(OK)
def test_bad_rc_nonzero(e):
    """rc != 0 on an ACCEPT AGREE cell is not an artifact"""
    set_rc(e, OK, "rc=1\n")
    r = e.step(OK, "m", "r1")
    assert r.returncode == 1, r.stdout + r.stderr
    row = e.table()[0]
    assert row["rc"] == "1" and "rc=1" in bad_reasons(row["flag"]), row


@with_env(OK)
def test_bad_rc_missing(e):
    """no rc file (the unit ended without writing one) is bad"""
    os.remove(e.path(OK + ".cell.rc"))
    r = e.step(OK, "m", "r1")
    assert r.returncode == 1
    row = e.table()[0]
    assert row["rc"] == "missing" and "rc-missing" in bad_reasons(row["flag"]), row


@with_env(TS2)
def test_bad_rc2_real_panic_line(e):
    """the same rc 2 shape, but the restart tail also has a WARN fail-stop line -> not the artifact"""
    with open(e.path(TS2, "crash-restart-tail.log"), "a") as fh:
        fh.write(
            "2026-10-13T15:38:00.000000Z  WARN torus_consensus::state_hash: Latching fail-stop: state hash mismatch\n"
        )
    r = e.step(TS2, "s2", "t1")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "rc=2" in bad_reasons(e.table()[0]["flag"]), e.table()


@with_env(TS2)
def test_bad_rc2_no_restart_tail(e):
    """the INFO-line excuse needs the restart tail as evidence"""
    os.remove(e.path(TS2, "crash-restart-tail.log"))
    assert e.step(TS2, "s2", "t1").returncode == 1
    assert "rc=2" in bad_reasons(e.table()[0]["flag"])


@with_env(TS2)
def test_bad_rc2_other_crash_reason(e):
    """a crash gate failure other than the INFO line (e.g. a rewind) is real"""
    edit_json(
        e.path(TS2, "summary.json"),
        lambda d: d["crash"]["fail_reasons"].append(
            "rewind 9 blocks beyond the exec queue (max 2)"
        ),
    )
    assert e.step(TS2, "s2", "t1").returncode == 1
    assert "rc=2" in bad_reasons(e.table()[0]["flag"])


@with_env(TM)
def test_bad_disagree_hard_digest_field(e):
    """DISAGREE where a cash field differs is a fork signal, not the mark-only artifact"""
    p = e.path(TM, "state-digest-val0.txt")
    lines = open(p).read().splitlines()
    o = json.loads(lines[-1])
    o["cashBalance"] = "1.00000000"
    lines[-1] = json.dumps(o, separators=(",", ":"))
    open(p, "w").write("\n".join(lines) + "\n")
    r = e.step(TM, "m", "t1")
    assert r.returncode == 1, r.stdout + r.stderr
    reasons = bad_reasons(e.table()[0]["flag"])
    assert "disagree" in reasons and "rc=2" in reasons, reasons


@with_env(TM)
def test_bad_disagree_block_hash_differs(e):
    """mark-only digest lines do not excuse a block hash split"""
    edit_json(
        e.path(TM, "summary.json"),
        lambda d: d["agreement"].update(block_hash_equal=False),
    )
    assert e.step(TM, "m", "t1").returncode == 1
    assert "disagree" in bad_reasons(e.table()[0]["flag"])


def _mark_only_with_spread(e, spread):
    edit_json(
        e.path(TM, "summary.json"),
        lambda d: d["agreement"].update(height_spread=spread),
    )
    return e.step(TM, "m", "t1")


@with_env(TM)
def test_artifact_mark_only_digest_head_spread_six_ok(e):
    """the mark-only excuse takes summarize.py's head-height spread limit of 15 for every N
    (18c chat msg 213, option A): spread 6 was over the old limit of 5"""
    r = _mark_only_with_spread(e, 6)
    assert r.returncode == 0, r.stdout + r.stderr
    (row,) = e.table()
    assert row["flag"] == "ok", row
    assert "mark-only-digest" in r.stdout, r.stdout


@with_env(TM)
def test_artifact_mark_only_digest_head_spread_fifteen_ok(e):
    """spread 15 is the limit itself: still the mark-only artifact"""
    r = _mark_only_with_spread(e, 15)
    assert r.returncode == 0, r.stdout + r.stderr
    (row,) = e.table()
    assert row["flag"] == "ok", row
    assert "mark-only-digest" in r.stdout, r.stdout


@with_env(TM)
def test_bad_disagree_mark_only_head_spread_sixteen(e):
    """a head spread of 16 (a stalled node) is not excused by mark-only digest lines"""
    r = _mark_only_with_spread(e, 16)
    assert r.returncode == 1, r.stdout + r.stderr
    reasons = bad_reasons(e.table()[0]["flag"])
    assert "disagree" in reasons, reasons
    assert "mark-only-digest" not in r.stdout, r.stdout


@with_env(TM)
def test_bad_disagree_no_digest_files(e):
    """DISAGREE without the digest files to classify it stays DISAGREE"""
    for v in range(3):
        os.remove(e.path(TM, "state-digest-val%d.txt" % v))
    assert e.step(TM, "m", "t1").returncode == 1
    assert "disagree" in bad_reasons(e.table()[0]["flag"])


@with_env(OK)
def test_bad_disagree_plain_cell(e):
    """a non-restart cell reporting DISAGREE (rc 2) is bad: disagree"""

    def f(d):
        d["headline"]["agreement_verdict"] = "DISAGREE"
        d["agreement"]["agreement_verdict"] = "DISAGREE"
        d["agreement"]["state_digest_equal"] = False
        d["validity"] = {
            "verdict": "REJECT",
            "accepted": False,
            "fail_reasons": ["validator agreement failed"],
            "unverified_reasons": [],
        }

    edit_json(e.path(OK, "summary.json"), f)
    set_rc(e, OK, "rc=2\n")
    assert e.step(OK, "m", "r1").returncode == 1
    row = e.table()[0]
    assert row["agree"] == "no" and "disagree" in bad_reasons(row["flag"]), row


@with_env(OK)
def test_bad_md5_mismatch_staged(e):
    """the staged md5 of the arm differs from what the cell ran"""
    r = e.step(OK, "m", "r1", md5=MD5["s2"])
    assert r.returncode == 1
    row = e.table()[0]
    assert row["md5_ok"] == "no" and "md5" in bad_reasons(row["flag"]), row


@with_env(OK)
def test_bad_md5_one_process_differs(e):
    """one of the three running node processes had another binary"""
    p = e.path(OK, "node-environ-trie.txt")
    t = open(p).read().splitlines()
    t[2] = t[2].replace("exe_md5=f1081574", "exe_md5=4e0d8b84")
    open(p, "w").write("\n".join(t) + "\n")
    assert e.step(OK, "m", "r1").returncode == 1
    row = e.table()[0]
    assert row["md5_ok"] == "no" and "md5" in bad_reasons(row["flag"]), row


@with_env(TS2)
def test_bad_md5_restarted_process_differs(e):
    """the restarted val1 of a restart cell must run the same binary"""
    p = e.path(TS2, "node-restart-env.txt")
    t = open(p).read()
    assert "exe_md5=4e0d8b84" in t, t
    open(p, "w").write(t.replace("exe_md5=4e0d8b84", "exe_md5=f1081574"))
    assert e.step(TS2, "s2", "t1").returncode == 1
    assert e.table()[0]["md5_ok"] == "no"


@with_env(OK)
def test_bad_md5_no_process_record(e):
    """no record of the running processes = md5 unproven = no"""
    os.remove(e.path(OK, "node-environ-trie.txt"))
    assert e.step(OK, "m", "r1").returncode == 1
    assert e.table()[0]["md5_ok"] == "no"


@with_env(OK)
def test_bad_no_summary(e):
    """a cell that died before summary.json: bad, metric 0"""
    os.remove(e.path(OK, "summary.json"))
    open(e.path(OK, "node-death.txt"), "w").write(
        "DEATH node val1 comm=torus-node pid=1 detected\n"
    )
    r = e.step(OK, "m", "r1")
    assert r.returncode == 1, r.stdout + r.stderr
    row = e.table()[0]
    reasons = bad_reasons(row["flag"])
    assert "no-summary" in reasons and "death" in reasons, reasons
    assert row["metric"] == "0" and row["agree"] == "no", row


@with_env(OK)
def test_bad_analysis_exit_nonzero(e):
    """the campaign's analysis script fails on this cell -> bad:analysis"""
    r = e.step(OK, "m", "r1", env={"FAKE_ANALYSIS_MODE": "fail"})
    assert r.returncode == 1, r.stdout + r.stderr
    row = e.table()[0]
    assert bad_reasons(row["flag"]) == ["analysis"], row
    assert row["md5_ok"] == "yes" and row["agree"] == "yes", row
    # metric falls back to summary.json
    assert (
        float(row["metric"])
        == json.load(open(e.path(OK, "summary.json")))["headline"]["matched_s_avg"]
    )
    # the analysis output is kept for the analyst
    assert "KeyError" in open(e.run_path("cell-step", OK + ".analysis.log")).read()


@with_env(OK)
def test_bad_analysis_no_row(e):
    """exit 0 but no row for this cell is an analysis failure too"""
    assert e.step(OK, "m", "r1", env={"FAKE_ANALYSIS_MODE": "norow"}).returncode == 1
    assert bad_reasons(e.table()[0]["flag"]) == ["analysis"]


@with_env(OK)
def test_bad_analysis_timeout(e):
    """a hung analysis is killed at --analysis-timeout; the row is still written (bad:analysis-timeout)"""
    pidf = e.run_path("analysis.pid")
    t0 = time.time()
    r = e.step(
        OK,
        "m",
        "r1",
        env={"FAKE_ANALYSIS_MODE": "sleep", "FAKE_ANALYSIS_PIDFILE": pidf},
        extra=["--analysis-timeout", "2"],
    )
    took = time.time() - t0
    assert r.returncode == 1, r.stdout + r.stderr
    assert took < 20, took
    assert bad_reasons(e.table()[0]["flag"]) == ["analysis-timeout"], e.table()
    assert not alive(int(open(pidf).read())), "hung analysis process still alive"


@with_env(OK)
def test_bad_analysis_malformed_command(e):
    """unbalanced quoting in the analysis command (shlex 'No closing quotation') is an analysis failure: the row
    is still written (bad:analysis), with the CELL-FLAG line and a queued rerun; the step does not crash"""
    r = e.step(
        OK, "m", "r1", analysis="python3 -I '%s {out} {prefix} {tag}:{arm}" % FAKE
    )
    assert r.returncode == 1, r.stdout + r.stderr
    assert "Traceback" not in r.stderr, r.stderr
    (row,) = e.table()
    assert bad_reasons(row["flag"]) == ["analysis"], row
    assert e.flags() == ["CELL-FLAG %s analysis" % OK], e.flags()
    assert [x[0] for x in e.pending()] == [OK + "-rerun"]
    ev = json.load(open(e.run_path("cell-step", OK + ".json")))
    assert "quotation" in ev["analysis"]["detail"], ev["analysis"]


@with_env(OK)
def test_extra_reasons_flag_a_good_cell_bad(e):
    """--extra-reasons (the campaign's own failed hard checks) make an otherwise good cell bad:<reason>, with the
    CELL-FLAG line and a queued rerun; the reasons come after the step's own and before the analysis reason"""
    r = e.step(OK, "m", "r1", extra=["--extra-reasons", "check"])
    assert r.returncode == 1, r.stdout + r.stderr
    (row,) = e.table()
    assert row["flag"] == "bad:check" and row["agree"] == "yes", row
    assert e.flags() == ["CELL-FLAG %s check" % OK], e.flags()
    assert [x[0] for x in e.pending()] == [OK + "-rerun"]
    set_rc(e, OK, "rc=1\n")
    shutil.copytree(e.path(OK), e.path(OK + "-b"))
    shutil.copy(e.path(OK + ".cell.rc"), e.path(OK + "-b.cell.rc"))
    r = e.step(
        OK + "-b",
        "m",
        "r1",
        tag="r1-m-b",
        env={"FAKE_ANALYSIS_MODE": "fail"},
        extra=["--extra-reasons", "check,Stop FD!"],
    )
    assert r.returncode == 1, r.stdout + r.stderr
    assert bad_reasons(e.table()[1]["flag"]) == [
        "rc=1",
        "check",
        "stop-fd",
        "analysis",
    ], e.table()


# ---------------------------------------------------------------- CELL-FLAG, rerun queue, duplicates


@with_env(OK)
def test_cell_flag_line_format(e):
    """bad cell -> exactly one 'CELL-FLAG <label> <reason>' line, reason = the flag after 'bad:'"""
    set_rc(e, OK, "rc=1\n")
    e.step(OK, "m", "r1", env={"FAKE_ANALYSIS_MODE": "fail"})
    flag = e.table()[0]["flag"]
    assert e.flags() == ["CELL-FLAG %s %s" % (OK, flag[4:])], (e.flags(), flag)
    assert re.fullmatch(r"CELL-FLAG \S+ \S+", e.flags()[0])


@with_env(OK)
def test_rerun_queued_and_bad_row_kept(e):
    """bad -> <label>-rerun queued; the rerun gets its own row; the bad row stays untouched"""
    set_rc(e, OK, "rc=1\n")
    assert e.step(OK, "m", "r1", spec="r1-m:m:120:none:").returncode == 1
    bad_line = e.rows()[1]
    assert e.pending() == [[OK + "-rerun", OK, "m", "r1", "r1-m:m:120:none:"]], (
        e.pending()
    )
    e.add(OK, OK + "-rerun")
    r = e.step(OK + "-rerun", "m", "r1", tag="r1-m-rerun")
    assert r.returncode == 0, r.stdout + r.stderr
    lines = e.rows()
    assert lines[1] == bad_line, lines
    assert lines[2].split("\t")[0] == OK + "-rerun" and lines[2].endswith("\tok"), lines
    assert e.pending() == [], "a rerun that has its row is no longer pending"


@with_env(OK)
def test_rerun_bad_again_not_requeued(e):
    """a bad rerun is flagged (row + CELL-FLAG) but not queued again"""
    set_rc(e, OK, "rc=1\n")
    e.step(OK, "m", "r1")
    e.add(OK, OK + "-rerun")
    set_rc(e, OK + "-rerun", "rc=1\n")
    r = e.step(OK + "-rerun", "m", "r1", tag="r1-m-rerun")
    assert r.returncode == 1
    assert [x["flag"] for x in e.table()] == ["bad:rc=1", "bad:rc=1"], e.table()
    assert e.flags() == ["CELL-FLAG %s rc=1" % OK, "CELL-FLAG %s-rerun rc=1" % OK], (
        e.flags()
    )
    q = open(e.run_path("rerun-queue.tsv")).read().splitlines()
    assert [x.split("\t")[0] for x in q] == [OK + "-rerun"], q
    assert e.pending() == []
    assert "not requeued" in r.stdout, r.stdout


@with_env(OK)
def test_duplicate_label_refused(e):
    """a second step for a label that already has a row is refused (exit 3), nothing changes"""
    assert e.step(OK, "m", "r1").returncode == 0
    before = open(e.run_path("cells.tsv")).read()
    set_rc(e, OK, "rc=1\n")
    r = e.step(OK, "m", "r1")
    assert r.returncode == 3, (r.returncode, r.stdout, r.stderr)
    assert open(e.run_path("cells.tsv")).read() == before
    assert e.flags() == [] and e.pending() == []
    assert any(ln.startswith("CELL-STEP DUPLICATE %s" % OK) for ln in e.log()), e.log()


@with_env(OK)
def test_duplicate_bad_label_not_requeued_twice(e):
    """re-invoking a bad label neither adds a row nor a second queue entry"""
    set_rc(e, OK, "rc=1\n")
    e.step(OK, "m", "r1")
    assert e.step(OK, "m", "r1").returncode == 3
    assert len(e.table()) == 1
    assert len(open(e.run_path("rerun-queue.tsv")).read().splitlines()) == 1
    assert len(e.flags()) == 1


@with_env(OK)
def test_evidence_json_written(e):
    """per-cell decision record next to cells.tsv (what was checked, why)"""
    e.step(OK, "m", "r1")
    d = json.load(open(e.run_path("cell-step", OK + ".json")))
    assert d["row"]["flag"] == "ok" and d["label"] == OK, d


# ---------------------------------------------------------------- bash helper + template


def alive(pid):
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    try:  # a zombie is dead for our purpose
        st = open("/proc/%d/stat" % pid).read().rsplit(")", 1)[1].split()[0]
        return st != "Z"
    except OSError:
        return False


def bash(script, env=None, timeout=90):
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(
        ["bash", "-c", script], capture_output=True, text=True, env=e, timeout=timeout
    )


def helper_call(e, label, arm, rep, tag=None):
    tag = tag or label[len(P) :]
    return (
        "source %s; CELL_STEP_ANALYSIS_CMD=%s CELL_STEP_ANALYSIS_METRIC=ms "
        'cell_step %s %s %s %s %s %s %s %s; echo "helper_rc=$?"'
        % (
            q(HELPER),
            q(ANALYSIS),
            q(e.RUN),
            q(e.path(label)),
            q(label),
            q(tag),
            q(arm),
            q(rep),
            q(MD5[arm]),
            q("%s:%s:120:none:" % (tag, arm)),
        )
    )


def q(s):
    return "'" + s.replace("'", "'\\''") + "'"


@with_env(OK, TS2)
def test_helper_runs_step(e):
    """cell-step.sh: cell_step writes the row in sequence and returns the step's code"""
    r = bash(helper_call(e, OK, "m", "r1") + "; " + helper_call(e, TS2, "s2", "t1"))
    assert "helper_rc=0" in r.stdout and r.stdout.count("helper_rc=0") == 2, (
        r.stdout + r.stderr
    )
    assert [x["flag"] for x in e.table()] == ["ok", "ok"], e.table()


@with_env(OK)
def test_helper_timeout_fallback_row(e):
    """outer CELL_STEP_TIMEOUT: the step is killed (with its analysis child), a bad:step-timeout row,
    CELL-FLAG and rerun are still recorded, and the helper returns 124 promptly"""
    pidf = e.run_path("analysis.pid")
    t0 = time.time()
    r = bash(
        helper_call(e, OK, "m", "r1"),
        env={
            "CELL_STEP_TIMEOUT": "3",
            "CELL_STEP_ANALYSIS_TIMEOUT": "100",
            "FAKE_ANALYSIS_MODE": "sleep",
            "FAKE_ANALYSIS_PIDFILE": pidf,
        },
    )
    took = time.time() - t0
    assert "helper_rc=124" in r.stdout, r.stdout + r.stderr
    assert took < 40, took
    (row,) = e.table()
    assert (
        row["flag"] == "bad:step-timeout"
        and row["agree"] == "no"
        and row["md5_ok"] == "no"
    ), row
    assert re.fullmatch(r"\d+(\.\d+)?", row["metric"]), row
    assert e.flags() == ["CELL-FLAG %s step-timeout" % OK], e.flags()
    assert [x[0] for x in e.pending()] == [OK + "-rerun"]
    for _ in range(50):
        if not alive(int(open(pidf).read())):
            break
        time.sleep(0.1)
    assert not alive(int(open(pidf).read())), "analysis child survived the step timeout"


@with_env(OK)
def test_helper_step_error_fallback_row(e):
    """a step that exits without writing its row (crash, unexpected error) still leaves a row: cell_step writes
    bad:step-error (metric from summary.json), the CELL-FLAG line and the rerun, and returns 1"""
    crash = e.run_path("crash_step.py")
    with open(crash, "w") as fh:
        fh.write("import sys\nraise ValueError('No closing quotation')\n")
    r = bash(helper_call(e, OK, "m", "r1"), env={"CELL_STEP_PY": crash})
    assert "helper_rc=1" in r.stdout, r.stdout + r.stderr
    (row,) = e.table()
    assert row["label"] == OK and row["arm"] == "m" and row["rep"] == "r1", row
    assert (
        row["flag"] == "bad:step-error"
        and row["agree"] == "no"
        and row["md5_ok"] == "no"
        and row["rc"] == "0"
    ), row
    assert re.fullmatch(r"\d+(\.\d+)?", row["metric"]), row
    assert e.flags() == ["CELL-FLAG %s step-error" % OK], e.flags()
    assert [x[0] for x in e.pending()] == [OK + "-rerun"]
    # a step that did write its row is left alone (no second row, no extra CELL-FLAG)
    r = bash(helper_call(e, OK, "m", "r1"))
    assert "helper_rc=3" in r.stdout, r.stdout + r.stderr
    assert len(e.table()) == 1 and len(e.flags()) == 1, (e.table(), e.flags())


@with_env(OK)
def test_helper_extra_reasons_arg(e):
    """cell_step's optional 9th arg (failed campaign checks) reaches the row; on a step timeout the fallback row
    carries it too (bad:check,step-timeout)"""
    r = bash(helper_call(e, OK, "m", "r1").replace("; echo", " check; echo", 1))
    assert "helper_rc=1" in r.stdout, r.stdout + r.stderr
    assert e.table()[0]["flag"] == "bad:check", e.table()
    e.add(OK, OK + "-t")
    r = bash(
        helper_call(e, OK + "-t", "m", "r1", tag="r1-m-t").replace(
            "; echo", " check; echo", 1
        ),
        env={
            "CELL_STEP_TIMEOUT": "3",
            "CELL_STEP_ANALYSIS_TIMEOUT": "100",
            "FAKE_ANALYSIS_MODE": "sleep",
            "FAKE_ANALYSIS_PIDFILE": e.run_path("analysis.pid"),
        },
    )
    assert "helper_rc=124" in r.stdout, r.stdout + r.stderr
    assert e.table()[1]["flag"] == "bad:check,step-timeout", e.table()
    assert e.flags()[-1] == "CELL-FLAG %s-t check,step-timeout" % OK, e.flags()


@with_env(OK)
def test_helper_rerun_pass_runs_each_once(e):
    """cell_step_rerun_pass: calls the campaign's runner once per pending rerun (spec, -rerun suffix),
    and a bad rerun is not run again by a second pass"""
    set_rc(e, OK, "rc=1\n")
    e.step(OK, "m", "r1", spec="r1-m:m:120:none:")
    calls = e.run_path("runner-calls.txt")
    # stub runner: what campaign-template.sh's run_spec does (cell + step), on fixtures
    script = (
        "source %s\n"
        'run_spec() { echo "$1|$2" >> %s; cp -r %s %s; echo rc=1 > %s; '
        'CELL_STEP_ANALYSIS_CMD= cell_step %s %s %s r1-m-rerun m r1 %s "$1"; return 0; }\n'
        'cell_step_rerun_pass %s run_spec; echo "pass1=$?"\n'
        'cell_step_rerun_pass %s run_spec; echo "pass2=$?"\n'
        % (
            q(HELPER),
            q(calls),
            q(e.path(OK)),
            q(e.path(OK + "-rerun")),
            q(e.path(OK + "-rerun.cell.rc")),
            q(e.RUN),
            q(e.path(OK + "-rerun")),
            q(OK + "-rerun"),
            q(MD5["m"]),
            q(e.RUN),
            q(e.RUN),
        )
    )
    r = bash(script)
    assert "pass1=0" in r.stdout and "pass2=0" in r.stdout, r.stdout + r.stderr
    assert open(calls).read().splitlines() == ["r1-m:m:120:none:|-rerun"], open(
        calls
    ).read()
    assert [x["label"] for x in e.table()] == [OK, OK + "-rerun"]
    assert e.pending() == []


@with_env()
def test_helper_rerun_pass_counter_advances_when_the_runner_clobbers_its_names(e):
    """the RERUN i/N progress line counts 1/3 2/3 3/3 even when the runner assigns the pass's own variable names
    without `local` (bash dynamic scope: the template's cell() sets n=0 for its TORUS_SHUTDOWN_TIMEOUT_SECS check,
    which made every rerun of ozarchy-p4gate print 1/N), and every queued rerun still runs once with its own spec"""
    with open(e.run_path("rerun-queue.tsv"), "w") as fh:
        for i in (1, 2, 3):
            fh.write("c%d-rerun\tc%d\tm\tr%d\tc%d:m:120:none:\n" % (i, i, i, i))
    calls = e.run_path("runner-calls.txt")
    script = (
        "source %s\n"
        'run_spec() { echo "$1|$2" >> %s; n=0; i=0; rl=clobbered; ol=x; arm=x; rep=x; spec=x; q=(); line=x; out=x; '
        "return 0; }\n"
        'cell_step_rerun_pass %s run_spec; echo "pass=$?"\n'
        % (q(HELPER), q(calls), q(e.RUN))
    )
    r = bash(script)
    out = r.stdout + r.stderr
    assert "pass=0" in r.stdout, out
    prog = re.findall(r"RERUN (\d+)/(\d+) (\S+)", r.stdout)
    assert prog == [
        ("1", "3", "c1-rerun"),
        ("2", "3", "c2-rerun"),
        ("3", "3", "c3-rerun"),
    ], out
    assert open(calls).read().splitlines() == [
        "c1:m:120:none:|-rerun",
        "c2:m:120:none:|-rerun",
        "c3:m:120:none:|-rerun",
    ], open(calls).read()


@with_env()
def test_helper_rerun_pass_stop_names_the_failing_rerun(e):
    """a runner that fails (and clobbers rl) stops the pass, and the STOP line names the rerun that failed"""
    with open(e.run_path("rerun-queue.tsv"), "w") as fh:
        fh.write("c1-rerun\tc1\tm\tr1\tc1:m:120:none:\n")
    r = bash(
        "source %s\n"
        "run_spec() { rl=clobbered; return 1; }\n"
        'cell_step_rerun_pass %s run_spec; echo "pass=$?"\n' % (q(HELPER), q(e.RUN))
    )
    assert "pass=1" in r.stdout, r.stdout + r.stderr
    assert "RERUN PASS STOP at c1-rerun" in r.stdout, r.stdout + r.stderr


@with_env()
def test_template_run_spec_with_stub_cell(e):
    """the template's own run_spec (extracted verbatim) with a stub cell(): a started bad cell gets its row and a
    rerun, a cell that never started gets no row, the rerun pass runs <tag>-rerun once with the same arm and rep"""
    t = open(TEMPLATE).read()
    m = re.search(r"^run_spec\(\) \{.*?^\}\n", t, re.M | re.S)
    assert m, "run_spec() not found in the template"
    calls = e.run_path("cell-calls.txt")
    stub = r"""
set -u
source %(helper)s
export CELL_STEP_ANALYSIS_CMD=%(analysis)s CELL_STEP_ANALYSIS_METRIC=ms
R=%(R)s RUN=%(RUN)s P=ozarchy-p3gate-300m FIX=%(fix)s
declare -A ABIN=([m]=m [s2]=s2) BMD5=([m]=%(md5m)s [s2]=%(md5s2)s)
cell() { # stub: tag arm dur kind killat. r1-m: bad first time (rc=1), its rerun ok; x9-m never starts (ABORT)
    echo "$*" >> %(calls)s
    local label=$P-$1
    case $1 in x9-m) return 1 ;; esac
    cp -r "$FIX/$P-r1-m" "$R/$label"; STARTED_LABEL=$label
    if [ "$1" = r1-m ]; then echo rc=1 > "$R/$label.cell.rc"; else echo rc=0 > "$R/$label.cell.rc"; fi
    return 0
}
%(run_spec)s
run_spec "r1-m:m:120:none:"; echo "spec1=$?"
run_spec "x9-m:m:120:none:"; echo "spec2=$?"
cell_step_rerun_pass "$RUN" run_spec; echo "pass=$?"
cell_step_rerun_pass "$RUN" run_spec; echo "pass2=$?"
""" % {
        "helper": q(HELPER),
        "analysis": q(ANALYSIS),
        "R": q(e.R),
        "RUN": q(e.RUN),
        "fix": q(FIX),
        "md5m": MD5["m"],
        "md5s2": MD5["s2"],
        "calls": q(calls),
        "run_spec": m.group(0),
    }
    r = bash(stub)
    out = r.stdout + r.stderr
    for want in ("spec1=0", "spec2=1", "pass=0", "pass2=0"):
        assert want in r.stdout, (want, out)
    assert open(calls).read().splitlines() == [
        "r1-m m 120 none ",
        "x9-m m 120 none ",
        "r1-m-rerun m 120 none ",
    ], open(calls).read()
    tab = e.table()
    assert [(x["label"], x["rep"], x["flag"]) for x in tab] == [
        (OK, "r1", "bad:rc=1"),
        (OK + "-rerun", "r1", "ok"),
    ], tab
    assert "did not start" in r.stdout, out
    assert e.flags() == ["CELL-FLAG %s rc=1" % OK], e.flags()


@with_env()
def test_template_run_spec_check_fail_flags_row(e):
    """the template's run_spec (verbatim) with a stub cell() whose run is clean (rc 0, ACCEPT AGREE) but whose
    hard checks fail after the start (CHECK FAIL, return 1): the row is bad:check with CELL-FLAG and a queued
    rerun (not flag=ok, queued=0); a STOP site's own reason (CELL_FAIL) is used instead of 'check'"""
    t = open(TEMPLATE).read()
    m = re.search(r"^run_spec\(\) \{.*?^\}\n", t, re.M | re.S)
    assert m, "run_spec() not found in the template"
    stub = r"""
set -u
source %(helper)s
export CELL_STEP_ANALYSIS_CMD=%(analysis)s CELL_STEP_ANALYSIS_METRIC=ms
R=%(R)s RUN=%(RUN)s P=ozarchy-p3gate-300m FIX=%(fix)s
declare -A ABIN=([m]=m [s2]=s2) BMD5=([m]=%(md5m)s [s2]=%(md5s2)s)
cell() { # stub: a clean run, then a failed campaign check (r1-m) or a STOP (r2-m)
    local label=$P-$1
    cp -r "$FIX/$P-r1-m" "$R/$label"; STARTED_LABEL=$label; echo rc=0 > "$R/$label.cell.rc"
    case $1 in
        r1-m) echo "CHECK FAIL $label: TORUS_CHECKPOINT_INTERVAL_MS set on 3 node lines (arm m has no knob)"; return 1 ;;
        r2-m) CELL_FAIL=stop-fd; echo "STOP: a node held too many fds"; return 1 ;;
    esac
    return 0
}
%(run_spec)s
run_spec "r1-m:m:120:none:"; echo "spec1=$?"
run_spec "r2-m:m:120:none:"; echo "spec2=$?"
""" % {
        "helper": q(HELPER),
        "analysis": q(ANALYSIS),
        "R": q(e.R),
        "RUN": q(e.RUN),
        "fix": q(FIX),
        "md5m": MD5["m"],
        "md5s2": MD5["s2"],
        "run_spec": m.group(0),
    }
    r = bash(stub)
    out = r.stdout + r.stderr
    assert "spec1=1" in r.stdout and "spec2=1" in r.stdout, out
    tab = e.table()
    assert [(x["label"], x["agree"], x["flag"]) for x in tab] == [
        (P + "r1-m", "yes", "bad:check"),
        (P + "r2-m", "yes", "bad:stop-fd"),
    ], (tab, out)
    assert e.flags() == [
        "CELL-FLAG %sr1-m check" % P,
        "CELL-FLAG %sr2-m stop-fd" % P,
    ], e.flags()
    assert [x[0] for x in e.pending()] == [P + "r1-m-rerun", P + "r2-m-rerun"]


def test_template_cell_names_its_failures():
    """every 'return 1' in cell() after the cell started either sits in the hard-check block (reason: check, the
    run_spec default) or sets CELL_FAIL first, so the row names why the campaign failed the cell"""
    t = open(TEMPLATE).read()
    body = re.search(r"^cell\(\) \{.*?^\}\n", t, re.M | re.S).group(0)
    after = body[body.index("STARTED_LABEL=$label") :]
    hc0 = after.index("# >>> CAMPAIGN-SPECIFIC: per-cell hard checks")
    hc1 = after.index("# <<< CAMPAIGN-SPECIFIC: per-cell hard checks")
    for mm in re.finditer(r"return 1", after):
        if hc0 < mm.start() < hc1:
            continue
        line = after[after.rfind("\n", 0, mm.start()) + 1 : mm.start()]
        assert "CELL_FAIL=" in line, "return 1 without CELL_FAIL: %r" % line
    run_spec = re.search(r"^run_spec\(\) \{.*?^\}\n", t, re.M | re.S).group(0)
    assert "CELL_FAIL=" in run_spec, "run_spec must reset CELL_FAIL before each cell"


def test_template_samples_resident_rows_and_users_bytes():
    """item 6 Phase 4 step 2: R no longer holds positions and balances (the record set Users does), so
    torus_exec_resident_rows_bytes is R's raw rows only; the G lines sample it and the users bytes gauge,
    whose sum is the older rows-bytes meaning (a binary without the users gauge just omits it: reads 0)"""
    t = open(TEMPLATE).read()
    g = [line for line in t.splitlines() if 'echo "G $(date' in line]
    assert len(g) == 1, g
    assert "exec_resident_rows_bytes|" in g[0], g[0]
    assert "exec_resident_users_bytes|" in g[0], g[0]


def test_bash_syntax():
    """bash -n on the helper and the campaign template"""
    for f in (HELPER, TEMPLATE):
        r = subprocess.run(["bash", "-n", f], capture_output=True, text=True)
        assert r.returncode == 0, (f, r.stderr)


def test_template_wires_step_and_reruns():
    """the template sources the helper, steps every cell before the next starts, and ends with the rerun pass"""
    t = open(TEMPLATE).read()
    assert re.search(r"^\s*source .*cell-step\.sh", t, re.M), (
        "template must source cell-step.sh"
    )
    assert "cell_step " in t, "template must call cell_step"
    assert "cell_step_rerun_pass" in t, "template must run the rerun pass"
    assert t.index('cell_step_rerun_pass "$RUN"') > t.index('for c in "${CELLS[@]}"'), (
        "rerun pass after the main loop"
    )
    assert "CAMPAIGN-SPECIFIC" in t, "campaign-specific parts must be marked"
    # no background job between the cell's end and the step: the step call is a plain foreground call
    assert not re.search(r"cell_step [^\n]*&\s*$", t, re.M), (
        "cell_step must not be backgrounded"
    )


def main():
    tests = [
        (k, v)
        for k, v in sorted(globals().items())
        if k.startswith("test_") and callable(v)
    ]
    failed = 0
    for name, fn in tests:
        try:
            fn()
            print("ok   %s" % name)
        except Exception as ex:  # noqa: BLE001
            failed += 1
            print("FAIL %s: %s: %s" % (name, type(ex).__name__, str(ex)[:600]))
    print("test_cell_step.py: %d passed, %d failed" % (len(tests) - failed, failed))
    return 1 if failed else 0


if __name__ == "__main__":
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    sys.exit(main())
