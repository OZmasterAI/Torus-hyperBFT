# Q6: checkpoint batch slicing (section 71.8 option B): design and options

Status: PHASE 1, design only, revision 2 (s127). Revision 1 (`9898c980`) went through two adversarial reviews
(Codex torus-adversarial: needs-attention, 2 x P1 + 2 x P2; Claude adversarial: build with changes, 7 findings +
missing tests). This revision handles every finding (section 7) and reorders the plan so measurement comes first
(owner s127, section 5). Nothing built beyond Q6 step 1 (the `torus_commit_manifest_seconds` timer, `a1097df3`).
Code anchors are `file:line` on `9898c980` (= `a1097df3` for code; main `dcee5cac` + step 1). RocksDB anchors are
in the vendored source, `rocksdb/` = `~/.cargo/registry/src/index.crates.io-*/librocksdb-sys-0.17.3+10.4.2/rocksdb/`
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
  this is the write queue, not the write controller. Writeback throttling ruled out at Dirty <= 75 MB (71.4, 71.5).
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
    and the consensus-thread pair above. The trade writer and the pruner are background threads: their waits do
    not sit on the block path, so for block p99 the consensus-thread pair is what matters (Claude review 6).
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

All three are bounded by the size of the one batch in flight. That is the hypothesis the waits in 71.3 / 74.3 fit
(they match the checkpoint DB write, 0.48-0.62 s and 0.65-0.97 s). The split between the three (WAL vs insert vs
switch) is NOT measured, and neither is the claim that the residual block-p99 gap scales with batch size. Step 0
and step Z (section 5) measure both before anything is built.

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
exec_pipeline.rs:899). In batch order:
1. The layer's rows (`PendingState::append_to_batch`, backend.rs:672-693): CF by CF in `ALL_CF_NAMES` order
   (cf.rs:283), and **within a CF all puts (sorted), then all deletes (sorted)** (backend.rs:683, :689).
2. W's side rows (action status), same routine.
3. Trie ops (`apply_native_dirty`) or the stale mark `META_NATIVE_TRIE_STALE` (backend.rs:2142).
4. The applied marker = T (backend.rs:2155).
5. The window's kept running-hash rows (`ChainRows::append_to_batch`, running_hash.rs:575; `CF_CONSENSUS_META`).
6. One `target.write(batch)` (backend.rs:2194).

Two facts revision 1 missed (both reviews, verified):
- **The layer itself carries the marker.** The pipelined path puts `META_NATIVE_APPLIED_HEIGHT` = n into every
  block's overlay (app.rs:4244-4248), and empty / non-native blocks are `FrozenPending::marker_only(n)`
  (backend.rs:1099-1112). `CheckpointLayer::merge` keeps every CF (checkpoint_layer.rs:213-226, app.rs:2777-2780),
  so the layer holds marker = T as an ordinary `CF_CONSENSUS_META` row, which step 1 emits at the position of
  `CF_CONSENSUS_META` (cf.rs:319), BEFORE `CF_BOOK_ORDER_ROWS` (:327), `CF_STATE_HASH_VOTES`,
  `CF_BLOCK_ACTION_STATUS`, `CF_NATIVE_LIQUIDATION`, `CF_CORE_WRITER_TX` (:331), the side rows, the trie tail and the
  running-hash rows. In one atomic batch that is harmless (step 4 writes the same bytes again). In slices it is not.
- **Other marker-gated meta rows are layer rows too**: the epoch plan and current set (`CF_CONSENSUS_META`,
  epoch_plan.rs:134, :156, :270, :296), staking and governance meta (staking.rs:1169-1175, governance.rs:1498).
  `epoch_plan` (app.rs:6698) reads the marker first, then the plan.

Every op is a blind `put_cf` / `delete_cf`: no merge operator, no `delete_range` (backend.rs:672,
running_hash.rs:456, :575; the only `delete_range` users are the pruner and `ensure_trade_history_format`, neither in
this batch). Put and delete key sets within a CF are disjoint (backend.rs:664-668), so op order inside a CF does not
change the final state.

If the batch is cut into slices B1..Bn and the process dies after Bk (k < n), the DB holds T-values for the keys
of B1..Bk, S-values elsewhere, and (if the marker is not in B1..Bk) marker S. Boot replay re-executes S+1.. and
reads the T-values: divergence. Power loss with `sync=false` is worse only in degree: the WAL keeps a prefix of
batches (point-in-time recovery), so any prefix of slices can survive.

Readers while slices land (live, no crash):
- E (exec thread) reads own -> R -> parent -> L -> C -> DB (checkpoint_layer.rs:11-12). C, the checkpoint in
  flight, holds every state row of the batch and is dropped only once W reports `durable_height >= T`
  (`checkpoint_read_layers` app.rs:2643-2672; W stores `durable_height` after the whole job, exec_pipeline.rs:760).
  So E never reads a sliced state row from the DB. Shielded today, no change needed.
- The published ingress view (mempool, RPC submit) is blocks above S -> C -> DB (`publish_ingress_view`
  app.rs:2838). Same argument.
- DB-direct readers: validator RPC (`torus.rs`, `eth.rs`; documented to lag, item6-phase3-impl.md step 2),
  `row_book_depth` (scan.rs:745-770, one snapshot per market, rows of `CF_NATIVE_ORDER_BOOKS` only; the depth
  decode reads nothing else, book_reader.rs:423), the pruner (reads the marker, pruner.rs:203), `marker_is`
  (exec_pipeline.rs:1053), consensus's epoch-plan read (marker first, then the plan, app.rs:6698-6722). Today each
  of them sees "every key >= the marker it read". With slices they must still see that, plus a new case: within
  one read pass, some keys at T and some at S, for the ~1 s of slicing. The marker-gated ones (pruner, `marker_is`,
  `epoch_plan`) are only safe if marker = T is visible no earlier than every other row of T.

