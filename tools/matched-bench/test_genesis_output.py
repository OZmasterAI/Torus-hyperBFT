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
        # gen-accounts --offset O --count N: N rows "idx 0x<idx as 40 hex>".
        self.binary.write_text('''#!/usr/bin/env bash
set -eu
[ "$1" = gen-accounts ]
[ "$2" = --offset ] && [ "$3" = 60 ]
[ "$4" = --count ]
printf '%s %s\\n' "$OUT" "$5" >> "$CALL_LOG"
for i in $(seq "$3" $(( $3 + $5 - 1 ))); do printf '%s 0x%040x\\n' "$i" "$i"; done
''')
        self.binary.chmod(0o755)

    def run_script(self, force=False, **extra):
        environment = dict(os.environ)
        for name in ('BASE', 'BIN', 'OUT', 'FORCE', 'MARKETS', 'TIMEOUT_BASE_MS',
                     'BULK_OFFSET', 'BULK_COUNT', 'NATIVE_AVAIL', 'EVM_WEI',
                     'EVM_BLOCK_INTERVAL', 'STATE_HASH_ACTIVATION', 'BOOK_MODE',
                     'BOOK_WINDOW_COMMIT'):
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
        self.assertEqual(self.calls.read_text().splitlines(), [str(self.full) + ' 2'])

    def test_force_regenerates_weighted_base_separately_from_custom_output(self):
        self.full.write_text(json.dumps({**self.base, 'stale': True}))
        self.output.write_text('old final artifact')
        full, final = self.generate(force=True)
        self.assert_artifacts(full, final)
        self.assertNotIn('stale', full)
        self.assertEqual(len(full['accounts']), 3)
        self.assertEqual(self.calls.read_text().splitlines(), [str(self.full) + ' 2'])

    def bulk_rows(self, n):
        return [{'address': '0x%040x' % i, 'available': '1.0', 'note': 'bulk-test %d' % i}
                for i in range(60, 60 + n)]

    def write_legacy_full(self, bulk, **extra):
        """A weighted-full genesis built before the params sidecar: only its
        rows say which BULK_COUNT built it."""
        self.full.write_text(json.dumps({**self.base, **extra,
                                         'native_balances': self.base['native_balances'] + self.bulk_rows(bulk)}))

    def test_existing_weighted_base_is_reused_without_invoking_child(self):
        self.write_legacy_full(2, reused=True)
        full, final = self.generate()
        self.assert_artifacts(full, final)
        self.assertTrue(final['reused'])
        self.assertFalse(self.calls.exists())

    # ---- BULK_COUNT (bench queue 8f): the funded bulk-sender count of the
    # weighted-full genesis. Default 100000; a reused genesis built with a
    # different count is refused, never silently reused.

    def params(self):
        return json.loads(self.full.with_name('genesis-weighted-full.params.json').read_text())

    def test_bulk_count_reaches_the_weighted_generator_and_is_recorded(self):
        full, final = self.generate(force=True, BULK_COUNT='5')
        self.assertEqual(self.calls.read_text().splitlines(), [str(self.full) + ' 5'])
        notes = [b.get('note') for b in final['native_balances']]
        self.assertEqual(notes[1:], ['bulk-test %d' % i for i in range(60, 65)])
        self.assertEqual(len(final['accounts']), 6)
        p = self.params()
        self.assertEqual((p['bulk_offset'], p['bulk_count']), (60, 5))
        self.assertEqual(p['bytes'], self.full.stat().st_size)

    def test_summary_names_the_bulk_count(self):
        result = self.run_script(force=True, BULK_COUNT='3')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('building weighted-full genesis (BULK_COUNT=3 bulk senders)', result.stdout)
        self.assertRegex(result.stdout, r'bulk_count = 3\b')
        self.assertNotIn('100k', result.stdout)

    def test_reuse_with_the_same_bulk_count_does_not_rebuild(self):
        self.generate(force=True, BULK_COUNT='3')
        self.calls.unlink()
        self.generate(BULK_COUNT='3')
        self.assertFalse(self.calls.exists())

    def test_reuse_with_a_different_bulk_count_is_refused(self):
        self.generate(force=True, BULK_COUNT='3')
        self.calls.unlink()
        self.output.unlink()
        result = self.run_script(BULK_COUNT='4')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('BULK_COUNT=3', result.stderr)
        self.assertIn('BULK_COUNT=4', result.stderr)
        self.assertIn('FORCE=1', result.stderr)
        self.assertFalse(self.output.exists(), 'refused before the 3-val genesis is written')
        self.assertFalse(self.calls.exists(), 'never rebuilt implicitly')

    def test_force_rebuilds_at_the_new_bulk_count(self):
        self.generate(force=True, BULK_COUNT='3')
        full, _ = self.generate(force=True, BULK_COUNT='4')
        self.assertEqual(len(full['native_balances']), 5)
        self.assertEqual(self.params()['bulk_count'], 4)

    def test_legacy_full_without_params_is_checked_by_its_rows(self):
        self.write_legacy_full(2)
        self.generate(BULK_COUNT='2')
        self.output.unlink()
        result = self.run_script(BULK_COUNT='7')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('BULK_COUNT=2', result.stderr)
        self.assertFalse(self.output.exists())

    def test_default_bulk_count_is_100000(self):
        # Unset: the generator's old default; a 2-row legacy genesis is refused against it.
        self.write_legacy_full(2)
        result = self.run_script(BULK_COUNT=None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('BULK_COUNT=100000', result.stderr)
        weighted = (self.root / 'testnet/gen-weighted-genesis.sh').read_text()
        self.assertIn('BULK_COUNT="${BULK_COUNT:-100000}"', weighted)

    def test_params_whose_size_does_not_match_fall_back_to_the_rows(self):
        self.generate(force=True, BULK_COUNT='3')
        # Replace the genesis behind the sidecar's back (size changes).
        self.write_legacy_full(2)
        self.output.unlink()
        result = self.run_script(BULK_COUNT='3')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('BULK_COUNT=2', result.stderr)

    def test_bad_bulk_count_is_refused_before_anything_is_written(self):
        for value in ('0', 'abc', '-1', '1e6', '10 '):
            with self.subTest(value=value):
                result = self.run_script(force=True, BULK_COUNT=value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('BULK_COUNT must be a positive integer', result.stderr)
                self.assertFalse(self.output.exists())
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

    def test_running_state_hash_defaults_on_and_off_is_explicit(self):
        # Owner s136: the running state hash is on in every newly generated
        # genesis, WRITTEN EXPLICITLY (Codex P1: the node reads a genesis
        # naming no activation as off, its legacy meaning). Unset
        # STATE_HASH_ACTIVATION writes consensus.activations (hash + L6.1 from
        # block 1, Classic); 'off' writes no activation at all (the pre-s136
        # bytes); H writes the alias as before.
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        final = json.loads(self.output.read_text())
        self.assertEqual(final['consensus']['activations'],
                         {'running_state_hash': 1, 'book_window_commit': 1})
        self.assertNotIn('state_hash_activation_height', final['consensus'])
        self.assertNotIn('book_window_commit', final['consensus'])
        self.assertIn('  state_hash_activation = 1\n', result.stdout)
        result = self.run_script(STATE_HASH_ACTIVATION='off')
        self.assertEqual(result.returncode, 0, result.stderr)
        final = json.loads(self.output.read_text())
        for field in ('activations', 'state_hash_activation_height', 'book_window_commit'):
            self.assertNotIn(field, final['consensus'])
        self.assertIn('  state_hash_activation = off\n', result.stdout)
        result = self.run_script(STATE_HASH_ACTIVATION='7')
        self.assertEqual(result.returncode, 0, result.stderr)
        final = json.loads(self.output.read_text())
        self.assertEqual(final['consensus']['state_hash_activation_height'], 7)
        self.assertNotIn('activations', final['consensus'])
        self.assertIn('  state_hash_activation = 7\n', result.stdout)

    def test_inherited_hash_spellings_never_survive_the_scripts_choice(self):
        # Codex P2 (s136): a weighted base carrying the legacy alias
        # state_hash_activation_height (or the L6.1 bool, or activations of
        # its own) must not leak into the final genesis: with 'off' the hash
        # was reported off while the parser merged the inherited alias back
        # in. This script alone decides the hash and L6.1 spellings.
        inherited = [
            {'state_hash_activation_height': 7},
            {'state_hash_activation_height': 7, 'book_window_commit': True},
            {'activations': {'running_state_hash': 7, 'book_window_commit': 7}},
        ]
        cases = [
            ('off', None, 'off', False),
            ('unset', {'running_state_hash': 1, 'book_window_commit': 1}, '1', True),
            ('5', None, '5', False),
        ]
        for extra in inherited:
            consensus = {**self.base['consensus'], **extra}
            (self.root / 'testnet/genesis-weighted-base.json').write_text(
                json.dumps({**self.base, 'consensus': consensus}))
            for value, activations, shown, window in cases:
                with self.subTest(inherited=extra, STATE_HASH_ACTIVATION=value):
                    env = {} if value == 'unset' else {'STATE_HASH_ACTIVATION': value}
                    result = self.run_script(force=True, **env)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    final = json.loads(self.output.read_text())['consensus']
                    self.assertEqual(final.get('activations'), activations, final)
                    self.assertEqual(final.get('state_hash_activation_height'),
                                     int(value) if value.isdigit() else None, final)
                    self.assertNotIn('book_window_commit', final)
                    self.assertIn('  state_hash_activation = %s\n' % shown, result.stdout)
                    self.assertEqual('book_window_commit = on' in result.stdout, window, result.stdout)

    def test_default_classic_genesis_reports_window_books_on(self):
        # The default for a Classic genesis is the hash + L6.1 from block 1
        # (CL21), written as activations; the summary says both. A row book
        # mode, BOOK_WINDOW_COMMIT=0, an explicit hash height or 'off' get no
        # L6.1.
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('  book_window_commit = on (L6.1)\n', result.stdout)
        for extra, activations in (({'BOOK_MODE': '3'}, {'running_state_hash': 1}),
                                   ({'BOOK_WINDOW_COMMIT': '0'}, {'running_state_hash': 1}),
                                   ({'STATE_HASH_ACTIVATION': '1'}, None),
                                   ({'STATE_HASH_ACTIVATION': 'off'}, None)):
            with self.subTest(**extra):
                result = self.run_script(**extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn('book_window_commit = on', result.stdout)
                final = json.loads(self.output.read_text())
                self.assertEqual(final['consensus'].get('activations'), activations)

    def test_book_window_commit_needs_the_state_hash_and_sets_the_consensus_flag(self):
        # L6.1 (ported from perf/l61-window-books 9930fc60): chain-wide genesis field
        final = self.generate(STATE_HASH_ACTIVATION='1', BOOK_WINDOW_COMMIT='1')[1]
        self.assertEqual(final['consensus']['state_hash_activation_height'], 1)
        self.assertIs(final['consensus']['book_window_commit'], True)
        for value in (None, '', '0'):
            with self.subTest(value=value):
                final = self.generate(STATE_HASH_ACTIVATION='1', BOOK_WINDOW_COMMIT=value)[1]
                self.assertNotIn('book_window_commit', final.get('consensus', {}))
        # Owner s136: unset = the node default (hash on), so only 'off' refuses.
        final = self.generate(BOOK_WINDOW_COMMIT='1')[1]
        self.assertIs(final['consensus']['book_window_commit'], True)
        result = self.run_script(STATE_HASH_ACTIVATION='off', BOOK_WINDOW_COMMIT='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('BOOK_WINDOW_COMMIT=1 needs the running state hash', result.stderr)

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

    # ---- BOOK_MODE (owner s131): the order-book storage mode is the genesis
    # field `.consensus.book_mode` (absent = classic); the node takes its mode
    # from genesis. Names and 0-3 both accepted, written as the name.

    BOOK_MODES = (('classic', 'classic'), ('0', 'classic'), ('order_rows', 'order_rows'),
                  ('1', 'order_rows'), ('level_authority', 'level_authority'), ('2', 'level_authority'),
                  ('level_authority_chunked', 'level_authority_chunked'), ('3', 'level_authority_chunked'))

    def test_book_mode_sets_the_final_genesis_only(self):
        for value, name in self.BOOK_MODES:
            if name == 'classic':
                continue  # absent: test_explicit_classic_is_the_absent_field
            with self.subTest(value=value):
                result = self.run_script(BOOK_MODE=value)
                self.assertEqual(result.returncode, 0, result.stderr)
                full = json.loads(self.full.read_text())
                final = json.loads(self.output.read_text())
                # owner s136: the hash is written explicitly; a row mode gets no L6.1
                self.assertEqual(final['consensus'], {**self.base['consensus'], 'book_mode': name,
                                                      'activations': {'running_state_hash': 1}})
                self.assertNotIn('book_mode', full['consensus'], 'the reused weighted base is never patched')
                self.assertIn('book_mode  = %s' % name, result.stdout)

    def test_unset_book_mode_leaves_the_genesis_byte_identical(self):
        unset = self.generate(BOOK_MODE=None)[1]
        unset_bytes = self.output.read_bytes()
        self.assertNotIn('book_mode', unset['consensus'])
        result = self.run_script(BOOK_MODE='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), unset_bytes)
        self.assertIn('book_mode  = absent (classic)', result.stdout)

    def test_explicit_classic_is_the_absent_field(self):
        """Review rec (b): Classic is never written; classic / 0 leave the
        field absent, so OUT is byte-identical to a run without BOOK_MODE
        (one Classic chain, one genesis md5)."""
        self.generate(BOOK_MODE=None)
        unset_bytes = self.output.read_bytes()
        for value in ('classic', '0'):
            with self.subTest(value=value):
                result = self.run_script(BOOK_MODE=value)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.output.read_bytes(), unset_bytes)
                self.assertIn('book_mode  = absent (classic)', result.stdout)

    def test_no_stale_book_mode_between_runs(self):
        self.assertEqual(self.generate(BOOK_MODE='3')[1]['consensus']['book_mode'], 'level_authority_chunked')
        self.assertNotIn('book_mode', self.generate()[1]['consensus'])

    def test_bad_book_mode_is_refused_before_anything_is_written(self):
        for value in ('4', 'mode3', 'Classic', ' 3', '-1', 'x'):
            with self.subTest(value=value):
                if self.output.exists():
                    self.output.unlink()
                result = self.run_script(BOOK_MODE=value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('BOOK_MODE must be classic, order_rows, level_authority, level_authority_chunked or 0-3', result.stderr)
                self.assertFalse(self.output.exists(), 'refused before any genesis is written')

    def test_book_mode_names_match_the_node(self):
        source = Path(__file__).resolve().parents[2]
        types = (source / 'crates/torus-types/src/book_mode.rs').read_text()
        for _, name in self.BOOK_MODES:
            self.assertIn('=> "%s"' % name, types)


if __name__ == '__main__':
    unittest.main()
