#!/usr/bin/env python3
"""test_cell_endpass.py — run-cell.sh's report-only scans, in-line vs deferred to the campaign end pass.

    python3 tools/matched-bench/test_cell_endpass.py

Pinned (bench queue pacing rule, s131: report-only post-cell work moves to one pass after the campaign's last
cell, validity work stays in-line):
1. classification: with DEFER_REPORT_SCANS=1 run-cell.sh skips only the ingest eviction scan and the log
   excerpts (log-scans.sh REPORT_SCANS); the panic / fail-stop / ERROR counts (agreement), the dissemination
   counts (collect_logs.py, which feed validity) and the val<i>.log.gz copies stay in-line, outside the
   deferrable blocks.
2. equivalence: run-cell.sh's own blocks (extracted from the script) in-line, and deferred + cell-endpass.sh,
   give the same val<i>.log.excerpt, val<i>.log.gz content, run.log "ingest:" line and summary.json
   .ingest.mempool_nonce_expired_evictions_per_node, also when the nodes log more evictions after the point
   where the in-line scan ran (the end pass reads each log only up to the recorded byte length).
3. cell-endpass.sh removes the marker, is a no-op on a second run, and keeps the marker (rc 1) when a log is
   missing or corrupt.
"""

import gzip
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
RUN_CELL_SH = os.path.join(HERE, "run-cell.sh")
SCANS = os.path.join(HERE, "log-scans.sh")
ENDPASS = os.path.join(HERE, "cell-endpass.sh")


def block(name):
    """run-cell.sh's text between '# [report-scans:NAME]' and '# [/report-scans:NAME]'"""
    src = open(RUN_CELL_SH).read()
    m = re.search(
        r"^# \[report-scans:%s\]\n(.*?)^# \[/report-scans:%s\]\n" % (name, name),
        src,
        re.M | re.S,
    )
    assert m, "run-cell.sh lost its report-scans:%s block" % name
    return m.group(1)


def node_log(i, early):
    """a node log with ANSI colours, eviction lines, excerpt matches (more than 400 on val1)"""
    L = []
    for k in range(30 if i != 1 else 450):
        L.append(
            "2026-10-19T10:00:%02d.000Z \x1b[33m WARN\x1b[0m torus_node: body fetch retry %d"
            % (k % 60, k)
        )
        L.append(
            "2026-10-19T10:00:%02d.000Z  INFO torus_mempool: evicted nonce-expired evicted=%d left=3"
            % (k % 60, k + i)
        )
        L.append(
            "2026-10-19T10:00:%02d.000Z  INFO torus_node: a quiet line %d" % (k % 60, k)
        )
    if not early:
        L.append(
            "2026-10-19T10:09:00.000Z  INFO torus_mempool: evicted nonce-expired evicted=1000 left=0 (after the scan)"
        )
        L.append(
            "2026-10-19T10:09:01.000Z \x1b[31mERROR\x1b[0m torus_node: shutdown error after the scan"
        )
    return "\n".join(L) + "\n"


class ClassificationTest(unittest.TestCase):
    def test_only_report_scans_are_deferrable(self):
        src = open(RUN_CELL_SH).read()
        blocks = block("ingest") + block("save")
        outside = src.replace(block("ingest"), "").replace(block("save"), "")
        # the knob's default and doc lines aside, DEFER_REPORT_SCANS only steers the two blocks
        uses = [
            ln
            for ln in outside.splitlines()
            if "DEFER_REPORT_SCANS" in ln and not ln.lstrip().startswith("#")
        ]
        self.assertEqual(
            [ln.strip()[:40] for ln in uses],
            [
                "DEFER_REPORT_SCANS=${DEFER_REPORT_SCANS:-0}"[:40],
                'case "$DEFER_REPORT_SCANS" in 0|1) ;; *)',
                "for kv in $EXTRA_ENV; do case $kv in DEFER_REPORT_SCANS=*|GENESIS_CACHE_DIR=*)"[:40],
            ],
            uses,
        )
        for inline in (
            "panics=$(sed -E",
            "errors=$(grep -c",
            "collect_logs.py",
            'crash-kill.sh" scan',
        ):
            self.assertIn(inline, outside, inline)
            self.assertNotIn(inline, blocks, inline)
        self.assertIn('gzip -c "$RUN_DIR/val$i.log" > "$OUT/val$i.log.gz"', blocks)
        self.assertNotRegex(
            blocks.split("gzip -c")[0].split("\n")[-1],
            r"DEFER",
            "gzip is never deferred",
        )
        out = subprocess.run(
            ["bash", "-c", '. "$1"; echo "$REPORT_SCANS"', "x", SCANS],
            capture_output=True,
            text=True,
        )
        self.assertEqual(out.stdout.split(), ["ingest-evictions", "log-excerpts"])

    def test_dissemination_feeds_validity(self):
        """why collect_logs.py stays in-line: summarize.py folds dissemination_clean into the validity verdict"""
        h = open(os.path.join(HERE, "health.py")).read()
        self.assertIn("dissemination failures", h)
        s = open(os.path.join(HERE, "summarize.py")).read()
        self.assertIn("dissem.get('dissemination_clean')", s)


class EquivalenceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="endpass-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def run_cell_blocks(self, defer):
        """run-cell.sh's ingest block, then later node log lines, then its save block"""
        run = os.path.join(self.tmp, "run-%d" % defer)
        out = os.path.join(self.tmp, "cell-%d" % defer)
        os.makedirs(run)
        os.makedirs(out)
        for i in range(3):
            with open(os.path.join(run, "val%d.log" % i), "w") as f:
                f.write(node_log(i, True))
        with open(os.path.join(out, "bench.log"), "w") as f:
            f.write("Submitted (load-gen accepted): 1,234,567\n")
        script = "\n".join(
            [
                "set -uo pipefail",
                "OUT=%s; RUN_DIR=%s; DEFER_REPORT_SCANS=%d" % (out, run, defer),
                'log() { printf "[%s] %s\\n" "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }',
                ". %s" % SCANS,
                block("ingest"),
                # the nodes keep logging (shutdown) after the in-line scan point
                'for i in 0 1 2; do printf \'%s\' "$LATE" >> "$RUN_DIR/val$i.log"; done',
                block("save"),
                'echo "EVICTED=[$EVICTED]"',
            ]
        )
        late_text = (
            node_log(0, False).split("\n")[-3]
            + "\n"
            + node_log(0, False).split("\n")[-2]
            + "\n"
        )
        r = subprocess.run(
            ["bash", "-c", script],
            capture_output=True,
            text=True,
            env=dict(os.environ, LATE=late_text),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        ev = re.search(r"EVICTED=\[(.*)\]", r.stdout).group(1)
        # summarize.py's .ingest (the in-line values, or [] when deferred: --evicted "")
        with open(os.path.join(out, "summary.json"), "w") as f:
            json.dump(
                {
                    "label": "x",
                    "ingest": {
                        "bench_submitted_actions": 1234567,
                        "mempool_nonce_expired_evictions_per_node": [
                            int(x) for x in ev.split()
                        ],
                        "note": "n",
                    },
                    "validity": {"verdict": "ACCEPT"},
                },
                f,
                indent=1,
            )
        return out, ev

    def ingest_lines(self, out):
        return [
            re.sub(r"^\[\d\d:\d\d:\d\d\] ", "", ln)
            for ln in open(os.path.join(out, "run.log")).read().splitlines()
            if "ingest: " in ln
        ]

    def test_deferred_plus_end_pass_equals_inline(self):
        a, ev_a = self.run_cell_blocks(0)
        b, ev_b = self.run_cell_blocks(1)
        self.assertEqual(ev_b, "")
        self.assertRegex(ev_a, r"^\d+ \d+ \d+ $")
        self.assertNotIn(
            "1000", ev_a.split(), "the in-line scan ran before the late lines"
        )
        self.assertTrue(os.path.exists(os.path.join(b, "deferred-scans.json")))
        self.assertFalse(os.path.exists(os.path.join(a, "deferred-scans.json")))
        for i in range(3):
            self.assertFalse(
                os.path.exists(os.path.join(b, "val%d.log.excerpt" % i)),
                "deferred: no excerpt in-line",
            )
        self.assertEqual(self.ingest_lines(b), [])
        self.assertIn(
            "ingest scan deferred to the campaign end pass",
            open(os.path.join(b, "run.log")).read(),
        )
        r = subprocess.run([ENDPASS, b], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("END PASS OK cell-1", r.stdout)
        for i in range(3):
            fa, fb = (os.path.join(d, "val%d.log.excerpt" % i) for d in (a, b))
            self.assertEqual(open(fa).read(), open(fb).read(), "excerpt val%d" % i)
            self.assertLessEqual(len(open(fa).read().splitlines()), 400)
            ga, gb = (
                gzip.open(os.path.join(d, "val%d.log.gz" % i)).read() for d in (a, b)
            )
            self.assertEqual(ga, gb)
            self.assertIn(b"after the scan", ga)
        self.assertEqual(
            len(open(os.path.join(a, "val1.log.excerpt")).read().splitlines()), 400
        )
        self.assertEqual(self.ingest_lines(a), self.ingest_lines(b))
        self.assertEqual(len(self.ingest_lines(a)), 1)
        sa, sb = (json.load(open(os.path.join(d, "summary.json"))) for d in (a, b))
        self.assertEqual(sa, sb)
        self.assertFalse(os.path.exists(os.path.join(b, "deferred-scans.json")))
        r = subprocess.run([ENDPASS, b], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0)
        self.assertIn("nothing deferred", r.stdout)
        self.assertEqual(len(self.ingest_lines(b)), 1, "a second run adds nothing")

    def test_missing_or_corrupt_log_keeps_the_marker(self):
        b, _ = self.run_cell_blocks(1)
        os.rename(os.path.join(b, "val2.log.gz"), os.path.join(b, "val2.log.gz.x"))
        r = subprocess.run([ENDPASS, b], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("END PASS FAIL", r.stdout)
        self.assertTrue(os.path.exists(os.path.join(b, "deferred-scans.json")))
        with open(os.path.join(b, "val2.log.gz"), "wb") as f:
            f.write(b"\x1f\x8bnot gzip")
        r = subprocess.run([ENDPASS, b], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertTrue(os.path.exists(os.path.join(b, "deferred-scans.json")))

    def test_without_summary_json(self):
        b, _ = self.run_cell_blocks(1)
        os.remove(os.path.join(b, "summary.json"))
        r = subprocess.run([ENDPASS, b], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("no summary.json", r.stdout)
        self.assertEqual(len(self.ingest_lines(b)), 1)


if __name__ == "__main__":
    unittest.main()
