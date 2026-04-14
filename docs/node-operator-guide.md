# Node Operator Guide

## Hardware Requirements

### Known Requirements

- **Disk**: RocksDB stores all chain state across 33 column families. Disk usage
  grows linearly with block count. The primary consumers are block bodies and
  receipts. With pruning enabled (`--retention-blocks`), disk growth is bounded
  to approximately the retention window. Without pruning (archive mode), plan for
  continuous growth.
- **RAM**: RocksDB uses memory-mapped files and block cache. Minimum 4 GB
  recommended; 8+ GB for validators under load.
- **CPU**: Block building involves EVM execution and state root computation.
  Multi-core recommended for validators. Non-validator full nodes have lower
  CPU requirements.

### Unknowns

Exact requirements depend on network transaction volume, block size, and
validator set size. Monitor resource usage with the Grafana dashboards
(see [monitoring-setup.md](monitoring-setup.md)) and adjust accordingly.

## Software Prerequisites

- **Rust toolchain**: Edition 2021. Install via [rustup](https://rustup.rs/).
- **System dependencies**: `libclang-dev` (for RocksDB bindgen), `pkg-config`,
  `libssl-dev`.
- **Optional**: `promtool` (for validating alerting rules), `node_exporter`
  (for infrastructure metrics).

## Building from Source

```bash
git clone <repository-url>
cd Torus-hyperBFT
cargo build --release -p torus-node
```

The binary is at `target/release/torus-node`.

## Running a Full Node

### Minimal (non-validator, read-only)

```bash
./torus-node \
  --genesis genesis.json \
  --data-dir ./data \
  --keystore validator.keystore \
  --rpc-addr 0.0.0.0:8545
```

### All CLI Flags

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--genesis` | path | none | Path to genesis.json (required for first run) |
| `--data-dir` | path | `./data` | RocksDB data directory |
| `--validator-key` | hex | none | **DEPRECATED** Ed25519 signing key (visible in ps/history) |
| `--keystore` | path | none | Path to encrypted keystore file |
| `--passphrase-file` | path | none | Path to passphrase file for automated deployments |
| `--restore-from-snapshot` | path | none | Restore database from a snapshot before starting |
| `--p2p-listen` | multiaddr | `/ip4/0.0.0.0/udp/30333/quic-v1` | libp2p listen address |
| `--p2p-peers` | string | none | Comma-separated bootstrap peer multiaddrs |
| `--rpc-addr` | addr | `0.0.0.0:8545` | JSON-RPC listen address |
| `--log-level` | string | `info` | Log level: trace, debug, info, warn, error |
| `--archive` | flag | off | Archive mode: retain all historical data |
| `--retention-blocks` | u64 | none | Enable pruning: keep last N blocks of history |

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `keygen --output <path>` | Generate a new validator keypair and encrypted keystore |

## Network Ports

| Port | Protocol | Service | Description |
|------|----------|---------|-------------|
| 8545 | TCP | JSON-RPC | Ethereum-compatible RPC (HTTP + WebSocket) |
| 30333 | UDP | P2P | libp2p QUIC transport for consensus and block sync |
| 9090 | TCP | Telemetry | Prometheus metrics endpoint (`/metrics`, `/health`) |

### Firewall Rules

- **Validators**: Open UDP 30333 inbound for peer connectivity.
- **RPC nodes**: Open TCP 8545 for client queries. Consider restricting to
  trusted IPs or using a reverse proxy.
- **Telemetry**: TCP 9090 should only be accessible to your Prometheus server.

## Genesis

The `--genesis` flag points to a `genesis.json` file that contains:
- Initial account balances
- Chain configuration (chain ID, gas limit, epoch length, etc.)
- Initial validator set

Genesis initialization only runs on first start (when the database is empty).
Subsequent starts skip genesis even if `--genesis` is provided.

## Data Directory

The `--data-dir` directory contains:
- RocksDB database files (SST files, WAL, MANIFEST)
- All 33 column families (EVM state, blocks, receipts, staking, etc.)

Back up this directory for disaster recovery. For snapshots, use
`--restore-from-snapshot` which creates an atomic RocksDB checkpoint.

## Chain Configuration

Default values (set in code, overridden by genesis.json):

| Parameter | Default | Description |
|-----------|---------|-------------|
| `chain_id` | 7777 | EVM chain ID |
| `evm_gas_limit` | 30,000,000 | Block gas limit |
| `base_fee_per_gas` | 1 gwei | Initial base fee |
| `epoch_length` | 100,000 | Blocks per epoch |
| `max_validators` | 21 | Maximum active validators |
| `min_stake` | 10,000 TRS | Minimum validator self-delegation |
| `fee_burn_bps` | 1000 (10%) | Fee percentage burned |
| `fee_treasury_bps` | 4500 (45%) | Fee percentage to treasury |
| `fee_dev_pool_bps` | 4500 (45%) | Fee percentage to dev pool |
