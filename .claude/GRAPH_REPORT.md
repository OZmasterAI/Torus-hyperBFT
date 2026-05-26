# Code Graph: Torus-hyperBFT
_Auto-generated 2026-05-23 07:38 UTC — do not edit._

**4744 nodes across 1183 files**

## Key Hubs
| Node | File | Connections |
|------|------|------------|
| place_order | crates/torus-core/src/order_book.rs | 81 |
| extend_from_slice | extend_from_slice | 80 |
| addr | crates/torus-core/src/order_book.rs | 72 |
| fp | crates/torus-core/src/order_book.rs | 72 |
| book | crates/torus-core/src/order_book.rs | 70 |
| build | build | 64 |
| put_cf_raw | put_cf_raw | 63 |
| put_account | put_account | 59 |
| to_bytes | to_bytes | 57 |
| verifying_key | verifying_key | 56 |
| get_cf_raw | get_cf_raw | 55 |
| limit_buy | crates/torus-core/src/order_book.rs | 55 |
| limit_sell | crates/torus-core/src/order_book.rs | 54 |
| copy_from_slice | copy_from_slice | 53 |
| stop | stop | 52 |

## Communities
- **crates** (587) — files: `cf`, `alloy_primitives`, `argon2`, `committer` | fns: `:ALL_CF_NAMES`, `:Address`, `:Argon2`, `:B256`
- **crates-1** (233) — files: `add_speculative_commit`, `advance_highest_pc_from_remote`, `crates/torus-mempool/src/evm_pool.rs`, `app_view` | fns: `all_senders`, `blacklist_contains_server_address`, `blacklist_sync_server`, `block_to_commit`
- **crates-2** (206) — files: `Ed25519Sig`, `add_address`, `crates/torus-bridge/tests/session_key_tests.rs`, `crates/hotstuff_rs/src/types/validator_set.rs` | fns: `addr`, `apply_updates`, `attestation_roundtrip`, `attestation_tampered_actions_fails`
- **crates-3** (192) — files: `crates/torus-integration-tests/tests/fee_flow.rs`, `crates/torus-economics/tests/permanent_stake_tests.rs`, `crates/torus-economics/tests/governance_tests.rs`, `crates/torus-economics/src/rewards.rs` | fns: `addr`, `addr`, `addr`, `addr`
- **crates-4** (183) — files: `crates/torus-core/tests/precompile_tests.rs`, `crates/torus-core/src/liquidation.rs`, `crates/torus-integration-tests/tests/stress.rs`, `crates/torus-core/tests/margin_tests.rs` | fns: `abi_address_roundtrip`, `addr`, `addr`, `addr`
- **crates-5** (169) — files: `abs_diff`, `crates/torus-economics/src/staking.rs`, `crates/torus-economics/src/governance.rs`, `crates/torus-economics/src/dev_pool.rs` | fns: `addr`, `addr`, `all_entries`, `all_pending_rotations`
- **crates-6** (167) — files: `add_evm_tx`, `crates/torus-core/src/oracle.rs`, `crates/torus-core/src/position.rs`, `atomic_write` | fns: `aggregate_price`, `aggregated_price_key`, `apply_fill`, `base_fee_decreases_below_target`
- **crates-7** (136) — files: `crates/hotstuff_rs/src/block_tree/accessors/internal.rs`, `crates/hotstuff_rs/src/block_sync/messages.rs`, `app_state`, `crates/torus-state/src/overlay.rs` | fns: `add_speculative_commit`, `advance_highest_pc_from_remote`, `advertise_block`, `app_view`
- **crates-8** (134) — files: `accept`, `crates/torus-integration-tests/tests/snapshot_dos_keys.rs`, `tools/wallet/src/keystore.rs`, `apply_pending_rotations` | fns: `addr`, `address_from_key`, `archive_mode_prunes_nothing`, `auto_snapshot_creates_and_prunes`
- **crates-9** (132) — files: `crates/torus-state/src/db.rs`, `crates/torus-bridge/src/native_executor.rs`, `aggregate_price`, `crates/torus-core/tests/oracle_tests.rs` | fns: `account_storage`, `aggregate_oracle_prices`, `all_accounts`, `all_same_price`
- **crates-10** (126) — files: `crates/torus-mempool/src/lib.rs`, `crates/torus-integration-tests/tests/rate_limit_mev.rs`, `all_senders`, `all_validators` | fns: `add_evm_tx`, `addr`, `address_from_key`, `anti_mev_same_gas_different_parent_hash`
- **tools** (121) — files: `abort`, `crates/torus-mempool/src/lib.rs`, `add_native_action_presigned`, `address_from_key` | fns: `add_native_action`, `add_native_action_presigned`, `block_number`, `call`
- **crates-12** (118) — files: `crates/torus-state/tests/state_tests.rs`, `account_storage`, `all_accounts`, `crates/torus-integration-tests/tests/fee_flow.rs` | fns: `account_delete`, `account_storage_iterator`, `account_write_read_roundtrip`, `all_accounts_iterator`
- **crates-13** (116) — files: `allows`, `crates/torus-types/src/eip712.rs`, `crates/torus-integration-tests/tests/cross_vm_write.rs`, `crates/torus-types/src/lib.rs` | fns: `batch_verify_all_valid_eip712`, `batch_verify_native_actions`, `call_data`, `canonical_bytes`
- **crates-14** (106) — files: `crates/torus-core/src/order_book.rs`, `crates/torus-state/src/overlay.rs`, `is_none_or`, `next_back` | fns: `accept_buy_stop_above_market`, `accept_valid_tick_size`, `addr`, `alloc_id`
- **crates-15** (102) — files: `internal`, `signed_messages`, `sha2`, `messages` | fns: `:BlockTreeError`, `:Certificate`, `:Digest`, `:Message`
- **crates-16** (92) — files: `build`, `dedup`, `crates/torus-rpc/tests/eth_compliance_tests.rs`, `crates/torus-rpc/src/lib.rs` | fns: `estimate_gas_returns_error_on_revert`, `eth_block_number`, `eth_call_rejects_historical_block_tag`, `eth_call_simple_transfer`
- **devnet** (80) — files: `Crypto.Hash`, `KeyAPI`, `PrivateKey`, `ThreadPoolExecutor` | fns: `address_to_uint256`, `bool_to_uint256`, `eip712_signing_hash`, `get_block_native_count`
- **crates-18** (78) — files: `aes-gcm`, `alloy-consensus`, `alloy-eips`, `alloy-primitives`
- **crates-19** (67) — files: `crates/torus-explorer/src/api.rs`, `as_array`, `as_i64`, `as_object` | fns: `api_address_txs`, `api_get_block`, `api_get_tx`, `api_list_blocks`

## Directory Map
- `./` — 943 files
- `crates/` — 209 files
- `tools/` — 22 files
- `devnet/` — 9 files

---
For deeper queries use `run_tool('indexer', '<tool>', ...)`:
- `code_search` — fuzzy find by name or keyword
- `code_callers` / `code_readers` — trace call/read chains
- `code_blast_radius` — impact analysis for a function
- `code_clusters` / `code_cluster_detail` — community details
