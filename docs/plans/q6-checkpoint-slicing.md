# Q6: checkpoint batch slicing (section 71.8 option B): design and options

Status: PHASE 1, design only (s127). Nothing built beyond Q6 step 1 (the `torus_commit_manifest_seconds`
timer, `a1097df3`). The owner picks from section 5; the build follows on `perf/q6-ckpt-slicing`.
Code anchors are `file:line` on `a1097df3` (main `dcee5cac` + step 1). RocksDB anchors are in the vendored
source, `rocksdb/` = `~/.cargo/registry/src/index.crates.io-*/librocksdb-sys-0.17.3+10.4.2/rocksdb/`
(rust-rocksdb 0.24.0, RocksDB 10.4.2). Numbers are from the results doc
(`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`, sections 71 and 74) unless marked **(estimate)**.

## 1. Problem

### 1.1 Measured
- Every 15 s the flush worker W writes the since-checkpoint layer as ONE `WriteBatch` into the state DB:
  ~200 MB (193-201 checkpoints per 600 s cell, batch 199-201 MB mean, max ~269 MB), DB write 0.60 s mean,
  flush 0.84-0.89 s (74.1, 71.2). Payload (41.2, measured on tw, peak): ~1.4M keys, positions ~185 MB, book blobs up to ~41 MB.
- Before option A (71): every other state-DB writer queues behind it. In windows the mean DB write is 12-13x longer
  (16.6-18.7 vs 1.26-1.46 ms), write_other 13-24x more frequent, every admission > 0.512 s is in a window,
  val0 commit p99 is 460-810 ms higher. RocksDB stall counters (write_stall, stall micros, stopped) are 0:
  this is the write queue, not the write controller. Writeback throttling ruled out (71.4).
- Option A (HotStuff + DA in their own DB, `docs/plans/hs-da-own-db-impl.md`, on main) removed 90.6% of the
  in-window non-checkpoint write wait (2.27 -> 0.21 s per window-second), but in-window block p99 only fell
  1,575 -> 1,370 ms (out of window 946 / 936 ms); 434 of the 629 ms in-vs-out gap remains (74.1, 74.6).
- What is left is on the state DB (0.21 s/s; the consensus DB adds 0.002 s/s, 74.2). On the consensus thread:
  - `on_committed_block` (its only state-DB write is the commit-manifest put, app.rs:7748): 118 vs 106 ms per call
    in vs out of windows; B - A in windows +25 ms per call, CI [+10, +40]; 0.10-0.14 s per window-second,
    41-68% of B's remaining state-DB excess (74.3).
  - `persist_committed_block_durably` (header + body + manifest delete, app.rs:1582, called at app.rs:8284): no
    shift A -> B, but rare full-batch waits in both arms: 6-8 node-seconds > 250 ms per cell, max 0.65-0.97 s (74.3).
    These are the size of the checkpoint write itself, so they land in the p99.
  - The rest (0.06-0.14 s/s) is on other state-DB writers the data cannot name (74.6 item 5): candidates are the
    trade writer (`bg_writer.rs`, chunked + low_pri), the pruner (`pruner.rs:313-343`, direct `delete_range_cf`),
    and the consensus-thread pair above.
- Section 74's Q6 success criterion: in-window block p99 within ~100 ms of out-of-window p99, and
  `on_committed_block` in-window = out (`docs/perf/bench-queue.md` Q6).

### 1.2 What the consensus thread waits behind (RocksDB write path, verified in the vendored source)
State DB options: pipelined write ON by default (`DbTuning.pipelined_write`, db.rs:532, set at db.rs:737), per-CF
memtable 128 MiB (db.rs:796), DB-wide memtable budget `TORUS_DB_WRITE_BUFFER_MB` default 1024 MiB (db.rs:780,
db.rs:1608), `sync=false` on every write (`StateDb::write`, db.rs:1160), default WAL recovery mode
(point-in-time). A writer arriving while the checkpoint batch is being written waits in up to three places:
1. **WAL stage.** Writers join the write thread; the leader groups followers only up to
   `max_write_batch_group_size_bytes` (1 MiB default, `rocksdb/include/rocksdb/options.h:1271`; not in the C API).
   A leader above 128 KiB takes nobody along (`rocksdb/db/write_thread.cc:451-455`), so the 200 MB batch is a
   group of one, and the next group writes its WAL record only after it.
