#!/usr/bin/env python3
"""test_harness.py — unit tests for the matched-bench harness itself.

    python3 tools/matched-bench/test_harness.py

Runs offline (no devnet, no cargo, no network beyond 127.0.0.1 loopback stubs)
and in a few seconds. Two things are pinned here, both of which cost real bench
time when they broke:

1. `digest-node.sh` (the 3-validator state digest) must produce a stream in the
   SAME deterministic order as the pre-r6 serial loop even though it fans the
   per-market RPCs out `-P` ways, or two honest validators hash differently and
   a good candidate reads as a fork.
2. `summarize.py` must (a) report a `first120` window so 120 s and 300 s cells
   are comparable, and (b) never call `validators_agree=true` without an equal
   state digest, while NOT calling a cell a fork when the only thing that
   differs is a digest taken while the chain was still moving.
3. `crash-kill.sh` (the TORUS_EXEC_PIPELINE crash gate) must NEVER SIGKILL
   anything but the one DEVNET validator it was asked for — in particular not
   the live testnet validator that runs from ~/.cargo-target against
   testnet/data — and must put the RESTARTED pid back into the devnet pids
   file, or `stop-3val.sh` leaves an orphan node holding 8646/9162 and every
   later cell in the campaign dies in pre-flight.
"""

import gzip
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
ATTEST_RPCS = " ".join("http://127.0.0.1:%d" % p for p in (8645, 8646, 8647))
DIGEST_SH = os.path.join(HERE, "digest-node.sh")
CRASH_KILL_SH = os.path.join(HERE, "crash-kill.sh")
SUMMARIZE = os.path.join(HERE, "summarize.py")
RUN_CELL_SH = os.path.join(HERE, "run-cell.sh")
REPO = os.path.dirname(os.path.dirname(HERE))
WEIGHTED_FULL = os.path.join(REPO, "testnet", "genesis-weighted-full.json")


