# r1 lens storage-durable: ideas (novel-s80)

Base: main `eb993e3f`, read-only. Numbers come from `~/bench-results-matched/ozarchy-9j2-{std,deep}-r4-CL21/metrics-after-val0.txt`
(r2 cross-checked), plus map/measured.md and map/state.md. "Chain" = `torus_exec_chain_seconds` per native block:
std 266.2 s / 6939 = 38.4 ms, deep 272.7 s / 7548 = 36.1 ms. Memory check: search_knowledge found no prior "exec does not
wait on the ticket", "shared CF maps" or "nonce high-water" idea. Closest: 020b56cd (merge-on-E root cause; the open item
there is "concurrent-read L") and 2826d2dbc (T5.3 design).

## Key finding: end_resident gates the block boundary

E's storage waits per native block:

| wait | std | deep |
|---|---|---|
| end_resident_wait | 10.63 s / 6939 = 1.53 ms | 9.58 s / 7548 = 1.27 ms |
| commit-persist ticket | 2.54 s = 0.37 ms | 6.57 s = 0.87 ms |
| ckpt settle | 1.19 s = 0.17 ms | 1.31 s = 0.17 ms |
| upkeep, incl. deferred join 1.96 / 2.00 s | 4.61 s = 0.66 ms | 8.23 s = 1.09 ms |
| **total** | **2.73 ms (7.1 % of the chain)** | **3.40 ms (9.4 %)** |

The ordering matters, and the maps do not mention it.
- `end_resident_on_worker` is handed off at app.rs:6434, after the W hand-off (:6254) and the upkeep (:6274).
- It is joined at the next block's `begin_resident` (app.rs:5610).
- So only the next block's prefix overlaps the worker's 4.20 ms (29.14 s / 6939): settle (:4856), ticket wait (:6829), verify 2.66 ms, replay guard 0.55 ms and the sort.

Any cut inside that prefix turns into extra end_resident_wait. This applies to verify (crypto lens), the replay guard and small ticket waits. It also explains why `exec-prep-stage` (move verify into the overlap) recovered only 0.4 ms.

Idea 1 removes the gate. Ideas 2-4 only pay fully on top of it, except for the parts on E's direct path: ticket stalls over 25 ms and the upkeep/close.

---

## 1. end-resident-gate-shrink: hand off before upkeep, borrow the frozen delta, parallel re-value

**Mechanism.**
- (0) Call `end_resident_on_worker` right after `pipeline_handoff(Job::Flush)` succeeds (app.rs:6254), before `checkpoint_upkeep` (:6274). The upkeep then overlaps the worker. A failed upkeep fail-stops anyway.
- (A) In `EndResidentJob::run` (native_executor.rs:5072-5125), stop cloning the delta.
  - Today `BlockDelta::into_delta` calls `FrozenPending::resident_delta`, which copies every position and balance key and value (backend.rs:1025-1034, 1130).
  - Instead, `Users::apply` (users.rs:487) and `ResidentRows::apply` iterate the frozen set borrowed (`cf_stream`, backend.rs:797).
  - Drop `retired`, the delta and the frozen Arc after `done.send(slot)` (:4920-4932), not before it (:5084, :5117).
- (B) Split `Users::apply` into two phases.
  - A parallel read-only phase per trader group: decode the rows, and run `SumsCarry::trader`'s `build_sums` / `sums_with_changes` (:2010-2053). It runs on the exec pool, which is idle during verify.
  - A serial install: map entry, holders, traders, `carry.finish`.

**Target.** E idle per block (end_resident_wait), matched/s, native blk/s.

**Expected gain.**
- Worker time today: 4.20 ms/blk = users 2.60 + rows 0.06 + other 1.53 (the delta copy + frees, `carry.finish`, `retired`).
  - (A) removes ~1.0 ms.
  - (B) takes users 2.60 to ~0.9 ms (4 helpers plus a ~0.5 ms serial install).
  - Worker ~1.6-2.0 ms.