2. **Memtable stage.** With pipelined write, memtable groups are entered in order
   (`EnterAsMemTableWriter`, `rocksdb/db/db_impl/db_impl_write.cc:1108`); a one-writer group inserts serially
   (`InsertInto`, :1113-1124), and the sequence is published only after the whole batch
   (`SetLastSequence`, :1120). A small writer that finished its WAL record still waits for the checkpoint's
   ~1.4M-key insert before its own insert.
3. **Memtable switch.** ~185 MB of positions (41.2) overflow the 128 MiB positions memtable during the insert
   (`CheckMemtableFull` per put, `rocksdb/db/write_batch.cc:2930`), which queues a flush. The next WAL leader runs
   `PreprocessWrite` (`db_impl_write.cc:1492`): `!flush_scheduler_.Empty()` -> `WaitForPendingWrites()`
   (:1538), which with pipelined write waits for EVERY memtable writer (`rocksdb/db/db_impl/db_impl.h:2286-2297`)
   while it holds the WAL leadership, so every writer behind it waits too.

All three are bounded by the size of the one batch in flight. That is why the waits in 71.3 / 74.3 match the
checkpoint DB write (0.48-0.62 s, 0.65-0.97 s), and why cutting the batch bounds them. The split between the
three (WAL vs insert vs switch) is not measured; step 0 below measures it.

## 2. How Hyperliquid avoids this
hl-node keeps state in memory, streams blocks to append-only files, logs consensus messages to hourly files, and
writes a full state snapshot (msgpack) only every 10,000 blocks; no shared database write queue (hl-node README,
s121 memory). Consensus and order flow never share a write queue with a snapshot. What it implies for us:
- The structural end state is "state checkpoint in a store nobody else writes" (option D2). Option A was the
  first step; the commit-time block records (headers / bodies / manifest) are the remaining shared writers.
- HL can lose its in-memory state and rebuild from the last snapshot + block log; so can we (boot replay from the
  marker, `replay_committed`, app.rs:6326). The constraint HL does not have: our replay reads the state DB, so the
  DB must be at exactly one checkpoint height when replay starts.

## 3. The constraint any slicing must meet
Today the batch is built in `flush_pending_after_batch` (backend.rs:1969, via `flush_checkpoint_batch`
backend.rs:1944 <- `CheckpointLayer::flush_checkpoint` checkpoint_layer.rs:340 <- W's `Job::Checkpoint`
exec_pipeline.rs:899): the layer's rows (backend.rs:2033), W's side rows (action status), trie ops or the stale
mark (backend.rs:2142), the applied marker = T (backend.rs:2155), the window's kept running-hash rows
(backend.rs:2187), one `target.write(batch)` (backend.rs:2194). Every op is a blind `put_cf` / `delete_cf`:
no merge operator, no `delete_range` (`PendingState::append_to_batch` backend.rs:672,
`running_hash::append_to_batch` running_hash.rs:456, `ChainRows::append_to_batch` running_hash.rs:575; the
only `delete_range` users are the pruner and `ensure_trade_history_format`, neither in this batch).

If the batch is cut into slices B1..Bn and the process dies after Bk (k < n), the DB holds T-values for the keys
of B1..Bk, S-values elsewhere, and marker S. Boot replay re-executes S+1.. and reads the T-values: divergence.
Power loss with `sync=false` is worse only in degree: the WAL keeps a prefix, so any prefix of slices can survive.

