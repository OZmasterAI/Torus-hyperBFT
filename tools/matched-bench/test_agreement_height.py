#!/usr/bin/env python3
"""test_agreement_height.py — run-cell.sh's agreement step compares a block every node SERVES.

    python3 -m pytest -q tools/matched-bench/test_agreement_height.py

eth_getBlockByNumber answers null above eth_blockNumber = min(applied marker, committed). Item 6 Phase 3+
writes the applied marker only at checkpoints (15 s, ~160 blocks at 10 markets), so the old compare
height HMIN-5 (HMIN from torus_block_height = committed) could sit above it: the block hash read "ERR"
on every node and seven ozarchy-p4gate 10-market cells were DISAGREE with equal state digests on all
three nodes. Pinned here, against loopback stub nodes (no devnet, no cargo):
  * the compare height is at or below EVERY node's served head (min over nodes), and every node then
    returns a hash for it;
  * an older binary (marker every block, served head >= HMIN-5) keeps HMIN-5;
  * a node whose head does not read is left out of the min (WARNING), so its block read still fails
    and the cell stays DISAGREE; a block a node should serve but does not is logged as an RPC failure;
  * a genuine hash mismatch at a served height is still DISAGREE.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
RUN_CELL_SH = os.path.join(HERE, "run-cell.sh")


def make_node(head, hash_prefix="0xh", head_error=False, broken_blocks=False):
    """One stub validator: eth_blockNumber = head (or a JSON-RPC error), eth_getBlockByNumber = a header for
    any height <= head and null above it (the node's eth_header contract), or always null (broken_blocks)."""

    class Node(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        calls = []

        def log_message(self, *_a):
            pass

        def do_POST(self):
            req = json.loads(self.rfile.read(int(self.headers["content-length"])))
            m, p = req.get("method"), req.get("params", [])
            Node.calls.append((m, p))
            if m == "eth_blockNumber":
                if head_error:
                    resp = {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "error": {"code": -32603, "message": "x"},
                    }
                else:
                    resp = {"jsonrpc": "2.0", "id": 1, "result": hex(head)}
            elif m == "eth_getBlockByNumber":
                h = int(p[0], 16)
                res = None
                if h <= head and not broken_blocks:
                    res = {
                        "hash": "%s%x" % (hash_prefix, h),
                        "stateRoot": "0x0",
                        "number": p[0],
                    }
                resp = {"jsonrpc": "2.0", "id": 1, "result": res}
            else:
                resp = {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32601, "message": "no"},
                }
            body = json.dumps(resp).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    srv = ThreadingHTTPServer(("127.0.0.1", 0), Node)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, "http://127.0.0.1:%d" % srv.server_address[1], Node


class AgreementCompareHeightTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(RUN_CELL_SH) as f:
            cls.src = f.read()

    def fn(self, start):
        """A top-level bash function from run-cell.sh, verbatim: from `start` to its closing line."""
        i = self.src.index("\n" + start) + 1
        first = self.src[i : self.src.index("\n", i) + 1]
        if first.rstrip().endswith("}"):  # one-liner: log() { ...; }
            return first
        return self.src[i : self.src.index("\n}\n", i) + 3]

    def agreement_block(self):
        """run-cell.sh from the compare-height pick up to (not incl.) the agreement.jsonl dump: the compare
        height and the per-node block fetch, run verbatim against the stub nodes."""
        i = self.src.index('agree_cmp_height "$HCMP"')
        j = self.src.index('cat "$OUT/agreement.jsonl" >> "$OUT/run.log"', i)
        return self.src[i:j]

    def run_step(self, nodes, hgt, live=(0, 1, 2), rpcs=None):
        """Runs the agreement step (compare height + block fetch) against `nodes` (make_node kwargs) with
        metrics heights `hgt`; returns (agreement rows, run.log text)."""
        servers = [make_node(**n) for n in nodes]
        out = tempfile.mkdtemp(prefix="agree-h-")
        run_dir = tempfile.mkdtemp(prefix="agree-run-")
        for d in (out, run_dir):
            self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        open(os.path.join(out, "run.log"), "w").close()
        for i in range(3):
            with open(os.path.join(out, "metrics-after-val%d.txt" % i), "w") as f:
                f.write(
                    "torus_orders_matched_total 10\ntorus_orders_placed_accepted_total 20\n"
                    "torus_orders_resting_total 3\ntorus_native_actions_processed_total 30\n"
                    "torus_exec_resident_rebuilds_total 1\n"
                )
            open(os.path.join(run_dir, "val%d.log" % i), "w").close()
        hmin = min(hgt)
        script = "\n".join(
            [
                "set -u",
                "OUT=%s RUN_DIR=%s RPC_TIMEOUT=10" % (out, run_dir),
                self.fn("log() {"),
                self.fn("mval() {"),
                self.fn("rpc() { #"),
                self.fn("agree_cmp_height() {"),
                "RPCS=(%s)" % " ".join(rpcs or [s[1] for s in servers]),
                # run-cell's survivors (KILL_NO_RESTART) or every node, and its LIVE_RPCS line verbatim
                "LIVE_IDX=(%s)" % " ".join(map(str, live)),
                next(l for l in self.src.splitlines() if l.startswith("LIVE_RPCS=(); for i in")),
                "declare -a HGT=(%s) DHGT=(%s) DIGSECS=(0 0 0) DIGSHA=(d d d)"
                % (" ".join(map(str, hgt)), " ".join(map(str, hgt))),
                "HCMP=$(( %d - 5 )); HHEX=$(printf '0x%%x' \"$HCMP\")" % hmin,
                self.agreement_block(),
            ]
        )
        try:
            r = subprocess.run(
                ["bash", "-c", script], capture_output=True, text=True, timeout=120
            )
        finally:
            for s in servers:
                s[0].shutdown()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(os.path.join(out, "agreement.jsonl")) as f:
            rows = [json.loads(line) for line in f if line.strip()]
        with open(os.path.join(out, "run.log")) as f:
            log = f.read()
        return rows, log

    def test_p4gate_marker_lag_compares_at_the_lowest_served_head(self):
        """ozarchy-p4gate-10m-m1-b: heights 2037 2036 2037 -> old compare 2031, but the applied markers sat
        lower (one checkpoint behind). The compare height is the lowest served head and every node hashes it."""
        rows, log = self.run_step(
            [dict(head=1950), dict(head=2010), dict(head=1990)], [2037, 2036, 2037]
        )
        self.assertEqual([r["cmp_height"] for r in rows], [1950] * 3, log)
        self.assertEqual({r["block_hash"] for r in rows}, {"0xh%x" % 1950}, log)
        self.assertIn("comparing block 1950", log)

    def test_one_node_behind_alone_lowers_the_compare_height(self):
        """ozarchy-p4gate-10m-m3-b: only val2 read ERR. min over EVERY node: the one lagging marker decides."""
        rows, log = self.run_step(
            [dict(head=2071), dict(head=2071), dict(head=2060)], [2071, 2071, 2070]
        )
        self.assertEqual([r["cmp_height"] for r in rows], [2060] * 3, log)
        self.assertNotIn("ERR", {r["block_hash"] for r in rows}, log)

    def test_kill_no_restart_compares_the_survivors_only(self):
        """KILL_NO_RESTART=val1: survivors val0 + val2. val1 (lowest head here) is not asked; the compare height
        is the survivors' lowest served head, and val2's served head is read at ITS position in LIVE_RPCS"""
        rows, log = self.run_step([dict(head=1990), dict(head=1950), dict(head=2000)], [2037, 2036, 2037],
                                  live=(0, 2))
        self.assertEqual([r["node"] for r in rows], ["val0", "val2"], log)
        self.assertEqual([r["cmp_height"] for r in rows], [1990, 1990], log)
        self.assertNotIn("ERR", {r["block_hash"] for r in rows}, log)
        self.assertNotIn("ERROR", log)

    def test_kill_no_restart_dead_rpc_is_never_called(self):
        servers = [make_node(head=1990), make_node(head=2000)]
        for s in servers:
            self.addCleanup(s[0].shutdown)
        rows, log = self.run_step([dict(head=1), dict(head=1), dict(head=1)], [2037, 2036, 2037], live=(0, 2),
                                  rpcs=[servers[0][1], "http://127.0.0.1:9", servers[1][1]])
        self.assertEqual([r["node"] for r in rows], ["val0", "val2"], log)
        self.assertEqual([r["cmp_height"] for r in rows], [1990, 1990], log)
        self.assertEqual({r["block_hash"] for r in rows}, {"0xh%x" % 1990}, log)

    def test_older_binary_keeps_hmin_minus_5(self):
        """an older binary writes the marker every block: its served head is the committed head, >= HMIN-5,
        so the compare height (and the requests) are exactly what they were."""
        rows, log = self.run_step(
            [dict(head=1000), dict(head=1001), dict(head=1002)], [1000, 1001, 1002]
        )
        self.assertEqual([r["cmp_height"] for r in rows], [995] * 3, log)
        self.assertEqual({r["block_hash"] for r in rows}, {"0xh%x" % 995}, log)
        self.assertNotIn("instead", log)

    def test_unreadable_head_is_not_papered_over(self):
        """a node whose eth_blockNumber fails stays out of the min with a WARNING; it does not serve blocks
        either, so its hash reads ERR at the chosen height and summarize.py will call the cell DISAGREE."""
        rows, log = self.run_step(
            [
                dict(head=1990),
                dict(head=1990),
                dict(head=1990, head_error=True, broken_blocks=True),
            ],
            [2000, 2000, 2000],
        )
        self.assertEqual([r["cmp_height"] for r in rows], [1990] * 3, log)
        self.assertEqual([r["block_hash"] for r in rows][2], "ERR", log)
        self.assertRegex(log, r"WARNING: agreement: eth_blockNumber unreadable")

    def test_err_at_a_served_height_is_logged_as_an_rpc_failure(self):
        """a node that reports a head at or above the compare height but returns no block there is a real
        RPC failure: the hash stays ERR and run.log says so (not the applied-marker lag)."""
        rows, log = self.run_step(
            [dict(head=2000), dict(head=2000), dict(head=2000, broken_blocks=True)],
            [2000, 2000, 2000],
        )
        self.assertEqual([r["block_hash"] for r in rows][2], "ERR", log)
        self.assertRegex(log, r"ERROR: agreement: val2 did not return block 1995")

    def test_genuine_hash_mismatch_at_a_served_height_stays_visible(self):
        """different hashes at a height every node serves are recorded as-is (summarize.py: DISAGREE)."""
        rows, log = self.run_step(
            [dict(head=1990), dict(head=1990, hash_prefix="0xfork"), dict(head=1990)],
            [2000, 2000, 2000],
        )
        self.assertEqual([r["cmp_height"] for r in rows], [1990] * 3, log)
        self.assertEqual(len({r["block_hash"] for r in rows}), 2, log)
        self.assertNotIn("ERR", {r["block_hash"] for r in rows}, log)


class AgreementVerdictTest(unittest.TestCase):
    """summarize.py side of the same contract (GREEN pins): an ERR block hash never agrees, even with equal
    quiescent digests, and a hash mismatch at a served height is DISAGREE."""

    def setUp(self):
        sys.path.insert(0, HERE)
        import test_harness

        self.th = test_harness
        self.d = tempfile.mkdtemp(prefix="agree-v-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.th.write_cell(self.d, 1_000, 1_000)

    def verdict(self, hashes):
        self.th.write_agreement(self.d, ["same"] * 3, hashes=hashes)
        s, _ = self.th.run_summarize(self.d, extra=["--digest-quiescent", "1"])
        return s["agreement"]["agreement_verdict"]

    def test_err_hash_with_equal_digests_is_never_agree(self):
        self.assertEqual(self.verdict(["ERR"] * 3), "DISAGREE")
        self.assertEqual(self.verdict(["0xa", "0xa", "ERR"]), "DISAGREE")

    def test_hash_mismatch_with_equal_digests_is_disagree(self):
        self.assertEqual(self.verdict(["0xa", "0xb", "0xa"]), "DISAGREE")

    def test_equal_served_hashes_with_equal_digests_agree(self):
        self.assertEqual(self.verdict(["0xa"] * 3), "AGREE")


if __name__ == "__main__":
    unittest.main()