## 4. Options

### A1. Slices + a redo (intent) file, all consensus meta in the last slice (recommended, after step 0 / Z)
Mechanism (revision 2; changes from revision 1 marked R2):
1. **Build** exactly as today, all in memory, before any write: layer rows, side rows, trie ops or stale mark,
   marker T, running-hash rows (the trie ops read D_S, as today; nobody else writes trie CFs during a window). With
   `TORUS_CKPT_SLICE_MB` unset / 0: today's single batch, byte for byte, no redo, no fence, no other change.
2. **Partition (R2)** into a body and a tail. The `SliceSink` routes:
   - to the **tail** (always the last slice, Bn): every `CF_CONSENSUS_META` row of the layer and side rows except
     `META_NATIVE_APPLIED_HEIGHT`, any trie-CF row (expected none: pipelined blocks are native-only, EVM blocks are
     serial, app.rs:4237), the trie ops or stale mark, the marker T (once), the running-hash rows. The layer's own
     marker row is stripped; the sink checks its value is T (else `JobError::Write` before any write: a code bug).
     So the marker, the running hash, the stale mark, the epoch plan and the staking / governance meta land
     together, after everything else. The tail is small (meta keys) unless trie maintenance is on (owner Q5).
   - to the **body**: everything else, in `ALL_CF_NAMES` order, except that in each `READER_SCANNED_CFS` CF
     (cf.rs:105: `CF_NATIVE_ORDER_BOOKS`, 8-byte market prefix) puts and deletes are merged into ONE key-ordered
     run (disjoint key sets, so the final state is identical; only the sliced path reorders). Cuts go at op
     boundaries every ~`TORUS_CKPT_SLICE_MB`, never inside a scanned-CF market prefix, so each market's puts AND
     deletes land in one slice (a slice can exceed the target by up to one market's rows).
3. **Pre-write, on W, in this order (R2):**
   a. `M0` = the DB marker (`get_cf_raw`). Require `M0 + 1` = the first height of W's window (W records it on the
      first digest; to add). The base is never W's `durable_height`: serial barrier blocks write the marker on E
      (`pipeline_barrier` app.rs:2509-2550 then the serial path, `flush_marker_only` app.rs:2967), so the DB marker
      can be ahead of W's last checkpoint. Mismatch = `JobError::Write` before any write.
   b. **Fence, unconditional, every sliced checkpoint:** sync the state-DB WAL. New `StateDb::sync_own_wal`
      (`flush_wal(true)` on this DB only): `sync_wal` (db.rs:1191-1196) also fsyncs the attached consensus-DB WAL,
      which the redo protocol does not need. This makes durable: M0, the serial barrier blocks before it, and the
      previous checkpoint's slices and EVM resync.
   c. Unlink every `redo-<T'>` with T' <= M0 (covered by b).
   d. Write `<state dir>/ckpt-redo/redo-<T>.tmp`: header {magic, version, base M0, target T, n, the window's EVM
      resync addresses}, per slice {len, crc32c, rep (`WriteBatch::data()`, rust-rocksdb write_batch.rs:206)},
      footer {crc, magic}. Write-behind `sync_data` every 8 MiB (keeps the redo's dirty pages <= ~16 MiB),
      final `sync_data`, rename to `redo-<T>`, fsync the directory. Any IO error (ENOSPC, EIO, failed sync or
      rename): best-effort unlink, `JobError::Write` with no slice written, so the DB is consistent at M0 and the
      node fail-stops exactly as a failed checkpoint write does today.
4. **Slices** B1..Bn, one `db.write` each, in order (each returns before the next is issued, so WAL order = issue
   order). Optional pacing gap after each body slice, `TORUS_CKPT_SLICE_GAP_US` (default 0; owner Q10).
   A write error after k >= 1 slices landed: the DB is mixed until restart, so W latches fail-stop AND the
   process exits with a clear error (R2, owner Q11); a restart applies the redo. k = 0: today's fail-stop.
5. After Bn: caches commit, EVM resync for the window's addresses (as today, exec_pipeline.rs:948), then
   `durable_height = T`, C dropped. `redo-<T>` stays until step 3c of the next sliced checkpoint or shutdown.
6. Shutdown, after `flush_for_shutdown` (main.rs:1703): `sync_own_wal`, unlink every redo with T <= marker.
7. **Boot recovery**, before anything reads state (before `replay_committed`; in the node right after
   `node_dbs` opens the DBs):
   - delete `*.tmp` (never renamed = no slice was submitted);
   - read M. For each `redo-<T>` (a checksum failure on a renamed file = fail-stop):
     - **T <= M (stale), R2:** run the EVM resync for its addresses, `sync_own_wal`, unlink. The resync is
       idempotent and derived from current state (`resync_evm_accounts` recomputes the mirror from CF_ACCOUNTS and
       skips accounts already in sync, incremental.rs:431-456), so it is correct for any M >= T.
     - **T > M, base = M:** apply every slice in order (`WriteBatch::from_data` + `write`), EVM resync,
       `sync_own_wal`, unlink. Now M = T.
     - T > M with base != M, or more than one redo with T > M: fail-stop with a clear error (cannot happen by the
       argument below; a check, never a guess).
   - Then replay from the (new) marker as today. Recovery is idempotent: a kill or power cut at any point of it
     leaves the redo in place, and the next boot reruns it to the same result.

Crash safety (proof sketch, R2):
- **Tail-last invariant.** No slice but Bn holds a `CF_CONSENSUS_META` key. Slices are issued in order and land in
  WAL order, so "DB marker = T" <=> "Bn landed" <=> "every slice of T landed". Hence a redo with T <= M is truly
  complete, and every marker-gated reader (pruner, `marker_is`, `epoch_plan`) sees T only with all of T. (Revision 1
  emitted the layer's marker row mid-batch: a crash after that slice gave M = T, the redo was taken as stale and
  deleted, replay ran from T on a partial T: divergence. Both reviews' P1 / critical.)
- **Apply.** Let D_S be the DB at marker S and B = B1..Bn. For blind puts/deletes, apply(B, X) depends on X only
  through the keys B does not touch. Every crash state X_k = D_S + B1..Bk + (other writers' writes) agrees with
  D_S outside B's keys, PROVIDED no other writer touches B's keys during the window. So apply(B, X_k) = D_T on B's
  keys for every k (k = n: rewrites the same bytes; k = 0: plain apply).
- **Key disjointness** (pinned by a test that fails on overlap): during a window the other state-DB writers are the
  consensus thread (CF_BLOCK_HEADERS / BODIES / COMMIT_MANIFEST), the trade writer (trade CFs), the pruner
  (bodies / action status / receipts / trades below the cutoff <= marker, `PRUNE_META_KEY` in CF_BLOCK_HEADERS
  pruner.rs:487, `META_TRADES_PRUNED_HEIGHT` pruner.rs:501), and the previous checkpoint's EVM resync (mirror / EVM
  trie CFs, finished before step 3 of this one). E writes the DB only after a barrier (`wait_idle`, W idle). The
  batch writes state CFs, state meta keys in CF_CONSENSUS_META, action status for heights > S, native trie CFs.
- **SIGKILL anywhere** (page cache survives, so every completed write survives): (a) during 3a-3d -> at most a
  `.tmp`, no slice, M = M0: delete, replay as today. (b) after the rename, before or between slices -> `redo-<T>`,
  M = M0 = base: apply -> D_T, M = T, resync. (c) after Bn, before or after the resync -> M = T: stale path, resync,
  unlink. (d) during recovery -> rerun.
- **Power loss, `sync=false`.** The WAL recovers a batch prefix. (i) B1 is issued only after the redo's
  `sync_data` + rename + dir fsync: a surviving slice implies a surviving redo. (ii) Step 3b makes M0 durable before
  the redo exists: a surviving `redo-<T>` implies the durable marker is >= its base, so base != M cannot arise from
  a lost barrier write (Claude review 2). (iii) A redo is unlinked only after a WAL sync that followed its Bn and its
  resync: an unlinked redo's slices and mirror are on disk. (iv) An unlink lost to power loss (no dir fsync after
  3c) brings back a stale redo: resync + unlink, harmless. (v) Checkpoint T2's fence and slices come after T's Bn,
  so if T2's redo survives, all of T is durable and M >= T: at most one redo with T > M, and its base is M.
  Same guarantee as today: the node restarts at a consistent, possibly older, checkpoint.
- **EVM mirror (R2).** Today a kill after the checkpoint batch and before `resync_evm` leaves CF_ACCOUNTS ahead of a
  non-empty mirror, and boot's `is_trie_built` (incremental.rs:131-149) only probes existence (memory
  dd3cd11320d1c0d6, issue d48eb561). With slicing on, the redo keeps the addresses until the next fence and every
  boot resyncs them, so the gap closes for sliced checkpoints. Per-block flushes and unsliced checkpoints keep
  today's gap (owner Q12).
