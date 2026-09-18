#!/usr/bin/env python3
"""Small genesis-wrapper regressions: real jq, stub weighted generator, no cargo.

Run: python3 tools/matched-bench/test_genesis.py
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(shutil.which("jq"), "genesis wrapper requires jq")
class GenesisOutputTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="genesis-wrapper-")
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name) / "fixture repo"
        for directory in ("devnet/wsl", "testnet/lib", "results"):
            (self.repo / directory).mkdir(parents=True)
        for relative in ("devnet/wsl/gen-3val-genesis.sh", "testnet/lib/cargo-bin.sh"):
            shutil.copy2(ROOT / relative, self.repo / relative)
        self.base = {
            "chain_id": 7778,
            "consensus": {"timeout_base_ms": 500},
            "validators": [{"address": "weighted-validator", "stake": "99"}],
            "markets": [{"market_id": i, "base_asset": f"BASE{i}", "quote_asset": "USD",
                         "lot_size": "1.0", "tick_size": "1.0", "initial_margin": "5.0"}
                        for i in (1, 2)],
            "native_balances": [{"address": "funded", "available": "12345"}],
            "accounts": [{"address": "funded", "balance": "67890"}],
            "permanent_stakes": [{"address": "governance", "amount": "321"}],
        }
        self.validators = [{"address": f"devnet-{i}", "stake": str(i)} for i in range(4)]
        (self.repo / "devnet/genesis.json").write_text(json.dumps({"validators": self.validators}))
        self.fixture = self.repo / "weighted-fixture.json"
        self.fixture.write_text(json.dumps(self.base))
        self.full = self.repo / "testnet/genesis-weighted-full.json"
        self.output = self.repo / "results/custom final genesis.json"
        self.log = self.repo / "weighted-calls.jsonl"
        self.bench = self.repo / "bench-stub"
        self.bench.write_text("#!/bin/sh\n# The weighted stub must never invoke a real bench.\nexit 73\n")
        self.bench.chmod(0o755)
        generator = self.repo / "testnet/gen-weighted-genesis.sh"
        generator.write_text("""#!/usr/bin/env python3
import json, os
from pathlib import Path
output = os.environ.get("OUT", "testnet/genesis-weighted-full.json")
with open(os.environ["STUB_LOG"], "a") as log:
    log.write(json.dumps({"out": output, "bin": os.environ.get("BIN")}) + "\\n")
Path(output).write_text(Path(os.environ["STUB_GENESIS"]).read_text())
""")
        generator.chmod(0o755)

    def run_wrapper(self, *, output=True, **overrides):
        env = os.environ.copy()
        for key in ("OUT", "BENCH_BIN", "BIN", "MARKETS", "FORCE", "TIMEOUT_BASE_MS"):
            env.pop(key, None)
        env.update(BENCH_BIN=str(self.bench), STUB_LOG=str(self.log), STUB_GENESIS=str(self.fixture))
        if output:
            env["OUT"] = str(self.output)
        env.update(overrides)
        result = subprocess.run(["bash", str(self.repo / "devnet/wsl/gen-3val-genesis.sh")],
                                env=env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads((self.output if output else self.repo / "devnet/wsl/genesis-3val.json").read_text())

    def assert_nested_output(self):
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [{"out": str(self.full), "bin": str(self.bench)}])
        self.assertEqual(json.loads(self.full.read_text()), self.base)

    def test_fresh_exported_final_output_does_not_redirect_weighted_base(self):
        final = self.run_wrapper(MARKETS="3", TIMEOUT_BASE_MS="800")
        self.assert_nested_output()
        self.assertEqual(final["validators"], self.validators[:3])
        self.assertEqual([m["market_id"] for m in final["markets"]], [1, 2, 3])
        self.assertEqual(final["markets"][:2], self.base["markets"])
        self.assertEqual(final["consensus"]["timeout_base_ms"], 800)
        for field in ("chain_id", "native_balances", "accounts", "permanent_stakes"):
            self.assertEqual(final[field], self.base[field])

    def test_cached_weighted_base_skips_nested_generation(self):
        self.full.write_text(json.dumps(self.base))
        final = self.run_wrapper(MARKETS="1")
        self.assertFalse(self.log.exists())
        self.assertEqual(json.loads(self.full.read_text()), self.base)
        self.assertEqual(final["markets"], self.base["markets"][:1])

    def test_force_rebuild_uses_weighted_path_and_default_final_output(self):
        self.full.write_text(json.dumps({"stale": True}))
        final = self.run_wrapper(output=False, FORCE="1")
        self.assert_nested_output()
        self.assertEqual(final, dict(self.base, validators=self.validators[:3]))


if __name__ == "__main__":
    unittest.main()
