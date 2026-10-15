# Configuration Reference

## CLI Flags

### Node Operation

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--genesis <path>` | PathBuf | none | Path to genesis.json. Required on first run to initialize the database. Ignored if the database already contains accounts. |
| `--data-dir <path>` | PathBuf | `./data` | RocksDB data directory. Created if it doesn't exist. |
| `--log-level <level>` | String | `info` | Log verbosity. One of: `trace`, `debug`, `info`, `warn`, `error`. Sets the `RUST_LOG` environment variable. |

### Identity

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--keystore <path>` | PathBuf | none | Path to encrypted keystore file containing the Ed25519 validator key. |
| `--passphrase-file <path>` | PathBuf | none | Path to a file containing the keystore passphrase (for unattended startup). File should have restrictive permissions. |
| `--validator-key <hex>` | String | none | **DEPRECATED**. Raw Ed25519 signing key as 64-character hex. Visible in process listings. Use `--keystore` instead. |

### Network

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--p2p-listen <multiaddr>` | String | `/ip4/0.0.0.0/udp/30333/quic-v1` | libp2p listen multiaddress. Uses QUIC transport over UDP. |
| `--p2p-peers <addrs>` | String | none | Comma-separated bootstrap peer multiaddresses. Each must include a `/p2p/<peer_id>` suffix. |
| `--rpc-addr <addr>` | String | `0.0.0.0:8545` | JSON-RPC HTTP + WebSocket listen address. |

### State Management

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--restore-from-snapshot <path>` | PathBuf | none | Restore database from a snapshot directory before starting. The snapshot must have been created by the node's snapshot system. |
| `--archive` | bool | false | Explicitly enable archive mode. Retains all historical data. Cannot be combined with `--retention-blocks`. |
| `--retention-blocks <N>` | u64 | none | Enable pruning: keep the last N blocks of historical block bodies and receipts. Everything older is deleted to save disk space. Cannot be combined with `--archive`. |
| `--trade-retention-blocks <N>` | u64 | none | Trade-history retention (D13): keep only the trade rows (`cf_native_trades`, `cf_native_user_trades`) of the last N blocks. Unset = keep all trade history. Independent of `--retention-blocks`; never prunes at or above the durable applied height. Config file key `trade-retention-blocks`. Cannot be combined with `--archive`. Node-local (no consensus or state-hash effect). See `pruning-guide.md`. |

**Default behavior**: If neither `--archive` nor a retention flag is specified, the node runs in archive mode (no data is ever pruned).

### Subcommands

