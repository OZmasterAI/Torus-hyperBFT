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

### Stopping the Node

Stop the node with SIGTERM or SIGINT (Ctrl+C). The node then shuts down
cleanly: it stops consensus, executes and persists every committed block
still in the execution queue, flushes memtables and syncs the WAL. A restart
after a clean stop replays no blocks and no WAL.

The shutdown is bounded by `TORUS_SHUTDOWN_TIMEOUT_SECS` (default 60). If the
bound passes, or a second signal arrives, the node exits at once. Recovery is
then the same as after a kill: the node replays on restart, which is slower
but safe.

The service manager must wait longer than this bound before it sends SIGKILL.
The defaults of Docker (10 s) and Kubernetes (30 s) are shorter, so set the
grace period to the bound plus a margin, e.g. 75 s:

| Runtime | Setting |
|---------|---------|
| Docker | `docker stop -t 75 <container>` |
| Docker Compose | `stop_grace_period: 75s` on the service |
| Kubernetes | `terminationGracePeriodSeconds: 75` in the pod spec |
| systemd | `TimeoutStopSec=75` and `KillSignal=SIGTERM` in the unit |

Make sure the signal reaches `torus-node` itself. In a container, start the
node in exec form (`ENTRYPOINT ["torus-node", ...]`) or with `exec` in a
wrapper script. A shell started as `sh -c "torus-node ..."` may not forward
SIGTERM, so the node gets only the SIGKILL. Do not send a second signal
(`docker kill`, a second Ctrl+C) during a normal stop: it cuts the clean
shutdown short. If you raise `TORUS_SHUTDOWN_TIMEOUT_SECS`, raise the grace
period with it.

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

### Order-book storage mode (`consensus.book_mode`)

The order-book storage mode is a genesis field, fixed for the chain's life
(owner s131). Modes other than Classic change the state format, so every
node of a chain must run the same mode; taking it from genesis makes a fleet
that shares a genesis agree by construction.

```json
"consensus": { "...": "...", "book_mode": "level_authority_chunked" }
```

| Value | Number | Mode |
|-------|--------|------|
| `"classic"` (or absent) | `0` | Whole-book blob per market. Default: every existing genesis, including the testnet's. |
| `"order_rows"` | `1` | C4 per-order rows. |
| `"level_authority"` | `2` | 3c level rows + node-local order rows. |
| `"level_authority_chunked"` | `3` | Mode 2 with the chunked level hash ("mode 3"). |

Any other value refuses the genesis. At startup the node logs
`book mode: <mode> (genesis consensus.book_mode)` and refuses to start when:

- `TORUS_BOOK_ROWS` is set and names another mode, or is not `0`-`3`.
  The variable is deprecated: unset it. If it repeats the genesis mode the
  node starts with a deprecation warning.
- the data dir's `__book_mode__` marker (written when genesis is applied and
  on the first block) names another mode: the data dir belongs to another
  chain, or the genesis file changed after it was applied.

A restart without `--genesis` takes the mode from the marker.

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