- **Running hash / trie / stale mark / epoch plan / governance / pruner keys:** all in Bn (above). Trie ops are
  computed from D_S reads before B1, as today.
- **W errors:** `marker_is` (exec_pipeline.rs:1053) again means "Bn landed" (revision 1 would have reported a
  mid-batch error as "landed, trie stale" once the early marker slice was in).

Reader consistency: E and the ingress view are shielded by C (section 3). DB-direct readers see per-key monotone
values (S or T, never older than the marker they read), mixed across keys for ~1 s per window; marker-gated readers
see T only with all of T. Book scans see each market whole (puts and deletes of a market in one slice). Validator
RPC multi-key answers (a position at T, a balance at S) can be mixed during slicing: new in rate, not in kind
(owner Q4); never for longer than one window, because a mid-slice failure exits the process (Q11). Offline tools
that open the state DB after a crash must see the pending redo: read-only open warns, read-write tools refuse
until the node ran once (owner Q7).

Flags:
- `TORUS_EPOCH_FAST_PATH` (exec_pipeline.rs:64, default off): a boundary checkpoints at its own height
  (app.rs:2768) and consensus reads the boundary+L plan once the durable marker reaches the boundary
  (app.rs:6698-6722). The plan rows now land in Bn with the marker. Slicing delays that durability by the extra W
  wall (**estimate** +0.2-0.5 s); consensus refuses boundary+L until then (liveness only, refuse and retry), which
  only bites if L blocks pass in less than that. Tests run both settings.
- `TORUS_CKPT_DEFER_BOOKS`: E-side only; the layer handed to W is whole after `settle_all` / `take_layer`
  (app.rs:2706-2790), and the flag "never changes what a checkpoint writes" (checkpoint_layer.rs:52-60). No
  interaction; tests run both settings.
- Trie maintenance (`TORUS_NATIVE_TRIE_MAINTENANCE`, off by default): trie ops go in the tail, so with
  maintenance on the last slice is large and slicing helps less (owner Q5).
