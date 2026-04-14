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

**Default behavior**: If neither `--archive` nor `--retention-blocks` is specified, the node runs in archive mode (no data is ever pruned).

### Subcommands

| Command | Description |
|---------|-------------|
| `keygen --output <path>` | Generate a new Ed25519 keypair and write an encrypted keystore file. Default output: `./validator.keystore`. |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Overridden by `--log-level`. Controls tracing filter. Examples: `info`, `torus_consensus=debug,info`, `trace`. |

## Port Assignments

| Port | Protocol | Service | Configurable Via |
|------|----------|---------|-----------------|
| 8545 | TCP | JSON-RPC | `--rpc-addr` |
| 30333 | UDP | P2P (QUIC) | `--p2p-listen` |
| 9090 | TCP | Telemetry (Prometheus) | Hardcoded |

## ChainConfig Fields

Set via genesis.json. Cannot be changed after genesis initialization.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `chain_id` | u64 | 7777 | EVM chain ID |
| `chain_name` | string | `"torus"` | Human-readable chain name |
| `evm_gas_limit` | u64 | 30,000,000 | Maximum gas per block |
| `base_fee_per_gas` | u64 | 1,000,000,000 | Initial base fee (1 gwei) |
| `epoch_length` | u64 | 100,000 | Blocks per validator epoch |
| `max_validators` | u32 | 21 | Maximum active validator set size |
| `min_stake` | U256 | 10,000 TRS | Minimum self-delegation to register |
| `fee_burn_bps` | u16 | 1000 | Fee burn percentage (basis points) |
| `fee_validator_bps` | u16 | 0 | Fee to block proposer (basis points) |
| `fee_treasury_bps` | u16 | 4500 | Fee to treasury (basis points) |
| `fee_dev_pool_bps` | u16 | 4500 | Fee to dev pool (basis points) |

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