| Command | Description |
|---------|-------------|
| `keygen --output <path>` | Generate a new Ed25519 keypair and write an encrypted keystore file. Default output: `./validator.keystore`. |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Overridden by `--log-level`. Controls tracing filter. Examples: `info`, `torus_consensus=debug,info`, `trace`. |
| `TORUS_RPC_MAX_SCAN_ROWS` | The maximum number of rows one public read RPC may read before it stops with `-32005` (R24). Default `250000`. An unset, empty, unparsable or `0` value uses the default. Read once at startup. Applies to `torus_getOpenInterest`, `torus_getUserLimits`, `torus_getOpenOrders`, `torus_getOrderByCloid`, `torus_getOrderBook`, `torus_getMarkPrice`, `torus_getBalances`, `torus_getLiquidatorVault`, `torus_getPosition` (cross), `torus_getLeader`, `torus_getProposal`, `torus_getProposals`, `torus_getUserTrades`, `torus_getTradeHistory`, `torus_getTradeHistoryRange`, `torus_getMarkets` and `torus_getBlockTrades`. Classic book blobs, packed user-trade rows, market trade rows and proposal rows count one row per started 128 bytes. Under the limit every method answers as before. Lower it on public RPC nodes. See `docs/api/read-limits.md` (every read method and what bounds it). |
| `TORUS_ALL_MARKET_TRADES` | Allows all-markets `newTrades` WebSocket subscriptions (no `marketId`). Unset: allowed only on `--rpc-only` nodes, rejected with `-32602` on validators. `1`: allowed. `0`: rejected. Any other value logs a warning and uses the default. Read once at startup. Per-market `newTrades` and `userFills` are always allowed. See `docs/api/streams.md`. |
| `TORUS_CHECKPOINT_INTERVAL_MS` | Item 6 Phase 3 coalesced state checkpoints: execution time (milliseconds) between checkpoints; executed state reaches disk once per interval and at every 100th block, barrier and shutdown, not every block. Unset or empty: the role default, 15000 (15 s) on validators, 0 on `--rpc-only` nodes (step 6). `0`: per-block flush. Not a whole number: a warning and the per-block flush. With a deferred book mode (`TORUS_BOOK_ROWS=2` or `3`) an explicit value above 0 refuses to start, and the unset default is off (per-block flush, a warning). Needs the exec pipeline (`TORUS_EXEC_PIPELINE` not `0`). Node-local. Validator RPC reads other than the submit checks lag by up to one interval (`docs/validator-home-network.md`). Read once at startup. |
| `TORUS_CHECKPOINT_MAX_BYTES` | Item 6 Phase 3: also checkpoint when the since-checkpoint layer reaches this many bytes. Unset, `0` or not a number (warned): no byte cap. Only with checkpoints on. |
| `TORUS_EPOCH_FAST_PATH` | s124 epoch-boundary fast path, opt-in. By default every epoch-boundary block (height a multiple of the epoch length) first drains the exec pipeline (a forced checkpoint of everything not yet on disk, then a wait for the flush worker) and then runs and writes its state on the execution thread. With this on, a boundary block with no EVM tx and no buffered slash skips that drain and takes the pipelined path like any other block. With checkpoints on (`TORUS_CHECKPOINT_INTERVAL_MS`) such a boundary is also checkpointed at its own height, so the next epoch's validator-set plan it computes reaches disk without waiting for later blocks (consensus reads that plan only from disk). A boundary with an EVM tx or a slash still drains first. State, roots and the running state hash are identical either way; only timing changes. `1` or `true`: on. Unset, empty, `0` or `false`: off. Any other value: off, with a warning. Needs the exec pipeline (`TORUS_EXEC_PIPELINE` not `0`); without it the setting has no effect (warned at startup). Node-local. Read once at startup; when on, the node logs `epoch fast path ON` at startup and boundary blocks log `pipelined=true` in `executing finalized block`. |
| `TORUS_ADMISSION_EXEC_AWARE` | Exec-aware native admission, opt-in. RPC ingress sheds non-cancel native actions with a retryable "busy" before signature checks once the pooled normal actions reach the admission limit (recent 10 s commit rate x `TORUS_ADMISSION_HORIZON_MS`, default 20000, never below 4 full blocks). With this on, the normal actions of committed blocks that were handed to execution but have not executed yet (they left the pool at dispatch; up to 64 queued blocks plus parked ones) count toward that limit too, so ingress also sheds while execution lags. Cancels and oracle submissions are never counted and never shed by this check. `1` or `true`: on. Unset, empty, `0` or `false`: off (admission exactly as before). Any other value: off, with a warning. No effect when `TORUS_ADMISSION_HORIZON_MS=0` (admission limit off). Node-local. Read once at startup. Gauge: `torus_mempool_native_committed_unexecuted` (always 0 when off). |

## Port Assignments

| Port | Protocol | Service | Configurable Via |
|------|----------|---------|-----------------|
| 8545 | TCP | JSON-RPC | `--rpc-addr` |
| 30333 | UDP | P2P (QUIC) | `--p2p-listen` |
| 9090 | TCP | Telemetry (Prometheus) | Hardcoded |

## ChainConfig Fields

Set via genesis.json. Cannot be changed after genesis initialization, except the two EVM block
values, which governance can change (see "EVM blocks" below).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `chain_id` | u64 | 7777 | EVM chain ID |
| `chain_name` | string | `"torus"` | Human-readable chain name |
| `evm_gas_limit` | u64 | 3,000,000 | EVM gas budget per EVM block: the genesis header's `evm_gas_limit`. Genesis key `evm.gas_limit` (required). |
| `evm_block_interval` | u64 | 11 | N: EVM transactions only in blocks with `height % N == 0`. The genesis header's `evm_block_interval`. Genesis key `evm.block_interval` (absent = 11; 0 is refused; 1 = EVM in every block). |
| `base_fee_per_gas` | u64 | 1,000,000,000 | Initial base fee (1 gwei) |
| `epoch_length` | u64 | 100,000 | Blocks per validator epoch |
| `max_validators` | u32 | 21 | Maximum active validator set size |
| `min_stake` | U256 | 10,000 TRS | Minimum self-delegation to register |
| `fee_burn_bps` | u16 | 1000 | Fee burn percentage (basis points) |
| `fee_validator_bps` | u16 | 0 | Fee to block proposer (basis points) |
| `fee_treasury_bps` | u16 | 4500 | Fee to treasury (basis points) |
| `fee_dev_pool_bps` | u16 | 4500 | Fee to dev pool (basis points) |

## EVM blocks (item 7 step 1)

The block header carries `evm_gas_limit` (the EVM gas budget of an EVM block) and
`evm_block_interval` (N). Both start at the genesis values above. Every shipped genesis file uses
3M / 11; the CI Uniswap genesis (`devnet/uniswap/make-ci-genesis.py`) uses a 10M budget, because
the Router deploy needs about 4.2M gas, and keeps N = 11. The bench genesis generators
(`testnet/gen-weighted-genesis.sh`, `devnet/wsl/gen-3val-genesis.sh`) copy the EVM section of
`testnet/genesis-weighted-base.json`, so bench cells run at 3M / 11 too (an untracked
`testnet/genesis-weighted-full.json` generated before the change keeps N = 5:
`gen-3val-genesis.sh` reuses it until it is rebuilt with `FORCE=1`).

