# Torus-hyperBFT: Technical Requirements Document

**Date:** 2026-04-12
**Status:** Draft v1.0
**Stack:** hotstuff_rs + revm + alloy + reth-trie + libp2p + jsonrpsee + RocksDB
**Decision Basis:** [hotstuff-evm-options.md](./hotstuff-evm-options.md), [hyperliquid-deep-dive.md](./hyperliquid-deep-dive.md)

---

## Table of Contents

1. [System Architecture](#1-system-architecture)
2. [Crate Structure](#2-crate-structure)
3. [Consensus Layer](#3-consensus-layer)
4. [Execution Layer (EVM)](#4-execution-layer-evm)
5. [Native Execution Layer (torus-core)](#5-native-execution-layer-torus-core)
6. [State Layer](#6-state-layer)
7. [Network Layer](#7-network-layer)
8. [RPC Layer](#8-rpc-layer)
9. [Consensus-Execution Bridge](#9-consensus-execution-bridge)
10. [Cross-VM Interaction](#10-cross-vm-interaction)
11. [Economic Modules](#11-economic-modules)
12. [Genesis Format](#12-genesis-format)
13. [Dependency Version Matrix](#13-dependency-version-matrix)

---

## 1. System Architecture

### 1.1 High-Level Architecture Diagram

```
                            ┌─────────────────────────────────────────┐
                            │              Clients                    │
                            │  MetaMask / Foundry / Custom SDK / CLI  │
                            └──────────────────┬──────────────────────┘
                                               │
                                    JSON-RPC (HTTP/WS)
                                               │
                            ┌──────────────────▼──────────────────────┐
                            │            RPC Layer                    │
                            │         (jsonrpsee)                     │
                            │  eth_* / net_* / torus_* namespaces     │
                            │  WebSocket subscriptions                │
                            └──────────┬──────────────┬───────────────┘
                                       │              │
                            ┌──────────▼──────┐  ┌────▼──────────────┐
                            │   Transaction   │  │   Query Engine    │
                            │      Pool       │  │  (read from state)│
                            │  (mempool)      │  │                   │
                            └──────────┬──────┘  └───────────────────┘
                                       │
                            ┌──────────▼──────────────────────────────┐
                            │         Consensus Layer                 │
                            │        (hotstuff_rs)                    │
                            │                                         │
                            │  ┌─────────────────────────────────┐    │
                            │  │  MonadBFT-Inspired Extensions   │    │
                            │  │  - Tail-fork resistance (NEC)   │    │
                            │  │  - Speculative finality          │    │
                            │  │  - Active PaceMaker              │    │
                            │  └─────────────────────────────────┘    │
                            │                                         │
                            │  App trait: produce_block()              │
                            │             validate_block()             │
                            │             validate_block_for_sync()    │
                            └──────────────────┬──────────────────────┘
                                               │
                            ┌──────────────────▼──────────────────────┐
                            │     Consensus-Execution Bridge          │
                            │     (torus-bridge crate)                │
                            │                                         │
                            │  Block Proposal Construction            │
                            │  Transaction Ordering                   │
                            │  State Diff Collection                  │
                            │  MPT State Root Computation             │
                            │  Receipt/Log Storage                    │
                            └────┬────────────────────┬───────────────┘
                                 │                    │
                    ┌────────────▼──────┐  ┌──────────▼───────────────┐
                    │  Native Engine    │  │     EVM Engine           │
                    │  (torus-core)     │  │     (revm)               │
                    │                   │  │                          │
                    │  Order Book CLOB  │  │  Cancun-spec EVM         │
                    │  Margin Engine    │  │  Custom Precompiles      │
                    │  Liquidations     │  │  StatefulPrecompile      │
                    │  Staking Logic    │  │  CoreWriter Contract     │
                    │  Fee Split Logic  │  │                          │
                    │  Governance       │  │  Database trait impl     │
                    └────────┬─────────┘  └──────────┬───────────────┘
                             │                       │
                             │   Cross-VM Bridge     │
                             │◄─────────────────────►│
                             │  - Read precompiles   │
                             │  - CoreWriter actions  │
                             │  - Lockbox transfers   │
                             │                       │
                    ┌────────▼───────────────────────▼───────────────┐
                    │              State Layer                       │
                    │         (torus-state crate)                    │
                    │                                                │
                    │  ┌────────────┐  ┌──────────────────────────┐  │
                    │  │  RocksDB   │  │  Merkle Patricia Trie    │  │
                    │  │  (storage) │  │  (reth-trie)             │  │
                    │  │            │  │  State root computation  │  │
                    │  │  Column    │  │  Proof generation        │  │
                    │  │  Families  │  │                          │  │
                    │  └────────────┘  └──────────────────────────┘  │
                    └───────────────────────────────────────────────┘
                                               │
                            ┌──────────────────▼──────────────────────┐
                            │          Network Layer                  │
                            │         (rust-libp2p)                   │
                            │                                         │
                            │  ┌───────────┐ ┌──────────┐ ┌────────┐ │
                            │  │ Consensus │ │   Tx     │ │ Block  │ │
                            │  │ Messages  │ │ Gossip   │ │  Sync  │ │
                            │  │ (GossipSub│ │(GossipSub│ │(Req/Res│ │
                            │  │  topic)   │ │ topic)   │ │ proto) │ │
                            │  └───────────┘ └──────────┘ └────────┘ │
                            │                                         │
                            │  QUIC transport / Kademlia DHT          │
                            └─────────────────────────────────────────┘
```

### 1.2 Data Flow: Transaction Lifecycle

```
Client submits tx (eth_sendRawTransaction)
    │
    ▼
RPC Layer: decode, validate signature, check nonce
    │
    ▼
Mempool: insert, prioritize by gas price / native priority
    │
    ▼
Consensus Leader: produce_block()
    │
    ├── Pull native actions from native mempool
    │   (order placements, cancellations, staking, governance)
    │
    ├── Pull EVM transactions from EVM mempool
    │
    └── Construct Block { native_actions, evm_txs, timestamp, parent_hash }
    │
    ▼
Consensus: Broadcast proposal → Validators vote → QC formed
    │
    ▼
All Validators: validate_block()
    │
    ├── 1. Execute native actions (torus-core)
    │   └── Order matching, margin checks, staking state changes
    │
    ├── 2. Execute EVM transactions (revm)
    │   └── Smart contract execution, state diffs collected
    │
    ├── 3. Process EVM-to-Core transfers (lockbox)
    │
    ├── 4. Process CoreWriter actions (delayed queue)
    │
    ├── 5. Compute fee split (burn/validator/treasury/dev-pool)
    │
    ├── 6. Compute MPT state root over all state changes
    │
    └── 7. Verify state root matches proposal
    │
    ▼
Commit: Block finalized, state persisted to RocksDB
    │
    ▼
Notify: RPC subscribers (newHeads, logs, newPendingTransactions)
```

### 1.3 Dual-VM Execution Model

Following Hyperliquid's HyperCore/HyperEVM pattern:

| Layer | Engine | Purpose | Gas Model |
|---|---|---|---|
| **Native (torus-core)** | Custom Rust | Order book, staking, fees, governance | Zero gas (native actions) |
| **EVM (torus-evm)** | revm (Cancun) | Arbitrary smart contracts | EIP-1559 gas pricing |

**Execution order within each block (critical for determinism):**
1. Native actions execute first (order matches, staking, governance)
2. EVM transactions execute second (using latest native state)
3. EVM-to-native transfers processed (lockbox outflows)
4. CoreWriter actions from EVM queued for next block (anti-frontrunning delay)
5. Fee distribution computed over all actions
6. State root computed over combined state

---

## 2. Crate Structure

### 2.1 Workspace Layout

```
torus-hyperbft/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── torus-node/               # Binary entry point
│   ├── torus-consensus/          # Consensus layer (hotstuff_rs integration)
│   ├── torus-bridge/             # Consensus-execution bridge
│   ├── torus-evm/                # EVM execution (revm wrapper)
│   ├── torus-core/               # Native execution engine (order book, matching)
│   ├── torus-state/              # State storage (RocksDB + MPT)
│   ├── torus-network/            # P2P networking (libp2p)
│   ├── torus-rpc/                # JSON-RPC server (jsonrpsee)
│   ├── torus-mempool/            # Transaction pool
│   ├── torus-economics/          # Staking, fees, governance
│   ├── torus-types/              # Shared types, primitives
│   └── torus-genesis/            # Genesis parsing and chain config
├── bin/
│   └── torus/                    # CLI binary (wraps torus-node)
├── tests/
│   ├── consensus/                # Consensus safety tests
│   ├── integration/              # Multi-node integration tests
│   └── evm/                      # Ethereum test vector suite
└── research/                     # This document + prior research
```

### 2.2 Crate Responsibilities

#### `torus-types` (leaf crate, no internal dependencies)
- **Purpose:** Shared types used across all crates
- **Public API:**
  - `Block`, `BlockHeader`, `BlockBody` — canonical block format
  - `NativeAction` — enum of all native action types (orders, staking, governance)
  - `EvmTransaction` — wrapper around alloy `Transaction`
  - `StateDiff` — account/storage changes from execution
  - `Receipt`, `Log` — execution results
  - `ValidatorSet`, `ValidatorInfo` — validator metadata
  - `ChainConfig` — chain parameters (chain ID, gas limits, fee ratios)
- **Dependencies:** `alloy-primitives`, `alloy-consensus`, `serde`, `borsh`

#### `torus-state` (storage layer)
- **Purpose:** RocksDB storage + MPT state root computation
- **Public API:**
  - `StateDb` — implements revm `Database` trait
  - `StateDb::commit(diffs: &[StateDiff])` — apply state changes
  - `StateDb::state_root() -> B256` — compute MPT root via reth-trie
  - `StateDb::get_proof(address, slots) -> AccountProof` — Merkle proof
  - `StateDb::snapshot() -> StateSnapshot` — point-in-time read view
  - Column family accessors for each data domain
- **Dependencies:** `rocksdb`, `reth-trie`, `torus-types`, `alloy-primitives`

#### `torus-evm` (EVM execution)
- **Purpose:** Execute EVM transactions, manage precompiles
- **Public API:**
  - `EvmExecutor::new(state: &StateDb, cfg: &ChainConfig) -> Self`
  - `EvmExecutor::execute_block(txs: &[EvmTransaction]) -> EvmBlockResult`
  - `EvmBlockResult { state_diffs, receipts, logs, gas_used }`
  - `register_precompiles(handler: &mut EvmHandler)` — inject Torus precompiles
- **Dependencies:** `revm`, `torus-state`, `torus-types`, `alloy-eips`

#### `torus-core` (native execution engine)
- **Purpose:** Order book CLOB, margin engine, liquidations
- **Public API:**
  - `CoreEngine::new(state: &StateDb) -> Self`
  - `CoreEngine::execute_actions(actions: &[NativeAction]) -> CoreResult`
  - `CoreResult { state_diffs, events, matched_trades }`
  - `OrderBook` — price-time priority matching
  - `MarginEngine` — cross/isolated margin checks
  - `LiquidationEngine` — continuous monitoring + ADL
- **Dependencies:** `torus-state`, `torus-types`, `torus-economics`

#### `torus-economics` (economic modules)
- **Purpose:** Staking, fee split, governance weight
- **Public API:**
  - `PermanentStaking::stake(addr, amount)` / `::rewards(epoch) -> Distribution`
  - `FeeSplitter::split(total_fees, epoch) -> FeeDistribution`
  - `FeeDistribution { burn, validator, treasury, dev_pool }`
  - `GovernanceWeight::voting_power(addr) -> U256` — includes 1.5x permanent multiplier
- **Dependencies:** `torus-state`, `torus-types`

#### `torus-consensus` (consensus integration)
- **Purpose:** Implement hotstuff_rs `App` trait, manage validator sets
- **Public API:**
  - `TorusApp` — implements `hotstuff_rs::app::App`
  - `ValidatorManager` — epoch-based validator set rotation
  - `ConsensusConfig` — hotstuff_rs configuration wrapper
- **Dependencies:** `hotstuff_rs`, `torus-bridge`, `torus-types`

#### `torus-bridge` (consensus-execution bridge)
- **Purpose:** The critical glue between consensus and execution
- **Public API:**
  - `BlockProposer::build_block(mempool, parent) -> Block`
  - `BlockValidator::validate_block(block) -> Result<ValidatedBlock>`
  - `BlockCommitter::commit_block(validated) -> Result<()>`
  - `ValidatedBlock { block, state_diffs, receipts, state_root }`
- **Dependencies:** `torus-evm`, `torus-core`, `torus-state`, `torus-economics`, `torus-types`

#### `torus-network` (P2P layer)
- **Purpose:** libp2p networking for consensus, tx gossip, block sync
- **Public API:**
  - `NetworkService::new(config) -> Self`
  - `NetworkService::broadcast_consensus(msg: ConsensusMessage)`
  - `NetworkService::broadcast_tx(tx: Transaction)`
  - `NetworkService::request_blocks(peer, range) -> Vec<Block>`
  - `NetworkService::subscribe() -> NetworkEventStream`
- **Dependencies:** `libp2p`, `torus-types`

#### `torus-rpc` (JSON-RPC server)
- **Purpose:** Ethereum-compatible RPC + Torus-specific namespace
- **Public API:**
  - `RpcServer::new(state, mempool, config) -> Self`
  - `RpcServer::start(addr) -> JoinHandle`
  - `EthApi` — eth_* method implementations
  - `TorusApi` — torus_* method implementations (native actions)
- **Dependencies:** `jsonrpsee`, `torus-state`, `torus-mempool`, `torus-types`

#### `torus-mempool` (transaction pool)
- **Purpose:** Dual mempool for native actions + EVM transactions
- **Public API:**
  - `Mempool::insert_evm(tx: EvmTransaction) -> Result<()>`
  - `Mempool::insert_native(action: NativeAction) -> Result<()>`
  - `Mempool::drain_for_block(limits) -> (Vec<NativeAction>, Vec<EvmTransaction>)`
  - Priority: native non-GTC > cancellations > EVM by gas price > native GTC
- **Dependencies:** `torus-types`, `torus-state`

#### `torus-genesis` (chain initialization)
- **Purpose:** Parse genesis file, initialize chain state
- **Public API:**
  - `Genesis::from_file(path) -> Result<Genesis>`
  - `Genesis::initialize(state: &mut StateDb) -> Result<B256>`
- **Dependencies:** `torus-state`, `torus-types`, `serde_json`

#### `torus-node` (binary orchestrator)
- **Purpose:** Wire all crates together, start node
- **Public API:** `main()` — CLI entry point
- **Dependencies:** All crates above, `clap`, `tokio`, `tracing`

### 2.3 Dependency Graph

```
torus-types (leaf)
    │
    ├── torus-state
    │       │
    │       ├── torus-evm
    │       ├── torus-core ──── torus-economics
    │       └── torus-mempool
    │
    ├── torus-economics
    │
    ├── torus-bridge ─── torus-evm + torus-core + torus-state + torus-economics
    │
    ├── torus-consensus ─── hotstuff_rs + torus-bridge
    │
    ├── torus-network ─── libp2p
    │
    ├── torus-rpc ─── jsonrpsee + torus-state + torus-mempool
    │
    ├── torus-genesis ─── torus-state
    │
    └── torus-node ─── ALL
```

---

## 3. Consensus Layer

### 3.1 hotstuff_rs v0.4.0 Integration

hotstuff_rs v0.4.0 (Apache-2.0, crates.io) provides a pluggable BFT consensus library. **v0.4.0 had a near-complete API overhaul from v0.3** — do not reference older documentation.

#### App Trait (consensus application interface)

The `App<K: KVStore>` trait is the primary integration point. All three methods required, no defaults.

```rust
// hotstuff_rs::app::App (v0.4.0 exact signatures)
pub trait App<K: KVStore>: Send {
    fn produce_block(
        &mut self,
        request: ProduceBlockRequest<'_, K>,
    ) -> ProduceBlockResponse;

    fn validate_block(
        &mut self,
        request: ValidateBlockRequest<'_, '_, K>,
    ) -> ValidateBlockResponse;

    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<'_, '_, K>,
    ) -> ValidateBlockResponse;
}

// ProduceBlockRequest (fields private, accessed via methods):
//   cur_view() -> ViewNumber
//   parent_block() -> Option<CryptoHash>     // None only for genesis
//   block_tree() -> &AppBlockTreeView<'_, K>  // deterministic state queries

// ProduceBlockResponse (all fields public):
pub struct ProduceBlockResponse {
    pub data_hash: CryptoHash,                              // SHA-256 hash over data
    pub data: Data,                                         // Vec<Datum> (Vec<Vec<u8>>)
    pub app_state_updates: Option<AppStateUpdates>,         // UpdateSet<Vec<u8>, Vec<u8>>
    pub validator_set_updates: Option<ValidatorSetUpdates>, // UpdateSet<VerifyingKey, Power>
}

// ValidateBlockResponse:
pub enum ValidateBlockResponse {
    Valid {
        app_state_updates: Option<AppStateUpdates>,
        validator_set_updates: Option<ValidatorSetUpdates>,
    },
    Invalid,
}
```

**Critical design notes:**
- App state changes and validator changes are NOT stored in the Block — returned during validation, committed only at 2-QC depth
- `data` is raw `Vec<Datum>` (`Vec<Vec<u8>>`) — format entirely app-defined (we serialize `TorusBlock` into it)
- `AppBlockTreeView` exposes only deterministic fields: block queries, committed app state, current validator set

#### KVStore Trait (consensus-internal storage)

```rust
// hotstuff_rs::block_tree::pluggables (v0.4.0)
// NOT dyn-compatible due to GAT on Snapshot
pub trait KVStore: KVGet + Clone + Send + 'static {
    type WriteBatch: WriteBatch;
    type Snapshot<'a>: 'a + KVGet;

    fn write(&mut self, wb: Self::WriteBatch);
    fn clear(&mut self);
    fn snapshot<'b>(&'b self) -> Self::Snapshot<'_>;
}

// KVGet supertrait — only 1 required method, 24 provided methods
pub trait KVGet {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;  // Only method to implement
    // 24 provided: block(), block_height(), committed_app_state(),
    // committed_validator_set(), highest_committed_block(), etc.
}

pub trait WriteBatch {
    fn new() -> Self;
    fn set(&mut self, key: &[u8], value: &[u8]);
    fn delete(&mut self, key: &[u8]);
}
```

Implementation: Wrap RocksDB with a dedicated column family (`cf_consensus_meta`). Only `get()` needs custom implementation — the 24 provided methods handle all block tree queries automatically.

#### Network Trait (message transport)

```rust
// hotstuff_rs::networking::network (v0.4.0)
pub trait Network: Clone + Send {
    fn init_validator_set(&mut self, validator_set: ValidatorSet);
    fn update_validator_set(&mut self, updates: ValidatorSetUpdates);
    fn broadcast(&mut self, message: Message);        // non-blocking
    fn send(&mut self, peer: VerifyingKey, message: Message);  // non-blocking
    fn recv(&mut self) -> Option<(VerifyingKey, Message)>;     // non-blocking poll
}
```

- `broadcast` must reach all peers including non-voting listeners
- `Message` wraps `ProgressMessage` (consensus) and `BlockSyncMessage` (sync)
- Peers identified by Ed25519 `VerifyingKey`, not libp2p `PeerId` — bridge required

Implementation: Bridge to libp2p GossipSub (broadcast) and direct peer connections (send/recv). Maintain `HashMap<VerifyingKey, PeerId>` for address translation.

#### Replica Configuration and Startup

```rust
// 12 required fields via TypedBuilder
let config = Configuration::builder()
    .me(signing_key)                                        // Ed25519 SigningKey
    .chain_id(ChainID(7777))
    .epoch_length(EpochLength(100_000))
    .max_view_time(Duration::from_secs(2))                  // min 500ms
    .progress_msg_buffer_capacity(BufferSize(1024))
    .block_sync_request_limit(10)
    .block_sync_server_advertise_time(Duration::from_millis(500))
    .block_sync_response_timeout(Duration::from_secs(5))
    .block_sync_blacklist_expiry_time(Duration::from_secs(30))
    .block_sync_trigger_min_view_difference(3)
    .block_sync_trigger_timeout(Duration::from_secs(10))
    .log_events(true)
    .build();

// Initialize genesis (once, before start)
Replica::initialize(kv_store.clone(), initial_app_state, initial_validator_set_state);

// Build and start (runs in background threads, drop to shut down)
let replica = ReplicaSpec::builder()
    .app(torus_app).network(torus_network).kv_store(kv_store)
    .configuration(config)
    .on_commit_block(|e: CommitBlockEvent| { /* notify RPC */ })
    // 25 optional event handlers available
    .build().start();
```

**Important:** Pacemaker is NOT injectable in v0.4.0 — it's a built-in concrete module. Leader selection uses Interleaved Weighted Round-Robin (IWRR), proportional to `Power/TotalPower`. Block sync is fully automatic (triggered by view gap or timeout).

### 3.2 Block Format

**hotstuff_rs Block vs Torus Block:** The library's `Block` contains `{height, hash, justify: PhaseCertificate, data_hash, data: Vec<Datum>}`. It has NO timestamp, parent_hash, or structured fields — `data` is raw bytes. We serialize our `TorusBlock` into the library's `Data` field.

```rust
/// Canonical Torus block — Borsh-serialized into hotstuff_rs Data field.
/// Each TorusBlock becomes a single Datum in the library's Vec<Datum>.
#[derive(BorshSerialize, BorshDeserialize, Clone)]
pub struct TorusBlock {
    /// Block header (Torus application-level)
    pub header: TorusBlockHeader,
    /// Native actions (order book, staking, governance)
    pub native_actions: Vec<NativeAction>,
    /// EVM transactions (RLP-encoded, signed)
    pub evm_transactions: Vec<Bytes>,
    /// CoreWriter action queue (from previous block's EVM)
    pub core_writer_actions: Vec<CoreWriterAction>,
}

#[derive(BorshSerialize, BorshDeserialize, Clone)]
pub struct TorusBlockHeader {
    pub timestamp: u64,
    pub proposer: Address,
    /// State root after executing all actions in this block
    pub state_root: B256,
    /// Receipts root (EVM transactions only)
    pub receipts_root: B256,
    /// Logs bloom (EVM transactions only)
    pub logs_bloom: Bloom,
    /// Gas used by EVM transactions
    pub evm_gas_used: u64,
    /// Gas limit for EVM transactions in this block
    pub evm_gas_limit: u64,
    /// Native action count
    pub native_action_count: u32,
    /// EVM transaction count
    pub evm_tx_count: u32,
    /// Base fee per gas (EIP-1559)
    pub base_fee_per_gas: u64,
    /// Epoch number (validator set epoch)
    pub epoch: u64,
    /// Validator set hash for this epoch
    pub validator_set_hash: B256,
}
// Note: height and parent come from hotstuff_rs Block.height and Block.justify
// respectively — no duplication needed.
```

### 3.3 Validator Set Management

**Epoch-based rotation** (following Hyperliquid's model):

- Epoch length: 100,000 consensus rounds (~configurable, start with shorter for devnet)
- Validator set is **static within an epoch** — no mid-epoch changes
- At epoch boundary:
  1. Snapshot permanent + delegated stake from `torus-economics`
  2. Rank by total stake (self + delegated)
  3. Top N validators become active set (N=4 for devnet, 21 for production)
  4. Compute `validator_set_hash = keccak256(sorted_validators)`
  5. All nodes must agree on the new set before producing epoch+1 blocks

**Validator state machine:**

```
Candidate ──stake──► Active ──epoch_end──► Re-evaluated
    ▲                  │                       │
    │                  ▼                       ▼
    │               Jailed ──unjail──►     Active (if still top N)
    │                  │                   or Candidate (if not)
    │                  ▼
    └──────────── Tombstoned (permanent ban, optional)
```

### 3.4 MonadBFT-Inspired Enhancements

These are Phase 2+ enhancements to the base hotstuff_rs protocol:

1. **Tail-forking resistance (reproposal + NEC):**
   - When a leader fails, the next leader must repropose the failed leader's block if it received votes
   - No-Endorsement Certificate (NEC): proof that a block was NOT endorsed by quorum, allowing skip
   - Prevents validators from selectively dropping competitor transactions

2. **Speculative finality:**
   - Execute block optimistically after 1 QC (speculative commit)
   - Deterministic commit after 2 QCs (standard HotStuff 2-chain rule)
   - Rollback speculative state on equivocation (rare, requires Byzantine leader)

3. **Active PaceMaker:**
   - Timeout certificates aggregate into view-change proof
   - Exponential backoff: 500ms base, 2x per consecutive timeout, cap at 30s
   - Leader reputation: track proposal success rate, skip known-bad leaders

---

## 4. Execution Layer (EVM)

### 4.1 revm Database Trait Implementation

The `Database` trait from revm v19.2.0 (pinned version) must be implemented over our RocksDB state:

```rust
// revm::Database (v19.2.0 — exact signatures)
#[auto_impl(&mut, Box)]
pub trait Database {
    type Error: DBErrorMarker;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error>;
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error>;
    fn storage(&mut self, address: Address, index: StorageKey) -> Result<StorageValue, Self::Error>;
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error>;
}

// AccountInfo fields: balance: U256, nonce: u64, code_hash: B256, code: Option<Bytecode>

// DatabaseRef (&self variant) also available — use WrapDatabaseRef<T> to adapt
// DatabaseCommit: fn commit(&mut self, changes: AddressMap<Account>)
```

**Implementation strategy — use revm's `State<DB>` wrapper:**

```rust
use revm::db::{State, StateBuilder};

// Our raw RocksDB implementation
pub struct TorusRocksDb {
    db: Arc<rocksdb::DB>,
    block_hash_cache: LruCache<u64, B256>,
}

impl Database for TorusRocksDb {
    type Error = TorusDbError;
    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Read from cf_accounts column family
    }
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        // Read from cf_code column family
    }
    fn storage(&mut self, address: Address, index: StorageKey) -> Result<StorageValue, Self::Error> {
        // Read from cf_storage column family (key = address ++ slot)
    }
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        // Read from cf_block_headers, cache in LRU
    }
}

// Wrap in State<DB> to accumulate BundleState for batch commit + trie
let mut state = StateBuilder::new_with_database(rocks_db)
    .with_bundle_update()
    .build();

// After executing all txs in a block:
let bundle: BundleState = state.take_bundle();
// → Use with reth-trie: HashedPostState::from_bundle_state(&bundle.state)
```
```

### 4.2 EVM Configuration (v19.2.0 API)

```rust
// Chain configuration — following Hyperliquid's exact pattern
let cfg = CfgEnvWithHandlerCfg::new_with_spec_id(
    CfgEnv {
        chain_id: 7777,                    // Torus mainnet (devnet: 7778)
        disable_eip3607: true,             // Allow txs from contracts (system txs)
        ..Default::default()
    },
    SpecId::CANCUN,                        // Cancun EVM spec (no EIP-4844 blobs)
);

// Block environment (set per block)
let block_env = BlockEnv {
    number: U256::from(block_height),
    timestamp: U256::from(block_timestamp),
    coinbase: proposer_address,            // Proposer gets validator share of fees
    gas_limit: U256::from(30_000_000),     // 30M gas limit per block
    basefee: U256::from(base_fee),         // EIP-1559 dynamic base fee
    ..Default::default()
};

// EVM construction with custom precompiles (v19 builder pattern)
let mut evm = Evm::builder()
    .with_db(&mut state)
    .with_env_with_handler_cfg(cfg)
    .append_handler_register_box(Box::new(move |handler| {
        // Inject precompiles via handler.pre_execution.load_precompiles
        inject_torus_precompiles(handler, core_state.clone());
    }))
    .build();

// Execute single transaction
evm.context.evm.env.tx = tx_env;
let result_and_state = evm.transact()?;
// result_and_state.result: ExecutionResult { Success { logs, output, gas_used, .. } | Revert | Halt }
// result_and_state.state: HashMap<Address, Account> (state changes)
```

### 4.3 State Diff Collection

After executing each EVM transaction, collect state changes:

```rust
pub struct EvmStateDiff {
    pub address: Address,
    pub balance_change: Option<(U256, U256)>,  // (old, new)
    pub nonce_change: Option<(u64, u64)>,
    pub code_change: Option<Bytecode>,
    pub storage_changes: Vec<StorageChange>,
    pub destroyed: bool,
}

pub struct StorageChange {
    pub slot: U256,
    pub old_value: U256,
    pub new_value: U256,
}
```

### 4.4 Receipt and Log Format

```rust
/// EVM execution receipt (alloy-compatible)
pub struct TorusReceipt {
    pub tx_hash: B256,
    pub tx_index: u64,
    pub block_hash: B256,
    pub block_number: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub cumulative_gas_used: u64,
    pub gas_used: u64,
    pub contract_address: Option<Address>,  // If contract creation
    pub logs: Vec<Log>,
    pub logs_bloom: Bloom,
    pub status: bool,                       // true = success
    pub effective_gas_price: u64,
}

/// Event log (ERC-20 Transfer, etc.)
pub struct Log {
    pub address: Address,
    pub topics: Vec<B256>,     // topic[0] = event signature hash
    pub data: Bytes,
}
```

---

## 5. Native Execution Layer (torus-core)

### 5.1 Order Book Matching Engine

**Architecture:** Price-time priority Central Limit Order Book (CLOB), modeled on Hyperliquid's HyperCore.

```rust
pub struct OrderBook {
    /// Market identifier
    pub market_id: MarketId,
    /// Buy orders sorted by price (descending), then time (ascending)
    pub bids: BTreeMap<PriceLevel, VecDeque<Order>>,
    /// Sell orders sorted by price (ascending), then time (ascending)  
    pub asks: BTreeMap<PriceLevel, VecDeque<Order>>,
    /// Order index for O(1) cancel lookups
    pub order_index: HashMap<OrderId, OrderRef>,
    /// Tick size (minimum price increment)
    pub tick_size: Decimal,
    /// Lot size (minimum quantity increment)
    pub lot_size: Decimal,
}

pub struct Order {
    pub id: OrderId,
    pub trader: Address,
    pub side: Side,              // Buy or Sell
    pub price: Decimal,
    pub remaining_qty: Decimal,
    pub original_qty: Decimal,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub timestamp: u64,          // Consensus timestamp for ordering
    pub reduce_only: bool,
}
```

**Matching algorithm:**
1. New order arrives
2. Check margin (pre-trade margin check)
3. Match against opposite side at best available price
4. For each fill: verify margin at execution time (double-check)
5. Update positions, balances, and PnL
6. If partial fill and GTC: insert remainder into book
7. Emit `TradeEvent` for each fill

### 5.2 Order Types

| Type | Enum Variant | Behavior |
|---|---|---|
| Market | `OrderType::Market` | Immediate execution at best available price |
| Limit | `OrderType::Limit` | Execute at specified price or better |
| Stop Market | `OrderType::StopMarket { trigger }` | Becomes market when trigger price hit |
| Stop Limit | `OrderType::StopLimit { trigger, limit }` | Becomes limit when trigger price hit |
| Post Only (ALO) | `TimeInForce::PostOnly` | Rejected if would immediately match |

**Time-in-force modifiers:**
- `GTC` (Good-Till-Cancelled): Default. Rests on book.
- `IOC` (Immediate-Or-Cancel): Fill what you can, cancel rest.
- `FOK` (Fill-Or-Kill): Fill entirely or reject entirely.
- `PostOnly`: Must make liquidity; rejected if crosses spread.

### 5.3 Margin Model

```rust
pub struct MarginEngine {
    pub mode: MarginMode,  // Cross or Isolated
}

pub enum MarginMode {
    /// Shared collateral across all positions
    Cross,
    /// Dedicated collateral per position
    Isolated { margin_per_position: HashMap<MarketId, Decimal> },
}

/// Margin check result
pub struct MarginCheck {
    pub initial_margin_required: Decimal,    // To open position
    pub maintenance_margin_required: Decimal, // To keep position
    pub available_margin: Decimal,
    pub margin_ratio: Decimal,               // equity / notional
    pub is_liquidatable: bool,               // maintenance breached
}
```

**Margin tiers** (following Hyperliquid's model for large positions):

| Notional Value | Max Leverage | Maintenance Margin |
|---|---|---|
| $0 - $1M | 40x (BTC), 25x (ETH) | 2.5% |
| $1M - $5M | 20x | 5% |
| $5M - $10M | 10x | 10% |
| $10M+ | 5x | 20% |

### 5.4 Liquidation Engine

```rust
pub struct LiquidationEngine;

impl LiquidationEngine {
    /// Check all positions for liquidation eligibility.
    /// Called every block after execution.
    pub fn check_liquidations(
        positions: &[Position],
        oracle_prices: &HashMap<MarketId, Decimal>,
    ) -> Vec<LiquidationAction>;

    /// Execute liquidation: close position at oracle price.
    /// If insufficient counterparty liquidity, trigger ADL.
    pub fn execute_liquidation(
        action: LiquidationAction,
        order_book: &mut OrderBook,
    ) -> LiquidationResult;
}

pub enum LiquidationAction {
    /// Standard liquidation: force-close at oracle price
    ForcedClose { position: PositionId, price: Decimal },
    /// Auto-Deleverage: force-reduce profitable counter-positions
    ADL { position: PositionId, counter_positions: Vec<PositionId> },
}
```

### 5.5 Native Action Types

```rust
/// All native (non-EVM) actions processed by torus-core.
/// Analogous to Hyperliquid's ~70 HyperCore action types.
#[derive(Serialize, Deserialize)]
pub enum NativeAction {
    // === Order Book ===
    PlaceOrder(PlaceOrderParams),
    CancelOrder { order_id: OrderId },
    CancelAllOrders { market_id: Option<MarketId> },
    ModifyOrder { order_id: OrderId, new_price: Option<Decimal>, new_qty: Option<Decimal> },

    // === Transfers ===
    TransferToPerp { amount: U256 },       // Spot -> Perp balance
    TransferToSpot { amount: U256 },       // Perp -> Spot balance
    Withdraw { amount: U256, to: Address }, // Withdraw TRS to EVM address

    // === Staking ===
    Delegate { validator: Address, amount: U256 },
    Undelegate { validator: Address, amount: U256 },
    PermanentStake { amount: U256 },       // Irreversible permanent lock
    ClaimRewards,

    // === Governance ===
    SubmitProposal(Proposal),
    Vote { proposal_id: u64, option: VoteOption },

    // === Validator ===
    RegisterValidator { pubkey: PublicKey, commission: u16 },
    UpdateCommission { new_rate: u16 },
    JailVote { target: Address },
    UnjailSelf,

    // === Admin (governance-gated) ===
    UpdateMarketParams { market_id: MarketId, params: MarketParams },
    ListMarket(MarketListing),
    DelistMarket { market_id: MarketId },
}
```

---

## 6. State Layer

### 6.1 RocksDB Column Family Layout

```
Column Family                    | Key Format               | Value Format
─────────────────────────────────┼──────────────────────────┼─────────────────────────
cf_accounts                      | Address (20 bytes)       | AccountInfo (borsh)
cf_storage                       | Address ++ Slot (52 B)   | U256 (32 bytes)
cf_code                          | CodeHash (32 bytes)      | Bytecode (raw bytes)
cf_block_headers                 | BlockNumber (8 B BE)     | TorusBlockHeader (borsh)
cf_block_bodies                  | BlockNumber (8 B BE)     | TorusBlockBody (borsh)
cf_block_hash_to_number          | BlockHash (32 bytes)     | BlockNumber (8 B BE)
cf_receipts                      | BlockNum ++ TxIdx (12 B) | Receipt (borsh)
cf_logs                          | BlockNum ++ LogIdx       | Log (borsh)
cf_logs_bloom                    | BlockNumber (8 B BE)     | Bloom (256 bytes)
cf_tx_hash_to_location           | TxHash (32 bytes)        | BlockNum ++ TxIdx
─────────────────────────────────┼──────────────────────────┼─────────────────────────
cf_native_orders                 | OrderId (16 bytes)       | Order (borsh)
cf_native_positions              | Address ++ MarketId      | Position (borsh)
cf_native_balances               | Address ++ AssetId       | Balance (borsh)
cf_native_order_books            | MarketId ++ Side ++ ...  | PriceLevel entries
cf_native_markets                | MarketId (8 bytes)       | MarketParams (borsh)
─────────────────────────────────┼──────────────────────────┼─────────────────────────
cf_staking_validators            | Address (20 bytes)       | ValidatorState (borsh)
cf_staking_delegations           | Delegator ++ Validator   | Delegation (borsh)
cf_staking_permanent             | Address (20 bytes)       | PermanentStake (borsh)
cf_staking_rewards               | Address (20 bytes)       | PendingRewards (borsh)
─────────────────────────────────┼──────────────────────────┼─────────────────────────
cf_governance_proposals          | ProposalId (8 B BE)      | Proposal (borsh)
cf_governance_votes              | ProposalId ++ Voter      | Vote (borsh)
cf_fee_config                    | "current" (static key)   | FeeConfig (borsh)
cf_treasury                      | "balance" (static key)   | U256 (32 bytes)
cf_dev_pool                      | DeployerAddress          | DevPoolEntry (borsh)
─────────────────────────────────┼──────────────────────────┼─────────────────────────
cf_consensus_meta                | Various keys             | hotstuff_rs internal state
cf_trie_nodes                    | NodeHash (32 bytes)      | MPT node (raw bytes)
cf_trie_accounts                 | HashedAddress (32 B)     | Account trie leaf
cf_trie_storage                  | HashedAddr ++ HashedSlot | Storage trie leaf
```

### 6.2 MPT State Root Computation

Using `reth-trie` (from reth v1.1.5) for Ethereum-compatible state root computation:

```rust
use reth_trie::StateRoot;
use reth_trie_common::HashedPostState;

/// Compute state root from revm BundleState.
/// Uses reth-trie's incremental update approach.
pub fn compute_state_root(
    tx: &rocksdb::Transaction,      // or our DB cursor wrapper
    bundle: &BundleState,
) -> Result<(B256, TrieUpdates)> {
    // 1. Convert revm BundleState to hashed post state
    let hashed_state = HashedPostState::from_bundle_state(&bundle.state);
    //    → accounts: B256Map<Option<Account>> (keccak256-hashed addresses)
    //    → storages: B256Map<HashedStorage>

    // 2. Construct prefix sets (which trie paths need updating)
    let prefix_sets = hashed_state.construct_prefix_sets();

    // 3. Write hashed state to DB (cf_trie_accounts, cf_trie_storage)
    write_hashed_state(tx, &hashed_state)?;

    // 4. Compute state root using DB trie cursors
    let (root, trie_updates) = StateRoot::from_tx(tx)
        .with_prefix_sets(prefix_sets)
        .root_with_updates()?;
    //    → root: B256 (new state root)
    //    → trie_updates: TrieUpdates (AccountsTrie + StoragesTrie node changes)

    // 5. Persist trie node updates
    write_trie_updates(tx, &trie_updates)?;

    Ok((root, trie_updates))
}
```

**Strategy:** Incremental updates via prefix sets. Only trie paths affected by the current block's state diffs are recomputed. Requires `HashedAccounts`, `HashedStorages`, `AccountsTrie`, and `StoragesTrie` column families (following reth's table layout).

### 6.3 Native State Root

The EVM state root (§6.2) only covers EVM accounts and storage. Native state (order books,
positions, balances, staking, governance) must also be committed to a verifiable root.

**Approach: Composite state root**

```rust
/// Final block state root = keccak256(evm_state_root || native_state_root)
pub fn compute_composite_state_root(
    evm_root: B256,
    native_root: B256,
) -> B256 {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(evm_root.as_slice());
    data[32..].copy_from_slice(native_root.as_slice());
    keccak256(&data)
}
```

The native state root is computed over a separate Merkle tree covering all native column
families (`cf_native_*`, `cf_staking_*`, `cf_governance_*`, `cf_fee_config`, `cf_treasury`,
`cf_dev_pool`). This tree uses the same reth-trie infrastructure with a dedicated set of
trie CFs (`cf_native_trie_nodes`, `cf_native_trie_accounts`).

This design allows EVM state proofs to work unchanged (standard `eth_getProof`) while also
providing verifiable proofs over native state via `torus_getProof`.

### 6.4 Pruning Strategy

Two node modes: **pruned** (default, validators and standard RPC) and **archive** (`--archive` flag,
for bridges, DeFi protocols, and block explorers that need historical state proofs).

**Pruned node (default):**

| Data Type | Retention | Rationale |
|---|---|---|
| Current state (accounts, storage) | Forever | Required for execution |
| Block headers | Forever | Required for chain verification |
| Block bodies | Last 1,000,000 blocks | Sync and replay |
| Receipts | Last 1,000,000 blocks | RPC query support |
| Trie nodes (historical) | Last 256 blocks | eth_getProof for recent blocks |
| Order book snapshots | Every 10,000 blocks | State recovery |

**Archive node (`--archive`):**

| Data Type | Retention | Rationale |
|---|---|---|
| All of the above | Forever | Full historical state |
| Trie nodes (all) | Forever | eth_getProof at any block height |

Pruning runs as a background task on pruned nodes, compacting RocksDB after deletion.
Archive nodes skip pruning entirely.

---

## 7. Network Layer

### 7.1 libp2p Protocol Design

Three logical protocols over libp2p:

#### 7.1.1 Consensus Messages (GossipSub)

```
Topic: /torus/consensus/1.0
Messages: Proposal, Vote, NewView, TimeoutCertificate
Encoding: Borsh (fixed-size, fast)
Validators: ONLY validators publish/subscribe
Validation: Verify sender is in current validator set
```

- **Message size:** ~1-10 KB (block proposals larger, votes small)
- **Latency target:** <50ms between validators
- **Flood publishing:** All validators see all messages (no mesh optimization needed at 21 validators)

#### 7.1.2 Transaction Gossip (GossipSub)

```
Topic: /torus/transactions/1.0
Messages: SignedTransaction (EVM), SignedNativeAction
Encoding: RLP (EVM txs), Borsh (native actions)
Publishers: Any node (validators + non-validators)
Validation: Signature check, basic format validation, nonce check
```

- **Deduplication:** Track seen tx hashes for 60 seconds
- **Rate limiting:** Per-peer rate limit (100 tx/sec default)
- **Size limit:** 128 KB per transaction message

#### 7.1.3 Block Sync (Request/Response)

```
Protocol: /torus/sync/1.0
Pattern: Request-response (not pub/sub)
Requests:
  - GetBlockHeaders { start: u64, count: u64 } -> Vec<BlockHeader>
  - GetBlockBodies { numbers: Vec<u64> } -> Vec<BlockBody>
  - GetState { root: B256, paths: Vec<Nibbles> } -> Vec<TrieNode>
Encoding: Borsh
```

- Used for initial sync (new node catching up) and fork recovery
- Parallel downloads from multiple peers
- Verify block hashes chain together before executing

### 7.2 Transport Configuration

```rust
let transport = libp2p::quic::tokio::Transport::new(quic_config);

let swarm = SwarmBuilder::with_new_identity()
    .with_tokio()
    .with_quic()
    .with_behaviour(|key| {
        let gossipsub = gossipsub::Behaviour::new(
            MessageAuthenticity::Signed(key.clone()),
            gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_millis(500))
                .max_transmit_size(256 * 1024)  // 256 KB
                .build()?,
        )?;

        let kademlia = kad::Behaviour::new(
            peer_id,
            kad::store::MemoryStore::new(peer_id),
        );

        let request_response = request_response::Behaviour::new(
            [(StreamProtocol::new("/torus/sync/1.0"), ProtocolSupport::Full)],
            Default::default(),
        );

        Ok(TorusBehaviour { gossipsub, kademlia, request_response })
    })?
    .build();
```

### 7.3 Peer Management

- **Bootstrap nodes:** Hardcoded list of seed nodes for initial peer discovery
- **Kademlia DHT:** Peer discovery after bootstrap
- **Connection limits:** Max 50 peers (validators prioritized)
- **Peer scoring:** Track message delivery rate, penalize spam, reward reliable peers
- **Ban list:** Peers sending invalid messages get temporarily banned (exponential backoff)

---

## 8. RPC Layer

### 8.1 Minimum Viable eth_* Methods

Required for MetaMask and Foundry compatibility:

| Method | Priority | Description |
|---|---|---|
| `eth_chainId` | P0 | Return chain ID (7777/7778) |
| `eth_blockNumber` | P0 | Return latest block number |
| `eth_getBalance` | P0 | Account balance |
| `eth_getTransactionCount` | P0 | Account nonce |
| `eth_getCode` | P0 | Contract bytecode |
| `eth_getStorageAt` | P0 | Storage slot value |
| `eth_call` | P0 | Simulate transaction (no state change) |
| `eth_estimateGas` | P0 | Estimate gas for transaction |
| `eth_sendRawTransaction` | P0 | Submit signed transaction |
| `eth_getBlockByNumber` | P0 | Block by number (with/without tx bodies) |
| `eth_getBlockByHash` | P0 | Block by hash |
| `eth_getTransactionByHash` | P0 | Transaction by hash |
| `eth_getTransactionReceipt` | P0 | Receipt by tx hash |
| `eth_getLogs` | P0 | Filter logs by block range, address, topics |
| `eth_gasPrice` | P0 | Current gas price suggestion |
| `eth_maxPriorityFeePerGas` | P0 | EIP-1559 priority fee suggestion (required by MetaMask) |
| `eth_feeHistory` | P1 | EIP-1559 fee history |
| `eth_getBlockTransactionCountByNumber` | P1 | Tx count in block |
| `eth_getTransactionByBlockNumberAndIndex` | P1 | Tx by position |
| `net_version` | P0 | Network version (chain ID as string) |
| `net_listening` | P1 | Always true |
| `web3_clientVersion` | P1 | "Torus/v0.1.0" |

### 8.2 WebSocket Subscriptions

| Subscription | Description |
|---|---|
| `eth_subscribe("newHeads")` | New block headers |
| `eth_subscribe("logs", filter)` | Filtered event logs |
| `eth_subscribe("newPendingTransactions")` | Mempool transactions |

### 8.3 Torus-Specific RPC Namespace

```
torus_getOrderBook(market_id) -> { bids, asks, spread }
torus_getPosition(address, market_id) -> Position
torus_getBalances(address) -> { spot, perp, permanent_stake }
torus_getMarkets() -> Vec<MarketInfo>
torus_getValidators() -> Vec<ValidatorInfo>
torus_getEpoch() -> EpochInfo
torus_getStakingInfo(address) -> StakingInfo
torus_submitNativeAction(signed_action) -> TxHash
torus_getTradeHistory(address, market_id, limit) -> Vec<Trade>
torus_getGovernanceProposals(status_filter) -> Vec<Proposal>
```

### 8.4 jsonrpsee Server Setup

```rust
use jsonrpsee::server::ServerBuilder;

let server = ServerBuilder::default()
    .max_connections(1000)
    .max_request_body_size(10 * 1024 * 1024)  // 10 MB
    .build("0.0.0.0:8545")
    .await?;

// Register method handlers
let mut module = RpcModule::new(state);
module.register_async_method("eth_blockNumber", |_, ctx| async move { ... })?;
module.register_async_method("eth_getBalance", |params, ctx| async move { ... })?;
// ... etc

let handle = server.start(module);
```

---

## 9. Consensus-Execution Bridge

This is the most critical and complex component (~8,000-15,000 lines estimated). It connects hotstuff_rs consensus decisions to torus-core + revm execution.

### 9.1 Block Proposal Construction

```rust
impl BlockProposer {
    /// Called by TorusApp::produce_block() on the consensus leader.
    pub fn build_block(
        &self,
        parent: &TorusBlockHeader,
        mempool: &Mempool,
        timestamp: u64,
        proposer: Address,
    ) -> TorusBlock {
        // 1. Drain native actions from mempool (priority ordered)
        let native_actions = mempool.drain_native(MAX_NATIVE_ACTIONS_PER_BLOCK);

        // 2. Drain EVM transactions (gas-price ordered, respecting gas limit)
        let evm_txs = mempool.drain_evm(parent.evm_gas_limit);

        // 3. Pull queued CoreWriter actions from previous block
        let core_writer_actions = self.core_writer_queue.drain();

        // 4. Construct block
        TorusBlock {
            header: TorusBlockHeader {
                timestamp,
                proposer,
                // state_root computed during validation
                state_root: B256::ZERO,
                ..Default::default()
            },
            // Note: height and parent_hash come from hotstuff_rs Block,
            // not from TorusBlockHeader (see §3.2)
            native_actions,
            evm_transactions: evm_txs,
            core_writer_actions,
        }
    }
}
```

### 9.2 Block Validation Pipeline

```rust
impl BlockValidator {
    /// Called by TorusApp::validate_block() on ALL validators.
    /// Returns state root if valid, None if invalid.
    pub fn validate_block(
        &mut self,
        block: &TorusBlock,
        parent_state: &StateDb,
    ) -> Option<B256> {
        // Create a state overlay (don't mutate base state yet)
        let mut overlay = StateOverlay::new(parent_state);

        // === Phase 1: Native execution ===
        let core_result = self.core_engine.execute_actions(
            &block.native_actions,
            &mut overlay,
        )?;

        // === Phase 2: EVM execution ===
        let evm_result = self.evm_executor.execute_block(
            &block.evm_transactions,
            &mut overlay,
        )?;

        // === Phase 3: EVM-to-Core transfers (lockbox) ===
        let lockbox_result = self.process_lockbox_transfers(
            &evm_result,
            &mut overlay,
        )?;

        // === Phase 4: CoreWriter actions (from previous block's EVM) ===
        let cw_result = self.core_engine.execute_actions(
            &block.core_writer_actions,
            &mut overlay,
        )?;

        // === Phase 5: Fee distribution ===
        let total_fees = core_result.fees + evm_result.fees;
        let fee_dist = self.fee_splitter.split(
            total_fees,
            block.header.epoch,
            block.header.proposer,
        );
        self.apply_fee_distribution(&fee_dist, &mut overlay)?;

        // === Phase 6: Staking rewards (if epoch boundary) ===
        if self.is_epoch_boundary(block.header.height) {
            self.distribute_staking_rewards(&mut overlay)?;
        }

        // === Phase 7: Compute state root ===
        let state_root = overlay.compute_state_root()?;

        Some(state_root)
    }
}
```

### 9.3 Block Commit Pipeline

```rust
impl BlockCommitter {
    /// Called by TorusApp::commit() after consensus finality.
    /// This is irreversible — state is persisted to RocksDB.
    pub fn commit_block(
        &mut self,
        block: &TorusBlock,
        validated: &ValidatedBlock,
    ) -> Result<()> {
        // 1. Write all state diffs to RocksDB (atomic batch)
        let mut batch = rocksdb::WriteBatch::default();
        for diff in &validated.state_diffs {
            self.apply_diff_to_batch(&mut batch, diff);
        }

        // 2. Write block header and body
        batch.put_cf(cf_block_headers, block.header.height.to_be_bytes(), &block.header.encode());
        batch.put_cf(cf_block_bodies, block.header.height.to_be_bytes(), &block.body.encode());

        // 3. Write receipts and logs
        for (i, receipt) in validated.receipts.iter().enumerate() {
            let key = encode_receipt_key(block.header.height, i as u32);
            batch.put_cf(cf_receipts, key, &receipt.encode());
        }

        // 4. Write tx hash -> location index
        for (i, tx) in block.evm_transactions.iter().enumerate() {
            let tx_hash = tx.hash();
            batch.put_cf(cf_tx_hash_to_location, tx_hash, &encode_location(block.header.height, i as u32));
        }

        // 5. Atomic write
        self.db.write(batch)?;

        // 6. Update in-memory caches
        self.update_block_hash_cache(block);

        // 7. Notify RPC subscribers
        self.notify_new_head(&block.header);
        self.notify_logs(&validated.logs);

        Ok(())
    }
}
```

---

## 10. Cross-VM Interaction

### 10.1 Precompile Address Map

Following Hyperliquid's pattern with Torus-specific addresses:

```
Address Range          | Purpose                  | Direction
───────────────────────┼──────────────────────────┼──────────
0x0000...0000_0800     | Read: Order book state   | EVM → Core (read)
0x0000...0000_0801     | Read: Positions           | EVM → Core (read)
0x0000...0000_0802     | Read: Balances            | EVM → Core (read)
0x0000...0000_0803     | Read: Oracle prices       | EVM → Core (read)
0x0000...0000_0804     | Read: Staking info        | EVM → Core (read)
0x0000...0000_0805     | Read: Governance state    | EVM → Core (read)
0x0000...0000_0806     | Read: Core block number   | EVM → Core (read)
0x3333...3333_3333     | CoreWriter (write actions) | EVM → Core (write, delayed)
0x2000...{token_idx}   | Lockbox (asset transfer)  | EVM ↔ Core (bidirectional)
```

### 10.2 Read Precompiles

Read precompiles allow EVM contracts to query native state:

```rust
/// Example: Read order book precompile at 0x0800
impl StatefulPrecompile for OrderBookPrecompile {
    fn call(
        &self,
        input: &Bytes,     // ABI-encoded: (market_id, depth)
        gas_limit: u64,
        context: &PrecompileContext,
    ) -> PrecompileResult {
        // Decode ABI input
        let (market_id, depth) = abi_decode(input)?;

        // Read from native state (always one block behind)
        let snapshot = self.core_state.snapshot();
        let book = snapshot.get_order_book(market_id)?;

        // Encode response as ABI
        let (bids, asks) = book.top_levels(depth);
        let output = abi_encode(&(bids, asks));

        Ok(PrecompileOutput { output, gas_used: 3000 })
    }
}
```

### 10.3 CoreWriter Pattern

The CoreWriter is a system contract that queues write actions from EVM to native state. Actions are **delayed by one block** to prevent frontrunning.

```solidity
// CoreWriter system contract at 0x3333...3333
interface ICoreWriter {
    /// Place a native order from EVM
    function placeOrder(
        uint64 marketId,
        bool isBuy,
        uint256 price,
        uint256 quantity,
        uint8 orderType,
        uint8 timeInForce
    ) external;

    /// Cancel a native order from EVM
    function cancelOrder(uint128 orderId) external;

    /// Delegate stake from EVM
    function delegate(address validator, uint256 amount) external;

    /// Permanently stake from EVM
    function permanentStake(uint256 amount) external;
}
```

**Processing flow:**
1. EVM contract calls `CoreWriter.placeOrder(...)` in block N
2. Bridge records the action in `core_writer_queue`
3. In block N+1, the queued action is executed by `torus-core`
4. Result is visible to EVM in block N+2

### 10.4 Lockbox Mechanism

Direct asset transfer between EVM and native layers without wrapped tokens:

```rust
/// Lockbox precompile: transfer TRS between EVM balance and native balance.
/// Each native token has a lockbox at 0x2000...{token_index}.
impl StatefulPrecompile for LockboxPrecompile {
    fn call(&self, input: &Bytes, gas_limit: u64, ctx: &PrecompileContext) -> PrecompileResult {
        let (direction, amount) = abi_decode(input)?;
        match direction {
            Direction::EvmToNative => {
                // Debit EVM balance, credit native spot balance
                // Atomic within the same block execution
            }
            Direction::NativeToEvm => {
                // Debit native spot balance, credit EVM balance
                // Only from CoreWriter (delayed by 1 block)
            }
        }
    }
}
```

---

## 11. Economic Modules

### 11.1 Permanent Staking State Machine

Permanent staking is **separate from validator delegation**. Any address can lock TRS tokens
from their liquid balance forever. No validator selection required. Permanently staked tokens
earn inflationary rewards and contribute to the total staked supply for validator selection
weight calculations (boosting overall network security).

```
    Liquid Balance ──PermanentStake──► Permanently Staked
                                       ┌──────────────────────────┐
                                       │ Cannot unlock (forever)   │
                                       │ 5% APY (inflationary mint)│
                                       │ 1.5x governance weight    │
                                       │ Counts toward total stake  │
                                       │ Tracked per-address        │
              (impossible) ◄───────────│                            │
                                       └──────────────────────────┘

    Liquid Balance ──Delegate──► Delegated (to validator)
        ▲                            │
        │ Undelegate (7-day queue)   │ Earns share of validator fee income
        └────────────────────────────┘
```

Note: These are two independent mechanisms. A user can permanently stake AND delegate
separately — the tokens used for each come from the liquid balance independently.

```rust
pub struct PermanentStaking {
    /// Total permanently staked TRS across all addresses
    pub total_permanent_stake: U256,
}

impl PermanentStaking {
    /// Lock tokens permanently from liquid balance. Irreversible.
    /// Separate from validator delegation — no validator field needed.
    pub fn permanent_stake(
        &mut self,
        state: &mut StateDb,
        staker: Address,
        amount: U256,
    ) -> Result<()> {
        // 1. Verify staker has sufficient liquid (non-delegated) balance
        // 2. Debit liquid balance, credit permanent stake record
        // 3. Update total_permanent_stake
        // 4. Emit PermanentStakeEvent
    }

    /// Distribute 5% annual rewards to permanent stakers.
    /// Called at epoch boundaries. Rewards are inflationary (chain mints new TRS).
    pub fn distribute_rewards(
        &self,
        state: &mut StateDb,
        blocks_in_epoch: u64,
    ) -> Result<U256> {
        // Annual rate: 500 bps (5%)
        // Per-epoch rate: 500 * blocks_in_epoch / (blocks_per_year * 10000)
        // Mint new TRS proportional to each staker's permanent balance
        // Does NOT auto-compound (rewards go to liquid balance)
    }
}
```

### 11.2 Delegator Reward Distribution

Standard DPoS delegation: delegators earn a share of validator fee income.

```rust
pub struct DelegatorRewards;

impl DelegatorRewards {
    /// Distribute the validator's fee share to delegators.
    /// Called after fee split assigns the validator portion.
    pub fn distribute(
        state: &mut StateDb,
        validator: Address,
        validator_fee_share: U256,
    ) -> Result<()> {
        let commission_bps = state.get_validator_commission(validator);
        let commission = validator_fee_share * commission_bps / 10000;
        let delegator_pool = validator_fee_share - commission;

        // Validator keeps commission
        state.credit_balance(validator, commission);

        // Remaining pool distributed pro-rata to delegators by stake weight
        for (delegator, stake) in state.get_delegations(validator) {
            let share = delegator_pool * stake / state.get_total_delegation(validator);
            state.credit_balance(delegator, share);
        }
    }
}
```

Delegator rewards come entirely from the validator's fee split share — no additional
inflation. This is separate from permanent staking rewards (which are inflationary).

### 11.3 Fee Split Logic

Porting the proven `x/fees` design from torus-chain:

```rust
pub struct FeeSplitter {
    /// Split ratios evolve linearly over 1825 epochs (~5 years)
    /// Start: 10% burn / 0% validator / 45% treasury / 45% dev-pool
    /// End:   25% burn / 25% validator / 25% treasury / 25% dev-pool
    pub start_ratios: FeeRatios,
    pub end_ratios: FeeRatios,
    pub transition_epochs: u64,  // 1825
}

#[derive(Clone)]
pub struct FeeRatios {
    pub burn_bps: u16,       // basis points (10000 = 100%)
    pub validator_bps: u16,
    pub treasury_bps: u16,
    pub dev_pool_bps: u16,   // gets remainder (no rounding loss)
}

impl FeeSplitter {
    pub fn split(&self, total_fees: U256, epoch: u64, proposer: Address) -> FeeDistribution {
        // IMPORTANT: No floating-point arithmetic — all integer basis-point math
        // for cross-validator determinism.
        let clamped_epoch = min(epoch, self.transition_epochs);

        // Integer linear interpolation: start + (end - start) * clamped / total
        // All values are u16 basis points, intermediate math uses u64 to avoid overflow
        let burn_bps = lerp_bps(self.start_ratios.burn_bps, self.end_ratios.burn_bps,
                                clamped_epoch, self.transition_epochs);
        let validator_bps = lerp_bps(self.start_ratios.validator_bps, self.end_ratios.validator_bps,
                                     clamped_epoch, self.transition_epochs);
        let treasury_bps = lerp_bps(self.start_ratios.treasury_bps, self.end_ratios.treasury_bps,
                                    clamped_epoch, self.transition_epochs);

        let burn = total_fees * U256::from(burn_bps) / U256::from(10000u16);
        let validator = total_fees * U256::from(validator_bps) / U256::from(10000u16);
        let treasury = total_fees * U256::from(treasury_bps) / U256::from(10000u16);
        let dev_pool = total_fees - burn - validator - treasury; // remainder, no rounding loss

        FeeDistribution { burn, validator: (proposer, validator), treasury, dev_pool }
    }
}

/// Integer-only linear interpolation in basis points.
/// Returns: start + (end - start) * numerator / denominator
/// Handles both increasing and decreasing interpolation.
fn lerp_bps(start: u16, end: u16, numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 { return end; }
    if start <= end {
        let delta = (end - start) as u64;
        start + ((delta * numerator) / denominator) as u16
    } else {
        let delta = (start - end) as u64;
        start - ((delta * numerator) / denominator) as u16
    }
}
```

The validator fee share is distributed to the proposing validator, who then splits
it with their delegators via the DelegatorRewards mechanism (see §11.2).

### 11.4 Governance Weight Calculation

Voting power is computed automatically by the chain — voters cannot override their weight.

```rust
pub struct GovernanceWeight;

impl GovernanceWeight {
    /// Calculate voting power for an address.
    /// Permanently staked tokens get 1.5x multiplier.
    /// Standard delegated tokens get 1x weight.
    /// This is computed by the chain, not user-supplied.
    pub fn voting_power(
        state: &StateDb,
        voter: Address,
    ) -> U256 {
        let standard_stake = state.get_delegation(voter);
        let permanent_stake = state.get_permanent_stake(voter);

        // Standard delegated tokens: 1x weight
        // Permanently staked tokens: 1.5x weight (integer: * 3 / 2, rounds down)
        let weight = standard_stake + (permanent_stake * U256::from(3u64) / U256::from(2u64));

        weight
    }
}
```

---

## 12. Genesis Format

```json
{
    "chain_id": 7777,
    "chain_name": "torus-mainnet",
    "timestamp": 1700000000,
    "consensus": {
        "protocol": "hotstuff",
        "epoch_length": 100000,
        "timeout_base_ms": 500,
        "timeout_max_ms": 30000
    },
    "evm": {
        "spec_id": "cancun",
        "gas_limit": 30000000,
        "base_fee_initial": 1000000000,
        "chain_id": 7777
    },
    "economics": {
        "fee_split": {
            "start": { "burn": 1000, "validator": 0, "treasury": 4500, "dev_pool": 4500 },
            "end": { "burn": 2500, "validator": 2500, "treasury": 2500, "dev_pool": 2500 },
            "transition_epochs": 1825
        },
        "permanent_staking": {
            "annual_rate_bps": 500,
            "governance_multiplier_bps": 15000
        },
        "validator": {
            "min_self_delegation": "10000000000000000000000",
            "max_validators": 21,
            "unbonding_period_blocks": 604800,
            "max_commission_bps": 5000,
            "max_commission_change_bps": 100
        }
    },
    "validators": [
        {
            "address": "0x...",
            "pubkey": "0x...",
            "stake": "1000000000000000000000000",
            "commission_bps": 500
        }
    ],
    "accounts": [
        {
            "address": "0x...",
            "balance": "1000000000000000000000000000",
            "note": "treasury"
        },
        {
            "address": "0x...",
            "balance": "500000000000000000000000000",
            "note": "dev_pool"
        }
    ],
    "markets": [
        {
            "market_id": 0,
            "base_asset": "TRS",
            "quote_asset": "USDC",
            "tick_size": "0.01",
            "lot_size": "0.1",
            "max_leverage": 20,
            "maintenance_margin_bps": 500
        }
    ],
    "evm_alloc": {
        "0x...": {
            "balance": "0x...",
            "code": "0x...",
            "storage": {}
        }
    },
    "precompiles": {
        "order_book_reader": "0x0000000000000000000000000000000000000800",
        "position_reader": "0x0000000000000000000000000000000000000801",
        "balance_reader": "0x0000000000000000000000000000000000000802",
        "oracle_reader": "0x0000000000000000000000000000000000000803",
        "staking_reader": "0x0000000000000000000000000000000000000804",
        "governance_reader": "0x0000000000000000000000000000000000000805",
        "core_block_reader": "0x0000000000000000000000000000000000000806",
        "core_writer": "0x3333333333333333333333333333333333333333",
        "lockbox_base": "0x2000000000000000000000000000000000000000"
    }
}
```

---

## 13. Dependency Version Matrix

Pin exact versions following Hyperliquid's approach. Avoid floating semver.

### Core Dependencies

| Crate | Version | Purpose | License | Notes |
|---|---|---|---|---|
| `hotstuff_rs` | `=0.4.0` | BFT consensus | Apache-2.0 | crates.io, ParallelChain |
| `revm` | `=19.2.0` | EVM executor | MIT | Hyperliquid's pinned version |
| `alloy-primitives` | `=0.9.2` | Ethereum types | Apache-2.0/MIT | Matches Hyperliquid |
| `alloy-consensus` | `=0.9.2` | Block/tx types | Apache-2.0/MIT | |
| `alloy-eips` | `=0.9.2` | EIP implementations | Apache-2.0/MIT | |
| `alloy-rlp` | `=0.3.11` | RLP encoding | Apache-2.0/MIT | |
| `reth-trie` | `=1.1.5` | MPT state root | Apache-2.0/MIT | Matches Hyperliquid's reth-primitives |
| `reth-primitives` | `=1.1.5` | Reth shared types | Apache-2.0/MIT | |

### Storage

| Crate | Version | Purpose | License |
|---|---|---|---|
| `rocksdb` | `=0.22.0` | State storage | Apache-2.0 |

### Networking

| Crate | Version | Purpose | License |
|---|---|---|---|
| `libp2p` | `=0.56.0` | P2P networking (umbrella) | MIT |
| `libp2p-gossipsub` | `=0.49.x` | Pub/sub messaging | MIT |
| `libp2p-kad` | (workspace) | Peer discovery (DHT) | MIT |
| `libp2p-quic` | `=0.13.x` | QUIC transport | MIT |
| `libp2p-request-response` | (workspace) | Block sync req/resp | MIT |
| `libp2p-connection-limits` | `=0.6.0` | Connection management | MIT |

Features: `tokio, tcp, noise, yamux, quic, gossipsub, kad, request-response, cbor, identify, connection-limits, allow-block-list, macros, dns`

### RPC

| Crate | Version | Purpose | License |
|---|---|---|---|
| `jsonrpsee` | `=0.26.0` | JSON-RPC server | MIT |
| `tower` | `=0.5.2` | Middleware (rate limiting) | MIT |
| `tower-http` | `=0.6.2` | CORS layer | MIT |

Features: `server, client, macros`

### Serialization and Runtime

| Crate | Version | Purpose | License |
|---|---|---|---|
| `serde` | `=1.0.217` | Serialization framework | Apache-2.0/MIT |
| `serde_json` | `=1.0.135` | JSON serialization | Apache-2.0/MIT |
| `borsh` | `=1.5.3` | Binary serialization (consensus) | Apache-2.0/MIT |
| `tokio` | `=1.42.0` | Async runtime | MIT |
| `tracing` | `=0.1.41` | Structured logging | MIT |
| `clap` | `=4.5.23` | CLI argument parsing | Apache-2.0/MIT |

### Crypto

| Crate | Version | Purpose | License |
|---|---|---|---|
| `k256` | `=0.13.4` | secp256k1 (Ethereum signing) | Apache-2.0/MIT |
| `sha3` | `=0.10.8` | Keccak-256 hashing | Apache-2.0/MIT |
| `ed25519-dalek` | `=2.1.1` | Ed25519 (validator signing) | BSD-3 |

### Testing

| Crate | Version | Purpose | License |
|---|---|---|---|
| `proptest` | `=1.5.0` | Property-based testing | Apache-2.0/MIT |
| `criterion` | `=0.5.1` | Benchmarking | Apache-2.0/MIT |
| `tempfile` | `=3.14.0` | Test state directories | Apache-2.0/MIT |

---

## Appendix A: Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Serialization (consensus) | Borsh | Fixed-size, fast, deterministic. Used by Solana, Near. |
| Serialization (RPC) | JSON | Ethereum compatibility |
| Serialization (state snapshots) | Borsh | Compact, fast recovery |
| EVM spec level | Cancun (no EIP-4844) | Same as Hyperliquid. Blobs not needed on L1. |
| Signing algorithm (validators) | Ed25519 | Fast verification, used by hotstuff_rs |
| Signing algorithm (EVM txs) | secp256k1 (ECDSA) | Ethereum compatibility |
| Block size limit | 2 MB (native) + 30M gas (EVM) | Balance throughput vs. propagation |
| State snapshot interval | Every 10,000 blocks | Fast node bootstrap |
| Chain ID | 7777 (mainnet), 7778 (testnet) | Unique, not conflicting with existing chains |

## Appendix B: Hyperliquid Reference Architecture

Key patterns extracted from `hyper-evm-sync` and reverse engineering:

| Component | Hyperliquid's Choice | Torus Equivalent |
|---|---|---|
| revm version | `=19.2.0` | Match exactly |
| alloy version | `0.9.2` | Match exactly |
| reth-primitives | `v1.1.5` | Match exactly |
| State serialization | MessagePack (rmp-serde) | Borsh (faster, smaller) |
| Block execution order | Core → EVM → Transfers → CoreWriter | Same |
| Precompile addresses | `0x0800` read, `0x3333` write, `0x2000` lockbox | Same pattern |
| EVM spec | Cancun (no 4844) | Same |
| Dual-block model | 1s small + 60s large | Single block type initially |
| State DB | RocksDB (inferred) | RocksDB (explicit) |
