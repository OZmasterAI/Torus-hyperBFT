#!/usr/bin/env python3
"""Dissemination verdict in KILL_NO_RESTART cells (bench 9k2, s138). python3 -m unittest.

A validator SIGKILLed without restart makes the survivors' sends to it fail (native-da / block-data OUTBOUND
FAILURE peer=<its id>, once its QUIC connection times out). Those failures are expected and must not REJECT the
cell. Only a line that itself names the killed node as its peer, dated at or after the kill, is exempt.
Everything else stays strict: a failure toward the OTHER survivor, a failure toward the killed node before its
kill, a failure whose line names no peer, every body / justify fetch fallback (the fetch rotates across all other
validators before it falls back to sync, so the surviving peer failed too, whoever proposed the block) and any
cell without the per-line evidence all still count. A cell without a kill is judged exactly as before.

Per-peer evidence (collect_logs.py, from the node logs run-cell.sh keeps as val<i>.log.gz):
  * each node's own `local_peer_id=` startup line maps peer ids to val0/val1/val2;
  * OUTBOUND FAILURE lines name the peer they failed toward (`peer=<id>`);
  * body / justify fetch fallbacks name only the block hash; its proposer (the node whose log re-sent that
    block's header, "header re-send: view=V block=<7-char base64 prefix> ...") is recorded for the report
    only and never exempts the line.
"""

import base64
import datetime
import gzip
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from collect_logs import collect_all  # noqa: E402
from health import kill_dissemination  # noqa: E402
from test_harness import BENCH_START, run_summarize  # noqa: E402
from test_kill_norestart import KILL_AT, write_kill_cell  # noqa: E402

PIDS = {
    "val0": "12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X",
    "val1": "12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3",
    "val2": "12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo",
}
KILL_TS = BENCH_START + KILL_AT + 0.5  # write_kill_record's default kill_ts
HASH_V1 = bytes(range(1, 33))  # a block val1 proposed (val1 re-sent its header)
HASH_V2 = bytes(range(101, 133))  # a block val2 proposed (val2 re-sent its header)
HASH_NONE = bytes(32)  # a block nobody re-sent (genesis justify): proposer unknown


def stamp(t):
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%S.%fZ"
    )


def prefix(h):
    return base64.b64encode(h).decode()[:7]


def line(t, text, level="INFO", target="torus_network::swarm"):
    return "\x1b[2m%s\x1b[0m \x1b[33m%s\x1b[0m %s: %s\n" % (
        stamp(t),
        level,
        target,
        text,
    )


def startup(node):
    return line(
        BENCH_START - 30, "local_peer_id=%s" % PIDS[node], target="libp2p_swarm"
    )


def outbound(t, peer, kind="native-da"):
    return line(
        t,
        "%s OUTBOUND FAILURE peer=%s error=Io(Custom { kind: NotConnected, error: "
        "ConnectionLost(TimedOut) })" % (kind, PIDS[peer]),
        level="WARN",
    )


def body_fallback(t, h):
    return line(
        t,
        "body fetch exhausted 9 retries (proposer + rotation) for %s — falling back to sync"
        % list(h),
        level="WARN",
        target="hotstuff_rs::hotstuff::implementation",
    )


def justify_fallback(t, h):
    return line(
        t,
        "justify fetch exhausted 9 retries for %s — falling back to sync" % list(h),
        level="WARN",
        target="hotstuff_rs::hotstuff::implementation",
    )


def resend(t, h, view=2508):
    return line(
        t,
        "header re-send: view=%d block=%s to 2 validators (still in own view)"
        % (view, prefix(h)),
        target="hotstuff_rs::hotstuff::implementation",
    )


def write_logs(d, extra, gz=False):
    """val<i>.log (or .log.gz) per node: its local_peer_id line, some noise, then extra[node] lines."""
    paths = []
    for node in ("val0", "val1", "val2"):
        text = (
            startup(node)
            + line(BENCH_START, "FULL-BODY push bytes=10")
            + "".join(extra.get(node, ()))
        )
        p = Path(d) / ("%s.log.gz" % node if gz else "%s.log" % node)
        if gz:
            with gzip.open(p, "wt") as f:
                f.write(text)
        else:
            p.write_text(text)
        paths.append(p)
    return paths


def summary_of(d, extra, gz=False):
    return collect_all(write_logs(d, extra, gz))


AFTER = (
    KILL_TS + 10
)  # the survivors' QUIC connection to the dead node times out ~10 s after the kill


class CollectAttributionTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="kd-coll-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def test_counts_and_stdout_line_are_unchanged_and_each_failure_line_is_recorded(
        self,
    ):
        s = summary_of(
            self.d,
            {
                "val0": [outbound(AFTER, "val2")] * 3 + [body_fallback(AFTER, HASH_V1)],
                "val1": [resend(AFTER - 5, HASH_V1)],
            },
        )
        c0 = s["val0"]["counts"]
        self.assertEqual(
            (
                c0["da_outbound_fail"],
                c0["exhausted"],
                c0["sync_fallback"],
                c0["body_push"],
            ),
            (3, 1, 1, 1),
        )
        self.assertEqual(s["val0"]["local_peer_id"], PIDS["val0"])
        fails = s["val0"]["failures"]
        self.assertEqual(
            len(fails), 4
        )  # one record per LINE (the fallback line is exhausted + sync_fallback)
        self.assertEqual(fails[0]["kinds"], ["da_outbound_fail"])
        self.assertEqual(fails[0]["peer_id"], PIDS["val2"])
        self.assertAlmostEqual(fails[0]["ts"], AFTER, places=3)
        self.assertEqual(fails[3]["kinds"], ["exhausted", "sync_fallback"])
        self.assertEqual(fails[3]["block"], prefix(HASH_V1))
        self.assertEqual(
            fails[3]["proposers"], ["val1"]
        )  # resolved across the three logs
        self.assertNotIn(
            "resent", s["val1"]
        )  # the re-send prefix set is not written out

    def test_gzipped_logs_give_the_same_summary(self):
        extra = {"val0": [outbound(AFTER, "val2"), justify_fallback(AFTER, HASH_NONE)]}
        plain = summary_of(self.d, extra)
        sub = os.path.join(self.d, "gz")
        os.mkdir(sub)
        packed = summary_of(sub, extra, gz=True)
        for n in ("val0", "val1", "val2"):
            self.assertEqual(plain[n]["counts"], packed[n]["counts"])
            self.assertEqual(plain[n]["failures"], packed[n]["failures"])
        self.assertEqual(packed["val0"]["failures"][1]["proposers"], [])


class KillDisseminationTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="kd-unit-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def judge(self, extra, killed="val2", kill_ts=KILL_TS):
        return kill_dissemination(summary_of(self.d, extra), killed, kill_ts)

    def test_failures_only_toward_the_killed_node_after_its_kill_are_exempt(self):
        r = self.judge(
            {
                "val0": [outbound(AFTER, "val2")] * 8,
                "val1": [outbound(AFTER + 1, "val2", "block-data")],
            }
        )
        self.assertIs(r["clean"], True, r)
        self.assertEqual(r["exempt_toward_killed"], 9)
        self.assertEqual(r["live_failures"], 0)

    def test_failures_between_the_survivors_still_reject(self):
        r = self.judge(
            {"val0": [outbound(AFTER, "val2")] * 8 + [outbound(AFTER, "val1")]}
        )
        self.assertIs(r["clean"], False)
        self.assertEqual(r["live_failures"], 1)
        self.assertEqual(r["exempt_toward_killed"], 8)
        self.assertEqual(r["live_by_node"]["val0"]["toward"], {"val1": 1})

    def test_a_fallback_for_a_block_the_other_survivor_proposed_rejects(self):
        r = self.judge(
            {
                "val0": [body_fallback(AFTER, HASH_V1)],
                "val1": [resend(AFTER - 5, HASH_V1)],
            }
        )
        self.assertIs(r["clean"], False)
        self.assertEqual(r["live_by_node"]["val0"]["toward"], {"val1": 1})

    def test_a_fallback_for_a_block_only_the_killed_node_proposed_still_rejects(self):
        # Codex review: a body fetch rotates across every other validator before it
        # falls back to sync, so the fallback means the surviving peer failed too.
        # Only a line that itself names the killed node as its peer is exempt.
        for fallback in (body_fallback, justify_fallback):
            r = self.judge(
                {
                    "val0": [fallback(AFTER, HASH_V2)],
                    "val2": [resend(KILL_TS - 1, HASH_V2)],
                }
            )
            self.assertIs(r["clean"], False, r)
            self.assertEqual(r["exempt_toward_killed"], 0)
            self.assertEqual(r["live_by_node"]["val0"]["lines"], 1)


    def test_failures_toward_the_killed_node_before_its_kill_still_reject(self):
        r = self.judge({"val0": [outbound(KILL_TS - 1, "val2")]})
        self.assertIs(r["clean"], False)

    def test_unattributed_failures_still_reject(self):
        for extra in (
            {"val0": [justify_fallback(AFTER, HASH_NONE)]},  # no proposer evidence
            {
                "val0": [
                    line(
                        AFTER, "native-da OUTBOUND FAILURE peer=12D3KooWunknown", "WARN"
                    )
                ]
            },
            {"val0": [line(AFTER, "header-first body starvation", "WARN")]},
        ):
            r = self.judge(extra)
            self.assertIs(r["clean"], False, extra)
            self.assertEqual(r["live_by_node"]["val0"]["toward"], {"unknown": 1})

    def test_the_killed_nodes_own_pre_kill_failures_count(self):
        r = self.judge({"val2": [outbound(KILL_TS - 2, "val0")]})
        self.assertIs(r["clean"], False)

    def test_evidence_that_does_not_cover_every_counted_line_is_strict(self):
        s = summary_of(self.d, {"val0": [outbound(AFTER, "val2")] * 2})
        del s["val0"]["failures"][1]  # e.g. truncated
        self.assertIs(kill_dissemination(s, "val2", KILL_TS)["clean"], False)
        s = summary_of(self.d, {"val0": [outbound(AFTER, "val2")] * 2})
        for n in s:  # an old log-summary.json: counts only, no per-line records
            s[n].pop("failures")
        self.assertIs(kill_dissemination(s, "val2", KILL_TS)["clean"], False)
        s = summary_of(self.d, {"val0": [outbound(AFTER, "val2")] * 2})
        r = kill_dissemination(
            s, "val2", KILL_TS, counts={"val0": {"da_outbound_fail": 3}}
        )
        self.assertIs(
            r["clean"], False
        )  # the --dissem counts disagree with the log summary

    def test_no_failures_is_clean_and_no_kill_is_not_judged_here(self):
        self.assertIs(self.judge({})["clean"], True)
        self.assertIsNone(
            kill_dissemination(
                summary_of(self.d, {"val0": [outbound(AFTER, "val2")]}), None, None
            )
        )


