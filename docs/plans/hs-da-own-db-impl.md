# Section 71.8 option A: HotStuff store and DA store in their own RocksDB (prototype)

Branch `perf/hs-da-own-db`. Goal: QC / lock / vote writes (`RocksKVStore`) and DA mirrors
(`NativeDaStore::put_batch`) stop queueing behind the ~200 MB state checkpoint `WriteBatch`.
No checkpoint slicing (option B), no flags, no env vars.

## What moved
- New DB at **`<data_dir>/consensus-db`** (`torus_state::consensus_db_path`), opened by
  `StateDb::open_consensus` with CFs `cf_consensus_meta`, `cf_native_pending`, `cf_native_shards`.
  Same per-CF options and `DbTuning` (env) as the state DB. Own WAL, write queue, 64 MiB block
  cache, 256 MiB memtable budget (state DB keeps 256 MiB cache, `TORUS_DB_WRITE_BUFFER_MB`).
- ALL `RocksKVStore` traffic (genesis `Replica::initialize`, the replica, `torus-unwedge`,
  `torus-wedge-inject`) and the DA store (mempool via `Mempool::with_da_store`, network serve).
- Stays in the state DB: its own `cf_consensus_meta` with the state meta keys of the checkpoint
  batch (applied marker, trie-stale, running hash, epoch plan, governance, pruner, trade rows),
  headers / bodies / commit manifest, `persist_committed_block_durably`, `persist_commit_manifest`.
  No state-side reader does a full scan of `cf_consensus_meta`; HotStuff keys were never hashed.
- Wiring is `crates/torus-node/src/node_dbs.rs`. app.rs: one doc comment only.

## Sync / durability points
| point | decision |
|---|---|
| `StateDb::sync_wal` (torus-state db.rs) | syncs the attached peer (consensus DB) WAL **first**, then its own. When it returns, every write issued before it in either DB is durable, as with one WAL; a durable state-DB write still implies the consensus-DB writes before the call are durable. |
| `sync_committed_wal` (app.rs, `TORUS_SYNC_WAL_ON_COMMIT`, default off) | unchanged code, covered by `sync_wal` above. It runs inside `on_committed_block` (`enqueue_for_execution` -> `dispatch_to_exec`), before hotstuff writes `APP_FED_BLOCK_HEIGHT`, so with the toggle on the order manifest/header/body/frontier/DA before APP_FED stays power-loss safe. Cost when on: 2 fsyncs per commit instead of 1. |
| `flush_for_shutdown` (main.rs) | flushes both DBs' memtables (consensus first), then `sync_wal` (both WALs). |
| `sync=true` writes | none in the codebase (all `WriteOptions` default, `write_with` callers only set `low_pri`). |
| implicit one-WAL ordering, toggle off (default), power loss only | weakened, accepted: e.g. `APP_FED_BLOCK_HEIGHT` (consensus DB) may survive while the manifest (state DB) is lost, or DA bodies lost while the block body survives. Process kill: unchanged (completed writes are in the page cache for both DBs). |

## Restart rule (`node_dbs::check_layout`, before genesis init)
Probe: HotStuff state = `COMMITTED_VALIDATOR_SET` (`[5]`, always written by `Replica::initialize`)
in `cf_consensus_meta`; chain = any row in `cf_accounts`, `cf_staking_validators` or
`cf_block_headers` of the state DB.
- HotStuff in the state DB (old layout, or mixed) -> refuse; `consensus-db` is not created. No migration.
- HotStuff in consensus-db, state DB empty -> refuse (genesis would reset the locks).
- state DB has a chain, no HotStuff in consensus-db (deleted, restored snapshot) -> refuse.
- both empty (fresh) or both present (restart) -> start.

## Metrics
`torus_rocksdb_consensus_*`: the same 29 unlabelled DB-wide gauges for the consensus DB, same
sampler and interval (`TORUS_ROCKSDB_STATS_INTERVAL_SECS`). State DB names unchanged. The 60 s per-CF
gauges add `{cf=..., db="consensus"}` series. `tools/matched-bench/run-cell.sh` WIDE_COLS is not
changed: the new names must be added there (or to archy's scraper) to land in the wide CSV.

## Tests
- torus-state `consensus_db_tests.rs` (5): CFs + reopen inside the state dir, DA writes only in the
  consensus DB and survive reopen, `sync_wal` syncs the peer (`rocksdb.wal.synced`), attach once.
- torus-mempool: `da_mirrors_land_in_the_given_da_store_not_the_state_db`.
- torus-telemetry: `consensus_db_stat_metrics_register_unlabelled_with_their_own_names`.
- torus-node `node_dbs` (8): HotStuff writes (genesis init + vote state) only in consensus-db and
  state meta keys only in the state DB; DA writes; reopen recovers HotStuff + DA; the 3 refusals
  and the rule table. main.rs `shutdown_flush_covers_the_consensus_db`.
- torus-node `sigterm_shutdown.rs` (real binary): after a run, HotStuff keys only in consensus-db,
  the checkpoint's applied marker only in the state DB, restart resumes and the voted view advances;
  an old-layout data dir exits non-zero with the error and no `consensus-db`.

## Open
- `--restore-from-snapshot` restores the state DB only (snapshots never had a node-side producer),
  so a restored dir is refused by the restart rule.