- Overlap after (0): upkeep 0.66 + ticket 0.37 + verify 2.66 + replay 0.55 + sort ≈ 4.5 ms.
- end_resident_wait: std 1.53 → ~0.3 ms (−1.2 ms/blk, −3.2 % of the chain); deep 1.27 → ~0.25 (−1.0 ms, −2.8 %).
- It also leaves ~2.5 ms of slack, so later cuts to verify, the replay guard or the ticket count fully.

**Evidence.**
- Metrics: `torus_exec_end_resident_seconds` 29.14 s, `_users` 18.07 s, `_rows` 0.44 s, `_wait` 10.63 s (std r4).
- Wait histogram: 28 % of blocks > 0.5 ms, 14 % > 4 ms, 84 > 16 ms.
- The hand-off order is in app.rs:6243-6274 vs 6434. The prefix runs at :4856, :5382, :5459-5565 and :5610.

**Nearest prior.**
- `end-resident-on-worker` (merged): moved the work off E; it is now the residual gate.
- `end-resident-reuse-decoded-positions` (merged: `into_cache` reuse).
- `resident-apply-inplace-copy` (no gain: `ResidentRows::apply` copy).
- `exec-prep-stage` (rejected 0.4 ms; explained above).
- `phase5-step2` (excluded: Users mutated in place during exec; untouched here).
- Grepped: end_resident, begin_resident, resident delta, into_delta, rows slot, Users::, copy count.

**Risks.**
- Users must be bit-identical, so the sums carry must stay exact. Gates: users_tests.rs:391 `users_equal_decoded_rows_after_random_deltas` and perf_equivalence_golden.
- Pool helpers can be preempted on a 32-thread shared host.
- (0) must keep the "failed hand-off leaves the slot empty" contract.
- The L3.1 close `try_lock` of the holder (app.rs:4320) must not touch the rows slot.

**Effort.** 4-5 person-days: (0) 0.5, (A) 1, (B) 3.

**Prototype (≤ 3 h).**
- Run ubench_econ with `UB_R_WORKER=1 UB_MARKS=1` at 300 markets. The harness has no inter-block work, so `timer/blk` equals the worker's critical path.
- Record `timer/blk`, `users/blk` and `wait/blk` before and after (A).
- Then (0)+(A) in a 120 s local 300m cell: `end_resident_wait_seconds_sum` / blocks should be ≤ 0.6 ms.

## 2. lazy-commit-persist-ticket: exec stops waiting on the T5.3 ticket

**Mechanism.**
- `execution_loop` blocks on `ticket.wait()` before every block (app.rs:6829-6845). The commit-persist writer shares the state DB write queue with W's checkpoint batch (writer at app.rs:720-760), so it stalls behind 284-447 ms batches.
- The rows are only needed for three things:
  - the pipelined decision (`durable.header && durable.body`, app.rs:4703-4709);
  - the fallback header/body writes (:4652, :6516);
  - the next block's parent read (`parent_block_ms` → `header_at`, which does a DB read plus JSON decode per block, :2976-3000, :3549, :5639).
- The change:
  - `peek` the ticket. If it is unfilled, run the block pipelined speculatively and queue `(height, ticket, header bytes)`.
  - Serve the parent header from an in-memory `last_header`.
  - W waits for all tickets ≤ h before it writes checkpoint h. Any rare not-durable row is folded into that same batch, so the marker never passes its header.
  - EVM, slash and barrier blocks still wait.

**Target.** E stalls, block and exec-age tail, matched/s (deep).

**Expected gain.**
- std: 2.54 s / 6939 = −0.37 ms/blk (−1.0 %). 20 waits > 25.6 ms and 13 > 102 ms disappear.
- deep: 6.57 s / 7548 = −0.87 ms/blk (−2.4 %). 36 waits > 25.6 ms and 16 at 205-410 ms disappear.
- The stall tail (~75-85 % of the sum) is on E's direct path, so it counts even without idea 1. The small waits need idea 1.

**Evidence.**
- `torus_exec_commit_persist_wait_seconds` sums: std r4 2.54 s, r2 4.12 s; deep r4 6.57 s, r2 6.97 s.
- Histogram buckets as above.
- `state_write_db` 21.3 s / 75 = 284 ms (std), 35.7 s / 80 = 447 ms (deep).
- Commit-persist write averages 3.36 / 4.71 ms.