def dissem_arg(log_summary):
    return [
        "--dissem",
        " ".join(
            n
            + ":"
            + ",".join(
                "%s=%d" % (k, log_summary[n]["counts"][k])
                for k in (
                    "exhausted",
                    "sync_fallback",
                    "da_outbound_fail",
                    "starvation",
                )
            )
            for n in ("val0", "val1", "val2")
        ),
    ]


class SummarizeKillDissemTest(unittest.TestCase):
    """run-cell.sh: collect_logs.py writes log-summary.json and prints the counts summarize.py gets as --dissem."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="kd-summ-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def summ(self, extra, killed=True):
        write_kill_cell(
            self.d,
            killed=killed,
            agree_rows=("val0", "val1") if killed else ("val0", "val1", "val2"),
        )
        logs = os.path.join(self.d, "logs")
        os.mkdir(logs)
        ls = summary_of(logs, extra)
        Path(self.d, "log-summary.json").write_text(json.dumps(ls))
        heights = "1200 1201" if killed else "1200 1201 1202"
        s, _ = run_summarize(
            self.d,
            dur=120,
            extra=dissem_arg(ls)
            + ["--digest-quiescent", "1", "--digest-heights", heights],
        )
        return s

    def test_kill_cell_failing_only_toward_the_killed_node_accepts(self):
        s = self.summ({"val0": [outbound(AFTER, "val2")] * 8})
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        dis = s["dissemination"]
        self.assertTrue(dis["dissemination_clean"])
        self.assertEqual(dis["total_failures"], 8)  # the raw count is kept
        self.assertEqual(dis["kill_exemption"]["exempt_toward_killed"], 8)
        self.assertEqual(dis["kill_exemption"]["killed_node"], "val2")
        self.assertTrue(s["headline"]["dissemination_clean"])

    def test_kill_cell_with_failures_between_the_survivors_rejects(self):
        # val0: 8 sends to the dead val2 (exempt) + a fallback for a block val1 proposed (a survivor)
        s = self.summ(
            {
                "val0": [outbound(AFTER, "val2")] * 8 + [body_fallback(AFTER, HASH_V1)],
                "val1": [resend(AFTER - 3, HASH_V1)],
            }
        )
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertIn("dissemination failures", s["validity"]["fail_reasons"])
        self.assertFalse(s["dissemination"]["dissemination_clean"])
        self.assertEqual(s["dissemination"]["kill_exemption"]["live_failures"], 1)

    def test_kill_cell_with_a_fallback_on_a_block_the_killed_node_proposed_rejects(self):
        s = self.summ(
            {
                "val0": [outbound(AFTER, "val2")] * 8 + [body_fallback(AFTER, HASH_V2)],
                "val2": [resend(KILL_TS - 1, HASH_V2)],
            }
        )
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertIn("dissemination failures", s["validity"]["fail_reasons"])
        self.assertEqual(s["dissemination"]["kill_exemption"]["exempt_toward_killed"], 8)
        self.assertEqual(s["dissemination"]["kill_exemption"]["live_failures"], 1)

    def test_kill_cell_without_a_log_summary_stays_strict(self):
        write_kill_cell(self.d)
        counts = "exhausted=0,sync_fallback=0,da_outbound_fail=%d,starvation=0"
        s, _ = run_summarize(
            self.d,
            dur=120,
            extra=[
                "--dissem",
                "val0:%s val1:%s val2:%s" % (counts % 8, counts % 0, counts % 0),
                "--digest-quiescent",
                "1",
                "--digest-heights",
                "1200 1201",
            ],
        )
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertFalse(s["dissemination"]["dissemination_clean"])

    def test_no_kill_cell_is_unchanged(self):
        s = self.summ({"val0": [outbound(AFTER, "val2")] * 8}, killed=False)
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertIn("dissemination failures", s["validity"]["fail_reasons"])
        self.assertNotIn("kill_exemption", s["dissemination"])

    def test_no_kill_cell_without_failures_still_accepts(self):
        s = self.summ({}, killed=False)
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertNotIn("kill_exemption", s["dissemination"])


if __name__ == "__main__":
    unittest.main()
