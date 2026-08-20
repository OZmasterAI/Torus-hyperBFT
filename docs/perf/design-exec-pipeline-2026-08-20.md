# Design — exec pipeline: take the fixed per-block costs off the serial exec thread (2026-08-20)

Branch `perf/matched-200k` @ `1dcf339`. Status: DESIGN, **revision 2** (after two adversarial
reviews; §8 lists every finding and how it is resolved). No production code in this commit.
Prior work this builds on and does not redo: `docs/design-flush-pipeline.md` (async
body_persist, crash-window table, applied-height fence), `docs/plans/async-native-trie-maintenance.md`
(root has no live consumer; Option 0/A/B analysis), `docs/plans/exec-ceiling*.md`,
`docs/perf/matched-200k-campaign-2026-08-18.md` §15–18. All line numbers are as of `1dcf339`;
they drift — re-grep before editing.

Goal restated honestly: the exec critical chain per FULL native block at 10 markets / cap 200 is
**880–1170 ms** (cells below) against an empty-block cadence of ~65 ms. Of that, the engine itself
is 345–708 ms and **cannot be pipelined** (it is the state transition). What can come off the
serial thread is the fixed post-engine tail: `flush` (root ~88 + state_write ~162, of which RocksDB
write ~147), `save_books` (~101–139), plus a self-inflicted `load_books` (59–80 ms/blk of resident-
book rebuilds caused by empty blocks — §1.3). The realistic floor for a cap-200 block at 10 markets
is therefore **~verify + engine + book-drain + residual ≈ 0.8–0.9 s**, not 100 ms; approaching
100 ms requires *thinner blocks at a higher cadence*, which only pays once the per-block fixed costs
are off the chain (§4, candidate `block-cap-resweep-under-pipeline`).

---

## 1. The exec chain today

### 1.1 Threads and order (verified in code)

ONE exec worker thread (`execution_loop`, `torus-consensus/src/app.rs:1852`) receives
`CommittedBlockMsg` from a `sync_channel(EXEC_QUEUE_DEPTH=64)` (`app.rs:2653`) and runs
`execute_committed_block_with` (`app.rs:1161`) strictly serially per committed block. The SAME
function is also called on the **boot thread** by `replay_committed` (`app.rs:2889` →
`replay_gap` `:1027` → `exec_ctx.execute_committed_block`), with `DurableRows::default()`
(header/body NOT dispatch-durable → `fold_header` `:1350` puts the header into the flush batch).

```
skip-check (marker >= h ? conflicting-commit check : continue)        app.rs:1191  reads CF_CONSENSUS_META marker (DB)
parent-link check                                                      app.rs:1219  reads CF_BLOCK_HEADERS (DB); missing row => "cannot assert" => false (:1124-1127)
pending slashes -> self.staking.slash(...)  DIRECT DB WRITE            app.rs:1233
verify: batch_verify_native_actions_cached(get_session via state_db)   app.rs:1366-1384  reads CF_SESSIONS (DB)
replay guard: get_cf_raw(CF_NATIVE_NONCES) per action, via state_db    app.rs:1453
overlay = NativeStateOverlay::new(state_db)  (FRESH per block)         app.rs:1483
fold_header: header -> overlay put CF_BLOCK_HEADERS (replay only)      app.rs:1491
lock resident_books; ctx = NativeExecContext::new_env(overlay, ...)    app.rs:1503-1522  staleness guard reads marker via overlay (native_executor.rs:1514-1532, :1653)
engine: execute_batch x2, drain_core_writer, governance, fees, epoch   app.rs:1537-1564  process_epoch_boundary at :1564 (epoch_length 100_000 in the bench)
save_books: ctx.save_order_books()  (overlay puts)                     app.rs:1597
stash_resident; drop(resident_books)                                   app.rs:1626
nonces -> overlay puts                                                 app.rs:1630
flush_with_native_trie_stats(state_db, Some(h), trie_cache, member_cache)  app.rs:1647-1673 -> backend.rs:622
   = append_to_batch (build) + apply_native_dirty (root) + marker + ONE target.write(batch)
resync_evm_accounts(state_db, overlay.dirty_evm_accounts())  DB->DB   app.rs:1725-1729 -> incremental.rs:410  (NOT a no-op: see §3.1 row 8)
trades -> trade_writer (bg thread, node-local CFs)                     app.rs:1742
body_persist (skipped: durable at dispatch, FIX 1a)                    app.rs:1769
empty/non-native block: write_native_applied_height DIRECT DB WRITE    app.rs:1830
```

The flush batch (`torus-state/src/backend.rs:622-768`) is the crash-safety fence: native state CFs
+ nonces + trie/mirror ops (`CF_NATIVE_TRIE`/`CF_NATIVE_HASHED`) + `META_NATIVE_APPLIED_HEIGHT`
commit in one `WriteBatch` (`:727`). Restart: `replay_committed` reads `find_last_committed_height`
(headers) and the marker, and `replay_gap` re-executes `(applied, committed]` from the
dispatch-time durable bodies. After replay, boot seeds `exec_next_height` from the durable marker
(`app.rs:3949`, `:4244`) and parks manifest heights above it (`:2754`).

Consensus-side dispatch (`dispatch_to_exec`, `app.rs:4448`) is already non-blocking (rank-2
`deferred_exec` parks with a Ready-byte budget); proposer pacing reads
`exec_queue_len + deferred_exec.len()` (`app.rs:4161-4175`) but only scales caps when
`TORUS_EXEC_THROTTLE_WATERMARKS` is set (unset in every campaign cell = no pacing).

The consensus thread also touches root CFs directly: `epoch_validator_set_updates`
(`app.rs:3178-3245`, called from `validate_block :3828` and produce `:3671`) reads
`CF_STAKING_*` and WRITES `apply_pending_rotations` (`:3192`) / `update_validator_statuses`
(`:3235`) at every epoch boundary — today already racing the exec thread's writes with up to 64
blocks of exec lag (§3.1 row 11).

### 1.2 Measured ms per native block (val0, 10 markets, cap 200; `summary.json` `phase_by_node.val0`)

| cell | head | dur | block ms | verify | replay | load_books | engine | save_books | flush (root + sw[build+db]) | residual | busy | wall/native blk | commit interval | matched/s avg |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `r9-merged-confirm-10m-r2` | d7a073f | 300 s | **1168.0** | 27.2 | 4.9 | 0.2 | 708.4 | 130.7 | 261.8 (88.0 + 162.2 [14.8 + 147.4]) | 34.8 | 0.913 | 1279 | 886 | 42,284 |
| `bl-base-10m-r1` | 1dcf339 | 120 s | 789.8 | 23.3 | 4.7 | **80.5** | 344.7 | 101.2 | 207.0 | 28.1 | 0.528 | 1496 | 751 | (idle 12.3) |
| `bl-base-10m-r2` | 1dcf339 | 120 s | 969.2 | 29.9 | 4.6 | **59.4** | 441.4 | 138.6 | 258.5 | 36.6 | 0.828 | 1170 | 372 | 40,498 |

