#!/usr/bin/env python3
"""The 4-validator devnet (run-cell.sh N_VALIDATORS=4, dead-leader bench 9p) and the dead-leader window report
(dl_window.py). python3 -m unittest.

N_VALIDATORS unset or 3: every piece behaves as before (3 nodes, the same argv, ports, genesis, messages). 4: val3
(key 04, ports 30404 / 8648 / 9164, dials val0..val2) is launched, genesis-registered with the keys-4val account,
sampled, drained, attested, scored and may be the KILL_NO_RESTART target; DL_K / DL_JCAP land in the genesis only
when set. Stub processes and stub HTTP servers on ephemeral 127.0.0.1 ports only.
"""

import csv
import gzip
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from datetime import datetime, timezone
from http.server import ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

from test_harness import BENCH_START, RUN_CELL_SH, run_summarize, sandbox_repo  # noqa: E402
from test_kill_norestart import (  # noqa: E402
    KILL_AT,
    RUN_ID,
    Advancing,
    lib,
    src_between,
    write_kill_record,
    write_run_identity,
)
import dl_window  # noqa: E402
import state_attest as sa  # noqa: E402

WSL = os.path.join(REPO, "devnet", "wsl")
KEYS3 = os.path.join(WSL, "bench-validator-keys.json")
KEYS4 = os.path.join(WSL, "keys-4val", "bench-validator-keys.json")
SIGNERS4 = os.path.join(WSL, "keys-4val", "bench-oracle-signer-keys.json")
HEALTH = os.path.join(HERE, "health.py")
BASE_STAKE = "1000000000000000000000000"


def bash(script, *args, env=None, cwd=None, timeout=60):
    base = {
        k: v
        for k, v in os.environ.items()
        if k not in ("N_VALIDATORS", "DL_K", "DL_JCAP", "VALIDATOR_STAKES")
    }
    return subprocess.run(
        ["bash", "-c", script, "_", *args],
        capture_output=True,
        text=True,
        env=dict(base, **(env or {})),
        cwd=cwd,
        timeout=timeout,
    )


# ------------------------------------------------------------------ devnet/wsl: env, peers, launch
class DevnetTopologyTest(unittest.TestCase):
    def env(self, n=None):
        e = {} if n is None else {"N_VALIDATORS": n}
        return bash(
            'source "$1" && echo "PORTS=$METRICS_PORTS" && echo "RPCS=$RPC_URLS" && echo "N=$N_VALIDATORS"',
            os.path.join(WSL, "env.sh"),
            env=e,
        )

    def test_default_is_three_nodes_unchanged(self):
        r = self.env()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("PORTS=9161 9162 9163\n", r.stdout)
        self.assertIn(
            "RPCS=http://localhost:8645 http://localhost:8646 http://localhost:8647\n",
            r.stdout,
        )
        self.assertIn("N=3\n", r.stdout)

    def test_four_adds_val3_and_five_is_refused(self):
        r = self.env("4")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("PORTS=9161 9162 9163 9164\n", r.stdout)
        self.assertIn("http://localhost:8648\n", r.stdout)
        r = self.env("5")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("N_VALIDATORS must be 3 or 4", r.stderr)

    def test_peer_ids_derive_from_the_keys(self):
        """PID<i> = the libp2p ed25519 peer id of KEY<i> (cross-checked against the two docker-compose ids)"""
        try:
            from cryptography.hazmat.primitives import serialization as ser
            from cryptography.hazmat.primitives.asymmetric.ed25519 import (
                Ed25519PrivateKey,
            )
        except ImportError:
            self.skipTest("python cryptography not installed")
        alpha = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

        def b58(b):
            n, o = int.from_bytes(b, "big"), ""
            while n:
                n, r = divmod(n, 58)
                o = alpha[r] + o
            return "1" * (len(b) - len(b.lstrip(b"\0"))) + o

        r = bash(
            'source "$1"; for i in 0 1 2 3; do v=KEY$i; p=PID$i; echo "${!v} ${!p:-}"; done',
            os.path.join(WSL, "env.sh"),
        )
        for line in r.stdout.splitlines():
            key, pid = line.split()
            pk = (
                Ed25519PrivateKey.from_private_bytes(bytes.fromhex(key))
                .public_key()
                .public_bytes(ser.Encoding.Raw, ser.PublicFormat.Raw)
            )
            proto = b"\x08\x01\x12\x20" + pk
            self.assertEqual(b58(b"\x00" + bytes([len(proto)]) + proto), pid)
        # val3's ed25519 key is devnet/genesis.json's 4th validator
        with open(os.path.join(REPO, "devnet", "genesis.json")) as f:
            g = json.load(f)
        pk = (
            Ed25519PrivateKey.from_private_bytes(bytes([4]) + bytes(31))
            .public_key()
            .public_bytes(ser.Encoding.Raw, ser.PublicFormat.Raw)
        )
        self.assertEqual(g["validators"][3]["pubkey"], "0x" + pk.hex())

    def test_node_peers(self):
        r = bash(
            'source "$1"; source "$2"; for i in 0 1 2 3; do echo "$(node_peers $i)"; done; node_peers 4 || echo REFUSED',
            os.path.join(WSL, "env.sh"),
            os.path.join(WSL, "start-node.sh"),
        )
        lines = r.stdout.splitlines()
        v = lambda port, pid: "/ip4/127.0.0.1/udp/%d/quic-v1/p2p/%s" % (port, pid)
        p0 = v(30401, "12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X")
        p1 = v(30402, "12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3")
        p2 = v(30403, "12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo")
        self.assertEqual(
            lines, [p1, p0, p0 + "," + p1, ",".join((p0, p1, p2)), "REFUSED"]
        )


