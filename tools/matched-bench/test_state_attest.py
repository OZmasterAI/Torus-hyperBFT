#!/usr/bin/env python3
"""Self-tests for state_attest.py (run-cell.sh STATE_HASH_ATTEST=1). Stub RPC / metrics servers on ephemeral
127.0.0.1 ports (never the devnet's 8645-8647 / 9161-9163), temp logs and key files. python3 -m unittest."""

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import state_attest as sa  # noqa: E402

KEYS_JSON = os.path.join(REPO, "devnet", "wsl", "bench-validator-keys.json")
ESC = "\x1b"


def h32(c):
    return "0x" + c * 64


class Node:
    """one stub node: checkpoints {height: (local, {addr: hash}, quorum)}, metrics text"""

    def __init__(self, checkpoints, metrics):
        self.checkpoints = checkpoints
        self.metrics = metrics
        self.on_call = None  # on_call(height or None) before each torus_getStateHash reply
        self.fail_heights = set()  # heights answered with an RPC error although retained


def make_handler(node):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def reply(self, body, ctype="application/json"):
            b = body.encode()
            self.send_response(200)
            self.send_header("content-type", ctype)
            self.send_header("content-length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)

        def do_GET(self):
            self.reply(node.metrics, "text/plain")

        def do_POST(self):
            req = json.loads(self.rfile.read(int(self.headers["content-length"])))
            assert req["method"] == "torus_getStateHash", req
            params = req.get("params") or []
            if node.on_call is not None:
                node.on_call(params[0] if params else None)
            hs = sorted(node.checkpoints)
            h = (
                params[0]
                if params and params[0] is not None
                else (hs[-1] if hs else None)
            )
            if h not in node.checkpoints or h in node.fail_heights:
                out = {
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "error": {
                        "code": -32602,
                        "message": "height %s is not a retained state hash checkpoint"
                        % h,
                    },
                }
            else:
                local, votes, quorum = node.checkpoints[h]
                out = {
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "result": {
                        "height": hex(h),
                        "localHash": local,
                        "votes": [
                            {"validator": a, "hash": v, "matchesLocal": v == local}
                            for a, v in sorted(votes.items())
                        ],
                        "quorumHash": quorum,
                    },
                }
            self.reply(json.dumps(out))

    return H


def metrics_text(submitted=12, mismatch=None, no_quorum=0, unverified=0, height=1200):
    lines = [
        "# HELP torus_state_hash_attestations_submitted Attestations submitted.",
        "# TYPE torus_state_hash_attestations_submitted counter",
        "torus_state_hash_attestations_submitted_total %s" % submitted,
        "torus_state_hash_no_quorum_total %s" % no_quorum,
        "torus_state_hash_height %s" % height,
        "torus_state_hash_unverified %s" % unverified,
    ]
    for v, n in (mismatch or {}).items():
        lines.append('torus_state_hash_mismatch_total{validator="%s"} %s' % (v, n))
    return "\n".join(lines) + "\n"


def log_text(addr, on="true", extra=(), ansi=True):
    e = ESC
    cfg = (
        "%s[2m2026-10-21T07:07:08Z%s[0m %s[32m INFO%s[0m %s[2mtorus_consensus::state_hash%s[0m%s[2m:%s[0m running state hash "
        "fail-stop (TORUS_STATE_HASH_FAILSTOP) %s[3mon%s[0m%s[2m=%s[0m%s\n"
        % (e, e, e, e, e, e, e, e, e, e, e, e, on)
        if ansi
        else "2026-10-21T07:07:08Z  INFO torus_consensus::state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=%s\n"
        % on
    )
    lines = [
        "2026-10-21T07:07:07Z  INFO torus: starting node\n",
        "2026-10-21T07:07:08Z  INFO torus: running state hash: automatic AttestStateHash enabled addr=%s\n"
        % addr,
        cfg,
    ]
    for h in range(100, 1300, 100):
        lines.append(
            "2026-10-21T07:08:00Z  INFO torus_consensus::state_hash: state hash attestation submitted height=%d validator=%s resubmission=false\n"
            % (h, addr)
        )
    lines += list(extra)
    return "".join(lines)


class Cluster:
    def __init__(self, test, nodes, logs):
        self.servers = []
        self.rpcs, self.mets = [], []
        for n in nodes:
            srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(n))
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            test.addCleanup(srv.server_close)
            test.addCleanup(srv.shutdown)
            url = "http://127.0.0.1:%d" % srv.server_address[1]
            self.rpcs.append(url)
            self.mets.append(url + "/metrics")
        self.logs = logs


class StateAttestTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="state-attest-test-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.keys = sa.read_keys(KEYS_JSON)
        self.addrs = [a for _, a, _ in self.keys]

    def checkpoints(self, lo=100, hi=1200, local=None, quorum=True, voters=None):
        """every height lo..hi: h_n local(h), every voter's vote = local, the quorum (= local) iff quorum(h)"""
        local = local or (lambda h: h32("%x" % (h // 100 % 16)))
        voters = self.addrs if voters is None else voters
        out = {}
        for h in range(lo, hi + 1, 100):
            q = quorum(h) if callable(quorum) else quorum
            out[h] = (local(h), {a: local(h) for a in voters}, local(h) if q else None)
        return out

    def run_scan(self, nodes, logs=None, wait_s=0.2, activation=None, hole_wait_s=0.0):
        paths = []
        for i, text in enumerate(logs or [log_text(a) for a in self.addrs]):
            p = os.path.join(self.d, "val%d.log" % i)
            with open(p, "w") as f:
                f.write(text)
            paths.append(p)
        c = Cluster(self, nodes, paths)
        return sa.scan(
            [sa.Rpc(u, timeout=5) for u in c.rpcs],
            self.keys,
            c.mets,
            c.logs,
            wait_s,
            sleep=0.05,
            activation=activation,
            hole_wait_s=hole_wait_s,
        )

    def three(self, **kw):
        return [Node(self.checkpoints(**kw), metrics_text()) for _ in range(3)]

    # ---------------------------------------------------------------- keys
    def test_keys_are_written_owner_only_with_the_registered_addresses(self):
        d = os.path.join(self.d, "run", "attest-keys")
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                os.path.join(HERE, "state_attest.py"),
                "keys",
                "--keys",
                KEYS_JSON,
                "--dir",
                d,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            r.stdout.split(), ["val%d=%s" % (i, a) for i, a in enumerate(self.addrs)]
        )
        self.assertEqual(stat.S_IMODE(os.stat(d).st_mode), 0o700)
        with open(KEYS_JSON) as f:
            want = {
                v["index"]: v["private_key"].lower().removeprefix("0x")
                for v in json.load(f)["validators"]
            }
        for i in range(3):
            p = os.path.join(d, "val%d.key" % i)
            self.assertEqual(stat.S_IMODE(os.stat(p).st_mode), 0o600)
            with open(p) as f:
                self.assertEqual(f.read().strip(), want[i])
        # the genesis registers exactly these addresses (gen-3val-genesis.sh reads the same file)
        self.assertEqual(len(set(self.addrs)), 3)

    def test_keys_file_without_three_validators_is_refused(self):
        p = os.path.join(self.d, "k.json")
        with open(p, "w") as f:
            json.dump(
                {
                    "validators": [
                        {
                            "index": 0,
                            "address": "0x" + "1" * 40,
                            "private_key": "0x" + "2" * 64,
                        }
                    ]
                },
                f,
            )
        with self.assertRaises(ValueError):
            sa.read_keys(p)

    # ---------------------------------------------------------------- scan
    def test_all_agree_with_quorum_is_ok(self):
        out = self.run_scan(self.three())
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual(out["compared"], 12)
        self.assertEqual(out["heights"], [100, 1200])
        self.assertTrue(out["hn_equal"])
        self.assertEqual(out["quorum_reached"], 12)
        self.assertEqual(
            out["votes_per_validator"], {"val0": 12, "val1": 12, "val2": 12}
        )
        self.assertEqual(out["newest"]["height"], 1200)
        self.assertTrue(all(e["agree"] and len(e["h_n"]) == 3 for e in out["table"]))
        for n in ("val0", "val1", "val2"):
            pn = out["per_node"][n]
            self.assertEqual(pn["submitted"], 12)
            self.assertEqual(pn["mismatch"], 0)
            self.assertEqual(pn["log"]["failstop_on"], [True])
            self.assertEqual(pn["log"]["submitted"], 12)
        line = sa.line_of(out)
        self.assertIn(
            "state attest: ok=1 compared 12 checkpoint(s) 100..1200 on 3 nodes, h_n equal=yes",
            line,
        )
        self.assertIn("quorum 12/12", line)

    def test_compares_only_heights_every_node_retains(self):
        nodes = self.three()
        nodes[1].checkpoints = self.checkpoints(lo=500)  # val1 pruned below 500
        nodes[2].checkpoints = self.checkpoints(hi=1100)  # val2's newest is 1100
        out = self.run_scan(nodes)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual(out["heights"], [500, 1100])
        self.assertEqual(out["partial_heights"], [400])

    def test_one_node_with_a_different_h_n_fails(self):
        nodes = self.three()
        cp = dict(nodes[2].checkpoints)
        bad = h32("e")
        loc, votes, q = cp[700]
        cp[700] = (
            bad,
            dict(votes),
            q,
        )  # val2 diverged at 700 (its quorum view != its h_n)
        nodes[2].checkpoints = cp
        out = self.run_scan(nodes)
        self.assertFalse(out["ok"])
        self.assertEqual(out["hn_differs_at"], [700])
        self.assertFalse(out["hn_equal"])
        text = " | ".join(out["problems"])
        self.assertIn(
            "h_n differs between nodes at 1 of 12 checkpoints, first 700", text
        )
        self.assertIn("val2=" + bad, text)
        self.assertIn("quorum hash differs from the node's h_n: val2@700", text)
        self.assertIn("ok=0", sa.line_of(out))

    def test_a_vote_that_differs_from_h_n_fails(self):
        nodes = self.three()
        for n in nodes:
            cp = dict(n.checkpoints)
            loc, votes, q = cp[300]
            votes = dict(votes)
            votes[self.addrs[1]] = h32("f")
            cp[300] = (loc, votes, q)
            n.checkpoints = cp
        out = self.run_scan(nodes)
        self.assertFalse(out["ok"])
        self.assertTrue(
            any(
                "on-chain attestation(s) differ" in p and "val1@300" in p
                for p in out["problems"]
            ),
            out["problems"],
        )

    def test_newest_without_quorum_is_pending_older_is_a_failure(self):
        out = self.run_scan(self.three(quorum=lambda h: h != 1200))
        self.assertTrue(out["ok"], out["problems"])
        self.assertTrue(out["pending_newest"])
        self.assertEqual(out["quorum_reached"], 11)
        self.assertEqual(
            out["votes_per_validator"], {"val0": 11, "val1": 11, "val2": 11}
        )
        out = self.run_scan(self.three(quorum=lambda h: h not in (1200, 800)))
        self.assertFalse(out["ok"])
        self.assertIn(
            "1 checkpoint(s) below the newest without an on-chain quorum on every node: 800",
            out["problems"],
        )

    def test_no_attestations_at_all_fails(self):
        nodes = [
            Node(self.checkpoints(quorum=False, voters=[]), metrics_text(submitted=0))
            for _ in range(3)
        ]
        logs = [
            "2026  INFO torus_consensus::state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false\n"
        ] * 3
        out = self.run_scan(nodes, logs)
        self.assertFalse(out["ok"])
        text = " | ".join(out["problems"])
        self.assertIn("no compared checkpoint reached an on-chain quorum", text)
        self.assertIn(
            "val0: torus_state_hash_attestations_submitted_total = 0.0 (the node sent no attestation)",
            text,
        )
        self.assertIn(
            "val1: no 'running state hash: automatic AttestStateHash enabled' line",
            text,
        )
        self.assertIn(
            "val2: fail-stop not live (TORUS_STATE_HASH_FAILSTOP on=[False])", text
        )

    def test_a_validator_missing_its_votes_fails(self):
        out = self.run_scan(self.three(quorum=True, voters=self.addrs[:2]))
        self.assertFalse(out["ok"])
        self.assertIn("val2 voted on 0 of the 12 quorum checkpoints", out["problems"])

    def test_wrong_attest_address_and_failstop_lines_fail(self):
        logs = [log_text(a) for a in self.addrs]
        logs[0] = log_text(self.addrs[1])
        logs[1] = log_text(
            self.addrs[1],
            extra=[
                "2026  ERROR torus_consensus::state_hash: STATE HASH MISMATCH: on-chain quorum hash differs from the local checkpoint\n",
                "2026  ERROR torus_consensus::state_hash: FAIL-STOP (TORUS_STATE_HASH_FAILSTOP=1): latching exec_failed, node stops voting\n",
            ],
            ansi=False,
        )
        out = self.run_scan(self.three(), logs)
        self.assertFalse(out["ok"])
        text = " | ".join(out["problems"])
        self.assertIn(
            "val0: attests as ['%s'], its validator address is %s"
            % (self.addrs[1], self.addrs[0]),
            text,
        )
        self.assertIn("val1: 1 STATE HASH MISMATCH log line(s)", text)
        self.assertIn("val1: 1 FAIL-STOP log line(s)", text)
        self.assertEqual(
            out["per_node"]["val1"]["log"]["failstop_on"], [True]
        )  # plain (non-ANSI) form parsed too

    def test_json_log_lines_are_parsed(self):
        p = os.path.join(self.d, "json.log")
        with open(p, "w") as f:
            f.write(
                '{"level":"INFO","fields":{"message":"running state hash: automatic AttestStateHash enabled","addr":"%s"}}\n'
                '{"level":"INFO","fields":{"message":"running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP)","on":true}}\n'
                '{"level":"INFO","fields":{"message":"state hash attestation submitted","height":100}}\n' % self.addrs[0]
            )
        lf = sa.log_facts(p)
        self.assertEqual(lf["attest_enabled_addrs"], [self.addrs[0]])
        self.assertEqual(lf["failstop_on"], [True])
        self.assertEqual(lf["submitted"], 1)

    def test_mismatch_metric_fails(self):
        nodes = self.three()
        nodes[0].metrics = metrics_text(mismatch={"quorum": 1, self.addrs[2]: 2})
        out = self.run_scan(nodes)
        self.assertFalse(out["ok"])
        self.assertIn("val0: torus_state_hash_mismatch_total = 3.0", out["problems"])

    def test_missing_no_quorum_or_unverified_metric_fails(self):
        """review nit 2: both are always exported; absent = the scrape is not the node's, not 'no problem'"""
        nodes = self.three()
        nodes[1].metrics = "\n".join(
            l for l in metrics_text().splitlines() if "no_quorum" not in l and "unverified" not in l
        ) + "\n"
        out = self.run_scan(nodes)
        self.assertFalse(out["ok"])
        self.assertIn("val1: torus_state_hash_no_quorum_total missing from the scrape", out["problems"])
        self.assertIn("val1: torus_state_hash_unverified missing from the scrape", out["problems"])

    def test_window_shorter_than_activation_allows_fails(self):
        """review nit 1: an RPC error / short retention must not shrink the compared window silently"""
        nodes = self.three()
        nodes[0].checkpoints = self.checkpoints(lo=1100)  # val0 serves only 1100, 1200
        out = self.run_scan(nodes, activation=1)
        self.assertFalse(out["ok"])
        self.assertEqual((out["compared"], out["min_compared"]), (2, 11))
        self.assertTrue(any(p.startswith("compared only 2 checkpoint(s), want >= 11") for p in out["problems"]), out["problems"])
        self.assertEqual(sa.min_window(1200, 1), 11)
        self.assertEqual(sa.min_window(100000, 1), 64)
        self.assertEqual(sa.min_window(150, 1), 1)
        self.assertEqual(sa.min_window(1200, None), 1)

    def test_full_window_with_activation_is_ok(self):
        out = self.run_scan(self.three(), activation=1)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual((out["compared"], out["min_compared"]), (12, 11))

    # bug 2 (9g-hn smoke 2026-10-21): the chain keeps committing during the scan, so every node's newest
    # checkpoint advances and RETAIN prunes the oldest one before the newest-first walk reaches it
    def advancing(self, at, steps=1, lo=200, hi=6500):
        """3 nodes retaining lo..hi (64 checkpoints at the defaults); the first walk read of height `at` on any
        node advances every node by `steps` checkpoints (adds the new newest, prunes to RETAIN)"""
        nodes = self.three(lo=lo, hi=hi)
        lock = threading.Lock()
        fired = []

        def advance(h):
            with lock:
                if h != at or fired:
                    return
                fired.append(h)
                for n in nodes:
                    for _ in range(steps):
                        top = max(n.checkpoints) + 100
                        n.checkpoints[top] = self.checkpoints(lo=top, hi=top)[top]
                        n.checkpoints.pop(top - 100 * sa.RETAIN)
        for n in nodes:
            n.on_call = advance
        return nodes

    def test_chain_advancing_during_the_walk_prunes_the_oldest_ok(self):
        nodes = self.advancing(at=3000)
        out = self.run_scan(nodes, activation=1)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual(out["compared"], 63)
        self.assertEqual(out["heights"], [300, 6500])
        self.assertEqual(out["latest_after_walk"], [6600, 6600, 6600])
        self.assertEqual((out["advanced_during_scan"], out["min_compared"]), (1, 63))

    def test_chain_advancing_two_checkpoints_ok(self):
        out = self.run_scan(self.advancing(at=3000, steps=2), activation=1)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual((out["compared"], out["min_compared"]), (62, 62))

    def test_nodes_apart_at_scan_start_window_is_the_common_retained_one(self):
        """val0 already one checkpoint ahead when T is read: it retains 300..6600, T = 6500 -> 63 common"""
        nodes = self.three(lo=200, hi=6500)
        nodes[0].checkpoints = self.checkpoints(lo=300, hi=6600)
        out = self.run_scan(nodes, activation=1)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual((out["compared"], out["min_compared"], out["advanced_during_scan"]), (63, 63, 1))

    def test_mid_window_rpc_failure_on_an_advancing_chain_fails(self):
        nodes = self.advancing(at=5000)
        nodes[1].fail_heights = {3000}
        out = self.run_scan(nodes, activation=1)
        self.assertFalse(out["ok"])
        self.assertEqual((out["compared"], out["min_compared"]), (35, 63))
        self.assertEqual(out["partial_heights"], [3000])
        self.assertTrue(any(p.startswith("compared only 35 checkpoint(s), want >= 63") for p in out["problems"]), out["problems"])

    def test_rpc_failure_just_above_the_pruned_bottom_fails(self):
        """the advance explains exactly one missing checkpoint (200), not a failure at 300 as well"""
        nodes = self.advancing(at=3000)
        nodes[2].fail_heights = {300}
        out = self.run_scan(nodes, activation=1)
        self.assertFalse(out["ok"])
        self.assertEqual((out["compared"], out["min_compared"]), (62, 63))

    def test_latest_unreadable_after_the_walk_keeps_the_full_window(self):
        """no proof of an advance (latest unreadable after the walk): the window is not shortened"""
        nodes = self.advancing(at=3000)
        calls = {"n": 0}
        inner = nodes[0].on_call

        def hook(h):
            inner(h)
            if h is None:
                calls["n"] += 1
                if calls["n"] == 2:  # the post-walk latest read
                    nodes[0].fail_heights = set(nodes[0].checkpoints)
        nodes[0].on_call = hook
        out = self.run_scan(nodes, activation=1)
        self.assertFalse(out["ok"])
        self.assertEqual(out["latest_after_walk"][0], None)
        self.assertEqual((out["advanced_during_scan"], out["min_compared"]), (0, 64))

    def hole(self, nodes, h, voters):
        """checkpoint h without val2's vote and without a quorum on every node"""
        for n in nodes:
            local, _, _ = n.checkpoints[h]
            n.checkpoints[h] = (local, {a: local for a in voters}, None)

    def test_hole_below_newest_heals_within_the_resubmit_wait(self):
        """9j2 smoke: val2's attestation for 3000 was skipped as a replay (oracle feed, same key, same-ms nonce);
        the node re-signs after 65 s, so the scan waits for the hole instead of failing at once"""
        nodes = self.three()
        self.hole(nodes, 700, self.addrs[:2])
        calls = {"n": 0}

        def heal(h):
            if h == 700:
                calls["n"] += 1
                if calls["n"] > 6:  # the resubmission lands after a few re-reads
                    for n in nodes:
                        local = n.checkpoints[700][0]
                        n.checkpoints[700] = (local, {a: local for a in self.addrs}, local)

        nodes[0].on_call = heal
        out = self.run_scan(nodes, hole_wait_s=5)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual(out["holes_initial"], [700])
        self.assertEqual(out["holes_healed"], [700])
        self.assertEqual(out["quorum_reached"], 12)
        self.assertEqual(out["votes_per_validator"], {"val0": 12, "val1": 12, "val2": 12})

    def test_hole_that_never_heals_still_fails(self):
        nodes = self.three()
        self.hole(nodes, 700, self.addrs[:2])
        out = self.run_scan(nodes, hole_wait_s=0.3)
        self.assertFalse(out["ok"])
        self.assertEqual(out["holes_initial"], [700])
        self.assertEqual(out["holes_healed"], [])
        self.assertGreaterEqual(out["hole_wait_s"], 0.3)
        self.assertTrue(any("without an on-chain quorum on every node: 700" in p for p in out["problems"]), out["problems"])

    def test_initial_vote_mismatch_at_a_hole_is_never_healed_away(self):
        """Codex P1 probe: a hole whose first read shows a vote differing from h_n must fail even if a later read
        is clean with a quorum"""
        nodes = self.three()
        for n in nodes:
            local = n.checkpoints[700][0]
            bad = {self.addrs[0]: local, self.addrs[1]: h32("e")}
            n.checkpoints[700] = (local, bad, None)

        def heal(h):
            if h == 700:
                for n in nodes:
                    local = n.checkpoints[700][0]
                    n.checkpoints[700] = (local, {a: local for a in self.addrs}, local)

        reads = {"n": 0}

        def on_call(h):
            if h == 700:
                reads["n"] += 1
                if reads["n"] > 3:  # after the initial walk read
                    heal(h)

        nodes[0].on_call = on_call
        out = self.run_scan(nodes, hole_wait_s=3)
        self.assertFalse(out["ok"])
        self.assertEqual(out["holes_healed"], [])
        self.assertEqual(out["holes_not_retried"], [700])
        self.assertTrue(any("differ from a node's h_n" in p for p in out["problems"]), out["problems"])

    def test_retry_that_shows_a_mismatch_keeps_it_as_evidence(self):
        nodes = self.three()
        self.hole(nodes, 700, self.addrs[:2])
        reads = {"n": 0}

        def on_call(h):
            if h == 700:
                reads["n"] += 1
                if reads["n"] > 3:  # the retry sees a quorum hash that is not the node's h_n
                    for n in nodes:
                        local = n.checkpoints[700][0]
                        n.checkpoints[700] = (local, {a: local for a in self.addrs}, h32("d"))

        nodes[0].on_call = on_call
        out = self.run_scan(nodes, hole_wait_s=3)
        self.assertFalse(out["ok"])
        self.assertEqual(out["holes_healed"], [])
        self.assertEqual(out["holes_not_retried"], [700])
        self.assertTrue(any("quorum hash differs" in p for p in out["problems"]), out["problems"])

    def test_retry_mismatch_is_kept_when_a_peer_errors(self):
        """Codex P1 probe on 3ea3ddfd: a bad vote on val0 and an RPC error on val2 in the same retry must fail
        the cell, even if a later retry is clean with a quorum"""
        nodes = self.three()
        self.hole(nodes, 700, self.addrs[:2])
        reads = {"n": 0}

        def on_call(h):
            if h != 700:
                return
            reads["n"] += 1
            if reads["n"] == 4:  # first retry: val0 reports a vote differing from h_n, val2 errors
                local = nodes[0].checkpoints[700][0]
                nodes[0].checkpoints[700] = (local, {self.addrs[0]: local, self.addrs[1]: h32("e")}, None)
                nodes[2].fail_heights.add(700)
            elif reads["n"] == 5:  # later retries: clean with a quorum everywhere
                nodes[2].fail_heights.discard(700)
                for n in nodes:
                    local = n.checkpoints[700][0]
                    n.checkpoints[700] = (local, {a: local for a in self.addrs}, local)

        nodes[0].on_call = on_call
        out = self.run_scan(nodes, hole_wait_s=3)
        self.assertFalse(out["ok"])
        self.assertEqual(out["holes_healed"], [])
        self.assertEqual(out["holes_not_retried"], [700])
        self.assertTrue(any("differ from a node's h_n" in p for p in out["problems"]), out["problems"])

    def test_hole_wait_budget_is_an_absolute_deadline_with_slow_rpcs(self):
        """Codex P2 probe: slow replies over several holes must not stretch the wait past the budget"""
        nodes = self.three()
        for h in (300, 500, 700, 900):
            self.hole(nodes, h, self.addrs[:2])

        def slow(h):
            if h in (300, 500, 700, 900):
                time.sleep(0.4)  # 4 holes x 3 nodes x 0.4 s = 4.8 s per polling pass

        for n in nodes:
            n.on_call = slow
        out = self.run_scan(nodes, hole_wait_s=1.0)
        self.assertFalse(out["ok"])
        self.assertEqual(sorted(out["holes_initial"]), [300, 500, 700, 900])
        self.assertLessEqual(out["hole_wait_s"], 1.6)  # one in-flight request may finish past the deadline

    def test_no_hole_wait_by_default(self):
        nodes = self.three()
        self.hole(nodes, 700, self.addrs[:2])
        out = self.run_scan(nodes)
        self.assertFalse(out["ok"])
        self.assertEqual(out["hole_wait_s"], 0.0)

    def test_newest_without_quorum_is_not_a_hole(self):
        nodes = self.three(quorum=lambda h: h != 1200)
        out = self.run_scan(nodes, hole_wait_s=5)
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual(out["holes_initial"], [])
        self.assertEqual(out["hole_wait_s"], 0.0)

    def test_unreachable_node_fails(self):
        nodes = self.three()
        c = Cluster(self, nodes, [])
        rpcs = [sa.Rpc(u, timeout=2) for u in c.rpcs[:2]] + [
            sa.Rpc("http://127.0.0.1:9/", timeout=1)
        ]
        out = sa.scan(rpcs, self.keys, [], [], 0.1, sleep=0.05)
        self.assertFalse(out["ok"])
        self.assertTrue(
            out["problems"][0].startswith("val2: no retained checkpoint readable"),
            out["problems"],
        )
        self.assertEqual(out["compared"], 0)

    # ---------------------------------------------------------------- summary
    def write_cell(self, scan):
        with open(os.path.join(self.d, "summary.json"), "w") as f:
            json.dump(
                {
                    "status": "OK",
                    "validity": {
                        "verdict": "ACCEPT",
                        "accepted": True,
                        "fail_reasons": [],
                    },
                    "headline": {"benchmark_accepted": True},
                },
                f,
            )
        if scan is not None:
            with open(os.path.join(self.d, "state-attest.json"), "w") as f:
                json.dump(scan, f)

    def summary(self):
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                os.path.join(HERE, "state_attest.py"),
                "summary",
                "--dir",
                self.d,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(self.d, "summary.json")) as f:
            return json.load(f)

    def test_summary_ok_keeps_the_verdict(self):
        self.write_cell(self.run_scan(self.three()))
        d = self.summary()
        s = d["state_attest"]
        self.assertTrue(s["ok"])
        self.assertEqual(s["compared"], 12)
        self.assertEqual(s["file"], "state-attest.json")
        self.assertEqual(s["per_node"]["val0"]["attest_address"], self.addrs[0])
        self.assertEqual(s["per_node"]["val2"]["failstop_on"], [True])
        self.assertEqual(d["validity"]["verdict"], "ACCEPT")
        self.assertNotIn("table", s)  # the per-height table stays in state-attest.json

    def test_summary_problems_reject_idempotently(self):
        nodes = self.three()
        cp = dict(nodes[1].checkpoints)
        cp[1200] = (h32("d"), cp[1200][1], cp[1200][2])
        nodes[1].checkpoints = cp
        self.write_cell(self.run_scan(nodes))
        d = self.summary()
        d = self.summary()
        self.assertEqual(d["validity"]["verdict"], "REJECT")
        self.assertEqual(d["status"], "INVALID")
        self.assertFalse(d["headline"]["benchmark_accepted"])
        reasons = [r for r in d["validity"]["fail_reasons"] if r.startswith(sa.REASON)]
        self.assertEqual(len(reasons), 1, d["validity"]["fail_reasons"])
        self.assertIn("h_n differs between nodes", reasons[0])

    def test_summary_without_a_scan_rejects(self):
        self.write_cell(None)
        d = self.summary()
        self.assertFalse(d["state_attest"]["ok"])
        self.assertEqual(d["validity"]["verdict"], "REJECT")


if __name__ == "__main__":
    unittest.main()
