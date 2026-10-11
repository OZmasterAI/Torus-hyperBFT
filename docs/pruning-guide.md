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

`--archive` cannot be combined with `--retention-blocks` or
`--trade-retention-blocks`. If no retention flag is specified, the node
defaults to archive mode.

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
