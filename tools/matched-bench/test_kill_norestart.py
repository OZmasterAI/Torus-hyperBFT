#!/usr/bin/env python3
"""One validator down without restart (run-cell.sh KILL_NO_RESTART, s134 baseline). python3 -m unittest.

Knob off: nothing changes (no kill record, no excluded node, the same drain / summary shape). Knob on: crash-kill.sh
norestart kills the guarded target once, blanks its pids line and records it; the drain, agreement, liveness and the
state hash attestation scan run over the survivors and record the dead node as EXCLUDED; summary.json carries
kill_no_restart (killed node, kill time, height at kill, per-node timeouts/min before and after the kill) and stays
ACCEPT when the survivors agree; VALIDATOR_STAKES writes the genesis stakes and is a genesis cache key input.
Stub processes, stub HTTP servers on ephemeral 127.0.0.1 ports (never the devnet's 8645-8647 / 9161-9163).
"""

import csv
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

from health import (  # noqa: E402
    COMMITTED,
    FLUSH,
    NODES,
    DrainTracker,
    assess_liveness,
    height_target,
)
from test_harness import (  # noqa: E402
    BENCH_START,
    CRASH_KILL_SH,
    RUN_CELL_SH,
    run_summarize,
    write_cell,
)
import state_attest as sa  # noqa: E402
from test_state_attest import KEYS_JSON, Cluster, Node, h32, log_text, metrics_text  # noqa: E402

HEALTH = os.path.join(HERE, "health.py")
GEN_3VAL = os.path.join(REPO, "devnet", "wsl", "gen-3val-genesis.sh")
STOP_3VAL = os.path.join(REPO, "devnet", "wsl", "stop-3val.sh")
BASE_GENESIS = os.path.join(REPO, "devnet", "genesis.json")
DRIVER_BODY = os.path.join(HERE, "campaign", "driver-body.sh")
RUN_CELL_PY = os.path.join(HERE, "campaign", "run_cell.py")
BASE_STAKE = "1000000000000000000000000"


def lib(script, *args, env=None, timeout=30):
    """bash with crash-kill.sh's functions only"""
    return subprocess.run(
        ["bash", "-c", 'source "$1"; shift; ' + script, "_", CRASH_KILL_SH, *args],
        capture_output=True,
        text=True,
        env=dict(os.environ, CRASH_KILL_LIB="1", **(env or {})),
        timeout=timeout,
    )


def src_between(path, start, end, include_end=True):
    with open(path) as f:
        s = f.read()
    a = s.index(start)
    b = s.index(end, a)
    return s[a : b + (len(end) if include_end else 0)]


# ------------------------------------------------------------------ crash-kill.sh library
class QuorumAndPidLineTest(unittest.TestCase):
    def quorum(self, idx, stakes):
        return lib('crash_norestart_quorum_ok "$@"', str(idx), stakes).returncode == 0

    def test_equal_stakes_leave_no_quorum(self):
        """3 equal validators: the survivors hold exactly 2/3, hotstuff needs more -> refused"""
        self.assertFalse(self.quorum(2, "1,1,1"))
        self.assertFalse(self.quorum(1, "1,1,1"))

    def test_221_kill_the_stake_one_node_keeps_four_fifths(self):
        self.assertTrue(self.quorum(2, "2,2,1"))
        self.assertTrue(self.quorum(1, "2,1,2"))
        # killing a stake-2 node leaves 3/5 < 2/3
        self.assertFalse(self.quorum(1, "2,2,1"))

    def test_bad_input_is_refused(self):
        for idx, w in (
            (0, "2,2,1"),
            (3, "2,2,1"),
            ("", "2,2,1"),
            (2, "2,2"),
            (2, "0,1,1"),
            (2, "2,2,1,1"),
            (2, "a,b,c"),
            (2, ""),
        ):
            self.assertFalse(self.quorum(idx, w), (idx, w))

    def test_blanked_pid_line_keeps_positions_and_xargs_skips_it(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "pids")
            with open(p, "w") as f:
                f.write("1001\n1002\n1003\n")
            r = lib('crash_replace_pid_line "$@"', p, "2", "")
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(p) as f:
                self.assertEqual(f.read(), "1001\n1002\n\n")
            r = subprocess.run(
                ["xargs", "-a", p, "-r", "-I{}", "echo", "[{}]"],
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(r.stdout.split(), ["[1001]", "[1002]"])


class StopThreeValBlankLineTest(unittest.TestCase):
    """stop-3val.sh with a blanked line (the no-restart kill): stops the two survivors, removes the pids file"""

    def test_survivors_are_stopped_and_the_blank_line_is_skipped(self):
        d = tempfile.mkdtemp(prefix="knr-stop-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        os.makedirs(os.path.join(d, "run"))
        # two stub survivors, children of a holder shell that reaps them (a zombie still answers kill -0)
        pf = os.path.join(d, "stub-pids")
        holder = subprocess.Popen(
            ["bash", "-c", "sleep 60 & echo $! >> %s; sleep 60 & echo $! >> %s; wait" % (pf, pf)],
            start_new_session=True,
        )
        self.addCleanup(holder.wait, 10)
        self.addCleanup(lambda: os.killpg(holder.pid, signal.SIGKILL) if holder.poll() is None else None)
        for _ in range(200):
            if os.path.exists(pf) and len(open(pf).read().split()) == 2:
                break
            time.sleep(0.05)
        stubs = [int(x) for x in open(pf).read().split()]
        pids = os.path.join(d, "run", "pids")
        with open(pids, "w") as f:
            f.write("%d\n%d\n\n" % tuple(stubs))
        t0 = time.monotonic()
        r = subprocess.run(
            [STOP_3VAL],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DATA_ROOT=d),
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        for pid in stubs:  # signalled by stop-3val.sh; the holder reaps them within moments
            for _ in range(100):
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.02)
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)
        # the blank line does not keep the 10 s grace loop waiting
        self.assertLess(time.monotonic() - t0, 8)
        self.assertFalse(os.path.exists(pids))
        self.assertEqual(r.stdout.count("stopping pid "), 2, r.stdout)