Readers while slices land (live, no crash):
- E (exec thread) reads own -> R -> parent -> L -> C -> DB (checkpoint_layer.rs:11-12). C, the checkpoint in
  flight, holds every state row of the batch and is dropped only once W reports `durable_height >= T`
  (`checkpoint_read_layers` app.rs:2643-2672; W stores `durable_height` after the whole job, exec_pipeline.rs:760).
  So E never reads a sliced state row from the DB. Shielded today, no change needed.
- The published ingress view (mempool, RPC submit) is blocks above S -> C -> DB (`publish_ingress_view`
  app.rs:2838). Same argument.
- DB-direct readers: validator RPC (`torus.rs`, `eth.rs`; documented to lag, item6-phase3-impl.md step 2),
  `scan.rs:752` (one snapshot per market book), the pruner (reads the marker), consensus's epoch-plan read
  (marker first, then the plan, `epoch_plan` app.rs:6698). Today each of them already sees "every key >= the
  marker it read" (the atomic batch can land between two reads). With slices they still see that, plus a new
  case: within one read pass, some keys at T and some at S, for the ~1 s of slicing.

## 4. Options

### A1. Slices + a redo (intent) file, marker and hash in the last slice (recommended)
Mechanism:
1. W builds the checkpoint exactly as today, but into a `SliceSink` that starts a new `WriteBatch` every
   `TORUS_CKPT_SLICE_MB` (default for the A/B: 0 = today's one batch, byte for byte). Op order is today's order;
   slices are contiguous runs of it. Never cut: inside a `CF_NATIVE_ORDER_BOOKS` market prefix
   (`READER_SCANNED_CFS`, cf.rs:105, 8 bytes; keeps `scan.rs`'s "both sides from one snapshot"), and the tail
   group (trie ops or stale mark, applied marker, running-hash rows) is always the last slice.
2. W writes the slices' raw reps (`WriteBatch::data()`, rust-rocksdb write_batch.rs:206) to
   `<state dir>/ckpt-redo/redo-<T>.tmp`: header {magic, version, base S, target T, n, the window's EVM resync
   addresses}, per slice {len, crc32c, rep}, footer {crc, magic}. Write-behind `sync_data` every 8 MiB so host
   Dirty stays low (section 71.4: throttling starts near 160 MB at the 256 MB limit), final `sync_data`,
   rename to `redo-<T>`, fsync the directory.
3. Then W writes B1..Bn into the state DB, one `db.write` each. Queued writers get in between (FIFO write thread).
4. After Bn: caches commit, `durable_height = T`, C is dropped, as today. `redo-<T>` stays.
5. Deletion fence: at the start of the next checkpoint (or at shutdown, after `flush_for_shutdown` main.rs:1703),
   W calls `StateDb::sync_wal` (db.rs:1191) and then unlinks every redo with target <= the last completed T.
6. Boot, before anything reads state (before `replay_committed`, and in the node right after `node_dbs` opens
   the DBs): read marker M; delete `*.tmp`; for a `redo-<T>` with T <= M: delete (after a `sync_wal`); for
   T > M: validate it; if `base == M`, apply every slice (`WriteBatch::from_data` + `write`), run the EVM resync
   for its addresses, `sync_wal`, delete; any other case (bad checksum on a renamed file, `base != M`, two
   candidates) = fail-stop with a clear error, never a guess. Then replay from the (new) marker as today.

Crash safety (proof sketch):
- Let D_S be the DB at marker S and B the ops of the checkpoint (= B1..Bn concatenated). For blind puts/deletes,
  apply(B, X) depends on X only through the keys B does not touch. Every crash state X_k = D_S + B1..Bk + (other
  writers' writes) agrees with D_S on keys outside B, PROVIDED no other writer touches B's keys during the window.
  So apply(B, X_k) = apply(B, D_S) = D_T (on B's keys) for every k, including k = n (re-applying landed slices
  rewrites the same bytes) and k = 0.
- Key disjointness (to audit and pin by test in the build): during a window the other state-DB writers are the
  consensus thread (CF_BLOCK_HEADERS / BODIES / COMMIT_MANIFEST), the trade writer (trade CFs), the pruner
  (bodies / action status / receipts / trades below the cutoff <= marker, `PRUNE_META_KEY` in CF_BLOCK_HEADERS
  pruner.rs:487, `META_TRADES_PRUNED_HEIGHT` pruner.rs:501), EVM resync after the checkpoint. E writes serially
  only after a barrier (`wait_idle`, W idle). The batch writes state CFs, state meta keys in CF_CONSENSUS_META,
  action status for heights > S, trie CFs. Expected disjoint; the build adds a test that fails on overlap.
- SIGKILL anywhere: (a) during the redo write -> only `.tmp` exists, no slice written, M = S: delete, replay as
  today. (b) after the rename, before or between slices -> `redo-<T>`, M = S = base: apply -> D_T, M = T.
  (c) after Bn -> M = T: the redo is stale, delete. Page cache survives a process kill, so every completed write
  survives.
- Power loss, `sync=false`: the WAL recovers a prefix (point-in-time). Slice B1 is submitted only after the redo's
  `sync_data` + rename + dir fsync, so any surviving slice implies a surviving redo. A redo is deleted only after
  a `sync_wal` that followed its Bn, so a deleted redo's slices are all on disk. Checkpoint T2's slices start
  after T's Bn, so if any T2 slice survives, all of T survived and M >= T: at most one redo ever needs applying,
  and its base is M. Same guarantee as today: the node restarts at a consistent, possibly older, checkpoint.
- Trie / running hash / stale mark / epoch plan / governance / pruner keys: the tail (trie ops, stale, marker,
  chain rows) lands atomically in Bn, so "marker = T" still implies "h_T and trie rows for T". Epoch plan and
  governance rows are ordinary layer rows (state meta keys): covered by the proof; their readers gate on the
  marker. Trie ops are computed from D_S reads before B1 is written (no other writer of trie CFs), as today.
- W write error mid-slices: W latches fail-stop as today (exec_pipeline.rs:786); restart takes case (b).
  `marker_is` (exec_pipeline.rs:1053) still separates "landed but trie stale" (marker = T) from "not landed".

Reader consistency: E and the ingress view are shielded by C (section 3). DB-direct readers see per-key
monotone values (S or T, never older than the marker they read), mixed across keys for ~1 s per window. Book
scans stay whole per market (no cut inside a market prefix). Validator RPC multi-key answers (a position at T,
a balance at S) can be mixed during slicing: new in rate, not in kind (owner Q4). Offline tools that open the
state DB after a crash must see the pending redo: read-only open warns, read-write tools refuse until the node
ran once (owner Q7).

Flags:
- `TORUS_EPOCH_FAST_PATH` (exec_pipeline.rs:64, default off): a boundary checkpoints at its own height
  (app.rs:2768) and consensus reads the boundary+L plan once the durable marker reaches the boundary
  (app.rs:6698-6722). Slicing adds W latency to that durability (**estimate** +0.2-0.5 s: redo + fence); a
  refusal there is liveness-only (refuse, retry). Tests run both settings.
- `TORUS_CKPT_DEFER_BOOKS`: E-side only; the layer handed to W is whole after `settle_all` / `take_layer`
  (app.rs:2706-2790), and the flag "never changes what a checkpoint writes" (checkpoint_layer.rs:52-60). No
  interaction; tests run both settings.
- Trie maintenance (`TORUS_NATIVE_TRIE_MAINTENANCE`, off by default): trie ops go in the tail slice, so with
  maintenance on the last slice is large and slicing helps less (owner Q5).
- Pruner: separate thread, keys disjoint (above), reads the marker. No change.

Expected effect: a waiter's queue time is bounded by about one to two slices instead of one batch. At 4 MiB
(~50 slices, ~12 ms each at today's 3 ms/MB) **(estimate)**: `on_committed_block` in-window ~= out (high
confidence), the 0.65-0.97 s `persist_committed_block_durably` waits gone (high), the remaining state-DB excess
(0.21 s/s) mostly gone (medium-high). In-window block p99 1,370 -> ~1,050-1,200 ms (**estimate**, medium-low): the
unattributed rest of the 434 ms gap may be CPU / IO (rocksdb-bg flush and compaction threads wake 12.5 s/s in
windows vs 1.3 out, 71.3), which slicing does not touch. So the ~100 ms criterion is uncertain. Matched/s:
no change expected (exec-bound, exec queue 60 of 64, 74.1). Costs: W checkpoint wall +0.2-0.5 s **(estimate)**,
disk writes +~200 MB per checkpoint (~+13 MB/s per node, vs 34 MB/s state DB in option A), CPU < +0.5%
(memcpy + crc of 200 MB per 15 s), one WAL fsync per checkpoint on W.

Covers: EVERY state-DB writer (consensus thread, trade writer, pruner, post-flush writer, barrier writes).
Code: ~500 lines production (torus-state sink + redo + recovery, W job, boot hook) + ~700 test lines.
Risk: medium. New on-disk artifact and boot step; the proof rests on key disjointness and blind writes, both
testable; the fence's cost on other writers is unmeasured (the existing `TORUS_SYNC_WAL_ON_COMMIT` does one
per commit on the consensus thread; this is one per 15 s on W).

### A2. Slices + an undo log (pre-images)
Before each slice, read and log the S-values of its keys; boot rolls partial slices back to D_S. Correct, but
reads ~1.4M keys per checkpoint from RocksDB (**estimate** 1-3 s of point reads on W) and writes ~the same bytes
again. Strictly worse than A1 (A1 logs what it already has in memory). Rejected.

### A3. Versioned keys (RocksDB user-defined timestamps)
rust-rocksdb exposes them (`Options::set_comparator_with_ts` db_options.rs:1721, `put_cf_with_ts`,
`ReadOptions::set_timestamp`, `increase_full_history_ts_low`). Write slices at ts = T, read everything at
ts = marker: partial slices are invisible to replay and to every DB reader, atomic visibility for free. Cost:
comparator change on every state CF (new DB format, no in-place migration), every read and iterator in ~40 RPC
call sites and all state code gets a timestamp, tombstone GC via `full_history_ts_low`. XL, high risk. Rejected
for Q6.

### A4. Slices with no recovery log, refuse to start on a partial checkpoint
A sentinel in B1 says "checkpoint T in progress"; boot finds it and refuses. Safe but needs a state sync we do
not have (snapshots have no producer). Rejected.

### B1. Commit manifest into the consensus DB
Move `CF_COMMIT_MANIFEST` to the option-A consensus DB (`StateDb::open_consensus`, db.rs:682): `persist_commit_manifest`
(app.rs:1709) and its readers (`manifest_heights_above` :1726, `load_commit_manifest` :1747) follow; the manifest
delete in `persist_committed_block_durably`'s batch (app.rs:1629-1640) becomes a lazy delete below the applied
marker (a cross-DB delete in that batch would break power-loss ordering). Crash: manifest and HotStuff
`APP_FED_BLOCK_HEIGHT` are then in one WAL again (better than today); manifest vs body is cross-DB, but the
manifest is only deleted below the marker, so a lost body still has its manifest. Readers: boot recovery only.
Effect: removes the measured +25 ms per call (the `on_committed_block` criterion), NOT the 0.65-0.97 s
`persist_committed_block_durably` waits, nor other writers. Covers: the consensus thread's manifest put only.
Code: S-M (~150 lines + layout rule in `node_dbs::check_layout`: an old dir has manifests in the state DB, so a
one-time move or a refusal). Risk: low-medium.

### B2. All consensus-thread commit writes into the consensus DB
B1 plus `CF_BLOCK_HEADERS` / `CF_BLOCK_BODIES`. The consensus thread then never writes the state DB. Readers to
move: boot replay (`find_last_committed_height`, `load_replay_header/body`), `detect_conflicting_commit`,
RPC block / receipt reads, block-sync serving, the pruner, the exec-time header/body rewrite (`DurableRows`),
EVM `commit_block_metadata`. Coupling: since Codex D12 r2 the serial flush puts the block's header in the SAME
batch as the marker ("neither can lag the marker", app.rs:2958-2960; also app.rs:1285, :3113); moving headers out breaks that and
needs a new argument (header in consensus DB written before the marker; process kill ordered, power loss
weakened, as option A accepted). Effect: covers both consensus-thread state writes (the criterion and the rare
full waits); not the trade writer / pruner. Code: L (~25 call sites, layout change: wipe or migration).
Risk: medium-high (block-record readers everywhere).

### B3. Consensus-thread state writes on their own writer thread
Not workable alone: the manifest must be in the WAL before HotStuff advances `APP_FED_BLOCK_HEIGHT` (s121 audit),
so the consensus thread would wait for the writer thread, i.e. for the same queue. Rejected.

### C1. `unordered_write`
Exposed (`set_unordered_write`, db_options.rs:2497). Writers do the WAL in order, then insert concurrently, so a
small writer would skip mechanism 2. But: incompatible with pipelined write (`rocksdb/db/db_impl/db_impl_open.cc:270`,
our default since 9.13); mechanism 3 still waits for all pending memtable writes, and in unordered mode it does
so holding the DB mutex (`db_impl.h:2299-2307`), which the 200 MB batch triggers every window; snapshots lose
immutability (affects `scan.rs`). Crash safety unchanged (one WAL record). Effect: unknown, likely small for the
switch case. Code: S (flag). Risk: medium (DB-wide behaviour change). Not recommended; only if step 0 shows the
insert stage dominates and no switch wait.

### C2. Low priority / rate limiting the checkpoint write
`WriteOptions::set_low_pri` (db_options.rs:3792) acts only when compaction is behind (write controller); the
stall counters are 0, so it never engages. The rate limiter covers flush / compaction IO; per-write
`rate_limiter_priority` is not in the C API. Throttling the batch would make it hold the queue LONGER. Rejected.

### C3. Other RocksDB knobs
- Pipelined write: already on.
- `max_write_batch_group_size_bytes`, `two_write_queues`, TransactionDB write policies (WRITE_UNPREPARED would
  be RocksDB-native sliced atomic writes): not in rust-rocksdb 0.24 / the C API (transactions/options.rs has no
  write policy). Rejected.
- SST ingestion of the checkpoint (`ingest_external_file_cf_opts`): ingestion blocks all writers and flushes an
  overlapping memtable first; positions always overlap. Worse. Rejected.
- `set_memtable_insert_hint_per_batch` (db_options.rs:3805): may speed the sorted insert; does not remove the
  queue. A micro-bench at most.

### D1. A1 with WAL-off slices (the redo file is the log)
Write slices with `disable_wal(true)`: disk bytes back to today's, and slices skip the WAL stage. But then slice
rows are durable only after a memtable flush, the marker can reach disk before the rows (different CFs flush
independently), and the deletion fence becomes "all touched CFs flushed" (forced flushes or seqno tracking not
exposed). Recovery must apply the retained redo chain in order. Correct with care; more complex. Follow-up only if
Q6 shows A1's extra IO matters.

### D2. State DB holds state rows only
B2 plus trade rows and action status in a "chain DB". The HL-like end state (2). Biggest change; not for Q6.

### Summary
| option | covers | crash safe | reader impact | size | risk |
|---|---|---|---|---|---|
| **A1 slices + redo** | every state-DB writer | yes (proof above) | DB-direct readers mixed ~1 s/window | M | medium |
| A2 slices + undo | every writer | yes | same as A1 | L, + reads | high cost |
| A3 user timestamps | every writer | yes | none (atomic) | XL | high |
| A4 refuse on partial | every writer | no recovery | - | S | unacceptable |
| B1 manifest to consensus DB | manifest put only | yes | none | S-M | low-medium |
| B2 commit records to consensus DB | consensus thread | weakens D12 r2 | block readers move | L | medium-high |
| B3 writer thread | - | breaks ordering | - | S | rejected |
| C1 unordered_write | insert stage only | yes | snapshots weaker | S | medium, likely small gain |
| C2 low_pri / rate limit | none | yes | none | S | counterproductive |
| D1 A1 + WAL-off slices | every writer | yes, harder | as A1 | M+ | medium-high |

## 5. Recommendation
Build **A1** behind `TORUS_CKPT_SLICE_MB` (unset / 0 = today's single batch byte for byte, the redo path unused;
boot recovery always runs and is a no-op without redo files). Reasons:
- It is the only option that bounds the wait for every state-DB writer, which section 74 says is where the rest
  is; B1 / B2 cover the consensus thread only, and B1 misses the 0.65-0.97 s persist waits.
- No state format, CF or DB layout change (B1 / B2 need a layout rule; A3 a new format).
- The crash argument is short and testable: blind writes + disjoint keys + redo fsynced before B1 + fence
  before delete. E and the ingress view are already shielded by C.
- Correct with `TORUS_EPOCH_FAST_PATH` and `TORUS_CKPT_DEFER_BOOKS` on or off (neither changes the batch).

Step 0, in the same build (cheap, informs the slice size and C1): time the checkpoint write's stages on W only
with RocksDB's thread-local perf context (`rocksdb::perf::set_perf_stats`, `PerfMetric::WriteWalTime`,
`WriteMemtableTime`, `WritePreAndPostProcessTime`, `DbMutexLockNanos`; rust-rocksdb perf.rs:73-77, 116), plus
histograms for redo write, fence and per-slice write. Export them to the sampler.

Q6 form: a **flag A/B on one binary** (arm A slice 0, arm B slice 4 MiB), so build noise is out and n = 4 per arm
ABBA fits (74.6: 4-5 per arm resolves ~200 ms). Other flags at the Q1 / Q4 defaults settled by then, same in both
arms. Success as in bench-queue Q6, plus: no rise in `torus_exec_checkpoint_wait_seconds` / checkpoint hand-off
waits, flush-worker checkpoint wall reported, Dirty / Writeback max in windows reported (redo IO).

### Test plan (tests first: each RED before the code)
torus-state (`cargo nextest run -p torus-state $F`):
1. `sliced_checkpoint_equals_one_batch_random`: random layer + side rows + chain rows, trie maintenance on and
   off, slice sizes {1 op, 1 KiB, 4 MiB, off}: full CF dump equals today's one-batch dump.
2. `sliced_checkpoint_never_cuts_a_market_prefix_and_keeps_the_tail_last`.
3. `crash_after_every_slice_recovers_to_the_full_checkpoint`: for k = 0..=n: redo + B1..Bk, drop the DB without
   finishing, reopen, recover: dump = D_T, marker T, running hash rows = one-batch run.
4. `torn_redo_is_ignored`: `.tmp` truncated at every record boundary and with a flipped byte: recover leaves D_S.
5. `renamed_redo_with_bad_checksum_or_wrong_base_fail_stops`; `stale_redo_is_deleted_when_marker_reached_target`.
6. `redo_deleted_only_after_wal_sync` (`wal_sync_count` ticker before the unlink, as `consensus_db_tests.rs`).
7. `concurrent_writers_during_slices_survive_recovery`: a thread writing header / body / manifest / trade rows
   between slices; crash at every k; recover: both sets present. Plus `checkpoint_keys_disjoint_from_other_writers`
   (the audit pinned).
torus-consensus (`cargo nextest run -p torus-consensus $F`, then `rdeps(torus-state)`):
8. W gate seam `hold_after_slice(k)`: parked mid-checkpoint, E's overlay and the published view return T-values
   for every key of C, `durable_height` = S, C still held, DB marker = S; release: durable T, C dropped.
9. App crash-replay: extend `checkpoint_crash_replay_with_cloids_matches_uncrashed_{classic,order_rows}`
   (app.rs:28014-28019) with kills between slices: full dump + running hash equal to an uncrashed run, for
   `TORUS_EPOCH_FAST_PATH` {0,1} x `TORUS_CKPT_DEFER_BOOKS` {0,1} x trie maintenance {off,on} x slice {0, small}
   (nextest runs each test in its own process, so the once-read env flags are per test).
10. Boot order: a pending redo is applied before `replay_committed` reads the marker; with EVM addresses, the
   resync runs.
11. Epoch fast path + slicing: the boundary+L plan becomes readable after the sliced boundary checkpoint lands.
torus-node: `sigterm_shutdown.rs` (real binary) with slicing on: shutdown leaves no redo; a SIGKILL during
slices (test-only slow-slice env) restarts, recovers and agrees on the state hash.
Before commit: `cargo nextest run --workspace $F` + doc tests (TESTING.md).

## 6. Owner questions
1. **Which option to build?** (a) A1 slices + redo; (b) B1 manifest to consensus DB (smallest, consensus-thread
   manifest only); (c) B2 all commit records to consensus DB; (d) A1 now, B1 later only if Q6 leaves the
   manifest wait. **Pick: (a).**
2. **Power-loss fence before deleting a redo:** (a) `sync_wal` on W at the start of the next checkpoint (one WAL
   fsync per 15 s); (b) every K = 4 checkpoints, keeping up to 4 redo files (~800 MB); (c) no fence (power loss
   could leave a divergent DB: not acceptable). **Pick: (a), measured by the step 0 fence histogram; (b) if it
   shows up in other writers' waits.**
3. **Extra disk writes (+~200 MB per checkpoint, ~+13 MB/s per node):** (a) accept for Q6; (b) build D1 (WAL-off
   slices) first. **Pick: (a); D1 only if Q6 shows IO cost.**
4. **Validator RPC during slicing (mixed S / T across keys for ~1 s per window, per-key monotone, book scans
   whole per market):** (a) accept and document next to the existing lag note; (b) pin a RocksDB snapshot for
   RPC reads while slices land (~40 call sites, L). **Pick: (a).**
5. **Trie maintenance on:** (a) trie ops in the last slice (atomic with marker; less benefit with maintenance on);
   (b) slice them too (mid-window root readers would see a mixed trie). **Pick: (a)** (maintenance is off by default
   and not on the bench).
6. **Slice size and arms:** (a) 4 MiB only; (b) 4 and 16 MiB (one more arm, ~+45 min); (c) step 0 first, then
   pick. **Pick: (a), with the step 0 split reported from the same cells.**
7. **Offline tools on a dir with a pending redo:** (a) read-only opens warn, read-write tools refuse; (b) every
   `StateDb::open` runs recovery. **Pick: (a)** (recovery stays in one place: node boot).
8. **Q6 shape:** (a) flag A/B on one binary, n = 4 ABBA, defaults as settled by Q1 / Q4; (b) commit vs commit
   (main vs branch). **Pick: (a).**
9. **C1 `unordered_write` as an extra arm?** (a) no; (b) build the flag (~20 lines) and smoke it only if step 0
   shows the insert stage dominates with no switch wait. **Pick: (a)** (pipelined is the benched default and the
   switch wait still blocks).