- `TORUS_SYNC_WAL_ON_COMMIT=1`: the consensus thread fsyncs both WALs per commit while W runs `sync_own_wal`;
  RocksDB serialises WAL syncs internally. Tested together (test 12).
- Pruner: separate thread, keys disjoint, reads the marker (now only T with all of T). No change.

Expected effect (R2; all **(estimate)**): at 4 MiB, ~50 body slices of ~12 ms each at today's 3 ms/MB.
- Waits bounded by about one slice + one memtable switch (the switch waits for pending memtable writers, now one
  slice, not 200 MB): the 0.65-0.97 s `persist_committed_block_durably` waits fall to tens of ms (high confidence).
- `on_committed_block`: W submits slices back to back, so the queue is still ~100% busy for ~1 s per window; a
  small write arriving at random waits ~half a slice plus any switch, ~4-10 ms more than out of window, vs +25 ms
  now. So "in-window = out" is NOT expected without pacing (confidence medium, was "high" in revision 1; Claude
  review 3). A pacing gap g after each slice (Q10) cuts the mean residual roughly by the duty cycle (g = slice
  time: about half), at +n x g of W wall (~+0.6 s at g = 12 ms).
- Remaining state-DB excess (0.21 s/s): mostly gone (medium). In-window block p99 1,370 -> ~1,050-1,200 ms
  (medium-low): the unattributed rest of the gap may be CPU / IO (rocksdb-bg flush and compaction threads wake
  12.5 s/s in windows vs 1.3 out, 71.3), which slicing does not touch. Step Z tests this premise first.
- Matched/s: no change on pipelined blocks (exec-bound, exec queue 60 of 64, 74.1), BUT every barrier block
  (EVM blocks, slashes, epoch boundaries with the fast path off, non-durable rows; `pipeline_barrier`
  app.rs:2509) waits for `checkpoint_drain` + `wait_idle`, i.e. for the whole sliced checkpoint: +0.2-0.5 s per
  barrier on top of today's 0.84-0.89 s. The Q6 A/B reports barrier counts and their exec time (Claude review 5).

Costs (R2; **(estimate)**): W checkpoint wall +0.2-0.5 s (redo ~200 MB at disk speed with write-behind syncs
0.1-0.3 s, fence one state-WAL fsync, ~50 write calls) + n x g with pacing. Disk writes +~200 MB per checkpoint
(~+13 MB/s per node, vs 34 MB/s state DB in option A). Host Dirty: +<= ~16 MiB from the redo (write-behind), not
+200 MB; the 160 MB throttling point (71.4) is not reached by the redo alone, but the redo, the slices' WAL bytes
and the memtable flush now overlap in one window: step 0 / Z / the A/B sample Dirty and Writeback (Claude review 4).
Other writers write with `sync=false`, so they meet the redo's disk IO only through dirty throttling, or through
WAL fsyncs with `TORUS_SYNC_WAL_ON_COMMIT=1`. CPU < +0.5% (memcpy + crc of 200 MB per 15 s).

Covers: every state-DB writer; the one that matters for block p99 is the consensus thread.
Code: ~600 lines production (torus-state sink + redo + recovery, W job, boot hook, exit on mid-slice failure) +
~1,100 test lines (the power-cut model and fault injection are new). Risk: medium. New on-disk artifact and boot step;
the proof rests on the tail-last invariant, key disjointness and blind writes, all pinned by tests.

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
one-time move or a refusal). Risk: low-medium. Revision 2 note: since A1 without pacing leaves an estimated
+4-10 ms per `on_committed_block` call, B1 is the fallback if the A/B misses that criterion (owner Q1 (d)).

### B2. All consensus-thread commit writes into the consensus DB
B1 plus `CF_BLOCK_HEADERS` / `CF_BLOCK_BODIES`. The consensus thread then never writes the state DB. Readers to
move: boot replay (`find_last_committed_height`, `load_replay_header/body`), `detect_conflicting_commit`,
RPC block / receipt reads, block-sync serving, the pruner, the exec-time header/body rewrite (`DurableRows`),
EVM `commit_block_metadata`. Coupling: since Codex D12 r2 the serial flush puts the block's header in the SAME
batch as the marker ("neither can lag the marker", app.rs:2958-2960; also app.rs:1285, :3113); moving headers out breaks that and
needs a new argument (header in consensus DB written before the marker; process kill ordered, power loss
weakened, as option A accepted). Effect: covers both consensus-thread state writes (the criterion and the rare
full waits); not the trade writer / pruner (background, section 1.1). Code: L (~25 call sites, layout change: wipe
or migration). Risk: medium-high (block-record readers everywhere).

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

