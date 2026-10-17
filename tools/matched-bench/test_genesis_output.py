#!/usr/bin/env python3
"""Run real genesis scripts against tiny fixtures; no cargo or bulk generation."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class GenesisOutputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='genesis fixture ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        source = Path(__file__).resolve().parents[2]
        for name in ('devnet/wsl/gen-3val-genesis.sh', 'devnet/wsl/bench-validator-keys.json',
                     'testnet/gen-weighted-genesis.sh', 'testnet/lib/cargo-bin.sh'):
            destination = self.root / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / name, destination)
        self.validators = [{'address': f'devnet-{i}', 'pubkey': str(i)} for i in range(4)]
        (self.root / 'devnet/genesis.json').write_text(json.dumps({'validators': self.validators}))
        self.keys_file = self.root / 'devnet/wsl/bench-validator-keys.json'
        keys = json.loads(self.keys_file.read_text())['validators']
        # The bench validators: the first 3 devnet entries, each under the
        # address of the key file entry with the same index.
        self.expected_validators = [
            {**self.validators[i], 'address': next(k['address'] for k in keys if k['index'] == i)}
            for i in range(3)]
        self.base = {
            'chain_id': 1, 'consensus': {'timeout_base_ms': 500},
            'evm': {'spec_id': 'cancun', 'gas_limit': 30000000, 'base_fee_initial': 1000000000,
                    'chain_id': 7778},
            'validators': [{'address': 'weighted-' + str(i)} for i in range(4)],
            'native_balances': [{'address': 'base', 'available': '9.0'}],
            'accounts': [{'address': 'base', 'balance': '9'}], 'permanent_stakes': [],
            'markets': [{'market_id': i, 'base_asset': f'S{i}', 'quote_asset': 'USD',
                         'lot_size': '1.0', 'tick_size': '1.0', 'initial_margin': '5.0'} for i in (1, 2)],
        }
        (self.root / 'testnet/genesis-weighted-base.json').write_text(json.dumps(self.base))
        self.full = self.root / 'testnet/genesis-weighted-full.json'
        self.output = self.root / 'custom final.json'
        self.calls = self.root / 'child-outputs.txt'
        self.binary = self.root / 'mock-bench'
        self.binary.write_text('''#!/usr/bin/env bash
set -eu
[ "$1" = gen-accounts ]
[ "$2" = --offset ] && [ "$3" = 60 ]
[ "$4" = --count ] && [ "$5" = 2 ]
printf '%s\\n' "$OUT" >> "$CALL_LOG"
printf '60 0x0000000000000000000000000000000000000060\\n61 0x0000000000000000000000000000000000000061\\n'
''')
        self.binary.chmod(0o755)

    def run_script(self, force=False, **extra):
        environment = dict(os.environ)
        for name in ('BASE', 'BIN', 'OUT', 'FORCE', 'MARKETS', 'TIMEOUT_BASE_MS',
                     'BULK_OFFSET', 'BULK_COUNT', 'NATIVE_AVAIL', 'EVM_WEI',
                     'EVM_BLOCK_INTERVAL', 'STATE_HASH_ACTIVATION'):
            environment.pop(name, None)
        environment.update(OUT=str(self.output), BENCH_BIN=str(self.binary), MARKETS='3',
                           FORCE='1' if force else '0', BULK_OFFSET='60', BULK_COUNT='2',
                           CALL_LOG=str(self.calls))
        for name, value in extra.items():
            if value is None:
                environment.pop(name, None)
            else:
                environment[name] = value
        return subprocess.run(['bash', str(self.root / 'devnet/wsl/gen-3val-genesis.sh')],
                              env=environment, capture_output=True, text=True, timeout=10)

    def generate(self, force=False, **extra):
        result = self.run_script(force, **extra)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(self.full.read_text()), json.loads(self.output.read_text())

    def assert_artifacts(self, full, final):
        self.assertEqual(full['validators'], self.base['validators'])
        self.assertEqual(final['validators'], self.expected_validators)
        self.assertEqual(len(full['markets']), 2)
        self.assertEqual([market['market_id'] for market in final['markets']], [1, 2, 3])
        self.assertEqual(full['native_balances'], final['native_balances'])
        self.assertEqual(full['accounts'], final['accounts'])

    def test_fresh_weighted_base_does_not_inherit_final_output(self):
        full, final = self.generate()
        self.assert_artifacts(full, final)
        self.assertEqual(len(full['native_balances']), 3)
        self.assertEqual(self.calls.read_text().splitlines(), [str(self.full)])

    def test_force_regenerates_weighted_base_separately_from_custom_output(self):
        self.full.write_text(json.dumps({**self.base, 'stale': True}))
        self.output.write_text('old final artifact')
        full, final = self.generate(force=True)
        self.assert_artifacts(full, final)
        self.assertNotIn('stale', full)
        self.assertEqual(len(full['accounts']), 3)
        self.assertEqual(self.calls.read_text().splitlines(), [str(self.full)])

    def test_existing_weighted_base_is_reused_without_invoking_child(self):
        self.full.write_text(json.dumps({**self.base, 'reused': True}))
        full, final = self.generate()
        self.assert_artifacts(full, final)
        self.assertTrue(final['reused'])
        self.assertFalse(self.calls.exists())

    def test_key_file_holds_three_distinct_test_keys_in_validator_order(self):
        keys = json.loads(self.keys_file.read_text())
        self.assertIn('TEST-ONLY', keys['note'])
        self.assertEqual([k['index'] for k in keys['validators']], [0, 1, 2])
        for k in keys['validators']:
            self.assertRegex(k['address'], r'^0x[0-9a-f]{40}$')
            self.assertRegex(k['private_key'], r'^0x[0-9a-f]{64}$')
        self.assertEqual(len({k['address'] for k in keys['validators']}), 3)

    def test_key_file_missing_a_validator_index_fails(self):
        keys = json.loads(self.keys_file.read_text())
        keys['validators'] = [k for k in keys['validators'] if k['index'] != 2]
        self.keys_file.write_text(json.dumps(keys))
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('index 2', result.stderr)


    # ---- EVM_BLOCK_INTERVAL (bench Q12, item 7 step 3 option S): the genesis
    # EVM cadence N, `.evm.block_interval` (EvmConfig.block_interval; absent =
    # torus_types::DEFAULT_EVM_BLOCK_INTERVAL = 11).

    def test_evm_block_interval_sets_the_final_genesis_only(self):
        # MARKETS=3 > the base's 2: the synthesis branch, which is the one the
        # 300-market (hsdb-300m) cells take over the 100-market weighted base.
        for markets in ('3', '1', None):
            with self.subTest(markets=markets):
                full, final = self.generate(EVM_BLOCK_INTERVAL='22', MARKETS=markets)
                self.assertEqual(final['evm'], {**self.base['evm'], 'block_interval': 22})
                self.assertNotIn('block_interval', full['evm'], 'the reused weighted base is never patched')
                self.assertEqual(final['validators'], self.expected_validators)

    def test_unset_evm_block_interval_leaves_the_genesis_byte_identical(self):
        unset = self.generate(EVM_BLOCK_INTERVAL=None)[1]
        unset_bytes = self.output.read_bytes()
        self.assertNotIn('block_interval', unset['evm'])
        empty = self.generate(EVM_BLOCK_INTERVAL='')[1]
        self.assertEqual(self.output.read_bytes(), unset_bytes)
        self.assertEqual(empty, unset)

    def test_no_stale_evm_block_interval_between_runs(self):
        self.assertEqual(self.generate(EVM_BLOCK_INTERVAL='33')[1]['evm']['block_interval'], 33)
        self.assertNotIn('block_interval', self.generate()[1]['evm'])
        self.assertEqual(self.generate(EVM_BLOCK_INTERVAL='11')[1]['evm']['block_interval'], 11)
        self.assertEqual(self.generate(EVM_BLOCK_INTERVAL='2')[1]['evm']['block_interval'], 2)
        self.assertEqual(self.generate(EVM_BLOCK_INTERVAL='100')[1]['evm']['block_interval'], 100)

    def test_evm_block_interval_outside_the_governance_range_is_refused(self):
        for value in ('0', '1', '101', '-5', 'abc', '1e2', '22 ', '022x', '99999999999999999999'):
            with self.subTest(value=value):
                if self.output.exists():
                    self.output.unlink()
                result = self.run_script(EVM_BLOCK_INTERVAL=value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('EVM_BLOCK_INTERVAL must be an integer in 2..=100', result.stderr)
                self.assertIn(repr(value).strip("'"), result.stderr)
                self.assertFalse(self.output.exists(), 'refused before any genesis is written')

    def test_range_matches_the_node_governance_range(self):
        # The script's 2..=100 is torus_types::EVM_BLOCK_INTERVAL_RANGE; this
        # fails if the node's range moves without the script.
        source = Path(__file__).resolve().parents[2]
        types = (source / 'crates/torus-types/src/lib.rs').read_text()
        self.assertIn('pub const EVM_BLOCK_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 2..=100;', types)
        self.assertIn('pub const DEFAULT_EVM_BLOCK_INTERVAL: u64 = 11;', types)


if __name__ == '__main__':
    unittest.main()
