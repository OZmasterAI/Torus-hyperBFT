#!/usr/bin/env python3
"""attest_traffic.py: STATE_HASH_ATTEST=1 cells tell the validators'
AttestStateHash actions apart from every other native action by kind, read
from the block bodies (torus_getBlockBody) of a height window."""

import json
from pathlib import Path
import subprocess
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from attest_traffic import ATTEST_KIND, action_kind, classify_body, scan_window

HELPER = str(Path(__file__).with_name("attest_traffic.py"))


def signed(kind):
    """A SignedNativeAction as torus_getBlockBody returns it: serde's external
    tag (a struct / tuple variant is {kind: ...}, a unit variant the string)."""
    action = (
        kind if kind in ("ClaimRewards", "UnjailSelf", "ClaimUnbonded") else {kind: {}}
    )
    return {"action": action, "nonce": 1, "signature": {"Eip712": "0x00"}}


def body(*kinds, status="executed"):
    """status: one label for every action, a list, or None (not executed yet)."""
    labels = (
        None
        if status is None
        else (status if isinstance(status, list) else [status] * len(kinds))
    )
    return {
        "blockNumber": "0x1",
        "nativeActions": [signed(k) for k in kinds],
        "nativeActionCount": len(kinds),
        "nativeActionStatus": labels,
        "nativeActionFailures": [] if labels is not None else None,
        "evmTransactions": [],
        "evmTransactionStatus": None if labels is None else [],
    }


class FakeRpc:
    """torus_getBlockBody over HTTP: single and batch requests (batch=False
    answers a batch with HTTP 500, as an over-cap response would)."""

    def __init__(self, bodies, batch=True):
        self.bodies, self.batch, self.calls = bodies, batch, []
        rpc = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                request = json.loads(
                    self.rfile.read(int(self.headers["content-length"]))
                )
                rpc.calls.append(request)
                if isinstance(request, list) and not rpc.batch:
                    self.send_response(500)
                    self.end_headers()
                    return

                def answer(r):
                    h = r["params"][0]
                    return {
                        "jsonrpc": "2.0",
                        "id": r["id"],
                        "result": rpc.bodies.get(h),
                    }

                reply = (
                    [answer(r) for r in request]
                    if isinstance(request, list)
                    else answer(request)
                )
                data = json.dumps(reply).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = "http://127.0.0.1:%d" % self.server.server_port
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


DEAD = "http://127.0.0.1:9"  # discard port: connection refused


class ClassifyTest(unittest.TestCase):
    def test_kind_is_the_serde_external_tag(self):
        self.assertEqual(action_kind(signed(ATTEST_KIND)), "AttestStateHash")
        self.assertEqual(action_kind(signed("CancelOrder")), "CancelOrder")
        self.assertEqual(action_kind(signed("ClaimRewards")), "ClaimRewards")
        for bad in (
            {},
            {"action": {}},
            {"action": {"A": 1, "B": 2}},
            {"action": 3},
            "x",
            None,
        ):
            self.assertIsNone(action_kind(bad), bad)

    def test_only_attestations_are_attestations_and_skipped_actions_do_not_count(self):
        self.assertEqual(
            classify_body(body(ATTEST_KIND, ATTEST_KIND, ATTEST_KIND)), (3, 0)
        )
        for kind in (
            "CancelOrder",
            "CancelAllOrders",
            "PlaceOrderBatch",
            "SubmitOraclePrices",
            "ModifyOrder",
            "ClaimRewards",
        ):
            self.assertEqual(classify_body(body(ATTEST_KIND, kind)), (1, 1), kind)
        # failed / rejected actions passed the replay guard (they move
        # torus_native_actions_processed_total); skipped ones did not
        self.assertEqual(classify_body(body("CancelOrder", status="failed")), (0, 1))
        self.assertEqual(classify_body(body("PlaceOrder", status="rejected")), (0, 1))
        self.assertEqual(classify_body(body("CancelOrder", status="skipped")), (0, 0))
        self.assertEqual(
            classify_body(
                body(ATTEST_KIND, "CancelOrder", status=["executed", "skipped"])
            ),
            (1, 0),
        )
        # not executed on this node yet: every action counts
        self.assertEqual(
            classify_body(body(ATTEST_KIND, "CancelOrder", status=None)), (1, 1)
        )
        self.assertEqual(classify_body(body()), (0, 0))

    def test_an_unknown_shape_counts_as_other_never_as_an_attestation(self):
        b = body(ATTEST_KIND)
        b["nativeActions"].append({"action": {"A": 1, "B": 2}})
        b["nativeActionStatus"].append("executed")
        b["nativeActionCount"] = 2
        self.assertEqual(classify_body(b), (1, 1))

    def test_malformed_bodies_are_errors(self):
        for bad in (
            None,
            {},
            {"nativeActions": "x"},
            dict(body(ATTEST_KIND), nativeActionStatus=["executed", "executed"]),
            dict(body(ATTEST_KIND), nativeActionStatus="executed"),
            dict(body(ATTEST_KIND), nativeActionCount=2),
        ):
            with self.assertRaises(ValueError, msg=bad):
                classify_body(bad)


