#!/usr/bin/env python3
import os
from pathlib import Path
import tempfile
import shutil
import subprocess
import unittest

from wal_budget import FLAG, MAX_MIB, MIB, parse_mib, provenance, require_fresh_data


class WalBudgetTests(unittest.TestCase):
    def launcher_fixture(self, root):
        wsl = root / "wsl fixture"
        wsl.mkdir()
        source = Path(__file__).resolve().parents[2] / "devnet/wsl/launch-3val.sh"
        shutil.copyfile(source, wsl / "launch-3val.sh")
        (wsl / "node").write_text("#!/bin/sh\nexit 99\n")
        (wsl / "node").chmod(0o755)
        (wsl / "genesis").write_text("{}")
        (wsl / "env.sh").write_text('BIN="$PWD/node"\nGENESIS="$PWD/genesis"\nRUN_DIR="$DATA_ROOT/run"\nRPC_URLS=fixture\nMETRICS_PORTS=fixture\n')
        (wsl / "start-node.sh").write_text('start_node() { echo "$1" >> "$DATA_ROOT/started"; STARTED_PID=2147483647; }\n')
        env = dict(os.environ, DATA_ROOT=str(root / "data root"), FRESH_ONLY="1", CLEAN="1")
        return wsl / "launch-3val.sh", env

    def test_defaults_and_checked_units(self):
        for raw in [None, "0", "000"]:
            self.assertEqual(parse_mib(raw), 0)
        self.assertEqual(parse_mib(" 1024 ") * MIB, 1 << 30)
        self.assertEqual(parse_mib(str(MAX_MIB)), MAX_MIB)
        for raw in ["", "-1", "+1", "1.5", "1_000", "１２", str(MAX_MIB + 1)]:
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                parse_mib(raw)

    def test_fresh_guard_never_changes_existing_data(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "data"
            self.assertEqual(require_fresh_data("1024", path), 1024)
            self.assertFalse(path.exists())
            path.mkdir()
            (path / "sentinel").write_bytes(b"keep")
            with self.assertRaises(ValueError):
                require_fresh_data("1024", path)
            self.assertEqual((path / "sentinel").read_bytes(), b"keep")
            self.assertEqual(require_fresh_data("0", path), 0)
            link = Path(root) / "dangling"
            os.symlink(Path(root) / "missing", link)
            with self.assertRaises(ValueError):
                require_fresh_data("1", link)

    def test_effective_provenance_distinguishes_missing_zero_and_candidate(self):
        self.assertFalse(provenance({})["recorded"])
        default = provenance({FLAG: "0"})
        self.assertTrue(default["recorded"])
        self.assertEqual(default["mode"], "rocksdb_automatic")
        candidate = provenance({FLAG: "1024"})
        self.assertTrue(candidate["valid"])
        self.assertEqual(candidate["effective_max_total_wal_size_bytes"], 1 << 30)
        self.assertFalse(provenance({FLAG: "bad"})["valid"])

    def test_launcher_refuses_data_that_appears_after_preflight(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            launch, env = self.launcher_fixture(root)
            data = Path(env["DATA_ROOT"]) / "data"
            require_fresh_data("1", data)
            # Simulate the exact preflight-to-launch race, without processes
            # or sleeps. CLEAN=1 must never delete this newly appeared path.
            data.mkdir(parents=True)
            (data / "sentinel").write_bytes(b"keep")
            result = subprocess.run(["bash", str(launch)], env=env, text=True, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("FRESH_ONLY requires", result.stderr)
            self.assertEqual((data / "sentinel").read_bytes(), b"keep")
            self.assertFalse((data.parent / "started").exists())

    def test_launcher_claims_fresh_path_without_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            launch, env = self.launcher_fixture(Path(temp))
            result = subprocess.run(["bash", str(launch)], env=env, text=True, capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("wiping", result.stdout)
            data_root = Path(env["DATA_ROOT"])
            self.assertTrue((data_root / "data").is_dir())
            self.assertEqual((data_root / "started").read_text().splitlines(), ["0", "1", "2"])


if __name__ == "__main__":
    unittest.main()