**Nearest prior.**
- `t53-offthread-header-body-writer` (merged): took the write off consensus, but E still waits per height.
- `dispatch-commit-persist-off-state-db` (proposed): about the consensus-thread write.
- `checkpoint-batch-slicing-q6-a1` (excluded): shrink the batch.
- `async-body-persist-off-exec` and `async-post-flush-body` (no gain): moved exec-time writes.
- Grepped: ticket, commit.persist, persist.*exec, header frontier, block_headers.

**Risks.**
- A new crash-safety argument for FIX 1a / T5.3.
- An RPC client may see executed state before the header row lands.
- Back-pressure moves to window granularity: the writer queue can run ≤ ~100 blocks (~68 MB of bodies) ahead.
- Every DB-direct header read on E must be found.

**Effort.** 3-4 person-days.

**Prototype (≤ 3 h).**
- Use the `CommitPersistWriter::delayed(.., 300 ms)` seam (app.rs ~788).
- Run 200 pipelined blocks with blocking vs speculative mode and compare E wall time.
- Check crash safety with the `CommitPersistWriter::held` seam plus reopen: assert the durable marker is never above the header frontier.

## 3. lazy-window-shared-cf-maps: no per-block layer merge

**Mechanism.**
- Today, per block:
  - A NEW `torus-ckpt-merge` OS thread (app.rs:426-458) clones every row into L (`merge_cf`, checkpoint_layer.rs:142-170). Positions are ~92 % of keys.
  - It then clones the ingress slice (backend.rs:1091-1112), which also holds positions and balances.
  - E blocks on the main part (`settle`, :168), on the deferred part (`settle_all`, :196) and on slices (`install_slices`, a blocking recv, :207-225).
  - At a close, E merges two blocks into L (:4111-4116) and floods the 2-slot dropper (:485) with ~100 single discards (:229-242). That likely leaves the old view's free inline on E: `drop_inline` 7460 = 99.5 per close.
- The change:
  - Freeze each CF map into an `Arc` (O(50) per block). The slice becomes 8 Arc clones.
  - Positions, balances and books stay per-block shared maps for the whole window and are never merged. E never reads them via L: `deferred_read_waits` = 0.
  - W does one heap k-way merge of the ≤ 100 maps (newest wins) into the single checkpoint batch at close. This reuses the `for_each_consensus_write` idea from backend.rs:824.
  - E merges only the small CFs inline (~150-300 rows).
  - Close discards go to the dropper as one item.

**Target.** E upkeep, settle and close spikes; deep stability; memory.

**Expected gain.**
- std: settle 0.17 + join 0.28 + close ((4.61 − 1.96 − ~0.76 regular) / 75 ≈ 25 ms → ~4 ms, i.e. 0.23 ms/blk) = −0.68 ms/blk (−1.8 %).
- deep: 0.17 + 0.27 + close ~68 → ~6 ms (0.66 ms/blk) = −1.1 ms/blk (−3.0 %). This also removes 84 upkeeps > 25.6 ms per cell.
- Memory: the L-deferred copy goes away. That is ~75 / 150 MB logical at ~2.1x, so −0.15 / −0.3 GB.
- The ckpt-merge thread (1.8 % of node user cycles, q8re) mostly disappears.

**Evidence.**
- std r4: upkeep 4.61 s, `deferred_join` 1.96 s, settle 1.19 s; all 75 closes fall in the 12.8-25.6 ms upkeep bucket.
- deep r4: upkeep 8.23 s, join 2.00 s; 7 upkeeps > 102 ms.
- Layer average 6.07 GB / 75 = 81 MB (std); batch 13.1 GB / 80 = 164 MB (deep).