def jq_compact(obj):
    """Byte-identical to `jq -cS` for the flat string maps this stub returns."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


class StubRpc(BaseHTTPRequestHandler):
    """Deterministic JSON-RPC stub with per-request latency skew, so a digest
    that concatenates in completion order (rather than market order) fails."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *_a):
        pass

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        method, params = req.get("method", ""), req.get("params", [])
        arg = params[0] if params else ""
        # Skew: high market ids answer FIRST, so completion order != id order.
        # The market id is the LAST param (torus_getPosition is [addr, market]).
        key = params[-1] if params else ""
        try:
            skew = max(0, 40 - int(str(key), 16)) if str(key).startswith("0x") else 5
        except ValueError:
            skew = 5
        import time

        time.sleep(skew / 1000.0)
        result = {"m": method, "a": str(arg)}
        if len(params) > 1:
            result["b"] = str(params[1])
            # Optional per-(account, market) position state (PositionDigestTest).
            result["s"] = self.server.positions.get((str(arg), str(params[1])))
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": req.get("id", 1),
                "result": result,
            }
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class DigestOrderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), StubRpc)
        cls.srv.positions = {}
        cls.url = "http://127.0.0.1:%d" % cls.srv.server_address[1]
        cls.t = threading.Thread(target=cls.srv.serve_forever, daemon=True)
        cls.t.start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()

    def expected_stream(self, markets, accounts, pos_accounts=(), positions=None):
        positions = positions or {}
        lines = []
        for m in range(1, markets + 1):
            hx = "0x%x" % m
            lines.append(jq_compact({"m": "torus_getOrderBook", "a": hx}))
            lines.append(jq_compact({"m": "torus_getOpenInterest", "a": hx}))
        for a in accounts:
            lines.append(jq_compact({"m": "torus_getBalances", "a": a}))
        # Positions last: account by account (file order), markets in id order.
        for a in pos_accounts:
            for m in range(1, markets + 1):
                hx = "0x%x" % m
                lines.append(
                    jq_compact(
                        {
                            "m": "torus_getPosition",
                            "a": a,
                            "b": hx,
                            "s": positions.get((a, hx)),
                        }
                    )
                )
        return "".join(line + "\n" for line in lines)

    def run_digest(self, markets, accounts, par, pos_accounts=None):
        """pos_accounts: None = the 6-argument call (no positions file);
        a list = written to a file passed as the 7th argument; "" = an
        explicit empty 7th argument."""
        d = tempfile.mkdtemp(prefix="digest-test-")
        try:
            acct = os.path.join(d, "accounts.txt")
            with open(acct, "w") as f:
                f.write("".join(a + "\n" for a in accounts))
            out = os.path.join(d, "state-digest.txt")
            argv = ["bash", DIGEST_SH, self.url, str(markets), acct, out, str(par), "10"]
            if pos_accounts == "":
                argv.append("")
            elif pos_accounts is not None:
                pos = os.path.join(d, "position-accounts.txt")
                with open(pos, "w") as f:
                    f.write("".join(a + "\n" for a in pos_accounts))
                argv.append(pos)
            r = subprocess.run(
                argv,
                capture_output=True,
                text=True,
                timeout=180,
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(out) as f:
                return f.read(), r.stdout.strip()
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_parallel_digest_matches_serial_order_and_hash(self):
        accounts = ["0xaa%02d" % i for i in range(7)]
        want = self.expected_stream(40, accounts)
        want_sha = hashlib.sha256(want.encode()).hexdigest()

        serial, out1 = self.run_digest(40, accounts, 1)
        self.assertEqual(serial, want, "serial (-P 1) stream must be id-ordered")

        par, out8 = self.run_digest(40, accounts, 8)
        self.assertEqual(par, serial, "-P 8 must not reorder the digest stream")
        self.assertIn(want_sha, out1)
        self.assertIn(want_sha, out8, "parallel digest must hash identically")

    def test_digest_is_stable_across_repeats(self):
        accounts = ["0xbb01", "0xbb02"]
        a, sa = self.run_digest(12, accounts, 6)
        b, sb = self.run_digest(12, accounts, 6)
        self.assertEqual(a, b)
        self.assertEqual(sa.split()[0], sb.split()[0])

    def test_reports_wall_seconds(self):
        _, out = self.run_digest(6, ["0xcc01"], 4)
        parts = out.split()
        self.assertEqual(len(parts), 2, "stdout must be '<sha256> <wall_seconds>'")
        self.assertEqual(len(parts[0]), 64)
        float(parts[1])

    # --- liquidation stress: positions of the protocol accounts (vault + ADL
    # escrows) via torus_getPosition per market, after the balances.
    def test_position_accounts_follow_balances_in_order(self):
        accounts = ["0xaa01", "0xaa02"]
        pos = ["0xee01", "0xee02", "0xee03"]
        want = self.expected_stream(30, accounts, pos)
        serial, out1 = self.run_digest(30, accounts, 1, pos)
        self.assertEqual(serial, want, "positions: account order, then market order")
        par, out8 = self.run_digest(30, accounts, 8, pos)
        self.assertEqual(par, serial, "-P 8 must not reorder the position parts")
        sha = hashlib.sha256(want.encode()).hexdigest()
        self.assertIn(sha, out1)
        self.assertIn(sha, out8)

    def test_no_or_empty_position_file_leaves_stream_unchanged(self):
        accounts = ["0xaa01", "0xaa02"]
        want = self.expected_stream(9, accounts)
        self.assertEqual(self.run_digest(9, accounts, 4)[0], want)
        self.assertEqual(self.run_digest(9, accounts, 4, "")[0], want)

    def test_escrow_position_changes_the_digest(self):
        esc = "0x746f7275732d61646c2d657363726f772d6c6e67"
        accounts = ["0xaa01", esc]
        try:
            self.srv.positions = {}
            flat, sflat = self.run_digest(5, accounts, 4, [esc])
            base, sbase = self.run_digest(5, accounts, 4)
            self.srv.positions = {(esc, "0x3"): {"side": "long", "size": "2.5"}}
            held, sheld = self.run_digest(5, accounts, 4, [esc])
            base2, sbase2 = self.run_digest(5, accounts, 4)
        finally:
            self.srv.positions = {}
        self.assertNotEqual(sflat.split()[0], sheld.split()[0])
        self.assertIn('"size":"2.5"', held)
        # Without the positions file the escrow position is invisible.
        self.assertEqual(sbase.split()[0], sbase2.split()[0])

    def test_unreadable_position_file_is_refused(self):
        d = tempfile.mkdtemp(prefix="digest-test-")
        try:
            acct = os.path.join(d, "a.txt")
            with open(acct, "w") as f:
                f.write("0xaa01\n")
            r = subprocess.run(
                ["bash", DIGEST_SH, self.url, "2", acct, os.path.join(d, "o"),
                 "2", "10", os.path.join(d, "missing.txt")],
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(r.returncode, 2, r.stderr)
            self.assertIn("position accounts", r.stderr)
        finally:
            shutil.rmtree(d, ignore_errors=True)


# ------------------------------------------------------------------ summarize.py
BENCH_START = 1_700_000_000


def write_cell(d, matched_first120_rate, matched_tail_rate, dur=300):
    """Synthetic 1 Hz sampler for one node trio: `matched` climbs at
    matched_first120_rate for 120 s, then at matched_tail_rate."""
    cols = [
        "torus_orders_matched_total",
        "torus_orders_placed_accepted_total",
        "torus_block_height",
        "torus_blocks_committed_total",
        "torus_native_actions_processed_total",
        "torus_orders_resting_total",
        "torus_exec_queue_depth",
        "torus_mempool_native_size",
    ]
    with open(os.path.join(d, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for s in range(dur + 1):
            matched = matched_first120_rate * min(s, 120) + matched_tail_rate * max(
                0, s - 120
            )
            for n in ("val0", "val1", "val2"):
                vals = [matched, matched * 2, 10 * s, 10 * s, matched, 5, 1, 0]
                f.write(
                    "%d,%s,%s\n"
                    % (BENCH_START + s, n, ",".join(str(int(v)) for v in vals))
                )
    with open(os.path.join(d, "bench.log"), "w") as f:
        f.write("Submitted (load-gen accepted): 1,000\n")


def write_agreement(
    d, digests, hashes=None, counters_equal=True, counters=None, roots=None
):
    """`counters` (3 dicts, merged over the equal baseline) writes a per-node
    funnel — the shape a SIGKILLed-and-restarted node leaves behind, whose
    Prometheus counters are process-lifetime and start again from 0."""
    hashes = hashes or ["0xdead"] * 3
    roots = roots or ["0x0"] * 3
    with open(os.path.join(d, "agreement.jsonl"), "w") as f:
        for i in range(3):
            row = {
                "node": "val%d" % i,
                "height": 1000 + i,
                "cmp_height": 995,
                "block_hash": hashes[i],
                "header_state_root": roots[i],
                "state_digest": digests[i],
                "matched": 100,
                "placed": 200,
                "resting": 5 if counters_equal or i == 0 else 6,
                "actions": 100,
                "panic_or_failstop_lines": 0,
                "error_lines": 0,
            }
            if counters:
                row.update(counters[i])
            f.write(json.dumps(row) + "\n")


def run_summarize(d, dur=300, drained="1", extra=()):
    cmd = [
        sys.executable,
        SUMMARIZE,
        "--out",
        d,
        "--label",
        "t",
        "--markets",
        "300",
        "--dur",
        str(dur),
        "--rate",
        "76000",
        "--senders",
        "5000",
        "--t-bench0",
        str(BENCH_START),
        "--t-bench1",
        str(BENCH_START + dur),
        "--t-drain",
        str(BENCH_START + dur),
        "--drained",
        drained,
        "--bench-rc",
        "0",
        *extra,
    ]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if r.returncode != 0:
        raise AssertionError(r.stderr)
    with open(os.path.join(d, "summary.json")) as f:
        return json.load(f), r.stdout


class SummarizeTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="summ-test-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def test_first120_window_is_reported_and_distinct_from_avg(self):
        write_cell(self.d, 50_000, 10_000, dur=300)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        h = s["headline"]
        self.assertAlmostEqual(h["matched_s_first120"], 50_000, delta=500)
        self.assertAlmostEqual(h["matched_s_avg"], 26_000, delta=500)
        self.assertAlmostEqual(
            s["funnel_by_node"]["val1"]["matched_s_first120"], 50_000, delta=500
        )

    def test_nan_column_keeps_the_row(self):
        """s64 (memory e20da0cb): an arm without the consensus DB gauges writes
        'nan' in those sampler.csv columns. The loader must keep the row: an
        empty value raised ValueError there and dropped every sample."""
        write_cell(self.d, 50_000, 10_000, dur=300)
        write_agreement(self.d, ["same"] * 3)
        path = os.path.join(self.d, "sampler.csv")
        with open(path) as f:
            lines = f.read().splitlines()
        cols = ",torus_rocksdb_consensus_write_other,torus_rocksdb_l0_files__cf_native_shards__consensus"
        with open(path, "w") as f:
            f.write(lines[0] + cols + "\n")
            f.writelines(l + ",nan,nan\n" for l in lines[1:])
        s, _ = run_summarize(self.d)
        self.assertAlmostEqual(s["headline"]["matched_s_avg"], 26_000, delta=500)
        self.assertAlmostEqual(s["headline"]["matched_s_first120"], 50_000, delta=500)

    def test_open_limit_rejects_are_in_the_node_funnel(self):
        write_cell(self.d, 50_000, 10_000, dur=300)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        for node in ("val0", "val1", "val2"):
            funnel = s["funnel_by_node"][node]
            self.assertEqual(funnel["delta_orders_rejected_open_limit_total"], 0)

    BENCH_LOG_CAPPED = (
        "Submitted (load-gen accepted): 73,251 native actions (610/s)  [secondary]\n"
        "Econ mix (load-gen accepted): place 69000 (94.2%) | cancel-all 4251 (5.8%) "
        "[sent: place 70000 cancel-all 4300]\n"
        "In-flight cap 1 action(s)/sender: released committed=72000 refused=1300 "
        "timeout=950 | in flight at end 4 | block tail http://127.0.0.1:8647: "
        "fetched=3600 errors=12 missed=1\n"
    )

    def test_capped_cell_records_cap_budget_rate_mix_and_releases(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        with open(os.path.join(self.d, "bench.log"), "w") as f:
            f.write(self.BENCH_LOG_CAPPED)
        s, _ = run_summarize(
            self.d, extra=["--max-in-flight", "1", "--open-order-budget", "900"]
        )
        self.assertEqual(s["cell"]["max_in_flight"], 1)
        self.assertEqual(s["cell"]["open_order_budget"], 900)
        ing = s["ingest"]
        self.assertEqual(ing["bench_submit_rate"], 610.0)
        self.assertEqual(
            ing["econ_mix"],
            {
                "place": 69000,
                "cancel_all": 4251,
                "place_share": 0.942,
                "cancel_all_share": 0.058,
                "sent_place": 70000,
                "sent_cancel_all": 4300,
            },
        )
        self.assertEqual(
            ing["in_flight"],
            {
                "cap": 1,
                "released_committed": 72000,
                "released_refused": 1300,
                "released_timeout": 950,
                "in_flight_at_end": 4,
                "tail_url": "http://127.0.0.1:8647",
                "tail_fetched": 3600,
                "tail_errors": 12,
                "tail_missed": 1,
                "tail_mode": "head",
            },
        )

    def test_commit_tail_cell_records_the_tail_mode(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        with open(os.path.join(self.d, "bench.log"), "w") as f:
            f.write(
                self.BENCH_LOG_CAPPED.replace("missed=1\n", "missed=1 | tail=commit\n")
            )
        s, _ = run_summarize(
            self.d, extra=["--max-in-flight", "1", "--in-flight-tail", "commit"]
        )
        self.assertEqual(s["cell"]["in_flight_tail"], "commit")
        self.assertEqual(s["ingest"]["in_flight"]["tail_mode"], "commit")
        self.assertEqual(s["ingest"]["in_flight"]["tail_missed"], 1)
        # A capped cell without the knob: the bench default, head.
        with open(os.path.join(self.d, "bench.log"), "w") as f:
            f.write(self.BENCH_LOG_CAPPED)
        s, _ = run_summarize(self.d, extra=["--max-in-flight", "1"])
        self.assertIsNone(s["cell"]["in_flight_tail"], "unset = flag omitted")
        self.assertEqual(s["ingest"]["in_flight"]["tail_mode"], "head")

    def test_rpc_max_response_mb_comes_from_the_node_env(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(
            self.d,
            extra=["--node-env", json.dumps({"TORUS_RPC_MAX_RESPONSE_MB": "64"})],
        )
        self.assertEqual(s["cell"]["rpc_max_response_mb"], 64)
        s, _ = run_summarize(self.d)
        self.assertIsNone(
            s["cell"]["rpc_max_response_mb"], "unset = node default 10 MiB"
        )

    def test_uncapped_legacy_cell_reports_none_for_the_new_fields(self):
        write_cell(self.d, 1_000, 1_000)  # bench.log: legacy one-liner, no rate
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        self.assertIsNone(s["cell"]["max_in_flight"])
        self.assertIsNone(s["cell"]["in_flight_tail"])
        self.assertIsNone(s["cell"]["open_order_budget"])
        self.assertIsNone(s["ingest"]["bench_submit_rate"])
        self.assertIsNone(s["ingest"]["econ_mix"])
        self.assertIsNone(s["ingest"]["in_flight"])

    def test_cancel_by_id_line_is_ingested(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        with open(os.path.join(self.d, "bench.log"), "w") as f:
            f.write(
                self.BENCH_LOG_CAPPED
                + (
                    "Cancel-by-id (load-gen): cancel sent 900 accepted 880 | modify sent 450 "
                    "accepted 440 | lookups 700 errors 3 | no own order 25\n"
                )
            )
        s, _ = run_summarize(self.d)
        self.assertEqual(
            s["ingest"]["cancel_by_id"],
            {
                "cancel_sent": 900,
                "cancel_accepted": 880,
                "modify_sent": 450,
                "modify_accepted": 440,
                "lookups": 700,
                "lookup_errors": 3,
                "no_own_order": 25,
            },
        )
        write_cell(self.d, 1_000, 1_000)
        s, _ = run_summarize(self.d)
        self.assertIsNone(s["ingest"]["cancel_by_id"])

    def test_cloid_line_is_ingested(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        with open(os.path.join(self.d, "bench.log"), "w") as f:
            f.write(
                self.BENCH_LOG_CAPPED
                + (
                    "Cloids (load-gen generated): 4521 of 9040 orders (50.0%) | --cloid-share 50%\n"
                )
            )
        s, _ = run_summarize(self.d)
        self.assertEqual(
            s["ingest"]["cloids"], {"share_pct": 50, "stamped": 4521, "placed": 9040}
        )
        # A bench before --cloid-share prints no line: None, not 0%.
        write_cell(self.d, 1_000, 1_000)
        s, _ = run_summarize(self.d)
        self.assertIsNone(s["ingest"]["cloids"])

    def test_first120_equals_avg_on_a_120s_cell(self):
        write_cell(self.d, 40_000, 0, dur=120)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d, dur=120)
        self.assertAlmostEqual(
            s["headline"]["matched_s_first120"], s["headline"]["matched_s_avg"], delta=1
        )

    def test_equal_digest_agrees(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertIs(s["headline"]["validators_agree"], True)

    def test_unequal_digest_while_quiescent_is_a_fork(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["a", "b", "b"])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertIs(s["headline"]["validators_agree"], False)

    def test_unequal_digest_while_chain_moved_is_unverified_not_a_fork(self):
        """r6-base-300m-r1: equal hash + equal counters, digest taken 4 min apart
        per node while the chain still moved. That must NOT read agree=False."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["a", "b", "b"])
        s, _ = run_summarize(self.d, drained="0", extra=["--digest-quiescent", "0"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertIsNone(
            s["headline"]["validators_agree"],
            "digest-unverified must be null, neither true nor false",
        )

    def test_never_agrees_without_an_equal_digest(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["a", "b", "c"])
        for q in ("0", "1"):
            s, _ = run_summarize(self.d, extra=["--digest-quiescent", q])
            self.assertIsNot(s["headline"]["validators_agree"], True)

    def test_hash_mismatch_is_always_a_fork(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3, hashes=["0xa", "0xb", "0xb"])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "0"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertIs(s["headline"]["validators_agree"], False)

    def test_digest_provenance_is_recorded(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(
            self.d,
            extra=[
                "--digest-quiescent",
                "1",
                "--digest-secs",
                "41 39 40",
                "--digest-heights",
                "1000 1001 1000",
                "--drain-timeout",
                "780",
                "--markets-per-sender",
                "3",
            ],
        )
        a = s["agreement"]
        self.assertEqual(a["state_digest_seconds_per_node"], [41.0, 39.0, 40.0])
        self.assertEqual(a["state_digest_heights"], [1000, 1001, 1000])
        self.assertTrue(a["state_digest_quiescent"])
        self.assertEqual(s["timing"]["drain_timeout_s"], 780)
        self.assertEqual(s["cell"]["markets_per_sender"], 3)

    def test_legacy_cell_without_digest_flags_still_summarizes(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertIsNone(s["cell"]["markets_per_sender"])

    # --- bl4: metrics-after is one scrape, the 3 digests are concurrent ---
    # torus_native_actions_processed_total keeps ticking between them, so a
    # 1-2 block skew in WHERE the digests landed moves that counter alone.
    def test_action_counter_skew_at_skewed_digest_heights_is_unverified(self):
        """Equal block hash + header root + state digest, matched/placed/resting
        equal, and ONLY the action counter apart while the three digests were
        taken 2 blocks apart: a sampling artifact, not a fork. Not proof of
        agreement either -> DIGEST_UNVERIFIED."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[{"actions": 21688}, {"actions": 21688}, {"actions": 21702}],
        )
        s, _ = run_summarize(
            self.d, extra=["--digest-quiescent", "1", "--digest-heights", "280 280 282"]
        )
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertIsNone(s["headline"]["validators_agree"])

    def test_action_counter_skew_far_apart_is_still_a_fork(self):
        """60 blocks apart is not a scrape skew, it is two different chains."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[{"actions": 21688}, {"actions": 21688}, {"actions": 41702}],
        )
        s, _ = run_summarize(
            self.d, extra=["--digest-quiescent", "1", "--digest-heights", "280 280 340"]
        )
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")

    def test_resting_mismatch_is_a_fork_even_at_skewed_heights(self):
        """The skew escape hatch is for the ACTION counter only: a settled-state
        counter apart is divergence whatever the digest heights were."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[{"resting": 5}, {"resting": 5}, {"resting": 6}],
        )
        s, _ = run_summarize(
            self.d, extra=["--digest-quiescent", "1", "--digest-heights", "280 280 282"]
        )
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")


# ------------------------------------------------------- crash-kill.sh (gate)
# The EXACT cmdline of the live testnet validator on this box (systemd --user
# torus-18c-validator, ports 8555/9090/30333). It must be unkillable by this
# tool no matter what index/pid it is offered under.
LIVE_VALIDATOR_CMDLINE = (
    "/home/18c/.cargo-target/release/torus-node "
    "--genesis /home/18c/projects/Torus-hyperBFT/testnet/genesis-weighted-full.json "
    "--data-dir /home/18c/projects/Torus-hyperBFT/testnet/data "
    "--keystore /home/18c/.torus-hbft/18c-validator.keystore "
    "--retention-blocks 100000 "
    "--p2p-listen /ip4/13.140.140.138/udp/30333/quic-v1 "
    "--rpc-addr 127.0.0.1:8555 --metrics-addr 127.0.0.1:9090 --log-level info"
)


def devnet_cmdline(data_root, idx, wt="/home/18c/projects/wt/matched-bench"):
    """The cmdline `start_node` produces for devnet val<idx> (= what /proc shows,
    NULs turned into spaces)."""
    return (
        "%s/target/release/torus-node "
        "--genesis=%s/devnet/wsl/genesis-3val.json "
        "--data-dir=%s/data/val%d "
        "--validator-key=0%d00000000000000000000000000000000000000000000000000000000000000 "
        "--p2p-listen=/ip4/0.0.0.0/udp/3040%d/quic-v1 --p2p-private-addrs "
        "--p2p-peers=/ip4/127.0.0.1/udp/30401/quic-v1/p2p/PID0 "
        "--rpc-addr=0.0.0.0:864%d --metrics-addr=0.0.0.0:916%d "
        "--log-level=info --native-gossip=true"
    ) % (wt, wt, data_root, idx, idx + 1, idx + 1, 5 + idx, 1 + idx)


class CrashKillGuardTest(unittest.TestCase):
    """crash-kill.sh's target guard is the only thing standing between a bench
    cell and `kill -9` on the live validator. Every rejection below is a hazard
    that has to stay rejected."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="crash-kill-test-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.root = os.path.join(self.d, "torus-wsl-devnet")
        os.makedirs(os.path.join(self.root, "run"))
        self.pids = os.path.join(self.root, "run", "pids")
        with open(self.pids, "w") as f:
            f.write("1001\n1002\n1003\n")

    def guard(self, idx, pid, cmdline, data_root=None, pids=None, env=None):
        e = dict(os.environ, CRASH_KILL_LIB="1")
        e.update(env or {})
        r = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; crash_target_ok "$@"',
                "_",
                CRASH_KILL_SH,
                str(idx),
                str(pid),
                cmdline,
                data_root if data_root is not None else self.root,
                pids if pids is not None else self.pids,
            ],
            capture_output=True,
            text=True,
            env=e,
            timeout=30,
        )
        return r.returncode == 0, r.stderr

    def test_accepts_the_devnet_node_it_was_asked_for(self):
        ok, err = self.guard(1, 1002, devnet_cmdline(self.root, 1))
        self.assertTrue(ok, err)

    def test_refuses_the_live_testnet_validator(self):
        """The whole point of the guard. Offered as val1, as val2, with its pid
        present in the pids file — it must still be refused."""
        with open(self.pids, "w") as f:
            f.write("1001\n2442841\n1003\n")
        for idx in (1, 2):
            ok, _ = self.guard(idx, 2442841, LIVE_VALIDATOR_CMDLINE)
            self.assertFalse(ok, "live validator must never be a kill target")

    def test_refuses_val0(self):
        """val0 serves the bench RPC and every headline/phase number; killing it
        does not test crash recovery, it voids the cell."""
        ok, _ = self.guard(0, 1001, devnet_cmdline(self.root, 0))
        self.assertFalse(ok)

    def test_refuses_out_of_range_index(self):
        for idx in ("3", "-1", "x", ""):
            ok, _ = self.guard(idx, 1002, devnet_cmdline(self.root, 1))
            self.assertFalse(ok, "idx %r must be refused" % idx)

    def test_refuses_a_cmdline_for_a_different_validator(self):
        """Off-by-one in the pids file must not silently kill the wrong node."""
        ok, _ = self.guard(1, 1002, devnet_cmdline(self.root, 2))
        self.assertFalse(ok)

    def test_refuses_a_foreign_data_root(self):
        ok, _ = self.guard(1, 1002, devnet_cmdline("/home/18c/somewhere-else", 1))
        self.assertFalse(ok)

    def test_refuses_a_pid_not_in_the_devnet_pids_file(self):
        ok, _ = self.guard(1, 9999, devnet_cmdline(self.root, 1))
        self.assertFalse(ok)

    def test_refuses_a_non_node_process(self):
        ok, _ = self.guard(1, 1002, "/usr/bin/python3 -m http.server")
        self.assertFalse(ok)

    def test_explicit_protected_pid_list_wins(self):
        ok, _ = self.guard(
            1,
            1002,
            devnet_cmdline(self.root, 1),
            env={"TORUS_PROTECTED_PIDS": "42 1002 77"},
        )
        self.assertFalse(ok)

    def test_pid_line_is_replaced_not_appended(self):
        """stop-3val.sh only kills what the pids file lists: if the restarted pid
        is appended (or lost) the node survives the cell and the NEXT cell dies
        in pre-flight on the port collision."""
        r = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; crash_replace_pid_line "$@"',
                "_",
                CRASH_KILL_SH,
                self.pids,
                "1",
                "2002",
            ],
            capture_output=True,
            text=True,
            env=dict(os.environ, CRASH_KILL_LIB="1"),
            timeout=30,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(self.pids) as f:
            self.assertEqual(f.read().split(), ["1001", "2002", "1003"])

    def test_kill_at_must_sit_inside_the_load_window(self):
        """Killing during the drain hangs the agreement probe; killing at t=0
        kills a node that has not executed anything yet."""

        def at_ok(at, dur):
            r = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; shift; crash_kill_at_ok "$@"',
                    "_",
                    CRASH_KILL_SH,
                    str(at),
                    str(dur),
                ],
                capture_output=True,
                text=True,
                env=dict(os.environ, CRASH_KILL_LIB="1"),
                timeout=30,
            )
            return r.returncode == 0

        self.assertTrue(at_ok(50, 120))
        self.assertTrue(at_ok(40, 120))
        self.assertTrue(at_ok(60, 120))
        self.assertFalse(at_ok(0, 120))
        self.assertFalse(at_ok(5, 120))
        self.assertFalse(at_ok(115, 120), "must leave load after the restart")
        self.assertFalse(at_ok(200, 120), "must never land in the drain")
        self.assertFalse(at_ok("x", 120))

    # ---- s75 multi-crash: CRASH_KILL_AT_S="60,180,300,420,540" ------------
    def lib(self, fn, *args):
        r = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; fn=$1; shift; "$fn" "$@"',
                "_",
                CRASH_KILL_SH,
                fn,
                *[str(a) for a in args],
            ],
            capture_output=True,
            text=True,
            env=dict(os.environ, CRASH_KILL_LIB="1"),
            timeout=30,
        )
        return r.returncode == 0, r.stdout.strip()

    def test_kill_list_accepts_a_spaced_increasing_list(self):
        self.assertTrue(self.lib("crash_kill_list_ok", "60,180,300,420,540", 660)[0])
        self.assertTrue(
            self.lib("crash_kill_list_ok", "60,150", 300)[0],
            "exactly 90 s apart is allowed",
        )

    def test_kill_list_single_int_keeps_todays_rule(self):
        self.assertTrue(self.lib("crash_kill_list_ok", "50", 120)[0])
        self.assertTrue(self.lib("crash_kill_list_ok", "60", 300)[0])
        self.assertFalse(self.lib("crash_kill_list_ok", "5", 120)[0])
        self.assertFalse(self.lib("crash_kill_list_ok", "115", 120)[0])

    def test_kill_list_must_strictly_increase(self):
        self.assertFalse(self.lib("crash_kill_list_ok", "60,300,180", 660)[0])
        self.assertFalse(self.lib("crash_kill_list_ok", "180,180", 660)[0])

    def test_kill_list_needs_90s_between_kills(self):
        """A rejoin takes up to ~40 s; closer kills measure one freeze twice."""
        self.assertFalse(self.lib("crash_kill_list_ok", "60,149", 660)[0])
        self.assertFalse(self.lib("crash_kill_list_ok", "60,180,240", 660)[0])

    def test_every_kill_must_sit_inside_the_load_window(self):
        self.assertFalse(
            self.lib("crash_kill_list_ok", "60,180,640", 660)[0],
            "last kill must leave >= 30 s of load",
        )
        self.assertFalse(self.lib("crash_kill_list_ok", "5,180", 660)[0])
        self.assertTrue(self.lib("crash_kill_list_ok", "60,180,630", 660)[0])

    def test_kill_list_rejects_malformed_input(self):
        for bad in ("", "60,,180", "60,", ",60", "60 180", "60;180", "x", "60,1e2"):
            self.assertFalse(
                self.lib("crash_kill_list_ok", bad, 660)[0], "%r must be refused" % bad
            )

    def test_record_names_keep_kill_one_on_the_legacy_files(self):
        """Kill 1 writes exactly what a single-kill cell always wrote, so
        summarize.py's crash gate and every older analysis script still work."""
        for stem, k, ext, want in (
            ("crash-kill", 1, "json", "crash-kill.json"),
            ("crash-kill", 2, "json", "crash-kill-2.json"),
            ("crash", 1, "json", "crash.json"),
            ("crash", 5, "json", "crash-5.json"),
            ("crash-restart-tail", 3, "log", "crash-restart-tail-3.log"),
        ):
            ok, out = self.lib("crash_seq_name", stem, k, ext)
            self.assertTrue(ok)
            self.assertEqual(out, want)
        for name, seq in (
            ("crash-kill.json", "1"),
            ("crash-kill-2.json", "2"),
            ("crash-kill-12.json", "12"),
        ):
            ok, out = self.lib("crash_record_seq", name)
            self.assertTrue(ok)
            self.assertEqual(out, seq)
        self.assertFalse(self.lib("crash_record_seq", "crash-kill-x.json")[0])
        self.assertFalse(self.lib("crash_record_seq", "../evil.json")[0])

    def test_guard_runs_before_the_sigkill_in_every_invocation(self):
        """Multi-crash kills by invoking crash-kill.sh once per kill; each
        invocation must re-run crash_target_ok on the CURRENT pid before
        `kill -9`, never reuse an earlier verdict."""
        with open(CRASH_KILL_SH) as f:
            src = f.read()
        cli = src[
            src.index(
                "# ------------------------------------------------------------------ CLI"
            ) :
        ]
        self.assertIn("crash_target_ok", cli)
        self.assertLess(cli.index("crash_target_ok"), cli.index("kill -9"))
        with open(RUN_CELL_SH) as f:
            run_cell = f.read()
        self.assertIn("crash_kill_list_ok", run_cell)
        self.assertNotIn("kill -9", run_cell, "run-cell.sh must never SIGKILL itself")

    def test_workload_knobs_reach_the_bench_with_legacy_defaults(self):
        """BAND / CROSS_FRACTION / CANCEL_FRACTION are runner env knobs (the
        campaign run_cell.py forwards them); a hardcoded bench flag silently
        ran an s83 CANCEL_FRACTION=0.2 cell at 0.05."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        for var, default in (
            ("BAND", "5"),
            ("CROSS_FRACTION", "0.5"),
            ("CANCEL_FRACTION", "0.05"),
        ):
            self.assertIn(f"{var}=${{{var}:-{default}}}", src)
        bench = src[src.index("BENCH_CMD=(") :]
        bench = bench[: bench.index(")\n")]
        self.assertIn('--cross-fraction "$CROSS_FRACTION"', bench)
        self.assertIn('--cancel-fraction "$CANCEL_FRACTION"', bench)
        self.assertIn('--band "$BAND"', bench)

    def test_panic_count_ignores_info_level_failstop_config_line(self):
        """s83: every running-hash node logs `INFO ... fail-stop (...) on=false`
        at startup; counting it made every hash cell DISAGREE. s131: WARN lines
        only MENTION fail-stop too ("fail-stop is off, so it keeps running");
        the node emits every real fail-stop at ERROR. Count raw `panicked`
        lines plus fail-stop / FATAL / conflicting-blocks lines at ERROR only
        (same rule as crash-kill.sh crash_failstop_count)."""
        with open(RUN_CELL_SH) as f:
            line = next(l for l in f if l.lstrip().startswith("panics=$("))
        esc = "\x1b"
        sample = (
            "\n".join(
                [
                    f"{esc}[2m2026-10-01T22:25:23Z{esc}[0m {esc}[32m INFO{esc}[0m state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false",
                    "2026-10-01T22:25:23Z  INFO state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false",
                    # s136: fail-stop on by default, the echo reads on=true
                    "2026-10-01T22:25:23Z  INFO state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=true",
                    "2026-10-01T22:26:00Z  WARN state_hash: running state hash: this node diverged from the on-chain quorum hash at this checkpoint (recorded); fail-stop is off, so it keeps running",
                    f"{esc}[2m2026-10-01T22:27:00Z{esc}[0m {esc}[33m WARN{esc}[0m state_hash: node is hash-unverified: no fail-stop decision on the quorum mismatch",
                    f"{esc}[2m2026-10-01T22:30:00Z{esc}[0m {esc}[31mERROR{esc}[0m state_hash: FAIL-STOP (TORUS_STATE_HASH_FAILSTOP=1): latching exec_failed, node stops voting",
                    "2026-10-01T22:30:30Z ERROR exec_pipeline: FATAL: flush worker PANICKED \u2014 latching fail-stop height=7",
                    "thread 'torus-execution' panicked at crates/x.rs:1:1",
                    '{"timestamp":"x","level":"ERROR","fields":{"message":"CRITICAL SAFETY VIOLATION: two conflicting blocks were finalized at the same height. Latching fail-stop"}}',
                ]
            )
            + "\n"
        )
        lg = os.path.join(tempfile.mkdtemp(prefix="panics-"), "val0.log")
        with open(lg, "w") as f:
            f.write(sample)
        r = subprocess.run(
            ["bash", "-c", line.strip() + '; echo "$panics"'],
            capture_output=True,
            text=True,
            env=dict(os.environ, lg=lg),
        )
        self.assertEqual(r.stdout.strip(), "4", r.stderr)
        # an absent log is 0, never empty
        r = subprocess.run(
            ["bash", "-c", line.strip() + '; echo "$panics"'],
            capture_output=True,
            text=True,
            env=dict(os.environ, lg=lg + ".missing"),
        )
        self.assertEqual(r.stdout.strip(), "0", r.stderr)

    def test_open_order_budget_reaches_the_bench_only_when_set(self):
        """OPEN_ORDER_BUDGET=N -> bench --open-order-budget N (keeps senders
        under the chain's per-user open-order limit). Unset = flag omitted,
        so older bench binaries and prior cells are unchanged."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        self.assertIn("OPEN_ORDER_BUDGET=${OPEN_ORDER_BUDGET:-}", src)
        self.assertIn(
            '[ -n "$OPEN_ORDER_BUDGET" ] && '
            'BENCH_CMD+=(--open-order-budget "$OPEN_ORDER_BUDGET")',
            src,
        )
        self.assertIn('[[ "$OPEN_ORDER_BUDGET" =~ ^[0-9]+$ ]]', src)
        self.assertIn("open_order_budget='${OPEN_ORDER_BUDGET:-unset}'", src)

    def test_retry_busy_reaches_the_bench_only_when_set(self):
        """RETRY_BUSY=1 -> bench --retry-busy (resend a shed action instead of
        drawing a new one, so the admitted mix keeps the cancel fraction).
        Unset = flag omitted, so older bench binaries and prior cells are
        unchanged."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        self.assertIn("RETRY_BUSY=${RETRY_BUSY:-}", src)
        self.assertIn('[ "$RETRY_BUSY" = 1 ] && BENCH_CMD+=(--retry-busy)', src)
        self.assertIn('[ -z "$RETRY_BUSY" ] || [ "$RETRY_BUSY" = 1 ]', src)
        self.assertIn("retry_busy='${RETRY_BUSY:-unset}'", src)

    def test_max_in_flight_reaches_the_bench_only_when_set(self):
        """MAX_IN_FLIGHT=N -> bench --max-in-flight N, with the block-body tail
        on val2 (never val0, which takes all ingress under BENCH_RPCS=0).
        Validated, logged, allowlisted in campaign/run_cell.py and recorded in
        summary.json via summarize.py (also on resummarize). Unset = flags
        omitted, so older bench binaries and prior cells are unchanged."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        self.assertIn("MAX_IN_FLIGHT=${MAX_IN_FLIGHT:-}", src)
        self.assertIn("max_in_flight='${MAX_IN_FLIGHT:-unset}'", src)
        self.assertIn('--max-in-flight "${MAX_IN_FLIGHT:-}"', src)
        self.assertIn('--open-order-budget "${OPEN_ORDER_BUDGET:-}"', src)
        self.assertLess(src.index("#   MAX_IN_FLIGHT=N"), src.index("set -uo pipefail"))
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            self.assertIn('"MAX_IN_FLIGHT",', f.read())
        with open(os.path.join(HERE, "resummarize.sh")) as f:
            res = f.read()
        self.assertIn('--max-in-flight "$(j \'.cell.max_in_flight // ""\')"', res)
        self.assertIn(
            '--open-order-budget "$(j \'.cell.open_order_budget // ""\')"', res
        )
        lines = [
            l
            for l in src.splitlines()
            if "MAX_IN_FLIGHT" in l
            and not l.lstrip().startswith(("#", "log ", "--", "python3", "if "))
        ]
        snippet = (
            "RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)\n"
            "BENCH_CMD=()\n" + "\n".join(lines) + '\necho "${BENCH_CMD[*]}"'
        )

        def run(**env):
            base = {k: v for k, v in os.environ.items() if k != "MAX_IN_FLIGHT"}
            return subprocess.run(
                ["bash", "-c", snippet],
                capture_output=True,
                text=True,
                env=dict(base, **env),
            )

        r = run()
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        r = run(MAX_IN_FLIGHT="1")
        self.assertEqual(
            r.stdout.strip(),
            "--max-in-flight 1 --in-flight-watch-rpc http://127.0.0.1:8647",
            r.stderr,
        )
        for bad in ("x", "0", "-1", "1.5"):
            r = run(MAX_IN_FLIGHT=bad)
            self.assertEqual(r.returncode, 2, (bad, r.stdout, r.stderr))
            self.assertIn("FATAL", r.stderr)

    def test_in_flight_tail_reaches_the_bench_only_when_set(self):
        """IN_FLIGHT_TAIL=head|commit -> bench --in-flight-tail (how the
        in-flight block tail finds new bodies). Unset (default) = flag
        omitted = the bench default, head (eth_blockNumber), so prior cells
        reproduce. commit needs MAX_IN_FLIGHT. Validated, logged, allowlisted
        in campaign/run_cell.py, recorded as cell.in_flight_tail (also on
        resummarize)."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        self.assertIn("IN_FLIGHT_TAIL=${IN_FLIGHT_TAIL:-}", src)
        self.assertIn("in_flight_tail='${IN_FLIGHT_TAIL:-unset}'", src)
        self.assertIn('--in-flight-tail "${IN_FLIGHT_TAIL:-}"', src)
        self.assertLess(src.index("#   IN_FLIGHT_TAIL="), src.index("set -uo pipefail"))
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            self.assertIn('"IN_FLIGHT_TAIL",', f.read())
        with open(os.path.join(HERE, "resummarize.sh")) as f:
            self.assertIn(
                '--in-flight-tail "$(j \'.cell.in_flight_tail // ""\')"', f.read()
            )
        lines = [
            l
            for l in src.splitlines()
            if "IN_FLIGHT_TAIL" in l
            and not l.lstrip().startswith(("#", "log ", "--", "python3", "if "))
        ]
        snippet = (
            "RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)\n"
            "MAX_IN_FLIGHT=${MAX_IN_FLIGHT:-}\nBENCH_CMD=()\n"
            + "\n".join(lines)
            + '\necho "${BENCH_CMD[*]}"'
        )

        def run(**env):
            base = {
                k: v
                for k, v in os.environ.items()
                if k not in ("MAX_IN_FLIGHT", "IN_FLIGHT_TAIL")
            }
            return subprocess.run(
                ["bash", "-c", snippet],
                capture_output=True,
                text=True,
                env=dict(base, **env),
            )

        r = run()
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        r = run(MAX_IN_FLIGHT="4")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        for mode in ("head", "commit"):
            r = run(MAX_IN_FLIGHT="4", IN_FLIGHT_TAIL=mode)
            self.assertEqual(
                (r.returncode, r.stdout.strip()),
                (0, f"--in-flight-tail {mode}"),
                r.stderr,
            )
        for bad in ("x", "applied", "1"):
            r = run(MAX_IN_FLIGHT="4", IN_FLIGHT_TAIL=bad)
            self.assertEqual(r.returncode, 2, (bad, r.stdout, r.stderr))
            self.assertIn("FATAL", r.stderr)
        r = run(IN_FLIGHT_TAIL="commit")
        self.assertEqual(r.returncode, 2, "commit tail without MAX_IN_FLIGHT")
        self.assertIn("FATAL", r.stderr)

    def test_max_in_flight_raises_the_node_rpc_response_cap(self):
        """Loaded torus_getBlockBody replies pass jsonrpsee's 10 MiB default
        (~68 KB per 400-order batch), and the cap's block tail would then miss
        every body and free slots only at nonce + 70 s. MAX_IN_FLIGHT therefore
        exports TORUS_RPC_MAX_RESPONSE_MB=64 to the nodes (after the TORUS_*
        clean, before EXTRA_ENV so a cell can still override it) and logs it;
        summary.json records it as cell.rpc_max_response_mb."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        on = src.index(
            'if [ -n "$MAX_IN_FLIGHT" ]; then\n    export TORUS_RPC_MAX_RESPONSE_MB'
        )
        block = src[on : src.index("\nfi", on)]
        self.assertIn("export TORUS_RPC_MAX_RESPONSE_MB=64", block)
        self.assertIn("log ", block)
        self.assertLess(src.index("for v in $(env | grep -oE '^TORUS_"), on)
        self.assertLess(on, src.index("for kv in $EXTRA_ENV; do export"))
        self.assertLess(on, src.index("NODE_ENV_JSON=$("))
        self.assertIn(
            "TORUS_RPC_MAX_RESPONSE_MB=64", src[: src.index("set -uo pipefail")]
        )

    def test_spam_cancel_reaches_the_bench_only_when_set(self):
        """SPAM_CANCEL_KEYS / SPAM_CANCEL_RATE / SPAM_CANCEL_FUNDED=1 -> bench
        --spam-cancel-keys / --spam-cancel-rate / --spam-cancel-funded,
        validated and recorded in the cell log line. Unset = flags omitted,
        so older bench binaries and prior cells are unchanged."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        for v in ("SPAM_CANCEL_KEYS", "SPAM_CANCEL_RATE", "SPAM_CANCEL_FUNDED"):
            self.assertIn(f"{v}=${{{v}:-}}", src)
            self.assertIn(f"{v.lower()}='${{{v}:-unset}}'", src)
        # Run run-cell.sh's own SPAM_CANCEL lines (defaults, validation, flag
        # mapping) in bash and look at the resulting bench flags.
        lines = [
            l
            for l in src.splitlines()
            if "SPAM_CANCEL" in l and not l.lstrip().startswith(("#", "log "))
        ]
        snippet = "BENCH_CMD=()\n" + "\n".join(lines) + '\necho "${BENCH_CMD[*]}"'

        def run(**env):
            base = {
                k: v for k, v in os.environ.items() if not k.startswith("SPAM_CANCEL")
            }
            return subprocess.run(
                ["bash", "-c", snippet],
                capture_output=True,
                text=True,
                env=dict(base, **env),
            )

        r = run()
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        r = run(SPAM_CANCEL_KEYS="8", SPAM_CANCEL_RATE="250.5")
        self.assertEqual(
            r.stdout.strip(), "--spam-cancel-keys 8 --spam-cancel-rate 250.5", r.stderr
        )
        r = run(SPAM_CANCEL_KEYS="8", SPAM_CANCEL_RATE="100", SPAM_CANCEL_FUNDED="1")
        self.assertEqual(
            r.stdout.strip(),
            "--spam-cancel-keys 8 --spam-cancel-rate 100 --spam-cancel-funded",
            r.stderr,
        )
        for bad in (
            {"SPAM_CANCEL_KEYS": "x"},
            {"SPAM_CANCEL_KEYS": "8"},  # no rate
            {"SPAM_CANCEL_KEYS": "8", "SPAM_CANCEL_RATE": "fast"},
            {
                "SPAM_CANCEL_KEYS": "8",
                "SPAM_CANCEL_RATE": "1",
                "SPAM_CANCEL_FUNDED": "yes",
            },
        ):
            r = run(**bad)
            self.assertEqual(r.returncode, 2, (bad, r.stdout, r.stderr))
            self.assertIn("FATAL", r.stderr)

    def test_cancel_by_id_reaches_the_bench_only_when_set(self):
        """Item 6 Phase 2 step 0.2: CANCEL_BY_ID_FRACTION / MODIFY_FRACTION ->
        bench --cancel-by-id-fraction / --modify-fraction, validated as
        fractions and logged. Unset = flags omitted (older bench binaries and
        prior cells unchanged)."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        for v in ("CANCEL_BY_ID_FRACTION", "MODIFY_FRACTION"):
            self.assertIn(f"{v}=${{{v}:-}}", src)
            self.assertIn(f"'${{{v}:-unset}}'", src)
        lines = [
            l
            for l in src.splitlines()
            if ("CANCEL_BY_ID_FRACTION" in l or "MODIFY_FRACTION" in l)
            and not l.lstrip().startswith(("#", "log "))
        ]
        snippet = "BENCH_CMD=()\n" + "\n".join(lines) + '\necho "${BENCH_CMD[*]}"'

        def run(**env):
            base = {
                k: v
                for k, v in os.environ.items()
                if k not in ("CANCEL_BY_ID_FRACTION", "MODIFY_FRACTION")
            }
            return subprocess.run(
                ["bash", "-c", snippet],
                capture_output=True,
                text=True,
                env=dict(base, **env),
            )

        r = run()
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        r = run(CANCEL_BY_ID_FRACTION="0.1", MODIFY_FRACTION="0.05")
        self.assertEqual(
            r.stdout.strip(),
            "--cancel-by-id-fraction 0.1 --modify-fraction 0.05",
            r.stderr,
        )
        for bad in ({"CANCEL_BY_ID_FRACTION": "2"}, {"MODIFY_FRACTION": "x"}):
            r = run(**bad)
            self.assertEqual(r.returncode, 2, (bad, r.stdout, r.stderr))
            self.assertIn("FATAL", r.stderr)

    def test_antispam_off_on_devnet_and_switched_on_by_the_harness(self):
        """Node anti-spam limits (items A, B, D) are OFF on the bench devnet
        (devnet/wsl/env.sh defaults), ANTISPAM=1 turns them ON for a cell
        (validated, logged), and EXTRA_ENV can still override any single
        knob (it is exported after). Item C (cancel block share) is a
        fairness fix and stays at the node default everywhere."""
        wsl_env = os.path.join(
            os.path.dirname(os.path.dirname(HERE)), "devnet", "wsl", "env.sh"
        )
        with open(wsl_env) as f:
            env_src = f.read()
        for knob in (
            "TORUS_INGRESS_MIN_COLLATERAL",
            "TORUS_ADDR_RATE_LIMIT",
            "TORUS_RPC_IP_WEIGHT_PER_MIN",
        ):
            self.assertIn(f'export {knob}="${{{knob}:-0}}"', env_src)
        self.assertNotIn("TORUS_CANCEL_BLOCK_SHARE_PCT=", env_src)
        r = subprocess.run(
            [
                "bash",
                "-c",
                f'source "{wsl_env}"; '
                'echo "$TORUS_INGRESS_MIN_COLLATERAL $TORUS_ADDR_RATE_LIMIT $TORUS_RPC_IP_WEIGHT_PER_MIN"',
            ],
            capture_output=True,
            text=True,
            env={k: v for k, v in os.environ.items() if not k.startswith("TORUS_")},
        )
        self.assertEqual(r.stdout.strip(), "0 0 0", r.stderr)

        with open(RUN_CELL_SH) as f:
            src = f.read()
        self.assertIn("ANTISPAM=${ANTISPAM:-}", src)
        self.assertIn('[ -z "$ANTISPAM" ] || [ "$ANTISPAM" = 1 ]', src)
        self.assertIn("antispam='${ANTISPAM:-unset}'", src)
        on = src.index('if [ "$ANTISPAM" = 1 ]; then')
        block = src[on : src.index("fi", on)]
        for kv in (
            "TORUS_INGRESS_MIN_COLLATERAL=1",
            "TORUS_ADDR_RATE_LIMIT=1",
            "TORUS_RPC_IP_WEIGHT_PER_MIN=1200",
        ):
            self.assertIn(kv, block)
        # exported after the env clean and before EXTRA_ENV
        self.assertLess(src.index("for v in $(env | grep -oE '^TORUS_"), on)
        self.assertLess(on, src.index('for kv in $EXTRA_ENV; do export "$kv"; done'))

    def test_open_limit_rejects_are_sampled(self):
        """The open-limit funnel counter is in both sampler column sets."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        for cols in ("FUNNEL_COLS=", "WIDE_COLS="):
            line = src[src.index(cols) :].split("\n", 1)[0]
            self.assertIn("torus_orders_rejected_open_limit_total", line, cols)

    def test_restart_uses_the_same_argv_as_launch(self):
        """launch-3val.sh and crash-kill.sh must start a node through ONE
        implementation: a restart with different flags is not a crash gate."""
        wsl = os.path.join(os.path.dirname(os.path.dirname(HERE)), "devnet", "wsl")
        with open(os.path.join(wsl, "launch-3val.sh")) as f:
            launch = f.read()
        with open(CRASH_KILL_SH) as f:
            crash = f.read()
        lib = os.path.join(wsl, "start-node.sh")
        self.assertTrue(os.path.exists(lib), "devnet/wsl/start-node.sh must exist")
        self.assertIn("start-node.sh", launch, "launch-3val.sh must source the lib")
        self.assertNotIn(
            'nohup "$BIN"', launch, "launch-3val.sh must not keep its own copy"
        )
        self.assertIn("start-node.sh", crash, "crash-kill.sh must use it")


# ------------------------------- start-node.sh: optional history retention (Q9)
class StartNodeRetentionTest(unittest.TestCase):
    """devnet/wsl/start-node.sh passes `--retention-blocks=N` only when
    TORUS_RETENTION_BLOCKS is set. Unset or empty = no flag, the argv of every
    earlier cell, byte for byte (since s131 that means the node default,
    prune by default with N = 100,000; TORUS_RETENTION_BLOCKS=0 = archive). The flag goes last, so crash-kill.sh's
    `--data-dir=` match (and with it the restart) still accepts the node."""

    START_NODE_SH = os.path.join(
        os.path.dirname(os.path.dirname(HERE)), "devnet", "wsl", "start-node.sh"
    )

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="start-node-test-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.argv_file = os.path.join(self.d, "argv")
        self.bin = os.path.join(self.d, "torus-node")
        with open(self.bin, "w") as f:
            f.write('#!/bin/bash\nprintf "%s\\0" "$@" > "$FAKE_ARGV"\n')
        os.chmod(self.bin, 0o755)

    def start(self, retention=None):
        """Run `start_node 1` under `set -u` with a fake binary; return
        (completed process, argv the binary got or None)."""
        if os.path.exists(self.argv_file):
            os.remove(self.argv_file)
        env = dict(
            os.environ,
            FAKE_ARGV=self.argv_file,
            BIN=self.bin,
            GENESIS=os.path.join(self.d, "genesis.json"),
            DATA_ROOT=self.d,
            RUN_DIR=os.path.join(self.d, "run"),
            KEY1="02" + "00" * 31,
            P2P1="30402",
            RPC1="8646",
            MET1="9162",
            PEER_TO_V0="/ip4/127.0.0.1/udp/30401/quic-v1/p2p/PID0",
            PEER_TO_V1="/ip4/127.0.0.1/udp/30402/quic-v1/p2p/PID1",
        )
        env.pop("TORUS_RETENTION_BLOCKS", None)
        if retention is not None:
            env["TORUS_RETENTION_BLOCKS"] = retention
        r = subprocess.run(
            [
                "bash",
                "-c",
                'set -u; source "$1"; start_node 1 || exit 7; wait "$STARTED_PID"',
                "_",
                self.START_NODE_SH,
            ],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
        argv = None
        if os.path.exists(self.argv_file):
            with open(self.argv_file) as f:
                argv = f.read().split("\0")[:-1]
        return r, argv

    def test_unset_or_empty_is_archive(self):
        r, argv = self.start()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--data-dir=%s/data/val1" % self.d, argv)
        self.assertFalse([a for a in argv if a.startswith("--retention-blocks")], argv)
        r, empty = self.start("")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(empty, argv)

    def test_set_appends_the_flag_last(self):
        _, archive = self.start()
        r, argv = self.start("5000")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(argv, archive + ["--retention-blocks=5000"])
        # crash-kill.sh still accepts the node for the restart.
        pids = os.path.join(self.d, "pids")
        with open(pids, "w") as f:
            f.write("1001\n")
        cmdline = " ".join([self.bin] + argv)
        g = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; crash_target_ok "$@"',
                "_",
                CRASH_KILL_SH,
                "1",
                "1001",
                cmdline,
                self.d,
                pids,
            ],
            capture_output=True,
            text=True,
            env=dict(os.environ, CRASH_KILL_LIB="1"),
            timeout=30,
        )
        self.assertEqual(g.returncode, 0, g.stderr)

    def test_non_numeric_refuses_to_start(self):
        r, argv = self.start("12k")
        self.assertEqual(r.returncode, 7)
        self.assertIn("not a block count", r.stderr)
        self.assertIsNone(argv, "the node must not start")


class StartNodeAttestKeyTest(StartNodeRetentionTest):
    """devnet/wsl/start-node.sh passes `--state-hash-attest-key=D/val<idx>.key`
    only when STATE_HASH_ATTEST_KEY_DIR=D is set (run-cell.sh STATE_HASH_ATTEST=1).
    Unset or empty = no flag, the argv of every earlier cell. The flag goes last
    (after the retention flag), so crash-kill.sh's restart still accepts the node."""

    def start(self, retention=None, key_dir=None):
        old = os.environ.pop("STATE_HASH_ATTEST_KEY_DIR", None)
        try:
            if key_dir is not None:
                os.environ["STATE_HASH_ATTEST_KEY_DIR"] = key_dir
            return super().start(retention)
        finally:
            os.environ.pop("STATE_HASH_ATTEST_KEY_DIR", None)
            if old is not None:
                os.environ["STATE_HASH_ATTEST_KEY_DIR"] = old

    def key_dir(self):
        d = os.path.join(self.d, "attest-keys")
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "val1.key"), "w") as f:
            f.write("ab" * 32 + "\n")
        os.chmod(os.path.join(d, "val1.key"), 0o600)
        return d

    def test_unset_or_empty_passes_no_attest_key(self):
        r, argv = self.start()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse([a for a in argv if "attest" in a], argv)
        r, empty = self.start(key_dir="")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(empty, argv)

    def test_set_appends_the_attest_key_last(self):
        _, plain = self.start()
        d = self.key_dir()
        r, argv = self.start(key_dir=d)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(argv, plain + ["--state-hash-attest-key=%s/val1.key" % d])
        r, both = self.start("5000", key_dir=d)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            both,
            plain + ["--retention-blocks=5000", "--state-hash-attest-key=%s/val1.key" % d],
        )
        pids = os.path.join(self.d, "pids")
        with open(pids, "w") as f:
            f.write("1001\n")
        g = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; crash_target_ok "$@"',
                "_",
                CRASH_KILL_SH,
                "1",
                "1001",
                " ".join([self.bin] + both),
                self.d,
                pids,
            ],
            capture_output=True,
            text=True,
            env=dict(os.environ, CRASH_KILL_LIB="1"),
            timeout=30,
        )
        self.assertEqual(g.returncode, 0, g.stderr)

    def test_missing_key_file_refuses_to_start(self):
        r, argv = self.start(key_dir=os.path.join(self.d, "nowhere"))
        self.assertEqual(r.returncode, 7)
        self.assertIn("has no readable val1.key", r.stderr)
        self.assertIsNone(argv, "the node must not start")


class StateHashAttestHarnessTest(unittest.TestCase):
    """STATE_HASH_ATTEST=1: attest keys + live fail-stop on the nodes, a post-cell
    h_n comparison (state_attest.py). Off (the default) the cell is unchanged."""

    @classmethod
    def setUpClass(cls):
        with open(RUN_CELL_SH) as f:
            cls.src = f.read()

    def test_default_off_documented_and_allowlisted(self):
        self.assertIn("STATE_HASH_ATTEST=${STATE_HASH_ATTEST:-0}", self.src)
        head = self.src[: self.src.index("set -uo pipefail")]
        self.assertIn("#   STATE_HASH_ATTEST=1", head)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            rc = f.read()
        self.assertIn('"STATE_HASH_ATTEST",', rc)
        self.assertIn('"BOOK_WINDOW_COMMIT",', rc)
        with open(os.path.join(HERE, "resummarize.sh")) as f:
            self.assertIn('"$T/state_attest.py" summary', f.read())

    def test_every_step_is_gated(self):
        for marker in (
            "    export TORUS_STATE_HASH_FAILSTOP=1",
            '    export STATE_HASH_ATTEST_KEY_DIR="$RUN_DIR/attest-keys"',
            '"$SELF_DIR/state_attest.py" keys',
            "--state-hash-attest-key=$STATE_HASH_ATTEST_KEY_DIR/val$i.key",
            '"$SELF_DIR/state_attest.py" scan',
            '"$SELF_DIR/state_attest.py" summary',
            "jq -e '.state_attest.ok == true'",
        ):
            i = self.src.index(marker)
            gate = self.src.rindex("if state_attest_on", 0, i)
            self.assertNotIn("\nfi\n", self.src[gate:i], marker)
        # an inherited key dir never reaches a cell that did not ask for it
        self.assertIn("unset STATE_HASH_ATTEST_KEY_DIR\nif state_attest_on; then", self.src)

    def test_order_failstop_before_extra_env_scan_before_stop(self):
        s = self.src
        fs = s.index("    export TORUS_STATE_HASH_FAILSTOP=1")
        self.assertLess(s.index("for v in $(env | grep -oE '^TORUS_[A-Za-z0-9_]+'); do unset"), fs)
        self.assertLess(fs, s.index('for kv in $EXTRA_ENV; do export "$kv"; done\n# HOTSTUFF_CPUS'))
        self.assertLess(fs, s.index('NODE_ENV_JSON=$(env | grep'))  # recorded in the node env
        argv = s.index("--state-hash-attest-key=$STATE_HASH_ATTEST_KEY_DIR/val$i.key")
        self.assertLess(s.index('mapfile -t PIDS < "$RUN_DIR/pids"'), argv)
        scan = s.index('"$SELF_DIR/state_attest.py" scan')
        self.assertLess(s.index('cat "$OUT/agreement.jsonl" >> "$OUT/run.log"'), scan)
        self.assertLess(
            scan,
            s.index("# ---------------------------------------------------------------- 9. stop + collect logs"),
        )
        summ = s.index('"$SELF_DIR/state_attest.py" summary')
        self.assertLess(s.index('python3 "$TOOLS_DIR/summarize.py"'), summ)
        self.assertLess(summ, s.index('log "done -> $OUT/summary.json"'))

    def test_bad_env_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="attest-pre-")
        try:
            fake_target(tmp)
            script, wt = sandbox_repo(tmp)
            env0 = {k: v for k, v in os.environ.items() if k not in ("STATE_HASH_ACTIVATION", "STATE_HASH_ATTEST")}
            env0.update(TARGET_DIR=tmp, RESULTS_ROOT=tmp, DATA_ROOT=tmp, BENCH_ALLOW_UNDETACHED="1")
            cases = [
                (dict(STATE_HASH_ATTEST="yes"), 2, "STATE_HASH_ATTEST must be unset, 0 or 1"),
                (dict(STATE_HASH_ATTEST="1"), 2, "STATE_HASH_ATTEST=1 needs STATE_HASH_ACTIVATION"),
                (dict(STATE_HASH_ATTEST="1", STATE_HASH_ACTIVATION="1"), 1, "state_attest.py"),
            ]
            for env, rc, msg in cases:
                r = subprocess.run(
                    [script, wt, "attest-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(env0, **env),
                )
                self.assertEqual(r.returncode, rc, r.stderr)
                self.assertIn(msg, r.stderr)
                self.assertFalse(os.path.exists(os.path.join(tmp, "attest-pre-x")), "must fail before any launch")
            # with state_attest.py beside run-cell.sh: the attestation-window reader and a
            # health.py that uses it (--attest-rpc) are required next
            shutil.copy2(os.path.join(HERE, "state_attest.py"), os.path.dirname(script))
            for missing, msg in (("attest_traffic.py", "needs %s/attest_traffic.py" % os.path.dirname(script)),
                                 ("health.py", "has no --attest-rpc")):
                r = subprocess.run(
                    [script, wt, "attest-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(env0, STATE_HASH_ATTEST="1", STATE_HASH_ACTIVATION="1"),
                )
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(msg, r.stderr)
                self.assertFalse(os.path.exists(os.path.join(tmp, "attest-pre-x")))
                shutil.copy2(os.path.join(HERE, missing), os.path.dirname(script))
            # a worktree whose start-node.sh lacks the key dir is refused
            with open(os.path.join(wt, "devnet", "wsl", "start-node.sh"), "w") as f:
                f.write("start_node() { :; }\n")
            r = subprocess.run(
                [script, wt, "attest-pre-x"],
                capture_output=True,
                text=True,
                timeout=30,
                env=dict(env0, STATE_HASH_ATTEST="1", STATE_HASH_ACTIVATION="1"),
            )
            self.assertEqual(r.returncode, 1, r.stderr)
            self.assertIn("does not pass --state-hash-attest-key", r.stderr)
            self.assertFalse(os.path.exists(os.path.join(tmp, "attest-pre-x")))
        finally:
            shutil.rmtree(tmp)


# --------------------------------------------- run-cell.sh: which tools score
class ResummarizeStateAttestTest(unittest.TestCase):
    """[claude-review] P2: summarize.py rewrites summary.json without .state_attest; a cell whose scan
    wrote no state-attest.json must come back REJECT from resummarize.sh, not ACCEPT."""

    def cell(self):
        d = tempfile.mkdtemp(prefix="attest-resum-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        write_cell(d, 1000, 1000, dur=120)
        write_agreement(d, ["0xaa"] * 3)
        run_summarize(d, dur=120)
        return d

    def resummarize(self, d):
        r = subprocess.run(["bash", os.path.join(HERE, "resummarize.sh"), d],
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(d, "summary.json")) as f:
            return json.load(f)

    def attest_summary(self, d):
        r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "state_attest.py"), "summary", "--dir", d],
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_scan_without_file_stays_rejected(self):
        d = self.cell()
        self.attest_summary(d)  # no state-attest.json: ok=false, REJECT
        s = self.resummarize(d)
        self.assertIs(s["state_attest"]["ok"], False)
        self.assertEqual(s["validity"]["verdict"], "REJECT")

    def test_cell_without_attest_is_untouched(self):
        s = self.resummarize(self.cell())
        self.assertNotIn("state_attest", s)


class ToolsDirResolutionTest(unittest.TestCase):
    """A candidate is scored by ITS OWN summarize.py. run-cell.sh lives in the
    integration repo but is handed a candidate worktree; resolving the scoring
    scripts next to the SCRIPT silently scored bl3 with the head's summarizer."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="tools-dir-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def make_wt(self, with_tools=True):
        wt = os.path.join(self.d, "wt")
        os.makedirs(os.path.join(wt, "devnet", "wsl"), exist_ok=True)
        if with_tools:
            t = os.path.join(wt, "tools", "matched-bench")
            os.makedirs(t, exist_ok=True)
            for f in (
                "summarize.py",
                "digest-node.sh",
                "crash-kill.sh",
                "win60.awk",
                "phase60.awk",
            ):
                open(os.path.join(t, f), "w").close()
        return wt

    def paths(self, wt, env=None):
        r = subprocess.run(
            ["bash", RUN_CELL_SH, wt, "probe-label"],
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(os.environ, RUN_CELL_PRINT_PATHS="1", **(env or {})),
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return dict(line.split("=", 1) for line in r.stdout.split() if "=" in line)

    def test_scoring_scripts_come_from_the_worktree_under_test(self):
        wt = self.make_wt()
        self.assertEqual(
            self.paths(wt)["TOOLS_DIR"], os.path.join(wt, "tools", "matched-bench")
        )

    def test_falls_back_to_its_own_dir_when_the_worktree_has_no_tools(self):
        wt = self.make_wt(with_tools=False)
        p = self.paths(wt)
        self.assertEqual(p["TOOLS_DIR"], HERE)
        self.assertEqual(p["TOOLS_FROM_WORKTREE"], "0")

    def test_the_fallback_can_be_forced(self):
        wt = self.make_wt()
        p = self.paths(wt, env={"TOOLS_FROM_WORKTREE": "0"})
        self.assertEqual(p["TOOLS_DIR"], HERE)


# ------------------------------------------- summarize.py: the crash-gate verdict
# The real bl3-...-crash-on-r1 funnel: val1 was SIGKILLed at t=50 s, so its
# process-lifetime Prometheus counters restart from 0 and land at ~76 % of the
# two survivors even though all three ended on the same state digest.
SURVIVOR_COUNTERS = {
    "matched": 5495893,
    "placed": 6890235,
    "resting": 4087861,
    "actions": 21688,
}
RESTARTED_COUNTERS = {
    "matched": 4213249,
    "placed": 5269835,
    "resting": 3121631,
    "actions": 16712,
}


def write_crash(
    d,
    gap=3,
    queue=2,
    panics=0,
    holes=0,
    restarted_pid=2002,
    replay_found=True,
    pipeline_line=True,
    name="crash.json",
    seq=None,
    kill_at=50,
    kill_ts=None,
    restart_ts=None,
):
    """The crash.json run-cell.sh drops next to summary.json after a
    CRASH_KILL_AT_S cell (crash-kill.sh's record + the post-run log scan).
    s75 multi-crash: kill k>=2 lands in crash-<k>.json with kill_seq=k."""
    applied = 1000
    obj = {
        "enabled": True,
        "kill_node": "val1",
        "kill_idx": 1,
        "kill_at_s": kill_at,
        "killed_pid": 1002,
        "restarted_pid": restarted_pid,
        "down_s": 1.4,
        "pre_kill": {
            "block_height": applied + gap,
            "exec_queue_depth": queue,
            "flush_worker_depth": 1,
            "log_bytes": 4096,
        },
        "restart": {
            "replay_line_found": replay_found,
            "applied_height_at_crash": applied if replay_found else None,
            "committed_height_at_crash": applied + gap if replay_found else None,
            "gap": gap if replay_found else 0,
            "pipeline_enabled_line": pipeline_line,
            "worker_attached_applied": applied + gap,
            "panic_or_failstop_lines": panics,
            "hole_lines": holes,
            "error_lines": 0,
        },
    }
    if seq is not None:
        obj["kill_seq"] = seq
    if kill_ts is not None:
        obj["kill_ts"] = kill_ts
        obj["restart_ts"] = restart_ts if restart_ts is not None else kill_ts + 1.4
    with open(os.path.join(d, name), "w") as f:
        json.dump(obj, f)


class CrashGateSummaryTest(unittest.TestCase):
    """The crash gate is the ONLY thing that can justify flipping
    TORUS_EXEC_PIPELINE on by default, so it must never read PASS for a cell
    that did not actually crash-and-replay a pipelined node."""

    ON = [
        "--node-env",
        json.dumps({"TORUS_EXEC_PIPELINE": "1"}),
        "--digest-quiescent",
        "1",
    ]
    # The pre-Phase-3 per-block flush: the durable applied height trails the
    # executed one by the depth-1 hand-off only.
    PER_BLOCK = [
        "--node-env",
        json.dumps({"TORUS_EXEC_PIPELINE": "1", "TORUS_CHECKPOINT_INTERVAL_MS": "0"}),
        "--digest-quiescent",
        "1",
    ]

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="crash-summ-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)

    def test_no_crash_cell_reports_none_not_false(self):
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertIsNone(s["crash"])
        self.assertIsNone(
            s["headline"]["crash_gate"],
            "a cell that never crashed must not read as a FAILED gate",
        )

    def test_clean_crash_and_replay_passes(self):
        write_crash(self.d, gap=3, queue=2)
        s, _ = run_summarize(self.d, extra=self.ON)
        c = s["crash"]
        self.assertEqual(c["rewind_blocks"], 3)
        self.assertEqual(c["exec_queue_depth_at_kill"], 2)
        self.assertEqual(c["rewind_beyond_exec_queue"], 1)
        self.assertEqual(c["verdict"], "PASS")
        self.assertEqual(s["headline"]["crash_gate"], "PASS")

    def test_rewind_beyond_the_exec_queue_is_bounded(self):
        """Per-block flush (checkpoint interval 0): depth-1 pipelining may cost
        ONE extra block of replay on top of the committed-but-unexecuted queue.
        Five is a broken fence."""
        write_crash(self.d, gap=8, queue=3)
        s, _ = run_summarize(self.d, extra=self.PER_BLOCK)
        self.assertEqual(s["crash"]["max_rewind_beyond_exec_queue"], 2)
        self.assertEqual(s["crash"]["rewind_beyond_exec_queue"], 5)
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_panic_after_restart_fails(self):
        write_crash(self.d, panics=1)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_unhealed_hole_fails(self):
        write_crash(self.d, holes=1)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_node_that_never_came_back_fails(self):
        write_crash(self.d, restarted_pid=None)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_a_fork_fails_the_gate(self):
        write_agreement(self.d, ["a", "b", "b"])
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_digest_unverified_cannot_pass_the_gate(self):
        write_agreement(self.d, ["a", "b", "b"])
        write_crash(self.d)
        s, _ = run_summarize(
            self.d,
            drained="0",
            extra=[
                "--node-env",
                json.dumps({"TORUS_EXEC_PIPELINE": "1"}),
                "--digest-quiescent",
                "0",
            ],
        )
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_pipeline_flag_must_have_been_on_after_the_restart(self):
        """The restarted node inherits the cell env; if the ENABLED line is
        missing, the gate crash-tested the SERIAL path and proves nothing about
        the flag it is supposed to unblock."""
        write_crash(self.d, pipeline_line=False)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["crash"]["verdict"], "FAIL")
        self.assertFalse(s["crash"]["pipeline_flag_confirmed"])

    def test_serial_cell_does_not_need_the_pipeline_line(self):
        """A crash cell with the flag OFF is still a valid (serial) control."""
        write_crash(self.d, pipeline_line=False)
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1"])
        self.assertEqual(s["crash"]["verdict"], "PASS")

    def test_no_replay_line_is_a_zero_rewind_pass(self):
        """applied == committed at the moment of the kill: nothing to replay."""
        write_crash(self.d, replay_found=False, queue=0)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["crash"]["rewind_blocks"], 0)
        self.assertEqual(s["crash"]["verdict"], "PASS")

    # ---- bl4: the killed node's counters RESET; the survivors' do not -------
    # Real shape from bl3-...-crash-on-r1: val1 was SIGKILLed at t=50 s and came
    # back with process-lifetime Prometheus counters at ~76 % of val0/val2,
    # while all three agreed on block hash, header root and state digest. The
    # old gate read that as a fork and could therefore NEVER pass.
    def test_killed_node_counters_are_scored_against_survivors_only(self):
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, SURVIVOR_COUNTERS],
        )
        write_crash(self.d, gap=1, queue=1)
        s, _ = run_summarize(
            self.d, extra=self.ON + ["--digest-heights", "280 280 282"]
        )
        a = s["agreement"]
        self.assertFalse(a["counters_equal_all_nodes"])
        self.assertTrue(a["counters_equal"])
        self.assertEqual(a["counters_compared_nodes"], ["val0", "val2"])
        self.assertEqual(a["counters_excluded_node"], "val1")
        self.assertEqual(a["agreement_verdict"], "AGREE")
        self.assertEqual(s["crash"]["verdict"], "PASS")
        self.assertEqual(s["headline"]["crash_gate"], "PASS")

    def test_a_forked_killed_node_still_fails_the_gate(self):
        """Counters are excused for the killed node. Its STATE is not."""
        write_agreement(
            self.d,
            ["same", "forked", "same"],
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, SURVIVOR_COUNTERS],
        )
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(
            any("state digest" in r for r in c["fail_reasons"]), c["fail_reasons"]
        )

    def test_a_killed_node_with_a_different_block_hash_fails(self):
        write_agreement(
            self.d,
            ["same"] * 3,
            hashes=["0xa", "0xb", "0xa"],
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, SURVIVOR_COUNTERS],
        )
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(
            any("block hash" in r for r in c["fail_reasons"]), c["fail_reasons"]
        )

    def test_a_killed_node_with_a_different_header_root_fails(self):
        write_agreement(
            self.d,
            ["same"] * 3,
            roots=["0x0", "0x1", "0x0"],
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, SURVIVOR_COUNTERS],
        )
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(
            any("header state root" in r for r in c["fail_reasons"]), c["fail_reasons"]
        )

    def test_survivors_that_disagree_still_fail_the_gate(self):
        """Excluding the killed node must not excuse the other two."""
        forked_survivor = dict(SURVIVOR_COUNTERS)
        forked_survivor["matched"] += 7
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, forked_survivor],
        )
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertFalse(s["agreement"]["counters_equal"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_crash_cell_needs_a_quiescent_digest(self):
        """An unpinned digest cannot prove the restarted node reconverged."""
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, SURVIVOR_COUNTERS],
        )
        write_crash(self.d)
        s, _ = run_summarize(
            self.d,
            drained="0",
            extra=[
                "--node-env",
                json.dumps({"TORUS_EXEC_PIPELINE": "1"}),
                "--digest-quiescent",
                "0",
            ],
        )
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(
            any("quiescent" in r for r in c["fail_reasons"]), c["fail_reasons"]
        )

    def test_non_crash_cell_compares_all_three_nodes(self):
        """No crash.json => nothing is excused; the old semantics exactly."""
        write_agreement(
            self.d,
            ["same"] * 3,
            counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS, SURVIVOR_COUNTERS],
        )
        s, _ = run_summarize(self.d, extra=self.ON)
        a = s["agreement"]
        self.assertIsNone(a["counters_excluded_node"])
        self.assertEqual(a["counters_compared_nodes"], ["val0", "val1", "val2"])
        self.assertFalse(a["counters_equal"])
        self.assertEqual(a["agreement_verdict"], "DISAGREE")


def add_failed_scrapes(d, node="val0", offsets=(140, 145, 150)):
    """Append the rows a FAILED metrics scrape leaves in sampler.csv: every
    metric 0 at that timestamp (Codex review of d7867617: such a row made a
    rate-based bound negative and failed a valid recovery)."""
    p = os.path.join(d, "sampler.csv")
    with open(p) as f:
        ncols = len(f.readline().strip().split(",")) - 2
    with open(p, "a") as f:
        for off in offsets:
            f.write("%d,%s,%s\n" % (BENCH_START + off, node, ",".join(["0"] * ncols)))


class CheckpointRewindBoundTest(unittest.TestCase):
    """Bench 8l (perf doc 107.6): with coalesced checkpoints (15 s, the
    validator default) the durable applied height is the LAST checkpoint W
    finished, so a SIGKILL legitimately replays the open checkpoint window
    plus at most one checkpoint W was still writing (8l: 27 blocks, 23 beyond
    the exec queue, digests agree). The old constant bound of 2 rejected that
    valid cell. Every 100th height forces a checkpoint, so with checkpoints on
    the bound is 2 + 2 x 100 = 202; with them off it stays 2."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="ckpt-rewind-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)

    def env(self, **kw):
        e = {"TORUS_EXEC_PIPELINE": "1"}
        e.update(kw)
        return ["--node-env", json.dumps(e), "--digest-quiescent", "1"]

    def crash(self, gap, queue, **kw):
        write_crash(self.d, gap=gap, queue=queue, kill_ts=BENCH_START + 150.0, **kw)

    def test_8l_shape_23_beyond_the_queue_passes(self):
        """8l Con: 27-block replay, 23 beyond the exec queue, default 15 s."""
        self.crash(gap=27, queue=4)
        s, _ = run_summarize(self.d, extra=self.env())
        c = s["crash"]
        self.assertEqual(c["rewind_beyond_exec_queue"], 23)
        self.assertEqual(c["checkpoint_interval_s"], 15.0)
        self.assertEqual(c["max_rewind_beyond_exec_queue"], 202)
        self.assertEqual(c["max_rewind_hard_cap"], 202)
        self.assertEqual(c["verdict"], "PASS", c["fail_reasons"])
        self.assertEqual(s["headline"]["crash_gate"], "PASS")

    def test_failed_scrape_rows_do_not_affect_the_bound(self):
        """Zero-valued rows from a failed val0 scrape around the kill: a
        23-block and a full-span (2 + 2 x 100) replay both still PASS."""
        add_failed_scrapes(self.d)
        for gap in (4 + 23, 4 + 202):
            self.crash(gap=gap, queue=4)
            s, _ = run_summarize(self.d, extra=self.env())
            c = s["crash"]
            self.assertEqual(c["max_rewind_beyond_exec_queue"], 202, gap)
            self.assertEqual(c["verdict"], "PASS", (gap, c["fail_reasons"]))

    def test_full_span_replay_passes_explicit_interval(self):
        """Open window (<= 100 blocks) + one checkpoint W was still writing
        (<= 100) + the hand-off slack 2: the worst valid case."""
        self.crash(gap=4 + 202, queue=4)
        s, _ = run_summarize(
            self.d, extra=self.env(TORUS_CHECKPOINT_INTERVAL_MS="15000")
        )
        self.assertEqual(s["crash"]["checkpoint_interval_s"], 15.0)
        self.assertEqual(s["crash"]["verdict"], "PASS", s["crash"]["fail_reasons"])

    def test_runaway_rewind_beyond_202_fails(self):
        add_failed_scrapes(self.d)
        self.crash(gap=4 + 203, queue=4)
        s, _ = run_summarize(self.d, extra=self.env())
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(any("rewind 203" in r for r in c["fail_reasons"]), c["fail_reasons"])
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")

    def test_per_block_configs_keep_the_tight_bound(self):
        """Interval 0, the flush worker off, or a deferred book mode (the role
        default yields to it) all mean the per-block flush: bound 2."""
        self.crash(gap=4 + 3, queue=4)
        for env in (
            self.env(TORUS_CHECKPOINT_INTERVAL_MS="0"),
            self.env(TORUS_EXEC_PIPELINE="0"),
            self.env(TORUS_BOOK_ROWS="3"),
        ):
            s, _ = run_summarize(self.d, extra=env)
            c = s["crash"]
            self.assertIsNone(c["checkpoint_interval_s"], env)
            self.assertEqual(c["max_rewind_beyond_exec_queue"], 2, env)
            self.assertEqual(c["verdict"], "FAIL", env)

    def test_genesis_book_mode_decides_the_checkpoint_default(self):
        """s131 review P1: the book mode is the genesis consensus.book_mode.
        A BOOK_MODE=-only mode-3 cell sets no TORUS_BOOK_ROWS on the nodes,
        so the summarizer must take the mode from --genesis-book-mode: mode
        2/3 turn the 15 s default off (bound 2, not 202), and the genesis
        wins over a node env that says otherwise. Empty = legacy env."""
        self.crash(gap=4 + 3, queue=4)
        cases = (
            # (genesis mode, node env TORUS_BOOK_ROWS, interval, label)
            ("level_authority_chunked", None, None, "mode3"),
            ("3", None, None, "mode3"),
            ("level_authority", None, None, "mode2"),
            ("order_rows", None, 15.0, "mode1"),
            ("classic", "3", 15.0, "classic"),
            ("0", None, 15.0, "classic"),
            ("", "3", None, "mode3"),      # legacy cell: the node env
            ("", None, 15.0, "classic"),
        )
        for mode, rows, want, label in cases:
            with self.subTest(mode=mode, rows=rows):
                kw = {} if rows is None else {"TORUS_BOOK_ROWS": rows}
                extra = self.env(**kw) + (["--genesis-book-mode", mode] if mode else [])
                s, _ = run_summarize(self.d, extra=extra)
                c = s["crash"]
                self.assertEqual(c["checkpoint_interval_s"], want)
                self.assertEqual(c["max_rewind_beyond_exec_queue"], 2 if want is None else 202)
                self.assertEqual(c["verdict"], "FAIL" if want is None else "PASS", c["fail_reasons"])
                self.assertEqual(s["cell"]["book_mode"], label)

    def test_bad_genesis_book_mode_is_refused(self):
        for mode in ("mode3", "Classic", "4", "x"):
            with self.subTest(mode=mode):
                with self.assertRaises(AssertionError) as cm:
                    run_summarize(self.d, extra=self.env() + ["--genesis-book-mode", mode])
                self.assertIn("is not a book mode", str(cm.exception))

    def test_interval_parse_matches_the_node(self):
        """TORUS_CHECKPOINT_INTERVAL_MS parses like the node's
        exec_pipeline::parse_checkpoint_interval: blank after trim -> the 15 s
        default; else Rust `trim().parse::<u64>()` (optional leading '+',
        ASCII digits, <= u64::MAX); 0 or any Err (incl. overflow) -> OFF."""
        self.crash(gap=4 + 3, queue=4)
        cases = (
            ("+15000", 15.0),
            (" 15000 ", 15.0),
            ("\t15000\n", 15.0),
            ("   ", 15.0),            # blank = unset = role default
            ("18446744073709551615", 18446744073709551615 / 1000.0),  # u64::MAX
            ("18446744073709551616", None),  # overflow -> Err -> off
            ("0", None),
            ("+0", None),
            ("+", None),
            ("-15000", None),
            ("++15000", None),
            ("15_000", None),
            ("15000ms", None),
            ("\u0661\u0665", None),  # non-ASCII digits: Rust rejects
        )
        for raw, want in cases:
            s, _ = run_summarize(
                self.d, extra=self.env(TORUS_CHECKPOINT_INTERVAL_MS=raw)
            )
            c = s["crash"]
            self.assertEqual(c["checkpoint_interval_s"], want, repr(raw))
            self.assertEqual(
                c["max_rewind_beyond_exec_queue"], 2 if want is None else 202, repr(raw)
            )

    def test_each_kill_of_a_multi_crash_cell_gets_the_checkpoint_bound(self):
        self.crash(gap=27, queue=4, seq=1)
        write_crash(
            self.d, gap=4 + 203, queue=4, name="crash-2.json", seq=2,
            kill_at=180, kill_ts=BENCH_START + 180.0,
        )
        s, _ = run_summarize(self.d, extra=self.env())
        ks = s["crash_kills"]
        self.assertEqual([k["verdict"] for k in ks], ["PASS", "FAIL"], ks)
        self.assertEqual(ks[1]["max_rewind_beyond_exec_queue"], 202)
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")


# ------------------------------------ run-cell.sh: s76 BENCH_RPCS (bench ingress)
class BenchRpcUrlsTest(unittest.TestCase):
    """run-cell.sh's bench_rpc_urls: unset/all (default since s76) spreads the
    senders over all three validators (the bench pins sender i to url i % n);
    BENCH_RPCS=0 is the val0-only ingress every pre-s76 cell used; anything
    else is a fatal typo, never a silent run of the wrong shape."""

    def setUp(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        a = src.index("bench_rpc_urls() {")
        self.fn = src[a : src.index("\n}\n", a) + 3]

    def urls(self, value=None):
        env = {k: v for k, v in os.environ.items() if k != "BENCH_RPCS"}
        if value is not None:
            env["BENCH_RPCS"] = value
        r = subprocess.run(
            [
                "bash",
                "-c",
                self.fn + "\nRPCS=(http://a:1 http://b:2 http://c:3); bench_rpc_urls",
            ],
            capture_output=True,
            text=True,
            env=env,
            timeout=30,
        )
        return r.returncode, r.stdout.strip()

    def test_zero_keeps_val0_only(self):
        self.assertEqual(self.urls("0"), (0, "http://a:1"))

    def test_unset_and_all_pass_every_validator_comma_separated(self):
        every = (0, "http://a:1,http://b:2,http://c:3")
        self.assertEqual(self.urls(), every)
        self.assertEqual(self.urls(""), every)
        self.assertEqual(self.urls("all"), every)

    def test_unknown_value_is_fatal(self):
        rc, out = self.urls("val1")
        self.assertNotEqual(rc, 0)
        self.assertEqual(out, "")


# ------------------------------------ run-cell.sh: s75 multi-crash kill sequence
STUB_CRASH_KILL = r"""#!/usr/bin/env bash
# stub: records its call, writes the record like crash-kill.sh, never kills.
echo "$4" >> "$3/calls"
n=$(wc -l < "$3/calls")
[ "$n" = "${STUB_FAIL_AT:-0}" ] && exit 1
sleep "${STUB_SLEEP:-0}"
printf '{"enabled": true, "restart_ts": %s}\n' "$(date +%s.%N)" > "$3/$4"
"""


class CrashSequenceTest(unittest.TestCase):
    """run-cell.sh's crash_sequence, run for real against a stub crash-kill.sh:
    kills in order, stop at the first failure, skip (never back-to-back) when
    a kill+restart overran into the next kill's slot."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="crash-seq-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        with open(RUN_CELL_SH) as f:
            src = f.read()
        a = src.index("crash_sequence() {")
        self.fn = src[a : src.index("\n}\n", a) + 3]
        with open(os.path.join(self.d, "crash-kill.sh"), "w") as f:
            f.write(STUB_CRASH_KILL)
        os.chmod(os.path.join(self.d, "crash-kill.sh"), 0o755)

    def run_seq(self, at, min_up, **env):
        script = (
            'source "$1"; '
            + self.fn
            + "\nCRASH_AT=(%s); CRASH_KILL_MIN_UP_S=%s; crash_sequence"
            % (" ".join(at), min_up)
        )
        r = subprocess.run(
            ["bash", "-c", script, "_", CRASH_KILL_SH],
            capture_output=True,
            text=True,
            env=dict(
                os.environ,
                CRASH_KILL_LIB="1",
                TOOLS_DIR=self.d,
                OUT=self.d,
                WT=self.d,
                KILL_IDX="1",
                **env,
            ),
            timeout=60,
        )
        try:
            with open(os.path.join(self.d, "calls")) as f:
                calls = f.read().split()
        except OSError:
            calls = []
        return r.returncode, calls, r.stdout

    def test_kills_run_in_order_with_their_record_names(self):
        rc, calls, _ = self.run_seq(["0", "0.2", "0.4"], 0)
        self.assertEqual(rc, 0)
        self.assertEqual(
            calls, ["crash-kill.json", "crash-kill-2.json", "crash-kill-3.json"]
        )

    def test_a_failed_kill_stops_the_sequence(self):
        rc, calls, out = self.run_seq(["0", "0.2", "0.4"], 0, STUB_FAIL_AT="2")
        self.assertNotEqual(rc, 0)
        self.assertEqual(calls, ["crash-kill.json", "crash-kill-2.json"])
        self.assertIn("FAILED", out)
        self.assertFalse(os.path.exists(os.path.join(self.d, "crash-kill-2.json")))

    def test_an_overrun_skips_the_remaining_kills(self):
        """kill 1 takes 1.5 s, kill 2 was due 1 s in: < min-up since kill 1's
        restart -> kills 2..3 are skipped, loudly, never fired back-to-back."""
        rc, calls, out = self.run_seq(["0", "1", "2"], 5, STUB_SLEEP="1.5")
        self.assertEqual(rc, 0)
        self.assertEqual(calls, ["crash-kill.json"])
        self.assertIn("SKIPPING kills 2..3", out)
        with open(os.path.join(self.d, "crash-kill.skipped")) as f:
            self.assertEqual(f.read().strip(), "2")


# ------------------------------------ summarize.py: s75 multi-crash per-kill list
class MultiCrashSummaryTest(unittest.TestCase):
    """CRASH_KILL_AT_S="60,180,..." kills val1 several times in one cell. Kill 1
    stays in crash.json (the legacy gate); kill k>=2 lands in crash-<k>.json.
    The headline gate must FAIL when ANY kill went wrong, not just the first."""

    ON = CrashGateSummaryTest.ON

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="multi-crash-summ-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        write_crash(self.d, seq=1, kill_at=60, kill_ts=BENCH_START + 60.0)

    def kill(self, k, **kw):
        at = 60 + 120 * (k - 1)
        write_crash(
            self.d,
            name="crash-%d.json" % k,
            seq=k,
            kill_at=at,
            kill_ts=BENCH_START + at + 0.25,
            **kw,
        )

    def test_single_kill_cell_has_no_per_kill_list(self):
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertNotIn("crash_kills", s)
        self.assertEqual(s["headline"]["crash_gate"], "PASS")

    def test_per_kill_list_is_reported_in_order(self):
        self.kill(2)
        self.kill(3, gap=1, queue=1)
        s, out = run_summarize(self.d, extra=self.ON)
        ks = s["crash_kills"]
        self.assertEqual([k["kill_seq"] for k in ks], [1, 2, 3])
        self.assertEqual([k["kill_at_s"] for k in ks], [60, 180, 300])
        for k in ks:
            for key in (
                "kill_ts",
                "restart_ts",
                "down_s",
                "rewind_blocks",
                "rewind_beyond_exec_queue",
                "panic_or_failstop_lines",
                "error_lines",
                "hole_lines",
                "verdict",
                "fail_reasons",
            ):
                self.assertIn(key, k)
            self.assertEqual(k["verdict"], "PASS", k)
        self.assertEqual(ks[1]["kill_ts"], BENCH_START + 180.25)
        self.assertEqual(ks[2]["rewind_blocks"], 1)
        self.assertEqual(s["crash"]["verdict"], "PASS")
        self.assertEqual(s["headline"]["crash_gate"], "PASS")
        self.assertIn("kill 3", out)

    def test_kills_sort_numerically_not_lexically(self):
        self.kill(10)
        self.kill(2)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual([k["kill_seq"] for k in s["crash_kills"]], [1, 2, 10])

    def test_a_panic_after_a_later_kill_fails_the_headline(self):
        self.kill(2, panics=1)
        self.kill(3)
        s, _ = run_summarize(self.d, extra=self.ON)
        ks = s["crash_kills"]
        self.assertEqual(ks[0]["verdict"], "PASS")
        self.assertEqual(ks[1]["verdict"], "FAIL")
        self.assertEqual(ks[1]["panic_or_failstop_lines"], 1)
        self.assertEqual(s["crash"]["verdict"], "FAIL")
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")
        self.assertTrue(
            any("kill 2" in r and "panic" in r for r in s["crash"]["fail_reasons"]),
            s["crash"]["fail_reasons"],
        )
        self.assertFalse(s["validity"]["accepted"])

    def test_a_later_kill_that_never_restarted_fails(self):
        self.kill(2, restarted_pid=None)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["crash_kills"][1]["verdict"], "FAIL")
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")

    def test_a_later_kill_with_an_unbounded_rewind_fails(self):
        self.kill(2, gap=8, queue=3)
        s, _ = run_summarize(self.d, extra=CrashGateSummaryTest.PER_BLOCK)
        self.assertEqual(s["crash_kills"][1]["rewind_beyond_exec_queue"], 5)
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")

    def test_a_later_kill_without_the_pipeline_line_fails_when_flag_on(self):
        self.kill(2, pipeline_line=False)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")

    def test_a_kill_record_without_its_log_scan_fails(self):
        """crash-kill-2.json exists (the kill happened) but the post-cell scan
        never produced crash-2.json: that kill is unverified, never silently
        dropped from the list."""
        self.kill(2)
        os.rename(
            os.path.join(self.d, "crash-2.json"),
            os.path.join(self.d, "crash-kill-2.json"),
        )
        s, _ = run_summarize(self.d, extra=self.ON)
        ks = s["crash_kills"]
        self.assertEqual([k["kill_seq"] for k in ks], [1, 2])
        self.assertEqual(ks[1]["verdict"], "FAIL")
        self.assertEqual(s["headline"]["crash_gate"], "FAIL")


# ------------------------------------------------ crash-freeze.py: per-kill freeze
CRASH_FREEZE = os.path.join(HERE, "crash-freeze.py")


def commit_line(t, h, ansi=True):
    """The val0 line crash-freeze.py reads, in the devnet's ANSI fmt::layer()."""
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t)) + (
        ".%06dZ" % round((t % 1) * 1e6)
    )
    if not ansi:
        return (
            "%s  INFO torus_consensus::app: on_committed_block: sending to "
            "execution pipeline height=%d evm_txs=0 native=200\n" % (stamp, h)
        )
    return (
        "\x1b[2m%s\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mtorus_consensus::app"
        "\x1b[0m\x1b[2m:\x1b[0m on_committed_block: sending to execution "
        "pipeline \x1b[3mheight\x1b[0m\x1b[2m=\x1b[0m%d \x1b[3mevm_txs\x1b[0m"
        "\x1b[2m=\x1b[0m0\n" % (stamp, h)
    )


class CrashFreezeTest(unittest.TestCase):
    """Per-kill chain freeze: window k = [kill_ts_k, kill_ts_{k+1}), the last
    one ends at bench end; max gap between consecutive val0 commits, with the
    stretch that straddles the kill counted in kill k's window."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="crash-freeze-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        T = BENCH_START
        # commits every 0.5 s, frozen (t0+60.2, t0+95.2) and (t0+180.1, t0+200.1)
        ts = [T + i * 0.5 for i in range(0, 601)]
        ts = [
            t
            for t in ts
            if not (T + 60.2 < t < T + 95.2) and not (T + 180.1 < t < T + 200.1)
        ]
        ts = sorted(set(ts) | {T + 60.2, T + 95.2, T + 180.1, T + 200.1})
        with gzip.open(os.path.join(self.d, "val0.log.gz"), "wt") as f:
            for h, t in enumerate(ts, start=100):
                f.write(commit_line(t, h, ansi=h % 2 == 0))
                if h % 7 == 0:  # the node logs some heights twice
                    f.write(commit_line(t + 0.001, h))
        with open(os.path.join(self.d, "summary.json"), "w") as f:
            json.dump({"timing": {"t_bench0": T, "t_bench1": T + 300}}, f)
        write_crash(self.d, seq=1, kill_at=60, kill_ts=T + 60.0, restart_ts=T + 61.0)
        write_crash(
            self.d,
            name="crash-2.json",
            seq=2,
            kill_at=180,
            kill_ts=T + 180.0,
            restart_ts=T + 181.5,
        )

    def run_freeze(self):
        r = subprocess.run(
            [sys.executable, CRASH_FREEZE, self.d, "--json"],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_per_kill_freeze_from_the_val0_commit_log(self):
        j = self.run_freeze()
        self.assertEqual(j["source"], "val0.log.gz")
        ks = j["kills"]
        self.assertEqual([k["kill_seq"] for k in ks], [1, 2])
        self.assertAlmostEqual(ks[0]["max_commit_gap_s"], 35.0, places=2)
        self.assertAlmostEqual(ks[0]["restart_to_first_commit_s"], 34.2, places=2)
        self.assertAlmostEqual(ks[0]["window_end_ts"], BENCH_START + 180.0)
        self.assertAlmostEqual(ks[1]["max_commit_gap_s"], 20.0, places=2)
        self.assertAlmostEqual(ks[1]["restart_to_first_commit_s"], 18.6, places=2)
        self.assertAlmostEqual(ks[1]["window_end_ts"], BENCH_START + 300.0)
        self.assertAlmostEqual(ks[0]["down_s"], 1.4)

    def test_falls_back_to_the_1hz_sampler(self):
        os.remove(os.path.join(self.d, "val0.log.gz"))
        write_cell(self.d, 1_000, 1_000)  # committed +10 every second
        j = self.run_freeze()
        self.assertEqual(j["source"], "sampler.csv")
        self.assertEqual([k["max_commit_gap_s"] for k in j["kills"]], [1, 1])

    def test_tsv_output(self):
        r = subprocess.run(
            [sys.executable, CRASH_FREEZE, self.d],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        rows = [l.split("\t") for l in r.stdout.splitlines() if not l.startswith("#")]
        self.assertEqual(rows[0][0], "kill_seq")
        self.assertEqual(len(rows), 3)


# The exact shape the node writes after a kill -9 restart (r9 waloff crash proof,
# mem d448f539: real line, real numbers).
REPLAY_TAIL = """2026-08-20T03:24:11.101234Z  INFO torus_node: starting torus-node
2026-08-20T03:24:11.201234Z  WARN torus_consensus::app: crash recovery: execution gap detected, replaying committed_height=90 applied_height=79 gap=11
2026-08-20T03:24:18.301234Z  INFO torus_consensus::app: bl2 exec pipeline ENABLED (TORUS_EXEC_PIPELINE=1): flush worker attached after replay applied=90
2026-08-20T03:24:18.401234Z  INFO torus_consensus::exec_pipeline: flush worker thread started (TORUS_EXEC_PIPELINE)
2026-08-20T03:24:24.501234Z  INFO torus_node: caught up
"""


class CrashScanTest(unittest.TestCase):
    """`crash-kill.sh scan` reads the ONE line that says how far the crash
    rewound the node. If that parse silently yields nothing, the gate reports a
    0-block rewind and PASSES a broken fence."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="crash-scan-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def scan(self, text):
        f = os.path.join(self.d, "tail.log")
        with open(f, "w") as fh:
            fh.write(text)
        r = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; crash_scan_restart_tail "$@"',
                "_",
                CRASH_KILL_SH,
                f,
            ],
            capture_output=True,
            text=True,
            env=dict(os.environ, CRASH_KILL_LIB="1"),
            timeout=60,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_parses_the_replay_line(self):
        j = self.scan(REPLAY_TAIL)
        self.assertTrue(j["replay_line_found"])
        self.assertEqual(j["applied_height_at_crash"], 79)
        self.assertEqual(j["committed_height_at_crash"], 90)
        self.assertEqual(j["gap"], 11)
        self.assertTrue(j["pipeline_enabled_line"])
        self.assertEqual(j["worker_attached_applied"], 90)
        self.assertEqual(j["panic_or_failstop_lines"], 0)
        self.assertEqual(j["hole_lines"], 0)

    def test_parses_the_json_log_shape_too(self):
        j = self.scan(
            '{"timestamp":"x","level":"WARN","fields":{"message":"crash recovery: '
            'execution gap detected, replaying","committed_height":90,'
            '"applied_height":79,"gap":11}}\n'
        )
        self.assertEqual(j["applied_height_at_crash"], 79)
        self.assertEqual(j["gap"], 11)

    def test_a_clean_restart_has_no_replay_line(self):
        j = self.scan("INFO torus_node: starting torus-node\nINFO ready\n")
        self.assertFalse(j["replay_line_found"])
        self.assertEqual(j["gap"], 0)
        self.assertIsNone(j["applied_height_at_crash"])
        self.assertFalse(j["pipeline_enabled_line"])

    # ---- 8l (perf doc 107.6): only REAL fail-stops count ------------------
    ESC = "\x1b"

    def test_info_failstop_config_line_is_not_a_failstop(self):
        """Every node logs `INFO ... running state hash fail-stop (...) on=false`
        at startup; the restart tail counted it and REJECTed a valid crash cell.
        WARN mentions ("fail-stop is off", "no fail-stop decision") are not
        fail-stops either: the node emits every real one at ERROR."""
        e = self.ESC
        j = self.scan(
            REPLAY_TAIL
            + "2026-10-20T06:31:02.1Z  INFO torus_consensus::state_hash: running state "
            "hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false\n"
            + f"{e}[2m2026-10-20T06:31:02.1Z{e}[0m {e}[32m INFO{e}[0m "
            f"{e}[2mtorus_consensus::state_hash{e}[0m{e}[2m:{e}[0m running state hash "
            f"fail-stop (TORUS_STATE_HASH_FAILSTOP) {e}[3mon{e}[0m{e}[2m={e}[0mfalse\n"
            + "2026-10-20T06:35:00.1Z  WARN torus_consensus::state_hash: running state "
            "hash: this node diverged from the on-chain quorum hash at this checkpoint "
            "(recorded); fail-stop is off, so it keeps running\n"
            + '{"timestamp":"x","level":"INFO","fields":{"message":"running state hash '
            'fail-stop (TORUS_STATE_HASH_FAILSTOP)","on":false}}\n'
            # s136: fail-stop on by default, the echo reads on=true
            + "2026-10-20T06:31:02.1Z  INFO torus_consensus::state_hash: running state "
            "hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=true\n"
            + f"{e}[2m2026-10-20T06:31:02.1Z{e}[0m {e}[32m INFO{e}[0m "
            f"{e}[2mtorus_consensus::state_hash{e}[0m{e}[2m:{e}[0m running state hash "
            f"fail-stop (TORUS_STATE_HASH_FAILSTOP) {e}[3mon{e}[0m{e}[2m={e}[0mtrue\n"
        )
        self.assertEqual(j["panic_or_failstop_lines"], 0)
        self.assertEqual(j["gap"], 11)

    def test_real_failstop_lines_count(self):
        e = self.ESC
        j = self.scan(
            REPLAY_TAIL
            + "2026-10-20T06:40:00.1Z ERROR torus_consensus::exec_pipeline: FATAL: flush "
            "worker PANICKED \u2014 latching fail-stop height=1200\n"
            + f"{e}[2m2026-10-20T06:40:01.1Z{e}[0m {e}[31mERROR{e}[0m "
            f"{e}[2mtorus_consensus::state_hash{e}[0m: FAIL-STOP "
            "(TORUS_STATE_HASH_FAILSTOP=1): latching exec_failed, node stops voting\n"
            + '{"timestamp":"x","level":"ERROR","fields":{"message":"CRITICAL SAFETY '
            "VIOLATION: a DIFFERENT block arrived at an already-applied height \u2014 two "
            'conflicting blocks were finalized at the same height."}}\n'
            + "thread 'torus-execution' panicked at crates/x.rs:1:1\n"
        )
        self.assertEqual(j["panic_or_failstop_lines"], 4)

    def test_counts_panics_holes_and_errors(self):
        j = self.scan(
            REPLAY_TAIL
            + "ERROR torus_consensus::app: crash recovery: execution gap could not be "
            "fully replayed LOCALLY hole_height=85\n"
            "thread 'torus-execution' panicked at src/app.rs:1\n"
            " ERROR something else\n"
        )
        self.assertEqual(j["panic_or_failstop_lines"], 1)
        self.assertGreaterEqual(j["hole_lines"], 1)
        self.assertGreaterEqual(j["error_lines"], 1)
        # the replay numbers survive the noise
        self.assertEqual(j["gap"], 11)