class LaunchTest(unittest.TestCase):
    """launch-3val.sh in a sandbox copy of devnet/wsl with a fake torus-node that records its argv"""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="launch4-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        wsl = os.path.join(self.d, "devnet", "wsl")
        os.makedirs(wsl)
        for f in ("env.sh", "start-node.sh", "launch-3val.sh"):
            shutil.copy2(os.path.join(WSL, f), wsl)
        open(os.path.join(wsl, "genesis-3val.json"), "w").write("{}")
        rel = os.path.join(self.d, "target", "release")
        os.makedirs(rel)
        b = os.path.join(rel, "torus-node")
        with open(b, "w") as f:
            f.write(
                '#!/bin/bash\nfor a in "$@"; do case $a in --validator-key=*) k=${a#*=};; esac; done\n'
                'printf "%s\\n" "$@" > "$DATA_ROOT/argv-$k"\n'
            )
        os.chmod(b, 0o755)
        self.launch = os.path.join(wsl, "launch-3val.sh")

    def run_launch(self, n=None):
        env = {"DATA_ROOT": os.path.join(self.d, "dr")}
        if n:
            env["N_VALIDATORS"] = n
        r = bash('"$1"; sleep 0.5', self.launch, env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        dr = env["DATA_ROOT"]
        argv = {}
        for f in sorted(os.listdir(dr)):
            if f.startswith("argv-"):
                argv[f[5:7]] = open(os.path.join(dr, f)).read().split("\n")
        pids = open(os.path.join(dr, "run", "pids")).read().split()
        return r.stdout, argv, pids

    def test_three_nodes_by_default(self):
        out, argv, pids = self.run_launch()
        self.assertEqual(sorted(argv), ["01", "02", "03"])
        self.assertEqual(len(pids), 3)
        self.assertIn("val{0,1,2}.log", out)
        self.assertNotIn("val3", out)

    def test_four_nodes(self):
        out, argv, pids = self.run_launch("4")
        self.assertEqual(sorted(argv), ["01", "02", "03", "04"])
        self.assertEqual(len(pids), 4)
        self.assertIn("val{0,1,2,3}.log", out)
        a3 = argv["04"]
        self.assertIn("--rpc-addr=127.0.0.1:8648", a3)
        self.assertIn("--metrics-addr=127.0.0.1:9164", a3)
        self.assertIn("--p2p-listen=/ip4/127.0.0.1/udp/30404/quic-v1", a3)
        peers = [a for a in a3 if a.startswith("--p2p-peers=")][0]
        self.assertEqual(peers.count("/p2p/"), 3)
        self.assertTrue(any(a.endswith("/data/val3") for a in a3))
        # val0..val2 keep exactly the 3-node argv
        _, argv3, _ = LaunchTest.run_launch(self)
        for k in ("01", "02", "03"):
            self.assertEqual(argv[k], argv3[k])


# ------------------------------------------------------------------ crash-kill.sh guard + quorum
class CrashKillFourTest(unittest.TestCase):
    def quorum(self, idx, stakes):
        return lib('crash_norestart_quorum_ok "$@"', str(idx), stakes).returncode == 0

    def test_four_node_quorum(self):
        for idx in (1, 2, 3):
            self.assertTrue(self.quorum(idx, "1,1,1,1"), idx)  # 3/4
        self.assertTrue(self.quorum(3, "2,2,1,1"))  # 5/6
        self.assertTrue(self.quorum(1, "2,1,2,1"))
        self.assertFalse(self.quorum(2, "2,1,2,1"))  # a stake-2 node down: 4/6
        self.assertFalse(self.quorum(1, "2,2,1,1"))  # 4/6 = 2/3: halt
        for idx, w in (
            (0, "1,1,1,1"),
            (4, "1,1,1,1"),
            (3, "1,1,1"),
            (2, "1,1,1,1,1"),
            (2, "0,1,1,1"),
        ):
            self.assertFalse(self.quorum(idx, w), (idx, w))

    def test_three_node_quorum_unchanged(self):
        self.assertTrue(self.quorum(2, "2,2,1"))
        self.assertFalse(self.quorum(2, "1,1,1"))
        self.assertFalse(self.quorum(3, "2,2,1"))

    def test_guard_takes_val3_only_on_the_four_node_devnet(self):
        d = tempfile.mkdtemp(prefix="ck4-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        pidsf = os.path.join(d, "pids")
        open(pidsf, "w").write("11\n12\n13\n4242\n")
        cmd = (
            "/x/target/release/torus-node --data-dir=%s/data/val3 --rpc-addr=127.0.0.1:8648"
            % d
        )
        for n, ok in ((None, False), ("3", False), ("4", True)):
            env = {} if n is None else {"N_VALIDATORS": n}
            r = lib('crash_target_ok "$@"', "3", "4242", cmd, d, pidsf, env=env)
            self.assertEqual(r.returncode == 0, ok, (n, r.stderr))
        r = lib(
            'crash_target_ok "$@"', "0", "11", cmd, d, pidsf, env={"N_VALIDATORS": "4"}
        )
        self.assertNotEqual(r.returncode, 0)


# ------------------------------------------------------------------ gen-3val-genesis.sh end to end (tiny base)
class GenesisFourTest(unittest.TestCase):
    """the real generator in a sandbox repo with a 2-account weighted-full genesis"""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="gen4-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        for rel in (
            "devnet/wsl/gen-3val-genesis.sh",
            "devnet/wsl/bench-validator-keys.json",
            "devnet/wsl/keys-4val/bench-validator-keys.json",
            "devnet/genesis.json",
            "testnet/lib/cargo-bin.sh",
        ):
            os.makedirs(os.path.dirname(os.path.join(self.d, rel)), exist_ok=True)
            shutil.copy2(os.path.join(REPO, rel), os.path.join(self.d, rel))
        full = {
            "chain_id": 1,
            "consensus": {"timeout_base_ms": 1200, "epoch_length": 100},
            "evm": {"chain_id": 1},
            "validators": [],
            "accounts": [],
            "permanent_stakes": [],
            "markets": [{"market_id": i, "base_asset": "M%d" % i} for i in (1, 2, 3)],
            "native_balances": [
                {"address": "0x" + "%040x" % (i + 1), "note": "bulk-test %d" % (60 + i)}
                for i in range(2)
            ],
        }
        with open(
            os.path.join(self.d, "testnet", "genesis-weighted-full.json"), "w"
        ) as f:
            json.dump(full, f)

    def gen(self, **env):
        out = os.path.join(self.d, "out.json")
        if os.path.exists(out):
            os.remove(out)
        r = bash(
            '"$1"',
            os.path.join(self.d, "devnet/wsl/gen-3val-genesis.sh"),
            env=dict(env, OUT=out, BENCH_BIN="/bin/true", BULK_COUNT="2", MARKETS="2"),
        )
        if r.returncode != 0:
            return r, None
        with open(out, "rb") as f:
            return r, f.read()

    def test_three_validator_genesis_unchanged_by_the_new_knobs(self):
        r0, g0 = self.gen()
        self.assertEqual(r0.returncode, 0, r0.stderr)
        for env in (
            {"N_VALIDATORS": "3"},
            {"DL_K": "", "DL_JCAP": ""},
            {"N_VALIDATORS": "3", "DL_K": ""},
        ):
            r, g = self.gen(**env)
            self.assertEqual((r.returncode, g), (0, g0), env)
            self.assertEqual(
                r.stdout.replace("\n", ""), r0.stdout.replace("\n", ""), env
            )
        d = json.loads(g0)
        with open(KEYS3) as f:
            addrs = [v["address"] for v in json.load(f)["validators"]]
        self.assertEqual([v["address"] for v in d["validators"]], addrs)
        self.assertNotIn("leader_miss_threshold", d["consensus"])
        self.assertNotIn("dead_leader", r0.stdout)

    def test_four_validators_and_stakes(self):
        r, g = self.gen(N_VALIDATORS="4", VALIDATOR_STAKES="2,2,1,1")
        self.assertEqual(r.returncode, 0, r.stderr)
        d = json.loads(g)
        with open(KEYS4) as f:
            addrs = [v["address"] for v in json.load(f)["validators"]]
        self.assertEqual([v["address"] for v in d["validators"]], addrs)
        self.assertEqual(
            [v["stake"] for v in d["validators"]],
            ["2" + BASE_STAKE[1:]] * 2 + [BASE_STAKE] * 2,
        )
        self.assertEqual(
            d["validators"][3]["pubkey"],
            "0x9be3287795907809407e14439ff198d5bfc7dce6f9bc743cb369146f610b4801",
        )
        self.assertIn("validators = 4", r.stdout)

    def test_dead_leader_fields_only_when_set(self):
        r, g = self.gen(DL_K="3", DL_JCAP="2")
        self.assertEqual(r.returncode, 0, r.stderr)
        c = json.loads(g)["consensus"]
        self.assertEqual(
            (c["leader_miss_threshold"], c["leader_exclusion_cap"]), (3, 2)
        )
        self.assertIn(
            "dead_leader = leader_miss_threshold 3 leader_exclusion_cap 2", r.stdout
        )
        r, g = self.gen(DL_K="0")
        c = json.loads(g)["consensus"]
        self.assertEqual(c["leader_miss_threshold"], 0)
        self.assertNotIn("leader_exclusion_cap", c)

    def test_bad_input_is_fatal(self):
        for env, msg in (
            ({"N_VALIDATORS": "5"}, "N_VALIDATORS must be 3 or 4"),
            (
                {"N_VALIDATORS": "4", "VALIDATOR_STAKES": "2,2,1"},
                "four positive integer weights",
            ),
            ({"VALIDATOR_STAKES": "2,2,1,1"}, "three positive integer weights"),
            ({"DL_K": "1"}, "DL_K"),
            ({"DL_K": "65"}, "DL_K"),
            ({"DL_JCAP": "11"}, "DL_JCAP"),
            ({"DL_JCAP": "x"}, "DL_JCAP"),
        ):
            r, _ = self.gen(**env)
            self.assertNotEqual(r.returncode, 0, env)
            self.assertIn(msg, r.stderr, env)


class KeyFilesTest(unittest.TestCase):
    def test_keys_4val_extend_the_3val_files(self):
        for three, four in (
            (KEYS3, KEYS4),
            (os.path.join(WSL, "bench-oracle-signer-keys.json"), SIGNERS4),
        ):
            with open(three) as f:
                a = json.load(f)["validators"]
            with open(four) as f:
                b = json.load(f)["validators"]
            self.assertEqual(b[:3], a)
            self.assertEqual([v["index"] for v in b], [0, 1, 2, 3])
        self.assertEqual([i for i, _, _ in sa.read_keys(KEYS4)], [0, 1, 2, 3])
        self.assertEqual([i for i, _, _ in sa.read_keys(KEYS3)], [0, 1, 2])

    def test_state_attest_exclude_must_name_a_validator(self):
        d = tempfile.mkdtemp(prefix="sa4-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        rc = sa.main(
            [
                "scan",
                "--rpc",
                "http://127.0.0.1:1",
                "--rpc",
                "http://127.0.0.1:1",
                "--rpc",
                "http://127.0.0.1:1",
                "--keys",
                KEYS3,
                "--exclude",
                "val3",
                "--out",
                os.path.join(d, "o"),
            ]
        )
        self.assertEqual(rc, 2)


class GenesisCacheKeyTest(unittest.TestCase):
    def test_new_knobs_are_key_inputs(self):
        spec = importlib.util.spec_from_file_location(
            "genesis_cache", os.path.join(HERE, "genesis_cache.py")
        )
        gc = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gc)
        for p in ("N_VALIDATORS", "DL_K", "DL_JCAP"):
            self.assertIn(p, gc.PARAMS)
        self.assertIn("devnet/wsl/keys-4val/bench-validator-keys.json", gc.KEY_FILES)
        params = {p: "" for p in gc.PARAMS}
        params.update(
            MARKETS="10",
            BULK_COUNT="100000",
            LIQ_THIN="0",
            LIQ_THIN_AVAIL="1000000.0",
            N_VALIDATORS="3",
        )
        with tempfile.TemporaryDirectory() as root:
            rc = os.path.join(root, "run-cell.sh")
            open(rc, "w").write("#!/bin/bash\n")
            keys = {
                gc.key_of(gc.key_inputs(root, rc, dict(params, **kv), {}))
                for kv in ({}, {"N_VALIDATORS": "4"}, {"DL_K": "3"}, {"DL_JCAP": "2"})
            }
        self.assertEqual(len(keys), 4)


