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
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
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
        try:
            skew = max(0, 40 - int(str(arg), 16)) if str(arg).startswith("0x") else 5
        except ValueError:
            skew = 5
        import time

        time.sleep(skew / 1000.0)
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": req.get("id", 1),
                "result": {"m": method, "a": str(arg)},
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
        cls.url = "http://127.0.0.1:%d" % cls.srv.server_address[1]
        cls.t = threading.Thread(target=cls.srv.serve_forever, daemon=True)
        cls.t.start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()

    def expected_stream(self, markets, accounts):
        lines = []
        for m in range(1, markets + 1):
            hx = "0x%x" % m
            lines.append(jq_compact({"m": "torus_getOrderBook", "a": hx}))
            lines.append(jq_compact({"m": "torus_getOpenInterest", "a": hx}))
        for a in accounts:
            lines.append(jq_compact({"m": "torus_getBalances", "a": a}))
        return "".join(line + "\n" for line in lines)

    def run_digest(self, markets, accounts, par):
        d = tempfile.mkdtemp(prefix="digest-test-")
        try:
            acct = os.path.join(d, "accounts.txt")
            with open(acct, "w") as f:
                f.write("".join(a + "\n" for a in accounts))
            out = os.path.join(d, "state-digest.txt")
            r = subprocess.run(
                ["bash", DIGEST_SH, self.url, str(markets), acct, out, str(par), "10"],
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
        s, _ = run_summarize(self.d, extra=["--max-in-flight", "1",
                                            "--open-order-budget", "900"])
        self.assertEqual(s["cell"]["max_in_flight"], 1)
        self.assertEqual(s["cell"]["open_order_budget"], 900)
        ing = s["ingest"]
        self.assertEqual(ing["bench_submit_rate"], 610.0)
        self.assertEqual(ing["econ_mix"], {
            "place": 69000, "cancel_all": 4251,
            "place_share": 0.942, "cancel_all_share": 0.058,
            "sent_place": 70000, "sent_cancel_all": 4300})
        self.assertEqual(ing["in_flight"], {
            "cap": 1, "released_committed": 72000, "released_refused": 1300,
            "released_timeout": 950, "in_flight_at_end": 4,
            "tail_url": "http://127.0.0.1:8647", "tail_fetched": 3600,
            "tail_errors": 12, "tail_missed": 1})

    def test_rpc_max_response_mb_comes_from_the_node_env(self):
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d, extra=[
            "--node-env", json.dumps({"TORUS_RPC_MAX_RESPONSE_MB": "64"})])
        self.assertEqual(s["cell"]["rpc_max_response_mb"], 64)
        s, _ = run_summarize(self.d)
        self.assertIsNone(s["cell"]["rpc_max_response_mb"], "unset = node default 10 MiB")

    def test_uncapped_legacy_cell_reports_none_for_the_new_fields(self):
        write_cell(self.d, 1_000, 1_000)  # bench.log: legacy one-liner, no rate
        write_agreement(self.d, ["same"] * 3)
        s, _ = run_summarize(self.d)
        self.assertIsNone(s["cell"]["max_in_flight"])
        self.assertIsNone(s["cell"]["open_order_budget"])
        self.assertIsNone(s["ingest"]["bench_submit_rate"])
        self.assertIsNone(s["ingest"]["econ_mix"])
        self.assertIsNone(s["ingest"]["in_flight"])

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
        at startup; counting it made every hash cell DISAGREE. Real fail-stops
        (ERROR/WARN) and raw `panicked` lines must still count."""
        with open(RUN_CELL_SH) as f:
            line = next(l for l in f if l.lstrip().startswith("panics=$(grep"))
        esc = "\x1b"
        sample = (
            "\n".join(
                [
                    f"{esc}[2m2026-10-01T22:25:23Z{esc}[0m {esc}[32m INFO{esc}[0m state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false",
                    "2026-10-01T22:25:23Z  INFO state_hash: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false",
                    f"{esc}[2m2026-10-01T22:30:00Z{esc}[0m {esc}[31mERROR{esc}[0m state_hash: STATE HASH FAIL-STOP latched at checkpoint 1900",
                    "thread 'torus-execution' panicked at crates/x.rs:1:1",
                    "2026-10-01T22:31:00Z  WARN app: Latching fail-stop: conflicting blocks",
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
        self.assertEqual(r.stdout.strip(), "3", r.stderr)

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
        self.assertIn("--max-in-flight \"$(j '.cell.max_in_flight // \"\"')\"", res)
        self.assertIn("--open-order-budget \"$(j '.cell.open_order_budget // \"\"')\"", res)
        lines = [l for l in src.splitlines() if "MAX_IN_FLIGHT" in l
                 and not l.lstrip().startswith(("#", "log ", "--", "python3", "if "))]
        snippet = ("RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)\n"
                   "BENCH_CMD=()\n" + "\n".join(lines) + '\necho "${BENCH_CMD[*]}"')

        def run(**env):
            base = {k: v for k, v in os.environ.items() if k != "MAX_IN_FLIGHT"}
            return subprocess.run(["bash", "-c", snippet], capture_output=True,
                                  text=True, env=dict(base, **env))

        r = run()
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        r = run(MAX_IN_FLIGHT="1")
        self.assertEqual(r.stdout.strip(), "--max-in-flight 1 --in-flight-watch-rpc "
                         "http://127.0.0.1:8647", r.stderr)
        for bad in ("x", "0", "-1", "1.5"):
            r = run(MAX_IN_FLIGHT=bad)
            self.assertEqual(r.returncode, 2, (bad, r.stdout, r.stderr))
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
        on = src.index('if [ -n "$MAX_IN_FLIGHT" ]; then\n    export TORUS_RPC_MAX_RESPONSE_MB')
        block = src[on : src.index("\nfi", on)]
        self.assertIn("export TORUS_RPC_MAX_RESPONSE_MB=64", block)
        self.assertIn("log ", block)
        self.assertLess(src.index("for v in $(env | grep -oE '^TORUS_"), on)
        self.assertLess(on, src.index("for kv in $EXTRA_ENV; do export"))
        self.assertLess(on, src.index("NODE_ENV_JSON=$("))
        self.assertIn("TORUS_RPC_MAX_RESPONSE_MB=64", src[: src.index("set -uo pipefail")])

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
        lines = [l for l in src.splitlines() if "SPAM_CANCEL" in l
                 and not l.lstrip().startswith(("#", "log "))]
        snippet = "BENCH_CMD=()\n" + "\n".join(lines) + '\necho "${BENCH_CMD[*]}"'

        def run(**env):
            base = {k: v for k, v in os.environ.items()
                    if not k.startswith("SPAM_CANCEL")}
            return subprocess.run(["bash", "-c", snippet], capture_output=True,
                                  text=True, env=dict(base, **env))

        r = run()
        self.assertEqual((r.returncode, r.stdout.strip()), (0, ""), r.stderr)
        r = run(SPAM_CANCEL_KEYS="8", SPAM_CANCEL_RATE="250.5")
        self.assertEqual(r.stdout.strip(),
                         "--spam-cancel-keys 8 --spam-cancel-rate 250.5", r.stderr)
        r = run(SPAM_CANCEL_KEYS="8", SPAM_CANCEL_RATE="100", SPAM_CANCEL_FUNDED="1")
        self.assertEqual(r.stdout.strip(), "--spam-cancel-keys 8 "
                         "--spam-cancel-rate 100 --spam-cancel-funded", r.stderr)
        for bad in ({"SPAM_CANCEL_KEYS": "x"},
                    {"SPAM_CANCEL_KEYS": "8"},  # no rate
                    {"SPAM_CANCEL_KEYS": "8", "SPAM_CANCEL_RATE": "fast"},
                    {"SPAM_CANCEL_KEYS": "8", "SPAM_CANCEL_RATE": "1",
                     "SPAM_CANCEL_FUNDED": "yes"}):
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
            ["bash", "-c", f'source "{wsl_env}"; '
             'echo "$TORUS_INGRESS_MIN_COLLATERAL $TORUS_ADDR_RATE_LIMIT $TORUS_RPC_IP_WEIGHT_PER_MIN"'],
            capture_output=True, text=True,
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


# --------------------------------------------- run-cell.sh: which tools score
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
        """depth-1 pipelining may cost ONE extra block of replay on top of the
        committed-but-unexecuted queue. Five is a broken fence."""
        write_crash(self.d, gap=8, queue=3)
        s, _ = run_summarize(self.d, extra=self.ON)
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
        s, _ = run_summarize(self.d, extra=self.ON)
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
                        os.environ, TARGET_DIR=tmp, RESULTS_ROOT=tmp, DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1", **env
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
                'BENCH=bt RPCS=(r0 r1) ORACLE_KEYS=k MARKETS=3 ORACLE_PRICE=30000 '
                "ORACLE_INTERVAL_MS=2000 OUT=o ORACLE_WALK_BP=%s\n" % walk
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
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            self.assertIn('"ORACLE_FEED_DRAIN",', f.read())
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
                (dict(ORACLE_FEED="1", ORACLE_FEED_DRAIN="yes"), "ORACLE_FEED_DRAIN must be 0 or 1"),
                (dict(ORACLE_FEED_DRAIN="1"), "ORACLE_FEED_DRAIN=1 needs ORACLE_FEED=1"),
                (dict(ORACLE_FEED="0", ORACLE_FEED_DRAIN="1"), "ORACLE_FEED_DRAIN=1 needs ORACLE_FEED=1"),
            ):
                r = subprocess.run(
                    [script, wt, "oracle-drain-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ, TARGET_DIR=tmp, RESULTS_ROOT=tmp, DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1", **env
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

    def _run_drain_flow(self, mode):
        """Run run-cell.sh's real bench-end pause block and drain block with
        python3 stubbed: each health.py call prints the feed's process state
        (T = SIGSTOPped) and its argv."""
        pause = self.fn('ORACLE_ALIVE_END=""\n', "\n# ------------------------------------------------"
                        "---------------- 7. drain")
        drain = self.fn("# ---------------------------------------------------------------- 7. drain",
                        "\nsleep 2\n")
        tmp = tempfile.mkdtemp(prefix="oracle-drain-flow-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        # what the (stubbed) health.py runs would have left behind
        os.makedirs(os.path.join(tmp, "feed-stop-settle"))
        with open(os.path.join(tmp, "drain.json"), "w") as f:
            json.dump({"drained": True, "feed_live": {
                "max_exec_lag": 2, "native_intervals": 4, "single_block_intervals": 3,
                "native_blocks": 5, "chain_ms": {"p50": 50.0, "p95": 200.0, "max": 200.0}}}, f)
        with open(os.path.join(tmp, "feed-stop-settle", "drain.json"), "w") as f:
            json.dump({"drained": True, "elapsed_s": 12.7}, f)
        script = (
            self.fn("alive() {", "\nstop_sampler() {")
            + '\nlog() { echo "LOG $*"; }\n'
            + "oracle_marks() { echo '{\"markets\":300,\"usable\":300}'; }\n"
            + 'python3() { echo "PY $(ps -o stat= -p "$FEED" | cut -c1) $*" >> "%s/py.log"; }\n' % tmp
            + "sleep 300 & ORACLE_PID=$!; FEED=$ORACLE_PID\n"
            + "trap 'kill -KILL $FEED 2>/dev/null' EXIT\n"
            + "OUT=%s TOOLS_DIR=/tools DRAIN_TIMEOUT=780 MARKETS=300 ORACLE_H0=7 "
              "ORACLE_FEED=1 ORACLE_FEED_DRAIN=%s T_BENCH1=$(date +%%s)\n" % (tmp, mode)
            + "METS=(9161 9162 9163)\n"
            + pause + "\n" + drain
            + '\ncat "$OUT/py.log"; echo "END $(ps -o stat= -p "$FEED" | cut -c1)"\n'
            # the real exit-path stop must still end the feed paused after the drain
            + 'stop_oracle_feed; alive "$FEED" && echo FEED_STILL_ALIVE; echo "STOPPED rc=$ORACLE_RC"\n'
        )
        r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return tmp, r.stdout

    def test_default_drain_flow_is_unchanged(self):
        out_dir, out = self._run_drain_flow("0")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        urls = " ".join("http://127.0.0.1:%d/metrics" % p for p in (9161, 9162, 9163))
        self.assertEqual(
            py,
            ["PY T /tools/health.py drain --out %s --timeout 780 --quiet 10 --urls %s"
             % (out_dir, urls)],
        )
        self.assertIn("LOG oracle feed paused (SIGSTOP) for drain + digest", out)
        self.assertIn("END T", out)
        self.assertNotIn("FEED_STILL_ALIVE", out)
        self.assertIn("STOPPED rc=143", out)

    def test_feed_drain_keeps_the_feed_live_then_pauses_and_settles(self):
        out_dir, out = self._run_drain_flow("1")
        py = [l for l in out.splitlines() if l.startswith("PY ")]
        self.assertEqual(len(py), 2, out)
        # live through the drain, judged by the feed-live criterion; the
        # mempool bound = 2 rounds x 3 validators x ceil(300/256) chunks
        self.assertTrue(py[0].startswith("PY S /tools/health.py drain --out %s " % out_dir), py[0])
        self.assertTrue(py[0].endswith(" --feed-live --feed-mempool-max 12"), py[0])
        # paused right after it, then a legacy quiet settle before the digest
        self.assertTrue(
            py[1].startswith("PY T /tools/health.py drain --out %s/feed-stop-settle --timeout 60 "
                             "--quiet 10 --urls " % out_dir), py[1])
        self.assertNotIn("--feed-live", py[1])
        self.assertTrue(os.path.isdir(os.path.join(out_dir, "feed-stop-settle")))
        self.assertIn("LOG feed-live drain:", out)
        self.assertIn("quiet_window_native_block_exec_ms", out)
        settle = [l for l in out.splitlines() if "feed-stop-settle" in l and l.startswith("LOG ")]
        self.assertEqual(len(settle), 1, out)
        self.assertRegex(settle[0], r"settle drained=1 after 12s")
        self.assertIn("quiet_window_native_block_exec_ms (oracle-only proxy) p50=50.0 p95=200.0 "
                      "max=200.0 native_blocks=5 single_block_intervals=3/4", out)
        self.assertIn("END T", out)
        self.assertNotIn("FEED_STILL_ALIVE", out)
        self.assertIn("STOPPED rc=143", out)
        # the existing exit paths still stop it (TERM + CONT): unchanged
        self.assertIn('kill -TERM "$pid" 2>/dev/null; kill -CONT "$pid" 2>/dev/null', self.src)

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
                (dict(ORACLE_FEED="1", ORACLE_WALK_BP="x"), 2, "ORACLE_WALK_BP must be"),
                (dict(ORACLE_FEED="1", ORACLE_WALK_BP="1250"), 2, "ORACLE_WALK_BP must be"),
                (dict(ORACLE_WALK_BP="10"), 2, "needs ORACLE_FEED=1"),
                (dict(ORACLE_FEED="1", ORACLE_WALK_BP="10"), 1, "has no --walk-bp"),
            ):
                r = subprocess.run(
                    [script, wt, "oracle-walk-pre-x"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=dict(
                        os.environ, TARGET_DIR=tmp, RESULTS_ROOT=tmp, DATA_ROOT=tmp,
                        BENCH_ALLOW_UNDETACHED="1", **env
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



LIQUIDATOR_VAULT = "0x746f7275732d6c697175696461746f722d766c74"


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
    shutil.copy2(os.path.join(REPO, "devnet", "wsl", "bench-validator-keys.json"), wsl)
    for p in (os.path.join(tools, "digest-node.sh"), os.path.join(wsl, "launch-3val.sh")):
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
        fake_target(self.tmp, "Usage: oracle-feed --price <PRICE> --walk-bp <N> --shock-bp <BP>")
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
        self.assertEqual(_stat(WEIGHTED_FULL), self.real_full, "the real worktree was touched")

    def write_full(self, rows):
        base = [{"address": "0x%040x" % i, "available": "1.0", "note": "bench sender %d" % i}
                for i in range(3)]
        with open(self.full, "w") as f:
            json.dump({"native_balances": base + rows, "accounts": []}, f)

    @staticmethod
    def bulk(n):
        return [{"address": "0x%040x" % (60 + i), "available": "100000000.0",
                 "note": "bulk-test %d" % (60 + i)} for i in range(n)]

    def run_cell(self, **env):
        e = {k: v for k, v in os.environ.items()
             if not k.startswith(("LIQ_", "ORACLE_", "EXPECT_")) and k not in ("FORCE", "SENDERS")}
        e.update(PATH=self.bin + os.pathsep + os.environ["PATH"], TARGET_DIR=self.tmp,
                 RESULTS_ROOT=os.path.join(self.tmp, "results"),
                 DATA_ROOT=os.path.join(self.tmp, "data"), BENCH_ALLOW_UNDETACHED="1",
                 SENDERS="5", **env)
        r = subprocess.run([self.script, self.wt, "guard-x"], capture_output=True,
                           text=True, timeout=60, env=e)
        try:
            with open(os.path.join(self.tmp, "results", "guard-x", "run.log")) as f:
                log = f.read()
        except OSError:
            log = ""
        return r, log

    def test_stale_weighted_full_genesis_fails_before_the_generator(self):
        # the s17 file: the stub bench's help line became one "bulk" row
        self.write_full([{"address": "oracle-feed", "available": "100000000.0",
                          "note": "bulk-test Usage:"}])
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
        for var, name in (("EXPECT_BENCH_MD5", "bench-throughput"), ("EXPECT_NODE_MD5", "torus-node")):
            with self.subTest(var=var):
                r, log = self.run_cell(OVERWRITE="1", **{var: "0123abcd"})
                self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
                fatal = [l for l in log.splitlines() if "FATAL" in l]
                self.assertEqual(len(fatal), 1, log)
                self.assertIn(name, fatal[0])
                self.assertIn(self.md5(name), fatal[0])
                self.assertIn("0123abcd", fatal[0])
                self.assertFalse(os.path.exists(self.called), "must stop before the genesis")

    def test_expected_md5_match_proceeds(self):
        self.write_full(self.bulk(5))
        r, log = self.run_cell(EXPECT_BENCH_MD5=self.md5("bench-throughput")[:8],
                               EXPECT_NODE_MD5=self.md5("torus-node"))
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

    def test_vault_joins_the_digest_only_when_enabled(self):
        on = self.fn("liq_stress_on() {", "\n}\n")
        block = self.fn(
            '"$BENCH" gen-accounts --offset 60 --count 50',
            'DIG_ACCTS=$(grep -c . "$OUT/digest-accounts.txt")',
        )
        tmp = tempfile.mkdtemp(prefix="liq-dig-")
        try:
            bench = os.path.join(tmp, "bt")
            with open(bench, "w") as f:
                f.write("#!/bin/sh\necho '60 0xaaa'\necho '61 0xbbb'\n")
            os.chmod(bench, 0o755)
            script = (
                'log() { echo "$*" >&2; }\nLIQUIDATOR_VAULT=%s\nBENCH=%s\nOUT=%s\n'
                % (LIQUIDATOR_VAULT, bench, tmp)
                + on
                + block
                + '\necho "$DIG_ACCTS"\n'
            )
            for env, accts in (
                (dict(LIQ_THIN="0", ORACLE_SHOCK_BP="0"), ["0xaaa", "0xbbb"]),
                (
                    dict(LIQ_THIN="5", ORACLE_SHOCK_BP="0"),
                    ["0xaaa", "0xbbb", LIQUIDATOR_VAULT],
                ),
                (
                    dict(LIQ_THIN="0", ORACLE_SHOCK_BP="400"),
                    ["0xaaa", "0xbbb", LIQUIDATOR_VAULT],
                ),
            ):
                r = self._run(script, **env)
                self.assertEqual(
                    (r.returncode, r.stdout.strip()),
                    (0, str(len(accts))),
                    (env, r.stderr),
                )
                with open(os.path.join(tmp, "digest-accounts.txt")) as f:
                    self.assertEqual(f.read().split(), accts, env)
        finally:
            shutil.rmtree(tmp)

    def test_vault_is_read_per_node_only_when_enabled(self):
        on = self.fn("liq_stress_on() {", "\n}\n")
        block = self.fn(
            "if liq_stress_on; then\n    for i in 0 1 2; do\n        rpc ", "\nfi\n"
        )
        self.assertIn("torus_getLiquidatorVault", block)
        # After the digests and their quiescence snapshot.
        at = self.src.index(block)
        self.assertLess(self.src.index('wait "${DIG_PIDS[@]}"'), at)
        self.assertLess(self.src.index("if Q_AFTER=$(funnel_snapshot)"), at)
        tmp = tempfile.mkdtemp(prefix="liq-vault-")
        try:
            script = (
                'log() { echo "$*" >&2; }\nRPCS=(u0 u1 u2)\nOUT=%s\n'
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
            capture_output=True, text=True, timeout=30, env=env,
        )
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("detach.sh", r.stderr)
        self.assertFalse(os.path.exists(results), "refused cell must not write results")
        self.assertFalse(os.path.exists(os.path.join(self.d, "data")))

    def _detach(self, name, script):
        log = os.path.join(self.d, "out.log")
        r = subprocess.run(
            ["bash", DETACH_SH, name, log, "bash", "-c", script],
            capture_output=True, text=True, timeout=30,
            env=dict(os.environ, DETACH_PROBE="carried"), cwd=self.d,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
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
            'echo "PROBE=$DETACH_PROBE"; echo "PWD=$PWD"; echo DONE',
        )
        kv = dict(l.split("=", 1) for l in out.splitlines() if "=" in l)
        self.assertTrue(kv["CG"].endswith("/bench-%s.service" % name), kv["CG"])
        self.assertEqual(kv["PARENT"], "systemd")
        self.assertEqual(kv["PROBE"], "carried", "caller's exported env must carry over")
        self.assertEqual(kv["PWD"], self.d, "caller's working directory must carry over")

    @unittest.skipUnless(_have_user_systemd(), "needs systemd --user")
    def test_run_cell_guard_accepts_a_detached_unit(self):
        with open(RUN_CELL_SH) as f:
            line = next(l for l in f if l.startswith("BENCH_UNIT_RE="))
        out = self._detach(
            "test-guard-%d" % os.getpid(),
            line + 'grep -qE "$BENCH_UNIT_RE" /proc/self/cgroup && echo IN=1; echo DONE',
        )
        self.assertIn("IN=1", out)

    def test_detach_needs_a_name_a_log_and_a_command(self):
        r = subprocess.run(["bash", DETACH_SH, "x", "/tmp/x.log"],
                           capture_output=True, text=True, timeout=10)
        self.assertEqual(r.returncode, 2)
        self.assertIn("usage", r.stderr)

if __name__ == "__main__":
    if not os.path.exists(DIGEST_SH):
        print("NOTE: %s missing — digest tests will fail" % DIGEST_SH)
    unittest.main(verbosity=2)
