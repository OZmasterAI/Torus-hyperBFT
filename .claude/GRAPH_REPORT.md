# Code Graph: Torus-hyperBFT
_Auto-generated 2026-07-12 10:01 UTC — do not edit._

**8105 nodes across 1565 files**

## Key Hubs
| Node | File | Connections |
|------|------|------------|
| extend_from_slice | extend_from_slice | 100 |
| build | build | 83 |
| run | crates/torus-node/src/main.rs | 83 |
| place_order | crates/torus-core/src/order_book.rs | 81 |
| alloy_primitives | alloy_primitives | 80 |
| to_bytes | to_bytes | 78 |
| put_cf_raw | put_cf_raw | 76 |
| verifying_key | verifying_key | 76 |
| addr | crates/torus-core/src/order_book.rs | 72 |
| fp | crates/torus-core/src/order_book.rs | 72 |
| torus_types | torus_types | 72 |
| book | crates/torus-core/src/order_book.rs | 70 |
| put_account | put_account | 67 |
| copy_from_slice | copy_from_slice | 66 |
| get_cf_raw | get_cf_raw | 64 |

## Communities
- **crates** (425) — files: `Cli`, `EconomicsError`, `EthApiServer`, `Logger` | fns: `NativeStateOverlay`, `crates/hotstuff_rs/src/lib.rs`, `crates/hotstuff_rs/src/networking/mod.rs`, `crates/torus-core/benches/order_book.rs`
- **crates-1** (349) — files: `crates/torus-consensus/src/kv_store.rs`, `crates/torus-state/src/db.rs`, `crates/torus-state/src/trie_cursor.rs`, `addr_of` | fns: `Delete`, `Set`, `account_storage`, `account_trie_cursor`
- **crates-2** (312) — files: `torus_consensus`, `hotstuff_rs`, `crates/torus-network/src/sync.rs`, `add_speculative_commit` | fns: `ChannelNetwork`, `GenesisConfig`, `Replica`, `RocksKVStore`
- **crates-3** (281) — files: `Ed25519Sig`, `crates/hotstuff_rs/tests/common/verifying_key_bytes.rs`, `add_address`, `crates/torus-bridge/tests/session_key_tests.rs` | fns: `VerifyingKeyBytes`, `addr`, `address_from_signing_key`, `all_vectors`
- **crates-4** (233) — files: `crates/torus-integration-tests/tests/fee_flow.rs`, `crates/torus-economics/tests/permanent_stake_tests.rs`, `crates/torus-integration-tests/tests/snapshot_dos_keys.rs`, `crates/torus-economics/tests/governance_tests.rs` | fns: `addr`, `addr`, `addr`, `addr`
- **crates-5** (208) — files: `crates/torus-core/tests/precompile_tests.rs`, `crates/torus-core/src/liquidation.rs`, `crates/torus-integration-tests/tests/stress.rs`, `crates/torus-core/tests/margin_tests.rs` | fns: `abi_address_roundtrip`, `addr`, `addr`, `addr`
- **crates-6** (199) — files: `abs_diff`, `crates/torus-economics/src/staking.rs`, `crates/torus-economics/src/governance.rs`, `crates/torus-economics/src/dev_pool.rs` | fns: `addr`, `addr`, `all_entries`, `all_pending_rotations`
- **crates-7** (190) — files: `crates/torus-consensus/src/app.rs`, `add_native_action_from_gossip_trusted`, `crates/torus-rpc/src/lib.rs`, `as_millis` | fns: `absorb_fetched_bodies`, `admit_rejects_counted_by_reason`, `bare_eth_call_succeeds_at_height_with_base_fee`, `bincode_roundtrip_preserves_action_hash`
- **crates-8** (172) — files: `crates/torus-explorer/src/api.rs`, `IntoResponse`, `Json`, `add_native_action_presigned` | fns: `ApiError`, `addr`, `base_fee`, `block_env_from_header`
- **crates-9** (167) — files: `add_evm_tx`, `add_native_action_from_gossip`, `app`, `crates/torus-node/src/main.rs` | fns: `apply_config_defaults`, `apply_rotation_cap`, `arrival_generation`, `arrivals`
- **..** (155) — files: `devnet/scripts/native-dup-factor.py`, `devnet/scripts/native-order-flood.py`, `devnet/scripts/native-transfer-probe.py`, `../../../linuxbrew/.linuxbrew/lib/node_modules/pyright/dist/typeshed-fallback/stdlib/collections/__init__.pyi` | fns: `(module) native-dup-factor.py`, `(module) native-order-flood.py`, `(module) native-transfer-probe.py`, `Counter`
- **crates-11** (151) — files: `PublicKey`, `crates/torus-mempool/src/lib.rs`, `crates/torus-bridge/tests/exec_isolation_tests.rs`, `all_senders` | fns: `accepts_pre_eip155_legacy_tx`, `add_evm_tx`, `add_native_action`, `add_native_action_from_gossip`
- **test** (145) — files: `db.rs`, `config.rs`, `serde`, `indexer.rs` | fns: `BlockRow`, `CandleRow`, `Config`, `Deserialize`
- **crates-12** (145) — files: `alloy_primitives`, `committer`, `committer.rs`, `error` | fns: `Address`, `B256`, `BlockCommitter`, `BlockCommitter`
- **..-14** (138) — files: `crates/torus-state/src/cf.rs`, `rocksdb`, `crates/torus-consensus/src/network.rs`, `Clone` | fns: `CF_CONSENSUS_META`, `Cache`, `ChannelNetwork`, `ColumnFamilyDescriptor`
- **crates-15** (131) — files: `torus_rpc:`, `common/mod.rs`, `torus_core::precompiles:`, `torus_economics::epoch:` | fns: `BlockNotifier`, `BlockNotifier`, `CoreWriterQueue`, `EpochManager`
- **crates-17** (123) — files: `crates/hotstuff_rs/src/block_tree/accessors/internal.rs`, `crates/hotstuff_rs/src/block_sync/messages.rs`, `crates/hotstuff_rs/src/types/validator_set.rs`, `crates/hotstuff_rs/src/block_tree/pluggables.rs` | fns: `add_speculative_commit`, `advance_highest_pc_from_remote`, `advertise_block`, `app_view`
- **crates-16** (123) — files: `abort`, `accept`, `crates/torus-network/src/transport.rs`, `tools/faucet/src/main.rs` | fns: `addr`, `address_from_signing_key`, `borsh_write_address`, `borsh_write_address`
- **crates-18** (119) — files: `torus_types`, `hotstuff_rs:app:`, `hotstuff_rs:types::update_sets:`, `data_types` | fns: `Address`, `App`, `AppStateUpdates`, `BlockHeight`
- **crates-19** (115) — files: `crates/torus-bridge/src/error.rs`, `crates/torus-bridge/src/error.rs:BridgeError:`, `crates/torus-core/src/error.rs`, `thiserror` | fns: `BridgeError`, `Core`, `CoreError`, `Error`

## Directory Map
- `./` — 1242 files
- `crates/` — 239 files
- `devnet/` — 34 files
- `tools/` — 29 files
- `testnet/` — 21 files

---
For deeper queries use `run_tool('indexer', '<tool>', ...)`:
- `code_search` — fuzzy find by name or keyword
- `code_callers` / `code_readers` — trace call/read chains
- `code_blast_radius` — impact analysis for a function
- `code_clusters` / `code_cluster_detail` — community details
