#!/usr/bin/env python3
"""Devnets of 3..7 validators (run-cell.sh N_VALIDATORS=5..7, all-up shapes, 18c msg 201). python3 -m unittest.

The 4-validator devnet (test_four_validators.py) generalised: val<i> = ed25519 key 0<i+1>, RPC 8645+i, metrics
9161+i, p2p 30401+i, dialing every lower-index validator (a full mesh); val4..val6 are genesis-registered with the
keys-<N>val account keys (hardhat dev mnemonic indices 108..110, oracle signers 111..113) and the ed25519 pubkeys
of keys 05..07 (devnet/genesis.json lists only 4 validators). N = 3 and N = 4 stay byte-identical: the genesis
generator against f4010070's, the run-cell.sh expansions and messages against their 3/4 literals. Stub processes
and stub HTTP servers on ephemeral 127.0.0.1 ports only; no node, no cargo.
"""

import hashlib
import hmac
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

from test_harness import BENCH_START, RUN_CELL_SH, run_summarize, sandbox_repo  # noqa: E402
from test_kill_norestart import (
    KILL_AT,
    RUN_ID,
    Advancing,
    lib,
    src_between,
    write_kill_record,
)  # noqa: E402
import state_attest as sa  # noqa: E402

WSL = os.path.join(REPO, "devnet", "wsl")
HEALTH = os.path.join(HERE, "health.py")
BASE_STAKE = "1000000000000000000000000"
# the generator before the 3..7 change (origin/main): 3- and 4-validator genesis files must stay its bytes
PRE_NVAL_COMMIT = "f4010070"
CLEAN_ENV = (
    "N_VALIDATORS",
    "DL_K",
    "DL_JCAP",
    "VALIDATOR_STAKES",
    "DL_REPORT",
    "KILL_NO_RESTART",
    "KILL_NO_RESTART_AT_S",
    "BENCH_RPCS",
    "CRASH_KILL_AT_S",
    "ORACLE_FEED",
    "DUR",
    "HOTSTUFF_CPUS",
)


def keys_file(n, kind="bench-validator-keys.json"):
    if n == 3:
        return os.path.join(WSL, kind)
    return os.path.join(WSL, "keys-%dval" % n, kind)


def bash(script, *args, env=None, cwd=None, timeout=60):
    base = {k: v for k, v in os.environ.items() if k not in CLEAN_ENV}
    return subprocess.run(
        ["bash", "-c", script, "_", *args],
        capture_output=True,
        text=True,
        env=dict(base, **(env or {})),
        cwd=cwd,
        timeout=timeout,
    )


def ed25519_pub(i):
    """the raw ed25519 public key of devnet key 0<i+1> (None without python cryptography)"""
    try:
        from cryptography.hazmat.primitives import serialization as ser
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError:
        return None
    return (
        Ed25519PrivateKey.from_private_bytes(bytes([i + 1]) + bytes(31))
        .public_key()
        .public_bytes(ser.Encoding.Raw, ser.PublicFormat.Raw)
    )


def peer_id(pub):
    alpha = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    proto = b"\x08\x01\x12\x20" + pub
    b = b"\x00" + bytes([len(proto)]) + proto
    n, o = int.from_bytes(b, "big"), ""
    while n:
        n, r = divmod(n, 58)
        o = alpha[r] + o
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + o


# ---- secp256k1 + keccak-256 (stdlib only): an account key file's private_key must give its address
_RC = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808A,
    0x8000000080008000,
    0x000000000000808B,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008A,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000A,
    0x000000008000808B,
    0x800000000000008B,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800A,
    0x800000008000000A,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
]
_ROT = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
]
_M64 = (1 << 64) - 1