class MarkStub(BaseHTTPRequestHandler):
    """val0 stand-in for the oracle freshness probe: head 100; market 1 fresh
    (aggregate at block 150), market 2 usable but written before the feed
    (block 90), market 3 no usable mark (RPC reports 0), market 4 an RPC error."""

    protocol_version = "HTTP/1.1"
    MARKS = {"0x1": ("30000", 150), "0x2": ("30000", 90), "0x3": ("0", 0)}

    def log_message(self, *_a):
        pass

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers["content-length"])))
        if req["method"] == "eth_blockNumber":
            resp = {"jsonrpc": "2.0", "id": 1, "result": "0x64"}
        elif req["params"][0] in self.MARKS:
            price, blk = self.MARKS[req["params"][0]]
            resp = {
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "marketId": req["params"][0],
                    "markPrice": price,
                    "indexPrice": price,
                    "lastTradePrice": "0",
                    "timestamp": blk,
                },
            }
        else:
            resp = {
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32602, "message": "x"},
            }
        body = json.dumps(resp).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class OracleFeedHarnessTest(unittest.TestCase):
    """s87 ORACLE_FEED=1: run-cell.sh runs `bench-throughput oracle-feed`
    around the load. Pinned: the freshness probe, the feed is stopped on every
    exit path (even while SIGSTOPped for the drain), and ORACLE_FEED unset
    leaves the cell untouched."""

    @classmethod
    def setUpClass(cls):
        with open(RUN_CELL_SH) as f:
            cls.src = f.read()

    def fn(self, start, end_marker):
        i = self.src.index(start)
        return self.src[i : self.src.index(end_marker, i)]

    def test_oracle_marks_counts_usable_and_fresh_since_the_feed_start(self):
        srv = ThreadingHTTPServer(("127.0.0.1", 0), MarkStub)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        try:
            url = "http://127.0.0.1:%d" % srv.server_address[1]
            script = (
                self.fn("oracle_marks() {", "\nORACLE_FRESH_S=")
                + '\nRPCS=("%s"); MARKETS=4; oracle_marks 100\n' % url
            )
            r = subprocess.run(
                ["bash", "-c", script], capture_output=True, text=True, timeout=60
            )
        finally:
            srv.shutdown()
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        self.assertEqual(
            (
                out["markets"],
                out["checked"],
                out["usable"],
                out["fresh"],
                out["errors"],
            ),
            (4, 4, 2, 1, 1),
        )
        self.assertEqual(out["head"], 100)
        self.assertEqual(out["not_fresh_ids"], [2, 3, 4])
        self.assertEqual(out["max_agg_age_blocks"], 10)

    def test_stop_oracle_feed_terminates_a_paused_feed_and_is_idempotent(self):
        script = (
            self.fn("alive() {", "\nstop_sampler() {")
            + '\nlog() { echo "LOG $*"; }\n'
            + 'sleep 300 & ORACLE_PID=$!; p=$ORACLE_PID; kill -STOP "$p"\n'
            + "stop_oracle_feed; stop_oracle_feed\n"
            + 'alive "$p" && echo STILL_ALIVE; echo "rc=$ORACLE_RC pid=[$ORACLE_PID]"\n'
        )
        t0 = time.monotonic()
        r = subprocess.run(
            ["bash", "-c", script], capture_output=True, text=True, timeout=30
        )
        self.assertLess(
            time.monotonic() - t0,
            8,
            "a TERM'd paused feed must not wait for the KILL grace",
        )
        self.assertNotIn("STILL_ALIVE", r.stdout)
        self.assertIn("rc=143 pid=[]", r.stdout)
        self.assertEqual(r.stdout.count("stopped rc="), 1, r.stdout)

    def test_feed_is_stopped_on_every_exit_path_before_the_nodes(self):
        ff = self.fn("finish_fail() {", "\n}\n")
        self.assertLess(ff.index("stop_oracle_feed"), ff.index("stop-3val.sh"))
        self.assertIn("then trap 'stop_oracle_feed' EXIT", self.src)
        # stopped after the digest, before the nodes go down
        dig = self.src.index('log "state digest done')
        stop = self.src.index("    stop_oracle_feed\n", dig)
        self.assertLess(
            stop, self.src.index('"$WSL/stop-3val.sh" >>"$OUT/run.log" 2>&1\n', dig)
        )

    def test_feed_paused_for_drain_and_digest_never_waited_on(self):
        pause = self.src.index('kill -STOP "$ORACLE_PID"')
        self.assertLess(
            pause,
            self.src.index(
                "# ---------------------------------------------------------------- 7. drain"
            ),
        )
        self.assertGreater(pause, self.src.index('log "bench exited rc='))
        self.assertNotIn(
            "\nwait\n", self.src, "a bare wait blocks forever on the paused feed"
        )
        self.assertIn('wait "${DIG_PIDS[@]}"', self.src)

    def test_feed_starts_after_health_and_before_the_load(self):
        start = self.src.index('"${ORACLE_CMD[@]}" >')
        self.assertGreater(start, self.src.index('log "idle blk/s'))
        self.assertLess(start, self.src.index('"${BENCH_CMD[@]}" >'))
        cmd = self.fn("ORACLE_CMD=(", ")\n")
        for flag in (
            "oracle-feed",
            "--rpc-urls",
            '--validator-keys "$ORACLE_KEYS"',
            '--markets "$MARKETS"',
            '--price "$ORACLE_PRICE"',
            '--interval-ms "$ORACLE_INTERVAL_MS"',
            "--stats-file",
        ):
            self.assertIn(flag, cmd)
        self.assertNotIn("cargo build", cmd)
        self.assertNotIn("consensus", cmd)

    def test_every_oracle_step_is_gated_and_defaults_off(self):
        self.assertIn("ORACLE_FEED=${ORACLE_FEED:-0}", self.src)
        self.assertIn(
            'ORACLE_KEYS="$MAINREPO/devnet/wsl/bench-validator-keys.json"', self.src
        )
        # every cell-flow oracle step sits inside an `if [ "$ORACLE_FEED" = 1 ]` block
        for marker in (
            "ORACLE_M=$(oracle_marks 0)",
            'kill -STOP "$ORACLE_PID"',
            '    stop_oracle_feed\n    log "oracle feed: oracle_feed=1',
            "d['oracle_feed'] = o",
        ):
            i = self.src.index(marker)
            gate = self.src.rindex('if [ "$ORACLE_FEED" = 1 ]', 0, i)
            self.assertNotIn("\nfi\n", self.src[gate:i], marker)

    def test_bad_oracle_env_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="oracle-pre-")
        try:
            tgt = os.path.join(tmp, "release")
            os.makedirs(tgt)
            for b in ("torus-node", "bench-throughput"):
                p = os.path.join(tgt, b)
                with open(p, "w") as f:
                    f.write("#!/bin/sh\nexit 0\n")
                os.chmod(p, 0o755)
            script, wt = sandbox_repo(tmp)
            for env, msg in (
                (dict(ORACLE_FEED="yes"), "ORACLE_FEED must be"),
                (dict(ORACLE_FEED="1", ORACLE_PRICE="-5"), "ORACLE_PRICE must be"),
                (
                    dict(ORACLE_FEED="1", ORACLE_INTERVAL_MS="10000"),
                    "ORACLE_INTERVAL_MS must be",
                ),
            ):
                r = subprocess.run(
                    [script, wt, "oracle-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        **env,
                    ),
                )
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn(msg, r.stderr)
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, "oracle-pre-x")),
                    "must fail before any launch",
                )
        finally:
            shutil.rmtree(tmp)

    def test_walk_bp_reaches_the_feed_only_when_set(self):
        """Item 6: ORACLE_WALK_BP=N -> oracle-feed --walk-bp N. 0 (default) =
        flag omitted, so the command is exactly the fixed-price feed and older
        bench-throughput binaries keep working. Recorded in the summary."""
        self.assertIn("ORACLE_WALK_BP=${ORACLE_WALK_BP:-0}", self.src)
        block = self.fn("    ORACLE_CMD=(", '\n    log "oracle feed: ${ORACLE_CMD[*]}"')
        for walk, want in (("0", []), ("10", ["--walk-bp", "10"])):
            script = (
                "BENCH=bt RPCS=(r0 r1) ORACLE_KEYS=k MARKETS=3 ORACLE_PRICE=30000 "
                "ORACLE_INTERVAL_MS=2000 OUT=o ORACLE_WALK_BP=%s\n"
                % walk
                + block
                + '\nprintf "%s\\n" "${ORACLE_CMD[@]}"\n'
            )
            r = subprocess.run(
                ["bash", "-c", script], capture_output=True, text=True, timeout=30
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            argv = r.stdout.split("\n")[:-1]
            self.assertEqual(argv[:2], ["bt", "oracle-feed"])
            self.assertEqual(argv[-len(want) :] if want else [], want, argv)
            self.assertEqual(argv.count("--walk-bp"), len(want) // 2, argv)
        self.assertIn("'walk_bp': int(walk_bp)", self.src)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            self.assertIn('"ORACLE_WALK_BP",', f.read())

    def test_feed_drain_defaults_off_and_bad_env_fails_preflight(self):
        """ORACLE_FEED_DRAIN=1 keeps the feed live through the drain. 0 (default)
        = today's cell; any other value, or 1 without ORACLE_FEED=1, is FATAL."""
        self.assertIn("ORACLE_FEED_DRAIN=${ORACLE_FEED_DRAIN:-0}", self.src)
        doc = self.src.index("#   ORACLE_FEED_DRAIN=1")
        self.assertLess(self.src.index("#   ORACLE_FEED=1"), doc)
        self.assertLess(doc, self.src.index("set -uo pipefail"))
        self.assertIn(
            "ORACLE_FEED_DRAIN_MAX_LAG=${ORACLE_FEED_DRAIN_MAX_LAG:-2}", self.src
        )
        self.assertLess(doc, self.src.index("#   ORACLE_FEED_DRAIN_MAX_LAG=N"))
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            allow = f.read()
        self.assertIn('"ORACLE_FEED_DRAIN",', allow)
        self.assertIn('"ORACLE_FEED_DRAIN_MAX_LAG",', allow)
        tmp = tempfile.mkdtemp(prefix="oracle-drain-pre-")
        try:
            tgt = os.path.join(tmp, "release")
            os.makedirs(tgt)
            for b in ("torus-node", "bench-throughput"):
                p = os.path.join(tgt, b)
                with open(p, "w") as f:
                    f.write("#!/bin/sh\nexit 0\n")
                os.chmod(p, 0o755)
            script, wt = sandbox_repo(tmp)
            for env, msg in (
                (dict(ORACLE_FEED_DRAIN="2"), "ORACLE_FEED_DRAIN must be 0 or 1"),
                (
                    dict(ORACLE_FEED="1", ORACLE_FEED_DRAIN="yes"),
                    "ORACLE_FEED_DRAIN must be 0 or 1",
                ),
                (
                    dict(ORACLE_FEED_DRAIN="1"),
                    "ORACLE_FEED_DRAIN=1 needs ORACLE_FEED=1",
                ),
                (
                    dict(ORACLE_FEED="0", ORACLE_FEED_DRAIN="1"),
                    "ORACLE_FEED_DRAIN=1 needs ORACLE_FEED=1",
                ),
                (
                    dict(
                        ORACLE_FEED="1",
                        ORACLE_FEED_DRAIN="1",
                        ORACLE_FEED_DRAIN_MAX_LAG="x",
                    ),
                    "ORACLE_FEED_DRAIN_MAX_LAG must be a non-negative integer",
                ),
                (
                    dict(
                        ORACLE_FEED="1",
                        ORACLE_FEED_DRAIN="1",
                        ORACLE_FEED_DRAIN_MAX_LAG="-1",
                    ),
                    "ORACLE_FEED_DRAIN_MAX_LAG must be a non-negative integer",
                ),
                (
                    dict(ORACLE_FEED="1", ORACLE_FEED_DRAIN_MAX_LAG="100"),
                    "ORACLE_FEED_DRAIN_MAX_LAG=100 needs ORACLE_FEED_DRAIN=1",
                ),
            ):
                r = subprocess.run(
                    [script, wt, "oracle-drain-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        **env,
                    ),
                )
                self.assertEqual(r.returncode, 2, (env, r.stderr))
                self.assertIn(msg, r.stderr)
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, "oracle-drain-pre-x")),
                    "must fail before any launch",
                )
        finally:
            shutil.rmtree(tmp)

    def _run_drain_flow(self, mode, max_lag="2", settle_max="600", settle_rc=0, settle=None,
                        attest="0", feed="1"):
        """Run run-cell.sh's real bench-end pause block and drain block with
        python3 stubbed: each health.py call prints the feed's process state
        (T = SIGSTOPped) and its argv; the feed-stop-settle call returns
        settle_rc and leaves `settle` as its drain.json."""
        pause = self.fn(
            'ORACLE_ALIVE_END=""\n',
            "\n# ------------------------------------------------"
            "---------------- 7. drain",
        )
        drain = self.fn(
            "# ---------------------------------------------------------------- 7. drain",
            "\nsleep 2\n",
        )
        tmp = tempfile.mkdtemp(prefix="oracle-drain-flow-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        # what the (stubbed) health.py runs would have left behind
        os.makedirs(os.path.join(tmp, "feed-stop-settle"))
        with open(os.path.join(tmp, "drain.json"), "w") as f:
            json.dump(
                {
                    "drained": True,
                    "feed_live": {
                        "max_exec_lag": 2,
                        "native_intervals": 4,
                        "single_block_intervals": 3,
                        "native_blocks": 5,
                        "chain_ms": {"p50": 50.0, "p95": 200.0, "max": 200.0},
                    },
                },
                f,
            )
        with open(os.path.join(tmp, "feed-stop-settle", "drain.json"), "w") as f:
            json.dump(
                settle
                or {
                    "drained": True,
                    "elapsed_s": 12.7,
                    "target_height": 4321,
                    "target_reached": True,
                    "exec_heights": {"val0": 4330, "val1": 4329, "val2": 4330},
                },
                f,
            )
        script = (
            self.fn("alive() {", "\nstop_sampler() {")
            + '\nlog() { echo "LOG $*"; }\n'
            + 'oracle_marks() { echo \'{"markets":300,"usable":300}\'; }\n'
            + 'python3() { echo "PY $(ps -o stat= -p "$FEED" | cut -c1) $*" >> "%s/py.log"; '
            'case "$*" in *feed-stop-settle*) return %d;; esac; }\n' % (tmp, settle_rc)
            + next(l for l in self.src.splitlines() if l.startswith("state_attest_on() {")) + "\n"
            + "sleep 300 & ORACLE_PID=$!; FEED=$ORACLE_PID\n"
            + "trap 'kill -KILL $FEED 2>/dev/null' EXIT\n"
            + "OUT=%s TOOLS_DIR=/tools DRAIN_TIMEOUT=780 MARKETS=300 ORACLE_H0=7 "
            "ORACLE_FEED=%s ORACLE_FEED_DRAIN=%s ORACLE_FEED_DRAIN_MAX_LAG=%s "
            "FEED_STOP_SETTLE_MAX_S=%s STATE_HASH_ATTEST=%s T_BENCH1=$(date +%%s)\n"
            % (tmp, feed, mode, max_lag, settle_max, attest)
            + "METS=(9161 9162 9163)\n"
            + "RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)\n"
            + pause
            + "\n"
            + drain
            + '\ncat "$OUT/py.log"; echo "END $(ps -o stat= -p "$FEED" | cut -c1)"\n'
            # the real exit-path stop must still end the feed paused after the drain
            + 'stop_oracle_feed; alive "$FEED" && echo FEED_STILL_ALIVE; echo "STOPPED rc=$ORACLE_RC"\n'
        )
        r = subprocess.run(
            ["bash", "-c", script], capture_output=True, text=True, timeout=30
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return tmp, r.stdout

    def test_default_drain_flow_is_unchanged(self):
        out_dir, out = self._run_drain_flow("0")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        urls = " ".join("http://127.0.0.1:%d/metrics" % p for p in (9161, 9162, 9163))
        self.assertEqual(
            py,
            [
                "PY T /tools/health.py drain --out %s --timeout 780 --quiet 10 --urls %s"
                % (out_dir, urls)
            ],
        )
        self.assertIn("LOG oracle feed paused (SIGSTOP) for drain + digest", out)
        self.assertIn("END T", out)
        self.assertNotIn("FEED_STILL_ALIVE", out)
        self.assertIn("STOPPED rc=143", out)

    def test_feed_drain_max_lag_reaches_the_feed_live_drain_only(self):
        """ORACLE_FEED_DRAIN_MAX_LAG (value-sum proof cells: exec ~0.34 s per
        block keeps the lag at ~60 while the feed runs) widens only the
        feed-live exec-lag bound; the paused settle keeps the idle criterion."""
        out_dir, out = self._run_drain_flow("1", "100")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        self.assertEqual(len(py), 2, out)
        self.assertTrue(
            py[0].endswith(" --feed-live --feed-mempool-max 12 --max-lag 100"), py[0]
        )
        self.assertNotIn("--max-lag", py[1])
        self.assertIn("exec lag <= 100", out)

    def test_feed_drain_keeps_the_feed_live_then_pauses_and_settles(self):
        out_dir, out = self._run_drain_flow("1")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        self.assertEqual(len(py), 2, out)
        # live through the drain, judged by the feed-live criterion; the
        # mempool bound = 2 rounds x 3 validators x ceil(300/256) chunks
        self.assertTrue(
            py[0].startswith("PY S /tools/health.py drain --out %s " % out_dir), py[0]
        )
        self.assertTrue(
            py[0].endswith(" --feed-live --feed-mempool-max 12 --max-lag 2"), py[0]
        )
        # paused right after it, then a legacy quiet settle before the digest
        # that also waits until every node executed the highest committed
        # height at the pause (s64: was a fixed 60 s)
        self.assertTrue(
            py[1].startswith(
                "PY T /tools/health.py drain --out %s/feed-stop-settle --timeout 600 "
                "--quiet 10 --to-height auto --urls " % out_dir
            ),
            py[1],
        )
        self.assertNotIn("--feed-live", py[1])
        self.assertTrue(os.path.isdir(os.path.join(out_dir, "feed-stop-settle")))
        self.assertIn("LOG feed-live drain:", out)
        self.assertIn("quiet_window_native_block_exec_ms", out)
        settle = [
            l
            for l in out.splitlines()
            if "feed-stop-settle" in l and l.startswith("LOG ")
        ]
        self.assertEqual(len(settle), 1, out)
        self.assertRegex(
            settle[0], r"settle drained=1 rc=0 after 12s: every node executed target height 4321"
        )
        self.assertIn(
            "quiet_window_native_block_exec_ms (oracle-only proxy) p50=50.0 p95=200.0 "
            "max=200.0 native_blocks=5 single_block_intervals=3/4",
            out,
        )
        self.assertIn("END T", out)
        self.assertNotIn("FEED_STILL_ALIVE", out)
        self.assertIn("STOPPED rc=143", out)
        # the existing exit paths still stop it (TERM + CONT): unchanged
        self.assertIn(
            'kill -TERM "$pid" 2>/dev/null; kill -CONT "$pid" 2>/dev/null', self.src
        )

    def test_feed_stop_settle_max_reaches_the_settle_and_a_miss_is_logged(self):
        """FEED_STOP_SETTLE_MAX_S bounds the paused settle; a settle that does
        not reach its height target (health.py rc 3) is logged as such and the
        cell carries on (the digest quiescence check decides, as before)."""
        out_dir, out = self._run_drain_flow(
            "1",
            settle_max="1500",
            settle_rc=3,
            settle={
                "drained": False,
                "elapsed_s": 1500.2,
                "reason": "height target 9000 not reached (val1 at 8800)",
                "target_height": 9000,
                "target_reached": False,
                "exec_heights": {"val0": 9001, "val1": 8800, "val2": 9001},
            },
        )
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        self.assertIn(" --timeout 1500 --quiet 10 --to-height auto ", py[1])
        settle = [l for l in out.splitlines() if "feed-stop-settle" in l and l.startswith("LOG ")]
        self.assertEqual(len(settle), 1, out)
        self.assertIn("settle drained=0 rc=3 after 1500s", settle[0])
        self.assertIn(
            "WARNING: target height 9000 NOT reached within FEED_STOP_SETTLE_MAX_S=1500 s "
            "(exec heights val0=9001 val1=8800 val2=9001)",
            settle[0],
        )
        self.assertIn("the digest quiescence check decides", settle[0])
        self.assertIn("END T", out)

    def test_attest_drain_tolerates_attestations(self):
        """9g-hn smoke 2026-10-21: STATE_HASH_ATTEST=1 keeps the native counter
        and mempool moving forever (one AttestStateHash per validator per
        checkpoint on idle blocks): the drain runs with --attest-live and the
        attestations' mempool footprint (2 checkpoints x 3 validators)."""
        out_dir, out = self._run_drain_flow("0", attest="1", feed="0")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        urls = " ".join("http://127.0.0.1:%d/metrics" % p for p in (9161, 9162, 9163))
        self.assertEqual(
            py,
            ["PY S /tools/health.py drain --out %s --timeout 780 --quiet 10 --urls %s "
             "--attest-live --attest-mempool-max 6 --attest-rpc %s" % (out_dir, urls, ATTEST_RPCS)],
        )
        self.assertIn("LOG attest-live drain: --attest-live --attest-mempool-max 6 --attest-rpc "
                      + ATTEST_RPCS, out)

    def test_attest_with_feed_drain_reaches_both_drains(self):
        out_dir, out = self._run_drain_flow("1", attest="1")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        self.assertEqual(len(py), 2, out)
        self.assertTrue(py[0].endswith(
            " --feed-live --feed-mempool-max 12 --max-lag 2 --attest-live --attest-mempool-max 6"
            " --attest-rpc " + ATTEST_RPCS), py[0])
        # the paused settle: the feed is stopped, the attestations are not
        self.assertTrue(py[1].endswith(" --attest-live --attest-mempool-max 6 --attest-rpc "
                                       + ATTEST_RPCS), py[1])
        self.assertIn(" --to-height auto ", py[1])
        self.assertNotIn("--feed-live", py[1])

    def test_attest_off_drain_args_unchanged(self):
        for mode in ("0", "1"):
            _, out = self._run_drain_flow(mode)
            self.assertNotIn("--attest-live", out)

    def test_feed_stop_settle_max_env(self):
        self.assertIn("FEED_STOP_SETTLE_MAX_S=${FEED_STOP_SETTLE_MAX_S:-600}", self.src)
        doc = self.src.index("#   FEED_STOP_SETTLE_MAX_S=N")
        self.assertLess(self.src.index("#   ORACLE_FEED_DRAIN=1"), doc)
        self.assertLess(doc, self.src.index("set -uo pipefail"))
        self.assertNotIn("--timeout 60 ", self.src)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            self.assertIn('"FEED_STOP_SETTLE_MAX_S",', f.read())
        tmp = tempfile.mkdtemp(prefix="settle-max-pre-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        tgt = os.path.join(tmp, "release")
        os.makedirs(tgt)
        for b in ("torus-node", "bench-throughput"):
            p = os.path.join(tgt, b)
            with open(p, "w") as f:
                f.write("#!/bin/sh\nexit 0\n")
            os.chmod(p, 0o755)
        script, wt = sandbox_repo(tmp)
        drain_on = dict(ORACLE_FEED="1", ORACLE_FEED_DRAIN="1")
        for env, msg in (
            (dict(drain_on, FEED_STOP_SETTLE_MAX_S="x"), "FEED_STOP_SETTLE_MAX_S must be a positive integer"),
            (dict(drain_on, FEED_STOP_SETTLE_MAX_S="0"), "FEED_STOP_SETTLE_MAX_S must be a positive integer"),
            (dict(ORACLE_FEED="1", FEED_STOP_SETTLE_MAX_S="900"), "FEED_STOP_SETTLE_MAX_S=900 needs ORACLE_FEED_DRAIN=1"),
        ):
            r = subprocess.run(
                [script, wt, "settle-max-pre-x"],
                capture_output=True,
                text=True,
                timeout=30,
                env=dict(
                    os.environ,
                    TARGET_DIR=tmp,
                    RESULTS_ROOT=tmp,
                    DATA_ROOT=tmp,
                    BENCH_ALLOW_UNDETACHED="1",
                    **env,
                ),
            )
            self.assertEqual(r.returncode, 2, (env, r.stderr))
            self.assertIn(msg, r.stderr)
            self.assertFalse(os.path.exists(os.path.join(tmp, "settle-max-pre-x")))

    def test_bad_walk_env_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="oracle-walk-pre-")
        try:
            tgt = os.path.join(tmp, "release")
            os.makedirs(tgt)
            for b in ("torus-node", "bench-throughput"):
                p = os.path.join(tgt, b)
                with open(p, "w") as f:
                    # an oracle-feed without --walk-bp (an older binary)
                    f.write("#!/bin/sh\necho 'Usage: oracle-feed --price <PRICE>'\n")
                os.chmod(p, 0o755)
            script, wt = sandbox_repo(tmp)
            for env, rc, msg in (
                (
                    dict(ORACLE_FEED="1", ORACLE_WALK_BP="x"),
                    2,
                    "ORACLE_WALK_BP must be",
                ),
                (
                    dict(ORACLE_FEED="1", ORACLE_WALK_BP="1250"),
                    2,
                    "ORACLE_WALK_BP must be",
                ),
                (dict(ORACLE_WALK_BP="10"), 2, "needs ORACLE_FEED=1"),
                (dict(ORACLE_FEED="1", ORACLE_WALK_BP="10"), 1, "has no --walk-bp"),
            ):
                r = subprocess.run(
                    [script, wt, "oracle-walk-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        **env,
                    ),
                )
                self.assertEqual(r.returncode, rc, (env, r.stderr))
                self.assertIn(msg, r.stderr)
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, "oracle-walk-pre-x")),
                    "must fail before any launch",
                )
        finally:
            shutil.rmtree(tmp)


    def test_separate_signer_feed_needs_the_signer_key_file(self):
        """s134: a bench binary whose oracle-feed has --signer (separate oracle
        signers by default) needs devnet/wsl/bench-oracle-signer-keys.json next
        to the validator key file; an older binary (no --signer) does not."""
        for help_text, with_file, rc in (
            ("Usage: oracle-feed --price <PRICE> --signer <SIGNER>", False, 1),
            ("Usage: oracle-feed --price <PRICE>", False, None),
            ("Usage: oracle-feed --price <PRICE> --signer <SIGNER>", True, None),
        ):
            tmp = tempfile.mkdtemp(prefix="oracle-signer-pre-")
            try:
                fake_target(tmp, help_text)
                script, wt = sandbox_repo(tmp)
                if with_file:
                    shutil.copy2(
                        os.path.join(REPO, "devnet", "wsl", "bench-oracle-signer-keys.json"),
                        os.path.join(wt, "devnet", "wsl"),
                    )
                r = subprocess.run(
                    [script, wt, "oracle-signer-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        ORACLE_FEED="1",
                    ),
                )
                if rc is not None:
                    self.assertEqual(r.returncode, rc, r.stderr)
                    self.assertIn("bench-oracle-signer-keys.json is missing", r.stderr)
                    self.assertFalse(os.path.exists(os.path.join(tmp, "oracle-signer-pre-x")))
                else:
                    self.assertNotIn("bench-oracle-signer-keys.json", r.stderr)
            finally:
                shutil.rmtree(tmp)


