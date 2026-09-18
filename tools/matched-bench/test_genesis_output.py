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
        for name in ('devnet/wsl/gen-3val-genesis.sh', 'testnet/gen-weighted-genesis.sh',
                     'testnet/lib/cargo-bin.sh'):
            destination = self.root / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / name, destination)
        self.validators = [{'address': f'devnet-{i}', 'pubkey': str(i)} for i in range(3)]
        (self.root / 'devnet/genesis.json').write_text(json.dumps({'validators': self.validators}))
        self.base = {
            'chain_id': 1, 'consensus': {'timeout_base_ms': 500},
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

    def generate(self, force=False):
        environment = dict(os.environ)
        for name in ('BASE', 'BIN', 'OUT', 'FORCE', 'MARKETS', 'TIMEOUT_BASE_MS',
                     'BULK_OFFSET', 'BULK_COUNT', 'NATIVE_AVAIL', 'EVM_WEI'):
            environment.pop(name, None)
        environment.update(OUT=str(self.output), BENCH_BIN=str(self.binary), MARKETS='3',
                           FORCE='1' if force else '0', BULK_OFFSET='60', BULK_COUNT='2',
                           CALL_LOG=str(self.calls))
        result = subprocess.run(['bash', str(self.root / 'devnet/wsl/gen-3val-genesis.sh')],
                                env=environment, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(self.full.read_text()), json.loads(self.output.read_text())

    def assert_artifacts(self, full, final):
        self.assertEqual(full['validators'], self.base['validators'])
        self.assertEqual(final['validators'], self.validators)
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


if __name__ == '__main__':
    unittest.main()