def keccak256(data):
    rol = lambda x, n: ((x << n) | (x >> (64 - n))) & _M64 if n else x
    p = bytearray(data) + b"\x01"
    while len(p) % 136:
        p += b"\x00"
    p[-1] |= 0x80
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(p), 136):
        for i in range(17):
            a[i % 5][i // 5] ^= int.from_bytes(
                p[off + 8 * i : off + 8 * i + 8], "little"
            )
        for rc in _RC:
            c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
            d = [c[(x - 1) % 5] ^ rol(c[(x + 1) % 5], 1) for x in range(5)]
            a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
            b = [[0] * 5 for _ in range(5)]
            for x in range(5):
                for y in range(5):
                    b[y][(2 * x + 3 * y) % 5] = rol(a[x][y], _ROT[x][y])
            a = [
                [b[x][y] ^ ((~b[(x + 1) % 5][y]) & b[(x + 2) % 5][y]) for y in range(5)]
                for x in range(5)
            ]
            a[0][0] ^= rc
    return b"".join(a[i % 5][i // 5].to_bytes(8, "little") for i in range(4))


_P = 2**256 - 2**32 - 977
_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def _add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a[0] == b[0] and (a[1] + b[1]) % _P == 0:
        return None
    if a == b:
        l = 3 * a[0] * a[0] * pow(2 * a[1], -1, _P) % _P
    else:
        l = (b[1] - a[1]) * pow(b[0] - a[0], -1, _P) % _P
    x = (l * l - a[0] - b[0]) % _P
    return (x, (l * (a[0] - x) - a[1]) % _P)


def _mul(k, pt=_G):
    r = None
    while k:
        if k & 1:
            r = _add(r, pt)
        pt = _add(pt, pt)
        k >>= 1
    return r


def eth_address(priv_hex):
    pt = _mul(int(priv_hex, 16))
    return (
        "0x"
        + keccak256(pt[0].to_bytes(32, "big") + pt[1].to_bytes(32, "big"))[-20:].hex()
    )


def hardhat_key(index):
    """m/44'/60'/0'/0/<index> of the hardhat/anvil dev mnemonic (BIP39 seed, BIP32 derivation)"""
    seed = hashlib.pbkdf2_hmac(
        "sha512",
        b"test test test test test test test test test test test junk",
        b"mnemonic",
        2048,
    )
    i = hmac.new(b"Bitcoin seed", seed, hashlib.sha512).digest()
    k, c = int.from_bytes(i[:32], "big"), i[32:]
    for n in (44 + 2**31, 60 + 2**31, 2**31, 0, index):
        if n >= 2**31:
            data = b"\x00" + k.to_bytes(32, "big") + n.to_bytes(4, "big")
        else:
            pt = _mul(k)
            data = (
                bytes([2 + (pt[1] & 1)])
                + pt[0].to_bytes(32, "big")
                + n.to_bytes(4, "big")
            )
        i = hmac.new(c, data, hashlib.sha512).digest()
        k, c = (int.from_bytes(i[:32], "big") + k) % _N, i[32:]
    return "0x%064x" % k


# ------------------------------------------------------------------ devnet/wsl: env, peers, launch, health
class DevnetTopologyNTest(unittest.TestCase):
    def env(self, n):
        return bash(
            'source "$1" && echo "PORTS=$METRICS_PORTS" && echo "RPCS=$RPC_URLS" && echo "N=$N_VALIDATORS"',
            os.path.join(WSL, "env.sh"),
            env={"N_VALIDATORS": str(n)},
        )

    def test_five_to_seven(self):
        for n in (3, 4, 5, 6, 7):
            r = self.env(n)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn(
                "PORTS=%s\n" % " ".join(str(9161 + i) for i in range(n)), r.stdout
            )
            self.assertIn(
                "RPCS=%s\n"
                % " ".join("http://localhost:%d" % (8645 + i) for i in range(n)),
                r.stdout,
            )
            self.assertIn("N=%d\n" % n, r.stdout)

    def test_out_of_range_is_refused(self):
        for n in ("2", "8", "x", "04"):
            r = self.env(n)
            self.assertNotEqual(r.returncode, 0, n)
            self.assertIn("N_VALIDATORS must be 3 to 7", r.stderr, n)

    def test_ports_and_peer_ids(self):
        r = bash(
            'source "$1"; for i in 0 1 2 3 4 5 6; do k=KEY$i p=PID$i a=P2P$i b=RPC$i c=MET$i; '
            'echo "${!k} ${!p} ${!a} ${!b} ${!c}"; done',
            os.path.join(WSL, "env.sh"),
            env={"N_VALIDATORS": "7"},
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        rows = [ln.split() for ln in r.stdout.splitlines()]
        self.assertEqual(len(rows), 7)
        for i, (key, pid, p2p, rpc, met) in enumerate(rows):
            self.assertEqual(key, "%02x" % (i + 1) + "00" * 31)
            self.assertEqual(
                (int(p2p), int(rpc), int(met)), (30401 + i, 8645 + i, 9161 + i)
            )
            pub = ed25519_pub(i)
            if pub is not None:
                self.assertEqual(peer_id(pub), pid, i)

    def test_node_peers_full_mesh(self):
        r = bash(
            'source "$1"; source "$2"; for i in 0 1 2 3 4 5 6; do echo "$(node_peers $i)"; done; '
            "node_peers 7 || echo REFUSED",
            os.path.join(WSL, "env.sh"),
            os.path.join(WSL, "start-node.sh"),
            env={"N_VALIDATORS": "7"},
        )
        lines = r.stdout.splitlines()
        pids = bash(
            'source "$1"; for i in 0 1 2 3 4 5 6; do p=PID$i; echo "${!p}"; done',
            os.path.join(WSL, "env.sh"),
        ).stdout.split()
        v = lambda i: "/ip4/127.0.0.1/udp/%d/quic-v1/p2p/%s" % (30401 + i, pids[i])
        want = (
            [v(1), v(0)]
            + [",".join(v(j) for j in range(i)) for i in range(2, 7)]
            + ["REFUSED"]
        )
        self.assertEqual(lines, want)


class LaunchNTest(unittest.TestCase):
    """launch-3val.sh in a sandbox copy of devnet/wsl with a fake torus-node that records its argv"""

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="launchn-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        wsl = os.path.join(self.d, "devnet", "wsl")
        os.makedirs(wsl)
        for f in ("env.sh", "start-node.sh", "launch-3val.sh", "health-3val.sh"):
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
        self.wsl = wsl

    def run_launch(self, n, root="dr"):
        env = {"DATA_ROOT": os.path.join(self.d, root), "N_VALIDATORS": str(n)}
        r = bash('"$1"; sleep 0.5', os.path.join(self.wsl, "launch-3val.sh"), env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        dr = env["DATA_ROOT"]
        argv = {}
        for f in sorted(os.listdir(dr)):
            if f.startswith("argv-"):
                argv[f[5:7]] = open(os.path.join(dr, f)).read().split("\n")
        pids = open(os.path.join(dr, "run", "pids")).read().split()
        return r.stdout, argv, pids

    def test_seven_nodes(self):
        out, argv, pids = self.run_launch(7)
        self.assertEqual(sorted(argv), ["%02x" % i for i in range(1, 8)])
        self.assertEqual(len(pids), 7)
        self.assertIn("val{0,1,2,3,4,5,6}.log", out)
        a6 = argv["07"]
        self.assertIn("--rpc-addr=127.0.0.1:8651", a6)
        self.assertIn("--metrics-addr=127.0.0.1:9167", a6)
        self.assertIn("--p2p-listen=/ip4/127.0.0.1/udp/30407/quic-v1", a6)
        peers = [a for a in a6 if a.startswith("--p2p-peers=")][0]
        self.assertEqual(peers.count("/p2p/"), 6)
        self.assertTrue(any(a.endswith("/data/val6") for a in a6))
        # val0..val3 keep exactly the 4-node argv
        _, argv4, _ = self.run_launch(4, root="dr4")
        for k in ("01", "02", "03", "04"):
            self.assertEqual([a.replace("/dr4/", "/dr/") for a in argv4[k]], argv[k], k)

    def test_three_and_four_launch_text_unchanged(self):
        out, _, _ = self.run_launch(3, root="d3")
        self.assertIn(
            "logs:          %s/val{0,1,2}.log\n" % os.path.join(self.d, "d3", "run"),
            out,
        )
        out, _, _ = self.run_launch(4, root="d4")
        self.assertIn(
            "logs:          %s/val{0,1,2,3}.log\n" % os.path.join(self.d, "d4", "run"),
            out,
        )

    def test_health_labels_every_node(self):
        r = bash(
            '"$1"',
            os.path.join(self.wsl, "health-3val.sh"),
            env={
                "N_VALIDATORS": "7",
                "WINDOW": "0",
                "DATA_ROOT": os.path.join(self.d, "h"),
            },
        )
        for i in range(7):
            self.assertIn("  val%d  height=" % i, r.stdout, r.stdout + r.stderr)
        self.assertIn("(<6)", r.stdout)


# ------------------------------------------------------------------ account / signer key files
class KeyFilesNTest(unittest.TestCase):
    def test_files_extend_each_other(self):
        for kind in ("bench-validator-keys.json", "bench-oracle-signer-keys.json"):
            prev = None
            for n in (3, 4, 5, 6, 7):
                with open(keys_file(n, kind)) as f:
                    v = json.load(f)["validators"]
                self.assertEqual([e["index"] for e in v], list(range(n)), (n, kind))
                if prev is not None:
                    self.assertEqual(v[: n - 1], prev, (n, kind))
                prev = v
        for n in (5, 6, 7):
            self.assertEqual(
                [i for i, _, _ in sa.read_keys(keys_file(n))], list(range(n))
            )

    def test_new_keys_are_the_documented_mnemonic_indices(self):
        """val4..val6 = hardhat dev mnemonic 108..110, their oracle signers 111..113; every address is its key's"""
        with open(keys_file(7)) as f:
            v = json.load(f)["validators"]
        with open(keys_file(7, "bench-oracle-signer-keys.json")) as f:
            s = json.load(f)["validators"]
        for i in (4, 5, 6):
            self.assertEqual(v[i]["private_key"], hardhat_key(104 + i), i)
            self.assertEqual(s[i]["private_key"], hardhat_key(107 + i), i)
        addrs = []
        for e in v + s:
            self.assertEqual(eth_address(e["private_key"]), e["address"], e["index"])
            addrs.append(e["address"])
        self.assertEqual(len(set(addrs)), 14)


# ------------------------------------------------------------------ gen-3val-genesis.sh end to end (tiny base)
class GenesisNTest(unittest.TestCase):
    """the real generator in a sandbox repo with a 2-account weighted-full genesis"""

    FILES = (
        "devnet/wsl/gen-3val-genesis.sh",
        "devnet/wsl/bench-validator-keys.json",
        "devnet/wsl/keys-4val/bench-validator-keys.json",
        "devnet/wsl/keys-5val/bench-validator-keys.json",
        "devnet/wsl/keys-6val/bench-validator-keys.json",
        "devnet/wsl/keys-7val/bench-validator-keys.json",
        "devnet/genesis.json",
        "testnet/lib/cargo-bin.sh",
    )

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="genn-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        for rel in self.FILES:
            os.makedirs(os.path.dirname(os.path.join(self.d, rel)), exist_ok=True)
            if os.path.exists(os.path.join(REPO, rel)):
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

    def gen(self, script="gen-3val-genesis.sh", **env):
        out = os.path.join(self.d, "out.json")
        if os.path.exists(out):
            os.remove(out)
        r = bash(
            '"$1"',
            os.path.join(self.d, "devnet/wsl", script),
            env=dict(env, OUT=out, BENCH_BIN="/bin/true", BULK_COUNT="2", MARKETS="2"),
        )
        if r.returncode != 0:
            return r, None
        with open(out, "rb") as f:
            return r, f.read()

    def test_five_and_seven(self):
        with open(os.path.join(REPO, "devnet", "genesis.json")) as f:
            base = json.load(f)["validators"]
        for n in (5, 6, 7):
            r, g = self.gen(N_VALIDATORS=str(n))
            self.assertEqual(r.returncode, 0, r.stderr)
            vals = json.loads(g)["validators"]
            with open(keys_file(n)) as f:
                addrs = [v["address"] for v in json.load(f)["validators"]]
            self.assertEqual([v["address"] for v in vals], addrs)
            self.assertEqual([v["stake"] for v in vals], [BASE_STAKE] * n)
            for i, v in enumerate(vals):
                if i < 4:
                    self.assertEqual(v, dict(base[i], address=addrs[i]), i)
                else:  # val4..: the 4th entry's fields with key 0<i+1>'s pubkey
                    self.assertEqual(list(v), list(base[3]), i)
                    self.assertEqual(v["commission_bps"], base[3]["commission_bps"])
                    pub = ed25519_pub(i)
                    if pub is not None:
                        self.assertEqual(v["pubkey"], "0x" + pub.hex(), i)
            self.assertEqual(len({v["pubkey"] for v in vals}), n)
            self.assertIn("validators = %d" % n, r.stdout)

    def test_seven_with_stakes(self):
        r, g = self.gen(N_VALIDATORS="7", VALIDATOR_STAKES="1,2,3,4,5,6,7")
        self.assertEqual(r.returncode, 0, r.stderr)
        vals = json.loads(g)["validators"]
        self.assertEqual(
            [v["stake"] for v in vals], [str(w) + BASE_STAKE[1:] for w in range(1, 8)]
        )

    def test_bad_input_is_fatal(self):
        for env, msg in (
            ({"N_VALIDATORS": "8"}, "N_VALIDATORS must be 3 to 7"),
            ({"N_VALIDATORS": "2"}, "N_VALIDATORS must be 3 to 7"),
            (
                {"N_VALIDATORS": "5", "VALIDATOR_STAKES": "1,1,1,1"},
                "5 positive integer weights",
            ),
            (
                {"N_VALIDATORS": "7", "VALIDATOR_STAKES": "1,1,1,1,1,1,1,1"},
                "7 positive integer weights",
            ),
            (
                {"N_VALIDATORS": "4", "VALIDATOR_STAKES": "2,2,1"},
                "four positive integer weights",
            ),
            ({"VALIDATOR_STAKES": "2,2,1,1"}, "three positive integer weights"),
        ):
            r, _ = self.gen(**env)
            self.assertNotEqual(r.returncode, 0, env)
            self.assertIn(msg, r.stderr, env)

    def test_three_and_four_byte_identical_to_the_pre_change_generator(self):
        old = subprocess.run(
            [
                "git",
                "-C",
                REPO,
                "show",
                "%s:devnet/wsl/gen-3val-genesis.sh" % PRE_NVAL_COMMIT,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if old.returncode != 0:
            self.skipTest(
                "commit %s not in this clone: %s"
                % (PRE_NVAL_COMMIT, old.stderr.strip())
            )
        p = os.path.join(self.d, "devnet", "wsl", "gen-old.sh")
        with open(p, "w") as f:
            f.write(old.stdout)
        os.chmod(p, 0o755)
        for env in (
            {},
            {"N_VALIDATORS": "3"},
            {"VALIDATOR_STAKES": "2,2,1"},
            {"N_VALIDATORS": "4"},
            {"N_VALIDATORS": "4", "VALIDATOR_STAKES": "2,2,1,1"},
            {"N_VALIDATORS": "4", "DL_K": "3", "DL_JCAP": "2"},
        ):
            with self.subTest(**env):
                rn, gn = self.gen(**env)
                ro, go = self.gen("gen-old.sh", **env)
                self.assertEqual(
                    (rn.returncode, ro.returncode), (0, 0), rn.stderr + ro.stderr
                )
                self.assertEqual(gn, go)
                self.assertEqual(rn.stdout, ro.stdout)


class GenesisCacheKeyNTest(unittest.TestCase):
    def test_key_files_cover_every_count(self):
        spec = importlib.util.spec_from_file_location(
            "genesis_cache", os.path.join(HERE, "genesis_cache.py")
        )
        gc = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gc)
        for n in (4, 5, 6, 7):
            self.assertIn(
                "devnet/wsl/keys-%dval/bench-validator-keys.json" % n, gc.KEY_FILES
            )


# ------------------------------------------------------------------ crash-kill.sh guard + quorum
class CrashKillNTest(unittest.TestCase):
    def quorum(self, idx, stakes):
        return lib('crash_norestart_quorum_ok "$@"', str(idx), stakes).returncode == 0

    def test_quorum_five_to_seven(self):
        for n in (5, 6, 7):
            for idx in range(1, n):
                self.assertTrue(self.quorum(idx, ",".join(["1"] * n)), (n, idx))
            self.assertFalse(self.quorum(0, ",".join(["1"] * n)), n)
            self.assertFalse(self.quorum(n, ",".join(["1"] * n)), n)
        self.assertTrue(self.quorum(4, "2,2,2,1,1"))  # 7/8
        self.assertFalse(self.quorum(1, "3,3,1,1,1"))  # 6/9 = 2/3: halt
        self.assertFalse(self.quorum(1, "1,1,1,1,1,1,1,1"))  # 8 weights
        self.assertFalse(self.quorum(1, "1,1,0,1,1"))

    def test_three_and_four_unchanged(self):
        self.assertTrue(self.quorum(2, "2,2,1"))
        self.assertFalse(self.quorum(2, "1,1,1"))
        self.assertFalse(self.quorum(3, "2,2,1"))
        self.assertTrue(self.quorum(3, "2,2,1,1"))
        self.assertFalse(self.quorum(1, "2,2,1,1"))
        self.assertFalse(self.quorum(4, "1,1,1,1"))

    def test_guard_takes_a_high_index_only_when_the_devnet_has_it(self):
        d = tempfile.mkdtemp(prefix="ckn-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        pidsf = os.path.join(d, "pids")
        open(pidsf, "w").write("11\n12\n13\n14\n15\n16\n4242\n")
        for idx in (4, 5, 6):
            cmd = (
                "/x/target/release/torus-node --data-dir=%s/data/val%d --rpc-addr=127.0.0.1:%d"
                % (d, idx, 8645 + idx)
            )
            for n in (None, "3", "4", "5", "6", "7"):
                env = {} if n is None else {"N_VALIDATORS": n}
                r = lib(
                    'crash_target_ok "$@"', str(idx), "4242", cmd, d, pidsf, env=env
                )
                ok = n is not None and int(n) > idx
                self.assertEqual(r.returncode == 0, ok, (idx, n, r.stderr))
        r = lib(
            'crash_target_ok "$@"',
            "7",
            "4242",
            "torus-node --data-dir=%s/data/val7" % d,
            d,
            pidsf,
            env={"N_VALIDATORS": "7"},
        )
        self.assertNotEqual(r.returncode, 0)


# ------------------------------------------------------------------ run-cell.sh pre-flight pieces
class RunCellNTest(unittest.TestCase):
    PRE_START = "# ---- no-restart kill (KILL_NO_RESTART) pre-flight."
    PRE_END = '[ -z "$KNR_IDX" ] || KNR_EXCLUDE_ARGS=(--exclude "$KILL_NO_RESTART")\n'

    def preflight(self, **env):
        idx = src_between(RUN_CELL_SH, "ALL_IDX=(0 1 2)\n", 'ALL_IDX+=("$i"); done\n')
        rpcs = src_between(
            RUN_CELL_SH,
            "RPCS=(http://127.0.0.1:8645",
            'RPCS+=("http://127.0.0.1:$((8645 + i))"); done\n',
        )
        script = (
            "set -uo pipefail\nN_VALIDATORS=${N_VALIDATORS:-3}\n"
            + idx
            + src_between(RUN_CELL_SH, "bench_rpc_urls() {", "\n}\n")
            + rpcs
            + "BENCH_RPC_URLS=$(bench_rpc_urls) || exit 2\n"
            + 'evm_load_on() { [ "${EVM_ON:-0}" = 1 ]; }\n'
            + "DUR=${DUR:-600}; CRASH_KILL_AT_S=${CRASH_KILL_AT_S:-}; ORACLE_FEED=${ORACLE_FEED:-0}\n"
            + "KILL_NO_RESTART=${KILL_NO_RESTART:-}; KILL_NO_RESTART_AT_S=${KILL_NO_RESTART_AT_S:-}\n"
            + "VALIDATOR_STAKES=${VALIDATOR_STAKES:-}; DL_REPORT=${DL_REPORT:-}; TOOLS_FROM_WORKTREE=1; SELF_DIR=$TOOLS_DIR\n"
            + src_between(RUN_CELL_SH, self.PRE_START, self.PRE_END)
            + 'echo "ALL=${ALL_IDX[*]}"; echo "LIVE=${LIVE_IDX[*]}"; echo "LIVE_RPCS=${LIVE_RPCS[*]}"; '
            'echo "BENCH=$BENCH_RPC_URLS"; echo "EXCL=${KNR_EXCLUDE_ARGS[*]+${KNR_EXCLUDE_ARGS[*]}}"; '
            'echo "KNR_IDX=$KNR_IDX"; echo "DLR=$DL_REPORT"\n'
        )
        base = {k: v for k, v in os.environ.items() if k not in CLEAN_ENV}
        r = subprocess.run(
            ["bash", "-c", script],
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(base, TOOLS_DIR=HERE, **env),
        )
        return r, dict(l.split("=", 1) for l in r.stdout.splitlines() if "=" in l)

    def test_all_up_five_to_seven(self):
        for n in (3, 4, 5, 6, 7):
            r, v = self.preflight(N_VALIDATORS=str(n))
            self.assertEqual(r.returncode, 0, r.stderr)
            idx = " ".join(str(i) for i in range(n))
            self.assertEqual(
                (v["ALL"], v["LIVE"], v["EXCL"], v["DLR"]), (idx, idx, "", "0")
            )
            self.assertEqual(
                v["BENCH"],
                ",".join("http://127.0.0.1:%d" % (8645 + i) for i in range(n)),
            )
        r, v = self.preflight(N_VALIDATORS="7", BENCH_RPCS="0,6")
        self.assertEqual(
            (r.returncode, v["BENCH"]),
            (0, "http://127.0.0.1:8645,http://127.0.0.1:8651"),
            r.stderr,
        )
        r, _ = self.preflight(N_VALIDATORS="5", BENCH_RPCS="0,5")
        self.assertEqual(r.returncode, 2, r.stderr)

    KNR7 = dict(
        N_VALIDATORS="7",
        KILL_NO_RESTART="val6",
        KILL_NO_RESTART_AT_S="30",
        DUR="120",
        BENCH_RPCS="0,1,2,3,4,5",
    )

    def test_kill_val6_of_seven(self):
        r, v = self.preflight(**self.KNR7)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            (v["LIVE"], v["EXCL"], v["KNR_IDX"], v["DLR"]),
            ("0 1 2 3 4 5", "--exclude val6", "6", "1"),
        )
        r, v = self.preflight(
            **dict(
                self.KNR7,
                N_VALIDATORS="5",
                KILL_NO_RESTART="val4",
                BENCH_RPCS="0,1",
                VALIDATOR_STAKES="2,2,2,1,1",
            )
        )
        self.assertEqual((r.returncode, v["LIVE"]), (0, "0 1 2 3"), r.stderr)

    def test_refusals(self):
        cases = {
            "val5 needs a devnet of 6 or more": (
                dict(
                    self.KNR7,
                    N_VALIDATORS="5",
                    KILL_NO_RESTART="val5",
                    BENCH_RPCS="0,1",
                ),
                "KILL_NO_RESTART must be val1 or val2 or val3 or val4 (never val0",
            ),
            "3,3,1,1,1 with a stake-3 node down halts": (
                dict(
                    self.KNR7,
                    N_VALIDATORS="5",
                    KILL_NO_RESTART="val1",
                    BENCH_RPCS="0,2",
                    VALIDATOR_STAKES="3,3,1,1,1",
                ),
                "the 4 validators left would hold no consensus quorum (> 2/3 of the power) under stakes 3,3,1,1,1",
            ),
            "never val0": (dict(self.KNR7, KILL_NO_RESTART="val0"), "never val0"),
            "bench sends to val6": (
                dict(self.KNR7, BENCH_RPCS="all"),
                "the bench sends to it",
            ),
        }
        for what, (env, msg) in cases.items():
            with self.subTest(what):
                r, _ = self.preflight(**env)
                self.assertEqual(r.returncode, 2, (what, r.stdout, r.stderr))
                self.assertIn(msg, r.stderr)

    def test_three_and_four_messages_unchanged(self):
        r, _ = self.preflight(
            KILL_NO_RESTART="val0", KILL_NO_RESTART_AT_S="30", DUR="120"
        )
        self.assertIn(
            "FATAL: KILL_NO_RESTART must be val1 or val2 (never val0 — it serves the bench RPC and every "
            "headline number)",
            r.stderr,
        )
        r, _ = self.preflight(
            N_VALIDATORS="4",
            KILL_NO_RESTART="val0",
            KILL_NO_RESTART_AT_S="30",
            DUR="120",
        )
        self.assertIn(
            "FATAL: KILL_NO_RESTART must be val1 or val2 or val3 (never val0", r.stderr
        )
        r, _ = self.preflight(
            N_VALIDATORS="4",
            KILL_NO_RESTART="val1",
            KILL_NO_RESTART_AT_S="30",
            DUR="120",
            BENCH_RPCS="0,2,3",
            VALIDATOR_STAKES="2,2,1,1",
        )
        self.assertIn(
            "FATAL: KILL_NO_RESTART=val1: the three validators left would hold no consensus quorum (> 2/3 "
            "of the power) under stakes 2,2,1,1 (VALIDATOR_STAKES): the chain would halt at the kill. "
            "Equal stakes (unset) keep 3/4; 2,2,1,1 needs val2 or val3 down",
            r.stderr,
        )

    def run_script(self, env, extra=None):
        tmp = tempfile.mkdtemp(prefix="rcn-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        tgt = os.path.join(tmp, "release")
        os.makedirs(tgt)
        for b in ("torus-node", "bench-throughput"):
            p = os.path.join(tgt, b)
            open(p, "w").write("#!/bin/sh\nexit 0\n")
            os.chmod(p, 0o755)
        script, wt = sandbox_repo(tmp)
        base = {k: v for k, v in os.environ.items() if k not in CLEAN_ENV}
        r = subprocess.run(
            [script, wt, "rcn-x", "10", "120", "1000"] + ([extra] if extra else []),
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
        self.assertFalse(
            os.path.exists(os.path.join(tmp, "rcn-x")), "must fail before any launch"
        )
        return r

    def test_script_preflight(self):
        for env, rc, msg in (
            (dict(N_VALIDATORS="8"), 2, "FATAL: N_VALIDATORS must be 3 to 7 (got '8')"),
            (dict(N_VALIDATORS="2"), 2, "FATAL: N_VALIDATORS must be 3 to 7 (got '2')"),
            (dict(N_VALIDATORS="5"), 1, "has no 5-validator devnet"),
            (dict(N_VALIDATORS="7"), 1, "has no 7-validator devnet"),
            (dict(N_VALIDATORS="4"), 1, "has no 4-validator devnet"),
            (
                dict(N_VALIDATORS="5", VALIDATOR_STAKES="1,1,1,1"),
                2,
                "VALIDATOR_STAKES must be 5 positive integer stake weights with N_VALIDATORS=5 (val0..val4; got '1,1,1,1')",
            ),
            (
                dict(N_VALIDATORS="4", VALIDATOR_STAKES="1,1,1"),
                2,
                "VALIDATOR_STAKES must be four positive integer stake weights a,b,c,d with N_VALIDATORS=4 (val0..val3; "
                "got '1,1,1')",
            ),
        ):
            with self.subTest(**env):
                r = self.run_script(env)
                self.assertEqual(r.returncode, rc, (env, r.stderr))
                self.assertIn(msg, r.stderr, env)

    def test_extra_env_refuses_the_knob(self):
        r = self.run_script({}, extra="N_VALIDATORS=7")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("a run-cell.sh knob", r.stderr)

    def test_helper_tools_carry_the_n_marker(self):
        """pre-flight greps these for 'N_VALIDATORS=3..7' before a 5..7-node cell"""
        for f in ("summarize.py", "health.py", "collect_logs.py", "crash-kill.sh"):
            with open(os.path.join(HERE, f)) as fh:
                self.assertIn("N_VALIDATORS=3..7", fh.read(), f)
        with open(os.path.join(WSL, "start-node.sh")) as fh:
            self.assertIn("N_VALIDATORS=3..7", fh.read())
        with open(RUN_CELL_SH) as fh:
            src = fh.read()
        self.assertIn("grep -q 'N_VALIDATORS=3..7'", src)


class ExpansionNTest(unittest.TestCase):
    """every N-aware run-cell.sh expression: 3 and 4 give the literals test_four_validators pins, 5..7 extend them"""

    def expand(self, n):
        src = open(RUN_CELL_SH).read()
        line = lambda start: src[
            src.index(start) : src.index("\n", src.index(start)) + 1
        ]
        script = (
            "set -uo pipefail\nN_VALIDATORS=%s; MARKETS=300; ORACLE_FEED_DRAIN_MAX_LAG=2; RUN_DIR=/R\n"
            % n
        )
        script += ". %s\n" % os.path.join(HERE, "log-scans.sh")
        script += src_between(
            RUN_CELL_SH, "ALL_IDX=(0 1 2)\n", 'ALL_IDX+=("$i"); done\n'
        )
        for start in (
            "RPCS=(http://127.0.0.1:8645",
            'for i in "${ALL_IDX[@]:3}"; do RPCS+=',
            "METS=(9161 9162 9163)",
            'for i in "${ALL_IDX[@]:3}"; do METS+=',
            "METS_URLS=()",
            "MIN_PEERS=",
            "DRAIN_URLS=(",
            "    FEED_DRAIN_ARGS=(--feed-live",
            "    ATTEST_DRAIN_ARGS=(--attest-live",
            "NODE_LOGS=();",
            "    SA_ARGS=(); for i in",
        ):
            script += line(start)
        script += src_between(
            RUN_CELL_SH,
            '    for i in "${ALL_IDX[@]}"; do SA_ARGS+=(--metrics',
            'SA_ARGS+=(--log "$RUN_DIR/val$i.log"); done\n',
        )
        i = src.index('--metrics-urls "') + len('--metrics-urls "')
        pm = src[i : src.index('" \\\n', i)]
        script += (
            'echo "PRELOAD_METS=' + pm + '"\n'
            'echo "DRAIN=${DRAIN_URLS[*]}"\necho "FEED=${FEED_DRAIN_ARGS[*]}"\necho "ATTEST=${ATTEST_DRAIN_ARGS[*]}"\n'
            'echo "HEALTH=all ${#ALL_IDX[@]} committing, peers>=$MIN_PEERS"\necho "LOGS=${NODE_LOGS[*]}"\n'
            'echo "SA=${SA_ARGS[*]}"\necho "STAKES=$(IFS=,; echo "${ALL_IDX[*]/#/val}")"\n'
            'echo "NODES=$(node_indices | tr "\\n" " ")"\n'
        )
        r = subprocess.run(
            ["bash", "-c", script], capture_output=True, text=True, timeout=30
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return dict(l.split("=", 1) for l in r.stdout.splitlines())

    def test_three_and_four(self):
        v = self.expand(3)
        self.assertEqual(
            v["PRELOAD_METS"],
            ",".join("http://127.0.0.1:%d/metrics" % p for p in (9161, 9162, 9163)),
        )
        self.assertEqual(v["HEALTH"], "all 3 committing, peers>=2")
        self.assertEqual(v["NODES"], "0 1 2 ")
        v = self.expand(4)
        self.assertTrue(v["PRELOAD_METS"].endswith(",http://127.0.0.1:9164/metrics"))
        self.assertEqual(v["HEALTH"], "all 4 committing, peers>=3")
        self.assertEqual(v["NODES"], "0 1 2 3 ")

    def test_seven(self):
        v = self.expand(7)
        m = ["http://127.0.0.1:%d/metrics" % (9161 + i) for i in range(7)]
        rpc = ["http://127.0.0.1:%d" % (8645 + i) for i in range(7)]
        self.assertEqual(v["PRELOAD_METS"], ",".join(m))
        self.assertEqual(v["DRAIN"], " ".join(m))
        self.assertEqual(
            v["ATTEST"],
            "--attest-live --attest-mempool-max 14 --attest-rpc " + " ".join(rpc),
        )
        self.assertIn("--feed-mempool-max %d " % (14 * ((300 + 255) // 256)), v["FEED"])
        self.assertEqual(v["HEALTH"], "all 7 committing, peers>=6")
        self.assertEqual(v["LOGS"], " ".join("/R/val%d.log" % i for i in range(7)))
        self.assertEqual(v["SA"].count("--rpc "), 7)
        self.assertEqual(v["STAKES"], ",".join("val%d" % i for i in range(7)))
        self.assertEqual(v["NODES"], "0 1 2 3 4 5 6 ")


class ResetArtifactsNTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="reset-n-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def reset(self, n):
        fn = src_between(RUN_CELL_SH, "reset_run_artifacts() {", "\n}\n")
        r = bash(
            "set -uo pipefail\n" + fn + '\nreset_run_artifacts "$1" "$2" "$3" "$4"',
            self.d,
            "c",
            "",
            str(n),
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(self.d, "run-id.json")) as f:
            return json.load(f)

    def test_a_smaller_run_clears_the_higher_nodes(self):
        for n in (3, 4, 5, 6, 7):
            files = ["metrics-before-val%d.txt" % i for i in range(7)] + [
                "val%d.log.gz" % i for i in range(7)
            ]
            for f in files:
                open(os.path.join(self.d, f), "w").write("x")
            self.assertEqual(self.reset(n)["n_validators"], n)
            for i in range(7):
                for f in ("metrics-before-val%d.txt" % i, "val%d.log.gz" % i):
                    self.assertEqual(
                        os.path.exists(os.path.join(self.d, f)), i < n, (n, f)
                    )


# ------------------------------------------------------------------ CLIs that take one entry per node
class CliNodeCountNTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="clin-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)
        srv = ThreadingHTTPServer(("127.0.0.1", 0), Advancing)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        base = "http://127.0.0.1:%d" % srv.server_address[1]
        self.urls = ["%s/n-val%d" % (base, i) for i in range(8)]

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

    def test_drain_takes_seven_and_excludes_val6(self):
        r = self.drain(self.urls[:7], "--exclude", "val6")
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(self.d, "drain.json")) as f:
            d = json.load(f)
        self.assertEqual(
            (d["nodes"], d["excluded_nodes"]),
            (["val%d" % i for i in range(6)], ["val6"]),
        )
        r = self.drain(self.urls[:5], "--exclude", "val4")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_drain_refusals(self):
        for urls, extra in (
            (self.urls, ()),
            (self.urls[:5], ("--exclude", "val5")),
            (self.urls[:2], ()),
        ):
            r = self.drain(urls, *extra)
            self.assertEqual(r.returncode, 2, (len(urls), extra, r.stderr))

    def collect(self, n):
        logs = []
        for i in range(n):
            p = os.path.join(self.d, "val%d.log" % i)
            open(p, "w").write("x HASH-ONLY manifest push\n" * i)
            logs.append(p)
        return subprocess.run(
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

    def test_collect_logs(self):
        r = self.collect(7)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("val6:manifest=6,", r.stdout)
        self.assertEqual(self.collect(8).returncode, 2)

    def test_sampler_argv(self):
        def run(urls):
            return subprocess.run(
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
                    *urls,
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )

        r = run(self.urls)
        self.assertEqual(r.returncode, 2)
        self.assertIn("3 to 7 metrics URLs", r.stderr)

    def test_state_attest_exclude_val6(self):
        rc = sa.main(
            [
                "scan",
                *sum((["--rpc", "http://127.0.0.1:1"] for _ in range(3)), []),
                "--keys",
                keys_file(3),
                "--exclude",
                "val6",
                "--out",
                os.path.join(self.d, "o"),
            ]
        )
        self.assertEqual(rc, 2)  # parsed (a choice), but not one of these 3 validators


class DeferredScansNTest(unittest.TestCase):
    def test_marker_and_end_pass_take_seven_logs(self):
        d = tempfile.mkdtemp(prefix="endpassn-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        r = bash(
            'source "$1"; for b in "10 20 30" "10 20 30 40" "10 20 30 40 50 60 70"; do '
            'write_deferred_marker "$2" 7 "$b"; cat "$2/deferred-scans.json"; done',
            os.path.join(HERE, "log-scans.sh"),
            d,
        )
        a, b, c = r.stdout.splitlines()
        self.assertEqual(
            a,
            '{"version": 1, "scans": "ingest-evictions log-excerpts", "bench_submitted": "7", '
            '"ingest_log_bytes": [10, 20, 30]}',
        )
        self.assertEqual(
            b,
            '{"version": 1, "scans": "ingest-evictions log-excerpts", "bench_submitted": "7", '
            '"ingest_log_bytes": [10, 20, 30, 40]}',
        )
        self.assertEqual(
            json.loads(c)["ingest_log_bytes"], [10, 20, 30, 40, 50, 60, 70]
        )
        import gzip

        for i in range(7):
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
            "evictions per node 0 0 0 4 5 6 7", r.stdout
        )  # 35-byte lines: 40+ bytes keep them
        self.assertTrue(os.path.exists(os.path.join(d, "val6.log.excerpt")))

    def test_node_indices(self):
        for n, want in (
            ("", "0 1 2"),
            ("3", "0 1 2"),
            ("4", "0 1 2 3"),
            ("6", "0 1 2 3 4 5"),
        ):
            r = bash(
                'source "$1"; node_indices | tr "\\n" " "',
                os.path.join(HERE, "log-scans.sh"),
                env={"N_VALIDATORS": n} if n else {},
            )
            self.assertEqual(r.stdout.strip(), want, n)


class RescoreNTest(unittest.TestCase):
    def test_counts(self):
        for n, ok in ((7, True), (5, True), (8, False), (2, False)):
            d = tempfile.mkdtemp(prefix="rescn-")
            self.addCleanup(shutil.rmtree, d, ignore_errors=True)
            src = os.path.join(d, "cell")
            os.makedirs(src)
            with open(os.path.join(src, "run-id.json"), "w") as f:
                json.dump({"n_validators": n}, f)
            r = subprocess.run(
                [os.path.join(HERE, "rescore.sh"), src, os.path.join(d, "dest")],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertNotEqual(r.returncode, 0)
            if ok:  # gets past the count to the missing summary.json
                self.assertIn("summary.json missing", r.stderr, n)
            else:
                self.assertIn("n_validators=%d (3 to 7)" % n, r.stderr, n)


class GapAttrNTest(unittest.TestCase):
    def test_node_logs_take_every_node(self):
        spec = importlib.util.spec_from_file_location(
            "gap_attr", os.path.join(HERE, "gap_attr.py")
        )
        ga = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(ga)
        d = tempfile.mkdtemp(prefix="gapn-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        for i in range(6):
            open(os.path.join(d, "val%d.log" % i), "w").write("")
        self.assertEqual(sorted(ga._node_logs(d)), ["val%d" % i for i in range(6)])


class RunCellPyPortsTest(unittest.TestCase):
    def test_ports_cover_every_node(self):
        with open(os.path.join(HERE, "campaign", "run_cell.py")) as f:
            src = f.read()
        self.assertNotIn('== "4"', src)
        self.assertIn("8645 + i", src)
        self.assertIn("9161 + i", src)


# ------------------------------------------------------------------ summarize.py with 7 validators
def write_n_cell(d, n, kill=None, dur=120):
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
    nodes = ["val%d" % i for i in range(n)]
    with open(os.path.join(d, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for s in range(dur + 1):
            for nd in nodes:
                if nd == kill and s > KILL_AT:
                    vals = [0] * len(cols)
                else:
                    to = 0 if s <= KILL_AT else min(s - KILL_AT, 20) // 2
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
                    "%d,%s,%s\n" % (BENCH_START + s, nd, ",".join(str(v) for v in vals))
                )
    open(os.path.join(d, "bench.log"), "w").write(
        "Submitted (load-gen accepted): 1,000\n"
    )
    for i in range(n):
        open(os.path.join(d, "metrics-before-val%d.txt" % i), "w").write(
            "torus_block_height 0\n"
        )
    with open(os.path.join(d, "agreement.jsonl"), "w") as f:
        for i, nd in enumerate(x for x in nodes if x != kill):
            f.write(
                json.dumps(
                    {
                        "node": nd,
                        "height": 1200 + i // 2,
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
    with open(os.path.join(d, "run-id.json"), "w") as f:
        json.dump(
            {
                "label": "t",
                "run_id": RUN_ID,
                "kill_no_restart": kill,
                "n_validators": n,
            },
            f,
        )
    if kill:
        write_kill_record(d, kill_node=kill)


class SummarizeNTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="summn-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def summ(self, n, heights):
        dissem = " ".join(
            "val%d:exhausted=0,sync_fallback=0,da_outbound_fail=0,starvation=0" % i
            for i in range(n)
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

    def test_all_seven_up(self):
        write_n_cell(self.d, 7)
        s, _ = self.summ(7, " ".join(str(1200 + i // 2) for i in range(7)))
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertEqual(
            sorted(s["liveness"]["nodes"]), ["val%d" % i for i in range(7)]
        )
        self.assertIn("val6", s["funnel_by_node"])

    def test_val6_killed_six_survivors(self):
        write_n_cell(self.d, 7, kill="val6")
        s, _ = self.summ(7, " ".join(str(1200 + i // 2) for i in range(6)))
        self.assertEqual(s["validity"]["verdict"], "ACCEPT", s["validity"])
        self.assertEqual(s["agreement"]["excluded_nodes"], ["val6"])
        self.assertEqual(
            s["kill_no_restart"]["survivors"], ["val%d" % i for i in range(6)]
        )

    def test_a_seven_node_cell_missing_a_row_is_incomplete(self):
        write_n_cell(self.d, 7)
        with open(os.path.join(self.d, "agreement.jsonl")) as f:
            rows = f.readlines()[:6]
        open(os.path.join(self.d, "agreement.jsonl"), "w").writelines(rows)
        s, _ = self.summ(7, " ".join(str(1200 + i // 2) for i in range(6)))
        self.assertEqual(s["agreement"]["agreement_verdict"], "INCOMPLETE")


class AgreementHeightSpreadNTest(unittest.TestCase):
    """s140 9v2 7p350-smoke: after the drain the chain keeps committing, so torus_block_height read in one
    scrape pass is up to 6-10 blocks apart on 7 nodes, with block hash at cmp_height, state digest and every
    counter equal (ozarchy-s140-9v2-7p350-smoke-s0/agreement.jsonl). The fixed `height_spread <= 5` guard
    scored it DISAGREE. N > 4 takes a wider limit; 3 / 4 nodes keep 5 (verdicts unchanged here)."""

    # the 7 head heights of ozarchy-s140-9v2-7p350-smoke-s0 (spread 6)
    SMOKE_HEADS = [6232, 6231, 6232, 6232, 6234, 6235, 6237]

    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="summspread-")
        self.addCleanup(shutil.rmtree, self.d, ignore_errors=True)

    def score(self, heads, digest_heights=None, mutate=None):
        """A len(heads)-node cell whose agreement rows differ only in head height (unless `mutate` changes
        row i: mutate(i, row)); returns summary.json["agreement"]."""
        n = len(heads)
        write_n_cell(self.d, n)
        path = os.path.join(self.d, "agreement.jsonl")
        with open(path) as f:
            rows = [json.loads(line) for line in f if line.strip()]
        for i, r in enumerate(rows):
            r["height"] = heads[i]
            r["cmp_height"] = min(heads) - 5
            if mutate:
                mutate(i, r)
        with open(path, "w") as f:
            f.writelines(json.dumps(r) + "\n" for r in rows)
        dh = digest_heights or [min(heads) + 100] * n
        dissem = " ".join(
            "val%d:exhausted=0,sync_fallback=0,da_outbound_fail=0,starvation=0" % i
            for i in range(n)
        )
        s, _ = run_summarize(
            self.d,
            dur=120,
            extra=[
                "--dissem",
                dissem,
                "--digest-quiescent",
                "1",
                "--digest-heights",
                " ".join(map(str, dh)),
            ],
        )
        return s["agreement"]

    def heads(self, n, spread):
        return [1200 + (spread if i == n - 1 else i % 2) for i in range(n)]

    def test_seven_smoke_head_lag_agrees(self):
        a = self.score(self.SMOKE_HEADS)
        self.assertEqual(a["height_spread"], 6)
        self.assertEqual(a["agreement_verdict"], "AGREE", a)
        self.assertIs(a["validators_agree"], True)

    def test_seven_spread_up_to_ten_agrees(self):
        for spread in (6, 8, 10):
            with self.subTest(spread=spread):
                a = self.score(self.heads(7, spread))
                self.assertEqual(a["height_spread"], spread)
                self.assertEqual(a["agreement_verdict"], "AGREE", a)

    def test_seven_records_the_limit(self):
        a = self.score(self.SMOKE_HEADS)
        self.assertEqual(a["height_spread_max"], 15)

    def test_seven_stalled_node_is_still_disagree(self):
        a = self.score(self.heads(7, 16))
        self.assertEqual(a["agreement_verdict"], "DISAGREE", a)

    def test_seven_state_digest_mismatch_is_disagree(self):
        def bad(i, r):
            if i == 4:
                r["state_digest"] = "other"

        for heads in (self.SMOKE_HEADS, self.heads(7, 0)):
            with self.subTest(heads=heads):
                a = self.score(heads, mutate=bad)
                self.assertFalse(a["state_digest_equal"])
                self.assertEqual(a["agreement_verdict"], "DISAGREE", a)
                self.assertIs(a["validators_agree"], False)

    def test_seven_block_hash_mismatch_is_disagree(self):
        def bad(i, r):
            if i == 6:
                r["block_hash"] = "0xbeef"

        a = self.score(self.SMOKE_HEADS, mutate=bad)
        self.assertFalse(a["block_hash_equal"])
        self.assertEqual(a["agreement_verdict"], "DISAGREE", a)

    def test_seven_action_counter_skew_with_head_lag_is_unverified(self):
        """the action-counter-skew-only branch takes the same limit: digests one block apart, only the
        action counter off, head spread 8 -> DIGEST_UNVERIFIED (not DISAGREE)."""

        def skew(i, r):
            if i == 3:
                r["actions"] += 1

        heads = self.heads(7, 8)
        a = self.score(
            heads, digest_heights=[1300 + i % 2 for i in range(7)], mutate=skew
        )
        self.assertTrue(a["action_counter_skew_only"], a)
        self.assertEqual(a["agreement_verdict"], "DIGEST_UNVERIFIED", a)

    def test_three_and_four_keep_five(self):
        for n in (3, 4):
            with self.subTest(n=n, spread=5):
                a = self.score(self.heads(n, 5))
                self.assertEqual(a["agreement_verdict"], "AGREE", a)
                self.assertNotIn("height_spread_max", a)
            with self.subTest(n=n, spread=6):
                a = self.score(self.heads(n, 6))
                self.assertEqual(a["agreement_verdict"], "DISAGREE", a)
                self.assertNotIn("height_spread_max", a)

    def test_four_action_counter_skew_with_spread_six_stays_disagree(self):
        def skew(i, r):
            if i == 3:
                r["actions"] += 1

        a = self.score(
            self.heads(4, 6),
            digest_heights=[1300 + i % 2 for i in range(4)],
            mutate=skew,
        )
        self.assertTrue(a["action_counter_skew_only"], a)
        self.assertEqual(a["agreement_verdict"], "DISAGREE", a)


if __name__ == "__main__":
    unittest.main()