`peak_exec_queue_depth` = 66 in `bl-base-10m-r2`: consensus ran the full 64 slots ahead of exec,
i.e. **the exec thread is the wall at the peak**, and commit cadence (372 ms) is already decoupled
from exec (1170 ms wall per native block) — what is coupled is *throughput*: matched/s =
fills/native-block ÷ exec-chain-per-native-block. Engine ms scales with fills per block (r9: 708 ms
at ~1.3 s of ingress per block; bl-r2: 441 ms with thinner blocks), so **every chain number must
be reported next to fills/native block** (candidate 2) or a thinner-block artefact reads as a win.

The fixed tail we can move: `save_books + flush ≈ 310–390 ms/blk` (27–40 % of the block), plus the
`load_books` 59–80 ms that should be zero. The `residual` (28–37 ms: skip/parent checks, lock
hand-offs, metric observes, trade hand-off) stays on E.

### 1.3 A self-inflicted term: resident-book rebuilds on every empty block (root cause, verified)

`NativeExecContext::new_with_mode` (`torus-bridge/src/native_executor.rs:1514-1532`) reuses the
resident books only if `holder.height + 1 == block_height && marker == holder.height`. The holder is
stashed only by native blocks (`app.rs:1626`); an empty/non-native block writes the standalone marker
(`app.rs:1830`) and never advances the holder, so the next native block fails `+1` and does a full
O(resting depth) reload (`load_books` 59–80 ms/blk average, multi-second stalls, the "13 mid-run
rebuilds" of §11.5 in the campaign doc). `bl-base-10m-r2` committed 461 blocks for 147 native ones —
every native block after an empty one rebuilt. **This gets worse, not better, once cadence is
independent of fullness** (more empty blocks per native block). The fix already exists unmerged:
`cand/r5-resident-books-stale-rebuild-restack-restack-restack` (196166b `advance the rank8
resident-book holder across untouched blocks` + test 4fb0831 + telemetry 4aaef1d; 390 lines, not an
ancestor of `perf/matched-200k`). It is candidate #1 below.

---

## 2. Target pipeline

### 2.1 Who runs what

Two threads per validator in the exec pipeline (plus the existing `trade_writer` /
`post_flush_writer` background CF writers, unchanged):

**E — the exec thread** (today's `execution_loop`), per committed block N. E owns three pieces of
RAM state the serial path does not have: `exec_applied: u64` (the **logical applied height**:
max(durable marker at W construction, every height handed to W or written serially)), `last_job:
Option<Arc<FrozenPending>>` (the pending set of the most recent job handed to W — Flush or Marker —
**always** layered under the next overlay, durable or not), and the W handle.

1. **skip-check uses `exec_applied`, not the DB marker** (`app.rs:1191`): `if exec_applied >= N`
   → `detect_conflicting_commit` (its hash read is `CF_BLOCK_HEADERS`, dispatch-durable — unchanged)
   → skip. The durable marker lags by the depth; a height re-delivered to E while its batch is on
   W must be skipped, not re-executed (review finding F3).
2. **Pipeline eligibility** (all must hold, else **barrier** = `worker.wait_idle()` then today's
   serial path verbatim, on the DB, followed by `exec_applied = N`, `last_job = None`):
   - `TORUS_EXEC_PIPELINE=1` **and the caller allowed it** (`replay_committed` / `replay_gap`
     pass `pipeline_allowed = false`: all replay is serial — F4, F9);
   - `durable.header && durable.body` (dispatch-durable rows; never `fold_header` — a folded header
     would ride a not-yet-durable batch and silently disable `detect_parent_link_violation` for
     N+1, which returns `false` on a missing parent row `:1124-1127` — F9);
   - `has_native && !has_evm && pending_slashes.is_empty()`;
   - `!EpochManager::is_epoch_boundary(N, epoch_length)` (epoch-boundary blocks carry
     `process_epoch_boundary` → inflation credits → `dirty_evm_accounts()` non-empty, and coincide
     with the consensus-thread `CF_STAKING_*` writes; serial — F2, F8);
   - after the engine: if `overlay.dirty_evm_accounts()` is non-empty the job carries the address
     set and W runs `resync_evm_accounts` after the write (§W.2 — belt and braces for credits
     outside a boundary).
   The bench is native-only with `epoch_length` 100 000, so the fast path covers every loaded block;
   production epoch boundaries take the serial path once per epoch.
3. parent-link check (`:1219`) unchanged — reads `CF_BLOCK_HEADERS`, which is dispatch-durable on
   the fast path by rule 2.
4. `overlay(N) = NativeStateOverlay::with_parent(state_db, parent = last_job)` — a
   read-through layer: `get_cf_raw` = pending(N) → parent → DB; `iterate_cf` = DB merged
   under parent merged under pending(N). **The parent is always the previous job's frozen pending
   set, whether or not it is already durable** (a layered read of a durable set returns the same
   bytes the DB does, so this is race-free and removes the `last_pending()/None` optimisation — F1,
   F6). `parent` is `None` only for the first block after W construction / after a barrier.
5. verify (`get_session`) and the nonce replay guard read **through `overlay(N)`**, not
   `state_db` (both are written by native actions of N−1: sessions, `CF_NATIVE_NONCES`).
6. engine on `overlay(N)`; `save_order_books` **pass 1** (journal drain + level digests, which read
   live book levels — `order_book.rs:1617 take_level_ops`) on E; pass 2 (overlay puts) on E in the
   first version (candidate #4 moves it to W).
7. nonces **and the applied-height marker `= N`** put into `overlay(N)` (`CF_CONSENSUS_META` is a
   non-root CF, `native_trie.rs:47-54` — root-neutral). `flush_with_native_trie_stats` keeps
   appending the marker itself — same key, same bytes; the batch has one duplicate put, the
   **final key/value set is identical to today's** (not "byte-identical batch" — corrected).
8. stash resident books; candidate-1 `advance_untouched` stays on E; **freeze `overlay(N)`**
   (no further writes; a `frozen` flag makes `put_cf_raw` panic in debug / error in release).
9. **Hand-off, rendezvous**: `send(Job::Flush{N, overlay(N), evm_addrs})` on a **`sync_channel(0)`**
   (zero capacity: `send` returns only when W has *received* the job, and W calls `recv()` only
   after finishing the previous job, so `send(N)` returning ⇒ W is done with N−1, Ok or latched).
   **Invariant (restated):** when E starts engine(N+1), the set of non-durable pending sets is
   ⊆ {pending(N)}, and `overlay(N+1).parent == pending(N)`. Debug assertion at overlay
   construction: `parent.height ∈ {durable_height, durable_height + 1}`. Blocked time is
   `exec_handoff_wait_seconds`. Before sending, check the W failure latch: a failed `write(N−1)`
   means N was executed on a non-durable base → latch `exec_failed`, fail-stop (restart replays
   from marker N−2). After sending: `exec_applied = N`, `last_job = pending(N)`.
10. Empty / non-native blocks on the fast path: enqueue `Job::Marker{N, pending}` where `pending`
    is a **1-key frozen pending set `{CF_CONSENSUS_META: META_NATIVE_APPLIED_HEIGHT = N}`** that
    participates in the parent chain exactly like a Flush job (so `read_applied_marker` through
    `overlay(N+1)` returns N while the marker write is still in flight — F5, F7). W writes it in
    commit order behind batch(N−1) (today's direct `write_native_applied_height` at `app.rs:1830`
    would otherwise land *before* batch(N−1) and be overwritten by it). `exec_applied = N`.

**W — the flush worker** (new thread, owns `trie_cache`, `member_cache`, the W half of the
metrics), strictly in height order, loop = `recv()` → job → `recv()`:

1. `Flush`: `flush_with_native_trie_stats(overlay(N), state_db, Some(N), trie_cache, member_cache)`
   — build + root + marker + ONE atomic `target.write(batch)` — same key/value set as today. Root
   reads only `CF_NATIVE_TRIE`/`CF_NATIVE_HASHED` + its caches, all owned by W and written only by
   W's previous batch, so root(N) is sequenced after write(N−1) by construction. `Marker`: one
   `put_cf` of the marker (today's `write_native_applied_height`).
2. On `Ok`: if the job carried non-empty `evm_addrs`, run `resync_evm_accounts(state_db, addrs)`
   **here** (DB→DB, sequenced after its own batch — F2); then publish `durable_height = N`
   (AtomicU64 + condvar for `wait_idle`). The frozen pending set is dropped by W; E's `overlay(N+1)`
   holds an `Arc`, so the layer stays readable until N+1 finishes — bounded memory: at most 2
   pending sets alive, ~4 MB each at 10 m, ~12 MB at 300 m.
3. On `Err` (RocksDB write or missing CF): invalidate caches, set the W failure latch, set
   `exec_failed` — **fail-stop**. Today a flush failure logs and continues to N+1 whose engine
   then reads a DB missing N's state (`app.rs:1701-1710`); the pipeline makes this strict. A
   trie-maintenance error alone (`trie_result` Err, write Ok) keeps today's semantics: state
   durable, root stale, caches invalidated.
4. `catch_unwind` around the job, same as `execution_loop`; a panic latches `exec_failed`.
5. `wait_idle()`: block until `durable_height == exec_applied` (or the latch is set). Called by
   every barrier path, by the in-process heal/`replay_gap` path before it starts, by the
   resident-book rebuild path (§3.3), by `Drop` (drain + join, same ordering as `trade_writer`: W
   is dropped with `ExecutionContext` at exec-thread exit, before `TorusApp::Drop` returns), and
   **by the harness digest** (`flush_worker_depth == 0`).
6. **Boot**: W is constructed AFTER `replay_committed` returns (replay is serial by rule 2, so
   the marker equals the replayed top height before `exec_next_height` / manifest parking read it
   — F4). `exec_applied` is seeded from the durable marker at W construction.

Kill switch: `TORUS_EXEC_PIPELINE` (`0`/unset = today's serial path, `1` = pipeline). Proposal:
**default OFF** until the tests in §5 and two agreeing cells exist; flip to ON in a separate
commit with the cell labels in the message.

### 2.2 What the critical chain becomes

Per full native block, E's wall = `verify + replay_guard + load_books(→0) + engine + save_books(drain
[+ pass 2]) + residual + handoff_wait`, where `handoff_wait = max(0, W(N−1) − E_compute(N))`.

| operating point | E compute (from §1.2, incl. residual) | W (root + build + db) | chain | today | Δ |
|---|---|---|---|---|---|
| 10 m, r9 cell shape | 27 + 5 + 0 + 708 + 131 + 35 ≈ **906** (≈ 830–870 with pass 2 on W) | 262 | **E-bound: ~906** | 1168 | −22 % |
| 10 m, bl-base-r2 shape | 30 + 5 + 0 + 441 + 139 + 37 ≈ 652 | 258 | E-bound: ~652 | 969 (incl. 59 load_books) | −33 % |
| 300 m, r9 cell | 29 + 4 + 523 + 156 + ~40 ≈ 752 | **853** | **W-bound: ~853** | 1616 | −47 % |

(Revision 1 said 871/−25 % for the r9 shape; that omitted the 35 ms residual, which stays on E.
The acceptance threshold in candidate 3 is set from 906, with the sd unknown — report both reps.)

At 300 markets W is the wall: depth 1 then caps the chain at `flush(N)`. The levers there are
`TORUS_NATIVE_TRIE_MAINTENANCE=0` (async-trie doc Option 0, −415 ms of W; a capability rollback,
§3.5) and WAL-off (`r9 waloff`, −113 ms of db) — not depth 2, which only helps if W's per-block
cost has variance, not a higher mean.

The engine is not pipelined. Two engines in flight (N and N+1) would need N+1's reads to see
N's *in-progress* state — that is the market-sharded design, out of scope.

### 2.3 Why the ordering is enough (read-your-writes)

Serial today: engine(N+1) reads DB after write(N) landed. Pipelined: engine(N+1) reads
pending(N+1) → pending(N) → DB, with pending(N) = exactly the key/value set that write(N) will
land (same maps, `append_to_batch` iterates them), and — by the rendezvous invariant — **every
height < N is durable** when engine(N+1) starts. For every key the layered read returns the same
bytes the serial read would have, provided **every reader goes through the layer**. The readers
that do not today are enumerated in §3.1 and each is either re-routed, moved to W, or barriered.

---

## 3. Hazard table

### 3.1 Readers of what each moved stage produces (grep of CFs / markers / caches at 1dcf339)

| # | data | writer (stage) | readers today | under the pipeline |
|---|---|---|---|---|
| 1 | native state CFs (`CF_NATIVE_*`, staking, governance, oracle, sessions, treasury, fee config) | state_write(N) | engine(N+1) via overlay (`backend.rs:819`), `diff_stop_rows` iterate (`native_executor.rs:2470`), **verify `state_db.get_session` (`app.rs:1370`)**, RPC (`torus-rpc`), snapshot, `build_native_trie_to_cf` (boot) | engine/stops: layered overlay (automatic). **verify: re-route to `overlay.get_session`** (new generic helper over `StateBackend`). RPC: lags ≤ 1 block; the harness digest runs after drain + `wait_idle`. Snapshot: `wait_idle`. Boot: W not yet constructed (§2.1 W.6). |
| 2 | `CF_NATIVE_NONCES` | state_write(N) | **replay guard `state_db.get_cf_raw` (`app.rs:1453`)**, sync/catch-up validator | **re-route to `overlay.get_cf_raw`** (must, else an action included in N and N+1 double-executes). Catch-up (`replay_gap`, boot and in-process heal) runs serial (`pipeline_allowed=false`) after `wait_idle`, so it never overlaps an in-flight W job. |
| 3 | `META_NATIVE_APPLIED_HEIGHT` | state_write(N) (batch) / empty-block direct put (`app.rs:1830`) | **skip-check (`app.rs:1191`)**, `detect_conflicting_commit`, **resident staleness guard (`native_executor.rs:1514-1532`, `:1653`)**, `replay_committed`, `exec_next_height` seeds (`:3949`, `:4244`), manifest park (`:2754`), snapshot | **skip-check: `exec_applied` (RAM, E-owned)** — the durable marker lags and would *miss* a skip (re-execute N on parent=pending(N): nonces dropped by the layer but governance/fees/epoch/trade hand-off re-run, W writes a second batch(N)) — F3. Guard: marker in `overlay(N)` (§2.1 step 7) and 1-key Marker jobs in the parent chain (step 10) so the layered read returns N for every block shape, including native-after-empty — F5/F7. Boot seeds / manifest park / replay: run before W exists (serial replay) — F4. |
| 4 | `CF_NATIVE_TRIE` / `CF_NATIVE_HASHED` + `trie_cache` / `member_cache` | root(N) | `apply_native_dirty(N+1)`, `persisted_native_root` (cache self-auth `native_trie.rs:443,758`), `build_native_trie_to_cf`, tests. No production consumer of the root value (async-trie doc §1a; re-verified: `flagged_native_root` callers are test-only; header `state_root` is the parent copy; `app.rs:1651/1662, 2621-2624` are the only cache touch points). | All owned by W, sequenced. Caches move from `ExecutionContext` mutexes (`app.rs:520-531`) into W's state (no mutex). |
| 5 | `CF_BOOK_ORDER_ROWS` (node-local rows), `CF_NATIVE_ORDER_BOOKS` level/stop/meta rows | save_books(N) | non-resident book load (`load_order_books_levels`), `diff_stop_rows`, RPC `torus_getOrderBook` | resident path never reads rows during exec; non-resident/rebuild path = **`wait_idle()` before constructing the ctx**. `diff_stop_rows(N+1)` reads through the layer. |
| 6 | `CF_BLOCK_HEADERS` / `CF_BLOCK_BODIES` | dispatch (durable before exec) **or `fold_header` in the flush batch (replay / failed dispatch batch)** | parent-link (`:1219`, returns `false` = "cannot assert" on a missing row), replay, RPC, sync | Fast path requires `durable.header && durable.body`; any `fold_header` block is serial. Otherwise the ancestry check would be silently disabled for every pipelined block whose parent header is still on W — F9. |
| 7 | `CF_NATIVE_TRADES` / user trades | `trade_writer` (post-flush) | RPC only | Handed to the writer after E's hand-off; the writer may land trade rows **before** batch(N) is durable (C1/C3 below): keys are deterministic (height, seq), replay overwrites with identical bytes — idempotent; a crash loses cosmetic rows, as today. |
| 8 | EVM CFs (`CF_ACCOUNTS`, `CF_HASHED_*`, `CF_TRIE_*`) | EVM commit / **`resync_evm_accounts` (`app.rs:1725`, `incremental.rs:410`)** | EVM validate (N+1), incremental root | EVM blocks are barriered. **`resync_evm_accounts` is NOT a no-op without EVM**: `process_epoch_boundary` → `distribute_validator_inflation` (`rewards.rs:166`) → `credit_balance` (`staking.rs:836` `put_account`) dirties `CF_ACCOUNTS` on every epoch-boundary native block; it reads `db.get_account` (`incremental.rs:410-424`) assuming batch(N) landed. Under the pipeline: boundary blocks are serial (rule 2), and any other block with non-empty `dirty_evm_accounts()` has the resync run **on W after `write(batch(N))` Ok** — F2. Test: boundary block ON vs OFF, compare `CF_HASHED_ACCOUNTS`. |
| 9 | staking validator rows via `self.staking.slash` | pending slashes (direct DB, exec thread) | engine (epoch/rewards) | barriered: non-empty `pending_slashes` → `wait_idle`, then serial. |
| 10 | trades / fees / governance side effects of re-executing a height | (none — they must run once) | — | covered by row 3 (`exec_applied`). |
| 11 | **`CF_STAKING_VALIDATORS` / `DELEGATIONS` read AND written by the consensus thread** (`epoch_validator_set_updates` `app.rs:3178-3245`: `apply_pending_rotations :3192`, `update_validator_statuses :3235`, from `validate_block :3828` / produce `:3671`) | consensus thread at each epoch boundary, concurrent with exec | consensus-visible `ValidatorSetUpdates` | **Pre-existing hazard, not created by the pipeline**: the consensus thread already computes the set from whatever height exec has made durable (up to 64 blocks behind) and already writes those keys concurrently with the exec thread's batches. The pipeline adds ≤ 1 block of lag and moves the exec-side writer from E to W; it does not add a new writer pair. Mitigation: boundary blocks are serial on E (rule 2), so W never holds a batch carrying `process_epoch_boundary` writes; E's fast-path batches touch staking keys only through delegation actions, exactly as today. **Stated scope: the bench/devnet staking set is static (no delegation/rotation actions in the workload; `epoch_length` 100 000 never reached), so the flag is validated only for that; staking-changing workloads near an epoch boundary are out of scope for `TORUS_EXEC_PIPELINE=1` until the differential test in §5 (stake change one block before a boundary, ON vs OFF) passes** — F8. The proper fix (consensus reads the set from the exec-applied snapshot at the boundary, not the live DB) is a separate consensus-visible change and not part of this flag. |

### 3.2 Crash windows (process dies between X and Y). "durable" = RocksDB WAL-synced per today's WriteOptions.

| # | crash point | durable on restart | replay does | double-apply / lost state? |
|---|---|---|---|---|
| C1 | E mid-engine(N+1), batch(N) **received/in-flight on W** (rendezvous: N−1 is durable) | marker N−1; state N−1; bodies N, N+1 (dispatch); possibly trade rows of N (`trade_writer`) | re-executes N then N+1 from bodies (serial, on the boot thread) | none: N's effects were only in RAM; deterministic re-execution reproduces them byte-identically; trade rows are overwritten with identical bytes |
| C2 | batch(N) durable, E mid-engine(N+1) | marker N; state N; body N+1 | re-executes N+1 | none (identical to today's W1) |
| C3 | W `target.write(batch(N))` returns Err | marker N−1, state N−1 (atomic batch) — **and E may already have run engine(N+1) on pending(N)**; trade rows of N possibly durable | E sees the latch → fail-stop, nothing of N+1 is ever written; restart replays N, N+1 | none; stricter than today (today continues and diverges) |
| C4 | crash after E put marker into overlay(N) but before hand-off | marker N−1 | replays N | none — the overlay marker is RAM only; the durable marker is the batch's |
| C5 | empty block N+1 Marker job received behind batch(N), crash before either | marker N−1 | replays N (native), N+1 (empty, idempotent) | none |
| C6 | Marker job for N+1 landed (after batch(N)) — ordering guaranteed by W | marker N+1, state N | nothing | none — same as today |
| C7 | graceful shutdown with a job in flight | W `Drop` drains + joins → batch(N) durable | — | none |
| C8 | crash between dispatch-time body write and exec | pre-existing FIX-1a window | unchanged | not in scope |
| C9 | crash with `resync_evm_accounts` pending on W (batch(N) durable, mirror not yet resynced) | marker N, state N, `CF_HASHED_*` stale for the dirtied accounts | replay does not re-run N (marker N) | **same as today's window between `target.write` and the resync at `:1728`** (separate writes today too); the incremental root is flag-gated/off-by-default and self-repairs on the next resync of the same accounts. Not worsened. |

The fence is unchanged: the marker is only ever advanced by the batch that carries the state it
covers (or by the ordered Marker job for a block with no native state). What the pipeline
adds is **one more block of rewind on a crash** (C1: both N and N+1 re-execute instead of N+1 only)
— bounded by the depth (1). Replay cost per rewound block ≈ one full exec chain (~1 s at cap 200);
the r9 `waloff` crash test replayed 11 blocks in ~7 s, so this is small.

### 3.3 Determinism (why all 3 validators stay byte-identical)

- The batch key/value set is produced by the same code (`append_to_batch` + `apply_native_dirty`
  + marker) from the same `PendingState`; W only changes *which thread and when* the batch is
  built and written, never its content. Root: serial fold in bucket order, parallel leaf hashing
  already proven byte-identical across thread counts (`native_trie.rs` determinism tests).
- Engine(N+1) observes, for every key, the value the serial execution would observe (§2.3),
  **independent of W's timing**: the parent layer is always pending(N) (durable or not), and
  everything below N is durable by the rendezvous. The revision-1 design (`sync_channel(1)` + layer
  only when W "still holds" N) was timing-dependent and could read N−1's keys from a DB that did
  not yet hold them (F1/F6) — the single most important correction of this revision.
- The set of bypassing readers is closed by §3.1 (nonce guard, `get_session`, staleness marker,
  skip-check, parent-link, resync) and by the barrier rule for everything that writes the DB
  directly (EVM, slashes, epoch boundaries, boot rebuild, snapshot, replay). A differential test
  (§5) runs the same block sequence ON vs OFF and compares every CF dump + root.
- Nothing consensus-visible changes: no header field, no genesis parameter, no message. A
  validator running `TORUS_EXEC_PIPELINE=0` next to two running `=1` produces the same state;
  the harness's 3-validator agreement (block hash + RPC digest + counters) is the gate.
- Timing-dependent behaviour that could differ per node: the resident-book guard (RAM-only; a
  guard miss rebuilds from the DB, which under the pipeline may lag one block — **therefore a
  rebuild must `wait_idle()` first**; otherwise a node could load books at N−1 and execute N+1
  → fork). With the marker in the overlay and Marker jobs in the parent chain the guard passes
  deterministically for every block shape, so the rebuild path is a cold path (boot, barrier
  exit), not a per-node lottery (F5/F7).

### 3.4 Backpressure

- Depth is exactly 1: **`sync_channel(0)` (rendezvous)** + the "`wait_idle` before barrier paths"
  rule. `send(N)` blocks E until W has finished N−1; the blocked time is
  `exec_handoff_wait_seconds` (new metric). Nothing is dropped, nothing reorders, memory is
  bounded to two pending sets + two overlays. (Revision 1's `sync_channel(1)` allowed two
  outstanding jobs — a buffer, not depth 1 — F1/F6.)
- Upstream of E nothing changes: the 64-slot exec channel, rank-2 parks, `exec_queue_len` pacing
  and the S470 mechanisms see a faster E and a lower queue depth — the signal is preserved.
- If W is structurally slower than E (300 m: 853 vs 752 ms), E's wait is the whole difference
  every block and the chain is W-bound; the metric makes that readable (§4.2) instead of
  hidden inside `block_ms`.

### 3.5 Costs we accept, stated plainly

- One more block of crash rewind (C1). Acceptable for a node-local perf flag; document it next to
  `waloff`'s 11-block rewind.
- Stricter fail-stop on a flush write error (C3). Today's "log and continue" is already unsafe.
- Two pending sets + two overlays in RAM (≤ ~25 MB at 300 m).
- RocksDB now has a dedicated writer thread contending with the consensus thread's dispatch
  batch and the `trade_writer`; `stall_micros` is 0 in every cell so far, but the sweep must keep
  reporting `rocksdb.stall_ms_per_native_block` and `write_stopped`.
- Epoch-boundary blocks, EVM blocks, slashes, replay: serial (one barrier per epoch in production,
  never in the bench).
- Staking-changing workloads near an epoch boundary: out of validated scope for the flag (§3.1
  row 11) until the differential test exists.
- `TORUS_NATIVE_TRIE_MAINTENANCE=0` (if used for 300 m) trades "root always current" for
  "rebuild at boot when a consumer appears" — keep it a separate, default-OFF flag.

### 3.6 What was refuted among the seed candidates

- **`flush-root-overlap-1deep` as a standalone stage is not buildable without breaking the
  fence.** Root's trie/mirror ops are appended to the *same* batch as state + marker
  (`backend.rs:684-727`). Overlapping root(N) with engine(N+1) while keeping state_write(N) *before*
  engine(N+1) requires two batches (state+marker first, trie later) — i.e. the async-trie doc's
  Option A with its trie-height marker and O(state) rebuild after every unclean shutdown, which that
  doc rejected. Moving the *whole* `flush_with_native_trie_stats(N)` to W (root + build + write,
  one batch) keeps atomicity and overlaps root for free. So the root overlap is folded into
  `state-write-async-overlay-carry`; the key is retired, not deferred.
- **`save-books-off-thread` cannot move pass 1.** `take_level_ops_chunked` computes level digests
  from the *live* book levels (`order_book.rs:1617`); engine(N+1) mutates those levels. Only pass 2
  (overlay puts of the drained ops, `diff_stop_rows`, `write_meta_if_moved`;
  `native_executor.rs:2378-2470`) can move, and its share of the 131 ms is unmeasured in production
  (the `save-timings` split is µbench-only). Kept as a candidate, gated on the attribution.

---

## 4. Commit cadence vs exec under the pipeline, and how to measure it

### 4.1 What actually couples the proposer to exec today

- The S470 **commit-lag backoff** (`hotstuff_rs/src/pacemaker/implementation.rs:1229`,
  `stall_multiplier :780`) lengthens view timeouts on `highest_qc_view − committed_view`, both
  consensus-visible; it does **not** look at local exec. `TORUS_COMMIT_LAG_BACKOFF_CAP=8` in the
  runner only matters during a header-pipeline wedge. Pipelining exec does not change it.
- `TORUS_PROPOSER_EXEC_WATERMARK` (`hotstuff/implementation.rs:231-235`) yields the proposal
  slot on `pending_headers + deferred_bodies` — OFF unless set; off in every cell.
- `TORUS_EXEC_THROTTLE_WATERMARKS` (`app.rs:4161-4175`) thins blocks on `exec_queue_len +
  deferred_exec` — unset in every cell. So the proposer will not throttle cadence back when E
  gets faster; the only coupling left is below.
- The **64-slot exec channel + rank-2 parks** are the only hard coupling: `peak_exec_queue_depth`
  66 in `bl-base-10m-r2` shows consensus running the full window ahead. Cadence (372 ms/commit)
  was already independent of exec (1170 ms/native block).
- The remaining coupling is the **mempool**: actions leave the pool at commit; full (cap-200)
  blocks form at `cap / ingress` ≈ 100k orders / 76k/s ≈ 1.3 s, which is *close to* the exec chain
  at 10 m. If the pipeline drops the chain to ~0.9 s, **ingress becomes the wall and matched/s will
  not move** — the work-conservation pattern already seen with `waloff` and `stops-dirty-flag`
  (phase wins, flat matched/s). Expect the win to appear as `chain_ms` + `handoff_wait_ms`, with
  matched/s flat, unless RATE is raised.

### 4.2 How the sweep must measure it

1. **Primary**: `phase_by_node.val0.block_ms` → split into `chain_ms` (E wall per native block,
   = today's `block_ms` semantics minus the moved stages plus `handoff_wait`) and `pipelined_ms`
   (W wall per native block), both from new histograms (`exec_chain_seconds`,
   `exec_handoff_wait_seconds`, `flush_worker_seconds`). Today's `exec_block_seconds` keeps its
   name and becomes E-only; `exec_flush_seconds`/`exec_root_seconds`/`exec_state_write_*` are
   observed by W so the per-phase table keeps its columns. **Always next to
   `fills_per_native_block`** (node counter `matched_total` delta ÷ native blocks) and
   `engine_ms_per_1k_fills`, so a thinner-block artefact cannot be read as a chain win.
2. **Cadence**: `commit_interval_ms` p50/p95 from the `commit_interval_seconds` histogram
   buckets (exponential buckets exist; `summarize.py:198` reports only the mean), `blk_s` split
   into native vs empty (`exec_engine_seconds_count` vs `exec_block_seconds_count`),
   `wall_ms_per_native_block`, `peak/mean exec_queue_depth`, and `exec_resident_rebuilds` (from
   the candidate-1 telemetry).
3. **Throughput guard**: matched/s avg + best-60 (node counters; must not drop > 5 %), and a
   **RATE=100000 probe** cell on the same binary so the mempool, not ingress, bounds the block —
   otherwise a chain win is unreadable in matched/s. State which cell every number is from.
4. **Agreement**: block hash + RPC digest + counters equal on all 3; any fork / digest mismatch /
   fail-stop / panic rejects the candidate regardless of speed. The digest runs after drain and
   must additionally wait for `flush_worker_depth == 0` on every node (W idle) — add that to
   `run-cell.sh`'s drain condition.
5. **Crash gate** for the pipeline: kill −9 one validator mid-load with the pipeline ON; restart
   must log a replay of ≤ depth+queue blocks and end with a digest identical to the other two
   (same procedure as the `r9-waloff-crashproof` cell).
6. n ≥ 2 per arm, interleaved ON/OFF on one binary (env-gated), idle blk/s 12–28 before each cell.

---

## 5. Tests that ship with the pipeline

- `exec_pipeline_state_identical_over_sequence` (app.rs test mod): same 30-block sequence with
  `TORUS_EXEC_PIPELINE` OFF/ON (including empty blocks between native ones, a session-create in
  block k read by verify in k+1, an action repeated in k and k+1 for the nonce guard, **a key
  written in k and not in k+1 and read in k+2**); compare every CF dump + `persisted_native_root`
  after `wait_idle`.
- `exec_pipeline_parked_worker_reads_previous_height` (**the F1/F6 test**): park W inside
  `write(N−1)` (injectable hook); E must **block on the hand-off of N** (the FIRST hand-off, not
  the second); release W; then N+1 reads a key written only by N−1 and gets N−1's value. Debug
  assertion `overlay(N+1).parent.height ∈ {durable, durable+1}` fires on violation.
- `exec_pipeline_redelivery_is_skipped` (F3): park W inside `write(N)`, deliver block N to E
  again; assert the second delivery is skipped (`exec_applied`), W receives exactly one job for
  N, no second trade hand-off.
- `exec_pipeline_replay_is_serial` (F4): `crash_recovery_replays_committed_block` variant with the
  flag ON; assert the marker equals the replayed top height **before** `exec_next_height` is
  seeded and that no W exists at that point (W constructed after replay).
- `exec_pipeline_fold_header_runs_serial` (F9): a block with `durable.header=false` under the flag
  takes the barrier path; the parent-link check of the next block asserts (not "cannot assert").
- `exec_pipeline_epoch_boundary_serial_and_resynced` (F2): `epoch_length=4`, boundary block ON vs
  OFF; compare `CF_ACCOUNTS` and `CF_HASHED_ACCOUNTS`; assert the boundary block was serial.
- `exec_pipeline_stake_change_before_boundary_differential` (F8): delegation action at boundary−1,
  ON vs OFF, compare `CF_STAKING_*` and the computed `ValidatorSetUpdates`. Until this passes, the
  scope statement in §3.1 row 11 stands.
- `exec_pipeline_crash_before_write_replays_both` (extends
  `crash_after_commit_before_execute_recovers_via_durable_body`): drop W with batch(N) undrained
  after engine(N+1) ran → `replay_committed` re-executes N and N+1, final state identical, no
  fail-stop.
- `exec_pipeline_write_error_latches_failstop`: inject `Err` from `target.write` on W → E's next
  hand-off latches `exec_failed`, nothing of N+1 is written.
- `exec_pipeline_empty_block_marker_ordered`: native N then empty N+1; assert the marker never
  regresses (read after each W job).
- `layered_overlay_read_semantics` (backend.rs): point reads and `iterate_cf` with a tombstone in
  the parent and a put in the child, etc.; a durable parent returns the same bytes as the DB.
- `resident_guard_sees_overlay_marker` (F5/F7), three variants: (a) native N, native N+1 with W
  parked — guard passes; (b) native N, **empty N+1**, native N+2 with W parked on the Marker job
  — guard passes, `exec_resident_rebuilds` stays 0; (c) candidate-1 `advance_untouched` + (b).
- Depth bound: with W parked inside write(N−1), the **first** hand-off (N) blocks; `flush_worker_depth`
  never exceeds 1.

---

## 6. Ordered candidates

Ranking = expected ms off the 10-market critical chain per unit of risk, with the measurement
candidate first because nothing after it is readable without it. Kill-switch defaults are
proposals; ON requires the stated tests + 2 agreeing cells.

| # | key | track | model | Δ chain ms/blk (10 m) | default | depends on |
|---|---|---|---|---|---|---|
| 1 | `resident-books-untouched-advance-restack` | pipeline | fable | −59…−80 (`load_books` → ~0) + stall removal | ON (it is the existing `TORUS_RESIDENT_BOOKS=1` path behaving as documented) | — |
| 2 | `exec-chain-sub-100-attribution` | instrumentation | opus | 0 (ruler) | n/a | — |
| 3 | `state-write-async-overlay-carry` (absorbs `flush-root-overlap-1deep`) | pipeline | fable | −262 (flush) at 10 m; chain becomes E-bound ~906 on the r9 shape | OFF → ON after gates | 1, 2 |
| 4 | `save-books-pass2-on-worker` (was `save-books-off-thread`) | pipeline | fable | −(pass-2 share of 131; unmeasured, guess 40–80) | OFF | 2 (drain/write split timers), 3 |
| 5 | `block-cap-resweep-under-pipeline` | harness | opus | chain latency ↓ with cap (50/100/200) at equal matched/s | n/a | 3 |
| 6 | `root-maintenance-skip-300m` (async-trie Option 0) | pipeline | fable | 0 at 10 m; −415 of W at 300 m (W-bound → E-bound) | OFF | 3 |

**1. resident-books-untouched-advance-restack.** Cherry-pick 196166b, 4fb0831, 4aaef1d from
`cand/r5-resident-books-stale-rebuild-restack-restack-restack` onto 1dcf339 (expect a conflict in
`app.rs` around `:1830` and in `summarize.py`); `ResidentBooks::advance_untouched(height)` runs on
the non-native path right after the standalone marker write, strict successor only. Acceptance:
`phase_by_node.val0.phases.load_books.ms` < 5 on a 120 s 10 m cell (was 59–80), the new
`exec_resident_rebuilds` counter 0 during load, `block_ms` down by the same amount, matched/s within
noise, AGREE. Hazard: a node whose holder is invalidated mid-run rebuilds from the DB — unchanged
semantics. **Under candidate 3** the advance stays on E, the guard's marker read must see N+1 for
native-after-empty (Marker jobs carry a 1-key pending layer — §2.1 step 10; without it the holder
says N+1, the DB says N, and every native-after-empty block rebuilds + barriers, reverting this
candidate's win), and the rebuild path must `wait_idle()`. The acceptance criterion is re-run
under candidate 3 (variant (c) of `resident_guard_sees_overlay_marker` + the cell).

**2. exec-chain-sub-100-attribution.** Telemetry: `exec_chain_seconds` (E wall per native block),
`exec_handoff_wait_seconds`, `flush_worker_seconds`, `flush_worker_depth` gauge,
`exec_save_books_drain_seconds` / `exec_save_books_write_seconds` (pass 1 / pass 2 of
`save_level_authority`, `native_executor.rs:2334`, production timers not `save-timings`),
`exec_native_blocks_total` vs empty. On a serial binary chain == block and worker == 0 by
construction (the summarizer must not divide by zero). `summarize.py`: add `chain_ms`,
`pipelined_ms`, `handoff_wait_ms`, `commit_interval_ms_p50/p95` (from histogram buckets),
`native_blk_s` / `empty_blk_s`, **`fills_per_native_block`, `engine_ms_per_1k_fills`**,
`gap_to_100ms = chain_ms − 100`; `run-cell.sh`: drain waits for `flush_worker_depth == 0`;
`test_summarize.py` fixtures for both binaries. Acceptance: identities hold on every node-cell
(`chain + moved phases == block` on serial, `chain ≥ engine + verify` on pipelined), overhead
< 5 ms/blk.

**3. state-write-async-overlay-carry.** Files: `torus-state/src/backend.rs` (`NativeStateOverlay::
with_parent(Arc<FrozenPending>)`, `freeze()`, layered `get_cf_raw`/`iterate_cf`,
`flush_with_native_trie_stats` unchanged in content; `FrozenPending{height, maps}` also
constructible from a single marker key), new `torus-consensus/src/exec_pipeline.rs` (W thread:
`Job::Flush{height, pending: Arc<FrozenPending>, evm_addrs}` | `Job::Marker{height, pending}`,
**`sync_channel(0)`**, failure latch, `durable_height` atomic + condvar, `wait_idle()`, owns
`NativeTrieCache`/`NativeMemberCache`, runs `resync_evm_accounts` after a successful write when
`evm_addrs` non-empty, `Drop` drains + joins), `app.rs` (`execute_committed_block_with` gains
`pipeline_allowed: bool` — `execution_loop` passes the flag, `replay_committed`/`replay_gap` pass
`false`; fast-path eligibility per §2.1 step 2 incl. `durable.header && durable.body` and
`!is_epoch_boundary`; skip-check `:1191` reads `exec_applied`; re-route `get_session` `:1370` and
the nonce guard `:1453` through the overlay; marker put into the overlay before stash; hand-off
replaces the inline flush + resync `:1647-1735`; empty-block marker `:1830` → Marker job with a
1-key pending layer; `ExecutionContext` loses the two cache mutexes and gains `exec_applied` +
`last_job`; W constructed after `replay_committed` in `TorusApp::new`; `execution_loop` joins W on
exit), `native_executor.rs` (rebuild path `wait_idle` hook; guard unchanged), telemetry.
Kill switch `TORUS_EXEC_PIPELINE` (default OFF in the first commit). Acceptance: `flush.ms` on val0
moves from the E table to `pipelined_ms` (≈ 260 at 10 m), `chain_ms` ≤ 940 on the r9 shape at
equal `fills_per_native_block` (`block_ms` 1168 → E-bound 906 predicted; report both reps),
`handoff_wait_ms` reported, matched/s avg and best-60 not below −5 % of the same-binary OFF arm
(expect flat unless the RATE=100k probe shows the mempool is not the wall), AGREE on both reps,
crash gate passed, `exec_resident_rebuilds` 0, `rocksdb.stall_ms_per_native_block` still 0, all
§5 tests green.

**4. save-books-pass2-on-worker.** Only after candidate 2 shows pass 2 ≥ 40 ms/blk. Snapshot
`book.stop_rows()` and the meta fields into `DrainedBook` on E; W applies the drained ops + stop diff
+ meta puts into a **separate W-owned `books(N)` pending layer** (NOT into `pending(N)`, which E is
concurrently reading as `overlay(N+1)`'s parent — a per-key write lock there would contend with
every parent read) and merges it into the batch at flush time. Legal only because the resident
engine never reads book rows (`CF_BOOK_ORDER_ROWS`, level/stop/meta rows): the layered overlay
records the CFs read during engine(N+1) in debug builds and panics on a book-row read. Non-resident
path barriers. Kill switch `TORUS_SAVE_BOOKS_ON_WORKER`, default OFF. Acceptance: `save_books.ms`
on E drops by the pass-2 share, `pipelined_ms` grows by the same, chain down, AGREE, differential
test.

**5. block-cap-resweep-under-pipeline.** No code: `BLOCK_CAP=50/100/200` × pipeline ON at
RATE=76k and 100k, 2 reps each; report `chain_ms`, `commit_interval_ms_p50/p95`, `native_blk_s`,
`fills_per_native_block`, matched/s. The deliverable is the curve "chain latency vs cap at constant
matched/s" — this is the only way a full block approaches the ~100 ms cadence (cap ~25–50 once
fixed costs are gone).

**6. root-maintenance-skip-300m.** Implement async-trie doc §8 (`TORUS_NATIVE_TRIE_MAINTENANCE`,
`META_NATIVE_TRIE_STALE` sentinel in the same batch, rebuild at boot, `native_root_routed` fallback).
With maintenance off, W must report `root_seconds = 0`, **invalidate `trie_cache`/`member_cache`
and mark them stale** (so a later re-enable rebuilds rather than self-authenticating against a
stale persisted root), and the summarizer must show the sentinel. Only relevant where W is the
wall (300 m). Default OFF; never a 10 m lever.

---

## 7. Open questions

- pass-1 vs pass-2 share of `save_books` in production (decides candidate 4).
- Is ingress (RATE=76k, cap 200) already within 15 % of the exec chain at 10 m? A RATE=100k probe on
  1dcf339 answers it before candidate 3 is benched; if yes, candidate 3's win shows in `chain_ms`
  and `handoff_wait`, not matched/s, and the throughput claim must wait for the cap resweep.
- Whether `stall_micros` stays 0 with a dedicated writer thread at 300 m (12 MB batches at 37 MB/s).
- The consensus-thread validator-set computation (§3.1 row 11) reading a lagging DB is a
  pre-existing, consensus-visible hazard independent of this flag; it deserves its own design.

---

## 8. Review resolution (revision 2)

| id | reviewer | severity | finding | resolution |
|---|---|---|---|---|
| F1/F6 | both | blocker | `sync_channel(1)` buffers one job → two non-durable pending sets with a single parent layer → N−1's keys read stale → per-node divergence | `sync_channel(0)` rendezvous; parent is ALWAYS the previous job's pending set (durable or not); invariant restated; debug assertion; new parked-worker test (§2.1 step 9, §3.4, §5) |
| F2 | crash | major | `resync_evm_accounts` is not a no-op (epoch inflation credits CF_ACCOUNTS); reads DB assuming batch(N) landed | epoch-boundary blocks serial; any other block with dirty EVM accounts has the resync run on W after its write (§2.1 rule 2, W.2; §3.1 row 8; C9; test) |
| F3 | crash | major | skip-check reads the durable marker → a re-delivered height inside the depth window is re-executed | E-owned `exec_applied` watermark used by the skip-check (§2.1 step 1, §3.1 row 3; test) |
| F4 | crash | major | boot replay through the same function with the flag → batches on W while `exec_next_height`/manifest seeds read the durable marker | replay passes `pipeline_allowed=false`; W constructed after replay (§2.1 rule 2, W.6; test) |
| F5/F7 | both | major | Marker-only jobs carry no pending set → resident guard sees N, holder says N+1 → rebuild + barrier on every native-after-empty block; candidate-1 acceptance not reproducible | Marker jobs carry a 1-key frozen pending layer in the parent chain (§2.1 step 10; §3.1 row 3; candidate 1 text; guard test variants (b)/(c)) |
| F8 | adversarial | major | consensus-thread `epoch_validator_set_updates` reads+writes `CF_STAKING_*` concurrently with W; absent from the table | §3.1 row 11: documented as pre-existing (64-block lag, concurrent writer today); boundary blocks serial; explicit scope statement (static staking set only) + differential test; proper fix flagged as separate consensus-visible work |
| F9 | adversarial | major | `fold_header` blocks under the pipeline make `detect_parent_link_violation` a silent no-op; in-process `replay_gap` not covered by "catch-up not concurrent" | fast path requires `durable.header && durable.body`; all replay serial after `wait_idle` (§2.1 rule 2; §3.1 rows 2, 6; test) |
| minor | crash | — | "byte-identical batch" false (duplicate marker put) | reworded: identical final key/value set (§2.1 step 7) |
| minor | crash | — | C1/C3 should mention trade rows durable ahead of state | added (§3.1 row 7, C1, C3) |
| minor | crash | — | candidate 4's write lock on pending(N) contends with E's parent reads | separate W-owned `books(N)` layer merged at flush (candidate 4) |
| minor | crash | — | candidate 6 needs `root_seconds=0` and cache invalidation stated | added (candidate 6) |
| minor | adversarial | — | r9 arithmetic 906 not 870; ≤ 900 acceptance unsupported; report fills/native block | §2.2 table corrected; acceptance ≤ 940 at equal fills; `fills_per_native_block` + `engine_ms_per_1k_fills` added to candidates 2, 3, 5 |