LIQUIDATOR_VAULT = "0x746f7275732d6c697175696461746f722d766c74"
# torus_core::liquidation::ADL_ESCROW_LONG / ADL_ESCROW_SHORT.
ADL_ESCROW_LONG = "0x746f7275732d61646c2d657363726f772d6c6e67"
ADL_ESCROW_SHORT = "0x746f7275732d61646c2d657363726f772d736874"
PROTOCOL_ACCOUNTS = [LIQUIDATOR_VAULT, ADL_ESCROW_LONG, ADL_ESCROW_SHORT]


def sandbox_repo(tmp):
    """A throwaway harness tree for running run-cell.sh: (run-cell.sh, worktree).

    run-cell.sh resolves MAINREPO from its own path, so a copy started from
    THIS checkout writes testnet/genesis-weighted-full.json (through
    gen-3val-genesis.sh) and target/release/torus-node here as soon as a
    preflight lets it through. s17: a red test-first run did, with a stub
    bench, and the stale file (61 balances, address 'oracle-feed') broke a
    later cell's genesis. Preflight tests therefore run a copy in `tmp`."""
    repo = os.path.join(tmp, "repo")
    tools = os.path.join(repo, "tools", "matched-bench")
    wsl = os.path.join(repo, "devnet", "wsl")
    os.makedirs(tools)
    os.makedirs(wsl)
    shutil.copy2(RUN_CELL_SH, tools)
    shutil.copy2(os.path.join(HERE, "log-scans.sh"), tools)  # run-cell.sh sources it at preflight
    shutil.copy2(os.path.join(REPO, "devnet", "wsl", "bench-validator-keys.json"), wsl)
    for p in (
        os.path.join(tools, "digest-node.sh"),
        os.path.join(wsl, "launch-3val.sh"),
    ):
        with open(p, "w") as f:
            f.write("#!/bin/sh\nexit 0\n")
        os.chmod(p, 0o755)
    return os.path.join(tools, "run-cell.sh"), repo


