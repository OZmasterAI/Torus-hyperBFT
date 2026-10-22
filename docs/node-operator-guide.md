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
| `--genesis` | path | none | Path to genesis.json (required for first run; afterwards the same file, see [Genesis](#genesis)) |
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
Subsequent starts do not apply it again, but the chain configuration (chain id,
timeouts, feature activations, ...) is still read from the file at every start.

The data directory is therefore bound to its genesis file: the first start
stores a digest of the file (`genesis digest stored: this data dir is bound to
the genesis file`), and a later start whose file differs refuses to start,
naming the changed fields:

```
refusing to start: genesis file /etc/torus/genesis.json differs from the genesis data dir /data was initialized with (stored digest 0x.., file digest 0x..); changed: consensus.timeout_base_ms. ...
```

Only values count: reformatting the file or reordering its keys is fine.
Restore the original file, or start from a fresh data directory to run a new
genesis. A data directory created by an older build has no stored digest; its
first start with this build stores the digest of the file it is given (logged
as a warning, since it cannot be checked against the original). A bound data
directory started without `--genesis` refuses to start.

## Running state hash: a hash-unverified node

With the running state hash on (genesis `consensus.activations.running_state_hash`;
it starts at the first block whose header carries the `RunningStateHash` bit),
each node chains a hash of its state over every block, and validators started
with `--state-hash-attest-key` post the hash every 100 blocks so nodes can
compare them on-chain. A node whose stored chain rows are lost (a data dir
restored without them, a damaged DB) or that skipped a height cannot extend
the chain: it is **hash-unverified**. Such a node:

- keeps running, executing and voting (it is not stopped);
- logs `RUNNING STATE HASH UNVERIFIED` at ERROR level at startup and again
  every 60 seconds while it lasts;
- sets the gauge `torus_state_hash_unverified` to 1 (alert on it);
- submits **no** state-hash attestations, not even for checkpoints it still
  holds from before the loss, and takes no fail-stop decision on a quorum
  mismatch (mismatches are still reported).

It stays that way until the chain is valid again: resync the node from a
fresh data dir, or restore a data dir that has its chain rows.
Options considered (owner, s134): (a) keep running and only log it; (b) keep
running and voting, ERROR + gauge, no attestations (chosen); (c) refuse to
start.

## Feature activations and upgrades

A change to how blocks execute (a **feature**) switches on at a block height
on every node at once. Each feature has a fixed id `N`; every block header
carries `features`, a bitmap of the features on at its height (bit `N`).
Validators refuse a header whose bits differ from their own schedule, and
execution (crash replay and block sync included) runs each block by its
header's bits, never by the local schedule or clock. A feature can only be
used by a binary that knows it; this binary's features are logged at startup
(`features this binary knows`).

### How an activation is scheduled

A node's schedule is the genesis `consensus.activations` united with the
table built into the binary for the chain id:

- **New chain (genesis):** `"consensus": {"activations": {"<feature_name>": <height>}}`.
  Heights 0 and 1 both mean "from block 1"; those bits are in the genesis
  parent header too, so a node with another genesis set fails block 1. An
  unknown feature name refuses to start (`invalid activations: ...`). The old
  field `consensus.state_hash_activation_height` is an alias of
  `activations.running_state_hash`; both set and different refuses to start.
  **Default (owner s136):** a genesis naming no activation (no
  `activations`, no alias) runs the running state hash from block 1;
  `"activations": {}` turns every feature off.
- **Running chain (a release):** the genesis file of a running chain cannot
  change (see "Genesis": the data dir refuses a changed file). A release
  carries the height in its built-in table (`builtin_activations`,
  `crates/torus-types/src/features.rs`, one row per chain id). A feature named
  in both the genesis and the table with different heights refuses to start
  (`feature <name> is at height <g> in the genesis and at <h> in this
  binary's built-in table`).
- Governance-scheduled activations (`ScheduleActivation`) do not exist yet.

### Before the activation height H

1. Install the release on **every** validator and every RPC / full node. A
   block at H needs a quorum (more than 2/3 of the stake) of validators with
   the same schedule; with 3 validators that is all of them. Fewer: the chain
   stalls at H on every node (no fork) until enough validators upgrade, then
   it continues from H.
2. Restart normally: same data dir, same genesis file. Nothing is migrated;
   the node goes on from its applied height.
3. Check each node:
   - startup log: `feature activation scheduled feature=<name> id=<N> height=<H>`;
   - `torus_getFeatureSchedule` (below): the feature is in `knownFeatures` and
     in `schedule` with `activationHeight` H, the same on every node;
   - gauges: `torus_feature_activation_height{feature="<name>"}` = H,
     `torus_feature_blocks_until_activation` counting down
     (alert `FeatureActivationApproaching` under 10000 blocks).

```
curl -s -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"torus_getFeatureSchedule","params":[]}' \
  http://127.0.0.1:8545
```

The reply (heights and bitmaps are hex):

| Field | Meaning |
|-------|---------|
| `chainId`, `height` | The chain id and the latest block this RPC serves; `active` / `activeBits` are read at `height` |
| `activeBits` | The feature bits a header at `height` carries under this node's schedule |
| `knownBits`, `knownFeatures[]` | What this binary can execute: `name`, `id`, `bit` |
| `schedule[]` | Each scheduled feature: `name`, `id`, `bit`, `activationHeight`, `active` (on at `height`), `builtIn` (this binary's table names it; else genesis only) |
| `nextActivationHeight`, `blocksUntilActivation` | The next activation above `height` (`null`: none) |
| `upgradeRequired` | `{feature, height}` once this node met a header with a feature it does not know, else `null` |

At H every header carries the bit and `torus_feature_active{feature="<name>"}`
turns 1 once the node applied H.

### UPGRADE REQUIRED (a node on an old release)

A node that does not know a feature the network switched on cannot take part
from H on. It:

- refuses to vote on, insert or propose on a header with the unknown bit and
  logs at ERROR: `UPGRADE REQUIRED: feature <N> at <H> -- refusing a header
  with a feature this binary does not know` (or `-- refusing to propose on it`);
  block sync logs `block_sync: UPGRADE REQUIRED -- block at height <H> from
  <peer> ...` and does not blacklist the upgraded peer;
- keeps running with its applied and committed heights below H (no fork, no
  state change);
- sets `torus_upgrade_required` = 1, `torus_upgrade_required_feature` = N and
  `torus_upgrade_required_height` = the lowest such height (H); counts
  `torus_upgrade_required_refusals_total{site}`; `torus_getFeatureSchedule`
  returns `upgradeRequired: {"feature": N, "height": "0x.."}`. Alert:
  `UpgradeRequired`.

If a block with an unknown bit is already committed in its data dir (an
upgraded node downgraded, or a crash after commit), execution stops before it
(`UPGRADE REQUIRED: feature <N> at <H> -- this binary cannot execute the
committed block (fail-stop)`), nothing of H is written, and the process exits
with code 70 (`FATAL: execution pipeline has died`). A supervisor restarting
the same binary gets the same exit.

**Recovery:** install the release that knows feature N (the network's
release), restart with the same data dir and genesis file. The node replays
or syncs on from below H and rejoins; no resync is needed. The gauges reset
with the restart.

A node that knows the feature but has **another height** for it (the wrong
release) rejects the headers at the network's H with
`REJECTED -- header feature bits differ from this node's activation schedule`
(WARN; no vote, no insertion) and stops there; `torus_upgrade_required` stays
0. Compare `torus_getFeatureSchedule` with an upgraded node and run the
network's release. Blocks it already committed execute by their header bits
(logged at ERROR: `committed header feature bits differ from this node's
activation schedule -- executing by the header`).

### Genesis file refused (`refusing to start: genesis file ... differs`)

Scheduling a feature by editing the genesis file of a running chain does not
work: the data dir is bound to the file it was initialized with and refuses a
changed one, naming the changed fields (`changed: consensus.activations.<name>`).
Restore the original file (keep a copy of it with each data dir); a new
activation on a running chain comes with a release (above). Only a new chain
(fresh data dirs on every node) takes a new genesis.

### Running state hash after an activation

The running state hash is the feature `running_state_hash` (id 0): each node
starts its hash chain at the first block whose header carries bit 0. A node
that lost its chain rows keeps running, hash-unverified (see "Running state
hash: a hash-unverified node", `torus_state_hash_unverified` = 1); recover by
resyncing from a fresh data dir or restoring a data dir that has its rows.

### Window-committed order books (L6.1) after an activation

L6.1 is the feature `book_window_commit` (id 1,
`docs/plans/l61-window-committed-books.md`). It needs `running_state_hash` in
the same schedule (refused otherwise) and the Classic book mode. Genesis:
`"activations": {"running_state_hash": 1, "book_window_commit": 1}`, or the
bool `"book_window_commit": true`, which means "from the running hash's
height" (an explicit `activations.book_window_commit` at another height next
to it refuses to start). From the first block whose header carries bit 1, the
running hash skips the order-book CF except at every 100th height, where the
block always runs the native phase and saves (and hashes) every book.
Execution runs this by the header's bit, so a node with another schedule
replays the same transition. L3.1 coalesced book saves (`TORUS_BOOK_COALESCE`)
run with the hash on only when the schedule starts L6.1 no later than the
hash; a header that carries the hash bit without bit 1 on such a node
fail-stops (fix the genesis activations).

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
