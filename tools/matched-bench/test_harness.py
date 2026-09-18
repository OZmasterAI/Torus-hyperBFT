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

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
DIGEST_SH = os.path.join(HERE, "digest-node.sh")
CRASH_KILL_SH = os.path.join(HERE, "crash-kill.sh")
SUMMARIZE = os.path.join(HERE, "summarize.py")
RUN_CELL_SH = os.path.join(HERE, "run-cell.sh")


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


def write_agreement(d, digests, hashes=None, counters_equal=True, counters=None,
                    roots=None):
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
        self.assertIsNone(s["cell"]["workload"])
        self.assertEqual(s["cell"]["rate_schedule_observed"], [])
        self.assertIsNone(s["scheduled_evidence"])

    def test_workload_and_observed_phase_provenance_survive_resummarizing(self):
        from workload import parse_workload
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3)
        workload = parse_workload("1", ".2", ".05", "0:1000,120:0,180:2000", 300)
        with open(os.path.join(self.d, "workload.json"), "w") as f:
            json.dump(workload, f)
        observed = {"index": 1, "start_s": 120, "end_s": 180, "rate_total": 0,
                    "observed_elapsed_s": 120.01, "planned_unix_s": BENCH_START + 120}
        with open(os.path.join(self.d, "bench.log"), "a") as f:
            f.write("\nRATE_SCHEDULE_PHASE " + json.dumps(observed) + "\n")
            f.write("RATE_SCHEDULE_PHASE {truncated\n")
        command = "bench consensus --econ --rate-schedule 0:1000,120:0,180:2000"
        for _ in range(2):
            s, _ = run_summarize(self.d, extra=["--bench-cmd", command])
            self.assertEqual(s["cell"]["workload"], workload)
            self.assertEqual(s["cell"]["rate_schedule_observed"], [observed])
            self.assertEqual(len(s["cell"]["rate_schedule_parse_errors"]), 1)
            self.assertFalse(s["cell"]["rate_schedule_provenance"]["valid"])
            self.assertFalse(s["validity"]["accepted"])
            self.assertIn("rate schedule provenance unverified", s["validity"]["unverified_reasons"])
        with open(os.path.join(self.d, "bench.log"), "w") as f:
            for index, phase in enumerate(workload["rate_schedule"]):
                record = dict(phase, index=index, observed_elapsed_s=phase["start_s"] + .01,
                              planned_unix_s=BENCH_START + phase["start_s"])
                f.write("RATE_SCHEDULE_PHASE " + json.dumps(record) + "\n")
        s, _ = run_summarize(self.d, extra=["--bench-cmd", command])
        self.assertTrue(s["cell"]["rate_schedule_provenance"]["valid"])
        self.assertNotIn("rate schedule provenance unverified", s["validity"]["unverified_reasons"])
        # New descriptive evidence never upgrades or relaxes existing gates.
        before = s["validity"]
        self.assertFalse(s["scheduled_evidence"]["valid"])
        from scheduled_report import COUNT_FIELDS
        counts = [{k: 0 for k in COUNT_FIELDS} for _ in workload["rate_schedule"]]
        counts[0].update(queued_requests=1, http_started_requests=1, http_started_actions=4,
                         completed_requests=1, completed_actions=4, acked_actions=3)
        accounting = dict(schema=1, phases=counts, complete=True, observed_elapsed_s=301)
        with open(os.path.join(self.d, "bench.log"), "a") as f:
            f.write("RATE_SCHEDULE_ACCOUNTING " + json.dumps(accounting) + "\n")
        s, _ = run_summarize(self.d, extra=["--bench-cmd", command])
        self.assertEqual(s["validity"], before)
        self.assertEqual(s["scheduled_evidence"]["phases"][0]["generator"]["acked_actions"], 3)
        self.assertEqual(s["scheduled_evidence"]["recovery"]["status"], "unverified")
        with open(os.path.join(self.d, "bench.log"), "a") as f:
            f.write("RATE_SCHEDULE_ACCOUNTING {truncated\n")
        s, _ = run_summarize(self.d, extra=["--bench-cmd", command])
        self.assertEqual(s["validity"], before)
        self.assertFalse(s["scheduled_evidence"]["valid"])

    # --- bl4: metrics-after is one scrape, the 3 digests are concurrent ---
    # torus_native_actions_processed_total keeps ticking between them, so a
    # 1-2 block skew in WHERE the digests landed moves that counter alone.
    def test_action_counter_skew_at_skewed_digest_heights_is_unverified(self):
        """Equal block hash + header root + state digest, matched/placed/resting
        equal, and ONLY the action counter apart while the three digests were
        taken 2 blocks apart: a sampling artifact, not a fork. Not proof of
        agreement either -> DIGEST_UNVERIFIED."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3,
                        counters=[{"actions": 21688}, {"actions": 21688},
                                  {"actions": 21702}])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1",
                                            "--digest-heights", "280 280 282"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DIGEST_UNVERIFIED")
        self.assertIsNone(s["headline"]["validators_agree"])

    def test_action_counter_skew_far_apart_is_still_a_fork(self):
        """60 blocks apart is not a scrape skew, it is two different chains."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3,
                        counters=[{"actions": 21688}, {"actions": 21688},
                                  {"actions": 41702}])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1",
                                            "--digest-heights", "280 280 340"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")

    def test_resting_mismatch_is_a_fork_even_at_skewed_heights(self):
        """The skew escape hatch is for the ACTION counter only: a settled-state
        counter apart is divergence whatever the digest heights were."""
        write_cell(self.d, 1_000, 1_000)
        write_agreement(self.d, ["same"] * 3,
                        counters=[{"resting": 5}, {"resting": 5}, {"resting": 6}])
        s, _ = run_summarize(self.d, extra=["--digest-quiescent", "1",
                                            "--digest-heights", "280 280 282"])
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
            for f in ("summarize.py", "digest-node.sh", "crash-kill.sh",
                      "win60.awk", "phase60.awk"):
                open(os.path.join(t, f), "w").close()
        return wt

    def paths(self, wt, env=None):
        r = subprocess.run(
            ["bash", RUN_CELL_SH, wt, "probe-label"],
            capture_output=True, text=True, timeout=30,
            env=dict(os.environ, RUN_CELL_PRINT_PATHS="1", **(env or {})),
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return dict(
            line.split("=", 1) for line in r.stdout.split() if "=" in line
        )

    def test_scoring_scripts_come_from_the_worktree_under_test(self):
        wt = self.make_wt()
        self.assertEqual(self.paths(wt)["TOOLS_DIR"],
                         os.path.join(wt, "tools", "matched-bench"))

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
SURVIVOR_COUNTERS = {"matched": 5495893, "placed": 6890235,
                     "resting": 4087861, "actions": 21688}
RESTARTED_COUNTERS = {"matched": 4213249, "placed": 5269835,
                      "resting": 3121631, "actions": 16712}


def write_crash(
    d,
    gap=3,
    queue=2,
    panics=0,
    holes=0,
    restarted_pid=2002,
    replay_found=True,
    pipeline_line=True,
):
    """The crash.json run-cell.sh drops next to summary.json after a
    CRASH_KILL_AT_S cell (crash-kill.sh's record + the post-run log scan)."""
    applied = 1000
    obj = {
        "enabled": True,
        "kill_node": "val1",
        "kill_idx": 1,
        "kill_at_s": 50,
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
    with open(os.path.join(d, "crash.json"), "w") as f:
        json.dump(obj, f)


class CrashGateSummaryTest(unittest.TestCase):
    """The crash gate is the ONLY thing that can justify flipping
    TORUS_EXEC_PIPELINE on by default, so it must never read PASS for a cell
    that did not actually crash-and-replay a pipelined node."""

    ON = ["--node-env", json.dumps({"TORUS_EXEC_PIPELINE": "1"}), "--digest-quiescent", "1"]

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
            extra=["--node-env", json.dumps({"TORUS_EXEC_PIPELINE": "1"}),
                   "--digest-quiescent", "0"],
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
        write_agreement(self.d, ["same"] * 3,
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  SURVIVOR_COUNTERS])
        write_crash(self.d, gap=1, queue=1)
        s, _ = run_summarize(self.d, extra=self.ON + ["--digest-heights",
                                                      "280 280 282"])
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
        write_agreement(self.d, ["same", "forked", "same"],
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  SURVIVOR_COUNTERS])
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(any("state digest" in r for r in c["fail_reasons"]),
                        c["fail_reasons"])

    def test_a_killed_node_with_a_different_block_hash_fails(self):
        write_agreement(self.d, ["same"] * 3, hashes=["0xa", "0xb", "0xa"],
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  SURVIVOR_COUNTERS])
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(any("block hash" in r for r in c["fail_reasons"]),
                        c["fail_reasons"])

    def test_a_killed_node_with_a_different_header_root_fails(self):
        write_agreement(self.d, ["same"] * 3, roots=["0x0", "0x1", "0x0"],
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  SURVIVOR_COUNTERS])
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(any("header state root" in r for r in c["fail_reasons"]),
                        c["fail_reasons"])

    def test_survivors_that_disagree_still_fail_the_gate(self):
        """Excluding the killed node must not excuse the other two."""
        forked_survivor = dict(SURVIVOR_COUNTERS)
        forked_survivor["matched"] += 7
        write_agreement(self.d, ["same"] * 3,
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  forked_survivor])
        write_crash(self.d)
        s, _ = run_summarize(self.d, extra=self.ON)
        self.assertFalse(s["agreement"]["counters_equal"])
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertEqual(s["crash"]["verdict"], "FAIL")

    def test_crash_cell_needs_a_quiescent_digest(self):
        """An unpinned digest cannot prove the restarted node reconverged."""
        write_agreement(self.d, ["same"] * 3,
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  SURVIVOR_COUNTERS])
        write_crash(self.d)
        s, _ = run_summarize(
            self.d, drained="0",
            extra=["--node-env", json.dumps({"TORUS_EXEC_PIPELINE": "1"}),
                   "--digest-quiescent", "0"])
        c = s["crash"]
        self.assertEqual(c["verdict"], "FAIL")
        self.assertTrue(any("quiescent" in r for r in c["fail_reasons"]),
                        c["fail_reasons"])

    def test_non_crash_cell_compares_all_three_nodes(self):
        """No crash.json => nothing is excused; the old semantics exactly."""
        write_agreement(self.d, ["same"] * 3,
                        counters=[SURVIVOR_COUNTERS, RESTARTED_COUNTERS,
                                  SURVIVOR_COUNTERS])
        s, _ = run_summarize(self.d, extra=self.ON)
        a = s["agreement"]
        self.assertIsNone(a["counters_excluded_node"])
        self.assertEqual(a["counters_compared_nodes"], ["val0", "val1", "val2"])
        self.assertFalse(a["counters_equal"])
        self.assertEqual(a["agreement_verdict"], "DISAGREE")


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
            ["bash", "-c", 'source "$1"; shift; crash_scan_restart_tail "$@"', "_",
             CRASH_KILL_SH, f],
            capture_output=True, text=True,
            env=dict(os.environ, CRASH_KILL_LIB="1"), timeout=60,
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


if __name__ == "__main__":
    if not os.path.exists(DIGEST_SH):
        print("NOTE: %s missing — digest tests will fail" % DIGEST_SH)
    unittest.main(verbosity=2)