class ScanTest(unittest.TestCase):
    def test_attestation_only_window_is_clean(self):
        bodies = {
            h: body(*([ATTEST_KIND] * 3 if h % 100 == 1 else [])) for h in range(1, 400)
        }
        with FakeRpc(bodies) as rpc:
            w = scan_window([rpc.url], 50, 350)
        self.assertEqual(
            (w["blocks"], w["attest"], w["other"], w["unread"]), (301, 9, 0, 0)
        )
        self.assertTrue(w["clean"])
        self.assertTrue(all(isinstance(c, list) for c in rpc.calls), "batched")

    def test_a_cancel_in_the_window_is_not_clean(self):
        bodies = {h: body() for h in range(1, 400)}
        bodies[200] = body(ATTEST_KIND, "CancelOrder")
        bodies[301] = body("CancelAllOrders")
        with FakeRpc(bodies) as rpc:
            w = scan_window([rpc.url], 50, 350)
            self.assertFalse(w["clean"])
            self.assertEqual(
                (w["attest"], w["other"], w["other_heights"]), (1, 2, [200, 301])
            )
            self.assertTrue(scan_window([rpc.url], 50, 199)["clean"])
            self.assertTrue(scan_window([rpc.url], 302, 350)["clean"])

    def test_missing_bodies_fall_back_to_the_next_node_then_fail_closed(self):
        full = {h: body() for h in range(1, 60)}
        holey = {h: b for h, b in full.items() if h != 42}
        with FakeRpc(holey) as a, FakeRpc(full) as b:
            w = scan_window([a.url, b.url], 1, 59)
            self.assertTrue(w["clean"], w)
            w = scan_window([DEAD, a.url], 1, 59)
            self.assertFalse(w["clean"])
            self.assertEqual((w["unread"], w["unread_heights"]), (1, [42]))

    def test_a_refused_batch_is_read_one_body_at_a_time(self):
        bodies = {h: body() for h in range(1, 30)}
        bodies[7] = body("CancelOrder")
        with FakeRpc(bodies, batch=False) as rpc:
            w = scan_window([rpc.url], 1, 29)
        self.assertEqual((w["blocks"], w["other"], w["unread"]), (29, 1, 0))

    def test_unreachable_nodes_and_bad_ranges_are_not_clean(self):
        self.assertFalse(scan_window([DEAD], 1, 5)["clean"])
        self.assertFalse(scan_window([], 1, 5)["clean"])
        self.assertFalse(scan_window([DEAD], 5, 4)["clean"])
        self.assertFalse(scan_window([DEAD], -1, 4)["clean"])

    def test_a_malformed_body_is_unread(self):
        with FakeRpc({1: body(), 2: {"nativeActions": "x"}}) as rpc:
            w = scan_window([rpc.url], 1, 2)
        self.assertEqual(
            (w["unread"], w["unread_heights"], w["clean"]), (1, [2], False)
        )


class CliTest(unittest.TestCase):
    def run_cli(self, rpcs, lo, hi):
        r = subprocess.run(
            [
                "python3",
                "-I",
                HELPER,
                "window",
                "--from",
                str(lo),
                "--to",
                str(hi),
                "--rpc",
                *rpcs,
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )
        return r.returncode, json.loads(r.stdout) if r.stdout.strip() else None

    def test_exit_status_clean_other_unread(self):
        bodies = {h: body(ATTEST_KIND) for h in range(1, 20)}
        bodies[10] = body("CancelOrder")
        with FakeRpc(bodies) as rpc:
            rc, w = self.run_cli([rpc.url], 1, 9)
            self.assertEqual((rc, w["clean"]), (0, True))
            rc, w = self.run_cli([rpc.url], 1, 19)
            self.assertEqual((rc, w["other"]), (1, 1))
            rc, w = self.run_cli([rpc.url], 15, 25)
            self.assertEqual((rc, w["unread"]), (2, 6))
        rc, w = self.run_cli([DEAD], 1, 2)
        self.assertEqual(rc, 2)

    def test_usage_errors(self):
        for args in (
            ["window", "--from", "1", "--to", "2"],
            ["window", "--from", "x", "--to", "2", "--rpc", DEAD],
        ):
            r = subprocess.run(
                ["python3", "-I", HELPER, *args],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(r.returncode, 2, args)


if __name__ == "__main__":
    unittest.main()
