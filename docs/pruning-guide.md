# Pruning Guide

## Overview

Torus-hyperBFT stores all blockchain state in RocksDB across 33 column families.
**Since s131 the node prunes by default**: a node started without `--archive`
keeps the last **100,000 blocks** of history (`--retention-blocks` default) and
removes older block bodies, receipts and DA bodies in the background. Archive
nodes (`--archive`, or `--retention-blocks 0`) keep everything and are the
source a fresh node syncs from genesis. **At least one validator must run
`--archive`** (block sync is served by validators only, see below), and an
existing archive data dir must be restarted with an explicit choice (see
[Upgrading an archive node](#upgrading-an-archive-node)).

## Archive vs. Pruned Mode

| | Archive Mode | Pruned Mode |
|---|---|---|
| **CLI** | `--archive` (or `--retention-blocks 0`) | Default (N = 100,000), or `--retention-blocks N` |
| **Historical queries** | All blocks available | Only last N blocks |
| **Disk usage** | Grows indefinitely | Bounded by retention window |
| **Explorer indexing** | Can serve full history | Cannot serve old blocks |
| **State queries** | Always available | Always available |

## Default Retention (prune by default, owner s131)

```bash
./torus-node \
  --keystore validator.keystore \
  --genesis genesis.json
```

With no retention flag this keeps the last 100,000 blocks of historical data
(the same as `--retention-blocks 100000`). Everything older is removed in the
background. The pruner runs every 30 seconds and processes blocks in batches
to avoid latency spikes. One retention N drives all three pruners:

| Pruner | Keeps | With the default |
|---|---|---|
| State pruner (bodies, receipts, action status, CoreWriter links; `TORUS_PRUNE_MODE=filter` by default) | the last N blocks, never at or above the durable applied height | 100,000 |
| Block-tree pruner (hotstuff `cf_consensus_meta`) | max(N, 1000) heights | 100,000 |
| RH4 DA body GC (committed native-action bodies and shards) | max(N, 1000) + 128 dispatched blocks, and only once the block tree pruned the block 128 heights ago (`TORUS_DA_BODY_RETENTION` overrides the window, `0` = off) | 100,128 |

Why 100,000: a validator that was offline can catch up by block sync only
while its peers still hold the blocks it missed (there is no state sync).
100,000 blocks is about 5-9 hours at the benched block rates (roadmap,
"Validator history retention"); the production N is revisited after Q9.

Other windows:

```bash
./torus-node ... --retention-blocks 500000   # keep more history
./torus-node ... --archive                   # keep everything (archive node)
./torus-node ... --retention-blocks 0        # the same as --archive
```

The config file takes the same settings (`archive = true`,
`retention-blocks = N`). Precedence: the command line wins over the file for
history retention as a whole: `--archive` or `--retention-blocks` on the
command line decides it and both file keys are ignored (`--archive` over a file
`retention-blocks = N`, `--retention-blocks N` over a file `archive = true`).
Without either flag the file decides; `archive = true` together with
`retention-blocks = N` in the file is a startup error naming both keys. The
config file rejects unknown keys, so a misspelled `archive` or
`retention-blocks` stops the node instead of leaving it on the default.

### At least one archive VALIDATOR per network

Block sync is served by **validators only**, and there is no state sync. A
validator serves the blocks its block tree keeps: all of them with `--archive`,
the last max(N, 1000) heights otherwise. A node whose next block is below
every validator's retained range (a new node joining after the first 100,000
blocks, or a validator offline longer than the window) can catch up only from
a validator started with `--archive`. An `--rpc-only` archive node does **not**
help: it keeps the history but serves no block sync.

Without such a validator the stuck node keeps retrying and logs (every 30 s):

```
block_sync: cannot sync: requested height H is below every peer's retained range
(K peers committed past it and pruned it; each served only its uncommitted tip)
— needs an archive validator peer (a validator started with --archive; an
--rpc-only archive node does not serve block sync). Still retrying.
```

The client concludes this only from what the peers answered: a peer that
advertised a committed height at or above H but answered a request for H with
only its uncommitted tip no longer keeps H. Once every peer past H answered
that way the warning above is logged; until then the client tries the other
peers first. Fix: start (or restart) one validator with `--archive` from a data
dir that still has the history, or re-provision the stuck node from a recent
snapshot.

Bench and devnet harnesses: `devnet/wsl/start-node.sh` and
`devnet/docker-compose.yml` pass `--retention-blocks=$TORUS_RETENTION_BLOCKS`
when it is set; unset now means the node default (100,000: state pruner,
block-tree pruner and the DA GC worker all run), and
`TORUS_RETENTION_BLOCKS=0` reproduces the archive arm of earlier benches. An
archive arm, and any comparison with a pre-merge baseline (all archive, e.g.
bench queue 9a / 9d), must set it explicitly. `tools/matched-bench` logs the
effective value per cell (`retention=` on the `book_mode=` line,
`summary.json` `cell.retention`).

### Mutual Exclusion

`--archive` cannot be combined with `--retention-blocks` on the command line,
nor `archive = true` with `retention-blocks = N` in the config file (one flag on
the command line overrides both file keys, see above). Archive cannot be
combined with trade retention either (`--trade-retention-blocks` or
`trade-retention-blocks`).

## Upgrading an archive node

Before s131 a node without `--retention-blocks` was an archive node. The same
start command on the prune-by-default build would keep only the last 100,000
blocks: the first state-pruner pass deletes every block body, receipt and
action status below `tip - 100,000`, the block tree prunes the same heights, and
the deletion cannot be undone. So the node **refuses to start** when all of
these hold:

- no retention was chosen (neither `--archive` / `--retention-blocks` nor
  `archive` / `retention-blocks` in the config file),
- the data dir has no saved prune cutoff (it was never pruned) and never ran
  with history retention on (the node records that at every start with
  retention on, in `cf_consensus_meta` key `history_retention_on`),
- its tip is past 100,000.

The error names the data dir, the tip, the first cutoff, and both choices.
Restart with one of them:

- `--archive` (or `archive = true`): keep every block, as before. Do this on at
  least one validator per network.
- `--retention-blocks N` (or `retention-blocks = N`): prune to the last N
  blocks from now on; `--retention-blocks 100000` accepts the default. After
  the first start with retention on, later starts need no flag.

DA bodies on an upgraded data dir: the RH4 DA body GC starts on such a store as
on a fresh one, at its first dispatched height after the upgrade. Bodies
stored before that (the archive era) have no GC record and are **never
reclaimed** (owner, s128). Only the state pruner and the block tree free that
history's space; to reclaim the old DA bodies too, re-sync the node into a
fresh data dir (from an archive validator) or restore a snapshot taken on a
pruning node.

## Trade-History Retention (D13)

Trade history (`cf_native_trades`, `cf_native_user_trades`) is written only
when trade history is on (default on for `--rpc-only`, off on validators;
`TORUS_TRADE_HISTORY` overrides). It is node-local: outside the consensus root
and the running state hash. To bound it:

```bash
./torus-node --rpc-only ... --trade-retention-blocks 2000000
```

- **Unset** (default): every trade row is kept, as before.
- **Set to N**: on each pruner pass (same 30 s tick and 1,000-block interval
  as body pruning), trade rows of blocks below
  `cutoff = min(current - N, applied)` are deleted, where `current` is the
  latest committed height and `applied` the durable native applied height
  (crash replay rewrites the rows of `(applied, committed]`, so those are never
  pruned first).
- Independent of `--retention-blocks`: either flag alone enables the pruner,
  and each prunes only its own data.
- Every pass deletes everything below its cutoff (not only what is new since
  the last pass), so rows the background trade writer lands late, and rows
  left by a pass before a restart, are removed by the next pass. The cutoff is
  persisted node-locally (`cf_consensus_meta` key `trades_pruned_height`,
  written before a pass deletes, never lowered, not hashed), and RPC loads it
  at startup: if N is **raised** across a restart, or the flag is removed,
  blocks below the old cutoff stay `DataPruned` (their rows are gone), not an
  empty "no trades" result.
- A pass runs on the blocking thread pool, not on a runtime worker.

| CF | Key | How |
|---|---|---|
| `cf_native_trades` | `market u64 \| block u64 \| chunk u16` | skip-scan markets; one `delete_range` `[market\|0\|0, market\|cutoff\|0)` per market with old rows |
| `cf_native_user_trades` | `address[20] \| !block u64` | skip-scan addresses; seek to the address's first row below the cutoff and point-delete to the end of the address |

RPC on a node with trade retention (the cutoff is raised before a pass
deletes, so a half-pruned range is never served):

| Endpoint | Below the cutoff |
|---|---|
| `torus_getTradeHistory` | newest-first list stops at the cutoff |
| `torus_getUserTrades` | newest-first list stops at the cutoff |
| `torus_getTradeHistoryRange` | `to_block` below the cutoff: `DataPruned` error; a range reaching below it returns the kept part `[cutoff, to_block]` |
| `torus_getBlockTrades` | `DataPruned` error |

## What Gets Pruned

### Pruned (4 column families)

| Data | Key Format | Impact |
|------|-----------|--------|
| **Block bodies** (`cf_block_bodies`) | height(8 BE) | Full transaction data for old blocks unavailable |
| **Receipts** (`cf_receipts`) | height(8 BE) + tx_index(4 BE) | Transaction receipts and logs for old blocks unavailable |
| **Action status** (`cf_block_action_status`) | height(8 BE) | Per-action executed / skipped / failed / rejected status and the block's CoreWriter results (`torus_getBlockBody`) unavailable for old blocks |
| **CoreWriter tx links** (`cf_core_writer_tx`) | target block(8 BE) + seq(8 BE) | `txHash` / `callIndex` of the CoreWriter results of old blocks (pruned with the drain block's body) |

These are the largest disk consumers. Block bodies contain full transaction
data; receipts contain execution results and event logs.

### Never Pruned

| Data | Reason |
|------|--------|
| **Block headers** | Chain structure — always needed for verification |
| **EVM state** (accounts, storage, code) | Current state required for all operations |
| **Consensus metadata** (block tree, PCs, TCs) | Consensus safety depends on this |
| **Staking/governance state** | Active protocol state |
| **Exchange state** (orders, positions, balances) | Active trading state |
| **Trie nodes** | See below |

### Not Yet Pruned (Future Work)

**Trie nodes** (`cf_trie_nodes`, `cf_trie_accounts`, `cf_trie_storage`) are
not pruned in this version. Trie pruning requires reachability analysis from
the current state root — determining which internal trie nodes are still
referenced by the current state and which are only reachable from old,
deleted state roots. This is a research-level problem that Ethereum clients
(geth, reth) spent years solving with approaches like:

- Reference counting
- Mark-and-sweep garbage collection
- Snapshot-based pruning (only store diffs from a base snapshot)

Trie pruning is planned for a future release.

**Slash records** and **jail votes** have similar key format issues (address
is the leading prefix, not height).

## RPC Behavior on Pruned Nodes

When a client queries historical data that has been pruned, the node returns
a clear JSON-RPC error:

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32000,
    "message": "historical data unavailable: block 1234 has been pruned. Connect to an archive node for historical queries."
  },
  "id": 1
}
```

### Affected Endpoints

| Endpoint | Behavior When Pruned |
|----------|---------------------|
| `eth_getBlockByNumber` | Error if block had transactions and body is pruned |
| `eth_getTransactionByHash` | Error if transaction's block is pruned |
| `eth_getTransactionReceipt` | Error if receipt's block is pruned |
| `eth_getLogs` | Error if from_block is in pruned range |
| `torus_getTradeHistory` | Stops at the body prune height (trade rows are pruned only by `--trade-retention-blocks`, above) |

### Unaffected Endpoints

These always work regardless of pruning:

- `eth_getBalance` — reads current account state
- `eth_getCode` — reads current contract code
- `eth_getStorageAt` — reads current storage
- `eth_getTransactionCount` — reads current nonce
- `eth_call` — executes against current state
- `eth_estimateGas` — executes against current state
- `eth_blockNumber` — returns latest height
- `eth_chainId` — returns chain ID
- `eth_gasPrice` — reads latest header
- `eth_feeHistory` — reads headers (always retained)

## Disk Space Monitoring

The `torus_db_size_bytes` Prometheus gauge reports the total size of the
RocksDB data directory. Updated every 60 seconds.

Pruner metrics: `torus_pruner_blocks_removed` (heights pruned), and in filter
mode `torus_pruner_files_dropped` (SST files dropped whole),
`torus_pruner_filter_rows_dropped` (rows the compaction filter dropped) and the
`torus_pruner_filter_cutoff` gauge (0 = the filter drops nothing).

Alerting rules in `monitoring/alerts/node.yml`:
- **DatabaseLargeWarning**: > 100 GB
- **DatabaseLargeCritical**: > 200 GB

## Block Explorer

The block explorer indexer must connect to an **archive node** for historical
data backfill. A pruned node cannot serve old block bodies or receipts needed
for indexing.

- Archive nodes: Set up dedicated archive nodes for explorer backends
- Pruned nodes: Suitable for validators and RPC nodes serving current state

## Recovery

### Re-sync from Archive Peer

If you need historical data after pruning, the only option is to re-sync:

1. Stop the node
2. Delete the data directory
3. Restart with `--archive` (or a larger `--retention-blocks` window)
4. The node will sync from peers, downloading all blocks

### Restore from Snapshot

If you have a snapshot from an archive node:

```bash
./torus-node \
  --restore-from-snapshot /path/to/snapshot \
  --data-dir ./data-restored \
  --keystore validator.keystore