# ------------------------------------------------------------------ crash-kill.sh norestart (end to end, stub node)
class MetricsStub(BaseHTTPRequestHandler):
    body = (
        "torus_block_height 1234\ntorus_blocks_committed_total 1236\ntorus_exec_queue_depth 2\n"
        "torus_flush_worker_depth 1\ntorus_orders_matched_total 777\n"
    )

    def log_message(self, *_):
        pass

    def do_GET(self):
        b = self.body.encode()
        self.send_response(200)
        self.send_header("content-length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)


class NorestartCliTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="knr-cli-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.root = os.path.join(self.d, "devnet")
        self.out = os.path.join(self.d, "cell")
        os.makedirs(os.path.join(self.root, "run"))
        os.makedirs(self.out)
        srv = ThreadingHTTPServer(("127.0.0.1", 0), MetricsStub)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        # a fake worktree: crash-kill.sh sources ITS devnet/wsl/env.sh + start-node.sh
        self.wt = os.path.join(self.d, "wt")
        wsl = os.path.join(self.wt, "devnet", "wsl")
        os.makedirs(wsl)
        with open(os.path.join(wsl, "env.sh"), "w") as f:
            f.write(
                'DATA_ROOT="${DATA_ROOT:?}"\nRUN_DIR="$DATA_ROOT/run"\nMET0=1\nMET1=1\nMET2=%d\n'
                % srv.server_address[1]
            )
        with open(os.path.join(wsl, "start-node.sh"), "w") as f:
            f.write(
                'node_var() { local v="$1$2"; printf \'%s\' "${!v-}"; }\n'
                'start_node() { echo restarted > "$DATA_ROOT/RESTARTED"; STARTED_PID=4242; }\n'
            )
        # the target: a "torus-node" with val2's devnet data dir, child of a holder shell that reaps it
        pf = os.path.join(self.d, "target.pid")
        self.holder = subprocess.Popen(
            [
                "bash",
                "-c",
                "python3 -c 'import time; time.sleep(120)' torus-node --data-dir=%s/data/val2 & "
                "echo $! > %s; wait" % (self.root, pf),
            ],
            start_new_session=True,
        )
        self.addCleanup(self.stop_holder)
        for _ in range(200):
            if os.path.exists(pf) and open(pf).read().strip():
                break
            time.sleep(0.05)
        self.target = int(open(pf).read())
        time.sleep(0.3)  # the exec'd cmdline is in /proc
        self.pids = os.path.join(self.root, "run", "pids")
        with open(self.pids, "w") as f:
            f.write("11111\n22222\n%d\n" % self.target)

    def stop_holder(self):
        try:
            os.killpg(self.holder.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.holder.wait(timeout=10)

    def run_norestart(self, idx="2", at="120"):
        return subprocess.run(
            [CRASH_KILL_SH, "norestart", self.wt, idx, self.out, at],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DATA_ROOT=self.root, KILL_WAIT="5"),
        )

    def alive(self, pid):
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False

    def test_kills_once_blanks_the_pid_line_records_and_never_restarts(self):
        r = self.run_norestart()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(
            "kill-no-restart: SIGKILL val2 pid %d (no restart; height=1234 committed=1236 "
            % self.target,
            r.stdout,
        )
        for _ in range(100):
            if not self.alive(self.target):
                break
            time.sleep(0.05)
        self.assertFalse(self.alive(self.target))
        with open(self.pids) as f:
            self.assertEqual(f.read(), "11111\n22222\n\n")
        self.assertFalse(os.path.exists(os.path.join(self.root, "RESTARTED")))
        with open(os.path.join(self.out, "kill-norestart.json")) as f:
            rec = json.load(f)
        self.assertEqual(
            (rec["kill_node"], rec["kill_idx"], rec["kill_at_s"], rec["killed_pid"]),
            ("val2", 2, 120, self.target),
        )
        self.assertEqual(rec["mode"], "no_restart")
        self.assertNotIn("restarted_pid", rec)
        self.assertEqual(rec["pre_kill"]["block_height"], 1234)
        self.assertEqual(rec["pre_kill"]["blocks_committed"], 1236)
        self.assertLessEqual(rec["kill_ts"], rec["dead_ts"])
        self.assertFalse(os.path.exists(os.path.join(self.out, "crash-kill.json")))
        self.assertIsNone(rec["run_id"])  # no run-id.json in this out dir

    def test_the_record_carries_this_runs_run_id(self):
        with open(os.path.join(self.out, "run-id.json"), "w") as f:
            json.dump({"label": "c", "run_id": "c/1700000000.1-77-5", "kill_no_restart": "val2"}, f)
        r = self.run_norestart()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(os.path.join(self.out, "kill-norestart.json")) as f:
            self.assertEqual(json.load(f)["run_id"], "c/1700000000.1-77-5")

    def test_the_guard_still_refuses_val0(self):
        r = self.run_norestart(idx="0")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("REFUSED", r.stderr)
        self.assertTrue(self.alive(self.target))
        self.assertFalse(os.path.exists(os.path.join(self.out, "kill-norestart.json")))

    def test_a_bad_kill_at_is_refused_before_anything(self):
        r = self.run_norestart(at="12x")
        self.assertEqual(r.returncode, 2)
        self.assertTrue(self.alive(self.target))


# ------------------------------------------------------------------ health.py: drain / liveness over the survivors
def sample(commits=100, flow=1000):
    from health import EXEC_QUEUE, FLOW, MEMPOOL

    return {
        COMMITTED: commits,
        MEMPOOL: 0,
        EXEC_QUEUE: 1,
        FLUSH: 0,
        **{k: flow for k in FLOW},
    }


class DrainUnitTest(unittest.TestCase):
    def test_two_node_tracker_drains_on_two_samples(self):
        tracker = DrainTracker(5, nodes=("val0", "val1"))
        out = None
        for t in range(8):
            out = tracker.observe(t, [sample(100 + t), sample(100 + t)])
        self.assertTrue(out["drained"], out)
        # three samples are not the two nodes it was built for
        self.assertFalse(
            DrainTracker(5, nodes=("val0", "val1")).observe(0, [sample()] * 3)[
                "drained"
            ]
        )

    def test_default_tracker_still_needs_three(self):
        tracker = DrainTracker(5)
        for t in range(8):
            out = tracker.observe(t, [sample(100 + t), sample(100 + t)])
        self.assertFalse(out["drained"])
        self.assertEqual(out["reason"], "missing or invalid metrics")

    def test_height_target_counts_the_given_nodes(self):
        from health import EXEC_HEIGHT, EXEC_QUEUE

        s = [dict(sample(), **{EXEC_HEIGHT: 500, EXEC_QUEUE: 2})] * 2
        self.assertIsNone(height_target(s))
        self.assertEqual(height_target(s, 2), 502)

    def test_liveness_over_the_survivors_ignores_the_dead_node(self):
        rows = {n: [dict(sample(t), ts=t) for t in range(101)] for n in NODES}
        rows["val2"] = rows["val2"][:40]  # killed at t=40: no valid scrape after it
        self.assertEqual(assess_liveness(rows, 0, 100)["verdict"], "UNKNOWN")
        live = assess_liveness(rows, 0, 100, nodes=("val0", "val1"))
        self.assertEqual(live["verdict"], "PASS")
        self.assertEqual(sorted(live["nodes"]), ["val0", "val1"])


class Advancing(BaseHTTPRequestHandler):
    counts = {}

    def log_message(self, *_):
        pass

    def do_GET(self):
        c = self.counts[self.path] = self.counts.get(self.path, 100) + 1
        b = "".join("%s %s\n" % kv for kv in sample(c).items()).encode()
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b)


