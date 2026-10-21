#!/usr/bin/env python3
"""test_genesis_cache.py — the genesis key and cache (genesis_cache.py) and run-cell.sh's reuse of it.

    python3 tools/matched-bench/test_genesis_cache.py

Pinned: the key changes when any genesis input changes (each generator file, the weighted-full base genesis and
its params sidecar, the BASE file, run-cell.sh, each cell param, each ambient env name the generator reads) and
is the same otherwise; every env name the generator scripts read is a key input or explicitly exempt; a cached
genesis whose md5 is not its meta's is never reused; FORCE=1 reuses only an entry a FORCE=1 cell built; and
run-cell.sh, run for real in a sandbox up to the launch, builds once, then reuses (the generator not called, the
same md5, the "genesis reused (key ..., md5 ...)" line), and rebuilds when an input changes.
"""

import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
GC = os.path.join(HERE, "genesis_cache.py")
RUN_CELL_SH = os.path.join(HERE, "run-cell.sh")


def load():
    spec = importlib.util.spec_from_file_location("genesis_cache", GC)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def md5(path):
    return hashlib.md5(open(path, "rb").read()).hexdigest()


PARAMS = {
    "MARKETS": "300",
    "BULK_COUNT": "1000000",
    "TIMEOUT_BASE_MS": "",
    "EVM_BLOCK_INTERVAL": "",
    "LIQ_THIN": "0",
    "LIQ_THIN_AVAIL": "1000000.0",
    "BOOK_MODE": "",
    "VALIDATOR_STAKES": "",
}


class KeyTest(unittest.TestCase):
    def setUp(self):
        self.gc = load()
        self.tmp = tempfile.mkdtemp(prefix="genesis-key-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.root = os.path.join(self.tmp, "repo")
        for rel in self.gc.KEY_FILES + (self.gc.DEFAULT_BASE,):
            p = os.path.join(self.root, rel)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "w") as f:
                f.write("content of %s\n" % rel)
        self.run_cell = os.path.join(self.tmp, "run-cell.sh")
        with open(self.run_cell, "w") as f:
            f.write("#!/bin/bash\n")

    def key(self, params=None, env=None):
        p = dict(PARAMS)
        p.update(params or {})
        return self.gc.key_of(
            self.gc.key_inputs(self.root, self.run_cell, p, env or {})
        )

    def test_same_inputs_same_key(self):
        self.assertEqual(self.key(), self.key())
        self.assertRegex(self.key(), r"^[0-9a-f]{%d}$" % self.gc.KEY_LEN)

    def test_each_file_changes_the_key(self):
        k0 = self.key()
        for rel in self.gc.KEY_FILES + (self.gc.DEFAULT_BASE,):
            with self.subTest(rel=rel):
                p = os.path.join(self.root, rel)
                old = open(p).read()
                with open(p, "a") as f:
                    f.write("x")
                self.assertNotEqual(self.key(), k0)
                with open(p, "w") as f:
                    f.write(old)
                self.assertEqual(self.key(), k0)
        with open(self.run_cell, "a") as f:
            f.write("# changed\n")
        self.assertNotEqual(
            self.key(), k0, "run-cell.sh (it patches the genesis) is a key input"
        )

    def test_absent_weighted_full_differs_from_present(self):
        k0 = self.key()
        os.remove(os.path.join(self.root, "testnet/genesis-weighted-full.json"))
        self.assertNotEqual(self.key(), k0)

    def test_each_param_changes_the_key(self):
        k0 = self.key()
        for name in self.gc.PARAMS:
            with self.subTest(name=name):
                self.assertNotEqual(self.key({name: PARAMS[name] + "7"}), k0)

    def test_each_ambient_env_changes_the_key(self):
        k0 = self.key()
        for name in self.gc.AMBIENT_ENV:
            with self.subTest(name=name):
                v = (
                    os.path.join(self.root, "other-base.json")
                    if name == "BASE"
                    else "5"
                )
                if name == "BASE":
                    with open(v, "w") as f:
                        f.write("another base\n")
                self.assertNotEqual(self.key(env={name: v}), k0)
        self.assertEqual(
            self.key(env={"STATE_HASH_ACTIVATION": ""}),
            k0,
            "empty = unset (${X:-default})",
        )
        self.assertEqual(self.key(env={"FUNDED_BULK_END": "100060", "OUT": "/x"}), k0)

    def test_missing_or_unknown_param_is_an_error(self):
        p = dict(PARAMS)
        del p["MARKETS"]
        with self.assertRaises(ValueError):
            self.gc.key_inputs(self.root, self.run_cell, p, {})
        p = dict(PARAMS, FOO="1")
        with self.assertRaises(ValueError):
            self.gc.key_inputs(self.root, self.run_cell, p, {})

    def test_generator_env_reads_are_key_inputs_or_exempt(self):
        """every ${NAME:-...} / ${NAME-...} the generator scripts read from the env is a key input or exempt"""
        gc = self.gc
        reads = set()
        for rel in (
            "devnet/wsl/gen-3val-genesis.sh",
            "testnet/gen-weighted-genesis.sh",
            "testnet/lib/cargo-bin.sh",
        ):
            reads |= set(
                re.findall(
                    r"\$\{([A-Z][A-Z0-9_]*):?-", open(os.path.join(REPO, rel)).read()
                )
            )
        self.assertIn("STATE_HASH_ACTIVATION", reads)  # the scan sees the scripts
        known = set(gc.PARAMS) | set(gc.AMBIENT_ENV) | set(gc.EXEMPT_ENV)
        self.assertEqual(sorted(reads - known), [])
        for rel in gc.KEY_FILES[:5]:
            self.assertTrue(os.path.exists(os.path.join(REPO, rel)), rel)
        self.assertTrue(os.path.exists(os.path.join(REPO, gc.DEFAULT_BASE)))

    def test_run_cell_passes_every_param(self):
        """run-cell.sh's key call names each PARAMS entry"""
        src = open(RUN_CELL_SH).read()
        for name in self.gc.PARAMS:
            self.assertRegex(src, r"--param %s=" % name)


