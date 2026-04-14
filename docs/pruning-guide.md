# Pruning Guide

## Overview

Torus-hyperBFT stores all blockchain state in RocksDB across 33 column families.
By default, the node operates in **archive mode**: all historical data is retained
indefinitely. Operators can enable **pruning** to automatically remove old block
bodies and receipts, reducing disk usage.

## Archive vs. Pruned Mode

| | Archive Mode | Pruned Mode |
|---|---|---|
| **CLI** | Default (no flags needed) | `--retention-blocks N` |
| **Historical queries** | All blocks available | Only last N blocks |
| **Disk usage** | Grows indefinitely | Bounded by retention window |
| **Explorer indexing** | Can serve full history | Cannot serve old blocks |
| **State queries** | Always available | Always available |

## Enabling Pruning

```bash
./torus-node \
  --keystore validator.keystore \
  --genesis genesis.json \
  --retention-blocks 100000
```

This keeps the last 100,000 blocks of historical data. Everything older is
deleted in background. The pruner runs every 30 seconds and processes blocks
in batches to avoid latency spikes.

### Mutual Exclusion

`--archive` and `--retention-blocks` cannot be used together. If neither is
specified, the node defaults to archive mode.

## What Gets Pruned

### Pruned (2 column families)

| Data | Key Format | Impact |
|------|-----------|--------|
| **Block bodies** (`cf_block_bodies`) | height(8 BE) | Full transaction data for old blocks unavailable |
| **Receipts** (`cf_receipts`) | height(8 BE) + tx_index(4 BE) | Transaction receipts and logs for old blocks unavailable |

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

**Trade history** (`cf_native_trades`) is keyed by `market_id + block_number`,
with market ID as the leading prefix. This makes efficient range deletion by
block height impossible without scanning every market. Deferred to a future
batch.

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
| `torus_getTradeHistory` | Returns empty (trade data not yet prunable) |

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
3. Restart with `--archive` mode (or `--retention-blocks` with a larger window)
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
- **RocksDB `delete_range`**: Uses efficient range deletion for contiguous
  key ranges (block bodies and receipts are keyed by height)