# ------------------------------------------------------------------ health.py / sampler / log collection CLIs
class CliNodeCountTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="cli4-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        srv = ThreadingHTTPServer(("127.0.0.1", 0), Advancing)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        base = "http://127.0.0.1:%d" % srv.server_address[1]
        self.urls = ["%s/four-val%d" % (base, i) for i in range(4)]

    def drain(self, urls, *extra):
        return subprocess.run(
            [
                sys.executable,
                HEALTH,
                "drain",
                "--out",
                self.d,
                "--timeout",
                "8",
                "--quiet",
                "2",
                "--urls",
                *urls,
                *extra,
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )

    def test_drain_takes_four_nodes_and_excludes_val3(self):
        r = self.drain(self.urls)
        self.assertEqual(r.returncode, 0, r.stderr)
        r = self.drain(self.urls, "--exclude", "val3")
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(self.d, "drain.json")) as f:
            d = json.load(f)
        self.assertEqual(
            (d["nodes"], d["excluded_nodes"]), (["val0", "val1", "val2"], ["val3"])
        )

    def test_drain_refusals(self):
        for urls, extra in (
            (self.urls[:2], ()),
            (self.urls + self.urls[:1], ()),
            (self.urls[:3], ("--exclude", "val3")),
            (
                self.urls,
                ("--attest-live", "--attest-rpc", "http://a", "http://b", "http://c"),
            ),
        ):
            r = self.drain(urls, *extra)
            self.assertEqual(r.returncode, 2, (len(urls), extra, r.stderr))

    def test_collect_logs_four(self):
        logs = []
        for i in range(4):
            p = os.path.join(self.d, "val%d.log" % i)
            open(p, "w").write("x HASH-ONLY manifest push\n" * i)
            logs.append(p)
        r = subprocess.run(
            [
                sys.executable,
                os.path.join(HERE, "collect_logs.py"),
                "--logs",
                *logs,
                "--out",
                os.path.join(self.d, "ls.json"),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("val3:manifest=3,", r.stdout)
        r = subprocess.run(
            [
                sys.executable,
                os.path.join(HERE, "collect_logs.py"),
                "--logs",
                *logs[:2],
                "--out",
                os.path.join(self.d, "ls.json"),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 2)

    def test_sampler_argv(self):
        r = subprocess.run(
            [
                sys.executable,
                os.path.join(HERE, "sample_metrics.py"),
                "--out",
                self.d,
                "--wide",
                "a",
                "--funnel",
                "a",
                "--phase",
                "a",
                "--buckets",
                "a",
                *self.urls[:2],
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 2)


class DeferredScansFourTest(unittest.TestCase):
    def test_marker_and_end_pass_take_four_logs(self):
        d = tempfile.mkdtemp(prefix="endpass4-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        r = bash(
            'source "$1"; write_deferred_marker "$2" 7 "10 20 30"; cat "$2/deferred-scans.json"; '
            'write_deferred_marker "$2" 7 "10 20 30 40"; cat "$2/deferred-scans.json"',
            os.path.join(HERE, "log-scans.sh"),
            d,
        )
        a, b = r.stdout.splitlines()
        self.assertEqual(
            a,
            '{"version": 1, "scans": "ingest-evictions log-excerpts", "bench_submitted": "7", '
            '"ingest_log_bytes": [10, 20, 30]}',
        )
        self.assertEqual(json.loads(b)["ingest_log_bytes"], [10, 20, 30, 40])
        for i in range(4):
            with gzip.open(os.path.join(d, "val%d.log.gz" % i), "wt") as f:
                f.write("a evicted nonce-expired evicted=%d\n" % (i + 1))
        r = subprocess.run(
            [os.path.join(HERE, "cell-endpass.sh"), d],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        self.assertIn(
            "evictions per node 0 0 0 4", r.stdout
        )  # byte lengths 10..30 cut the line (35 bytes), 40 keeps it
        self.assertTrue(os.path.exists(os.path.join(d, "val3.log.excerpt")))


# ------------------------------------------------------------------ run-cell.sh pre-flight pieces
class RunCellFourTest(unittest.TestCase):
    PRE_START = "# ---- no-restart kill (KILL_NO_RESTART) pre-flight."
    PRE_END = '[ -z "$KNR_IDX" ] || KNR_EXCLUDE_ARGS=(--exclude "$KILL_NO_RESTART")\n'

    def setUp(self):
        self.pre = src_between(RUN_CELL_SH, self.PRE_START, self.PRE_END)
        self.urls_fn = src_between(RUN_CELL_SH, "bench_rpc_urls() {", "\n}\n")
        self.idx = src_between(RUN_CELL_SH, "ALL_IDX=(0 1 2)\n", "ALL_IDX+=(3)\n")

    def preflight(self, **env):
        script = (
            "set -uo pipefail\nN_VALIDATORS=${N_VALIDATORS:-3}\n"
            + self.idx
            + self.urls_fn
            + 'RPCS=(http://a:1 http://b:2 http://c:3)\n[ "$N_VALIDATORS" = 3 ] || RPCS+=(http://d:4)\n'
            + "BENCH_RPC_URLS=$(bench_rpc_urls) || exit 2\n"
            + 'evm_load_on() { [ "${EVM_ON:-0}" = 1 ]; }\n'
            + "DUR=${DUR:-600}; CRASH_KILL_AT_S=${CRASH_KILL_AT_S:-}; ORACLE_FEED=${ORACLE_FEED:-0}\n"
            + "KILL_NO_RESTART=${KILL_NO_RESTART:-}; KILL_NO_RESTART_AT_S=${KILL_NO_RESTART_AT_S:-}\n"
            + "VALIDATOR_STAKES=${VALIDATOR_STAKES:-}; DL_REPORT=${DL_REPORT:-}; TOOLS_FROM_WORKTREE=1; SELF_DIR=$TOOLS_DIR\n"
            + self.pre
            + 'echo "LIVE=${LIVE_IDX[*]}"; echo "LIVE_RPCS=${LIVE_RPCS[*]}"; echo "BENCH=$BENCH_RPC_URLS"; '
            'echo "EXCL=${KNR_EXCLUDE_ARGS[*]+${KNR_EXCLUDE_ARGS[*]}}"; echo "KNR_IDX=$KNR_IDX"; echo "DLR=$DL_REPORT"\n'
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
                "N_VALIDATORS",
                "DL_REPORT",
            )
        }
        r = subprocess.run(
            ["bash", "-c", script],
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(base, TOOLS_DIR=HERE, **env),
        )
        return r, dict(l.split("=", 1) for l in r.stdout.splitlines() if "=" in l)

    ON4 = dict(
        N_VALIDATORS="4",
        KILL_NO_RESTART="val3",
        KILL_NO_RESTART_AT_S="30",
        DUR="120",
        BENCH_RPCS="0,1,2",
    )

    def test_off_three_and_four(self):
        r, v = self.preflight()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((v["LIVE"], v["EXCL"], v["DLR"]), ("0 1 2", "", "0"))
        self.assertEqual(v["BENCH"], "http://a:1,http://b:2,http://c:3")
        r, v = self.preflight(N_VALIDATORS="4")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((v["LIVE"], v["DLR"]), ("0 1 2 3", "0"))
        self.assertEqual(v["BENCH"], "http://a:1,http://b:2,http://c:3,http://d:4")

    def test_kill_val3_with_equal_stakes(self):
        r, v = self.preflight(**self.ON4)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            (v["LIVE"], v["EXCL"], v["KNR_IDX"], v["DLR"]),
            ("0 1 2", "--exclude val3", "3", "1"),
        )
        self.assertEqual(v["LIVE_RPCS"], "http://a:1 http://b:2 http://c:3")
        r, v = self.preflight(
            **dict(self.ON4, VALIDATOR_STAKES="2,2,1,1", DL_REPORT="0")
        )
        self.assertEqual((r.returncode, v["DLR"]), (0, "0"), r.stderr)
        r, v = self.preflight(
            **dict(self.ON4, KILL_NO_RESTART="val1", BENCH_RPCS="0,2,3")
        )
        self.assertEqual((r.returncode, v["LIVE"]), (0, "0 2 3"), r.stderr)

    def test_refusals(self):
        cases = {
            "val3 needs the 4-node devnet": dict(
                KILL_NO_RESTART="val3",
                KILL_NO_RESTART_AT_S="30",
                DUR="120",
                VALIDATOR_STAKES="2,2,1",
                BENCH_RPCS="0,1",
            ),
            "2,2,1,1 with a stake-2 node down halts": dict(
                self.ON4,
                KILL_NO_RESTART="val1",
                VALIDATOR_STAKES="2,2,1,1",
                BENCH_RPCS="0,2,3",
            ),
            "never val0": dict(self.ON4, KILL_NO_RESTART="val0"),
            "bench sends to val3": dict(self.ON4, BENCH_RPCS="all"),
            "DL_REPORT needs a kill": dict(N_VALIDATORS="4", DL_REPORT="1"),
            "DL_REPORT is 0 or 1": dict(self.ON4, DL_REPORT="yes"),
        }
        for what, env in cases.items():
            with self.subTest(what):
                r, _ = self.preflight(**env)
                self.assertEqual(r.returncode, 2, (what, r.stdout, r.stderr))
                self.assertIn("FATAL", r.stderr)

    def test_three_node_messages_unchanged(self):
        r, _ = self.preflight(
            KILL_NO_RESTART="val2",
            KILL_NO_RESTART_AT_S="30",
            DUR="120",
            BENCH_RPCS="0,1",
        )
        self.assertIn(
            "the two validators left would hold no consensus quorum (> 2/3 of the power) under stakes "
            "1,1,1 (VALIDATOR_STAKES): the chain would halt at the kill. Use VALIDATOR_STAKES=2,2,1 with "
            "KILL_NO_RESTART=val2",
            r.stderr,
        )
        r, _ = self.preflight(
            KILL_NO_RESTART="val0", KILL_NO_RESTART_AT_S="30", DUR="120"
        )
        self.assertIn(
            "FATAL: KILL_NO_RESTART must be val1 or val2 (never val0 — it serves the bench RPC and every "
            "headline number)",
            r.stderr,
        )

    def test_script_preflight(self):
        """the real run-cell.sh refuses a bad N, a 3-node worktree under N=4 and DL knobs on a binary without the
        feature, before anything is launched"""
        tmp = tempfile.mkdtemp(prefix="rc4-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        tgt = os.path.join(tmp, "release")
        os.makedirs(tgt)
        for b in ("torus-node", "bench-throughput"):
            p = os.path.join(tgt, b)
            open(p, "w").write("#!/bin/sh\nexit 0\n")
            os.chmod(p, 0o755)
        script, wt = sandbox_repo(tmp)
        for env, rc, msg in (
            (dict(N_VALIDATORS="5"), 2, "N_VALIDATORS must be 3 or 4"),
            (dict(N_VALIDATORS="4"), 1, "has no 4-validator devnet"),
            (dict(DL_K="3"), 1, "has no dead-leader skip"),
            (dict(DL_K="1"), 2, "DL_K"),
            (dict(EXTRA_ENV="N_VALIDATORS=4"), 2, "a run-cell.sh knob"),
        ):
            extra = env.pop("EXTRA_ENV", None)
            r = subprocess.run(
                [script, wt, "rc4-x", "10", "120", "1000"] + ([extra] if extra else []),
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
                os.path.exists(os.path.join(tmp, "rc4-x")),
                "must fail before any launch",
            )

    def test_no_three_node_loop_left(self):
        with open(RUN_CELL_SH) as f:
            src = f.read()
        body = src[src.index("set -uo pipefail") :]
        for needle in (
            "for i in 0 1 2",
            "${METS[2]}",
            "${RPCS[2]}",
            "val2.log",
            "nargs=3",
        ):
            self.assertNotIn(needle, body)
        for needle in (
            '--param N_VALIDATORS="$N_VALIDATORS" --param DL_K="$DL_K" --param DL_JCAP="$DL_JCAP"',
            "N_VALIDATORS=$N_VALIDATORS DL_K=$DL_K DL_JCAP=$DL_JCAP VALIDATOR_STAKES=$VALIDATOR_STAKES",
            '"$SELF_DIR/dl_window.py" --dir "$OUT"',
        ):
            self.assertIn(needle, src)
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            rc = f.read()
        for k in ("N_VALIDATORS", "DL_K", "DL_JCAP", "DL_REPORT"):
            self.assertIn('"%s",' % k, rc)


class ThreeNodeExpansionTest(unittest.TestCase):
    """N_VALIDATORS=3: every N-aware run-cell.sh expression expands to the literal the script had before the knob
    (the 3-node argv, URL lists, bounds and log texts, byte for byte); N_VALIDATORS=4 adds val3."""

    def expand(self, n):
        src = open(RUN_CELL_SH).read()
        line = lambda start: src[src.index(start): src.index("\n", src.index(start)) + 1]
        script = "set -uo pipefail\nN_VALIDATORS=%s; MARKETS=300; ORACLE_FEED_DRAIN_MAX_LAG=2; RUN_DIR=/R\n" % n
        script += ". %s\n" % os.path.join(HERE, "log-scans.sh")
        script += src_between(RUN_CELL_SH, "ALL_IDX=(0 1 2)\n", "ALL_IDX+=(3)\n")
        for start in ("RPCS=(http://127.0.0.1:8645", '[ "$N_VALIDATORS" = 3 ] || RPCS+=', "METS=(9161 9162 9163)",
                      '[ "$N_VALIDATORS" = 3 ] || METS+=', "METS_URLS=()", "MIN_PEERS=", "DRAIN_URLS=(",
                      "    FEED_DRAIN_ARGS=(--feed-live", "    ATTEST_DRAIN_ARGS=(--attest-live", "NODE_LOGS=();",
                      "    SA_ARGS=(); for i in"):
            script += line(start)
        script += src_between(RUN_CELL_SH, '    for i in "${ALL_IDX[@]}"; do SA_ARGS+=(--metrics',
                              'SA_ARGS+=(--log "$RUN_DIR/val$i.log"); done\n')
        i = src.index('--metrics-urls "') + len('--metrics-urls "')
        pm = src[i: src.index('" \\\n', i)]
        script += (
            'echo "PRELOAD_METS=' + pm + '"\n'
            'echo "DRAIN=${DRAIN_URLS[*]}"\necho "FEED=${FEED_DRAIN_ARGS[*]}"\necho "ATTEST=${ATTEST_DRAIN_ARGS[*]}"\n'
            'echo "HEALTH=all ${#ALL_IDX[@]} committing, peers>=$MIN_PEERS"\necho "LOGS=${NODE_LOGS[*]}"\n'
            'echo "SA=${SA_ARGS[*]}"\necho "STAKES=$(IFS=,; echo "${ALL_IDX[*]/#/val}")"\n'
            'LIVE_IDX=(0 1); echo "LIVE=only ${LIVE_IDX[*]/#/val} are"\n'
        )
        r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return dict(l.split("=", 1) for l in r.stdout.splitlines())

    def test_three(self):
        v = self.expand(3)
        m = ["http://127.0.0.1:%d/metrics" % p for p in (9161, 9162, 9163)]
        rpc = ["http://127.0.0.1:%d" % p for p in (8645, 8646, 8647)]
        self.assertEqual(v["PRELOAD_METS"], ",".join(m))
        self.assertEqual(v["DRAIN"], " ".join(m))
        self.assertEqual(v["FEED"], "--feed-live --feed-mempool-max %d --max-lag 2" % (6 * ((300 + 255) // 256)))
        self.assertEqual(v["ATTEST"], "--attest-live --attest-mempool-max 6 --attest-rpc " + " ".join(rpc))
        self.assertEqual(v["HEALTH"], "all 3 committing, peers>=2")
        self.assertEqual(v["LOGS"], "/R/val0.log /R/val1.log /R/val2.log")
        self.assertEqual(v["SA"], " ".join(["--rpc " + u for u in rpc] + ["--metrics " + u for u in m]
                                           + ["--log /R/val%d.log" % i for i in range(3)]))
        self.assertEqual(v["STAKES"], "val0,val1,val2")
        self.assertEqual(v["LIVE"], "only val0 val1 are")

    def test_four(self):
        v = self.expand(4)
        self.assertTrue(v["PRELOAD_METS"].endswith(",http://127.0.0.1:9164/metrics"))
        self.assertEqual(v["ATTEST"].split()[:3], ["--attest-live", "--attest-mempool-max", "8"])
        self.assertTrue(v["ATTEST"].endswith("http://127.0.0.1:8647 http://127.0.0.1:8648"))
        self.assertIn("--feed-mempool-max 16 ", v["FEED"])
        self.assertEqual(v["HEALTH"], "all 4 committing, peers>=3")
        self.assertTrue(v["LOGS"].endswith("/R/val3.log"))
        self.assertEqual(v["SA"].count("--rpc "), 4)
        self.assertEqual(v["STAKES"], "val0,val1,val2,val3")


# ------------------------------------------------------------------ summarize.py with val3
def write_four_cell(d, kill="val3", dur=120):
    cols = [
        "torus_orders_matched_total",
        "torus_orders_placed_accepted_total",
        "torus_block_height",
        "torus_blocks_committed_total",
        "torus_native_actions_processed_total",
        "torus_orders_resting_total",
        "torus_exec_queue_depth",
        "torus_mempool_native_size",
        "torus_flush_worker_depth",
        "torus_consensus_timeout_total_total",
        "torus_consensus_view",
        "scrape_valid",
    ]
    with open(os.path.join(d, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for s in range(dur + 1):
            for n in ("val0", "val1", "val2", "val3"):
                if n == kill and s > KILL_AT:
                    vals = [0] * (len(cols) - 1) + [0]
                else:
                    to = (
                        0 if s <= KILL_AT else min(s - KILL_AT, 20) // 2
                    )  # timeouts stop 20 s after the kill
                    vals = [
                        1000 * s,
                        2000 * s,
                        10 * s,
                        10 * s,
                        1000 * s,
                        5,
                        1,
                        0,
                        0,
                        to,
                        12 * s,
                        1,
                    ]
                f.write(
                    "%d,%s,%s\n" % (BENCH_START + s, n, ",".join(str(v) for v in vals))
                )
    open(os.path.join(d, "bench.log"), "w").write(
        "Submitted (load-gen accepted): 1,000\n"
    )
    for i in range(4):
        open(os.path.join(d, "metrics-before-val%d.txt" % i), "w").write(
            "torus_block_height 0\n"
        )
    with open(os.path.join(d, "agreement.jsonl"), "w") as f:
        for i, n in enumerate(n for n in ("val0", "val1", "val2", "val3") if n != kill):
            f.write(
                json.dumps(
                    {
                        "node": n,
                        "height": 1200 + i,
                        "cmp_height": 1195,
                        "block_hash": "0xdead",
                        "header_state_root": "0x0",
                        "state_digest": "same",
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
    if kill:
        write_run_identity(d, knob=kill)
        write_kill_record(d, kill_node=kill)
    else:
        write_run_identity(d, knob=None)


class SummarizeFourTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="summ4-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def summ(self, nodes, heights):
        dissem = " ".join(
            "%s:exhausted=0,sync_fallback=0,da_outbound_fail=0,starvation=0" % n
            for n in nodes
        )
        return run_summarize(
            self.d,
            dur=120,
            extra=[
                "--dissem",
                dissem,
                "--digest-quiescent",
                "1",
                "--digest-heights",
                heights,
            ],
        )

    def test_val3_killed_three_survivors_agree(self):
        write_four_cell(self.d)
        s, out = self.summ(("val0", "val1", "val2", "val3"), "1200 1201 1202")
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertEqual(s["agreement"]["excluded_nodes"], ["val3"])
        self.assertEqual(s["kill_no_restart"]["survivors"], ["val0", "val1", "val2"])
        self.assertIn("val3", s["kill_no_restart"]["per_node"])
        self.assertEqual(sorted(s["liveness"]["nodes"]), ["val0", "val1", "val2"])
        self.assertIn("val3", s["funnel_by_node"])

    def test_all_four_up(self):
        write_four_cell(self.d, kill=None)
        s, _ = self.summ(("val0", "val1", "val2", "val3"), "1200 1201 1202 1203")
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertEqual(
            sorted(s["liveness"]["nodes"]), ["val0", "val1", "val2", "val3"]
        )

    def test_a_four_node_cell_missing_a_row_is_incomplete(self):
        write_four_cell(self.d, kill=None)
        with open(os.path.join(self.d, "agreement.jsonl")) as f:
            rows = f.readlines()[:3]
        open(os.path.join(self.d, "agreement.jsonl"), "w").writelines(rows)
        s, _ = self.summ(("val0", "val1", "val2", "val3"), "1200 1201 1202")
        self.assertEqual(s["agreement"]["agreement_verdict"], "INCOMPLETE")


# ------------------------------------------------------------------ dl_window.py
def iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def delta_line(ts, h, exclude=1, readmit=0, ansi=True):
    if ansi:
        f = lambda k, v: "\x1b[3m%s\x1b[0m\x1b[2m=\x1b[0m%s" % (k, v)
        lvl = "\x1b[32m INFO\x1b[0m"
    else:
        f = lambda k, v: "%s=%s" % (k, v)
        lvl = " INFO"
    return (
        "\x1b[2m%s\x1b[0m %s \x1b[2mtorus_consensus::app\x1b[0m\x1b[2m:\x1b[0m epoch boundary: leader exclusion "
        "delta from the execution plan %s %s %s %s\n"
        % (
            iso(ts),
            lvl,
            f("height", h),
            f("exclude", exclude),
            f("readmit", readmit),
            f("excluded", exclude),
        )
    )


class DlWindowTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="dlw-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        write_four_cell(self.d)
        open(os.path.join(self.d, "summary.json"), "w").write('{"status": "OK"}')
        self.kill_ts = BENCH_START + KILL_AT + 0.5

    def logs(self, lines_by_node):
        for n in ("val0", "val1", "val2", "val3"):
            with gzip.open(os.path.join(self.d, n + ".log.gz"), "wt") as f:
                f.write("2026-10-22T00:00:00.000000Z  INFO torus_node: started\n")
                for l in lines_by_node.get(n, []):
                    f.write(l)

    def run_dl(self):
        out = subprocess.run(
            [
                sys.executable,
                os.path.join(HERE, "dl_window.py"),
                "--dir",
                self.d,
                "--t-bench0",
                str(BENCH_START),
                "--t-bench1",
                str(BENCH_START + 120),
                "--dl-k",
                "3",
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(out.returncode, 0, out.stderr + out.stdout)
        with open(os.path.join(self.d, "dl-window.json")) as f:
            return json.load(f), out.stdout

    def test_three_windows_around_the_exclusion(self):
        # boundary 800: committed height (10 s + 1) reaches 800 at s = 80 (ts = BENCH_START + 80)
        d0 = self.kill_ts + 15.2
        self.logs(
            {
                "val1": [delta_line(d0 + 0.1, 800)],
                "val0": [delta_line(d0, 800), delta_line(d0 + 0.3, 800)],
                "val2": [
                    delta_line(BENCH_START + 10, 200, exclude=0, readmit=1, ansi=False)
                ],
            }
        )
        d, line = self.run_dl()
        e = d["exclusion"]
        self.assertTrue(e["found"])
        self.assertEqual(
            (e["boundary_height"], e["delta_node"], e["exclude"]), (800, "val0", 1)
        )
        self.assertAlmostEqual(e["kill_to_delta_s"], 15.2, places=3)
        self.assertEqual(e["committed_ts"], BENCH_START + 80)
        self.assertAlmostEqual(e["kill_to_exclusion_s"], 19.5, places=3)
        self.assertEqual(set(e["committed_ts_by_node"]), {"val0", "val1", "val2"})
        self.assertEqual(d["survivors"], ["val0", "val1", "val2"])
        self.assertEqual(
            d["windows"]["post_exclusion"], [BENCH_START + 80, BENCH_START + 120]
        )
        h = d["headline"]
        self.assertEqual(h["pre_kill"]["blk_s"], 10.0)
        self.assertEqual(h["pre_kill"]["timeouts_per_min"], 0.0)
        self.assertGreater(h["kill_to_exclusion"]["timeouts_per_min"], 0)
        self.assertEqual(
            h["post_exclusion"]["timeouts_per_min"], 0.0
        )  # timeouts stopped at kill + 20 s
        self.assertEqual(h["post_exclusion"]["matched_s"], 1000.0)
        self.assertEqual(d["heights"]["killed_node_height_at_kill"], 600)
        self.assertEqual(d["heights"]["committed_at_kill_by_node"]["val0"], 601)
        self.assertEqual(d["heights"]["blocks_kill_to_boundary_val0"], 199)
        self.assertEqual(
            d["timeouts"]["last_increment_after_kill_s_by_node"]["val0"], 19.5
        )
        self.assertEqual(
            [x["height"] for x in d["deltas_after_kill"]], [800]
        )  # the pre-kill readmit is not listed
        self.assertEqual(d["problems"], [])
        with open(os.path.join(self.d, "summary.json")) as f:
            s = json.load(f)
        self.assertEqual(s["status"], "OK")
        self.assertEqual(s["dead_leader"]["exclusion"]["boundary_height"], 800)
        self.assertEqual(s["dead_leader"]["dl_k"], "3")
        self.assertIn(
            "dead-leader window: kill val3 at bench+60s h=600; exclusion delta at boundary h=800",
            line,
        )

    def test_no_exclusion_line(self):
        self.logs({})
        d, line = self.run_dl()
        self.assertFalse(d["exclusion"]["found"])
        self.assertIn("reason", d["exclusion"])
        self.assertEqual(
            d["windows"]["kill_to_exclusion"], [self.kill_ts, BENCH_START + 120]
        )
        self.assertIsNone(d["windows"]["post_exclusion"])
        self.assertIsNone(d["headline"]["post_exclusion"]["blk_s"])
        self.assertIn("NO exclusion observed", line)

    def test_parse_plain_and_json_shapes(self):
        p = dl_window.parse_delta_line(delta_line(BENCH_START, 300, ansi=False), "val1")
        self.assertEqual(
            (p["height"], p["exclude"], p["readmit"], p["ts"]), (300, 1, 0, BENCH_START)
        )
        j = (
            '{"timestamp":"%s","level":"INFO","fields":{"message":"epoch boundary: leader exclusion delta from the '
            'execution plan","height":400,"exclude":0,"readmit":1,"excluded":0}}'
            % iso(BENCH_START)
        )
        p = dl_window.parse_delta_line(j, "val2")
        self.assertEqual((p["height"], p["exclude"], p["readmit"]), (400, 0, 1))
        self.assertIsNone(
            dl_window.parse_delta_line("2026-10-22T00:00:00Z INFO other", "val0")
        )

    def test_missing_kill_record_is_usage_error(self):
        os.remove(os.path.join(self.d, "kill-norestart.json"))
        self.logs({})
        r = subprocess.run(
            [
                sys.executable,
                os.path.join(HERE, "dl_window.py"),
                "--dir",
                self.d,
                "--t-bench0",
                "0",
                "--t-bench1",
                "1",
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(r.returncode, 2)
        self.assertIn("FAILED", r.stdout)


if __name__ == "__main__":
    unittest.main()