def fake_target(tmp, help_text="Usage: oracle-feed --price <PRICE> --walk-bp <N>"):
    """TARGET_DIR with stub torus-node / bench-throughput binaries."""
    tgt = os.path.join(tmp, "release")
    os.makedirs(tgt)
    for b in ("torus-node", "bench-throughput"):
        p = os.path.join(tgt, b)
        with open(p, "w") as f:
            f.write("#!/bin/sh\necho '%s'\n" % help_text)
        os.chmod(p, 0o755)


def _stat(path):
    try:
        st = os.stat(path)
        return (st.st_size, st.st_mtime_ns)
    except FileNotFoundError:
        return None


class StaleArtifactGuardTest(unittest.TestCase):
    """run-cell.sh refuses stale artifacts before it generates a genesis:
    an untracked testnet/genesis-weighted-full.json that gen-3val-genesis.sh
    would reuse as-is (s17: a stub-bench test left one with 61 balances and
    the address 'oracle-feed'), and a node / bench binary whose md5 is not
    the one recorded for the cell (EXPECT_NODE_MD5 / EXPECT_BENCH_MD5; s17: a
    reflink-seeded target dir kept a stale bench). Runs a sandbox copy whose
    gen-3val-genesis.sh only records that it was called."""

    def setUp(self):
        self.real_full = _stat(WEIGHTED_FULL)
        self.tmp = tempfile.mkdtemp(prefix="stale-guard-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        fake_target(
            self.tmp, "Usage: oracle-feed --price <PRICE> --walk-bp <N> --shock-bp <BP>"
        )
        self.script, self.wt = sandbox_repo(self.tmp)
        self.called = os.path.join(self.tmp, "generator-called")
        gen = os.path.join(self.wt, "devnet", "wsl", "gen-3val-genesis.sh")
        with open(gen, "w") as f:
            f.write("#!/bin/sh\ntouch '%s'\n" % self.called)
        os.chmod(gen, 0o755)
        self.full = os.path.join(self.wt, "testnet", "genesis-weighted-full.json")
        os.makedirs(os.path.dirname(self.full))
        # pgrep finds nothing: a cargo build or bench elsewhere on the host
        # must not end the sandbox cell before the step under test.
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        with open(os.path.join(self.bin, "pgrep"), "w") as f:
            f.write("#!/bin/sh\nexit 1\n")
        os.chmod(os.path.join(self.bin, "pgrep"), 0o755)

    def tearDown(self):
        self.assertEqual(
            _stat(WEIGHTED_FULL), self.real_full, "the real worktree was touched"
        )

    def write_full(self, rows):
        base = [
            {
                "address": "0x%040x" % i,
                "available": "1.0",
                "note": "bench sender %d" % i,
            }
            for i in range(3)
        ]
        with open(self.full, "w") as f:
            json.dump({"native_balances": base + rows, "accounts": []}, f)

    @staticmethod
    def bulk(n):
        return [
            {
                "address": "0x%040x" % (60 + i),
                "available": "100000000.0",
                "note": "bulk-test %d" % (60 + i),
            }
            for i in range(n)
        ]

    def run_cell(self, **env):
        e = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("LIQ_", "ORACLE_", "EXPECT_"))
            and k not in ("FORCE", "SENDERS")
        }
        e.update(
            PATH=self.bin + os.pathsep + os.environ["PATH"],
            TARGET_DIR=self.tmp,
            RESULTS_ROOT=os.path.join(self.tmp, "results"),
            DATA_ROOT=os.path.join(self.tmp, "data"),
            BENCH_ALLOW_UNDETACHED="1",
            SENDERS="5",
            **env,
        )
        r = subprocess.run(
            [self.script, self.wt, "guard-x"],
            capture_output=True,
            text=True,
            timeout=60,
            env=e,
        )
        try:
            with open(os.path.join(self.tmp, "results", "guard-x", "run.log")) as f:
                log = f.read()
        except OSError:
            log = ""
        return r, log

    def test_stale_weighted_full_genesis_fails_before_the_generator(self):
        # the s17 file: the stub bench's help line became one "bulk" row
        self.write_full(
            [
                {
                    "address": "oracle-feed",
                    "available": "100000000.0",
                    "note": "bulk-test Usage:",
                }
            ]
        )
        r, log = self.run_cell()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("FATAL", log)
        self.assertIn(self.full, log)
        self.assertIn("oracle-feed", log)
        self.assertFalse(os.path.exists(self.called), "generator must not run")

    def test_weighted_full_with_too_few_bulk_senders_fails(self):
        self.write_full(self.bulk(3))
        r, log = self.run_cell()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("3 bulk-test rows < SENDERS=5", log)
        self.assertFalse(os.path.exists(self.called))

    def test_unparsable_weighted_full_fails(self):
        with open(self.full, "w") as f:
            f.write("{truncated")
        r, log = self.run_cell()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(self.full, log)
        self.assertFalse(os.path.exists(self.called))

    def test_good_or_absent_weighted_full_reaches_the_generator(self):
        for rows in (self.bulk(5), None):
            with self.subTest(rows=rows is not None):
                if rows is None:
                    os.remove(self.full)
                else:
                    self.write_full(rows)
                r, log = self.run_cell(OVERWRITE="1")
                self.assertTrue(os.path.exists(self.called), r.stdout + r.stderr)
                self.assertNotIn("genesis-weighted-full.json is stale", log)
                os.remove(self.called)

    def test_force_regenerates_instead_of_failing(self):
        self.write_full([{"address": "oracle-feed", "note": "bulk-test Usage:"}])
        r, _ = self.run_cell(FORCE="1")
        self.assertTrue(os.path.exists(self.called), r.stdout + r.stderr)

    def md5(self, name):
        with open(os.path.join(self.tmp, "release", name), "rb") as f:
            return hashlib.md5(f.read()).hexdigest()

    def test_expected_md5_mismatch_refuses_and_prints_both(self):
        self.write_full(self.bulk(5))
        for var, name in (
            ("EXPECT_BENCH_MD5", "bench-throughput"),
            ("EXPECT_NODE_MD5", "torus-node"),
        ):
            with self.subTest(var=var):
                r, log = self.run_cell(OVERWRITE="1", **{var: "0123abcd"})
                self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
                fatal = [l for l in log.splitlines() if "FATAL" in l]
                self.assertEqual(len(fatal), 1, log)
                self.assertIn(name, fatal[0])
                self.assertIn(self.md5(name), fatal[0])
                self.assertIn("0123abcd", fatal[0])
                self.assertFalse(
                    os.path.exists(self.called), "must stop before the genesis"
                )

    def test_expected_md5_match_proceeds(self):
        self.write_full(self.bulk(5))
        r, log = self.run_cell(
            EXPECT_BENCH_MD5=self.md5("bench-throughput")[:8],
            EXPECT_NODE_MD5=self.md5("torus-node"),
        )
        self.assertTrue(os.path.exists(self.called), r.stdout + r.stderr + log)
        self.assertIn("bench-throughput md5=%s" % self.md5("bench-throughput"), log)
        self.assertIn("expected %s" % self.md5("bench-throughput")[:8], log)

    def test_bad_expected_md5_fails_preflight(self):
        for v in ("xyz", "0123abc", "0123ABCD"):
            with self.subTest(v=v):
                r, _ = self.run_cell(EXPECT_BENCH_MD5=v)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn("EXPECT_BENCH_MD5 must be", r.stderr)
                self.assertFalse(os.path.exists(os.path.join(self.tmp, "results")))

    def test_expected_md5_knobs_documented_and_allowlisted(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        head = src[: src.index("set -uo pipefail")]
        self.assertIn("#   EXPECT_NODE_MD5", head)
        self.assertIn("EXPECT_BENCH_MD5", head)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            allow = f.read()
        for k in ("EXPECT_NODE_MD5", "EXPECT_BENCH_MD5"):
            self.assertIn('"%s",' % k, allow)


class ConsensusWideColsTest(unittest.TestCase):
    """Section 71.8 option A (frozen ozarchy-hsdb copy, upstreamed s64): every
    cell samples the consensus DB's 29 DB-wide gauges
    (torus_rocksdb_consensus_*, the Rust RocksdbStatMetrics::NAMES with the
    consensus prefix) and its 3 per-CF families x 3 CFs as CSV-safe
    <family>__<cf>__consensus alias columns (sample_metrics.py)."""

    TELEMETRY = os.path.join(REPO, "crates", "torus-telemetry", "src", "lib.rs")
    CFS = ("cf_consensus_meta", "cf_native_pending", "cf_native_shards")
    FAMILIES = (
        "torus_rocksdb_l0_files",
        "torus_rocksdb_memtable_bytes",
        "torus_rocksdb_pending_compaction_bytes",
    )

    def wide_cols(self, liq=False):
        """The effective WIDE_COLS of a cell (run-cell.sh's real block in bash)."""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i = src.index('WIDE_COLS="torus_blocks_committed_total')
        end = 'WIDE_COLS="$WIDE_COLS scrape_valid"\n'
        block = src[i : src.index(end, i) + len(end)]
        script = (
            "liq_stress_on() { %s; }\n" % ("true" if liq else "false")
            + block
            + 'echo "$WIDE_COLS"\n'
        )
        r = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.split()

    def rust_names(self):
        with open(self.TELEMETRY) as f:
            src = f.read()
        i = src.index("pub const NAMES: [&'static str; 29] = [")
        names = re.findall(r'"(torus_rocksdb_[a-z0-9_]+)"', src[i : src.index("];", i)])
        self.assertEqual(len(names), 29)
        return ["torus_rocksdb_consensus_" + n[len("torus_rocksdb_") :] for n in names]

    def test_wide_cols_carry_the_consensus_db_series(self):
        cols = self.wide_cols()
        aliases = [
            "%s__%s__consensus" % (f, c) for f in self.FAMILIES for c in self.CFS
        ]
        for name in self.rust_names() + aliases:
            self.assertIn(name, cols)
        consensus = [c for c in cols if "consensus" in c]
        # 29 + 9 new, plus the two consensus-thread series main already had
        self.assertEqual(len(consensus), 40, consensus)
        self.assertEqual(len(cols), len(set(cols)), "duplicate WIDE_COLS")
        self.assertEqual(cols[-1], "scrape_valid")
        # liq cells keep them too, and every column stays CSV-safe
        self.assertTrue(set(consensus) <= set(self.wide_cols(liq=True)))
        for c in cols:
            self.assertRegex(c, r"^[A-Za-z0-9_]+$")

    def test_absent_consensus_columns_are_nan_in_the_sampler(self):
        sys.path.insert(0, HERE)
        import sample_metrics

        for name in self.wide_cols():
            want = (
                "nan"
                if "rocksdb_consensus_" in name or name.endswith("__consensus")
                else "0"
            )
            self.assertEqual(sample_metrics.absent_value(name), want, name)


class LiqStressHarnessTest(unittest.TestCase):
    """Row 76 liquidation-stress cell: LIQ_THIN thin-seeds the first N load
    senders in the genesis, ORACLE_SHOCK_BP/ROUND pass a parity-signed price
    shock to the oracle feed, and the liquidator vault joins the digest (and
    is read per node) only when either is on. Defaults leave the cell as it
    was."""

    @classmethod
    def setUpClass(cls):
        with open(RUN_CELL_SH) as f:
            cls.src = f.read()

    def fn(self, start, end_marker):
        i = self.src.index(start)
        return self.src[i : self.src.index(end_marker, i) + len(end_marker)]

    def test_defaults_docs_and_allowlist(self):
        for var, default in (
            ("LIQ_THIN", "0"),
            ("LIQ_THIN_AVAIL", "1000000.0"),
            ("ORACLE_SHOCK_BP", "0"),
            ("ORACLE_SHOCK_ROUND", "0"),
        ):
            self.assertIn(f"{var}=${{{var}:-{default}}}", self.src)
        head = self.src[: self.src.index("set -uo pipefail")]
        for doc in ("#   LIQ_THIN=N", "#   ORACLE_SHOCK_BP=S"):
            self.assertIn(doc, head)
        self.assertIn(f"LIQUIDATOR_VAULT={LIQUIDATOR_VAULT}", self.src)
        self.assertIn(f"ADL_ESCROW_LONG={ADL_ESCROW_LONG}", self.src)
        self.assertIn(f"ADL_ESCROW_SHORT={ADL_ESCROW_SHORT}", self.src)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            allow = f.read()
        for k in (
            "LIQ_THIN",
            "LIQ_THIN_AVAIL",
            "ORACLE_SHOCK_BP",
            "ORACLE_SHOCK_ROUND",
        ):
            self.assertIn(f'"{k}",', allow)
        with open(os.path.join(HERE, "resummarize.sh")) as f:
            res = f.read()
        self.assertIn('--liq-thin "$(j \'.cell.liq_thin // ""\')"', res)
        self.assertIn('--liq-thin-avail "$(j \'.cell.liq_thin_avail // ""\')"', res)

    def _genesis(self, d):
        nb = [
            {"address": "0xbase%d" % i, "available": "5.0", "note": "hardhat %d" % i}
            for i in range(3)
        ]
        nb.append({"address": "0xnonote", "available": "7.0"})
        nb += [
            {
                "address": "0x%040x" % i,
                "available": "100000000.0",
                "note": "bulk-test %d" % i,
            }
            for i in range(58, 70)
        ]
        g = {
            "markets": [{"market_id": 1}],
            "native_balances": nb,
            "accounts": [{"note": "bulk-test 60", "balance": "1"}],
        }
        p = os.path.join(d, "genesis.json")
        with open(p, "w") as f:
            json.dump(g, f)
        return p, g

    def test_liq_thin_patch_sets_available_on_exactly_the_first_n_load_senders(self):
        fn = self.fn("liq_thin_patch() {", "\n}\n")
        tmp = tempfile.mkdtemp(prefix="liq-thin-")
        try:
            p, before = self._genesis(tmp)
            r = subprocess.run(
                ["bash", "-c", fn + 'liq_thin_patch "$1" 4 1000000.0', "x", p],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual((r.returncode, r.stdout.strip()), (0, "4"), r.stderr)
            with open(p) as f:
                after = json.load(f)
            thin = {"bulk-test %d" % i for i in range(60, 64)}
            for b, a in zip(before["native_balances"], after["native_balances"]):
                want = dict(b, available="1000000.0") if b.get("note") in thin else b
                self.assertEqual(a, want)
            self.assertEqual(
                after["accounts"], before["accounts"], "EVM balances untouched"
            )
            self.assertEqual(after["markets"], before["markets"])
            # More thin senders than the genesis funds: FATAL, genesis untouched.
            with open(p) as f:
                ok = f.read()
            r = subprocess.run(
                ["bash", "-c", fn + 'liq_thin_patch "$1" 11 1.0', "x", p],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertNotEqual(r.returncode, 0)
            with open(p) as f:
                self.assertEqual(f.read(), ok)
            self.assertEqual(
                sorted(os.listdir(tmp)), ["genesis.json"], "no temp file left"
            )
        finally:
            shutil.rmtree(tmp)

    def test_liq_thin_patch_runs_before_the_genesis_md5_and_only_when_set(self):
        call = self.src.index('liq_thin_patch "$GENESIS" "$LIQ_THIN" "$LIQ_THIN_AVAIL"')
        self.assertLess(
            self.src.index('"$MAINREPO/devnet/wsl/gen-3val-genesis.sh"'), call
        )
        self.assertLess(call, self.src.index('GEN_MD5=$(md5sum "$GENESIS"'))
        gate = self.src.rindex('if [ "$LIQ_THIN" -gt 0 ]; then', 0, call)
        self.assertNotIn("\nfi\n", self.src[gate:call])

    def test_shock_reaches_the_feed_only_when_set(self):
        block = self.fn("    ORACLE_CMD=(", '\n    log "oracle feed: ${ORACLE_CMD[*]}"')
        for bp, rnd, want in (
            ("0", "0", []),
            ("400", "30", ["--shock-bp", "400", "--shock-round", "30"]),
        ):
            script = (
                "BENCH=bt RPCS=(r0 r1) ORACLE_KEYS=k MARKETS=3 ORACLE_PRICE=30000 "
                "ORACLE_INTERVAL_MS=2000 OUT=o ORACLE_WALK_BP=10 ORACLE_SHOCK_BP=%s ORACLE_SHOCK_ROUND=%s\n"
                % (bp, rnd)
                + block.rsplit("\n", 1)[0]
                + '\nprintf "%s\\n" "${ORACLE_CMD[@]}"\n'
            )
            r = subprocess.run(
                ["bash", "-c", script], capture_output=True, text=True, timeout=30
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            argv = r.stdout.split("\n")[:-1]
            self.assertEqual(
                argv[-len(want) :] if want else argv[-2:],
                want or ["--walk-bp", "10"],
                argv,
            )
            self.assertEqual(argv.count("--shock-bp"), len(want) // 4, argv)
        self.assertIn("'shock_bp': int(shock_bp)", self.src)
        self.assertIn("'shock_round': int(shock_round)", self.src)

    def test_bad_liq_env_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="liq-pre-")
        try:
            fake_target(tmp)  # an oracle-feed without --shock-bp (an older binary)
            script, wt = sandbox_repo(tmp)
            for env, rc, msg in (
                (dict(LIQ_THIN="x"), 2, "LIQ_THIN must be"),
                (dict(LIQ_THIN="-1"), 2, "LIQ_THIN must be"),
                (dict(LIQ_THIN="6", SENDERS="5"), 2, "must not exceed SENDERS"),
                (dict(LIQ_THIN="5", LIQ_THIN_AVAIL="0"), 2, "LIQ_THIN_AVAIL must be"),
                (dict(LIQ_THIN="5", LIQ_THIN_AVAIL="abc"), 2, "LIQ_THIN_AVAIL must be"),
                (
                    dict(LIQ_THIN="5", LIQ_THIN_AVAIL="1.123456789"),
                    2,
                    "LIQ_THIN_AVAIL must be",
                ),
                (
                    dict(ORACLE_FEED="1", ORACLE_SHOCK_BP="x"),
                    2,
                    "ORACLE_SHOCK_BP must be",
                ),
                (
                    dict(ORACLE_FEED="1", ORACLE_SHOCK_BP="10000"),
                    2,
                    "ORACLE_SHOCK_BP must be",
                ),
                (
                    dict(
                        ORACLE_FEED="1", ORACLE_SHOCK_BP="400", ORACLE_SHOCK_ROUND="r"
                    ),
                    2,
                    "ORACLE_SHOCK_ROUND must be",
                ),
                (
                    dict(ORACLE_FEED="1", ORACLE_SHOCK_ROUND="30"),
                    2,
                    "ORACLE_SHOCK_ROUND needs ORACLE_SHOCK_BP",
                ),
                (dict(ORACLE_SHOCK_BP="400"), 2, "needs ORACLE_FEED=1"),
                (
                    dict(
                        ORACLE_FEED="1", ORACLE_SHOCK_BP="400", ORACLE_SHOCK_ROUND="30"
                    ),
                    1,
                    "has no --shock-bp",
                ),
            ):
                r = subprocess.run(
                    [script, wt, "liq-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        **env,
                    ),
                )
                self.assertEqual(r.returncode, rc, (env, r.stderr))
                self.assertIn(msg, r.stderr, env)
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, "liq-pre-x")),
                    "must fail before any launch",
                )
        finally:
            shutil.rmtree(tmp)

    def _run(self, script, **env):
        base = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("LIQ_", "ORACLE_SHOCK"))
        }
        return subprocess.run(
            ["bash", "-c", script],
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(base, **env),
        )

    def test_protocol_addresses_match_the_rust_constants(self):
        """The run-cell.sh hex addresses are the 20-byte ASCII names in
        torus_core::liquidation (a renamed constant must fail here, not
        silently digest an empty account)."""
        import re

        with open(
            os.path.join(REPO, "crates", "torus-core", "src", "liquidation.rs")
        ) as f:
            rs = f.read()
        for name in ("LIQUIDATOR_VAULT", "ADL_ESCROW_LONG", "ADL_ESCROW_SHORT"):
            m = re.search(
                r"pub const %s: Address = Address::new\(\*b\"([^\"]{20})\"\);" % name,
                rs,
            )
            self.assertIsNotNone(m, name)
            want = "0x" + m.group(1).encode().hex()
            self.assertIn(f"\n{name}={want}\n", self.src, name)
        self.assertIn(
            "*a == ADL_ESCROW_LONG || *a == ADL_ESCROW_SHORT", rs,
            "is_adl_escrow no longer names exactly the two escrows: update the digest",
        )

    def test_vault_joins_the_digest_only_when_enabled(self):
        """Liquidation stress: the vault and both ADL escrows join the
        balance accounts AND the position accounts (torus_getPosition per
        market); default cells digest exactly the 50 load senders, no
        positions file."""
        on = self.fn("liq_stress_on() {", "\n}\n")
        block = self.fn(
            '"$BENCH" gen-accounts --offset 60 --count 50',
            "DIG_POS_ACCTS=",
        )
        block = block + self.src[
            self.src.index("DIG_POS_ACCTS=", self.src.index(block)) + len("DIG_POS_ACCTS=") :
        ].split("\n", 1)[0] + "\n"
        tmp = tempfile.mkdtemp(prefix="liq-dig-")
        try:
            bench = os.path.join(tmp, "bt")
            with open(bench, "w") as f:
                f.write("#!/bin/sh\necho '60 0xaaa'\necho '61 0xbbb'\n")
            os.chmod(bench, 0o755)
            script = (
                'log() { echo "$*" >&2; }\nLIQUIDATOR_VAULT=%s\nADL_ESCROW_LONG=%s\n'
                "ADL_ESCROW_SHORT=%s\nBENCH=%s\nOUT=%s\n"
                % (LIQUIDATOR_VAULT, ADL_ESCROW_LONG, ADL_ESCROW_SHORT, bench, tmp)
                + on
                + block
                + '\necho "$DIG_ACCTS $DIG_POS_ACCTS ${#DIG_POS[@]} ${DIG_POS[*]-}"\n'
            )
            posf = os.path.join(tmp, "digest-position-accounts.txt")
            for env, accts, pos in (
                (dict(LIQ_THIN="0", ORACLE_SHOCK_BP="0"), ["0xaaa", "0xbbb"], None),
                (
                    dict(LIQ_THIN="5", ORACLE_SHOCK_BP="0"),
                    ["0xaaa", "0xbbb"] + PROTOCOL_ACCOUNTS,
                    PROTOCOL_ACCOUNTS,
                ),
                (
                    dict(LIQ_THIN="0", ORACLE_SHOCK_BP="400"),
                    ["0xaaa", "0xbbb"] + PROTOCOL_ACCOUNTS,
                    PROTOCOL_ACCOUNTS,
                ),
            ):
                if os.path.exists(posf):
                    os.unlink(posf)
                r = self._run(script, **env)
                want = (
                    "%d 0 0" % len(accts)
                    if pos is None
                    else "%d %d 1 %s" % (len(accts), len(pos), posf)
                )
                self.assertEqual(
                    (r.returncode, r.stdout.strip()), (0, want), (env, r.stderr)
                )
                with open(os.path.join(tmp, "digest-accounts.txt")) as f:
                    self.assertEqual(f.read().split(), accts, env)
                if pos is None:
                    self.assertFalse(os.path.exists(posf), env)
                else:
                    with open(posf) as f:
                        self.assertEqual(f.read().split(), pos, env)
        finally:
            shutil.rmtree(tmp)

    def test_digest_nodes_get_the_positions_file_only_when_enabled(self):
        loop = self.fn("DIG_PIDS=()\n", 'wait "${DIG_PIDS[@]}"')
        tmp = tempfile.mkdtemp(prefix="liq-dign-")
        try:
            stub = os.path.join(tmp, "digest-node.sh")
            with open(stub, "w") as f:
                f.write('#!/bin/sh\necho "$# ${7-}"\n')
            os.chmod(stub, 0o755)
            for pos, want in (
                ("DIG_POS=()", "6"),
                ('DIG_POS=("%s/p.txt")' % tmp, "7 %s/p.txt" % tmp),
            ):
                script = (
                    "set -u\nTOOLS_DIR=%s\nOUT=%s\nRPCS=(u0 u1 u2)\nLIVE_IDX=(0 1 2)\nMARKETS=3\n"
                    "DIGEST_PAR=2\nRPC_TIMEOUT=5\n%s\n" % (tmp, tmp, pos)
                    + loop
                    + "\n"
                )
                r = self._run(script)
                self.assertEqual(r.returncode, 0, r.stderr)
                for i in range(3):
                    with open(os.path.join(tmp, "digest-val%d.out" % i)) as f:
                        self.assertEqual(f.read().strip(), want, pos)
        finally:
            shutil.rmtree(tmp)

    def test_vault_is_read_per_node_only_when_enabled(self):
        on = self.fn("liq_stress_on() {", "\n}\n")
        block = self.fn(
            'if liq_stress_on; then\n    for i in "${LIVE_IDX[@]}"; do\n        rpc ', "\nfi\n"
        )
        self.assertIn("torus_getLiquidatorVault", block)
        # After the digests and their quiescence snapshot.
        at = self.src.index(block)
        self.assertLess(self.src.index('wait "${DIG_PIDS[@]}"'), at)
        self.assertLess(self.src.index("if Q_AFTER=$(funnel_snapshot)"), at)
        tmp = tempfile.mkdtemp(prefix="liq-vault-")
        try:
            script = (
                # LIVE_IDX: every node (run-cell's default; KILL_NO_RESTART leaves the survivors)
                'log() { echo "$*" >&2; }\nRPCS=(u0 u1 u2)\nLIVE_IDX=(0 1 2)\nOUT=%s\n'
                'rpc() { [ "$1" = u1 ] && return 7; echo \'{"result":{"address":"0x74","deficit":"5.0","openPositions":3}}\'; }\n'
                % tmp
                + on
                + block
            )
            r = self._run(script, LIQ_THIN="0", ORACLE_SHOCK_BP="0")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(os.listdir(tmp), [])
            r = self._run(script, LIQ_THIN="3", ORACLE_SHOCK_BP="0")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(
                sorted(os.listdir(tmp)),
                ["vault-val0.json", "vault-val1.json", "vault-val2.json"],
            )
            with open(os.path.join(tmp, "vault-val0.json")) as f:
                self.assertEqual(
                    json.load(f),
                    {"address": "0x74", "deficit": "5.0", "openPositions": 3},
                )
            with open(os.path.join(tmp, "vault-val1.json")) as f:
                self.assertEqual(json.load(f), "RPC_ERR")
        finally:
            shutil.rmtree(tmp)

    def test_liquidation_metrics_are_sampled_only_when_enabled(self):
        on = self.fn("liq_stress_on() {", "\n}\n")
        i = self.src.index("if liq_stress_on; then\n    WIDE_COLS=")
        block = self.src[i : self.src.index("\nfi\n", i) + 4]
        self.assertLess(i, self.src.index('WIDE_COLS="$WIDE_COLS scrape_valid"'))
        script = "WIDE_COLS='a b'\n" + on + block + 'echo "$WIDE_COLS"\n'
        r = self._run(script, LIQ_THIN="0", ORACLE_SHOCK_BP="0")
        self.assertEqual(r.stdout.strip(), "a b", r.stderr)
        r = self._run(script, LIQ_THIN="0", ORACLE_SHOCK_BP="400")
        self.assertEqual(
            r.stdout.strip(),
            "a b torus_liquidations_triggered_total torus_liquidator_vault_deficit"
            " torus_liquidation_step_seconds_sum torus_liquidation_step_seconds_count"
            " torus_liquidations_stage1_total torus_liquidations_backstop_total"
            " torus_liquidations_adl_total torus_liquidation_scanned_total"
            " torus_liquidation_acted_total torus_liquidation_pending"
            " torus_liquidation_deferred"
            " torus_exec_post_engine_tail_seconds_count"
            " torus_liquidation_adl_queue torus_liquidation_adl_queue_deficit",
            r.stderr,
        )

    # Codex P2: the summarizer of an older worktree (TOOLS_FROM_WORKTREE=1, e.g.
    # main 46bb3a40 or the Phase 4 build) rejects --liq-thin/--liq-thin-avail as
    # unrecognized, so passing them unconditionally lost every summary.
    OLD_SUMMARIZER = (
        "import argparse\nap = argparse.ArgumentParser()\n"
        "for a in ['out', 'label', 'in-flight-tail']:\n"
        "    ap.add_argument('--' + a, default='')\nap.parse_args()\n"
    )

    def _liq_args(self, tools_dir, **env):
        on = self.fn("liq_stress_on() {", "\n}\n")
        i = self.src.index("summarizer_has_liq_opts() {")
        j = self.src.index("SUMMARIZE_LIQ_ARGS=()", i)
        block = self.src[i : self.src.index("\nfi\n", j) + 4]
        script = (
            "TOOLS_DIR=%s\nTOOLS_FROM_WORKTREE=1\n" % tools_dir
            + on
            + block
            + 'printf "<%s>\\n" "${SUMMARIZE_LIQ_ARGS[@]}"\n'
        )
        return self._run(script, **env)

    def test_summarizer_liq_options_only_when_enabled_and_supported(self):
        tmp = tempfile.mkdtemp(prefix="liq-sumopt-")
        try:
            old = os.path.join(tmp, "old")
            broken = os.path.join(tmp, "broken")
            for d, body in (
                (old, self.OLD_SUMMARIZER),
                (broken, "import sys\nsys.exit(1)\n"),
            ):
                os.makedirs(d)
                with open(os.path.join(d, "summarize.py"), "w") as f:
                    f.write(body)
            off = dict(LIQ_THIN="0", LIQ_THIN_AVAIL="1000000.0", ORACLE_SHOCK_BP="0")
            thin = dict(LIQ_THIN="5", LIQ_THIN_AVAIL="2.5", ORACLE_SHOCK_BP="0")
            shock = dict(
                LIQ_THIN="0", LIQ_THIN_AVAIL="1000000.0", ORACLE_SHOCK_BP="400"
            )
            # Off: nothing passed, whatever the summarizer (exactly as on main).
            for d in (HERE, old, broken):
                r = self._liq_args(d, **off)
                self.assertEqual((r.returncode, r.stdout), (0, "<>\n"), (d, r.stderr))
            # On + a summarizer that knows the options: both passed.
            r = self._liq_args(HERE, **thin)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(
                r.stdout.split(), ["<--liq-thin>", "<5>", "<--liq-thin-avail>", "<2.5>"]
            )
            r = self._liq_args(HERE, **shock)
            self.assertEqual(
                r.stdout.split(),
                ["<--liq-thin>", "<0>", "<--liq-thin-avail>", "<1000000.0>"],
                r.stderr,
            )
            # On + an older (or broken) summarizer: FATAL, rc 1, names the fix.
            for d in (old, broken):
                for env in (thin, shock):
                    r = self._liq_args(d, **env)
                    self.assertEqual(r.returncode, 1, (d, env, r.stdout, r.stderr))
                    self.assertIn("FATAL", r.stderr)
                    self.assertIn(os.path.join(d, "summarize.py"), r.stderr)
                    self.assertIn("--liq-thin", r.stderr)
                    self.assertIn("TOOLS_FROM_WORKTREE=0", r.stderr)
        finally:
            shutil.rmtree(tmp)

    def test_summarizer_liq_options_checked_in_preflight(self):
        # Before the results dir exists and before anything is launched.
        at = self.src.index("SUMMARIZE_LIQ_ARGS=()")
        self.assertLess(self.src.index("liq_stress_on() {"), at)
        self.assertLess(at, self.src.index('mkdir -p "$OUT"'))
        # The summarizer call passes them only through the gated array.
        call = self.fn(
            'python3 "$TOOLS_DIR/summarize.py" \\', '| tee -a "$OUT/run.log"'
        )
        self.assertNotIn("--liq-thin", call)
        self.assertIn('"${SUMMARIZE_LIQ_ARGS[@]}"', call)

    def test_stress_cell_with_an_older_worktree_summarizer_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="liq-oldsum-")
        try:
            fake_target(
                tmp, "Usage: oracle-feed --price <PRICE> --walk-bp <N> --shock-bp <BP>"
            )
            script, wt = sandbox_repo(tmp)
            # The sandbox worktree IS the repo: its summarize.py is the one that
            # TOOLS_FROM_WORKTREE=1 selects.
            with open(
                os.path.join(wt, "tools", "matched-bench", "summarize.py"), "w"
            ) as f:
                f.write(self.OLD_SUMMARIZER)
            base = {
                k: v
                for k, v in os.environ.items()
                if not k.startswith(("LIQ_", "ORACLE_", "TOOLS_"))
            }
            for env in (
                dict(LIQ_THIN="5", SENDERS="5"),
                dict(ORACLE_FEED="1", ORACLE_SHOCK_BP="400", ORACLE_SHOCK_ROUND="30"),
            ):
                r = subprocess.run(
                    [script, wt, "liq-oldsum-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        base,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        **env,
                    ),
                )
                self.assertEqual(r.returncode, 1, (env, r.stdout, r.stderr))
                self.assertIn("TOOLS_FROM_WORKTREE=0", r.stderr, env)
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, "liq-oldsum-x")),
                    "must fail before any launch",
                )
        finally:
            shutil.rmtree(tmp)

    # Codex P2 (62898e32): under TOOLS_FROM_WORKTREE=1 an older worktree's
    # digest-node.sh silently ignores the 7th argument (the position accounts),
    # so a stress cell would report agreement without having digested a single
    # protocol position. Pre-flight therefore also asks the selected digest
    # script for its capabilities (`digest-node.sh --capabilities` must list
    # `position-accounts`); an older one only prints its usage and exits 2.
    NEW_SUMMARIZER = (
        "import argparse\nap = argparse.ArgumentParser()\n"
        "for a in ['out', 'label', 'in-flight-tail', 'liq-thin', 'liq-thin-avail']:\n"
        "    ap.add_argument('--' + a, default='')\nap.parse_args()\n"
    )

    def _tools(self, root, name, digest):
        """<root>/<name>/{summarize.py (new), digest-node.sh} -> the dir and
        the marker file the digest stub touches whenever it is invoked."""
        d = os.path.join(root, name)
        os.makedirs(d)
        with open(os.path.join(d, "summarize.py"), "w") as f:
            f.write(self.NEW_SUMMARIZER)
        marker = os.path.join(d, "digest-called")
        dig = os.path.join(d, "digest-node.sh")
        if digest == "new":
            shutil.copy2(DIGEST_SH, dig)
        else:
            body = {
                # The pre-62898e32 script: fewer than 4 arguments -> usage, rc 2.
                "old": "[ $# -ge 4 ] || { echo 'usage: digest-node.sh <rpc-url> ...' >&2; exit 2; }\n",
                # The sandbox stub: rc 0 but no capability listed.
                "noop": "exit 0\n",
                "other": "echo 'some-other-capability'\n",
            }[digest]
            with open(dig, "w") as f:
                f.write("#!/bin/sh\ntouch '%s'\n%s" % (marker, body))
            os.chmod(dig, 0o755)
        return d, marker

    def _preflight(self, tools_dir, **env):
        on = self.fn("liq_stress_on() {", "\n}\n")
        i = self.src.index("summarizer_has_liq_opts() {")
        j = self.src.index("SUMMARIZE_LIQ_ARGS=()", i)
        block = self.src[i : self.src.index("\nfi\n", j) + 4]
        script = (
            "TOOLS_DIR=%s\nTOOLS_FROM_WORKTREE=1\n" % tools_dir
            + on
            + block
            + 'echo "args=${SUMMARIZE_LIQ_ARGS[*]-} pos=${DIGEST_HAS_POS-unset}"\n'
        )
        return self._run(script, **env)

    def test_real_digest_script_lists_position_accounts(self):
        r = subprocess.run(
            [DIGEST_SH, "--capabilities"], capture_output=True, text=True, timeout=10
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("position-accounts", r.stdout.split("\n"))

    def test_stress_cell_needs_a_digest_script_that_covers_positions(self):
        tmp = tempfile.mkdtemp(prefix="liq-digcap-")
        try:
            off = dict(LIQ_THIN="0", LIQ_THIN_AVAIL="1000000.0", ORACLE_SHOCK_BP="0")
            thin = dict(LIQ_THIN="5", LIQ_THIN_AVAIL="2.5", ORACLE_SHOCK_BP="0")
            shock = dict(
                LIQ_THIN="0", LIQ_THIN_AVAIL="1000000.0", ORACLE_SHOCK_BP="400"
            )
            for kind in ("old", "noop", "other"):
                d, marker = self._tools(tmp, kind, kind)
                # Stress cell + digest script without the capability: FATAL rc 1.
                for env in (thin, shock):
                    r = self._preflight(d, **env)
                    self.assertEqual(r.returncode, 1, (kind, env, r.stdout, r.stderr))
                    self.assertIn("FATAL", r.stderr)
                    self.assertIn(os.path.join(d, "digest-node.sh"), r.stderr)
                    self.assertIn("position", r.stderr)
                    self.assertIn("TOOLS_FROM_WORKTREE=0", r.stderr)
                # Normal cell: unchanged, the digest script is not even asked.
                os.path.exists(marker) and os.unlink(marker)
                r = self._preflight(d, **off)
                self.assertEqual(
                    (r.returncode, r.stdout), (0, "args= pos=0\n"), (kind, r.stderr)
                )
                self.assertFalse(os.path.exists(marker), kind)
            # A digest script that covers positions: accepted, both cell kinds.
            d, _ = self._tools(tmp, "new", "new")
            r = self._preflight(d, **thin)
            self.assertEqual(
                (r.returncode, r.stdout),
                (0, "args=--liq-thin 5 --liq-thin-avail 2.5 pos=1\n"),
                r.stderr,
            )
            r = self._preflight(d, **shock)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertTrue(r.stdout.endswith(" pos=1\n"), r.stdout)
            r = self._preflight(d, **off)
            self.assertEqual((r.returncode, r.stdout), (0, "args= pos=0\n"), r.stderr)
        finally:
            shutil.rmtree(tmp)

    def test_digest_capability_checked_before_launch(self):
        at = self.src.index("digest_has_position_accounts ||")
        self.assertLess(self.src.index('[ -x "$TOOLS_DIR/digest-node.sh" ]'), at)
        self.assertLess(at, self.src.index('mkdir -p "$OUT"'))

    def test_stress_cell_with_an_older_worktree_digest_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="liq-olddig-")
        try:
            fake_target(
                tmp, "Usage: oracle-feed --price <PRICE> --walk-bp <N> --shock-bp <BP>"
            )
            script, wt = sandbox_repo(tmp)
            tools = os.path.join(wt, "tools", "matched-bench")
            # A summarizer that passes its own check, so only the digest
            # script (the sandbox stub: no capabilities) can refuse the cell.
            with open(os.path.join(tools, "summarize.py"), "w") as f:
                f.write(self.NEW_SUMMARIZER)
            base = {
                k: v
                for k, v in os.environ.items()
                if not k.startswith(("LIQ_", "ORACLE_", "TOOLS_"))
            }
            for env in (
                dict(LIQ_THIN="5", SENDERS="5"),
                dict(ORACLE_FEED="1", ORACLE_SHOCK_BP="400", ORACLE_SHOCK_ROUND="30"),
            ):
                r = subprocess.run(
                    [script, wt, "liq-olddig-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        base,
                        TARGET_DIR=tmp,
                        RESULTS_ROOT=tmp,
                        DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1",
                        **env,
                    ),
                )
                self.assertEqual(r.returncode, 1, (env, r.stdout, r.stderr))
                self.assertIn(os.path.join(tools, "digest-node.sh"), r.stderr, env)
                self.assertIn("TOOLS_FROM_WORKTREE=0", r.stderr, env)
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, "liq-olddig-x")),
                    "must fail before any launch",
                )
        finally:
            shutil.rmtree(tmp)

    def test_digest_log_claims_positions_only_when_covered(self):
        on = self.fn("liq_stress_on() {", "\n}\n")
        block = self.fn("DIG_POS=()\nif liq_stress_on; then", "\nfi\n")
        tmp = tempfile.mkdtemp(prefix="liq-diglog-")
        try:
            script = (
                'log() { echo "$*" >&2; }\nLIQUIDATOR_VAULT=0x1\nADL_ESCROW_LONG=0x2\n'
                "ADL_ESCROW_SHORT=0x3\nMARKETS=4\nOUT=%s\n" % tmp
                + on
                + block
            )
            stress = dict(LIQ_THIN="5", ORACLE_SHOCK_BP="0")
            r = self._run(script, DIGEST_HAS_POS="1", **stress)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("positions in all 4 markets", r.stderr)
            for has in ("0", ""):
                r = self._run(script, DIGEST_HAS_POS=has, **stress)
                self.assertNotIn("positions in all", r.stderr, has)
                self.assertIn("NOT", r.stderr, has)
            r = self._run(script, DIGEST_HAS_POS="1", LIQ_THIN="0", ORACLE_SHOCK_BP="0")
            self.assertEqual((r.returncode, r.stderr), (0, ""))
        finally:
            shutil.rmtree(tmp)

    def test_summary_records_liq_thin(self):
        d = tempfile.mkdtemp(prefix="liq-sum-")
        try:
            write_cell(d, 1000, 1000)
            write_agreement(d, ["same"] * 3)
            s, _ = run_summarize(d)
            self.assertEqual(
                (s["cell"]["liq_thin"], s["cell"]["liq_thin_avail"]), (0, None)
            )
            s, _ = run_summarize(
                d, extra=("--liq-thin", "200", "--liq-thin-avail", "1000000.0")
            )
            self.assertEqual(
                (s["cell"]["liq_thin"], s["cell"]["liq_thin_avail"]), (200, "1000000.0")
            )
        finally:
            shutil.rmtree(d)
        self.assertIn(
            '--liq-thin "$LIQ_THIN" --liq-thin-avail "$LIQ_THIN_AVAIL"', self.src
        )