class DrainExcludeHttpTest(unittest.TestCase):
    def setUp(self):
        srv = ThreadingHTTPServer(("127.0.0.1", 0), Advancing)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        self.base = "http://127.0.0.1:%d" % srv.server_address[1]
        dead = socket_closed_port()
        self.urls = [
            "%s/val0" % self.base,
            "%s/val1" % self.base,
            "http://127.0.0.1:%d/val2" % dead,
        ]
        self.d = tempfile.mkdtemp(prefix="knr-drain-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def drain(self, urls, extra=()):
        r = subprocess.run(
            [
                sys.executable,
                HEALTH,
                "drain",
                "--out",
                self.d,
                "--timeout",
                "4",
                "--quiet",
                "1",
                "--urls",
                *urls,
                *extra,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        return r, json.loads((Path(self.d) / "drain.json").read_text())

    def test_a_dead_node_never_drains_unless_excluded(self):
        r, d = self.drain(self.urls)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertFalse(d["drained"])
        r, d = self.drain(self.urls, ["--exclude", "val2"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(d["drained"])
        self.assertEqual(d["excluded_nodes"], ["val2"])
        self.assertEqual(d["nodes"], ["val0", "val1"])

    def test_knob_off_drain_json_has_no_exclusion_keys(self):
        r, d = self.drain(["%s/val%d" % (self.base, i) for i in range(3)])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("excluded_nodes", d)
        self.assertNotIn("nodes", d)

    def test_exclude_is_val1_or_val2_once(self):
        for extra in (
            ["--exclude", "val0"],
            ["--exclude", "val1", "--exclude", "val2"],
        ):
            r = subprocess.run(
                [
                    sys.executable,
                    HEALTH,
                    "drain",
                    "--out",
                    self.d,
                    "--timeout",
                    "1",
                    "--urls",
                    *self.urls,
                    *extra,
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(r.returncode, 2, (extra, r.stderr))


def socket_closed_port():
    import socket

    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


# ------------------------------------------------------------------ summarize.py
KILL_AT = 60
RUN_ID = "t/1700000000.000000000-4242-17"


def write_run_identity(d, run_id=RUN_ID, knob="val2"):
    """run-cell.sh reset_run_artifacts' run-id.json (this run's identity)"""
    with open(os.path.join(d, "run-id.json"), "w") as f:
        json.dump({"label": "t", "run_id": run_id, "kill_no_restart": knob}, f)


def write_kill_record(d, run_id=RUN_ID, kill_node="val2", kill_ts=None):
    """crash-kill.sh norestart's kill-norestart.json"""
    with open(os.path.join(d, "kill-norestart.json"), "w") as f:
        json.dump(
            {
                "enabled": True,
                "mode": "no_restart",
                "run_id": run_id,
                "kill_node": kill_node,
                "kill_idx": int(kill_node[-1]),
                "kill_at_s": KILL_AT,
                "killed_pid": 1003,
                "kill_ts": BENCH_START + KILL_AT + 0.5 if kill_ts is None else kill_ts,
                "dead_ts": BENCH_START + KILL_AT + 0.6,
                "pre_kill": {
                    "block_height": 600,
                    "blocks_committed": 602,
                    "exec_queue_depth": 2,
                    "flush_worker_depth": 0,
                    "matched": 60000,
                    "log_bytes": 4096,
                },
            },
            f,
        )


def rewrite_sampler(d, col, fn, nodes=("val0", "val1")):
    """sampler.csv column `col` := fn(seconds into the bench) on `nodes`"""
    path = Path(d) / "sampler.csv"
    with path.open() as f:
        reader = csv.DictReader(f)
        fields = reader.fieldnames
        data = list(reader)
    for row in data:
        if row["node"] in nodes:
            row[col] = str(fn(int(float(row["ts"])) - BENCH_START))
    with path.open("w") as f:
        w = csv.DictWriter(f, fields)
        w.writeheader()
        w.writerows(data)


def write_kill_cell(d, killed=True, agree_rows=("val0", "val1"), digests=None, kill_ts=None):
    """a 120 s cell: every node commits 10 blocks/s; the survivors' timeout counter is flat before the kill and
    climbs 0.5/s after it; val2 (killed) scrapes fail from KILL_AT on (all-zero rows, scrape_valid=0).
    run-id.json names this run (KILL_NO_RESTART=val2 when killed, unset otherwise); the record carries its run_id."""
    write_cell(d, 1000, 1000, dur=120)
    write_run_identity(d, knob="val2" if killed else None)
    path = Path(d) / "sampler.csv"
    with path.open() as f:
        reader = csv.DictReader(f)
        fields = reader.fieldnames + [
            FLUSH,
            "torus_consensus_timeout_total_total",
            "torus_consensus_view",
            "scrape_valid",
        ]
        data = list(reader)
    for row in data:
        s = int(row["ts"]) - BENCH_START
        row[FLUSH] = "0"
        row["torus_consensus_timeout_total_total"] = (
            str(int(0.5 * max(0, s - KILL_AT))) if killed else "0"
        )
        row["torus_consensus_view"] = str(12 * s)
        row["scrape_valid"] = "1"
        if killed and row["node"] == "val2" and s > KILL_AT:
            for k in fields[2:]:
                row[k] = "0"
    with path.open("w") as f:
        w = csv.DictWriter(f, fields)
        w.writeheader()
        w.writerows(data)
    digests = digests or {n: "same" for n in agree_rows}
    with open(os.path.join(d, "agreement.jsonl"), "w") as f:
        for i, n in enumerate(agree_rows):
            f.write(
                json.dumps(
                    {
                        "node": n,
                        "height": 1200 + i,
                        "cmp_height": 1195,
                        "block_hash": "0xdead",
                        "header_state_root": "0x0",
                        "state_digest": digests[n],
                        "matched": 100,
                        "placed": 200,
                        "resting": 5,
                        "actions": 100,
                        "panic_or_failstop_lines": 0,
                        "error_lines": 0,
                    }
                )
                + "\n"
            )
    if killed:
        write_kill_record(d, kill_ts=kill_ts)


CLEAN_DISSEM = [
    "--dissem",
    " ".join(
        "%s:exhausted=0,sync_fallback=0,da_outbound_fail=0,starvation=0" % n
        for n in NODES
    ),
]


class SummarizeKillTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="knr-summ-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def summ(self, heights="1200 1201"):
        return run_summarize(
            self.d,
            dur=120,
            extra=CLEAN_DISSEM
            + ["--digest-quiescent", "1", "--digest-heights", heights],
        )

    def test_survivors_agree_the_dead_node_is_excluded_and_the_cell_accepts(self):
        write_kill_cell(self.d)
        s, out = self.summ()
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        a = s["agreement"]
        self.assertEqual(a["agreement_verdict"], "AGREE")
        self.assertEqual(a["excluded_nodes"], ["val2"])
        self.assertEqual([r["node"] for r in a["nodes"]], ["val0", "val1"])
        self.assertEqual(s["liveness"]["verdict"], "PASS")
        self.assertEqual(s["liveness"]["excluded_nodes"], ["val2"])
        k = s["kill_no_restart"]
        self.assertEqual(
            (
                k["killed_node"],
                k["kill_at_s"],
                k["height_at_kill"],
                k["committed_at_kill"],
            ),
            ("val2", KILL_AT, 600, 602),
        )
        self.assertEqual(k["survivors"], ["val0", "val1"])
        self.assertEqual(k["kill_ts"], BENCH_START + KILL_AT + 0.5)
        self.assertIn("torus_consensus_timeout_total_total", k["timeouts_column"])
        v0 = k["per_node"]["val0"]
        self.assertEqual(v0["before"]["timeouts_per_min"], 0.0)
        self.assertAlmostEqual(v0["after"]["timeouts_per_min"], 30.0, delta=1.5)
        self.assertEqual(v0["before"]["blk_s"], 10.0)
        self.assertNotIn("after", k["per_node"]["val2"])  # dead: before only
        self.assertEqual(s["headline"]["killed_node"], "val2")
        self.assertEqual(s["headline"]["height_at_kill"], 600)
        # the dead node's failed scrapes are dropped: no counter "reset" to 0 in its funnel
        self.assertGreaterEqual(
            s["funnel_by_node"]["val2"]["delta_orders_matched_total"], 0
        )
        self.assertIn("KILL-NO-RESTART val2: at bench+60s height=600", out)

    def test_a_dead_node_row_in_agreement_is_dropped_not_a_disagreement(self):
        write_kill_cell(
            self.d,
            agree_rows=("val0", "val1", "val2"),
            digests={"val0": "same", "val1": "same", "val2": "ERR"},
        )
        s, _ = self.summ()
        self.assertEqual(s["agreement"]["agreement_verdict"], "AGREE")
        self.assertEqual(s["validity"]["verdict"], "ACCEPT")

    def test_survivors_that_disagree_still_reject(self):
        write_kill_cell(self.d, digests={"val0": "aaa", "val1": "bbb"})
        s, _ = self.summ()
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assertEqual(s["validity"]["verdict"], "REJECT")

    def test_without_the_kill_record_a_missing_node_is_incomplete(self):
        write_kill_cell(self.d)
        os.remove(os.path.join(self.d, "kill-norestart.json"))
        s, _ = self.summ()
        self.assertEqual(s["agreement"]["agreement_verdict"], "INCOMPLETE")
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertNotIn("kill_no_restart", s)

    def test_knob_off_cell_is_unchanged(self):
        write_kill_cell(self.d, killed=False, agree_rows=("val0", "val1", "val2"))
        s, out = self.summ("1200 1201 1202")
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertNotIn("kill_no_restart", s)
        self.assertNotIn("excluded_nodes", s["agreement"])
        self.assertNotIn("excluded_reason", s["agreement"])
        self.assertNotIn("excluded_nodes", s["liveness"])
        self.assertNotIn("killed_node", s["headline"])
        self.assertEqual(sorted(s["liveness"]["nodes"]), ["val0", "val1", "val2"])
        self.assertNotIn("KILL-NO-RESTART", out)
        self.assertNotIn("kill_record_check", s)

    # ---- Codex P2a: a kill record applies only to the run that wrote it
    def assert_not_excluded(self, s):
        self.assertNotIn("kill_no_restart", s)
        self.assertNotIn("excluded_nodes", s["agreement"])
        self.assertNotIn("excluded_nodes", s["liveness"])
        self.assertNotIn("killed_node", s["headline"])

    def test_stale_record_in_an_all_up_rerun_is_ignored_never_excludes(self):
        """the probe: OVERWRITE=1 all-up rerun over a dir that kept an old kill record (fresh=REJECT stale=ACCEPT
        excluded=['val2']). The live val2's divergent digest must still be scored: DISAGREE -> REJECT."""
        write_kill_cell(
            self.d,
            killed=False,
            agree_rows=("val0", "val1", "val2"),
            digests={"val0": "same", "val1": "same", "val2": "divergent"},
        )
        write_kill_record(self.d, run_id="t/an-earlier-run")
        s, out = self.summ("1200 1201 1202")
        self.assertNotEqual(s["validity"]["verdict"], "ACCEPT")
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertEqual(s["agreement"]["agreement_verdict"], "DISAGREE")
        self.assert_not_excluded(s)
        c = s["kill_record_check"]
        self.assertEqual(c["action"], "ignored")
        self.assertEqual((c["record_run_id"], c["run_id"]), ("t/an-earlier-run", RUN_ID))
        self.assertIn("did not set KILL_NO_RESTART", c["reason"])
        self.assertNotIn("KILL-NO-RESTART", out)

    def test_stale_record_in_an_all_up_rerun_with_agreeing_nodes_still_judges_all_three(self):
        write_kill_cell(self.d, killed=False, agree_rows=("val0", "val1", "val2"))
        write_kill_record(self.d, run_id="t/an-earlier-run")
        s, _ = self.summ("1200 1201 1202")
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertEqual(sorted(s["liveness"]["nodes"]), ["val0", "val1", "val2"])
        self.assert_not_excluded(s)
        self.assertEqual(s["kill_record_check"]["action"], "ignored")

    def test_record_from_another_run_rejects_without_excluding(self):
        write_kill_cell(self.d)
        write_kill_record(self.d, run_id="t/an-earlier-run")
        s, _ = self.summ()
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertTrue(
            any("stale kill record" in r for r in s["validity"]["fail_reasons"]),
            s["validity"],
        )
        self.assert_not_excluded(s)
        self.assertEqual(s["kill_record_check"]["action"], "rejected")
        self.assertEqual(s["status"], "INVALID")

    def test_record_naming_another_node_than_the_knob_rejects(self):
        write_kill_cell(self.d)
        write_run_identity(self.d, knob="val1")
        s, _ = self.summ()
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertIn("stale kill record", s["kill_record_check"]["reason"])
        self.assert_not_excluded(s)

    def test_record_without_a_run_identity_rejects(self):
        write_kill_cell(self.d)
        os.remove(os.path.join(self.d, "run-id.json"))
        s, _ = self.summ()
        self.assertEqual(s["validity"]["verdict"], "REJECT")
        self.assertEqual(s["kill_record_check"]["action"], "rejected")
        self.assertIn("no run-id.json", s["kill_record_check"]["reason"])
        self.assert_not_excluded(s)

    # ---- Codex P2b: one shared boundary sample between 'before' and 'after'
    def test_boundary_increment_between_the_last_pre_and_first_post_kill_scrape_counts_in_after(self):
        """the probe: one timeout and the kill's order-age observations land between the scrape at kill-0.5 s and
        the one at kill+0.5 s (timeouts_total=1.0 before/min=0.0 after/min=0.0; order_age before=None after=None)"""
        write_kill_cell(self.d)
        rewrite_sampler(
            self.d, "torus_consensus_timeout_total_total", lambda s: 1 if s > KILL_AT else 0
        )
        metric = "torus_order_age_admit_seconds_bucket"
        with open(os.path.join(self.d, "buckets.csv"), "w") as f:
            f.write("ts,node,metric,le,count\n")
            for s in range(121):
                post = s > KILL_AT
                for le, n in (("0.05", 4), ("0.1", 10), ("+Inf", 10)):
                    f.write("%d,val0,%s,%s,%d\n" % (BENCH_START + s, metric, le, n if post else 0))
        s, _ = self.summ()
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        v0 = s["kill_no_restart"]["per_node"]["val0"]
        self.assertEqual(v0["before"]["timeouts_per_min"], 0.0)
        self.assertEqual(v0["after"]["timeouts_per_min"], 1.0)  # 1 timeout over the 60 s after window
        self.assertEqual(v0["before"]["sampled"]["last_ts"], v0["after"]["sampled"]["first_ts"])
        self.assertEqual(v0["after"]["sampled"]["first_ts"], BENCH_START + KILL_AT)
        self.assertIsNone(v0["before"]["order_age_ms"]["admit"])  # nothing observed before the kill
        admit = v0["after"]["order_age_ms"]["admit"]
        self.assertEqual(admit["n"], 10)
        self.assertIsNotNone(admit["p50"])
        self.assertEqual(
            v0["before"]["order_age_sampled"]["last_ts"], v0["after"]["order_age_sampled"]["first_ts"]
        )
        self.assertIn("shared", s["kill_no_restart"]["boundary"])

    def test_windows_report_the_spans_they_actually_sampled(self):
        write_kill_cell(self.d)
        s, _ = self.summ()
        k = s["kill_no_restart"]
        v0 = k["per_node"]["val0"]
        self.assertEqual(
            v0["before"]["sampled"],
            {"samples": 61, "first_ts": BENCH_START, "last_ts": BENCH_START + KILL_AT, "span_s": 60.0},
        )
        self.assertEqual(
            v0["after"]["sampled"],
            {"samples": 61, "first_ts": BENCH_START + KILL_AT, "last_ts": BENCH_START + 120, "span_s": 60.0},
        )
        self.assertEqual(v0["before"]["span_s"], 60.0)
        # the dead node: before only, ending at its last valid scrape at or before the kill
        v2 = k["per_node"]["val2"]
        self.assertEqual(v2["before"]["sampled"]["last_ts"], BENCH_START + KILL_AT)
        self.assertNotIn("after", v2)
        # no buckets.csv in this cell: order age is None with a reason, not an empty or zero dict
        self.assertIsNone(v0["after"]["order_age_ms"])
        self.assertEqual(v0["after"]["order_age_sampled"]["samples"], 0)
        self.assertIn("buckets.csv", v0["after"]["order_age_reason"])
        self.assertEqual(k["windows"]["before"], [BENCH_START, BENCH_START + KILL_AT + 0.5])

    def test_a_window_with_fewer_than_two_samples_is_none_with_a_reason_not_zero(self):
        write_kill_cell(self.d, kill_ts=BENCH_START + 0.5)  # only the t0 scrape precedes the kill
        s, _ = self.summ()
        v0 = s["kill_no_restart"]["per_node"]["val0"]
        b = v0["before"]
        self.assertEqual(b["sampled"]["samples"], 1)
        for key in ("blk_s", "committed_blk_s", "matched_s", "timeouts_per_min", "views_per_min"):
            self.assertIsNone(b[key], key)
        self.assertIn(">= 2", b["reason"])
        self.assertEqual(v0["after"]["sampled"]["first_ts"], BENCH_START)  # the shared boundary
        self.assertIsNotNone(v0["after"]["blk_s"])


# ------------------------------------------------------------------ state_attest.py scan --exclude
class AttestExcludeTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="knr-attest-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        self.keys = sa.read_keys(KEYS_JSON)
        self.addrs = [a for _, a, _ in self.keys]

    def cps(self, local=lambda h: h32("%x" % (h // 100 % 16))):
        """checkpoints 100..1200; val2 voted up to 600, then was killed; val0+val1 (4/5) reach every quorum"""
        out = {}
        for h in range(100, 1201, 100):
            voters = self.addrs if h <= 600 else self.addrs[:2]
            out[h] = (local(h), {a: local(h) for a in voters}, local(h))
        return out

    def scan(self, nodes, excluded=()):
        logs = []
        for i, a in enumerate(self.addrs):
            p = os.path.join(self.d, "val%d.log" % i)
            with open(p, "w") as f:
                f.write(log_text(a))
            logs.append(p)
        c = Cluster(self, nodes, logs)
        dead = "http://127.0.0.1:%d" % socket_closed_port()
        rpcs = [sa.Rpc(u, timeout=2) for u in c.rpcs] + [sa.Rpc(dead, timeout=1)]
        return sa.scan(
            rpcs,
            self.keys,
            c.mets + [dead + "/metrics"],
            c.logs,
            0.2,
            sleep=0.05,
            excluded=excluded,
        )

    def test_survivors_only_and_the_dead_validators_votes_not_required(self):
        out = self.scan(
            [Node(self.cps(), metrics_text()) for _ in range(2)], excluded=("val2",)
        )
        self.assertTrue(out["ok"], out["problems"])
        self.assertEqual(out["nodes"], ["val0", "val1"])
        self.assertEqual(out["excluded_nodes"], ["val2"])
        self.assertEqual(
            out["votes_per_validator"], {"val0": 12, "val1": 12, "val2": 6}
        )
        self.assertEqual(sorted(out["per_node"]), ["val0", "val1"])
        self.assertIn("on 2 nodes", sa.line_of(out))
        self.assertEqual(sa.compact(out)["excluded_nodes"], ["val2"])

    def test_without_exclude_the_dead_node_fails_the_scan(self):
        out = self.scan([Node(self.cps(), metrics_text()) for _ in range(2)])
        self.assertFalse(out["ok"])
        self.assertTrue(
            any(
                p.startswith("val2: no retained checkpoint readable")
                for p in out["problems"]
            )
        )
        self.assertNotIn("excluded_nodes", out)
        self.assertNotIn("excluded_nodes", sa.compact(out))

    def test_a_survivor_that_differs_still_fails(self):
        bad = self.cps(
            local=lambda h: h32("e") if h == 900 else h32("%x" % (h // 100 % 16))
        )
        out = self.scan(
            [Node(self.cps(), metrics_text()), Node(bad, metrics_text())],
            excluded=("val2",),
        )
        self.assertFalse(out["ok"])
        self.assertTrue(
            any("h_n differs" in p for p in out["problems"]), out["problems"]
        )

    def test_cli_takes_exclude(self):
        c = Cluster(self, [Node(self.cps(), metrics_text()) for _ in range(2)], [])
        dead = "http://127.0.0.1:%d" % socket_closed_port()
        out = os.path.join(self.d, "state-attest.json")
        rc = sa.main(
            [
                "scan",
                "--rpc",
                c.rpcs[0],
                "--rpc",
                c.rpcs[1],
                "--rpc",
                dead,
                "--keys",
                KEYS_JSON,
                "--wait-s",
                "0.2",
                "--exclude",
                "val2",
                "--out",
                out,
            ]
        )
        self.assertEqual(rc, 0)
        with open(out) as f:
            self.assertTrue(json.load(f)["ok"])


# ------------------------------------------------------------------ run-cell.sh pieces
class RunCellKnobTest(unittest.TestCase):
    PRE_START = "# ---- no-restart kill (KILL_NO_RESTART) pre-flight."
    PRE_END = '[ -z "$KNR_IDX" ] || KNR_EXCLUDE_ARGS=(--exclude "$KILL_NO_RESTART")\n'

    def setUp(self):
        self.pre = src_between(RUN_CELL_SH, self.PRE_START, self.PRE_END)
        self.urls_fn = src_between(RUN_CELL_SH, "bench_rpc_urls() {", "\n}\n")

    def preflight(self, **env):
        script = (
            "set -uo pipefail\n"
            + self.urls_fn
            + "RPCS=(http://a:1 http://b:2 http://c:3)\nBENCH_RPC_URLS=$(bench_rpc_urls) || exit 2\n"
            + 'evm_load_on() { [ "${EVM_ON:-0}" = 1 ]; }\n'
            + "DUR=${DUR:-600}; CRASH_KILL_AT_S=${CRASH_KILL_AT_S:-}; ORACLE_FEED=${ORACLE_FEED:-0}\n"
            + "KILL_NO_RESTART=${KILL_NO_RESTART:-}; KILL_NO_RESTART_AT_S=${KILL_NO_RESTART_AT_S:-}\n"
            + "VALIDATOR_STAKES=${VALIDATOR_STAKES:-}; TOOLS_FROM_WORKTREE=1\n"
            + self.pre
            + 'echo "LIVE=${LIVE_IDX[*]}"; echo "LIVE_RPCS=${LIVE_RPCS[*]}"; '
            'echo "EXCL=${KNR_EXCLUDE_ARGS[*]+${KNR_EXCLUDE_ARGS[*]}}"; echo "KNR_IDX=$KNR_IDX"\n'
        )
        base = {
            k: v
            for k, v in os.environ.items()
            if k
            not in (
                "KILL_NO_RESTART",
                "KILL_NO_RESTART_AT_S",
                "VALIDATOR_STAKES",
                "BENCH_RPCS",
                "CRASH_KILL_AT_S",
                "ORACLE_FEED",
                "DUR",
            )
        }
        r = subprocess.run(
            ["bash", "-c", script],
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(base, TOOLS_DIR=HERE, **env),
        )
        vals = dict(l.split("=", 1) for l in r.stdout.splitlines() if "=" in l)
        return r, vals

    ON = dict(
        KILL_NO_RESTART="val2",
        KILL_NO_RESTART_AT_S="120",
        VALIDATOR_STAKES="2,2,1",
        BENCH_RPCS="0,1",
    )

    def test_off_keeps_all_three_nodes_and_no_exclude(self):
        r, v = self.preflight()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(v["LIVE"], "0 1 2")
        self.assertEqual(v["LIVE_RPCS"], "http://a:1 http://b:2 http://c:3")
        self.assertEqual(v["EXCL"], "")
        self.assertEqual(v["KNR_IDX"], "")

    def test_on_leaves_the_survivors_and_excludes_the_killed_node(self):
        r, v = self.preflight(**self.ON)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(v["LIVE"], "0 1")
        self.assertEqual(v["LIVE_RPCS"], "http://a:1 http://b:2")
        self.assertEqual(v["EXCL"], "--exclude val2")
        r, v = self.preflight(
            **dict(
                self.ON,
                KILL_NO_RESTART="val1",
                VALIDATOR_STAKES="2,1,2",
                BENCH_RPCS="0,2",
            )
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((v["LIVE"], v["EXCL"]), ("0 2", "--exclude val1"))

    def test_refusals(self):
        cases = {
            "equal stakes halt the chain": dict(self.ON, VALIDATOR_STAKES=""),
            "a stake-2 node down leaves 3/5": dict(
                self.ON, KILL_NO_RESTART="val1", BENCH_RPCS="0,2"
            ),
            "bench sends to the killed node": dict(self.ON, BENCH_RPCS="all"),
            "never val0": dict(self.ON, KILL_NO_RESTART="val0"),
            "kill time without a node": dict(KILL_NO_RESTART_AT_S="120"),
            "node without a kill time": dict(
                KILL_NO_RESTART="val2", VALIDATOR_STAKES="2,2,1", BENCH_RPCS="0,1"
            ),
            "inside the load window only": dict(self.ON, KILL_NO_RESTART_AT_S="590"),
            "not with the crash gate": dict(self.ON, CRASH_KILL_AT_S="60"),
            "not with an oracle feed": dict(self.ON, ORACLE_FEED="1"),
            "not with EVM load": dict(self.ON, EVM_ON="1"),
        }
        for what, env in cases.items():
            with self.subTest(what):
                r, _ = self.preflight(**env)
                self.assertEqual(r.returncode, 2, (what, r.stdout, r.stderr))
                self.assertIn("FATAL", r.stderr)

    def test_bench_rpc_lists(self):
        def urls(v):
            r = subprocess.run(
                [
                    "bash",
                    "-c",
                    self.urls_fn
                    + "\nRPCS=(http://a:1 http://b:2 http://c:3); bench_rpc_urls",
                ],
                capture_output=True,
                text=True,
                timeout=30,
                env=dict(os.environ, BENCH_RPCS=v),
            )
            return r.returncode, r.stdout.strip()

        self.assertEqual(urls("0,1"), (0, "http://a:1,http://b:2"))
        self.assertEqual(urls("2,0"), (0, "http://c:3,http://a:1"))
        self.assertEqual(urls("0"), (0, "http://a:1"))
        for bad in ("0,0", "3", "0,1,2,0", "1,", "val1"):
            self.assertNotEqual(urls(bad)[0], 0, bad)

    def test_wiring(self):
        """every post-load step reads LIVE_IDX / LIVE_RPCS / KNR_EXCLUDE_ARGS; the knobs are run-cell knobs"""
        with open(RUN_CELL_SH) as f:
            src = f.read()
        head = src[: src.index("set -uo pipefail")]
        for knob in (
            "#   KILL_NO_RESTART=val1|val2  KILL_NO_RESTART_AT_S=N",
            "#   VALIDATOR_STAKES=a,b,c",
        ):
            self.assertIn(knob, head)
        for needle in (
            'for i in "${LIVE_IDX[@]}"; do scrape_one "${METS[$i]}" > "$OUT/metrics-after-val$i.txt"; done',
            'agree_cmp_height "$HCMP" "${LIVE_RPCS[@]}"',
            '--rpc "${LIVE_RPCS[@]}"',
            '${KNR_EXCLUDE_ARGS[@]+"${KNR_EXCLUDE_ARGS[@]}"} >>"$OUT/run.log" 2>&1; then DRAINED=1; fi',
            '--param VALIDATOR_STAKES="$VALIDATOR_STAKES"',
            "VALIDATOR_STAKES=$VALIDATOR_STAKES MARKETS=$MARKETS",
            '"$TOOLS_DIR/crash-kill.sh" norestart "$WT" "$KNR_IDX" "$OUT" "$KILL_NO_RESTART_AT_S"',
            'die "no-restart kill did not complete (no kill-norestart.json)',
        ):
            self.assertIn(needle, src)
        self.assertEqual(
            src.count("${KNR_EXCLUDE_ARGS[@]+"), 3
        )  # drain, settle, attest scan
        with open(RUN_CELL_PY) as f:
            rc = f.read()
        for k in ("KILL_NO_RESTART", "KILL_NO_RESTART_AT_S", "VALIDATOR_STAKES"):
            self.assertIn('"%s",' % k, rc)
        with open(DRIVER_BODY) as f:
            self.assertIn('grep -qF "kill-no-restart: SIGKILL val$i pid $p "', f.read())


# ------------------------------------------------------------------ run-cell.sh run identity (Codex P2a)
class RunIdentityTest(unittest.TestCase):
    STALE = (
        "kill-norestart.json",
        "crash.json",
        "crash-2.json",
        "crash-kill.json",
        "crash-kill-2.json",
        "crash-kill.skipped",
        "crash-restart-tail.log",
        "crash-restart-tail-2.log",
    )

    def setUp(self):
        self.fn = src_between(RUN_CELL_SH, "reset_run_artifacts() {", "\n}\n")
        self.d = tempfile.mkdtemp(prefix="knr-runid-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def reset(self, label="cell-a", knr=""):
        r = subprocess.run(
            ["bash", "-c", "set -uo pipefail\n" + self.fn + '\nreset_run_artifacts "$1" "$2" "$3"',
             "_", self.d, label, knr],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(self.d, "run-id.json")) as f:
            return json.load(f)

    def test_clears_every_kill_artifact_and_keeps_the_rest(self):
        keep = ("sampler.csv", "run.log", "summary.json")
        for n in self.STALE + keep:
            with open(os.path.join(self.d, n), "w") as f:
                f.write("{}")
        ident = self.reset("cell-a", "val2")
        for n in self.STALE:
            self.assertFalse(os.path.exists(os.path.join(self.d, n)), n)
        for n in keep:
            self.assertTrue(os.path.exists(os.path.join(self.d, n)), n)
        self.assertEqual(ident["label"], "cell-a")
        self.assertEqual(ident["kill_no_restart"], "val2")
        self.assertTrue(ident["run_id"].startswith("cell-a/"), ident)

    def test_every_run_gets_a_new_run_id_and_an_off_knob_is_null(self):
        a = self.reset()
        b = self.reset()
        self.assertNotEqual(a["run_id"], b["run_id"])
        self.assertIsNone(a["kill_no_restart"])

    def test_wiring(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        i_out = src.index('mkdir -p "$OUT"\n: > "$OUT/run.log"\n')
        i_reset = src.index('reset_run_artifacts "$OUT" "$LABEL" "$KILL_NO_RESTART"')
        i_kill = src.index('"$TOOLS_DIR/crash-kill.sh" norestart')
        self.assertLess(i_out, i_reset)
        self.assertLess(i_reset, i_kill)
        with open(CRASH_KILL_SH) as f:
            self.assertIn('"$OUT/$REC" "$OUT/run-id.json"', f.read())


# ------------------------------------------------------------------ VALIDATOR_STAKES: genesis + cache key
class ValidatorStakesTest(unittest.TestCase):
    def patch(self, stakes):
        check = src_between(
            GEN_3VAL, 'if [ -n "${VALIDATOR_STAKES:-}" ]; then\n    [[', "\nfi\n"
        )
        block = src_between(GEN_3VAL, "# 2b. Optional stake weights", "\nfi\n")
        script = (
            "set -euo pipefail\n"
            + check
            + "VALS=$(jq '[.validators[0:3][]]' \"$1\")\n"
            + block
            + "jq -c '[.[].stake]' <<<\"$VALS\"\n"
        )
        env = {k: v for k, v in os.environ.items() if k != "VALIDATOR_STAKES"}
        if stakes is not None:
            env["VALIDATOR_STAKES"] = stakes
        r = subprocess.run(
            ["bash", "-c", script, "_", BASE_GENESIS],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
        return r.returncode, (json.loads(r.stdout) if r.returncode == 0 else r.stderr)

    def test_weights_multiply_the_base_stake(self):
        self.assertEqual(
            self.patch("2,2,1"),
            (0, ["2" + BASE_STAKE[1:], "2" + BASE_STAKE[1:], BASE_STAKE]),
        )
        self.assertEqual(self.patch("12,1,3")[1][0], "12" + BASE_STAKE[1:])

    def test_unset_leaves_the_base_stakes(self):
        self.assertEqual(self.patch(None), (0, [BASE_STAKE] * 3))
        self.assertEqual(self.patch(""), (0, [BASE_STAKE] * 3))

    def test_bad_weights_are_fatal(self):
        for bad in ("2,2", "0,1,1", "2,2,1,1", "a,b,c", "2.5,1,1"):
            rc, err = self.patch(bad)
            self.assertNotEqual(rc, 0, bad)
            self.assertIn("FATAL", err)

    def test_genesis_cache_keys_on_it(self):
        spec = importlib.util.spec_from_file_location(
            "genesis_cache", os.path.join(HERE, "genesis_cache.py")
        )
        gc = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gc)
        self.assertIn("VALIDATOR_STAKES", gc.PARAMS)
        params = {p: "" for p in gc.PARAMS}
        params.update(
            MARKETS="10", BULK_COUNT="100000", LIQ_THIN="0", LIQ_THIN_AVAIL="1000000.0"
        )
        with tempfile.TemporaryDirectory() as root:
            rc = os.path.join(root, "run-cell.sh")
            with open(rc, "w") as f:
                f.write("#!/bin/bash\n")
            k0 = gc.key_of(gc.key_inputs(root, rc, params, {}))
            k1 = gc.key_of(
                gc.key_inputs(root, rc, dict(params, VALIDATOR_STAKES="2,2,1"), {})
            )
            k2 = gc.key_of(
                gc.key_inputs(root, rc, dict(params, VALIDATOR_STAKES="2,1,2"), {})
            )
        self.assertEqual(len({k0, k1, k2}), 3)


if __name__ == "__main__":
    unittest.main()