**Nearest prior.**
- `merge-wait-deferred-part` / `p3-merge-wait` (merged): the deferred part is still merged each block and joined.
- `layer-per-cf-rwlock-or-persistent-map` (proposed: per-CF locks, move entries, persistent map).
- `ckpt-drop-thread` and `layer-merge-early-handback` (merged).
- `ingress-view-skip-position-slices` (parked: drops the slices, which loses coverage).
- `running-hash-streaming-kway-merge` (merged, hashing only).
- `ckpt-defer-books` (no gain).
- Not A1/Q6: the checkpoint is still ONE atomic batch with identical bytes.

**Risks.**
- A big CkptState refactor.
- Rare reads of the lazy CFs through the window (the R rebuild) probe ≤ 100 maps or use a heap merge.
- The W close build grows by +50-90 ms (durable p50 +1.5-2 %), unless W pre-merges incrementally off the critical path.
- With idea 1's stage (0), the upkeep sits inside the overlap, so this idea needs idea 1 (A)/(B) to count fully. Close spikes count regardless.

**Effort.** 6-8 person-days. The one-item dropper hand-off alone takes 0.5 day and is a safe first step.

**Prototype (≤ 3 h).**
- A torus-state microbench over 100 frozen sets from ubench_econ (~5.5k position rows each).
- Compare (a) `merge_part(Deferred)` + `slice` + `append_to_batch` with (b) Arc maps + heap k-way merge into a `WriteBatch`.
- Measure merge-thread ms/blk, E close ms, W build ms and peak bytes.
- Separately, an app test with 3 closes that checks `drop_inline` and the upkeep histogram with the one-item discard.

## 4. nonce-hwm-db-skip: replay-guard DB-miss shortcut

**Mechanism.**
- Each action's consumed-nonce `get_cf_raw` (app.rs:5511) misses every layer and then pays the RocksDB memtable and L0/level bloom probes (state.md corner 15).
- Nonce keys are time-major (`nonce(8 BE) ‖ sender`, nonce_window.rs) and bounded to ±60 s (eip712.rs:63-71).
- W keeps a node-local `u64` high-water mark: the max nonce prefix live in the DB. It recomputes it with one `SeekToLast` after each checkpoint write, before it publishes the durable height. Any non-W nonce DB write sets it to MAX.
- A nonce above the mark skips the DB read. There is no nonce set and no format change.

**Target.** The replay guard (E prefix).

**Expected gain.**
- 0.55 ms/blk / 63 actions = 8.7 µs per action. The DB miss is about 5-6 µs of that.
- In the bench, the shortcut applies to ~80 % of actions (nonce ≈ submit time, which is newer than the durable window).
- −0.25-0.3 ms/blk (−0.7 %), counted only with idea 1 (it sits inside the overlap).

**Evidence.** `exec_replay_guard_seconds` 3.84 s / 6939 (std), 3.75 s / 7548 (deep).

**Nearest prior.**
- `batch-nonce-replay-guard` (proposed: multi_get + memtable whole-key filter).
- `typed-or-in-memory-nonce-set` (rejected, decision 2a). This idea has no set.
- `nonce-window-d12` (merged).
- Grepped: nonce.

**Risks.**
- Every DB write path for nonces must invalidate the mark: serial/barrier blocks, replay and rpc-only flushes.
- An attacker using a far-future nonce disables the shortcut for ≤ ~2 min; the worst case is today's cost.
- Small gain.

**Effort.** 1.5-2 person-days.

**Prototype (≤ 3 h).** A StateDb microbench with a bench-shaped nonce CF (86k live rows, a checkpoint plus range deletes every 100 blocks): miss cost through the overlay vs the mark skip. Then `exec_replay_guard` in ubench/app.

## Not proposed (already in the ledger or excluded)

- WAL off/compression, SST ingest, memtable insert hint, HW CRC, BlobDB, slicing, interval/cap, change-set log (D5c), position-slice drop, decay tuning.
- Window-close parallel book encode: this would only flip the existing `TORUS_PARALLEL_BOOK_SAVE` at the L6.1 close (batch 10).
- Durable p50 is set by the window: exec 1.15 s + half-window 2.27 s + W 0.38-0.6 s. No novel non-excluded lever moves it by more than a few %. Ideas 2 and 3 only trim E stalls inside the window.