# ----------------------------------- cells run detached from the caller's shell
DETACH_SH = os.path.join(HERE, "campaign", "detach.sh")


def _in_bench_unit():
    with open("/proc/self/cgroup") as f:
        return "/bench-" in f.read()


def _have_user_systemd():
    if not shutil.which("systemd-run"):
        return False
    r = subprocess.run(
        ["systemctl", "--user", "is-system-running"], capture_output=True, text=True
    )
    return r.stdout.strip() in ("running", "degraded")


class DetachTest(unittest.TestCase):
    """2026-10-06 ozarchy: three 300-market runs each lost one process (val1,
    val2, the load generator) to a SIGKILL that was no OOM kill and no
    kill/tkill/tgkill. The cells ran as descendants of an agent's shell. A cell
    must run in its own transient systemd --user service (campaign/detach.sh),
    whose parent is the user manager, not the shell that started it."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="detach-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    @unittest.skipIf(_in_bench_unit(), "test process already runs in a bench unit")
    def test_run_cell_refuses_outside_a_bench_unit(self):
        wt = os.path.join(self.d, "wt")
        os.makedirs(os.path.join(wt, "devnet", "wsl"))
        results = os.path.join(self.d, "results")
        env = {
            k: v
            for k, v in os.environ.items()
            if k not in ("BENCH_ALLOW_UNDETACHED", "RUN_CELL_PRINT_PATHS")
        }
        env.update(RESULTS_ROOT=results, DATA_ROOT=os.path.join(self.d, "data"))
        r = subprocess.run(
            ["bash", RUN_CELL_SH, wt, "probe-label"],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("detach.sh", r.stderr)
        self.assertFalse(os.path.exists(results), "refused cell must not write results")
        self.assertFalse(os.path.exists(os.path.join(self.d, "data")))

    def _detach(self, name, script, extra_env=None):
        """Start SCRIPT through detach.sh in unit bench-NAME. HOME is a temp dir without projects/wt, so the unit
        takes no build lock (detach.sh: "No $HOME/projects/wt: no lock"; DetachBuildLockTest covers the lock):
        under the real ~/projects/wt/.cargo-heavy.lock a builder holding it made these tests wait out the 20 s
        deadline, fail, and leave the unit queued on the lock. The unit is stopped on cleanup either way."""
        log = os.path.join(self.d, "out.log")
        home = os.path.join(self.d, "home")
        os.makedirs(home, exist_ok=True)
        unit = "bench-%s.service" % re.sub(r"[^A-Za-z0-9_.-]", "-", name)
        self.addCleanup(
            subprocess.run,
            ["systemctl", "--user", "stop", unit],
            capture_output=True,
            timeout=30,
        )
        r = subprocess.run(
            ["bash", DETACH_SH, name, log, "bash", "-c", script],
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(os.environ, HOME=home, DETACH_PROBE="carried", **(extra_env or {})),
            cwd=self.d,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("holds the build lock", r.stdout, "the test unit must not queue on the shared build lock")
        deadline = time.time() + 20
        text = None
        while time.time() < deadline:
            if os.path.exists(log):
                with open(log) as f:
                    text = f.read()
                if "DONE" in text:
                    return text
            time.sleep(0.2)
        self.fail("detached command never finished; log: %r" % text)

    @unittest.skipUnless(_have_user_systemd(), "needs systemd --user")
    def test_detach_runs_in_a_bench_service_parented_by_the_user_manager(self):
        name = "test-detach-%d" % os.getpid()
        out = self._detach(
            name,
            'echo "CG=$(cut -d: -f3 /proc/self/cgroup)"; '
            'echo "PARENT=$(ps -o comm= -p $PPID)"; '
            'echo "GRANDPARENT=$(ps -o comm= -p $(ps -o ppid= -p $PPID))"; '
            'echo "PROBE=$DETACH_PROBE"; echo "PWD=$PWD"; echo DONE',
        )
        kv = dict(l.split("=", 1) for l in out.splitlines() if "=" in l)
        self.assertTrue(kv["CG"].endswith("/bench-%s.service" % name), kv["CG"])
        self.assertEqual(kv["PARENT"], "systemd")  # no build lock (_detach): no flock in between
        self.assertEqual(
            kv["PROBE"], "carried", "caller's exported env must carry over"
        )
        self.assertEqual(
            kv["PWD"], self.d, "caller's working directory must carry over"
        )

    @unittest.skipUnless(_have_user_systemd(), "needs systemd --user")
    def test_run_cell_guard_accepts_a_detached_unit(self):
        with open(RUN_CELL_SH) as f:
            line = next(l for l in f if l.startswith("BENCH_UNIT_RE="))
        out = self._detach(
            "test-guard-%d" % os.getpid(),
            line
            + 'grep -qE "$BENCH_UNIT_RE" /proc/self/cgroup && echo IN=1; echo DONE',
        )
        self.assertIn("IN=1", out)

    @unittest.skipUnless(_have_user_systemd(), "needs systemd --user")
    def test_detach_keeps_secrets_out_of_the_unit_env(self):
        """2026-10-08: a bench unit carried the launching Claude session's
        CLAUDE_CODE_MESSAGING_TOKEN, and `systemctl --user show` printed it into
        an agent transcript. Secrets must not reach the unit; other vars must."""
        secrets = {
            "CLAUDE_CODE_MESSAGING_TOKEN": "s1",
            "ANTHROPIC_API_KEY": "s2",
            "FIRECRAWL_API_KEY": "s3",
            "GH_TOKEN": "s4",
            "MY_SECRET": "s5",
            "DB_PASSWORD": "s6",
        }
        out = self._detach(
            "test-secrets-%d" % os.getpid(),
            "env | sort; echo DONE",
            extra_env=dict(secrets, TORUS_BOOK_CF_TARGET_FILE_MB="64"),
        )
        names = {l.split("=", 1)[0] for l in out.splitlines() if "=" in l}
        for k in secrets:
            self.assertNotIn(k, names, "%s must not reach the bench unit" % k)
        self.assertIn("TORUS_BOOK_CF_TARGET_FILE_MB", names)
        self.assertIn("DETACH_PROBE", names)

    def test_detach_needs_a_name_a_log_and_a_command(self):
        r = subprocess.run(
            ["bash", DETACH_SH, "x", "/tmp/x.log"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(r.returncode, 2)
        self.assertIn("usage", r.stderr)


FAKE_SYSTEMD_RUN = r"""#!/usr/bin/env bash
printf '%s\0' "$@" > "$FAKE_ARGV"
"""
FAKE_SYSTEMCTL = "#!/usr/bin/env bash\nexit 3\n"  # is-active: unit not running


class DetachBuildLockTest(unittest.TestCase):
    """s1156: a campaign started through detach.sh holds the shared build lock
    ($HOME/projects/wt/.cargo-heavy.lock, the one builders take with
    `flock -w 60 -o`, gate 26) for its whole run, so wrapped builds wait instead
    of skewing cells. Cells a campaign starts through detach.sh from inside its
    own bench unit take no lock (the campaign holds it; locking again would
    deadlock). No ~/projects/wt: no lock. Runs with a fake systemd-run (no unit
    is started, safe on a bench host)."""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="detach-lock-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.bin = os.path.join(self.d, "bin")
        os.makedirs(self.bin)
        for name, body in (
            ("systemd-run", FAKE_SYSTEMD_RUN),
            ("systemctl", FAKE_SYSTEMCTL),
        ):
            path = os.path.join(self.bin, name)
            with open(path, "w") as f:
                f.write(body)
            os.chmod(path, 0o755)
        self.home = os.path.join(self.d, "home")
        os.makedirs(self.home)
        self.argv_file = os.path.join(self.d, "argv")
        self.cgroup = os.path.join(self.d, "cgroup")
        with open(self.cgroup, "w") as f:  # an agent's shell, not a bench unit
            f.write("0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n")
        self.lock = os.path.join(self.home, "projects", "wt", ".cargo-heavy.lock")

    def _env(self):
        return dict(
            os.environ,
            PATH=self.bin + os.pathsep + os.environ["PATH"],
            HOME=self.home,
            FAKE_ARGV=self.argv_file,
            DETACH_CGROUP_FILE=self.cgroup,
        )

    def _unit_cmd(self, *cmd):
        """detach.sh's systemd-run argv after `--`: what the unit runs."""
        r = subprocess.run(
            ["bash", DETACH_SH, "camp", os.path.join(self.d, "c.log"), *cmd],
            capture_output=True, text=True, timeout=30, env=self._env(), cwd=self.d,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(self.argv_file) as f:
            argv = f.read().split("\0")[:-1]
        self.assertIn("--unit=bench-camp", argv)
        return argv[argv.index("--") + 1 :]

    def _run_unit(self, unit_cmd):
        return subprocess.run(
            unit_cmd, capture_output=True, text=True, timeout=30, env=self._env()
        )

    def test_campaign_holds_the_build_lock_for_its_whole_run(self):
        os.makedirs(os.path.dirname(self.lock))
        probe = 'flock -n -E 9 "%s" true; echo "rc=$?"; printf "<%%s>" "$@"' % self.lock
        cmd = self._unit_cmd("bash", "-c", probe, "x", "a b", "c")
        self.assertIn(self.lock, cmd)
        out = self._run_unit(cmd).stdout
        lines = out.splitlines()
        self.assertEqual(lines[0], "build lock: waiting")
        self.assertIn("rc=9", out, "lock must be held while the command runs")
        self.assertIn("<a b><c>", out, "arguments must reach the command intact")
        r = subprocess.run(["flock", "-n", "-E", "9", self.lock, "true"])
        self.assertEqual(r.returncode, 0, "lock must be free after the run")

    def test_campaign_waits_while_a_build_holds_the_lock(self):
        os.makedirs(os.path.dirname(self.lock))
        cmd = self._unit_cmd("bash", "-c", "echo STARTED")
        with open(self.lock, "w") as held:
            import fcntl

            fcntl.flock(held, fcntl.LOCK_EX)
            p = subprocess.Popen(
                cmd, stdout=subprocess.PIPE, text=True, env=self._env()
            )
            time.sleep(0.5)
            self.assertIsNone(p.poll(), "campaign must wait for the build")
            fcntl.flock(held, fcntl.LOCK_UN)
        out, _ = p.communicate(timeout=10)
        self.assertEqual(out.splitlines(), ["build lock: waiting", "STARTED"])

    def test_no_projects_wt_runs_without_the_lock(self):
        cmd = self._unit_cmd("bash", "-c", "echo hi", "x")
        self.assertEqual(cmd, ["bash", "-c", "echo hi", "x"])

    def test_cell_started_inside_a_bench_unit_takes_no_lock(self):
        os.makedirs(os.path.dirname(self.lock))
        with open(self.cgroup, "w") as f:
            f.write("0::/user.slice/user-1000.slice/user@1000.service/app.slice/bench-camp.service\n")
        cmd = self._unit_cmd("bash", "-c", "echo cell")
        self.assertEqual(cmd, ["bash", "-c", "echo cell"])

    def test_global_and_detach_use_the_same_lock_file(self):
        with open(DETACH_SH) as f:
            src = f.read()
        self.assertIn('"$HOME/projects/wt"', src)
        self.assertIn(".cargo-heavy.lock", src)
        self.assertIn("flock -o", src)



class BuildGuardTest(unittest.TestCase):
    """run-cell.sh's "never bench while building" guard (s1156 Codex review of
    40266a79): a builder waiting on the campaign's build lock
    (`flock -w 60 -o LOCK cargo build`) has "cargo build" in its argv but is not
    building; only a running cargo/rustc process may abort a cell."""

    def _guard(self, tmp):
        with open(RUN_CELL_SH) as f:
            line = next(l for l in f if "never bench while building" in l)
        script = os.path.join(tmp, "guard.sh")
        with open(script, "w") as f:
            f.write(line)  # a file, so the guard's own argv holds no "cargo build"
        return subprocess.run(["bash", script], capture_output=True, text=True)

    def _spawn(self, argv):
        p = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        time.sleep(0.2)
        return p

    def test_lock_waiter_does_not_abort_cell(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._spawn([sys.executable, "-c", "import time; time.sleep(30)",
                         "flock", "-w", "60", "-o", "/tmp/x.lock", "cargo", "build"])
            r = self._guard(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_running_cargo_aborts_cell(self):
        with tempfile.TemporaryDirectory() as tmp:
            fake = os.path.join(tmp, "cargo")
            os.symlink(shutil.which("sleep"), fake)
            self._spawn([fake, "30"])
            r = self._guard(tmp)
            self.assertEqual(r.returncode, 1)
            self.assertIn("never bench while building", r.stderr)


EVM_LOAD_PY = os.path.join(HERE, "evm_load.py")


class EvmBlockIntervalKnobTest(unittest.TestCase):
    """EVM_BLOCK_INTERVAL in EXTRA_ENV (bench Q12): the genesis EVM cadence N
    of one cell. Like TIMEOUT_BASE_MS it is a genesis field, so run-cell.sh
    lifts it out of EXTRA_ENV for gen-3val-genesis.sh (which patches only the
    per-cell genesis, never testnet/genesis-weighted-full.json), logs the N
    the genesis carries and records it in summary.json. Runs a sandbox copy
    whose generator writes a minimal genesis and whose launch fails, so the
    cell stops right after its genesis step."""

    def setUp(self):
        self.real_full = _stat(WEIGHTED_FULL)
        self.tmp = tempfile.mkdtemp(prefix="evm-n-knob-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        fake_target(self.tmp)
        self.script, self.wt = sandbox_repo(self.tmp)
        self.seen = os.path.join(self.tmp, "generator-env")
        self.gen = os.path.join(self.wt, "devnet", "wsl", "gen-3val-genesis.sh")
        self.write_generator("")
        launch = os.path.join(self.wt, "devnet", "wsl", "launch-3val.sh")
        with open(launch, "w") as f:
            f.write("#!/bin/sh\nexit 1\n")
        os.chmod(launch, 0o755)
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        with open(os.path.join(self.bin, "pgrep"), "w") as f:
            f.write("#!/bin/sh\nexit 1\n")
        os.chmod(os.path.join(self.bin, "pgrep"), 0o755)

    def tearDown(self):
        self.assertEqual(
            _stat(WEIGHTED_FULL), self.real_full, "the real worktree was touched"
        )

    def write_generator(self, stale_n, bulk="$BULK_COUNT", stale_book_mode=""):
        """A generator that records the EVM_BLOCK_INTERVAL it was given and
        writes a MARKETS-market genesis with that N (or with `stale_n`, the
        hand-patched weighted base case, when the knob is unset), funding
        `bulk` "bulk-test N" senders (default: the BULK_COUNT it was handed,
        which run-cell.sh checks after the genesis step). BOOK_MODE (s131) ->
        .consensus.book_mode, else `stale_book_mode` (empty = absent); the
        BOOK_MODE it was given goes to <seen>.book-mode."""
        with open(self.gen, "w") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                "printf '%%s\\n' \"${EVM_BLOCK_INTERVAL-<unset>}\" >> '%s'\n"
                "printf '%%s\\n' \"${BOOK_MODE-<unset>}\" >> '%s.book-mode'\n"
                "n=${EVM_BLOCK_INTERVAL:-%s}\n"
                "bm=${BOOK_MODE:-%s}\n"
                "jq -n --argjson m \"$MARKETS\" --arg n \"$n\" --arg bm \"$bm\" --argjson b \"%s\" '{markets: [range($m)],"
                " native_balances: [range($b) | {note: \"bulk-test \\(. + 60)\"}],"
                " consensus: ({timeout_base_ms: 500} + (if $bm == \"\" then {} else {book_mode: $bm} end)),"
                " evm: ({chain_id: 7778}"
                " + (if $n == \"\" then {} else {block_interval: ($n|tonumber)} end))}' > \"$OUT\"\n"
                % (self.seen, self.seen, stale_n, stale_book_mode, bulk)
            )
        os.chmod(self.gen, 0o755)

    def test_a_genesis_without_bulk_count_senders_stops_the_cell(self):
        # e.g. an older gen-3val-genesis.sh that ignores BULK_COUNT and reuses a 100k genesis
        self.write_generator("", bulk="7")
        r, log = self.run_cell(BULK_COUNT="8")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("genesis has 7 bulk-test senders, wanted BULK_COUNT=8", log)
        self.assertNotIn("launch-3val.sh failed", log)

    def run_cell(self, extra_env="", **env):
        e = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("LIQ_", "ORACLE_", "EXPECT_", "EVM_"))
            and k not in ("FORCE", "SENDERS", "BULK_COUNT")
        }
        e.update(
            PATH=self.bin + os.pathsep + os.environ["PATH"],
            TARGET_DIR=self.tmp,
            RESULTS_ROOT=os.path.join(self.tmp, "results"),
            DATA_ROOT=os.path.join(self.tmp, "data"),
            BENCH_ALLOW_UNDETACHED="1",
            SENDERS="5",
            BULK_COUNT="5",
            OVERWRITE="1",
        )
        e.update(env)
        r = subprocess.run(
            [self.script, self.wt, "knob-x", "4", "60", "1000", extra_env],
            capture_output=True,
            text=True,
            timeout=60,
            env=e,
        )
        try:
            with open(os.path.join(self.tmp, "results", "knob-x", "run.log")) as f:
                log = f.read()
        except OSError:
            log = ""
        return r, log

    def generator_saw(self):
        try:
            with open(self.seen) as f:
                return f.read().splitlines()
        except OSError:
            return []

    def test_extra_env_sets_n_and_the_next_cell_gets_the_default_back(self):
        r, log = self.run_cell("TORUS_X=1 EVM_BLOCK_INTERVAL=22 TIMEOUT_BASE_MS=1200")
        self.assertEqual(self.generator_saw(), ["22"], r.stdout + r.stderr + log)
        self.assertIn("genesis evm.block_interval=22 (EVM_BLOCK_INTERVAL=22)", log)
        self.assertIn("launch-3val.sh failed", log, "stops at the launch, after the genesis checks")
        # next cell, knob unset (an ambient EVM_BLOCK_INTERVAL must not leak in)
        r, log = self.run_cell("TORUS_X=1", EVM_BLOCK_INTERVAL="33")
        self.assertEqual(self.generator_saw(), ["22", ""], r.stdout + r.stderr + log)
        self.assertIn("genesis evm.block_interval=11 (absent: node default)", log)
        self.assertIn("launch-3val.sh failed", log)

    def test_a_genesis_whose_n_is_not_the_cells_fails(self):
        """The knob unset but the genesis carries another N (a hand-patched
        testnet/genesis-weighted-full.json, the no-knob route of the Q12
        spec, left behind): the cell refuses to run on it."""
        self.write_generator("22")
        r, log = self.run_cell("")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        fatal = [l for l in log.splitlines() if "FATAL" in l]
        self.assertEqual(len(fatal), 1, log)
        self.assertIn("evm.block_interval=22", fatal[0])
        self.assertIn("wanted 11", fatal[0])
        self.assertIn("genesis-weighted-full.json", fatal[0])
        self.assertNotIn("launch-3val.sh failed", log)

    def test_bad_extra_env_n_fails_preflight(self):
        for v in ("0", "1", "101", "x", "-3", ""):
            with self.subTest(v=v):
                r, _ = self.run_cell("EVM_BLOCK_INTERVAL=%s" % v)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn("EVM_BLOCK_INTERVAL must be an integer in 2..=100", r.stderr)
                self.assertEqual(self.generator_saw(), [])
                self.assertFalse(os.path.exists(os.path.join(self.tmp, "results")))

    def test_harness_vars_in_extra_env_fail_preflight(self):
        """Codex round 6: EXTRA_ENV is exported mid-run (after the RECORD_ENV), so a
        DEFER_REPORT_SCANS in it overwrote the harness's own value before the report
        scans read it; GENESIS_CACHE_DIR there is read by nothing but the nodes. Both
        are harness variables (the campaign driver sets them from its preflight):
        refused before anything runs"""
        for kv in ("DEFER_REPORT_SCANS=1", "DEFER_REPORT_SCANS=0", "GENESIS_CACHE_DIR=/x"):
            with self.subTest(kv=kv):
                r, _ = self.run_cell("TORUS_X=1 " + kv)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn(
                    "FATAL: %s in EXTRA_ENV (the node env)" % kv.split("=")[0], r.stderr
                )
                self.assertEqual(self.generator_saw(), [])
                self.assertFalse(os.path.exists(os.path.join(self.tmp, "results")))

    def test_n_is_the_evm_loads_cadence_and_is_recorded_in_the_summary(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        # the scan always checks the cadence: N defaults to the node's 11
        self.assertIn("EVM_N=$GEN_EVM_N", src)
        self.assertNotIn("EVM_N=$(jq -r '.evm.block_interval // empty'", src)
        # recorded after summarize.py (which rewrites summary.json), before "done"
        rec = src.index('record_genesis_evm_n "$OUT/summary.json" "$GEN_EVM_N"')
        self.assertLess(src.index('python3 "$TOOLS_DIR/summarize.py"'), rec)
        self.assertLess(rec, src.index('log "done -> $OUT/summary.json"'))
        head = src[: src.index("set -uo pipefail")]
        self.assertIn("EVM_BLOCK_INTERVAL=N", head)
        # the default is the node's
        with open(os.path.join(REPO, "crates", "torus-types", "src", "lib.rs")) as f:
            self.assertIn("pub const DEFAULT_EVM_BLOCK_INTERVAL: u64 = 11;", f.read())
        self.assertIn("DEFAULT_EVM_N=11", src)

    def test_record_genesis_evm_n_patches_only_that_field(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i = src.index("record_genesis_evm_n() {")
        fn = src[i : src.index("\n}\n", i) + 3]
        d = tempfile.mkdtemp(prefix="evm-n-rec-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        p = os.path.join(d, "summary.json")
        with open(p, "w") as f:
            json.dump({"genesis": {"md5": "ab", "markets": 300}, "label": "x"}, f)
        r = subprocess.run(["bash", "-c", fn + '\nrecord_genesis_evm_n "$1" 33', "_", p],
                           capture_output=True, text=True, timeout=10)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(p) as f:
            self.assertEqual(json.load(f), {"genesis": {"md5": "ab", "markets": 300,
                                                        "evm_block_interval": 33}, "label": "x"})
        self.assertEqual(sorted(os.listdir(d)), ["summary.json"], "no temp file left")

    def test_resummarize_keeps_the_genesis_n(self):
        d = tempfile.mkdtemp(prefix="evm-n-resum-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        write_cell(d, 1000, 1000, dur=120)
        write_agreement(d, ["0xaa"] * 3)
        run_summarize(d, dur=120)
        p = os.path.join(d, "summary.json")
        with open(p) as f:
            s = json.load(f)
        s["genesis"]["evm_block_interval"] = 22
        with open(p, "w") as f:
            json.dump(s, f)
        r = subprocess.run(["bash", os.path.join(HERE, "resummarize.sh"), d],
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(p) as f:
            self.assertEqual(json.load(f)["genesis"]["evm_block_interval"], 22)


class EvmLoadHarnessTest(unittest.TestCase):
    """EVM_LOAD_RATE (item 7): tools/tx-flood sends paced EVM transfers beside
    the native load. Off (the default) the cell's command lines are exactly the
    old ones; on, tx-flood starts with the bench and stops when it exits."""

    @classmethod
    def setUpClass(cls):
        with open(RUN_CELL_SH) as f:
            cls.src = f.read()

    def fn(self, start, end_marker):
        i = self.src.index(start)
        return self.src[i : self.src.index(end_marker, i)]

    def test_defaults_off_documented_and_allowlisted(self):
        self.assertIn("EVM_LOAD_RATE=${EVM_LOAD_RATE:-0}", self.src)
        self.assertIn("EVM_LOAD_SENDERS=${EVM_LOAD_SENDERS:-}", self.src)
        self.assertIn(
            "TX_FLOOD=${TX_FLOOD:-$TARGET_DIR/release/torus-tx-flood}", self.src
        )
        head = self.src[: self.src.index("set -uo pipefail")]
        self.assertIn("#   EVM_LOAD_RATE=R", head)
        self.assertIn("#   EVM_LOAD_SENDERS=S", head)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            rc = f.read()
        self.assertIn('"EVM_LOAD_RATE",', rc)
        self.assertIn('"EVM_LOAD_SENDERS",', rc)
        with open(os.path.join(HERE, "README.md")) as f:
            readme = f.read()
        self.assertIn("## EVM load in a cell (`EVM_LOAD_RATE`)", readme)

    def test_every_evm_load_step_is_gated(self):
        """Off, no EVM-load step runs: each sits in an `if evm_load_on` block
        (evm_load_stop in finish_fail is a no-op without a tx-flood pid)."""
        for marker in (
            "    if pgrep -x torus-tx-flood",
            "then trap 'stop_oracle_feed; evm_load_stop' EXIT",
            '    log "EVM load ON:',
            '    EVM_KEYS="$RUN_DIR/evm-load-keys.txt"',
            "then evm_load_start; fi",
            "then evm_load_stop; fi",
            '"$SELF_DIR/evm_load.py" scan',
            '"$SELF_DIR/evm_load.py" summary',
            "jq -e '.evm_load.ok == true'",
        ):
            i = self.src.index(marker)
            gate = self.src.rindex("if evm_load_on", 0, i)
            self.assertNotIn("\nfi\n", self.src[gate:i], marker)
        self.assertEqual(self.src.count("evm_load_start() {"), 1, "defined once")
        self.assertEqual(self.src.count("then evm_load_start; fi"), 1, "called once")
        ff = self.fn("finish_fail() {", "\n}\n")
        self.assertLess(ff.index("evm_load_stop"), ff.index("stop-3val.sh"))

    def test_the_evm_load_window_is_the_bench_window(self):
        """tx-flood starts right after the bench and stops right after it
        exits (before the drain); the scan reads the chain after the digest,
        before the nodes stop; the summary follows summarize.py."""
        s = self.src
        start = s.index("then evm_load_start; fi")
        self.assertLess(s.index('"${BENCH_CMD[@]}" > "$OUT/bench.log"'), start)
        self.assertLess(start, s.index("cpusampler & CPU_PID=$!"))
        stop = s.index("then evm_load_stop; fi")
        self.assertLess(s.index('log "bench exited rc='), stop)
        self.assertLess(
            stop,
            s.index(
                "# ---------------------------------------------------------------- 7. drain"
            ),
        )
        scan = s.index('"$SELF_DIR/evm_load.py" scan')
        # the ingest accounting (in-line, or deferred to the campaign end pass: test_cell_endpass.py)
        self.assertLess(s.index("# [/report-scans:ingest]"), scan)
        self.assertLess(
            scan,
            s.index(
                "# ---------------------------------------------------------------- 9. stop + collect logs"
            ),
        )
        summ = s.index('"$SELF_DIR/evm_load.py" summary')
        self.assertLess(s.index('python3 "$TOOLS_DIR/summarize.py"'), summ)
        self.assertLess(summ, s.index('log "done -> $OUT/summary.json"'))
        # the bench, oracle and node env are built without any EVM knob
        for block in (
            self.fn("BENCH_CMD=(", 'log "bench: ${BENCH_CMD[*]}"'),
            self.fn("    ORACLE_CMD=(", '\n    log "oracle feed: ${ORACLE_CMD[*]}"'),
            self.fn(
                "for v in $(env | grep -oE '^TORUS_", 'log "node env: $NODE_ENV_JSON"'
            ),
        ):
            self.assertNotIn("EVM_LOAD", block)
            self.assertNotIn("TX_FLOOD", block)

    def _flow(self, rate):
        """The cell's bench start .. bench exit, from the real script, with a
        stub bench (1 s) and a stub tx-flood (runs until SIGTERM). Returns
        (events, bench argv, tx-flood argv or None, log, keys file left?, stdout)."""
        tmp = tempfile.mkdtemp(prefix="evm-flow-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        ev = os.path.join(tmp, "events")
        bench = os.path.join(tmp, "bench")
        with open(bench, "w") as f:
            f.write(
                '#!/bin/bash\nprintf "%%s\\n" "$@" > "%s/bench.argv"\n'
                'echo "bench_start $(date +%%s.%%N)" >> "%s"\nsleep 1\n'
                'echo "bench_end $(date +%%s.%%N)" >> "%s"\n' % (tmp, ev, ev)
            )
        flood = os.path.join(tmp, "torus-tx-flood")
        with open(flood, "w") as f:
            f.write(
                '#!/bin/bash\nprintf "%%s\\n" "$@" > "%s/flood.argv"\n'
                'trap \'echo "flood_term $(date +%%s.%%N)" >> "%s"; exit 0\' TERM\n'
                'echo "flood_start $(date +%%s.%%N)" >> "%s"\n'
                "for _ in $(seq 1 300); do sleep 0.1; done\n" % (tmp, ev, ev)
            )
        for p in (bench, flood):
            os.chmod(p, 0o755)
        keys = os.path.join(tmp, "evm-load-keys.txt")
        with open(keys, "w") as f:
            f.write("20 0xabc 0xdef\n")
        gate = next(l for l in self.src.splitlines() if l.startswith("evm_load_on() {"))
        mval = next(l for l in self.src.splitlines() if l.startswith("mval() {"))
        script = "\n".join(
            [
                "set -uo pipefail",
                'OUT="%s"; DUR=1; RPCS=(u0 u1 u2); METS=(9161 9162 9163)' % tmp,
                'TX_FLOOD="%s"; EVM_LOAD_RATE=%s; EVM_LOAD_SENDERS=4' % (flood, rate),
                'BENCH_CMD=("%s" consensus --rate-total 100)' % bench,
                'log() { echo "LOG $*" >> "%s/log"; }' % tmp,
                'scrape_one() { echo "torus_block_height 123"; }',
                "cpusampler() { :; }",
                mval,
                gate,
                self.fn("alive() {", "\nstop_sampler() {"),
                'EVM_KEYS="%s"; EVM_CHAIN_ID=7778' % keys,
                self.fn("T_BENCH0=$(date +%s)\n", "cpusampler & CPU_PID=$!"),
                'wait "$BENCH_PID"; BENCH_RC=$?',
                'log "bench exited rc=$BENCH_RC"',
                "if evm_load_on; then evm_load_stop; fi",
                'echo "EVM_PID=[$EVM_PID] EVM_RC=[$EVM_RC] EVM_H0=[$EVM_H0] EVM_H1=[$EVM_H1]"',
            ]
        )
        r = subprocess.run(
            ["bash", "-c", script], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stderr)

        def read(name):
            try:
                with open(os.path.join(tmp, name)) as f:
                    return f.read()
            except OSError:
                return None

        def argv(name):
            text = read(name)
            return None if text is None else text.split("\n")[:-1]

        events = {}
        for line in (read("events") or "").splitlines():
            k, t = line.split()
            events[k] = float(t)
        return (
            events,
            argv("bench.argv"),
            argv("flood.argv"),
            read("log") or "",
            os.path.exists(keys),
            r.stdout,
        )

    def test_knob_off_runs_no_tx_flood_and_the_same_bench(self):
        _, bench0, flood0, log0, keys0, out0 = self._flow("0")
        self.assertIsNone(flood0, "no tx-flood when off")
        self.assertNotIn("EVM load", log0)
        self.assertIn("EVM_PID=[] EVM_RC=[] EVM_H0=[] EVM_H1=[]", out0)
        self.assertTrue(keys0, "nothing EVM-related touched")
        _, bench1, flood1, _, _, _ = self._flow("2.5")
        self.assertEqual(bench0, ["consensus", "--rate-total", "100"])
        self.assertEqual(
            bench1, bench0, "identical bench command line with the knob on"
        )
        self.assertIsNotNone(flood1)

    def test_knob_on_starts_and_stops_tx_flood_with_the_load_window(self):
        ev, _, flood, log, keys_left, out = self._flow("2.5")
        keys_arg = flood[flood.index("--keys-file") + 1]
        self.assertEqual(
            flood[: flood.index("--rate")],
            [
                "--rpc-urls",
                "u0,u1,u2",
                "--fanout",
                "pinned",
                "--keys-file",
                keys_arg,
                "--chain-id",
                "7778",
            ],
        )
        self.assertTrue(keys_arg.endswith("/evm-load-keys.txt"))
        self.assertEqual(flood[flood.index("--rate") + 1], "2.5")
        self.assertEqual(flood[flood.index("--duration-secs") + 1], "1")
        self.assertTrue(
            flood[flood.index("--stats-file") + 1].endswith("/evm-load-stats.json")
        )
        # started right after the bench started; SIGTERMed right after it ended
        self.assertLessEqual(ev["bench_start"] - 0.5, ev["flood_start"])
        self.assertLess(ev["flood_start"], ev["bench_end"])
        self.assertGreaterEqual(ev["flood_term"], ev["bench_end"])
        self.assertLess(
            ev["flood_term"] - ev["bench_end"], 5, "stopped when the bench exited"
        )
        self.assertIn("EVM_PID=[] EVM_RC=[0] EVM_H0=[123] EVM_H1=[123]", out)
        self.assertIn("EVM load: tx-flood pid", log)
        self.assertIn("(val0 committed height 123)", log)
        self.assertFalse(keys_left, "the sender keys are deleted when tx-flood stops")

    def test_a_cell_without_its_evm_load_is_not_accepted(self):
        """Codex reviews: with the knob on, a cell whose .evm_load is not ok
        is rejected by the canonical verdict (evm_load.py summary folded it
        into .validity): the real health.py exits 2 as for any rejected cell.
        No .evm_load at all: exit 1. Off: the old tail."""
        s = self.src
        gate = s.index("if evm_load_on && ! jq -e '.evm_load.ok == true'")
        self.assertLess(s.index('[ "$rc" = 0 ] || exit "$rc"'), gate)
        final = s.rindex('python3 "$TOOLS_DIR/health.py" accept "$OUT/summary.json"')
        self.assertLess(gate, final)
        tail = s[gate:]
        line = next(l for l in s.splitlines() if l.startswith("evm_load_on() {"))
        tmp = tempfile.mkdtemp(prefix="evm-accept-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        accepted = {"verdict": "ACCEPT", "accepted": True, "fail_reasons": []}
        rejected = {"verdict": "REJECT", "accepted": False,
                    "fail_reasons": ["EVM load failed: tx-flood rc=1"]}
        for rate, evm_load, validity, rc, said in (
            ("5", {"ok": False, "problems": ["tx-flood rc=1"]}, rejected, 2, '["tx-flood rc=1"]'),
            ("5", None, accepted, 1, '["no .evm_load in summary.json"]'),
            ("5", {"ok": True, "problems": []}, accepted, 0, None),
            ("0", None, accepted, 0, None),
        ):
            d = {"label": "x", "validity": validity}
            if evm_load is not None:
                d["evm_load"] = evm_load
            with open(os.path.join(tmp, "summary.json"), "w") as f:
                json.dump(d, f)
            script = "\n".join([
                'OUT="%s"; TOOLS_DIR="%s"; EVM_LOAD_RATE=%s' % (tmp, HERE, rate),
                'log() { echo "LOG $*"; }',
                line,
                tail,
            ])
            r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
            self.assertEqual(r.returncode, rc, (rate, evm_load, r.stdout, r.stderr))
            if said is None:
                self.assertNotIn("NOT accepted", r.stdout)
            else:
                self.assertIn("LOG EVM load: cell NOT accepted: " + said, r.stdout)

    def test_bad_evm_load_env_fails_preflight(self):
        tmp = tempfile.mkdtemp(prefix="evm-pre-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        tgt = os.path.join(tmp, "release")
        os.makedirs(tgt)
        for b in ("torus-node", "bench-throughput"):
            p = os.path.join(tgt, b)
            with open(p, "w") as f:
                f.write("#!/bin/sh\nexit 0\n")
            os.chmod(p, 0o755)
        script, wt = sandbox_repo(tmp)
        old_flood = os.path.join(tmp, "old-flood")
        with open(old_flood, "w") as f:
            f.write(
                "#!/bin/sh\necho 'Usage: torus-tx-flood --rpc-urls <URLS> --accounts <N>'\n"
            )
        os.chmod(old_flood, 0o755)
        for env, rc, msg in (
            (dict(EVM_LOAD_RATE="fast"), 2, "EVM_LOAD_RATE must be"),
            (dict(EVM_LOAD_RATE="-1"), 2, "EVM_LOAD_RATE must be"),
            (
                dict(EVM_LOAD_SENDERS="5"),
                2,
                "EVM_LOAD_SENDERS=5 needs EVM_LOAD_RATE > 0",
            ),
            (
                dict(EVM_LOAD_RATE="0", EVM_LOAD_SENDERS="5"),
                2,
                "needs EVM_LOAD_RATE > 0",
            ),
            (
                dict(EVM_LOAD_RATE="5", EVM_LOAD_SENDERS="41"),
                2,
                "EVM_LOAD_SENDERS must be",
            ),
            (
                dict(EVM_LOAD_RATE="5", EVM_LOAD_SENDERS="0"),
                2,
                "EVM_LOAD_SENDERS must be",
            ),
            (dict(EVM_LOAD_RATE="5"), 1, "(cargo build --release -p torus-tx-flood"),
            (dict(EVM_LOAD_RATE="5", TX_FLOOD=old_flood), 1, "has no --rate"),
        ):
            e = {
                k: v
                for k, v in os.environ.items()
                if not k.startswith(("EVM_LOAD", "TX_FLOOD"))
            }
            e.update(
                TARGET_DIR=tmp,
                RESULTS_ROOT=tmp,
                DATA_ROOT=tmp,
                BENCH_ALLOW_UNDETACHED="1",
                **env,
            )
            r = subprocess.run(
                [script, wt, "evm-pre-x"],
                capture_output=True,
                text=True,
                timeout=30,
                env=e,
            )
            self.assertEqual(r.returncode, rc, (env, r.stderr))
            self.assertIn(msg, r.stderr, env)
            self.assertFalse(
                os.path.exists(os.path.join(tmp, "evm-pre-x")),
                "fails before any launch",
            )


class EvmRpcStub(BaseHTTPRequestHandler):
    """eth_blockNumber (server.head(), may move), per-height executed EVM tx
    counts and per-address nonces; JSON-RPC batches unless server.no_batch."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *_a):
        pass

    def answer(self, req):
        m, p = req.get("method"), req.get("params") or []
        s = self.server
        if m == "eth_blockNumber":
            r = hex(s.head())
        elif m == "eth_getBlockTransactionCountByNumber":
            h = int(p[0], 16)
            r = None if h > s.head() or h in s.unreadable else hex(s.counts.get(h, 0))
        elif m == "eth_getTransactionCount":
            r = hex(s.nonces.get(p[0].lower(), 0))
        else:
            return {
                "jsonrpc": "2.0",
                "id": req.get("id"),
                "error": {"code": -32601, "message": "nope"},
            }
        return {"jsonrpc": "2.0", "id": req.get("id"), "result": r}

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        if isinstance(req, list):
            self.server.batches += 1
            out = (
                {
                    "jsonrpc": "2.0",
                    "id": None,
                    "error": {"code": -32600, "message": "batch disabled"},
                }
                if self.server.no_batch
                else [self.answer(r) for r in req]
            )
        else:
            self.server.singles += 1
            out = self.answer(req)
        body = json.dumps(out).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class EvmLoadScanTest(unittest.TestCase):
    """evm_load.py: the chain-side record of an EVM_LOAD_RATE cell."""

    def serve(self, head, counts, nonces=None, unreadable=(), no_batch=False):
        srv = ThreadingHTTPServer(("127.0.0.1", 0), EvmRpcStub)
        srv.head = head if callable(head) else (lambda: head)
        srv.counts, srv.nonces, srv.unreadable = counts, nonces or {}, set(unreadable)
        srv.no_batch, srv.batches, srv.singles = no_batch, 0, 0
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.shutdown)
        return srv, "http://127.0.0.1:%d" % srv.server_address[1]

    def run_py(self, *args):
        return subprocess.run(
            [sys.executable, EVM_LOAD_PY, *args],
            capture_output=True,
            text=True,
            timeout=60,
        )

    def scan(self, url, h0, h1, interval="11", stats=None, wait_s="5"):
        tmp = tempfile.mkdtemp(prefix="evm-scan-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        out = os.path.join(tmp, "scan.json")
        args = [
            "scan",
            "--rpc",
            url,
            "--from",
            str(h0),
            "--to",
            str(h1),
            "--interval",
            interval,
            "--wait-s",
            wait_s,
            "--out",
            out,
        ]
        if stats is not None:
            sp = os.path.join(tmp, "stats.json")
            with open(sp, "w") as f:
                json.dump(stats, f)
            args += ["--stats", sp]
        r = self.run_py(*args)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(out) as f:
            return json.load(f), r.stdout

    def test_counts_executed_txs_and_flags_blocks_off_the_cadence(self):
        srv, url = self.serve(200, {110: 3, 121: 2, 125: 1, 132: 1, 150: 9})
        d, line = self.scan(url, 100, 140)
        self.assertTrue(d["complete"])
        self.assertEqual((d["blocks_scanned"], d["scanned_to"]), (41, 140))
        self.assertEqual(
            (d["evm_blocks"], d["evm_txs"], d["evm_txs_per_block_max"]), (4, 7, 3)
        )
        self.assertEqual(d["evm_block_txs"], [[110, 3], [121, 2], [125, 1], [132, 1]])
        self.assertEqual((d["cadence_heights"], d["off_cadence_blocks"]), (3, 1))
        self.assertEqual(d["off_cadence_heights"], [125])
        self.assertEqual(d["read_errors"], 0)
        self.assertGreater(srv.batches, 0, "heights are read in JSON-RPC batches")
        self.assertIn(
            "4 EVM blocks, 7 EVM txs executed, off-cadence blocks 1 (N=11)", line
        )
        d, _ = self.scan(url, 100, 140, interval="")
        self.assertIsNone(d["off_cadence_blocks"], "no N, no cadence verdict")

    def test_waits_for_the_served_head_then_reads(self):
        t0 = time.monotonic()
        _, url = self.serve(
            lambda: 120 if time.monotonic() - t0 < 1.5 else 200, {121: 2, 132: 1}
        )
        d, _ = self.scan(url, 110, 140)
        self.assertTrue(d["complete"])
        self.assertGreaterEqual(d["waited_s"], 1.0)
        self.assertEqual((d["evm_blocks"], d["evm_txs"]), (2, 3))
        # a head that never gets there: scan what is served, complete=false
        _, url2 = self.serve(130, {121: 2, 132: 1})
        d, _ = self.scan(url2, 110, 140, wait_s="1")
        self.assertFalse(d["complete"])
        self.assertEqual((d["scanned_to"], d["evm_txs"]), (130, 2))

    def test_unreadable_heights_and_a_refused_batch(self):
        srv, url = self.serve(300, {110: 1, 121: 1}, unreadable={115}, no_batch=True)
        d, _ = self.scan(url, 100, 250)
        self.assertEqual(
            (d["evm_txs"], d["read_errors"], d["unread_heights"]), (2, 1, [115])
        )
        self.assertEqual(srv.batches, 1, "a refused batch is not retried")
        self.assertEqual(
            srv.singles, 151 + 1, "every height singly, plus the head poll"
        )

    def test_landed_by_nonce_from_the_tx_flood_stats(self):
        a, b = "0x" + "11" * 20, "0x" + "22" * 20
        stats = {
            "per_sender": [
                {"address": a, "start_nonce": 0, "next_nonce": 10},
                {"address": b, "start_nonce": 5, "next_nonce": 9},
            ]
        }
        _, url = self.serve(200, {121: 12}, nonces={a: 8, b: 9})
        d, line = self.scan(url, 110, 130, stats=stats)
        self.assertEqual((d["landed_by_nonce"], d["accepted_not_landed"]), (12, 2))
        self.assertEqual([s["chain_nonce"] for s in d["senders"]], [8, 9])
        self.assertIn("landed by nonce 12, accepted not landed 2", line)

    def test_summary_folds_an_evm_failure_into_the_canonical_verdict(self):
        """Codex review 2: readers of summary.json (health.py accept,
        ab-driver's benchmark_accepted, cell_step's validity) see a failed EVM
        load as a rejected cell. Idempotent; an ok EVM load changes nothing."""
        tmp = tempfile.mkdtemp(prefix="evm-fold-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        base = {"label": "x", "status": "OK", "headline": {"benchmark_accepted": True},
                "validity": {"verdict": "ACCEPT", "accepted": True, "fail_reasons": [],
                             "unverified_reasons": []}}
        scan = {"evm_txs": 9, "evm_blocks": 3, "interval": 11, "off_cadence_blocks": 0,
                "complete": True, "read_errors": 0}
        for rc, want_ok in (("0", True), ("1", False)):
            with open(os.path.join(tmp, "summary.json"), "w") as f:
                json.dump(base, f)
            with open(os.path.join(tmp, "evm-load-stats.json"), "w") as f:
                json.dump({"accepted": 9, "elapsed_s": 3.0}, f)
            with open(os.path.join(tmp, "evm-load-scan.json"), "w") as f:
                json.dump(scan, f)
            for _ in range(2):
                r = self.run_py("summary", "--dir", tmp, "--rate", "3", "--senders", "4", "--rc", rc)
                self.assertEqual(r.returncode, 0, r.stderr)
            with open(os.path.join(tmp, "summary.json")) as f:
                d = json.load(f)
            self.assertIs(d["evm_load"]["ok"], want_ok)
            if want_ok:
                self.assertEqual((d["status"], d["validity"], d["headline"]),
                                 (base["status"], base["validity"], base["headline"]))
            else:
                self.assertEqual(d["status"], "INVALID")
                self.assertEqual((d["validity"]["verdict"], d["validity"]["accepted"]), ("REJECT", False))
                self.assertEqual(d["validity"]["fail_reasons"], ["EVM load failed: tx-flood rc=1"],
                                 "one EVM reason, however often it runs")
                self.assertIs(d["headline"]["benchmark_accepted"], False)
                rc_h = subprocess.run([sys.executable, os.path.join(HERE, "health.py"), "accept",
                                       os.path.join(tmp, "summary.json")]).returncode
                self.assertEqual(rc_h, 2, "health.py accept rejects it")

    def test_resummarize_keeps_the_evm_load_and_its_verdict(self):
        """Codex review 2: resummarize.sh rebuilds summary.json with
        summarize.py, which knows nothing of .evm_load: it is re-derived from
        the retained evm-load-* artifacts, verdict included."""
        d = tempfile.mkdtemp(prefix="evm-resum-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        write_cell(d, 1000, 1000, dur=120)
        write_agreement(d, ["0xaa"] * 3)
        run_summarize(d, dur=120)
        with open(os.path.join(d, "evm-load-stats.json"), "w") as f:
            json.dump({"accepted": 30, "elapsed_s": 120.0, "fanout": "pinned"}, f)
        with open(os.path.join(d, "evm-load-scan.json"), "w") as f:
            json.dump({"evm_txs": 30, "evm_blocks": 10, "interval": 11, "off_cadence_blocks": 1,
                       "complete": True, "read_errors": 0}, f)
        r = self.run_py("summary", "--dir", d, "--rate", "0.25", "--senders", "40", "--rc", "0")
        self.assertEqual(r.returncode, 0, r.stderr)
        r = subprocess.run(["bash", os.path.join(HERE, "resummarize.sh"), d],
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(d, "summary.json")) as f:
            s = json.load(f)
        e = s["evm_load"]
        self.assertEqual((e["rate"], e["senders"], e["rc"], e["landed"]), ("0.25", 40, 0, 30))
        self.assertEqual(e["problems"], ["1 blocks carry EVM txs off the N=11 cadence"])
        self.assertIn("EVM load failed: 1 blocks carry EVM txs off the N=11 cadence",
                      s["validity"]["fail_reasons"])
        self.assertIs(s["headline"]["benchmark_accepted"], False)

    def test_funded_needs_every_sender_in_the_genesis_accounts(self):
        tmp = tempfile.mkdtemp(prefix="evm-funded-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        g, k = os.path.join(tmp, "genesis.json"), os.path.join(tmp, "keys.txt")
        a, b = "0x" + "aa" * 20, "0x" + "bb" * 20
        with open(k, "w") as f:
            f.write("20 %s 0x01\n21 %s 0x02\n" % (a, "0x" + "BB" * 20))
        with open(g, "w") as f:
            json.dump(
                {
                    "accounts": [
                        {"address": a, "balance": "1000000000000000000000000"},
                        {"address": b, "balance": "0x10"},
                    ]
                },
                f,
            )
        r = self.run_py("funded", "--genesis", g, "--keys", k)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(
            "2/2 EVM load senders funded in the genesis (min 16 wei)", r.stdout
        )
        with open(g, "w") as f:
            json.dump(
                {
                    "accounts": [
                        {"address": a, "balance": "5"},
                        {"address": b, "balance": "0"},
                    ]
                },
                f,
            )
        r = self.run_py("funded", "--genesis", g, "--keys", k)
        self.assertEqual(r.returncode, 1)
        self.assertIn("1 of 2 EVM load senders hold no EVM balance", r.stdout)

    def test_summary_merges_stats_scan_and_exec_evm_seconds(self):
        tmp = tempfile.mkdtemp(prefix="evm-summary-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)

        def w(name, obj):
            with open(os.path.join(tmp, name), "w") as f:
                f.write(obj if isinstance(obj, str) else json.dumps(obj))

        w("summary.json", {"label": "x", "status": "OK"})
        w(
            "evm-load-stats.json",
            {
                "fanout": "pinned",
                "sender_offset": 20,
                "stopped_by": "SIGTERM",
                "elapsed_s": 60.0,
                "sent": 130,
                "accepted": 120,
                "rejected": 10,
                "reject_reasons": {"mempool_full": 10},
                "throttled": 3,
                "missed": 0,
            },
        )
        w(
            "evm-load-scan.json",
            {
                "evm_txs": 118,
                "evm_blocks": 20,
                "landed_by_nonce": 118,
                "accepted_not_landed": 2,
                "interval": 11,
                "off_cadence_blocks": 0,
                "cadence_heights": 22,
                "from": 100,
                "scanned_to": 340,
                "complete": True,
                "read_errors": 0,
                "evm_txs_per_block_max": 9,
            },
        )
        for i in range(3):
            w(
                "metrics-before-val%d.txt" % i,
                "torus_exec_evm_seconds_sum 1.5\ntorus_exec_evm_seconds_count 100\n",
            )
            w(
                "metrics-after-val%d.txt" % i,
                "torus_exec_evm_seconds_sum %s\ntorus_exec_evm_seconds_count 340\n"
                % (1.5 + 0.1 * (i + 1)),
            )
        r = self.run_py(
            "summary", "--dir", tmp, "--rate", "2", "--senders", "40", "--rc", "0"
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(tmp, "summary.json")) as f:
            d = json.load(f)
        self.assertEqual(d["status"], "OK", "the rest of the summary is kept")
        e = d["evm_load"]
        self.assertEqual(
            (e["rate"], e["senders"], e["rc"], e["fanout"]), ("2", 40, 0, "pinned")
        )
        self.assertEqual(
            (e["sent"], e["accepted"], e["landed"], e["evm_blocks"]),
            (130, 120, 118, 20),
        )
        self.assertEqual(
            (e["off_cadence_blocks"], e["interval"], e["accepted_per_s"]), (0, 11, 2.0)
        )
        v1 = e["exec_evm_seconds"]["val1"]
        self.assertAlmostEqual(v1["delta_sum"], 0.2)
        self.assertEqual(v1["delta_count"], 240)
        self.assertAlmostEqual(v1["ms_per_evm_block"], 10.0)
        self.assertEqual((e["ok"], e["problems"]), (True, []))

    def test_summary_lists_why_a_cell_got_no_evm_load(self):
        """Codex review: a cell whose EVM load failed must not pass as a
        normal cell. .evm_load.problems names why; ok is false."""
        cases = (
            ("137", None, {"evm_txs": 5, "evm_blocks": 1, "interval": 11, "off_cadence_blocks": 2,
                           "complete": True, "read_errors": 0},
             ["tx-flood rc=137", "no evm-load-stats.json",
              "2 blocks carry EVM txs off the N=11 cadence"]),
            ("0", {"accepted": 40, "elapsed_s": 10.0}, {"evm_txs": 0, "evm_blocks": 0, "interval": 11,
                                                        "off_cadence_blocks": 0, "complete": False,
                                                        "read_errors": 3},
             ["scan incomplete: val0's eth head stayed below the committed head",
              "3 heights unreadable", "no EVM tx landed"]),
            ("0", {"accepted": 0, "elapsed_s": 10.0}, None,
             ["no EVM tx accepted", "no evm-load-scan.json"]),
            ("", {"accepted": 4, "elapsed_s": 2.0}, {"evm_txs": 4, "evm_blocks": 1, "interval": 11,
                                                     "off_cadence_blocks": 0, "complete": True,
                                                     "read_errors": 0},
             ["tx-flood rc=None"]),
        )
        for rc, stats, scan, want in cases:
            tmp = tempfile.mkdtemp(prefix="evm-problems-")
            self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
            with open(os.path.join(tmp, "summary.json"), "w") as f:
                json.dump({"label": "x"}, f)
            for name, obj in (("evm-load-stats.json", stats), ("evm-load-scan.json", scan)):
                if obj is not None:
                    with open(os.path.join(tmp, name), "w") as f:
                        json.dump(obj, f)
            r = self.run_py("summary", "--dir", tmp, "--rate", "2", "--senders", "4", "--rc", rc)
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(os.path.join(tmp, "summary.json")) as f:
                e = json.load(f)["evm_load"]
            self.assertEqual(e["problems"], want, rc)
            self.assertIs(e["ok"], False)



# ------------------------- run-cell.sh: BULK_COUNT and PRELOAD (bench queue 8f)
class BulkCountAndPreloadHarnessTest(unittest.TestCase):
    """BULK_COUNT (the funded bulk-sender count of the genesis, default
    100000) reaches gen-3val-genesis.sh, the cell= line and summary.json, and
    moves the bench's --funded-bulk-end. PRELOAD=N (default 0 = off) runs
    `bench-throughput preload` between node ready and the load start, before
    the metrics-before snapshot and the samplers, so pre-load blocks never
    count as load; its record lands in summary.json .preload and a missed
    target REJECTs the cell. Runs a sandbox copy (see sandbox_repo)."""

    @classmethod
    def setUpClass(cls):
        with open(RUN_CELL_SH) as f:
            cls.src = f.read()

    def setUp(self):
        self.real_full = _stat(WEIGHTED_FULL)
        self.tmp = tempfile.mkdtemp(prefix="bulk-preload-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        fake_target(self.tmp, "Usage: oracle-feed --price <PRICE> --walk-bp <N>; preload --per-owner <P>")
        self.script, self.wt = sandbox_repo(self.tmp)
        shutil.copy2(os.path.join(HERE, "preload.py"), os.path.dirname(self.script))
        self.called = os.path.join(self.tmp, "generator-called")
        gen = os.path.join(self.wt, "devnet", "wsl", "gen-3val-genesis.sh")
        with open(gen, "w") as f:
            f.write("#!/bin/sh\necho \"BULK_COUNT=${BULK_COUNT-unset}\" > '%s'\n" % self.called)
        os.chmod(gen, 0o755)
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        with open(os.path.join(self.bin, "pgrep"), "w") as f:
            f.write("#!/bin/sh\nexit 1\n")
        os.chmod(os.path.join(self.bin, "pgrep"), 0o755)

    def tearDown(self):
        self.assertEqual(_stat(WEIGHTED_FULL), self.real_full, "the real worktree was touched")

    def run_cell(self, **env):
        e = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("LIQ_", "ORACLE_", "EXPECT_", "PRELOAD", "SPAM_"))
            and k not in ("FORCE", "SENDERS", "BULK_COUNT")
        }
        e.update(
            PATH=self.bin + os.pathsep + os.environ["PATH"],
            TARGET_DIR=self.tmp,
            RESULTS_ROOT=os.path.join(self.tmp, "results"),
            DATA_ROOT=os.path.join(self.tmp, "data"),
            BENCH_ALLOW_UNDETACHED="1",
            SENDERS="5",
            FORCE="1",
            OVERWRITE="1",
            **env,
        )
        r = subprocess.run([self.script, self.wt, "bulk-x"], capture_output=True, text=True,
                           timeout=60, env=e)
        try:
            with open(os.path.join(self.tmp, "results", "bulk-x", "run.log")) as f:
                log = f.read()
        except OSError:
            log = ""
        return r, log

    def generator_saw(self):
        with open(self.called) as f:
            return f.read().strip()

    # ---- BULK_COUNT
    def test_bulk_count_reaches_the_generator_default_unchanged(self):
        r, log = self.run_cell()
        self.assertEqual(self.generator_saw(), "BULK_COUNT=100000", r.stderr + log)
        self.assertIn("bulk_count=100000", log)
        os.remove(self.called)
        r, log = self.run_cell(BULK_COUNT="1000000")
        self.assertEqual(self.generator_saw(), "BULK_COUNT=1000000", r.stderr + log)
        self.assertIn("bulk_count=1000000", log)

    def test_bad_bulk_count_or_too_many_senders_fails_preflight(self):
        for env, msg in (
            ({"BULK_COUNT": "abc"}, "BULK_COUNT must be a positive integer"),
            ({"BULK_COUNT": "0"}, "BULK_COUNT must be a positive integer"),
            ({"BULK_COUNT": "4"}, "SENDERS=5 exceeds BULK_COUNT=4"),
        ):
            with self.subTest(env=env):
                r, _ = self.run_cell(**env)
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertIn(msg, r.stderr)
                self.assertFalse(os.path.exists(self.called))

    def test_funded_bulk_end_reaches_the_bench_only_off_the_default(self):
        lines = [l for l in self.src.splitlines() if "--funded-bulk-end" in l and "BENCH_CMD+=" in l]
        self.assertEqual(len(lines), 1, lines)
        snippet = "BENCH_CMD=()\n" + lines[0] + '\necho "${BENCH_CMD[*]}"'
        for bulk, want in (("100000", ""), ("1000000", "--funded-bulk-end 1000060"), ("2000", "--funded-bulk-end 2060")):
            r = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True,
                               env=dict(os.environ, BULK_COUNT=bulk))
            self.assertEqual(r.stdout.strip(), want, r.stderr)

    def test_bulk_count_is_recorded_in_the_summary_and_kept_by_resummarize(self):
        i = self.src.index("record_genesis_bulk_count() {")
        fn = self.src[i : self.src.index("\n}\n", i) + 3]
        p = os.path.join(self.tmp, "summary.json")
        with open(p, "w") as f:
            json.dump({"genesis": {"md5": "x"}}, f)
        r = subprocess.run(["bash", "-c", fn + 'record_genesis_bulk_count "%s" 1000000' % p],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(p) as f:
            self.assertEqual(json.load(f)["genesis"], {"md5": "x", "bulk_count": 1000000})
        self.assertIn('record_genesis_bulk_count "$OUT/summary.json" "$BULK_COUNT"', self.src)
        with open(os.path.join(HERE, "resummarize.sh")) as f:
            resum = f.read()
        self.assertIn(".genesis.bulk_count", resum)
        self.assertIn("preload.py", resum)

    def test_knobs_are_documented_and_allowlisted(self):
        head = self.src[: self.src.index("set -uo pipefail")]
        for k in ("BULK_COUNT", "PRELOAD=N", "PRELOAD_PER_OWNER", "PRELOAD_OWNER_OFFSET",
                  "PRELOAD_DISTANCE", "PRELOAD_LEVELS", "PRELOAD_RATE", "PRELOAD_TIMEOUT"):
            self.assertIn(k, head)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            allow = f.read()
        for k in ("BULK_COUNT", "PRELOAD", "PRELOAD_PER_OWNER", "PRELOAD_OWNER_OFFSET",
                  "PRELOAD_DISTANCE", "PRELOAD_LEVELS", "PRELOAD_RATE", "PRELOAD_TIMEOUT"):
            self.assertIn('"%s",' % k, allow)

    # ---- PRELOAD
    def test_preload_defaults_off_and_every_step_is_gated(self):
        self.assertIn("PRELOAD=${PRELOAD:-0}", self.src)
        for marker in ('"${PRELOAD_CMD[@]}" >', '$(preload_record "', 'preload.py" summary --dir "$OUT" >/dev/null'):
            i = self.src.index(marker)
            gate = self.src.rindex("if preload_on", 0, i)
            self.assertNotIn("\nfi\n", self.src[gate:i], marker)
        r, log = self.run_cell()
        self.assertNotIn("pre-load", log)
        self.assertIn("preload='off'", log)

    def test_preload_runs_after_ready_and_before_the_load_window(self):
        run = self.src.index('"${PRELOAD_CMD[@]}" >')
        self.assertGreater(run, self.src.index('log "healthy after'))
        self.assertGreater(run, self.src.index('log "idle blk/s'))
        # after the oracle marks are fresh (orders are banded around the mark) ...
        self.assertGreater(run, self.src.index('log "oracle marks fresh on all'))
        # ... and before anything that measures the load.
        for later in ('scrape_one "${METS[$i]}" > "$OUT/metrics-before-val$i.txt"',
                      'python3 "$SELF_DIR/sample_metrics.py"', 'T_BENCH0=$(date +%s)',
                      '"${BENCH_CMD[@]}" >'):
            self.assertLess(run, self.src.index(later), later)

    def test_preload_cmd_carries_the_cell_shape(self):
        i = self.src.index("preload_cmd() {")
        fn = self.src[i : self.src.index("\n}\n", i) + 3]
        script = (
            'BENCH=/b; OUT=/o; MARKETS=300; SENDERS=5000; BULK_COUNT=1000000; BAND=5\n'
            'RPCS=(http://r0 http://r1 http://r2); METS=(1 2 3)\n'
            'PRELOAD=1000000; PRELOAD_PER_OWNER=900; PRELOAD_OWNER_OFFSET=5060; PRELOAD_DISTANCE=1000\n'
            'PRELOAD_LEVELS=100; PRELOAD_RATE=50; PRELOAD_TIMEOUT=900\n'
            + fn + 'preload_cmd; printf "%s\\n" "${PRELOAD_CMD[@]}"'
        )
        r = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        args = r.stdout.split("\n")
        self.assertEqual(args[:2], ["/b", "preload"])
        pairs = dict(zip(args[2::2], args[3::2]))
        self.assertEqual(pairs["--rpc-urls"], "http://r0,http://r1,http://r2")
        self.assertEqual(pairs["--metrics-urls"],
                         "http://127.0.0.1:1/metrics,http://127.0.0.1:2/metrics,http://127.0.0.1:3/metrics")
        want = {"--target": "1000000", "--markets": "300", "--per-owner": "900", "--owner-offset": "5060",
                "--sender-offset": "60", "--senders": "5000", "--funded-bulk-end": "1000060",
                "--target-margin": "1500", "--band": "5", "--distance": "1000", "--levels": "100",
                "--rate": "50", "--timeout-s": "900", "--out": "/o/preload.json"}
        for k, v in want.items():
            self.assertEqual(pairs.get(k), v, k)
        # The load uses the same econ mid (--target-margin 1500) and band.
        bench = self.src[self.src.index("BENCH_CMD=(") :]
        bench = bench[: bench.index(")\n")]
        self.assertIn("--target-margin 1500", bench)
        self.assertIn("--sender-offset 60", bench)

    def test_bad_preload_env_fails_preflight(self):
        for env, msg in (
            ({"PRELOAD": "lots"}, "PRELOAD must be"),
            ({"PRELOAD": "20000", "PRELOAD_PER_OWNER": "0"}, "PRELOAD_PER_OWNER must be"),
            ({"PRELOAD": "20000", "PRELOAD_PER_OWNER": "1001"}, "PRELOAD_PER_OWNER must be"),
            ({"PRELOAD": "20000", "PRELOAD_RATE": "fast"}, "PRELOAD_RATE must be"),
            ({"PRELOAD": "20000", "PRELOAD_DISTANCE": "x"}, "PRELOAD_DISTANCE must be"),
            # 1M / 900 = 1112 owners from 65: past a 1000-account genesis.
            ({"PRELOAD": "1000000", "BULK_COUNT": "1000"}, "not all genesis-funded"),
            # the top 50 funded indices are the spam keys
            ({"PRELOAD": "900", "BULK_COUNT": "60", "PRELOAD_OWNER_OFFSET": "100",
              "SPAM_CANCEL_KEYS": "30", "SPAM_CANCEL_RATE": "1", "SPAM_CANCEL_FUNDED": "1"}, "spam keys"),
        ):
            with self.subTest(env=env):
                r, _ = self.run_cell(**env)
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertIn(msg, r.stderr)
                self.assertFalse(os.path.exists(self.called), "refused before the genesis")

    def test_bench_without_preload_fails_preflight(self):
        for name in ("torus-node", "bench-throughput"):
            with open(os.path.join(self.tmp, "release", name), "w") as f:
                f.write("#!/bin/sh\necho 'Usage: oracle-feed --price <PRICE>'\n")
        r, _ = self.run_cell(PRELOAD="20000")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("no preload subcommand", r.stderr)

    def test_bench_dry_run_refusal_fails_preflight(self):
        with open(os.path.join(self.tmp, "release", "bench-throughput"), "w") as f:
            f.write("#!/bin/sh\ncase \"$*\" in *--dry-run*) echo 'preload: --distance 4 must exceed' >&2; exit 2;; esac\n"
                    "echo 'preload --per-owner'\n")
        r, _ = self.run_cell(PRELOAD="20000")
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("--distance 4 must exceed", r.stderr)

    def test_preload_phase_records_and_fails_the_cell_on_a_miss(self):
        """Run the real 4c block with a stub bench: a miss writes .preload with
        a REJECT and dies; a hit records and continues."""
        i = self.src.index("# ---------------------------------------------------------------- 4c. pre-load")
        block = self.src[i : self.src.index("# ---------------------------------------------------------------- 5. samplers")]
        i = self.src.index("preload_record() {")
        rec = self.src[i : self.src.index("\n}\n", i) + 3]
        out = os.path.join(self.tmp, "cell")
        os.makedirs(out)
        for reached, rc in ((False, 1), (True, 0)):
            with self.subTest(reached=reached):
                for f in os.listdir(out):
                    os.remove(os.path.join(out, f))
                rep = {"resting": 20000 if reached else 10, "reached": reached, "actions_failed": 0,
                       "owners": 23, "orders_placed": 20000}
                stub = os.path.join(self.tmp, "stub-preload")
                with open(stub, "w") as f:
                    f.write("#!/bin/sh\necho '%s' > '%s/preload.json'; exit %d\n" % (json.dumps(rep), out, rc))
                os.chmod(stub, 0o755)
                script = (
                    'OUT="%s"; SELF_DIR="%s"; PRELOAD=20000; MARKETS=10; PIDS=(); T_LAUNCH=100; T_HEALTHY=130\n'
                    'PRELOAD_TIMEOUT=900; PRELOAD_KILL_GRACE=60\n'
                    'PRELOAD_OWNERS="23 5060 5083"\n'
                    'log() { echo "LOG $*"; }; die() { echo "DIE $*"; exit 1; }\n'
                    'preload_on() { true; }\n'
                    'PRELOAD_CMD=("%s")\n' % (out, HERE, stub)
                    + rec + block + 'echo "CONTINUED"\n'
                )
                r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
                with open(os.path.join(out, "summary.json")) as f:
                    s = json.load(f)
                p = s["preload"]
                self.assertEqual(p["target"], 20000)
                self.assertEqual(p["boot_to_ready_s"], 30)
                self.assertIsInstance(p["seconds"], int)
                if reached:
                    self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
                    self.assertIn("CONTINUED", r.stdout)
                    self.assertTrue(p["ok"])
                else:
                    self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
                    self.assertIn("DIE", r.stdout)
                    self.assertNotIn("CONTINUED", r.stdout)
                    self.assertEqual(s["validity"]["verdict"], "REJECT")
                    self.assertFalse(p["ok"])

    def test_a_hung_preload_is_killed_past_its_timeout(self):
        """Belt and braces over the bench's own --timeout-s: the harness runs
        it under `timeout` (PRELOAD_TIMEOUT + PRELOAD_KILL_GRACE), so a hung
        pre-load fails the cell (rc 124) instead of blocking it."""
        self.assertIn("PRELOAD_KILL_GRACE=60", self.src)
        i = self.src.index("# ---------------------------------------------------------------- 4c. pre-load")
        block = self.src[i : self.src.index("# ---------------------------------------------------------------- 5. samplers")]
        i = self.src.index("preload_record() {")
        rec = self.src[i : self.src.index("\n}\n", i) + 3]
        out = os.path.join(self.tmp, "hung")
        os.makedirs(out)
        stub = os.path.join(self.tmp, "stub-hung")
        with open(stub, "w") as f:
            f.write("#!/bin/sh\nsleep 30\n")
        os.chmod(stub, 0o755)
        script = (
            'OUT="%s"; SELF_DIR="%s"; PRELOAD=20000; MARKETS=10; PIDS=(); T_LAUNCH=100; T_HEALTHY=130\n'
            'PRELOAD_TIMEOUT=1; PRELOAD_KILL_GRACE=1; PRELOAD_OWNERS="23 5060 5083"\n'
            'log() { echo "LOG $*"; }; die() { echo "DIE $*"; exit 1; }\n'
            'preload_on() { true; }\n'
            'PRELOAD_CMD=("%s")\n' % (out, HERE, stub)
            + rec + block + 'echo "CONTINUED"\n'
        )
        t0 = time.monotonic()
        r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
        self.assertLess(time.monotonic() - t0, 10)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("rc=124", r.stdout)
        with open(os.path.join(out, "summary.json")) as f:
            s = json.load(f)
        self.assertIn("preload rc=124", s["preload"]["problems"])
        self.assertEqual(s["validity"]["verdict"], "REJECT")

    def test_preload_is_re_recorded_after_summarize(self):
        summ = self.src.index('python3 "$TOOLS_DIR/summarize.py"')
        again = self.src.index('python3 "$SELF_DIR/preload.py" summary --dir "$OUT"', summ)
        self.assertLess(again, self.src.index('python3 "$TOOLS_DIR/health.py" accept'))



AUDITED_FLAGS = (
    "TORUS_BOOK_ROWS",
    "TORUS_BOOK_FAST_ENCODE",
    "TORUS_SETTLE_NO_MERGE",
    "TORUS_EVM_FAST_PATH",
    "TORUS_BOOK_COALESCE",
    "TORUS_CHECKPOINT_INTERVAL_MS",
)

# (TORUS_BOOK_ROWS value or None = unset, book mode summarize.py derives from
# the node env when run-cell.sh recorded no genesis mode: legacy cells). The
# old lenient parse: trimmed "1"/"2"/"3", anything else is Classic.
# (genesis consensus.book_mode, the cell= line / summary.json cell.book_mode label)
GENESIS_BOOK_MODES = (
    ("classic", "classic"),
    ("order_rows", "mode1"),
    ("level_authority", "mode2"),
    ("level_authority_chunked", "mode3"),
)

BOOK_MODE_CASES = (
    (None, "classic"),
    ("", "classic"),
    ("0", "classic"),
    ("1", "mode1"),
    ("2", "mode2"),
    ("3", "mode3"),
    (" 3 ", "mode3"),
    ("\t3\n", "mode3"),
    ("03", "classic"),
    ("4", "classic"),
    ("3 3", "classic"),
    ("x", "classic"),
)


# TORUS_RETENTION_BLOCKS -> the effective history retention of the cell
# (devnet/wsl/start-node.sh passes --retention-blocks=V when V is non-empty;
# torus-node: 0 = archive, unset = the default N = 100,000 since s131).
RETENTION_CASES = [
    (None, "100000(default)"),
    ("", "100000(default)"),
    ("0", "archive"),
    ("000", "archive"),
    ("1000", "1000"),
    ("0100", "100"),
    ("12x", "invalid"),
]


class BookModeNotPinnedTest(unittest.TestCase):
    """s131 (bench 8y, perf doc section 110): RECORD_ENV pinned
    TORUS_BOOK_ROWS=3, so a cell was mode 3 unless its campaign overrode the
    knob, and one 8y Classic smoke silently ran mode 3. The book mode is never
    implicit now: no override = the node default (Classic), and every cell logs
    and records its effective book mode and the default-changed flags. Runs a
    sandbox copy whose launch fails, so the cell stops right after the node env
    is exported and logged."""

    setUp = EvmBlockIntervalKnobTest.setUp
    tearDown = EvmBlockIntervalKnobTest.tearDown
    write_generator = EvmBlockIntervalKnobTest.write_generator
    run_cell = EvmBlockIntervalKnobTest.run_cell

    @staticmethod
    def flags_line(log):
        lines = [l for l in log.splitlines() if " book_mode=" in l]
        return lines

    def test_record_env_no_longer_sets_the_book_mode(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i = src.index("RECORD_ENV=(")
        block = src[i : src.index(")", i)]
        self.assertNotIn("TORUS_BOOK_ROWS", block)

    def test_no_override_runs_and_records_classic(self):
        r, log = self.run_cell("TORUS_X=1", TORUS_BOOK_ROWS="3")  # ambient: must not leak
        self.assertIn("launch-3val.sh failed", log, r.stdout + r.stderr + log)
        lines = self.flags_line(log)
        self.assertEqual(len(lines), 1, log)
        self.assertIn("cell=knob-x book_mode=classic", lines[0])
        self.assertIn("TORUS_BOOK_ROWS='unset (default)'", lines[0])
        env_line = [l for l in log.splitlines() if "node env: " in l][0]
        node_env = json.loads(env_line.split("node env: ", 1)[1])
        self.assertNotIn("TORUS_BOOK_ROWS", node_env)
        # logged after the node env is final and before the launch
        self.assertLess(log.index("node env: "), log.index(" book_mode="))
        self.assertLess(log.index(" book_mode="), log.index("launch-3val.sh failed"))

    def test_explicit_mode_3_records_mode_3(self):
        r, log = self.run_cell("TORUS_BOOK_ROWS=3")
        lines = self.flags_line(log)
        self.assertEqual(len(lines), 1, r.stdout + r.stderr + log)
        self.assertIn("cell=knob-x book_mode=mode3", lines[0])
        self.assertIn("TORUS_BOOK_ROWS='3'", lines[0])

    def test_explicit_empty_override_is_classic(self):
        r, log = self.run_cell("TORUS_BOOK_ROWS=")
        lines = self.flags_line(log)
        self.assertEqual(len(lines), 1, r.stdout + r.stderr + log)
        self.assertIn("book_mode=classic", lines[0])
        self.assertIn("TORUS_BOOK_ROWS='empty (default)'", lines[0])

    def test_flag_line_lists_every_default_changed_flag(self):
        r, log = self.run_cell(
            "TORUS_EVM_FAST_PATH=0 TORUS_CHECKPOINT_INTERVAL_MS= TORUS_BOOK_COALESCE=1"
        )
        lines = self.flags_line(log)
        self.assertEqual(len(lines), 1, r.stdout + r.stderr + log)
        line = lines[0]
        for k in AUDITED_FLAGS:
            self.assertEqual(line.count(" %s='" % k), 1, (k, line))
        self.assertIn("TORUS_EVM_FAST_PATH='0'", line)
        self.assertIn("TORUS_CHECKPOINT_INTERVAL_MS='empty (default)'", line)
        self.assertIn("TORUS_BOOK_COALESCE='1'", line)
        self.assertIn("TORUS_BOOK_FAST_ENCODE='unset (default)'", line)
        self.assertIn("TORUS_SETTLE_NO_MERGE='unset (default)'", line)
        # the order is fixed, so two arms' lines diff cleanly
        pos = [line.index(" %s='" % k) for k in AUDITED_FLAGS]
        self.assertEqual(pos, sorted(pos), line)

    def test_flag_line_records_the_effective_retention(self):
        """claude-review P2 (s131): since prune by default a cell without
        TORUS_RETENTION_BLOCKS runs retention (N = 100,000, DA GC worker and
        pruner on), so the line names the effective retention per cell."""
        for extra, want in [
            ("TORUS_X=1", "retention=100000(default)"),
            ("TORUS_RETENTION_BLOCKS=0", "retention=archive"),
            ("TORUS_RETENTION_BLOCKS=1000", "retention=1000"),
        ]:
            with self.subTest(extra=extra):
                r, log = self.run_cell(extra, TORUS_RETENTION_BLOCKS="7")  # ambient: must not leak
                lines = self.flags_line(log)
                self.assertEqual(len(lines), 1, r.stdout + r.stderr + log)
                self.assertIn(" book_mode=", lines[0])
                self.assertIn(" " + want + " ", lines[0])

    def test_bash_retention_mirrors_start_node_and_the_node(self):
        fn = self.node_flags_fn()
        for v, want in RETENTION_CASES:
            with self.subTest(v=v):
                env = {k: x for k, x in os.environ.items() if not k.startswith("TORUS_")}
                if v is not None:
                    env["TORUS_RETENTION_BLOCKS"] = v
                r = subprocess.run(["bash", "-c", fn + "\nnode_flags_line"],
                                   capture_output=True, text=True, timeout=10, env=env)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn(" retention=%s " % want, r.stdout, (v, r.stdout))

    def test_summary_records_the_effective_retention(self):
        d = tempfile.mkdtemp(prefix="retention-sum-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        write_cell(d, 1_000, 1_000)
        write_agreement(d, ["same"] * 3)
        for v, want in RETENTION_CASES:
            with self.subTest(v=v):
                env = {} if v is None else {"TORUS_RETENTION_BLOCKS": v}
                s, _ = run_summarize(d, extra=["--node-env", json.dumps(env)])
                self.assertEqual(s["cell"]["retention"], want)
        s, _ = run_summarize(d)
        self.assertIsNone(s["cell"]["retention"])

    def node_flags_fn(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i = src.index("NODE_FLAGS_AUDIT=(")
        j = src.index("node_flags_line() {")
        return src[i : src.index("\n}\n", j) + 3]

    def test_bash_book_mode_label_comes_from_the_genesis_mode(self):
        """s131 genesis book mode: the label is the cell genesis'
        consensus.book_mode (node_flags_line's argument), never the env."""
        fn = self.node_flags_fn()
        for name, label in GENESIS_BOOK_MODES:
            for v in (None, "", "3", "x"):
                with self.subTest(name=name, v=v):
                    env = {k: x for k, x in os.environ.items() if not k.startswith("TORUS_")}
                    if v is not None:
                        env["TORUS_BOOK_ROWS"] = v
                    r = subprocess.run(["bash", "-c", fn + "\nnode_flags_line " + name],
                                       capture_output=True, text=True, timeout=10, env=env)
                    self.assertEqual(r.returncode, 0, r.stderr)
                    self.assertTrue(r.stdout.startswith("book_mode=%s " % label), (name, v, r.stdout))

    def test_summary_records_book_mode_and_flags(self):
        d = tempfile.mkdtemp(prefix="book-mode-sum-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        write_cell(d, 1_000, 1_000)
        write_agreement(d, ["same"] * 3)
        for v, mode in BOOK_MODE_CASES:
            with self.subTest(v=v):
                env = {"TORUS_EVM_FAST_PATH": "0"}
                if v is not None:
                    env["TORUS_BOOK_ROWS"] = v
                s, _ = run_summarize(d, extra=["--node-env", json.dumps(env)])
                self.assertEqual(s["cell"]["book_mode"], mode)
                flags = s["cell"]["node_flags"]
                self.assertEqual(list(flags), list(AUDITED_FLAGS))
                self.assertEqual(flags["TORUS_EVM_FAST_PATH"], "0")
                self.assertEqual(flags["TORUS_SETTLE_NO_MERGE"], "unset (default)")
                want = ("unset (default)" if v is None
                        else "empty (default)" if v == "" else v)
                self.assertEqual(flags["TORUS_BOOK_ROWS"], want)
        # an empty node env (all defaults) is Classic too
        s, _ = run_summarize(d, extra=["--node-env", "{}"])
        self.assertEqual(s["cell"]["book_mode"], "classic")
        # a cell without a recorded node env says nothing rather than guessing
        s, _ = run_summarize(d)
        self.assertIsNone(s["cell"]["book_mode"])
        self.assertIsNone(s["cell"]["node_flags"])


class BookModeGenesisTest(unittest.TestCase):
    """s131: the book mode is the genesis field consensus.book_mode, and the
    node refuses a TORUS_BOOK_ROWS that disagrees with it. run-cell.sh lifts
    the mode out of EXTRA_ENV (TORUS_BOOK_ROWS=N, the existing campaign specs,
    or BOOK_MODE=<name|0-3>) into the cell genesis (gen-3val-genesis.sh
    BOOK_MODE), checks the genesis carries it, and labels the cell from the
    genesis. TORUS_BOOK_ROWS still reaches the nodes (it agrees: the node only
    warns). Sandbox copy whose launch fails, so the cell stops right after the
    node env is logged."""

    tearDown = EvmBlockIntervalKnobTest.tearDown
    write_generator = EvmBlockIntervalKnobTest.write_generator
    run_cell = EvmBlockIntervalKnobTest.run_cell
    generator_saw = EvmBlockIntervalKnobTest.generator_saw
    flags_line = staticmethod(BookModeNotPinnedTest.flags_line)

    # The sandbox's summarize.py (the one TOOLS_DIR resolves to): new = knows
    # --genesis-book-mode (this harness), old = a pre-s131 copy.
    NEW_SUMMARIZER = (
        "import argparse\nap = argparse.ArgumentParser()\n"
        "ap.add_argument('--genesis-book-mode', default='')\nap.parse_args()\n"
    )
    OLD_SUMMARIZER = "import argparse\nap = argparse.ArgumentParser()\nap.parse_args()\n"

    def setUp(self):
        EvmBlockIntervalKnobTest.setUp(self)
        self.write_summarizer(self.NEW_SUMMARIZER)

    def write_summarizer(self, body):
        with open(os.path.join(self.wt, "tools", "matched-bench", "summarize.py"), "w") as f:
            f.write(body)

    def book_mode_seen(self):
        try:
            with open(self.seen + ".book-mode") as f:
                return f.read().splitlines()
        except OSError:
            return []

    def genesis(self):
        with open(os.path.join(self.wt, "devnet", "wsl", "genesis-3val.json")) as f:
            return json.load(f)

    @staticmethod
    def node_env(log):
        line = [l for l in log.splitlines() if "node env: " in l][0]
        return json.loads(line.split("node env: ", 1)[1])

    def test_torus_book_rows_3_writes_mode_3_into_the_cell_genesis(self):
        r, log = self.run_cell("TORUS_X=1 TORUS_BOOK_ROWS=3")
        out = r.stdout + r.stderr + log
        self.assertEqual(self.book_mode_seen(), ["level_authority_chunked"], out)
        self.assertEqual(self.genesis()["consensus"]["book_mode"], "level_authority_chunked")
        self.assertIn("genesis consensus.book_mode=level_authority_chunked (TORUS_BOOK_ROWS=3)", log)
        lines = self.flags_line(log)
        self.assertEqual(len(lines), 1, out)
        self.assertIn("cell=knob-x book_mode=mode3", lines[0])
        # the existing specs' env still reaches the nodes: it agrees with genesis
        self.assertEqual(self.node_env(log).get("TORUS_BOOK_ROWS"), "3")
        self.assertIn("launch-3val.sh failed", log)

    def test_book_mode_knob_sets_the_genesis_without_a_node_env(self):
        for v in ("level_authority_chunked", "3"):
            with self.subTest(v=v):
                r, log = self.run_cell("TORUS_X=1 BOOK_MODE=%s" % v)
                out = r.stdout + r.stderr + log
                self.assertEqual(self.book_mode_seen()[-1], "level_authority_chunked", out)
                self.assertEqual(self.genesis()["consensus"]["book_mode"], "level_authority_chunked")
                self.assertIn("genesis consensus.book_mode=level_authority_chunked (BOOK_MODE=%s)" % v, log)
                self.assertIn("cell=knob-x book_mode=mode3", self.flags_line(log)[0])
                self.assertNotIn("TORUS_BOOK_ROWS", self.node_env(log))
                self.assertIn("launch-3val.sh failed", log)

    def test_every_mode_reaches_the_genesis_and_the_label(self):
        for (name, label), n in zip(GENESIS_BOOK_MODES, range(4)):
            with self.subTest(name=name):
                r, log = self.run_cell("TORUS_BOOK_ROWS=%d" % n)
                # Classic is the absent field: the generator gets an empty BOOK_MODE
                want = "" if name == "classic" else name
                self.assertEqual(self.book_mode_seen()[-1], want, r.stdout + r.stderr + log)
                self.assertIn("cell=knob-x book_mode=%s" % label, self.flags_line(log)[0])

    def test_no_knob_is_classic_and_the_genesis_has_no_field(self):
        # ambient values must not leak into the cell
        r, log = self.run_cell("TORUS_X=1", TORUS_BOOK_ROWS="3", BOOK_MODE="3")
        out = r.stdout + r.stderr + log
        self.assertEqual(self.book_mode_seen(), [""], out)
        self.assertNotIn("book_mode", self.genesis()["consensus"])
        self.assertIn("genesis consensus.book_mode=classic (absent: classic)", log)
        self.assertIn("cell=knob-x book_mode=classic", self.flags_line(log)[0])
        self.assertIn("launch-3val.sh failed", log)

    def test_explicit_classic_is_the_absent_field(self):
        """Review rec (b): an explicit Classic (BOOK_MODE=classic / 0,
        TORUS_BOOK_ROWS=0) builds the same genesis as no knob, the field
        absent, so one Classic chain has one genesis md5 and cache entry."""
        for kv in ("BOOK_MODE=classic", "BOOK_MODE=0", "TORUS_BOOK_ROWS=0"):
            with self.subTest(kv=kv):
                r, log = self.run_cell("TORUS_X=1 " + kv)
                out = r.stdout + r.stderr + log
                self.assertEqual(self.book_mode_seen()[-1], "", out)
                self.assertNotIn("book_mode", self.genesis()["consensus"])
                self.assertIn("genesis consensus.book_mode=classic (%s)" % kv, log)
                self.assertIn("cell=knob-x book_mode=classic", self.flags_line(log)[0])
                self.assertIn("launch-3val.sh failed", log)

    def test_book_mode_only_cell_with_an_older_summarizer_fails_preflight(self):
        """Review P1: a summarizer without --genesis-book-mode reads the mode
        from TORUS_BOOK_ROWS only, so a BOOK_MODE=-only non-Classic cell would
        be scored as Classic: refused before anything runs. With the env set
        (it reaches the nodes and the summarizer) or Classic, it runs."""
        self.write_summarizer(self.OLD_SUMMARIZER)
        r, log = self.run_cell("TORUS_X=1 BOOK_MODE=3")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr + log)
        self.assertIn("needs a summarizer that accepts --genesis-book-mode", r.stderr)
        self.assertEqual(self.book_mode_seen(), [])
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "results")))
        for kv in ("TORUS_BOOK_ROWS=3", "BOOK_MODE=3 TORUS_BOOK_ROWS=3", "BOOK_MODE=classic", ""):
            with self.subTest(kv=kv):
                r, log = self.run_cell("TORUS_X=1 " + kv)
                self.assertIn("launch-3val.sh failed", log, r.stdout + r.stderr + log)

    def test_summarizer_is_handed_the_genesis_mode(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i = src.index('python3 "$TOOLS_DIR/summarize.py" \\')
        call = src[i : src.index('| tee -a "$OUT/run.log"', i)]
        self.assertIn('"${SUMMARIZE_BOOK_ARGS[@]}"', call)
        before = src[src.rindex("SUMMARIZE_BOOK_ARGS=(", 0, i) : i]
        self.assertIn('--genesis-book-mode "$GEN_BOOK_MODE_GOT"', before)
        # the check runs in preflight, before the results dir exists
        self.assertLess(src.index("summarizer_has_genesis_book_mode; then"), src.index('mkdir -p "$OUT"'))

    def test_empty_torus_book_rows_is_classic(self):
        r, log = self.run_cell("TORUS_BOOK_ROWS=")
        self.assertEqual(self.book_mode_seen(), [""], r.stdout + r.stderr + log)
        self.assertIn("cell=knob-x book_mode=classic", self.flags_line(log)[0])

    def test_agreeing_knobs_run_and_disagreeing_ones_fail_preflight(self):
        r, log = self.run_cell("BOOK_MODE=level_authority_chunked TORUS_BOOK_ROWS=3")
        self.assertIn("cell=knob-x book_mode=mode3", self.flags_line(log)[0], r.stdout + r.stderr + log)
        shutil.rmtree(os.path.join(self.tmp, "results"))
        os.remove(self.seen + ".book-mode")
        r, _ = self.run_cell("BOOK_MODE=3 TORUS_BOOK_ROWS=0")
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("BOOK_MODE=3 and TORUS_BOOK_ROWS=0 name different book modes", r.stderr)
        self.assertEqual(self.book_mode_seen(), [])
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "results")))

    def test_bad_values_fail_preflight(self):
        for kv in ("TORUS_BOOK_ROWS=x", "TORUS_BOOK_ROWS=4", "TORUS_BOOK_ROWS=03",
                   "BOOK_MODE=mode3", "BOOK_MODE=", "BOOK_MODE=Classic", "BOOK_MODE=4"):
            with self.subTest(kv=kv):
                r, _ = self.run_cell("TORUS_X=1 " + kv)
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertIn("is not a book mode", r.stderr)
                self.assertEqual(self.book_mode_seen(), [])
                self.assertFalse(os.path.exists(os.path.join(self.tmp, "results")))

    def test_a_genesis_whose_mode_is_not_the_cells_fails(self):
        """The generator ignored BOOK_MODE (an older gen-3val-genesis.sh) or a
        hand-patched weighted-full genesis carries a mode: the cell refuses."""
        self.write_generator("", stale_book_mode="level_authority_chunked")
        r, log = self.run_cell("")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        fatal = [l for l in log.splitlines() if "FATAL" in l]
        self.assertEqual(len(fatal), 1, log)
        self.assertIn("consensus.book_mode=level_authority_chunked", fatal[0])
        self.assertIn("wanted classic", fatal[0])
        self.assertNotIn("launch-3val.sh failed", log)

    def test_record_genesis_book_mode_patches_the_summary(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i = src.index("record_genesis_book_mode() {")
        fn = src[i : src.index("\n}\n", i) + 3]
        d = tempfile.mkdtemp(prefix="book-mode-rec-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        p = os.path.join(d, "summary.json")
        for name, label in GENESIS_BOOK_MODES:
            with self.subTest(name=name):
                with open(p, "w") as f:
                    json.dump({"genesis": {"md5": "ab"}, "cell": {"book_mode": "classic", "x": 1}}, f)
                r = subprocess.run(["bash", "-c", fn + '\nrecord_genesis_book_mode "$1" "$2"', "_", p, name],
                                   capture_output=True, text=True, timeout=10)
                self.assertEqual(r.returncode, 0, r.stderr)
                with open(p) as f:
                    self.assertEqual(json.load(f), {"genesis": {"md5": "ab", "book_mode": name},
                                                    "cell": {"book_mode": label, "x": 1}})
                self.assertEqual(sorted(os.listdir(d)), ["summary.json"], "no temp file left")

    def test_book_mode_is_recorded_after_summarize_and_kept_by_resummarize(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        rec = src.index('record_genesis_book_mode "$OUT/summary.json" "$GEN_BOOK_MODE_GOT"')
        self.assertLess(src.index('python3 "$TOOLS_DIR/summarize.py"'), rec)
        self.assertLess(rec, src.index('log "done -> $OUT/summary.json"'))
        self.assertIn('--param BOOK_MODE="$GEN_BOOK_MODE"', src)
        head = src[: src.index("set -uo pipefail")]
        self.assertIn("BOOK_MODE=", head)
        with open(os.path.join(HERE, "resummarize.sh")) as f:
            resum = f.read()
        self.assertIn(".genesis.book_mode", resum)
        self.assertIn('--genesis-book-mode "$BOOK_MODE"', resum)


if __name__ == "__main__":
    if not os.path.exists(DIGEST_SH):
        print("NOTE: %s missing — digest tests will fail" % DIGEST_SH)
    unittest.main(verbosity=2)
