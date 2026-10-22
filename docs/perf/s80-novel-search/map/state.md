# System map: state and storage (crates/torus-state + its callers)

Main at `eb993e3f`. All refs are `path:line` on main. Measured numbers come from two 9j2 cells (val0,
`metrics-after-val0.txt` + `summary.json`): **STD** = `~/bench-results-matched/ozarchy-9j2-std-r4-CL21`
(300 markets, Classic, 100k accounts, 76k actions/s offered x 400 orders, 21.3 blk/s, 265.7k matched/s,
E chain 39.4 ms/blk) and **DEEP** = `ozarchy-9j2-deep-r4-CL21` (same, plus a 1M preload; 1.14M resting
orders, 21.2 blk/s, 258.0k matched/s). Both ran `TORUS_CHECKPOINT_INTERVAL_MS=15000`,
`TORUS_ROCKSDB_STATS=2` and `TORUS_INGRESS_MIN_COLLATERAL=0`.

**Provenance caveat:** `summary.json` says commit `0f8eedb3` (an ancestor of main). But the node log prints
"running state hash: L6.1 window-committed books (chain config) on=Some(1)" and
"TORUS_BOOK_COALESCE ... ON", and `git grep book_window_commit HEAD -- crates` finds nothing on main (the
name appears only in `devnet/wsl/gen-3val-genesis.sh:213`). So these cells ran batch-10-like book handling
(L6.1 is on the owner's excluded list). On main with the running hash configured, book coalesce is forced
OFF (`crates/torus-consensus/src/app.rs:1253-1270`), so Classic mode encodes every dirty book every block.

## 1. Architecture and data flow

**Two RocksDB instances per node** (`rocksdb = "0.24"`, `Cargo.toml:93`):
- **State DB:** all 50 CFs of `ALL_CF_NAMES` (`crates/torus-state/src/cf.rs:311`; the "44 CFs" comment in
  `db.rs:805` is stale), a 256 MiB shared LRU block cache (`db.rs:386`) and a 1 GiB DB-wide memtable cap
  (`db.rs:1737`). Opened by `StateDb::open_cfs` (`db.rs:749-926`).
- **Consensus DB** (`<data>/consensus-db`, section 71.8 option A): `cf_consensus_meta` (HotStuff store),
  `cf_native_pending` (DA bodies), `cf_native_shards` and `cf_commit_manifest` (`db.rs:373-378`), with a
  64 MiB cache and a 256 MiB memtable cap (`db.rs:389-392`). It has its own WAL and write queue;
  `kv_store.rs` wraps it for hotstuff_rs.

**Read layering for execution** (`NativeStateOverlay`, `backend.rs:1423-1459`). A read tries, in order:
1. the block's own pending writes;
2. the resident rows R (`attach_resident`; CFs positions, balances, liquidation, oracle and markets,
   `resident_rows.rs:44-45`; torus-bridge `Users` answers positions and balances);
3. the parent (frozen block N-1);
4. L, the since-checkpoint layer (positions and balances come from L's deferred part,
   `backend.rs:1633-1652`);
5. C, the checkpoint in flight;
6. the DB (`backend.rs:1601-1607`, `3038-3092`).

R's CFs never fall through to later layers (`backend.rs:3066-3077`).
- Every overlay read takes the pending `RwLock` read side and returns an owned `Vec`
  (`backend.rs:3052-3063`).
- Every write takes the write side and does `deletes.remove` + `writes.insert` (two allocations on the
  borrowed path, `backend.rs:3095-3135`). It adds a journal entry only while an EVM call-frame checkpoint is
  open (`backend.rs:690-704`).

**Write path of a native block on a validator** (defaults: pipeline on, checkpoint 15 s):
1. **E (exec thread) writes into `PendingState`**: 50 x (`BTreeMap` writes, `BTreeSet` deletes),
   `backend.rs:500-552`.
   - The position and balance caches are flushed into it at settle
     (`torus-core/src/position.rs:975-990`, `torus-bridge/src/native_executor.rs:407-426`).
   - Classic books: `save_order_books` (`native_executor.rs:7262`) encodes whole-book blobs
     (`encode_classic_book`, `:7321`; 97 B per resting order, `torus-core/src/order_book/classic_encode.rs:30-36`)
     unless they are coalesced (`:7314`), then writes the bbo rows (`:7435`).
2. **E freezes the overlay**: it moves the maps into an `Arc<FrozenPending>` (`backend.rs:1987-2001`) and
   hands `Job::Digest` to W (queue `DIGEST_QUEUE`=128, `exec_pipeline.rs:538`, `845-886`).
3. **E end-of-block upkeep** (`app.rs:3989-4150`):
   - It spawns a NEW `torus-ckpt-merge` OS thread per block (`app.rs:426-458`).
   - That thread folds block N-1 into L (`merge_cf` clones every key and value,
     `checkpoint_layer.rs:142-169`) and hands L's main part back.
   - It then merges the deferred part (positions and balances, `checkpoint_layer.rs:35-38`) and cuts an
     ingress slice: a clone of the block's `INGRESS_CFS` rows (`backend.rs:1091-1111`, `read_view.rs:58-66`).
   - E publishes a `ReadView` for the mempool (`app.rs:4180-4195`).
4. **W on `Job::Digest`** (`torus-flush-worker`, `exec_pipeline.rs:979`, `1161-1216`):
   - It encodes action status and EVM side rows into a sidecar.
   - It SHA-256 digests the block's consensus writes (`running_hash.rs:114-176`). This is a single stream
     per domain, run inline: there is no parallel split on this path.
   - It advances the in-memory chain memo. It writes nothing.
5. **Checkpoint trigger** (`app.rs:4083-4100`), checked in this order:
   1. `h % 100 == 0` (`STATE_HASH_CHECKPOINT_INTERVAL`, `running_hash.rs:192`);
   2. epoch boundary;
   3. the 15 s interval (`exec_pipeline.rs:140`);
   4. the byte cap (`TORUS_CHECKPOINT_MAX_BYTES`, unset).

   When one fires, E:
   - runs `settle_all` synchronously;
   - merges N-1 and N into L (`app.rs:4108-4114`);
   - coalesces owed books (`app.rs:4306`);
   - submits `Job::Checkpoint` (`app.rs:4246-4300`; at most one in flight, `exec_pipeline.rs:854-869`);
   - clears the view's blocks (`app.rs:235-240`) and publishes a new view.
6. **W on `Job::Checkpoint`** (`exec_pipeline.rs:1217-1298`) builds ONE `WriteBatch` from:
   - the EVM trie prefix (the window delta);
   - `layer.append_to_batch` and the side rows;
   - `META_NATIVE_TRIE_STALE` (trie maintenance is off by default, `native_trie.rs:319-342`;
     `backend.rs:2721`);
   - the applied marker and the kept running-hash rows.

   It writes the batch with `target.write(batch)` and default `WriteOptions` (no sync, WAL on;
   `backend.rs:2774`, `db.rs:1237-1240`). Then it runs `resync_evm` as separate writes
   (`exec_pipeline.rs:1451`) and records the order-age `Durable` stage.
7. **Off E, every block**:
   - `torus-commit-persist` writes the header and bin body to the state DB (`app.rs:690-760`,
     `2232-2318`; body codec `block_body.rs:1-23`). E waits on its ticket before executing
     (`app.rs:6829-6840`).
   - The consensus thread writes the HotStuff store, the DA body and the commit manifest to the consensus DB.
   - Trade history (`bg_writer.rs`, chunked 2048 rows, `low_pri`, `bg_writer.rs:147`, `318-352`) is OFF on
     validators: the default follows `--rpc-only` (`torus-node/src/main.rs:869`, `app.rs:1184-1194`).
8. **`--rpc-only` nodes:** interval 0 means a per-block `Job::Flush` (`exec_pipeline.rs:1300-1430`), with
   the same flush code and `ChainSrc::Memo`.

**Durability:**
- Writes are process-crash safe through the OS page cache.
- They are power-loss safe only with `TORUS_SYNC_WAL_ON_COMMIT=1` (default off, `db.rs:1786-1803`). That
  fsyncs the consensus WAL, then the state WAL, once per commit (`db.rs:1268-1274`, `app.rs:2313`,
  `2329-2340`).
- Other fsyncs: the prune-meta write with `set_sync(true)` (`pruner.rs:604-614`), the genesis digest
  (`node_dbs.rs:237-241`), and the shutdown flush + sync (`main.rs:1965-1977`).
- Crash replay restarts from the durable marker, which is the last checkpoint (at most 100 blocks back).

## 2. Threads / pipeline and what is serial

| Thread | Role | Serial constraint |
|---|---|---|
| E `execution_loop` | execute, freeze, upkeep, checkpoint close | the chain-critical path; every block |
| `torus-ckpt-merge` (1 spawned per block) | fold N-1 into L, slice | E waits for L's main part at the next upkeep (`settle`, `app.rs:168-192`) |
| `torus-flush-worker` W | digest every block; write every checkpoint | strictly height-ordered; one checkpoint in flight; digests queue behind a checkpoint write |
| `torus-ckpt-drop` | frees retired layers/views | queue of 2 (`app.rs:485`); overflow is freed inline on E |
| end_resident worker | applies the block delta to `Users` / R | E waits (`end_resident_wait`) |
| `torus-commit-persist` | header + body write (state DB) | shares the state DB write queue with W's checkpoint batch |
| RocksDB bg (4 jobs) | flush/compaction for both DBs | `max_background_jobs` 4, subcompactions unset |

Serial points worth knowing:
- **(a) The running-hash chain is sequential**, `h_n = SHA256(h_{n-1} || n || D_native || D_evm)`
  (`running_hash.rs:166-175`), but each `D(n)` is independent of other blocks.
- **(b) One `WriteBatch` per checkpoint means one writer.** WAL append and memtable insert run on one
  thread; pipelined write overlaps groups, not work within a batch.
- **(c) RocksDB serializes write groups per instance**, so the commit-persist write waits behind a
  checkpoint batch. The T5.3 fix moved the manifest and HotStuff writes to the consensus DB, but header and
  body still live in the state DB.
- **(d) Checkpoint close runs on E:** a merge of two blocks, the book coalesce and the view rebuild.

## 3. Hot paths with measured or estimated cost

| Path | Thread | STD | DEEP | Source |
|---|---|---|---|---|
| position + balance cache `flush_all` into overlay | E | 5.55 ms/blk (14% of 39.4 ms chain) | 5.32 ms/blk | `torus_exec_cache_flush_seconds`, summary phases.engine |
| end_resident (apply delta to Users/R) | worker | 4.32 ms/blk | ~4.2 | `torus_exec_end_resident_seconds` |
| end_resident_wait | E | 1.58 ms/blk | 1.42 | summary phases |
| checkpoint upkeep (avg) | E | 0.68 ms/blk | 1.19 ms/blk | `torus_exec_checkpoint_upkeep_seconds` |
| checkpoint close spike (derived: total minus ~0.1 ms x regular blocks, / checkpoints) | E | ~50 ms per checkpoint | ~92 ms per checkpoint; 84 blocks > 25.6 ms, 7 in 102-205 ms | upkeep histogram |
| save_books (coalesce/L6.1 ON in these cells) | E | 0.58 ms/blk | 1.14 ms/blk | summary phases |
| replay_guard (nonce reads) | E | 0.57 ms/blk | 0.55 | summary phases |
| merge settle wait / deferred join | E | 0.18 / 0.26 ms/blk | 0.19 / ~0.24 | `exec_checkpoint_settle_wait`, `_deferred_join` |
| commit-persist ticket wait | E | 0.34 ms/blk avg; 13 blocks > 100 ms | similar | `torus_exec_commit_persist_wait_seconds` |
| running-hash digest | W | 1.38 ms/blk (7.6k entries/blk) | 1.81 ms/blk | `torus_state_hash_seconds`, `_entries_total` |
| checkpoint batch build (`append_to_batch`) | W | 95 ms per ckpt | 160 ms | `torus_exec_state_write_build_seconds` (75 / 80 ckpts) |
| checkpoint RocksDB write | W | 284 ms per ckpt, 87 MB batch, ~292 MB/s | 447 ms, 181 MB, ~360 MB/s | `torus_exec_state_write_db_seconds`, `_batch_bytes` |
| state DB `db.write` max | W | 844 ms | 1.27 s | `torus_rocksdb_db_write_max_micros` |
| amortized state write | W | 4.22 ms/blk | 7.04 ms/blk | summary phases.flush |
| W busy | W | 39.7 s / ~350 s ~= 11% of a core | higher | `torus_flush_worker_seconds_sum` |
| commit-persist header+body write | persist thread | 3.36 ms avg; 32 of 7568 > 256 ms | - | `torus_commit_persist_write_seconds` |
| compaction CPU (state DB) | RocksDB bg | 26.8 s per run (~8% core) | 33.1 s | `torus_rocksdb_compaction_cpu_micros` |

**Bytes written (state DB unless noted):**

| | STD | DEEP |
|---|---|---|
| State WAL (logical ticker, see section 6 item 4) | 11.8 GB | 18.9 GB |
| State SST flushed | 2.34 GB | 3.5 GB |
| State compaction write | 0.54 GB | 0.78 GB |
| Consensus DB WAL | 6.2 GB | 6.0 GB |
| Top flushed CFs (LOG sum) | bodies 2.19 GB, positions 2.10 GB | order_books 2.32 GB (94 flushes), positions 2.14 GB, bodies 1.97 GB |
| Flush reasons | 84 "WAL Full", 75 "Write Buffer Full" | 139 / 120 |
| Stall micros | 0 | 0 |
| Checkpoint layer avg (`torus_exec_checkpoint_layer_bytes`) | ~81 MB | ~162 MB |
| `exec_checkpoint_drop_inline_total` | 7460 | 7527 |

The inline-drop counts are about 75-80 checkpoints x ~95-100 items each. Memory (DEEP val0,
`mem-samples.txt`): RSS p50 3.9 GB, max 5.07 GB, VmHWM 5.29 GB; jemalloc retained 7-8 GB.

**Per-block reads on E** (resident books and R on):
- R and the books are memory-only.
- Nonce point reads per action plus an expiry range scan (`nonce_window.rs:44-80`; 60 s window,
  `torus-types/src/eip712.rs:49`) fall through parent -> L -> C -> DB.
- Other CFs (fee config, sessions, staking, core-writer queue, governance) also go through the layers to the
  DB.

No per-CF read counter exists in production. The `test-probes` feature (`probes.rs`,
`backend.rs:1709-1925`) is the counting tool.

## 4. Knobs and current defaults (RocksDB options actually set)

**DB-wide** (`db.rs:759-817`):

| Option | Default | Env / ref |
|---|---|---|
| `create_if_missing`, `create_missing_column_families` | on | - |
| statistics | level 1 = tickers | `TORUS_ROCKSDB_STATS`, `db.rs:575-582` |
| `enable_pipelined_write` | on | `TORUS_ROCKSDB_PIPELINED_WRITE` |
| `max_total_wal_size` | 2048 MiB | `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB`, `db.rs:555` |
| WAL compression | off | `TORUS_ROCKSDB_WAL_COMPRESSION` |
| `max_background_jobs` | 4 | `TORUS_MAX_BG_JOBS`, `db.rs:1672` |
| `max_subcompactions` | unset | `TORUS_MAX_SUBCOMPACTIONS` |
| `bytes_per_sync` | 1 MiB | `TORUS_BYTES_PER_SYNC_MIB`, `db.rs:1686` |
| `db_write_buffer_size` | 1 GiB state / 256 MiB consensus | `TORUS_DB_WRITE_BUFFER_MB` |

**Table** (`db.rs:823-829`): shared LRU cache (256 / 64 MiB), full bloom filter at 10 bits/key, 16 KiB blocks,
`cache_index_and_filter_blocks`, `pin_l0_filter_and_index_blocks_in_cache`.

**Per-CF** (`db.rs:831-846`):

| Option | Default | Env / ref |
|---|---|---|
| `write_buffer_size` | 128 MiB | - |
| `max_write_buffer_number` | 4 | `TORUS_ROCKSDB_MAX_WRITE_BUFFERS` |
| L0 slowdown / stop | unset = 20 / 36 | `TORUS_ROCKSDB_L0_SLOWDOWN` / `_STOP` |
| compression | LZ4 per level, ZSTD bottommost | - |
| `level_compaction_dynamic_level_bytes` | on | - |

**Per-CF overrides:**

| CF | Override | Default | Env / ref |
|---|---|---|---|
| `cf_consensus_meta` | write buffer | 8 MiB | `db.rs:857` |
| churny CFs (books, native_hashed, book_order_rows) | write buffer | unset | `TORUS_CHURNY_CF_WRITE_BUFFER_MB`, `db.rs:868` |
| `cf_native_order_books` | `target_file_size_base` | 4 MiB | `TORUS_BOOK_CF_TARGET_FILE_MB`, `db.rs:886`, `1714` |
| height-keyed CFs (bodies, action status, receipts, core-writer tx) | compaction filter | on in prune mode `filter` (the default) | `db.rs:893`, `prune_filter.rs:131-161` |
| append-only CFs (trades, user trades, pending) | bottommost codec | unset | `TORUS_ROCKSDB_APPEND_CF_CODEC`, `db.rs:900-908` |

**NOT set (RocksDB defaults apply):** `max_open_files` (-1), `wal_bytes_per_sync`,
`allow_concurrent_memtable_write` (default on), `unordered_write`, `two_write_queues`, `manual_wal_flush`,
row cache, prefix extractor / memtable prefix bloom, partitioned index/filters, `optimize_filters_for_hits`,
blob files (BlobDB), direct I/O, `atomic_flush`, `periodic_compaction_seconds`.
- The default CF is opened with the DB `opts`, so it has its own default table factory and cache.
- Write options are default everywhere except the bg writer's `low_pri` (`bg_writer.rs:339-340`) and the
  synced prune-meta write.
- Read options are default except `iterate_upper_bound` for prefix scans (`db.rs:43-50`).

**Pipeline / state knobs:**

| Knob | Default | Ref |
|---|---|---|
| `TORUS_EXEC_PIPELINE` | on | - |
| `TORUS_CHECKPOINT_INTERVAL_MS` | 15 s on validators, 0 on rpc-only | `exec_pipeline.rs:140-232` |
| `TORUS_CHECKPOINT_MAX_BYTES` | unset | - |
| `TORUS_BOOK_COALESCE` | requested; off when the running hash is configured on main | `exec_pipeline.rs:241-260` |
| `TORUS_CKPT_DEFER_BOOKS` | off | `checkpoint_layer.rs:61-104` |
| `TORUS_EPOCH_FAST_PATH`, `TORUS_EVM_FAST_PATH` | on | - |
| `TORUS_NATIVE_TRIE_MAINTENANCE` | off | - |
| `TORUS_SYNC_WAL_ON_COMMIT` | off | - |
| `TORUS_TRADE_HISTORY` | follows rpc-only | - |
| `TORUS_BG_WRITER_CHUNK_KVS` / `_LOW_PRI` | 2048 / on | - |
| `TORUS_ASYNC_POST_FLUSH` | off | `app.rs:8094` |
| `PARALLEL_DIGEST_MIN_ENTRIES` | 4096, per-block flush path only | `backend.rs:2470` |
| `TORUS_INGRESS_MIN_COLLATERAL` | 1 TRS (benches set 0) | `torus-mempool/src/funded.rs:57-71` |

## 5. Known limits and failure modes

- **Checkpoint write is one large batch.** 87-181 MB, 284-447 ms per write, `db.write` max 0.84-1.27 s;
  header/body writes queue behind it. T5.3 fixed this for the manifest and HotStuff writes (in-window p99
  917 -> 485 ms, roadmap section T5.3). A1/Q6 checkpoint slicing is PARKED (owner list).
- **Fail-stop triggers:**
  - A W failure or panic latches the node fail-stop (`exec_pipeline.rs:1091-1100`).
  - A merge-thread panic means L is lost, so the node fail-stops (`app.rs:4060-4067`).
  - A failed checkpoint whose marker landed is reported as `TrieStale`.
- **Book coalesce and the running hash are mutually exclusive on main** (`app.rs:1253-1270`); L6.1
  (batch 10, excluded) is what lifts that.
- **Restart cost:** DB open takes 4.0 s with the 2 GiB WAL cap (`db.rs:410-417`). Then the node replays
  up to 100 blocks plus a cold book load (4.3-4.7 s at ~1M resting orders,
  `docs/perf/s74-boot-books-2026-09-28.md`).
- **Memory:** the untuned per-CF sum, 50 x 128 MiB x 4, is capped only by `db_write_buffer_size`. DEEP RSS
  peaks at ~5.3 GB against the 8 GB validator class.
- **Window size follows block rate.** At 21 blk/s the `h % 100` attest trigger fires every ~4.7 s and the
  15 s interval never fires; it fires only below 6.7 blk/s. Layer bytes, write time and crash-replay depth
  therefore move with blk/s.
- **Classic whole-book blobs:** one key per market, rewritten every block (coalesce off) or every window.
  Value size is ~97 B x resting orders (DEEP: ~367 KB per book).

## 6. Corners that look unexamined (observations, not proposals)

**Already explored (memory). Do not re-propose these as new:**
- r9 state write with the WAL off: db time -35%, throughput flat (mem `1748fa3c`).
- ozarchy-acc: WAL zstd cuts physical writes 69% but adds +1.3% node CPU; LZ4 append codec was rejected
  (+48% SST); 2 GiB WAL cap merged (mem `25725b65`).
- BlobDB on DA `cf_native_pending`: s124 candidate, KEEP, ~-3% CPU (mem `f9113815`, `005b6c03`).
- HW CRC32C (`-msse4.2 -mpclmul` for librocksdb-sys): s124 r2 candidate, ~-0.8..-1.0% (mem `77748bc9`).
- Merge-on-E root cause and the dropper design: p3s2g; the open item is a "concurrent-read L"
  (mem `020b56cd`).
- Running-hash SHA-256 runs at ~1.4 GB/s with SHA-NI (mem `aeab7dc1`).

**Corners:**
1. **Checkpoint close on E is a spike, not an average.** The upkeep sum minus regular blocks gives ~50 ms
   (STD) / ~92 ms (DEEP) per checkpoint on the exec thread. Its parts are not timed separately: the two
   synchronous `layer.merge` calls (`app.rs:4108-4114`), `settle_all`, the coalesce, the view rebuild and
   the inline frees. This overlaps T5.3 attribution (excluded); the composition itself is unmeasured.
2. **The dropper queue (2) is flooded at every close.** `clear_view_blocks` hands ~100 `Arc` handles one
   by one (`app.rs:235-240`). Then `publish_ingress_view` hands over the old view, which may hold the last
   references to ~100 slices (`app.rs:4190-4192`). `drop_inline_total` ~= checkpoints x ~95-100 (mem
   `005b6c03` saw ~37 per checkpoint at a lower block rate), so the queue is full at that moment. Whether
   the expensive view free lands on E is unmeasured.
3. **`torus_rocksdb_block_cache_bytes` probably reads the wrong cache.** It reads the DB-level property
   (`db.rs:988`), which RocksDB resolves against the default CF. That CF has its own table factory, not the
   shared 256 MiB cache, and the metric reads 87 bytes in both cells. Real block-cache use is unknown
   (inferred from code, not confirmed).
4. **WAL bytes are ~5x flushed SST bytes**, because each checkpoint rewrites the same keys. The
   `torus_rocksdb_wal_bytes` ticker is LOGICAL (mem `25725b65`).
   - About half the memtable flushes are "WAL Full" (the 2 GiB cap) and hit cold CFs (~12 tiny flushes
     each).
   - `wal_bytes_per_sync` is unset, so ~50 MB/s of logical WAL per node goes to kernel writeback with no
     smoothing. In benches, 3 nodes share one host.
5. **Block bodies are written twice:** to DA `cf_native_pending` (consensus DB, ~6 GB WAL per run) and to
   `CF_BLOCK_BODIES` (state DB, ~2.2 GB flushed). Bodies are the top flushed CF in STD. Only the DA side
   has a candidate (BlobDB).
6. **Per-row copy count.** One position row is:
   1. allocated by the overlay put;
   2. cloned into L (`checkpoint_layer.rs:151-156`);
   3. cloned into the ingress slice (`backend.rs:1095-1096`);
   4. copied into the resident delta (`backend.rs:1025-1034`);
   5. read by SHA-256;
   6. copied into the `WriteBatch`, then into the WAL and the memtable.

   Positions are ~92% of state keys (`checkpoint_layer.rs:27-29`). The slice copy is only read by
   `is_funded` fallback 3 (mem `005b6c03`).
7. **The digest is serial per block and single-stream.** `digest_on_worker` has no parallel split
   (`backend.rs:1370-1401`); the 4096-entry split exists only in `flush_pending_after_batch`
   (`backend.rs:2590-2605`). `D(n)` does not depend on `h_{n-1}`. W is ~11% busy today, so this is not on
   the critical path yet.
8. **The checkpoint batch has a single writer.** WAL append, CRC and memtable insert of ~10^6 keys run on
   one thread; pipelined and concurrent memtable writes cannot split a single batch. The commit-persist
   tail (32 writes > 256 ms) lines up with it.
9. **`ReadView` cost grows across the window.** Each `get_cf_raw` collects a `Vec` of layers and scans up
   to ~100 frozen sets newest-first (`read_view.rs:195-217`). The funded check (`funded.rs:124-150`: balance
   read, account read, positions prefix scan) is ON by default but OFF in every matched bench, so it is
   unmeasured.
10. **One OS thread spawn per block** for the layer merge (`app.rs:439-457`). It is missing from the
    `spawn_count` sites (`spawn_count.rs:10-30`), although every other exec-path spawn is counted or pooled.
11. **Memtable flushes leave no trace in the block-time data.** "Write Buffer Full" flushes of positions and
    order_books (66-94 per run) are not correlated with block time anywhere. Stall micros are 0.
12. **Native trie CFs.** `CF_NATIVE_TRIE` / `CF_NATIVE_HASHED` are opened but unmaintained (marker only,
    `native_trie.rs:319-342`). A boot with maintenance on triggers a full rebuild.
13. **The EVM mirror resync** runs as separate small writes after each checkpoint (`exec_pipeline.rs:1451`),
    outside the atomic batch.
14. **Peak RSS vs steady state.** VmHWM is 5.29 GB vs RSS p50 3.9 GB (DEEP), with jemalloc retaining
    7-8 GB. No sample ties the peak to the checkpoint layer or the `WriteBatch`.
15. **The nonce CF is a time-major queue.** Every block inserts ~65 rows and range-deletes the expired ones
    (`nonce_window.rs:1-80`). A read of a fresh nonce misses every layer and the memtable, then pays L0 and
    level bloom probes in the DB.
16. **Cache flush.** `position.rs:975-990` sorts the dirty keys, then does one locked overlay put per row
    (~7k per block). This is the largest state-layer cost on E (5.3-5.6 ms/blk). Item 6 Phase 5 profiling
    is excluded, but this path is the overlay's.
