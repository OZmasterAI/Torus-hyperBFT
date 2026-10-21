# Node Operator Guide

## Hardware Requirements

### Known Requirements

- **Disk**: RocksDB stores all chain state across 33 column families. Disk usage
  grows linearly with block count. The primary consumers are block bodies and
  receipts. With pruning (the default since s131, `--retention-blocks`, N =
  100,000), disk growth is bounded to approximately the retention window.
  Without pruning (`--archive`), plan for continuous growth.
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
| `--state-hash-attest-key` | path | none | Validators, running state hash on: file (hex, mode 0600) with the validator's secp256k1 **account** key, i.e. the registered validator address, not the Ed25519 consensus key. The node then signs and submits `AttestStateHash` for every durable checkpoint (every 100 blocks) and resubmits a vote that is not on chain after 65 s. A key whose address is not an Active validator submits nothing; check the `addr=` startup line. Unset: the node never attests but still checks. Keep it separate from the price feeder's oracle signer (same-account nonces collide). See [State Hash Attestations](validator-guide.md#state-hash-attestations---state-hash-attest-key) |
| `--restore-from-snapshot` | path | none | Restore database from a snapshot before starting |
| `--p2p-listen` | multiaddr | `/ip4/0.0.0.0/udp/30333/quic-v1` | libp2p listen address |
| `--p2p-peers` | string | none | Comma-separated bootstrap peer multiaddrs |
| `--rpc-addr` | addr | `0.0.0.0:8545` | JSON-RPC listen address |
| `--log-level` | string | `info` | Log level: trace, debug, info, warn, error |
| `--archive` | flag | off | Archive mode: retain all historical data. Run it on at least one **validator** per network (block sync is served by validators only) |
| `--retention-blocks` | u64 | 100000 | History retention, on by default since s131: keep the last N blocks of history; `0` = archive. An existing archive data dir past N must be restarted with `--archive` or an explicit `--retention-blocks N` (see `pruning-guide.md`) |

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

Classic is the absent field: write `"classic"` or leave `book_mode` out, the
node runs the same chain either way. The bench and devnet generators leave it
out (one Classic chain, one genesis file).

#### Upgrading an existing data dir

Before s131 each node took its mode from `TORUS_BOOK_ROWS` (unset = Classic),
and wrote the `__book_mode__` marker on its first book save. A data dir from
that build keeps working as follows when the node is upgraded:

| Data dir | Genesis file at restart | `TORUS_BOOK_ROWS` | Result |
|----------|------------------------|-------------------|--------|
| Classic (marker classic, or no marker yet) | no `book_mode` (or `"classic"`) | unset or `0` | Starts in Classic. Nothing to do; unset the variable. |
| Mode M != Classic (ran with `TORUS_BOOK_ROWS=M`) | no `book_mode` | `M` or unset | **Refuses to start** (the variable disagrees with the genesis, or the marker names mode M while the genesis says Classic). Fix: add `"book_mode": "<name of M>"` to `consensus` in the genesis file, the same file on every node of the chain, then unset the variable. |
| Mode M | `book_mode` = M | unset or `M` | Starts in mode M (a deprecation warning while the variable is set). |
| Mode M | `book_mode` = another mode | any | **Refuses to start** (marker mismatch). The mode of an existing chain cannot change: a new mode needs a new chain (fresh data dirs). |
| No marker (genesis applied, no block yet) | `book_mode` = X | unset or `X` | Starts in X; the first block writes the marker. |
| any | restart without `--genesis` | unset or the marker's mode | Starts in the marker's mode (Classic without a marker). |

Adding `book_mode` to the genesis file of a running chain changes no state:
the field is read at every start, the genesis state is never re-applied, and
it is not part of any hash. It only has to match the marker. Every node of a
chain must use the same value: two genesis files that differ only in
`book_mode` start fine and fork at the first block that saves a book.

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
