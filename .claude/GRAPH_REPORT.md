# Code Graph: Torus-hyperBFT
_Auto-generated 2026-06-14 14:16 UTC — do not edit._

**7476 nodes across 1285 files**

## Key Hubs
| Node | File | Connections |
|------|------|------------|
| Ok | ../../.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/result.rs | 90 |
| extend_from_slice | extend_from_slice | 89 |
| place_order | crates/torus-core/src/order_book.rs | 83 |
| unwrap | ../../.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/result.rs | 76 |
| build | build | 75 |
| addr | crates/torus-core/src/order_book.rs | 73 |
| fp | crates/torus-core/src/order_book.rs | 72 |
| alloy_primitives | alloy_primitives | 71 |
| book | crates/torus-core/src/order_book.rs | 71 |
| run | crates/torus-node/src/main.rs | 66 |
| put_cf_raw | put_cf_raw | 65 |
| to_bytes | to_bytes | 65 |
| std | std | 63 |
| torus_types | torus_types | 62 |
| put_account | put_account | 61 |

## Communities
- **crates** (402) — files: `crates/torus-core/src/error.rs`, `crates/torus-economics/src/error.rs`, `torus_state::cf:`, `torus_state` | fns: `Borsh`, `Borsh`, `CF_ACCOUNTS`, `CF_ACCOUNTS`
- **crates-1** (391) — files: `Cli`, `EconomicsError`, `EthApiServer`, `Logger` | fns: `MAX_ORDERS_PER_TRADER_PER_MARKET`, `MarketMarginConfig`, `crates/hotstuff_rs/src/hotstuff/mod.rs`, `crates/hotstuff_rs/src/lib.rs`
- **crates-2** (314) — files: `crates/torus-explorer/src/api.rs`, `crates/torus-bridge/src/error.rs`, `tools/faucet/src/main.rs`, `tools/tx-flood/src/main.rs` | fns: `AppState`, `BridgeError`, `Cli`, `Cli`
- **crates-3** (310) — files: `crates/hotstuff_rs/src/types/signed_messages.rs`, `crates/hotstuff_rs/src/block_sync/messages.rs`, `crates/hotstuff_rs/src/algorithm.rs`, `crates/hotstuff_rs/src/block_tree/variables.rs` | fns: `ActiveCollectorPair`, `AdvertiseBlock`, `AdvertisePC`, `Algorithm`
- **crates-4** (294) — files: `crates/torus-economics/src/reward_distributor.rs`, `crates/torus-state/src/cf.rs`, `crates/torus-consensus/src/slashing.rs`, `crates/torus-economics/src/governance.rs` | fns: `BLOCKS_PER_YEAR`, `CF_FEE_CONFIG`, `CF_TREASURY`, `DowntimeTracker`
- **crates-5** (277) — files: `Ed25519Sig`, `crates/hotstuff_rs/tests/common/node.rs`, `crates/hotstuff_rs/tests/common/number_app.rs`, `crates/torus-network/src/peer_scoring.rs` | fns: `Node`, `NumberAppTransaction`, `PeerScoring`, `SyncRequest`
- **tools** (271) — files: `crates/torus-explorer/src/api.rs`, `torus_types:`, `IntoResponse`, `crates/torus-genesis/src/lib.rs` | fns: `ApiError`, `FixedPoint`, `InvalidAddress`, `InvalidAmount`
- **crates-7** (267) — files: `crates/hotstuff_rs/src/block_tree/accessors/internal.rs`, `crates/hotstuff_rs/src/block_sync/messages.rs`, `../../.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/slice/iter.rs`, `../../.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/slice/iter/macros.rs` | fns: `add_speculative_commit`, `advance_highest_pc_from_remote`, `advertise_block`, `any`
- **crates-8** (262) — files: `crates/torus-state/src/cf.rs`, `crate::cf`, `crates/torus-state/src/native_trie.rs`, `crates/torus-types/src/lib.rs` | fns: `CF_HASHED_ACCOUNTS`, `CF_HASHED_ACCOUNTS`, `CF_NATIVE_HASHED`, `CF_NATIVE_TRIE`
- **crates-9** (205) — files: `torus_core::precompiles:`, `crates/torus-core/src/position.rs`, `torus_state::cf:`, `crates/torus-state/src/cf.rs` | fns: `ADDR_ORACLE_READER`, `ADDR_STAKING_READER`, `CF_NATIVE_BALANCES`, `CF_NATIVE_BALANCES`
- **crates-10** (176) — files: `crates/torus-state/src/native_trie.rs`, `torus_state::cf:`, `crates/torus-bridge/src/error.rs:BridgeError:`, `crates/torus-integration-tests/tests/common/mod.rs` | fns: `CF_NATIVE_BALANCES`, `CF_NATIVE_MARKETS`, `CF_NATIVE_ORACLE`, `CF_NATIVE_ORACLE`
- **crates-11** (157) — files: `torus_rpc`, `crates/torus-state/src/cf.rs`, `torus_evm`, `torus_mempool` | fns: `BlockNotifier`, `CF_STAKING_VALIDATORS`, `EvmExecutor`, `Mempool`
- **crates-12** (154) — files: `std::ops`, `crates/torus-evm/src/executor.rs`, `crates/torus-rpc/src/lib.rs`, `Default` | fns: `Add`, `BlockEnvCfg`, `BlockNotifier`, `Div`
- **crates-14** (145) — files: `crates/torus-state/src/cf.rs`, `rocksdb`, `crates/torus-types/src/lib.rs`, `crates/torus-bridge/src/error.rs` | fns: `CF_CONSENSUS_META`, `CF_NATIVE_NONCES`, `CF_NATIVE_PENDING`, `Cache`
- **test** (145) — files: `db.rs`, `config.rs`, `serde`, `indexer.rs` | fns: `BlockRow`, `CandleRow`, `Config`, `Deserialize`
- **crates-15** (137) — files: `torus_types`, `hotstuff_rs:app:`, `hotstuff_rs:types::update_sets:`, `data_types` | fns: `Address`, `App`, `AppStateUpdates`, `BlockHeight`
- **..** (136) — files: `devnet/scripts/exec_phase_table.py`, `devnet/scripts/native-dup-factor.py`, `devnet/scripts/native-order-flood.py`, `devnet/scripts/native-transfer-probe.py` | fns: `(module) exec_phase_table.py`, `(module) native-dup-factor.py`, `(module) native-order-flood.py`, `(module) native-transfer-probe.py`
- **crates-16** (136) — files: `torus_rpc:`, `common/mod.rs`, `torus_core::precompiles:`, `torus_economics::epoch:` | fns: `BlockNotifier`, `BlockNotifier`, `CoreWriterQueue`, `EpochManager`
- **crates-18** (122) — files: `crates/torus-types/src/lib.rs`, `torus_types:`, `PublicKey`, `../../.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/option.rs` | fns: `ListMarket`, `MarketListing`, `MarketParams`, `SubmitProposal`
- **crates-19** (109) — files: `crates/torus-core/src/order_book.rs`, `crates/torus-state/src/overlay.rs`, `is_none_or`, `modify_order` | fns: `accept_buy_stop_above_market`, `accept_valid_tick_size`, `addr`, `alloc_id`

## Directory Map
- `./` — 1034 files
- `crates/` — 213 files
- `tools/` — 26 files
- `devnet/` — 12 files

---
For deeper queries use `run_tool('indexer', '<tool>', ...)`:
- `code_search` — fuzzy find by name or keyword
- `code_callers` / `code_readers` — trace call/read chains
- `code_blast_radius` — impact analysis for a function
- `code_clusters` / `code_cluster_detail` — community details