N = 11 is the s123 owner value for benches and testing (it was 5). It is prime, so it shares no
factor with any validator count below 11: with round-robin leaders (one view per height), every
validator leads some EVM heights. At 3.2 blocks/s an EVM block comes every ~3.4 s, which at 3M
gas is ~0.9M EVM gas/s. Retune before testnet.

A header must carry its parent's values, except at an epoch boundary, where it carries the epoch
plan's values (unset in the plan = the parent's). Validators check this before voting and at
block insertion. Execution of boundary H copies the governance values into the plan for H + L
(L = `epoch_length`), so a change reaches the header between L and 2L blocks after it executes.

| Governance key (`ParameterChange`) | Range | Header field |
|---|---|---|
| `evm_block_gas_limit` | 1,000,000..=10,000,000 | `evm_gas_limit` |
| `evm_block_interval` | 2..=100 | `evm_block_interval` |

Values outside the range are refused at submission and at execution. A stored value that is
absent or invalid leaves the chain's value unchanged.

A node fills an EVM block, and admits a transaction, only up to `min(evm_gas_limit,
TORUS_EVM_BLOCK_GAS_BUDGET)`; the budget is node-local and defaults to 5,000,000. So an
`evm_block_gas_limit` above a node's budget changes neither that node's blocks nor its admission
cap, while `eth_estimateGas` still searches up to the header limit and can quote gas that the
same node's admission refuses. Raise `TORUS_EVM_BLOCK_GAS_BUDGET` on every node together with
such a governance change.

Per block, checked before voting (a block that breaks one gets no vote):

* EVM transactions only when `height % N == 0`; an honest proposer leaves them pooled in the other
  blocks.
* At most `evm_gas_limit / 21,000` EVM transactions.
* At most 256 KiB (262,144 bytes) of raw EVM transaction bytes; an honest proposer leaves the
  transactions that would go over pooled.
* Declared gas of the EVM transactions at most `evm_gas_limit` (step 0).

Admission to the EVM pool refuses a transaction above 256 KiB (`TxTooLarge`) or with a gas
limit above the `evm_gas_limit` of the last committed header handed to execution (or
`TORUS_EVM_BLOCK_GAS_BUDGET`, if lower). When a committed header lowers that limit, pooled
transactions above it are evicted (they could never be selected). `eth_call` and
`eth_estimateGas` use the head header's `evm_gas_limit` as their default gas and clamp a larger
`gas` to it.

Constants: `DEFAULT_EVM_GAS_LIMIT`, `DEFAULT_EVM_BLOCK_INTERVAL`, `MAX_EVM_BLOCK_TX_BYTES`,
`EVM_MIN_TX_GAS` and the two key names and ranges, in `crates/torus-types/src/lib.rs`.

## Economic Constants

Defined in `torus-economics/src/types.rs`:

| Constant | Value | Description |
|----------|-------|-------------|
| `MIN_SELF_DELEGATION` | 10,000 TRS (10^22 wei) | Minimum self-stake for validators |
| `MAX_COMMISSION_BPS` | 5000 (50%) | Maximum commission rate |
| `MAX_COMMISSION_CHANGE_BPS` | 100 (1%) | Maximum commission change per update |
| `COMMISSION_COOLDOWN_BLOCKS` | 28,800 | Blocks between commission changes |
| `DOUBLE_SIGN_SLASH_BPS` | 500 (5%) | Slash fraction for equivocation |
| `DOWNTIME_SLASH_BPS` | 10 (0.1%) | Slash fraction for downtime jailing |
| `JAIL_DURATION_BLOCKS` | 28,800 | Jail cooldown before unjail |
| `KEY_ROTATION_COOLDOWN_EPOCHS` | 1 | Epochs between key rotations |

## Consensus Parameters

Set via `hotstuff_rs::Configuration` in main.rs:

| Parameter | Value | Description |
|-----------|-------|-------------|
| `max_view_time` | 2000 ms | Maximum time per consensus view |
| `progress_msg_buffer_capacity` | 1024 | Message buffer size |
| `block_sync_request_limit` | 10 | Max concurrent sync requests |
| `block_sync_server_advertise_time` | 10 s | Sync server advertisement interval |
| `block_sync_response_timeout` | 3 s | Sync response timeout |
| `block_sync_blacklist_expiry_time` | 10 s | Peer blacklist duration |
| `block_sync_trigger_min_view_difference` | 2 | Views behind before triggering sync |
| `block_sync_trigger_timeout` | 60 s | Sync trigger timeout |