### C4. Smaller checkpoints with today's code (step Z, revision 2; Claude review 6)
`TORUS_CHECKPOINT_INTERVAL_MS` (exec_pipeline.rs:116-126, :190-209: whole milliseconds, any value > 0; 0 = per-block
flush, not a checkpoint; unset = role default 15 s on validators) and `TORUS_CHECKPOINT_MAX_BYTES` (:181-184,
:213-226: cap on the layer's bytes, counted from above, app.rs:2750-2773) already exist. Attest heights
(h % 100) still checkpoint (app.rs:2767). Zero production code. Used first as a measurement (section 5, step Z);
it becomes the answer only if it alone meets the Q6 criterion at acceptable cost.

### D1. A1 with WAL-off slices (the redo file is the log)
Write slices with `disable_wal(true)`: disk bytes back to today's, and slices skip the WAL stage. But then slice
rows are durable only after a memtable flush, the marker can reach disk before the rows (different CFs flush
independently), and the deletion fence becomes "all touched CFs flushed" (forced flushes or seqno tracking not
exposed). Recovery must apply the retained redo chain in order. Correct with care; more complex. Follow-up only if
the A/B shows A1's extra IO matters (Dirty / Writeback rule in section 5).

### D2. State DB holds state rows only
B2 plus trade rows and action status in a "chain DB". The HL-like end state (2). Biggest change; not for Q6.

### Summary
| option | covers | crash safe | reader impact | size | risk |
|---|---|---|---|---|---|
| **A1 slices + redo** | every state-DB writer | yes (proof above, R2) | DB-direct readers mixed ~1 s/window, marker-gated readers whole | M | medium |
| A2 slices + undo | every writer | yes | same as A1 | L, + reads | high cost |
| A3 user timestamps | every writer | yes | none (atomic) | XL | high |
| A4 refuse on partial | every writer | no recovery | - | S | unacceptable |
| B1 manifest to consensus DB | manifest put only | yes | none | S-M | low-medium |
| B2 commit records to consensus DB | consensus thread | weakens D12 r2 | block readers move | L | medium-high |
| B3 writer thread | - | breaks ordering | - | S | rejected |
| C1 unordered_write | insert stage only | yes | snapshots weaker | S | medium, likely small gain |
| C2 low_pri / rate limit | none | yes | none | S | counterproductive |
| C4 shorter interval | every writer, smaller batches | yes (today's) | none | 0 | low; less coalescing |
| D1 A1 + WAL-off slices | every writer | yes, harder | as A1 | M+ | medium-high |

## 5. Plan (revision 2: measurement first, owner s127)

### Step 0: checkpoint-write stage timers (build, bench, BEFORE A1)
Small branch commit on `perf/q6-ckpt-slicing`, no behaviour change:
- **W's checkpoint write:** RocksDB thread-local perf context around `target.write` on W only
  (`rocksdb::perf::set_perf_stats(PerfStatsLevel::EnableTime)` before, reset after; rust-rocksdb perf.rs:116;
  metrics perf.rs:73-78: `WriteWalTime`, `WriteMemtableTime`, `WriteDelayTime`, `WritePreAndPostProcessTime`,
  `DbMutexLockNanos`, `DbConditionWaitNanos`), exported as histograms.
- **The consensus thread's two state-DB writes** (manifest put app.rs:7748, `persist_committed_block_durably`
  app.rs:1582): the same perf context per write, split in-window vs out. RocksDB's `write_thread_wait_nanos` and
  `write_scheduling_flushes_compactions_time` (perf_context.h:204, :209) are NOT in rust-rocksdb's `PerfMetric` nor
  the C API (c.h:1996-2045), so the queue wait is the residual: wall - (WAL + memtable + delay + pre/post), with the
  mutex / condition-wait time read as the switch signal. perf_context.h:190-192 warns the split "may be inaccurate"
  with pipelined write: report it as a split, not as exact stage times.
- **Checkpoint start / end timestamps** per node (ms, one log line or a gauge pair per checkpoint), so windows can be
  cut to the DB write itself instead of the scrape-widened windows of 71.6 (needed for step Z).
- **Dirty / Writeback:** the existing 1 Hz sidecar `tools/matched-bench/soak-sampler.py` (host-1hz.tsv:
  Dirty, Writeback, per-node ctx switches) in every step 0 / Z / A/B cell; add a 10 Hz option (tooling only),
  since a window is ~1 s.
- Per-slice, redo-write and fence histograms come with A1 (they have nothing to time before).
- Bench: rides along step Z's control arm (no separate campaign). Output: the WAL / insert / switch / residual split
  of the consensus-thread waits in windows. If the switch dominates, slicing helps less than estimated (each switch
  still waits for one slice) and the memtable sizes are a cheaper lever; if WAL + insert dominate, slicing is aimed
  right.

### Step Z: zero-code arm (shorter interval), on the step 0 binary
- Arms: control `TORUS_CHECKPOINT_INTERVAL_MS=15000` (today's default), Z `=5000`; one binary, ABBA, n = 2 per arm
  first (a screen; extend to 4 if the rule below is ambiguous). Other flags as in Q6.
- **What it can show:** whether a smaller single batch shrinks the waits and the in-window block-p99 excess, i.e.
  whether the residual gap is "bounded by the batch in flight", which is A1's whole premise.
- **What it cannot show:** the redo IO, the fence, back-to-back slice occupancy (Z's batch is still ONE batch,
  ~1/3 the size or more), pacing, mixed reads, barrier cost of a longer W wall. Batches are not exactly 1/3: the
  layer coalesces rewrites of the same key, so 5 s batches are more than 1/3 of 15 s ones and total bytes per
  minute RISE (more WAL, flush, compaction). Windows are 3x as frequent, so overall p99 is not comparable: compare
  the in-vs-out gap, with windows cut from the step 0 timestamps (DB-write span, any node) so enough out-of-window
  time remains (3 nodes x ~0.3 s per 5 s ~ 20-30% busy; scrape-widened windows would cover nearly all of it).
- Measured per arm: batch bytes mean / max and checkpoint DB write s (W_db); manifest put (`torus_commit_manifest_seconds`)
  and persist wait p99 / max and > 250 ms counts in vs out (X); in-vs-out block p99 gap (G); block p99 overall;
  matched/s; state-DB bytes written per s; Dirty / Writeback max in windows.
- **Go / no-go for A1:**
  - GO if W_db falls by >= 40% and X and G each fall by at least half as much in relative terms (e.g. W_db -50% ->
    X and G -25% or more): the waits scale with the batch, A1 is aimed right.
  - NO-GO if W_db falls by >= 40% but X and G move by < 10% or stay inside the noise: the residual is not the batch
    in the write queue; A1 is not built; next lane is CPU / IO in windows (rocksdb-bg wakeups, 71.3).
  - SHIP-Z (stop, propose an interval change instead of A1) if Z alone meets the Q6 criterion (G <= ~100 ms and
    manifest put in = out) with matched/s inside the noise and the state-DB write rate rise accepted by the owner.
  - Anything else (e.g. W_db falls < 40%): extend to n = 4 or try `TORUS_CHECKPOINT_MAX_BYTES` instead, then decide.

### Step A1 (only on GO): build behind `TORUS_CKPT_SLICE_MB`
Unset / 0 = today's single batch byte for byte, redo path unused; boot recovery always runs and is a no-op without
redo files. Reasons:
- It bounds the wait for every state-DB writer, including both consensus-thread writes (B1 covers one, B2 is L and
  weakens D12 r2). That the trade writer and pruner are covered too is secondary (background threads).
- No state format, CF or DB layout change (B1 / B2 need a layout rule; A3 a new format).
- The crash argument is short and testable: tail-last + blind writes + disjoint keys + fence before the redo +
  redo fsynced before B1 + WAL sync before unlink. E and the ingress view are already shielded by C.
- Correct with `TORUS_EPOCH_FAST_PATH` and `TORUS_CKPT_DEFER_BOOKS` on or off.

### Q6 A/B
A **flag A/B on one binary** (arm A slice 0, arm B slice 4 MiB, gap 0), n = 4 per arm ABBA (74.6: 4-5 per arm
resolves ~200 ms). Other flags at the Q1 / Q4 defaults settled by then, same in both arms. Success as in bench-queue
Q6, plus reported: `torus_exec_checkpoint_wait_seconds` and checkpoint hand-off waits (no rise), flush-worker
checkpoint wall, redo write / fence / per-slice histograms, barrier block count and exec time per arm, epoch-boundary
refusals with the fast path on, Dirty / Writeback max in windows. Follow-ups decided from it: pacing (Q10) if
`on_committed_block` in - out > ~5 ms; D1 if Dirty max in windows exceeds ~120 MB, Writeback max exceeds ~100 MB,
or the redo write + fence p50 exceeds 0.3 s.

### Test plan (tests first: each RED before the code)
torus-state (`cargo nextest run -p torus-state $F`):
1. `sliced_checkpoint_equals_one_batch_random`: random layers AND app-shaped layers (marker row in every block,
   `marker_only` empty blocks incl. an empty LAST block and a window of only empty blocks, epoch plan / governance
   meta rows, book puts and deletes in one market), side rows, chain rows, trie maintenance on and off, slice sizes
   {1 op, 1 KiB, 4 MiB, off}: full CF dump equals today's one-batch dump.
2. `only_the_last_slice_holds_consensus_meta` (marker, chain rows, stale mark, epoch plan, staking / governance meta)
   and `layer_marker_other_than_target_is_rejected_before_any_write`.
3. `book_cf_slices_are_market_contiguous`: a snapshot after every slice: each market's `CF_NATIVE_ORDER_BOOKS` rows
   are all-S or all-T (puts and deletes of one market).
4. `crash_after_every_slice_recovers_to_the_full_checkpoint` on the app-shaped layers of test 1: for k = 0..=n,
   redo + B1..Bk, drop the DB, reopen, recover: dump = D_T, marker T, running hash rows = one-batch run; marker = S
   in the DB for every k < n.
5. `torn_redo_is_ignored` (`.tmp` truncated at every record boundary, flipped byte); `renamed_redo_with_bad_checksum_or_wrong_base_fail_stops`.
6. `stale_redo_runs_evm_resync_before_unlink`: M = T, mirror behind CF_ACCOUNTS (kill after Bn, before resync):
   after recovery the mirror equals a full rebuild and the incremental root equals the full root; the redo is
   unlinked only after the WAL sync.
7. `recovery_is_restartable`: kill at every recovery step (after each applied slice, after the resync, before the
   sync, before the unlink), rerun: same dump.
8. `power_cut_at_every_event` (model test on real RocksDB): redo IO goes through a `RedoFs` trait; the test
   `PowerCutFs` keeps synced vs written bytes per file and synced vs pending create / rename / unlink per directory;
   the DB target records batches and WAL syncs. For every cut point and every allowed survivor set (WAL: any batch
   prefix that includes everything before the last WAL sync; files: unsynced bytes dropped or kept as a prefix;
   unsynced directory ops each kept or dropped), materialise on a real DB copied from D_S, run recovery: the dump is
   D_S or D_T (plus surviving other writes), never mixed, and fail-stop never fires. Includes a serial barrier
   window between two checkpoints (base = DB marker), two checkpoints back to back, and a cut during recovery.
9. `wal_tail_loss_is_a_batch_prefix` (validates test 8's WAL model on our options: `sync=false`, point-in-time): a
   child process writes slices and is SIGKILLed; the parent copies the dir, truncates the live WAL at every record
   boundary and mid-record, reopens: the state is a batch prefix. Variant with `set_manual_wal_flush(true)`
   (db_options.rs:3249): unflushed WAL is lost on kill, only synced batches survive.
10. `redo_io_errors_write_no_slice`: ENOSPC / EIO injected at write, `sync_data`, rename, dir fsync -> error, no slice
    written, DB at M0, `.tmp` removed at boot; a failed fence sync -> no unlink, no redo.
11. `fence_runs_before_every_redo_write`: recorded order sync(state WAL) -> unlinks -> redo write / sync / rename /
    dir fsync -> B1..Bn on every sliced checkpoint, also when no older redo exists; `sync_own_wal` does not sync the
    consensus DB.
12. `concurrent_writers_during_slices_survive_recovery` (header / body / manifest / trade rows between slices, kill at
    every k) with a second thread running `sync_wal` per commit (`TORUS_SYNC_WAL_ON_COMMIT=1` shape);
    `checkpoint_keys_disjoint_from_other_writers` (the audit pinned).
torus-consensus (`cargo nextest run -p torus-consensus $F`, then `rdeps(torus-state)`):
13. W gate seam `hold_after_slice(k)`: E's overlay and the published view return T-values for every key of C,
    `durable_height` = S, C held, DB marker = S for every k < n, pruner cutoff <= S, `epoch_plan` still refuses;
    release: durable T, C dropped.
14. `sliced_checkpoint_after_a_barrier_uses_the_db_marker_as_base`: serial barrier blocks advance the DB marker past
    W's `durable_height`; the next redo's base = DB marker; an injected mismatch fail-stops with no slice written.
15. App crash-replay: extend `checkpoint_crash_replay_with_cloids_matches_uncrashed_{classic,order_rows}`
    (app.rs:28043-28048) with kills between slices, after Bn before the resync, and during recovery, and with an empty
    last block: full dump + running hash equal to an uncrashed run, for `TORUS_EPOCH_FAST_PATH` {0,1} x
    `TORUS_CKPT_DEFER_BOOKS` {0,1} x trie maintenance {off,on} x slice {0, small} (nextest runs each test in its own
    process, so the once-read env flags are per test).
16. Boot order: a pending redo is applied before `replay_committed` reads the marker; its EVM addresses are resynced.
17. Epoch fast path + slicing: the boundary+L plan becomes readable only once Bn of the sliced boundary checkpoint lands.
18. `mid_slice_write_failure_exits`: an injected write error at k >= 1 takes the exit path; at k = 0 today's fail-stop.
torus-node: `sigterm_shutdown.rs` (real binary) with slicing on: shutdown leaves no redo; a SIGKILL during slices
(test-only slow-slice env) restarts, recovers and agrees on the state hash; a SIGKILL during boot recovery, then a
second boot, recovers.
Limit: no test cuts real power. Tests 8-9 cover the ordering under the RocksDB / POSIX durability model; a
dm-flakey or VM power-cut run is not planned.
Before commit: `cargo nextest run --workspace $F` + doc tests (TESTING.md).

## 6. Owner questions
**Decided (s127): the owner accepted all nine picks below.** Revision 2 keeps them; where the review findings changed
the mechanism under a pick, the note says how.
1. **Which option to build?** (a) A1 slices + redo; (b) B1 manifest to consensus DB (smallest, consensus-thread
   manifest only); (c) B2 all commit records to consensus DB; (d) A1 now, B1 later only if Q6 leaves the
   manifest wait. **Pick: (a). Accepted.** R2: A1 is built only after step 0 + step Z say GO (section 5).
2. **Power-loss fence before deleting a redo:** (a) WAL sync on W at the start of the next checkpoint (one WAL
   fsync per 15 s); (b) every K = 4 checkpoints, keeping up to 4 redo files (~800 MB); (c) no fence (power loss
   could leave a divergent DB: not acceptable). **Pick: (a). Accepted.** R2: unconditional on every sliced
   checkpoint, BEFORE the redo write (also covers serial barrier writes), and state-DB WAL only (`sync_own_wal`).
3. **Extra disk writes (+~200 MB per checkpoint, ~+13 MB/s per node):** (a) accept for Q6; (b) build D1 (WAL-off
   slices) first. **Pick: (a); D1 only if Q6 shows IO cost. Accepted.** R2: the D1 trigger is now numeric (section 5).
4. **Validator RPC during slicing (mixed S / T across keys for ~1 s per window, per-key monotone, book scans
   whole per market):** (a) accept and document next to the existing lag note; (b) pin a RocksDB snapshot for
   RPC reads while slices land (~40 call sites, L). **Pick: (a). Accepted.** R2: "whole per market" now holds
   (merged put / delete order); mixed state never outlives a window (Q11).
5. **Trie maintenance on:** (a) trie ops in the last slice (atomic with marker; less benefit with maintenance on);
   (b) slice them too. **Pick: (a). Accepted.** R2: the tail now also holds all consensus meta rows.
6. **Slice size and arms:** (a) 4 MiB only; (b) 4 and 16 MiB; (c) step 0 first, then pick. **Pick: (a), with the
   step 0 split reported from the same cells. Accepted.** R2: owner s127 moved step 0 (and step Z) before the
   build; the A/B stays 4 MiB only.
7. **Offline tools on a dir with a pending redo:** (a) read-only opens warn, read-write tools refuse; (b) every
   `StateDb::open` runs recovery. **Pick: (a). Accepted.**
8. **Q6 shape:** (a) flag A/B on one binary, n = 4 ABBA, defaults as settled by Q1 / Q4; (b) commit vs commit.
   **Pick: (a). Accepted.**
9. **C1 `unordered_write` as an extra arm?** (a) no; (b) build the flag and smoke it only if step 0 shows the
   insert stage dominates with no switch wait. **Pick: (a). Accepted.**

New in revision 2: **decided (owner s127): all five picks accepted (10 b, 11 a, 12 a, 13 a, 14 a).**
10. **Slice pacing:** (a) no gap; (b) build `TORUS_CKPT_SLICE_GAP_US` (default 0, ~15 lines) and run one follow-up
    cell pair with a gap only if the A/B leaves `on_committed_block` in - out > ~5 ms; (c) a gap arm in the A/B
    itself. **Pick: (b). Accepted.** (the estimate says the criterion is likely missed without it, but a gap costs barrier
    latency; measure first).
11. **Sliced write fails after >= 1 slice landed:** (a) fail-stop and exit the process, restart applies the redo;
    (b) stay up fail-stopped and make DB-direct RPC refuse (~40 call sites); (c) as today (stay up, serve the mixed
    DB indefinitely). **Pick: (a). Accepted.** (one line in the W error path, the redo makes the restart exact).
12. **EVM-resync gap outside sliced checkpoints** (per-block flushes and unsliced checkpoints, issue d48eb561,
    pre-existing): (a) out of Q6 scope, its own issue; (b) write a small resync-intent file for every flush with EVM
    addresses. **Pick: (a). Accepted.**
13. **Barrier-triggered checkpoints:** (a) slice them like any other (one code path; +0.2-0.5 s per barrier block,
    reported by the A/B); (b) write them as one batch (barrier latency as today, but the consensus thread waits for
    the full batch again at each barrier). **Pick: (a). Accepted**, switch to (b) only if barrier block exec p99 rises
    measurably in the A/B.
14. **Step Z rule and size:** (a) n = 2 per arm screen with the GO / NO-GO / SHIP-Z rule of section 5; (b) n = 4
    per arm up front. **Pick: (a). Accepted**, extended to n = 4 only if ambiguous.

## 7. Review findings and how the design handles them
Reviews of revision 1: Codex `reviews/codex-q6-design-adv.md` (C1-C4), Claude `reviews/q6-claude-review.md` (R1-R7 +
missing tests). All verified in code at `9898c980`; all accepted. Disagreements are noted.

| finding | verified | handling (section) | tests |
|---|---|---|---|
| C1 / R1 (P1 / critical): the layer carries marker = T (every pipelined block's overlay app.rs:4244-4248, `marker_only` backend.rs:1099-1112, merge keeps all CFs checkpoint_layer.rs:213-226), emitted mid-batch in `ALL_CF_NAMES` order (cf.rs:319 before :327-331); a crash after that slice deletes the redo as stale; also misleads pruner / `marker_is` / `epoch_plan` live | yes | all `CF_CONSENSUS_META` rows to the tail, layer marker stripped and checked = T; tail-last invariant (A1.2, proof) | 1, 2, 4, 13, 15, 17 |
| C2 / R7 (P1 / low): stale-redo deletion skips an unfinished EVM resync | yes (exec_pipeline.rs:948, :1069-1080; incremental.rs:131-149) | every retained redo's addresses resynced at boot, also when T <= M, WAL sync before unlink; recovery restartable (A1.7) | 6, 7, 16 |
| C3 (P2): puts before deletes per CF (backend.rs:683-690), so a market's rows are not contiguous | yes | scanned CFs emitted as one merged key-ordered run, cut only between markets (A1.2); depth decode reads only those rows (book_reader.rs:423) | 3 |
| C4 (P2): drop/reopen and SIGKILL keep cached writes; a sync counter does not prove ordering | yes | `PowerCutFs` + recorded-WAL model over every cut point and survivor set on real RocksDB; WAL-tail truncation and `manual_wal_flush` tests validate the WAL model; IO-error injection; recovery cuts. Real power cut not tested (limit stated) | 8, 9, 10, 11 |
| R2 (high): serial barrier blocks advance the marker without W (app.rs:2509-2550, `flush_marker_only` :2967) | yes | base = DB marker checked against W's window; fence unconditional and before the redo write (A1.3a-b, power-loss (ii)) | 8, 11, 14 |
| R3 (high): back-to-back slices keep the queue busy; "in = out, high confidence" unsupported | agreed | confidence lowered to medium, residual +4-10 ms per call estimated; pacing knob as a measured follow-up (Q10); B1 named as the fallback | A/B |
| R4 (medium-high): redo IO in the window; `sync_wal` also fsyncs the consensus WAL (db.rs:1191-1196) | yes for the sync; partly for Dirty | write-behind keeps the redo's Dirty <= ~16 MiB (not +200 MB); fence uses a state-only WAL sync; Dirty / Writeback sampled in step 0 / Z / A/B with a numeric D1 trigger | 11, A/B |
| R5 (medium): longer W wall costs every barrier block and the epoch fast path; mid-slice failure leaves RPC on a mixed DB | yes (`pipeline_barrier` waits `checkpoint_drain` + `wait_idle`; `latch_failstop` exec_pipeline.rs:809 does not exit) | costs added (section 4 A1 expected effects); barrier counts / exec time and boundary refusals in the A/B; exit on mid-slice failure (Q11); barrier slicing pick (Q13) | 17, 18 |
| R6 (medium): zero-code alternative and measurement first | yes (knobs exec_pipeline.rs:118-226) | option C4; step 0 then step Z with a GO / NO-GO / SHIP-Z rule before any A1 code (section 5). Agreed that trade writer / pruner coverage is secondary | - |
| R missing tests: app-path layer; marker only in Bn; base after a barrier; fence before redo; second crash in recovery; ENOSPC / IO error; simulated power loss; `TORUS_SYNC_WAL_ON_COMMIT=1` with the fence | - | tests 1/4, 2, 14, 11, 7, 10, 8-9, 12 | - |
