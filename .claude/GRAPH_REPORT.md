# Code Graph: Torus-hyperBFT
_Auto-generated 2026-07-13 17:33 UTC — do not edit._

**8355 nodes across 1613 files**

## Key Hubs
| Node | File | Connections |
|------|------|------------|
| extend_from_slice | extend_from_slice | 101 |
| run | crates/torus-node/src/main.rs | 86 |
| build | build | 84 |
| put_cf_raw | put_cf_raw | 84 |
| place_order | crates/torus-core/src/order_book.rs | 81 |
| alloy_primitives | alloy_primitives | 80 |
| to_bytes | to_bytes | 78 |
| verifying_key | verifying_key | 78 |
| addr | crates/torus-core/src/order_book.rs | 72 |
| fp | crates/torus-core/src/order_book.rs | 72 |
| torus_types | torus_types | 72 |
| book | crates/torus-core/src/order_book.rs | 70 |
| get_cf_raw | get_cf_raw | 70 |
| copy_from_slice | copy_from_slice | 69 |
| put_account | put_account | 67 |

## Communities
- **crates** (437) — files: `Cli`, `EconomicsError`, `EthApiServer`, `Logger` | fns: `NativeStateOverlay`, `crates/hotstuff_rs/src/lib.rs`, `crates/hotstuff_rs/src/networking/mod.rs`, `crates/torus-core/benches/order_book.rs`
- **crates-1** (348) — files: `Ed25519Sig`, `crates/torus-network/src/tx_gossip.rs`, `crates/hotstuff_rs/tests/common/verifying_key_bytes.rs`, `add_address` | fns: `TxGossipHandle`, `VerifyingKeyBytes`, `addr`, `address_from_signing_key`
- **crates-2** (334) — files: `crates/torus-consensus/src/kv_store.rs`, `crates/torus-types/src/lib.rs`, `Serialize`, `crates/torus-state/src/db.rs` | fns: `Delete`, `Ed25519Sig`, `Set`, `account_storage`
- **crates-3** (293) — files: `torus_consensus`, `hotstuff_rs`, `crates/torus-network/src/sync.rs`, `add_speculative_commit` | fns: `ChannelNetwork`, `GenesisConfig`, `Replica`, `RocksKVStore`
- **crates-4** (241) — files: `crates/torus-consensus/src/app.rs`, `crates/torus-core/src/position.rs`, `atomic_write`, `crates/torus-bridge/tests/bridge_tests.rs` | fns: `absorb_fetched_bodies`, `app_with_recording_exec`, `apply_fill`, `block_hash_uses_canonical_encoding`
- **crates-5** (224) — files: `crates/torus-core/tests/precompile_tests.rs`, `crates/torus-core/src/liquidation.rs`, `crates/torus-integration-tests/tests/stress.rs`, `crates/torus-core/tests/margin_tests.rs` | fns: `abi_address_roundtrip`, `addr`, `addr`, `addr`
- **crates-6** (223) — files: `crates/torus-integration-tests/tests/fee_flow.rs`, `crates/torus-economics/tests/permanent_stake_tests.rs`, `crates/torus-integration-tests/tests/snapshot_dos_keys.rs`, `crates/torus-economics/tests/governance_tests.rs` | fns: `addr`, `addr`, `addr`, `addr`
- **crates-7** (195) — files: `crates/torus-explorer/src/api.rs`, `alloy_primitives`, `committer`, `committer.rs` | fns: `ApiError`, `B256`, `BlockCommitter`, `BlockCommitter`
- **crates-8** (183) — files: `abort`, `add_native_action_from_gossip_trusted`, `add_native_action_presigned`, `tools/faucet/src/main.rs` | fns: `address_from_signing_key`, `admit_rejects_counted_by_reason`, `bare_eth_call_succeeds_at_height_with_base_fee`, `bincode_roundtrip_preserves_action_hash`
- **crates-10** (155) — files: `PublicKey`, `crates/torus-mempool/src/lib.rs`, `crates/torus-bridge/tests/exec_isolation_tests.rs`, `tools/wallet/src/sign.rs` | fns: `accepts_pre_eip155_legacy_tx`, `add_evm_tx`, `add_native_action`, `add_native_action_from_gossip`
- **..** (155) — files: `devnet/scripts/native-dup-factor.py`, `devnet/scripts/native-order-flood.py`, `devnet/scripts/native-transfer-probe.py`, `../../../linuxbrew/.linuxbrew/lib/node_modules/pyright/dist/typeshed-fallback/stdlib/collections/__init__.pyi` | fns: `(module) native-dup-factor.py`, `(module) native-order-flood.py`, `(module) native-transfer-probe.py`, `Counter`
- **crates-11** (153) — files: `abs_diff`, `crates/torus-economics/src/staking.rs`, `crates/torus-economics/src/governance.rs`, `crates/torus-economics/src/dev_pool.rs` | fns: `addr`, `addr`, `all_entries`, `all_pending_rotations`
- **crates-12** (151) — files: `accept`, `add_evm_tx`, `add_native_action_from_gossip`, `all_senders` | fns: `apply_config_defaults`, `best_candidate`, `borsh_write_address`, `borsh_write_address`
- **test** (145) — files: `db.rs`, `config.rs`, `serde`, `indexer.rs` | fns: `BlockRow`, `CandleRow`, `Config`, `Deserialize`
- **crates-14** (139) — files: `crates/hotstuff_rs/src/block_tree/accessors/internal.rs`, `crates/torus-network/src/transport.rs`, `crates/hotstuff_rs/src/block_sync/messages.rs`, `crates/hotstuff_rs/src/block_tree/pluggables.rs` | fns: `add_speculative_commit`, `addr`, `advance_app_fed_block_height`, `advance_highest_pc_from_remote`
- **crates-15** (131) — files: `torus_rpc:`, `common/mod.rs`, `torus_core::precompiles:`, `torus_economics::epoch:` | fns: `BlockNotifier`, `BlockNotifier`, `CoreWriterQueue`, `EpochManager`
- **..-16** (129) — files: `crates/torus-state/src/cf.rs`, `rocksdb`, `crates/torus-consensus/src/network.rs`, `Clone` | fns: `CF_CONSENSUS_META`, `Cache`, `ChannelNetwork`, `ColumnFamilyDescriptor`
- **crates-17** (125) — files: `alloy_primitives`, `torus_core`, `alloy_rlp`, `types.rs` | fns: `Address`, `CoreWriterQueue`, `Decodable`, `Delegation`
- **crates-18** (119) — files: `torus_types`, `hotstuff_rs:app:`, `hotstuff_rs:types::update_sets:`, `data_types` | fns: `Address`, `App`, `AppStateUpdates`, `BlockHeight`
- **crates-19** (117) — files: `crates/torus-bridge/src/error.rs`, `crates/torus-bridge/src/error.rs:BridgeError:`, `crates/torus-core/src/error.rs`, `thiserror` | fns: `BridgeError`, `Core`, `CoreError`, `Error`

## Directory Map
- `./` — 1285 files
- `crates/` — 243 files
- `devnet/` — 35 files
- `tools/` — 29 files
- `testnet/` — 21 files

---
For deeper queries use `run_tool('indexer', '<tool>', ...)`:
- `code_search` — fuzzy find by name or keyword
- `code_graph(relation=callers|readers|path|blast_radius)` — call/read chains & impact
- `code_clusters` / `code_cluster_members` — community details