class CacheTest(unittest.TestCase):
    def setUp(self):
        self.gc = load()
        self.tmp = tempfile.mkdtemp(prefix="genesis-cache-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.dir = os.path.join(self.tmp, "cache")
        self.gen = os.path.join(self.tmp, "genesis-3val.json")
        with open(self.gen, "w") as f:
            f.write('{"markets": [1, 2, 3]}\n')

    def gen_out(self, root="/old/repo", head=None):
        """the generator's run.log output for self.gen, as genesis_cache.generator_output keeps it"""
        if head is None:
            head = ["reusing existing %s/testnet/genesis-weighted-full.json (BULK_COUNT=3)" % root]
        lines = head + ["wrote " + self.gen, "  timeout_ms = 500", "  state_hash_activation = off"]
        return self.gc.generator_output(lines, self.gen, root)

    def store(self, key="k1", forced=False, gen_out=None):
        return self.gc.store(
            self.dir,
            key,
            self.gen,
            md5(self.gen),
            forced,
            {"markets": "3", "bulk_count": "3"},
            {"x": 1},
            self.gen_out() if gen_out is None else gen_out,
        )

    def test_hit_copies_and_verifies(self):
        self.store()
        dest = os.path.join(self.tmp, "dest.json")
        rc, meta = self.gc.fetch(self.dir, "k1", dest, False)
        self.assertEqual(rc, 0, meta)
        self.assertEqual(md5(dest), md5(self.gen))
        self.assertEqual(meta["md5"], md5(self.gen))
        self.assertEqual(meta["markets"], "3")
        self.assertEqual([f for f in os.listdir(self.tmp) if "tmp" in f], [])
        self.assertEqual(sorted(os.listdir(self.dir)), ["k1.genesis", "k1.meta.json"])

    def test_miss(self):
        dest = os.path.join(self.tmp, "dest.json")
        rc, why = self.gc.fetch(self.dir, "nokey", dest, False)
        self.assertEqual(rc, 1)
        self.assertIn("no cache entry", why)
        self.assertFalse(os.path.exists(dest))

    def test_md5_mismatch_is_never_reused(self):
        self.store()
        with open(os.path.join(self.dir, "k1.genesis"), "a") as f:
            f.write(" ")
        dest = os.path.join(self.tmp, "dest.json")
        with open(dest, "w") as f:
            f.write("old\n")
        rc, why = self.gc.fetch(self.dir, "k1", dest, False)
        self.assertEqual(rc, 2)
        self.assertIn("md5", why)
        self.assertEqual(open(dest).read(), "old\n", "dest untouched on a mismatch")
        self.assertEqual([f for f in os.listdir(self.tmp) if "cache-tmp" in f], [])

    def test_meta_without_genesis_or_bad_meta(self):
        self.store()
        os.remove(os.path.join(self.dir, "k1.genesis"))
        self.assertEqual(self.gc.fetch(self.dir, "k1", self.gen + ".d", False)[0], 1)
        self.store()
        with open(os.path.join(self.dir, "k1.meta.json"), "w") as f:
            f.write("{bad")
        self.assertEqual(self.gc.fetch(self.dir, "k1", self.gen + ".d", False)[0], 2)

    def test_forced_needs_a_forced_entry(self):
        self.store(forced=False)
        dest = os.path.join(self.tmp, "dest.json")
        rc, why = self.gc.fetch(self.dir, "k1", dest, True)
        self.assertEqual(rc, 1)
        self.assertIn("FORCE=1", why)
        self.assertEqual(self.gc.fetch(self.dir, "k1", dest, False)[0], 0)
        self.store(forced=True)
        self.assertEqual(self.gc.fetch(self.dir, "k1", dest, True)[0], 0)

    def test_store_refuses_a_wrong_md5(self):
        with self.assertRaises(ValueError):
            self.gc.store(self.dir, "k1", self.gen, "0" * 32, False, {}, {}, self.gen_out())
        self.assertFalse(os.path.exists(os.path.join(self.dir, "k1.meta.json")))

    def test_generator_output_splits_at_the_wrote_line(self):
        full = "/old/repo/testnet/genesis-weighted-full.json"
        head = [
            "building weighted-full genesis (BULK_COUNT=3 bulk senders)...",
            "wrote %s (100 bytes, native=63 accts=50), bulk_offset=60 bulk_count=3) + x.params.json" % full,
        ]
        g = self.gen_out(head=head)
        self.assertEqual(g["root"], "/old/repo")
        self.assertEqual(g["head"], head, "the weighted-full build's own 'wrote' line is head, not the split")
        self.assertEqual(g["summary"], ["  timeout_ms = 500", "  state_hash_activation = off"])
        with self.assertRaises(ValueError):
            self.gc.generator_output(["wrote /elsewhere.json", "  x = 1"], self.gen, "/old/repo")

    def test_replay_is_what_the_generator_prints_on_this_cell(self):
        built = self.gen_out(head=["building weighted-full genesis (BULK_COUNT=3 bulk senders)...", "noise"])
        meta = self.store(gen_out=built)
        dest = "/new/repo/devnet/wsl/genesis-3val.json"
        # not FORCE=1: the generator would find the weighted-full genesis (a key input) and reuse it
        self.assertEqual(
            self.gc.replay(meta, dest, "/new/repo", False),
            [
                "reusing existing /new/repo/testnet/genesis-weighted-full.json (BULK_COUNT=3)",
                "wrote " + dest,
                "  timeout_ms = 500",
                "  state_hash_activation = off",
            ],
        )
        # FORCE=1: the generator rebuilds the weighted-full genesis, the same bytes as the build: its head again
        forced = self.gen_out(
            head=["building weighted-full genesis (BULK_COUNT=3 bulk senders)...", "wrote /old/repo/testnet/f.json + /old/repo/p"]
        )
        meta = self.store(forced=True, gen_out=forced)
        self.assertEqual(
            self.gc.replay(meta, dest, "/new/repo", True)[:3],
            [
                "building weighted-full genesis (BULK_COUNT=3 bulk senders)...",
                "wrote /new/repo/testnet/f.json + /new/repo/p",
                "wrote " + dest,
            ],
        )

    def test_entry_without_generator_output_is_rebuilt(self):
        self.store()
        meta_p = os.path.join(self.dir, "k1.meta.json")
        meta = json.load(open(meta_p))
        del meta["generator_output"]
        json.dump(meta, open(meta_p, "w"))
        rc, why = self.gc.fetch(self.dir, "k1", self.gen + ".d", False)
        self.assertEqual(rc, 2)
        self.assertIn("generator output", why)

    def test_cli_store_and_fetch_replay(self):
        out = os.path.join(self.tmp, "gen.out")
        with open(out, "w") as f:
            f.write("reusing existing %s/testnet/genesis-weighted-full.json (BULK_COUNT=3)\n" % self.tmp)
            f.write("wrote %s\n  state_hash_activation = off\n" % self.gen)
        run_cell = os.path.join(self.tmp, "run-cell.sh")
        open(run_cell, "w").close()
        params = sum((["--param", "%s=%s" % kv] for kv in PARAMS.items()), [])
        common = ["--root", self.tmp, "--run-cell", run_cell] + params
        r = subprocess.run(
            [sys.executable, "-I", GC, "store", "--dir", self.dir, "--genesis", self.gen, "--md5", md5(self.gen),
             "--meta", "bulk_count=3", "--gen-out", out] + common,
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        key = r.stdout.strip()
        dest = os.path.join(self.tmp, "dest.json")
        r = subprocess.run(
            [sys.executable, "-I", GC, "fetch", "--dir", self.dir, "--key", key, "--dest", dest, "--root", self.tmp],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        meta = json.loads(r.stdout)
        self.assertEqual(
            meta["replay"],
            [
                "reusing existing %s/testnet/genesis-weighted-full.json (BULK_COUNT=3)" % self.tmp,
                "wrote " + dest,
                "  state_hash_activation = off",
            ],
        )
        self.assertNotIn("generator_output", meta)

    def test_cli_rc(self):
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                GC,
                "fetch",
                "--dir",
                self.dir,
                "--key",
                "k9",
                "--dest",
                self.gen + ".d",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("no cache entry", r.stderr)


# ---------------------------------------------------------------- run-cell.sh, run for real in a sandbox

FAKE_GEN = r"""#!/bin/bash
# fake gen-3val-genesis.sh: counts its calls, writes a MARKETS-market genesis with BULK_COUNT bulk rows
echo call >> "$GEN_CALLS"
python3 - "$OUT" "$MARKETS" "$BULK_COUNT" "${TIMEOUT_BASE_MS:-500}" "${BOOK_MODE:-}" <<'PY'
import json, sys
out, m, n, t, bm = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
g = {"consensus": dict({"timeout_base_ms": t}, **({"book_mode": bm} if bm else {})), "evm": {"chain_id": 7},
     "markets": [{"market_id": i} for i in range(1, m + 1)],
     "native_balances": [{"address": "0x%040x" % (60 + i), "note": "bulk-test %d" % (60 + i)} for i in range(n)]}
json.dump(g, open(out, "w"))
PY
echo "wrote $OUT"
"""


class RunCellReuseTest(unittest.TestCase):
    """run-cell.sh in a sandbox repo (a copy, so the real weighted-full genesis is never touched), with a fake
    generator and a launch-3val.sh that stops the cell (exit 1) right after the genesis step"""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="genesis-reuse-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        repo = os.path.join(self.tmp, "repo")
        self.repo = repo
        tools = os.path.join(repo, "tools", "matched-bench")
        os.makedirs(tools)
        # summarize.py + health.py: run-cell.sh asks the summarizer (--help) whether
        # it scores the genesis book mode, at preflight
        for f in ("run-cell.sh", "genesis_cache.py", "log-scans.sh", "summarize.py", "health.py"):
            shutil.copy2(os.path.join(HERE, f), tools)
        self.script = os.path.join(tools, "run-cell.sh")
        for rel in (
            "devnet/wsl/bench-validator-keys.json",
            "devnet/genesis.json",
            "testnet/lib/cargo-bin.sh",
            "testnet/gen-weighted-genesis.sh",
            "testnet/genesis-weighted-base.json",
        ):
            os.makedirs(os.path.dirname(os.path.join(repo, rel)), exist_ok=True)
            shutil.copy2(os.path.join(REPO, rel), os.path.join(repo, rel))
        with open(os.path.join(repo, "testnet/genesis-weighted-full.json"), "w") as f:
            json.dump(
                {
                    "native_balances": [
                        {
                            "address": "0x%040x" % (60 + i),
                            "note": "bulk-test %d" % (60 + i),
                        }
                        for i in range(5)
                    ]
                },
                f,
            )
        self.calls = os.path.join(self.tmp, "gen-calls")
        wsl = os.path.join(repo, "devnet", "wsl")
        for name, body in (
            ("gen-3val-genesis.sh", FAKE_GEN),
            ("launch-3val.sh", "#!/bin/sh\necho STOP-AT-LAUNCH; exit 1\n"),
            ("stop-3val.sh", "#!/bin/sh\nexit 0\n"),
        ):
            with open(os.path.join(wsl, name), "w") as f:
                f.write(body)
            os.chmod(os.path.join(wsl, name), 0o755)
        with open(os.path.join(tools, "digest-node.sh"), "w") as f:
            f.write("#!/bin/sh\nexit 0\n")
        os.chmod(os.path.join(tools, "digest-node.sh"), 0o755)
        tgt = os.path.join(self.tmp, "release")
        os.makedirs(tgt)
        for b in ("torus-node", "bench-throughput"):
            with open(os.path.join(tgt, b), "w") as f:
                f.write("#!/bin/sh\necho fake\n")
            os.chmod(os.path.join(tgt, b), 0o755)
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        with open(os.path.join(self.bin, "pgrep"), "w") as f:
            f.write("#!/bin/sh\nexit 1\n")
        os.chmod(os.path.join(self.bin, "pgrep"), 0o755)
        self.cache = os.path.join(self.tmp, "run", "genesis-cache")
        self.n = 0

    def run_cell(self, markets="3", extra_env=None, **env):
        self.n += 1
        label = "reuse-%d" % self.n
        e = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("LIQ_", "ORACLE_", "EXPECT_", "EVM_", "PRELOAD"))
            and k
            not in (
                "FORCE",
                "SENDERS",
                "BULK_COUNT",
                "GENESIS_CACHE_DIR",
                "STATE_HASH_ACTIVATION",
                "BASE",
                "BULK_OFFSET",
                "NATIVE_AVAIL",
                "EVM_WEI",
            )
        }
        e.update(
            PATH=self.bin + os.pathsep + os.environ["PATH"],
            TARGET_DIR=self.tmp,
            RESULTS_ROOT=os.path.join(self.tmp, "results"),
            DATA_ROOT=os.path.join(self.tmp, "data"),
            BENCH_ALLOW_UNDETACHED="1",
            SENDERS="5",
            BULK_COUNT="5",
            GEN_CALLS=self.calls,
            GENESIS_CACHE_DIR=self.cache,
        )
        e.update(env)
        argv = [self.script, self.repo, label, markets]
        if extra_env is not None:
            argv += ["60", "1000", extra_env]
        r = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=120,
            env=e,
        )
        log = open(os.path.join(self.tmp, "results", label, "run.log")).read()
        self.assertIn(
            "STOP-AT-LAUNCH", log, r.stdout + r.stderr + log
        )  # reached the launch, past the genesis
        return log

    def calls_n(self):
        return (
            len(open(self.calls).read().splitlines())
            if os.path.exists(self.calls)
            else 0
        )

    def gen_md5(self, log):
        return re.search(
            r"genesis markets=\d+ native_balances=\d+ bulk_count=\d+ md5=([0-9a-f]{32})",
            log,
        ).group(1)

    def test_build_then_reuse_then_rebuild_on_change(self):
        log1 = self.run_cell()
        self.assertEqual(self.calls_n(), 1)
        self.assertRegex(log1, r"genesis built \(key [0-9a-f]{20}, md5 [0-9a-f]{32}\)")
        m1 = self.gen_md5(log1)
        key1 = re.search(r"genesis built \(key ([0-9a-f]{20})", log1).group(1)
        log2 = self.run_cell()
        self.assertEqual(
            self.calls_n(), 1, "the generator must not run on a reuse:\n" + log2
        )
        self.assertIn("genesis reused (key %s, md5 %s)" % (key1, m1), log2)
        self.assertEqual(self.gen_md5(log2), m1)
        self.assertIn("genesis timeout_base_ms=500", log2)
        self.assertIn("genesis evm.block_interval=11 (absent: node default)", log2)
        log3 = self.run_cell(markets="4")
        self.assertEqual(self.calls_n(), 2, "a changed input rebuilds")
        self.assertIn("genesis built (key ", log3)
        log4 = self.run_cell(STATE_HASH_ACTIVATION="9")
        self.assertEqual(self.calls_n(), 3, "an ambient generator env change rebuilds")
        log5 = self.run_cell()
        self.assertEqual(self.calls_n(), 3, "the first key is still cached")
        self.assertIn("genesis reused (key %s" % key1, log5)

    def test_corrupt_cache_entry_rebuilds(self):
        log1 = self.run_cell()
        key1 = re.search(r"genesis built \(key ([0-9a-f]{20})", log1).group(1)
        with open(os.path.join(self.cache, key1 + ".genesis"), "a") as f:
            f.write(" ")
        log2 = self.run_cell()
        self.assertEqual(self.calls_n(), 2)
        self.assertIn("md5", log2[log2.index("genesis cache") :])
        self.assertIn("genesis built (key %s" % key1, log2)
        self.assertEqual(self.calls_n(), 2)
        self.run_cell()
        self.assertEqual(self.calls_n(), 2, "the rebuilt entry is reused")

    def test_force_builds_first_then_reuses(self):
        self.run_cell()  # a non-forced entry exists
        self.assertEqual(self.calls_n(), 1)
        log2 = self.run_cell(FORCE="1")
        self.assertEqual(self.calls_n(), 2, "FORCE=1 never reuses a non-forced entry")
        self.assertIn("genesis built (key ", log2)
        self.assertIn("FORCE=1", log2)
        log3 = self.run_cell(FORCE="1")
        self.assertEqual(
            self.calls_n(), 2, "a FORCE=1 entry is reused by later FORCE=1 cells"
        )
        self.assertIn("genesis reused (key ", log3)

    def test_book_mode_is_a_key_input_and_reused_from_the_entry(self):
        """s131: the genesis book mode (EXTRA_ENV TORUS_BOOK_ROWS / BOOK_MODE)
        keys the entry; a reused mode-3 cell logs and checks the mode from
        the entry, and a Classic cell never reuses a mode-3 genesis."""
        log1 = self.run_cell(extra_env="TORUS_BOOK_ROWS=3")
        self.assertEqual(self.calls_n(), 1)
        self.assertIn("genesis consensus.book_mode=level_authority_chunked (TORUS_BOOK_ROWS=3)", log1)
        self.assertIn("book_mode=mode3", log1)
        log2 = self.run_cell(extra_env="TORUS_BOOK_ROWS=3")
        self.assertEqual(self.calls_n(), 1, "the generator must not run on a reuse:\n" + log2)
        self.assertIn("genesis reused (key ", log2)
        self.assertIn("genesis consensus.book_mode=level_authority_chunked (TORUS_BOOK_ROWS=3)", log2)
        self.assertIn("book_mode=mode3", log2)
        log3 = self.run_cell(extra_env="")
        self.assertEqual(self.calls_n(), 2, "a Classic cell builds its own genesis")
        self.assertIn("genesis consensus.book_mode=classic (absent: classic)", log3)
        self.assertIn("book_mode=classic", log3)
        log4 = self.run_cell(extra_env="BOOK_MODE=level_authority_chunked")
        self.assertEqual(self.calls_n(), 2, "same genesis mode, same entry")
        self.assertIn("genesis reused (key ", log4)
        self.assertIn("genesis consensus.book_mode=level_authority_chunked (BOOK_MODE=level_authority_chunked)", log4)
        # an explicit Classic is the absent field: the no-knob entry, reused
        for kv in ("BOOK_MODE=classic", "TORUS_BOOK_ROWS=0"):
            log5 = self.run_cell(extra_env=kv)
            self.assertEqual(self.calls_n(), 2, kv + " reuses the Classic entry:\n" + log5)
            self.assertIn("genesis reused (key ", log5)
            self.assertIn("genesis consensus.book_mode=classic (%s)" % kv, log5)

    def test_no_cache_dir_builds_every_cell(self):
        for _ in range(2):
            log = self.run_cell(GENESIS_CACHE_DIR="")
            self.assertIn("genesis built (no GENESIS_CACHE_DIR: not cached)", log)
        self.assertEqual(self.calls_n(), 2)
        self.assertFalse(os.path.exists(self.cache))


if __name__ == "__main__":
    unittest.main()