```

The snapshot must have been created before the data was pruned.

## Implementation Details

- **Pruner**: Runs as a background tokio task within the node process
- **Interval**: Checks every 30 seconds, prunes at configured block intervals
  (default: every 1,000 blocks)
- **Throttling**: Deletes in batches of 10,000 blocks with yields between
  batches to avoid RocksDB compaction latency spikes
- **Persistence**: Pruning progress is stored in the database and survives
  node restarts
- **Prune mode (`TORUS_PRUNE_MODE`, node-local, read at startup)**:
  - `filter` (default): no deletes. Each pass persists the new cutoff (synced),
    then RocksDB drops the old rows: whole SST files that lie below the cutoff
    go at once (`DeleteFilesInRange`), the rest during compactions RocksDB runs
    anyway (a compaction filter on the four height-keyed CFs). Rows below the
    cutoff that are still on disk are never served: every read of these CFs
    hides them, so RPC, crash replay and the DA GC restore answer exactly as
    with the eager delete. Perf doc section 103.6: the range tombstones of the
    `range` mode doubled `cf_block_bodies` compaction on the state DB.
  - `range`: today's eager `delete_range` per batch of contiguous heights (kept
    for A/B runs).

  Both modes share the cutoff on disk, so a node can switch between them across
  restarts. Code: `crates/torus-state/src/prune_filter.rs`.
