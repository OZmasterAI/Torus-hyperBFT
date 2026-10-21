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
            hs = sorted(node.checkpoints)
            h = (
                params[0]
                if params and params[0] is not None
                else (hs[-1] if hs else None)
            )
            if h not in node.checkpoints:
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

    def run_scan(self, nodes, logs=None, wait_s=0.2):
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
