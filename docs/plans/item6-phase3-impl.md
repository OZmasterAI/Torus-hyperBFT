# Implementation Plan: item 6 Phase 3 (coalesced state checkpoints + replay)

Status: UN-SHELVED s111 (owner, same day): step 1 starts. The shelve judged Phase 3 on a CPU rule
that misses its payoff; the prototype (plan row 47, results doc section 42) showed matched/s +4.7%,
native blocks/s +6.1%, CPU -2.1% at equal fills, disk writes -51%, +0.8 GB RAM/node, restart <= 26.6 s,
and Phases 4-5 build on Phase 3. Gate revised (section 1). Task 1.1 merged; step 3 built
(feat/trade-retention). Was: shelved s111; plan (s109, 2026-10-11). Design and options: `item6-phase3-design.md`
(Option C, owner s109). Parent design: `market-scaling-in-memory-design.md` Phase 3, D5 b, D6 b,
D7 c, D10 c for now, D11 a, D13 a. Short form: `item6-phases-2-5-plans.md` Phase 3.
Code references are `file:function` with line numbers on main `fd9e5dfa`.

Every number below is from the results doc (sections 37-40) or from the code, unless marked
**(estimate)**.

## Design decision

Option C, staged:
1. the core behind a node-local interval setting (unset = exactly today);
2. the validator ingress readers onto a published view;
3. D13 trade-history retention;
4. the gate campaign;
5. RPC readers (Option A) only if the owner asks.

Interval: starting default 15 s of execution, node-local (owner s109; 30 s later if archy's sizing
says it pays). Validators run it; RPC nodes keep interval 0 (today). Validator RPC may lag by up to
one interval (documented, D10 c).

## 1. Goal and gates

Goal: stop writing every native block's state to RocksDB. Keep the last value per key in memory
and write it in one atomic checkpoint every interval. Recover from a crash by re-executing the
blocks after the last checkpoint S. No consensus rule, no state format and no `h_n` change.

| gate | target | measured by |
|---|---|---|
| Phase gate, CPU | no node CPU-s/1M regression vs tw beyond noise (was <= -6%; revised owner s111) | interleaved cells, step 4 |
| Phase gate, disk | state-row bytes written down by the coalescing factor archy measures (step 0) | iostat + RocksDB counters, step 4 |
| Phase gate, restart | restart -> ready <= 60 s (SIGKILL val1, multi-crash cells) | step 4 |
| Phase gate, throughput | matched/s UP vs tw beyond noise; native blocks/s no regression (owner s111 after p3t, section 43: on tmpfs the prototype's blocks/s gain is +1.6%, not resolved, so "up" is not required) | step 4 |
| correctness | per-block write sets, `h_n` and full CF dump identical: interval 15 s vs 0 vs 1 block, crash at random points (incl. mid-checkpoint), 3 replicas with different intervals | tests, step 1 |
| RAM | RSS growth within archy's layer-size figure + 25% | step 4 |

The CPU ceiling is ~7% vs tw (step 0: flush worker + state-row memtable / compaction 1.76 CPU-s/1M
on Classic before tw). The per-block running-hash digest stays on W, so its cost is not saved.

## 2. What changes vs the design doc (code check s109)

- `chain_step` (`running_hash.rs:341`) reads h(n-1) from META on W. That is only correct while
  the pipeline is one block deep. h(n-1) moves to memory on W (task 1.3).
- Trade rows, block bodies / headers and DA are written outside the block batch
  (`app.rs:3157`, `app.rs:1020`, `native_da.rs`). Phase 3 coalesces state rows only.
- Modes 2/3 deferred book save: W applies pass 2 reading the DB as "every height < N durable"
  (`exec_pipeline.rs:374`). That is false with an interval, so **interval > 0 requires a
  non-deferred book mode** (Classic, the compiled default, or OrderRows). With modes 2/3 and an
  interval set, the node refuses to start (owner s109, 6.1).
- Resident staleness guards: `begin_resident` (`native_executor.rs:3379`) checks height, marker
  and parent; `new_with_mode` (`:4285`) checks marker == holder height. They must read the
  marker through the overlay (layer included), never the DB. Otherwise R and the books rebuild
  every block (task 1.6 test).
- Native post-commit credits EVM balances in `CF_ACCOUNTS` inside the native batch, then
  resyncs the EVM mirror after the write (`exec_pipeline.rs:520`). With an interval, `evm_addrs`
  accumulate and resync after the checkpoint is durable.
- Epoch plans: `epoch_validator_set_updates` (`app.rs:5263`) needs the durable marker >= H - L. The
  boundary block is already a barrier (`app.rs:1973`); barrier blocks become checkpoints, so the
  plan block is durable when needed. No reader change.
- State-hash monitor (`state_hash.rs:167`) reads attest checkpoints from the DB. **Every
  attest height (h % 100 == 0) forces a checkpoint**, so `AttestStateHash` is never delayed and
  the monitor is unchanged. At ~4.3 native blocks/s, 100 blocks is ~23 s, so this bounds
  windows at 100 blocks without cutting the 15 s interval much.
- Pruner cutoff is not clamped to the applied marker today (`pruner.rs:139`; opt-in
  `--retention-blocks`). Fixed first (task 1.1), independent of the interval.
- `SnapshotManager` is unwired. Not needed for the gate (Not building).

## 3. Shape of the core

```
E (exec thread)                              W (flush worker)
block n: own pending -> R -> parent(n-1)     per block: Job::Digest(n): h_n from h(n-1) in memory
         -> L (S+1..n-2, merged)                        (+ attest checkpoint kept in memory)
         -> C (checkpoint in flight, if any) checkpoint: Job::Checkpoint{S, L}: ONE batch =
         -> DB (durable up to S)                        L + marker S + h_S + attest rows in (S',S]
                                                        + action-status rows, then EVM resync
```

- **L** (since-checkpoint layer) holds the last value or tombstone per (CF, key) over S+1..top.
  It is kept like R: an `Arc` with no other clone alive at the end of block n+1, so the merge of
  frozen(n) runs on the end-of-block worker (`end_resident_on_worker`, `native_executor.rs:3507`)
  and settles at the next begin. Reads of R's 5 CFs never reach L (R holds them); L still carries
  them for the checkpoint write.
- **Checkpoint trigger**, checked at the end of block n: interval of execution time
  (`TORUS_CHECKPOINT_INTERVAL_MS`), L byte cap (`TORUS_CHECKPOINT_MAX_BYTES`), n % 100 == 0,
  shutdown. **Barrier blocks** (EVM, slash, epoch boundary, non-durable header/body) first drain
  L into a checkpoint, then run today's serial path verbatim, so after them S = that block.
- **W channel**: per-block digest jobs are small and the checkpoint write is big. The rendezvous
  `sync_channel(0)` would block E for the whole write, so it becomes bounded-buffered, with at
  most one checkpoint in flight. A second trigger while one is writing makes E wait (counted).
- **Fail-stop** is unchanged: a checkpoint write error latches `exec_failed`, and restart replays
  (S, committed] (`replay_committed`, `app.rs:4889`, serial, per-block writes as today).
- **Interval unset or 0** keeps today's per-block `Job::Flush` path byte for byte (no L, no
  digest job).

## 4. Steps and tasks

### Step 0: sizing on tw (archy, measurement only, no code)

Inputs:
- coalescing factor at 1 block / 5 / 15 / 30 s per CF, Classic book blobs separate;
- peak L bytes per window (= checkpoint batch size);
- restart budget per interval;
- CPU ceiling on tw (flush worker + state-row memtable / compaction CPU-s/1M).

Results doc sections 41 and 42, plan rows 46 and 47.

**Done (campaign `ozarchy-p3s`, 2026-10-11):** results doc section 41; review log row 46 is in
`item6-phase2-impl.md` section 8 (rows 42-45 live there; this file has no review log). Coalescing
(state bytes / keys) 5.95x / 1.58x at min(15 s, 100 blocks), 6.34x / 1.64x at min(30 s, 100 blocks);
layer peak ~214-219 MB key + value; restart budget 21-22 s (gate 60 s); CPU ceiling 1.55 CPU-s/1M
(6.2%), factor-scaled ~0.82 (3.3%): **owner decision needed before step 1** (stop rule).

**Prototype measurement (campaign `ozarchy-p3p`, 2026-10-11; owner via 18c s111):** results doc
section 42, review log row 47 (in `item6-phase2-impl.md`). The throwaway prototype `proto/p3-coalesce`
`682a8df5` (one checkpoint per 100 blocks) cuts node CPU-s/1M by 4.90% (all rows vs control), 5.07%
with the state hash on (pooled 4.99%), and 2.98% with book blobs only. At equal fills the cut is
2.1-2.5% (all) / 1.0% (books). Disk per validator: -51% bytes written (all) / -45% (books). Digest
measured ~0.3 CPU-s/1M. SIGKILL restart -> first commit 21.3 s mean (gate 60 s). **By the owner's
rule: shelve Phase 3 and keep the disk-write numbers (borderline: the 5% line is inside both CIs;
owner to confirm).** Steps 1-4 below are on hold.

**Disk isolation (campaign `ozarchy-p3t`, 2026-10-11):** results doc section 43, review log row 48. Same prototype node, with all
3 data dirs on tmpfs as an UPPER BOUND (no device I/O; not a separate-disk result, ozarchy has one NVMe) next to the shared NVMe.
CPU-s/1M -5.2% (shared NVMe) / -4.8% (tmpfs): the CPU saving is work removed. matched/s +5.8% / +3.1% (tmpfs not resolved),
native blocks/s +14.0% / +1.6%: most of the blocks/s gain was disk contention. Inference: a validator with its own disk lands
between the two (about +3% to +6% matched/s, ~5% less CPU-s/1M; a blocks/s gain above +2% only on a busy disk).

**Step 1 measured (campaign `ozarchy-p3c1`, 2026-10-11):** results doc section 45, review log row 50. `7c178637`, interval 15 s, vs
`2725e321`, shared NVMe and tmpfs (upper bound), n = 4. CPU-s/1M -7.9% / -8.3% (gate met), native blocks/s +96% / +86% (met),
matched/s -7.0% / -10.6%: the bench's in-flight release follows `eth_blockNumber`, which step 1 advances only at checkpoints
(every 100 blocks), so offered load falls 13-16%. The matched/s gate is not decidable before step 2. Checkpoints fire at the
100-block attest height (94%), not at 15 s; layer_shared 0. Restart and crash not measured there; they ride along
with the step 2 bench. Also valid for `a44bdde6` (byte-cap fix only; `TORUS_CHECKPOINT_MAX_BYTES` unset in the bench).

**Stop rule:** if the CPU ceiling on tw is below ~5% (gate -6% unreachable), the owner decides
before step 1. The options are: build anyway for the disk and W-ceiling gains with a lower gate,
or drop Phase 3.

### Step 1: core behind the interval setting

**Step 1 restart and crash cells (campaign `ozarchy-p3r1`, 2026-10-12):** results doc section 46, review log row 51.
`a44bdde6`, interval 15 s vs unset, state hash on, 48 val1 kills (KILL / TERM / INT, 8 per arm x signal). Restart
gate met: SIGKILL kill -> first commit 14.4 s mean / 23.2 s max (unset: 14.3 / 16.9); rewind from the last
checkpoint 7-162 blocks. Crash correctness: no finding (replay from marker + 1, complete, every checkpoint hash
equal on 3 validators). SIGTERM is a hard kill on this binary (main.rs waits only on SIGINT): owner to decide.

Each task: test first (fails before), then code, then verify with
`F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"`.

**Task 1.1: clamp the pruner cutoff to the durable applied marker**
- Test first (`pruner.rs` tests): `pruner_never_deletes_bodies_above_applied_marker`. The marker
  is at 50, current is 200, retention is 10, so the cutoff must be <= 50, and bodies 51..200 must
  survive. It fails today (cutoff 190).
- Code: `pruner.rs:maybe_prune` (:139-160): `cutoff = min(current - retention, applied_marker)`.
  The marker is read with the existing `read_applied_height` helper.
- Verify: `cargo nextest run -p torus-state $F -E 'test(pruner)'`.

**Task 1.2: settings**
- Test first: `checkpoint_interval_parse` (unset / "0" -> None; "15000" -> 15 s; garbage -> None
  with a warning) and `checkpoint_interval_refuses_deferred_book_modes` (interval > 0 with
  `TORUS_BOOK_ROWS=2|3` -> startup error naming both settings; interval > 0 with Classic or
  OrderRows, and interval unset with any mode -> starts).
- Code: `exec_pipeline.rs` next to `parse_exec_pipeline_toggle`: `parse_checkpoint_interval`,
  `parse_checkpoint_max_bytes`, and a pure `check_checkpoint_book_mode(interval, book_mode)`
  that returns the error. Node start (where `FlushWorker` is attached, `app.rs:4485-4490`)
  calls it with `book_mode()` (`native_executor.rs:2313`) and exits on error.
- Verify: `cargo nextest run -p torus-consensus $F -E 'test(checkpoint_interval)'`.

**Task 1.3: running hash h(n-1) in memory on W (interval 0, no behaviour change)**
- Test first: `running_hash_prev_from_memory_equals_meta`. Run the
  `running_hash_identical_serial_pipelined_restart_every_book_mode` sequence (`app.rs:16608`) with
  W's in-memory prev asserted equal to the META read on every block.
- Code: `running_hash.rs:chain_step` gets a variant taking `prev: Option<(u64, [u8; 32])>`. W
  seeds it from META at spawn (`FlushWorker::spawn`) and updates it after each digest. The serial
  path keeps the META read.
- Verify: `cargo nextest run -p torus-state -p torus-consensus $F -E 'test(running_hash)'`.

**Task 1.4: since-checkpoint layer type L**
- Test first (`torus-state`, new `checkpoint_layer.rs` tests):
  `layer_merge_matches_sequential_apply_random`. 500 random blocks of writes and tombstones over
  8 CFs; reading through L equals applying the blocks one by one to a `BTreeMap` model, and
  so does `append_to_batch` written to a temp DB.
- Also `layer_tombstone_hides_db_value` and `layer_bytes_tracks_inserts_and_overwrites`.
- Code: `CheckpointLayer` holds a `PendingState` (reuse its maps and
  `append_to_batch`, `backend.rs`). `merge(&FrozenPending)` moves entries with last-wins, a
  tombstone replacing a write, and `bytes()`.
- Read order in `NativeStateOverlay::get_cf_raw` (`backend.rs:2008`): own -> R -> parent -> L
  -> C -> DB. L and C are new optional fields set like `parent` (`with_parent`, `backend.rs:1147`).
- Verify: `cargo nextest run -p torus-state $F -E 'test(layer_)'`.

**Task 1.5: W jobs Digest and Checkpoint**
- Test first (`exec_pipeline.rs` tests):
  - `digest_jobs_then_checkpoint_writes_one_batch`: 20 frozen blocks, then a checkpoint at 20.
    The DB is unchanged until the checkpoint; after it the DB equals the per-block-flush DB at 20,
    including META marker, running hash and attest rows.
  - `checkpoint_write_failure_latches`: the `WorkerGate::fail_next` path.
  - `second_checkpoint_waits_for_first`.
- Code:
  - `Job::Digest { height, pending }` computes `block_digest` (`backend.rs:1540`) and chains from
    the in-memory prev.
  - `Job::Checkpoint { height, layer, evm_addrs, side_rows }` builds the batch through
    `flush_pending_after_batch` (`backend.rs:1582`) with the layer as `state`, the marker =
    `height`, h_S and the buffered attest checkpoints (`running_hash::append_to_batch` gets the
    kept rows). Then it runs the EVM resync.
  - The channel becomes `sync_channel(DIGEST_QUEUE)` **(estimate: 128)**, with a checkpoint
    in-flight flag.
- Verify: `cargo nextest run -p torus-consensus $F -E 'test(exec_pipeline) | test(digest_) | test(checkpoint_)'`.

**Task 1.6: E side, layer upkeep, triggers, barriers**
- Test first (`app.rs`, reusing `p2_run` / `P2Fault`, `app.rs:21510-21807`):
  - `checkpoint_interval_matches_per_block_classic`: 130 `p2_blocks` (Classic, serial and
    pipelined) at interval 0 vs every-5-blocks vs never-by-time (only %100 and barriers). Per-block
    write sets (`h_n`), running hash and the final full CF dump are equal.
  - `checkpoint_barriers_drain_layer`: EVM block, slash block and epoch boundary inside a window.
    S == that height after each; the R01 EVM test (`app.rs:15733`) passes with interval > 0.
  - `checkpoint_attest_heights_force_checkpoint`: the marker is durable at every h % 100.
  - `resident_guards_do_not_trip_with_interval`: `torus_exec_resident_rebuilds` stays at 1 over 130
    blocks at interval "never" (fails if the guards read the DB marker).
- Code:
  - `app.rs` fast path (:2943-2960): at interval > 0, hand `Job::Digest`, keep frozen(n) as the
    parent, merge frozen(n-1) into L on the end-of-block worker, and evaluate the trigger.
  - Barrier (`pipeline_barrier`, :1736) = checkpoint L + `wait_idle`.
  - Overlay construction (:2214-2224) gets L and C.
  - Guards: `begin_resident` / `new_with_mode` read the marker through the overlay.
  - Shutdown: final checkpoint before the `FlushWorker` drop.
- Verify: `cargo nextest run -p torus-consensus $F -E 'test(checkpoint_)'`.

**Task 1.7: crash and replay at interval > 0**
- Test first:
  - `checkpoint_crash_replay_matches_uncrashed_{classic,order_rows}`. `P2Fault::CrashAfter` at 6
    random points, including mid-checkpoint (`WorkerGate::hold` with the batch in flight, then a
    kill). Restart replays (S, committed]; per-block `h_n`, running hash and CF dump equal the
    uncrashed run.
  - `three_replicas_different_intervals_identical`: interval 0 / 5 blocks / never.
  - `checkpoint_trie_maintenance_root_equal`: `TORUS_NATIVE_TRIE_MAINTENANCE=1`, the root at
    each S equals interval 0.
- Code: replay needs no change (it runs from the durable marker). Fix whatever the tests find.
- Verify:
  ```
  cargo nextest run --workspace $F
  cargo test --doc -q > doc.log 2>&1; echo "rc=$?"
  cargo clippy --workspace --all-targets
  cargo fmt --check
  ```

**Review**: Codex torus-adversarial on step 1 (state hash, replay and fail-stop are
consensus-adjacent) before step 2.

### Step 2: validator ingress readers on a published view

The view is an `Arc<ReadView>` = frozen sets since S (newest first) + C + DB. It implements the
read half of `StateBackend` (`backend.rs:25`) so `PositionManager` / `StakingManager` /
`OracleManager` work unchanged. E publishes it after each block (one `Arc` swap). Off E, so
probing up to ~100 small maps per lookup is fine. Memory = un-coalesced bytes since S; checked
against step 0.

Readers (reader audit s109):
- mempool gossip session lookup (`torus-mempool/src/lib.rs:648`);
- min-collateral balance (`funded.rs:122-136`);
- duty exemption (`funded.rs:103`);
- oracle reporter (`lib.rs:871-893`);
- EVM nonce / balance at `add_evm_tx` (`validate.rs:143`) and `pending_nonce` (`lib.rs:1432`):
  balance only (native credits to `CF_ACCOUNTS`).

Validator RPC (`torus.rs`, `eth.rs`) stays on the DB and lags (documented in
`docs/validator-home-network.md`), except the RPC submit checks (owner s112, follow-up below).

- Test first: `ingress_view_matches_interval_zero`. A session created at n, a balance credited
  at n, an EVM credit at n; gossip admission at n+1 accepts with interval "never" exactly as with
  interval 0. Fails before (session not found).
- Verify: `cargo nextest run -p torus-mempool -p torus-consensus $F -E 'test(ingress_view)'`.
- **Built** (`feat/item6-phase3-step2`, off step 1 `a44bdde6`, s111): `torus_state::ReadView`
  (`read_view.rs`: frozen blocks above S newest first -> C -> DB; no layers = every call the
  `StateDb` call; writes refused). E keeps the frozen sets above S (`CkptState.view_blocks`) and
  publishes after each pipelined block (`publish_ingress_view`, after the hand-off and the
  trigger: a checkpoint here clears them, C holds them); a barrier and a fresh W publish `None`
  (all durable, the DB). **Memory (checked against step 0):** whole frozen sets would pin the
  un-coalesced state bytes, ~885 MB per plan-15 s window on tw (section 41: 7,966 MB / 9
  windows; Classic book blobs 77%), vs ~150 MB for L. So the view answers only the ingress CFs
  (`read_view::INGRESS_CFS`: sessions, native balances, accounts, positions, validators,
  oracle; markets and fee config since the s112 follow-up; other CFs read the DB) and the end-of-block merge thread also cuts block n-1 down to
  those CFs (`FrozenPending::slice`), swapped in at `settle`: full sets only for the newest two
  blocks. What is left is mostly positions (~23% of written state bytes, 1.5x coalescing), so
  **estimate** ~150-200 MB per window on tw; archy to measure RSS in step 4.
  The mempool reads `ingress_state()` (published view, else the DB) for the gossip session lookup, funded check, duty exemption, oracle reporter, `add_evm_tx`
  (nonce + balance: one account row), `pending_nonce` and the item B volume read. The
  post-commit EVM nonce prunes stay on the DB (EVM blocks are barriers, so DB nonces are
  current). Tests: `ingress_view_matches_interval_zero` (RED first: session not found, D
  unfunded, D balance 0), `ingress_view_follows_checkpoints_and_barriers` (ingress CFs equal
  interval 0 after every block without waiting for W, at every 3 / never / C parked in
  flight; barriers drop the view), `ingress_view_serves_duty_oracle_and_funded_checks`,
  `read_view_*` (random model, newest layer wins, DB-only, ingress CFs only, slices).
- **Follow-up (owner s112): RPC submit on the view.** `torus.rs` `verify_one_action_with`
  (session lookup, known market + tick / lot, price band: oracle mark + governance band width)
  is generic over `StateBackend`; both submit endpoints (`submitNativeAction`,
  `submitNativeActions[Bin]`) pass `mempool.ingress_state()` (fetched once per call; interval 0:
  the DB alone, unchanged). The market listings (`CF_NATIVE_MARKETS`, a governance
  `MarketListing` executes in a block) and the band width (`CF_FEE_CONFIG`, governance
  `ParameterChange`) can change inside a window, so `INGRESS_CFS` gains both (8 CFs). Memory:
  per block the markets CF gets one 16-byte order-id counter row when orders flowed (key 24 B)
  plus a row per listing; the fee-config CF only governance rows: ~100 B per block with
  overhead, ~10 KB per plan-15 s window, negligible next to positions. Test (RED first:
  `unknown market_id 1` x3): `torus-rpc` `rpc_submit_sees_ingress_view` (rows of block 5 only
  in the published view vs in the DB; session order accepted, 120 refused by the 10% band, 105
  accepted, through both endpoints).
- Still on the DB (plan: validator RPC stays): the other `torus_*` / `eth_*` reads and
  `eth_head` (`eth.rs` `eth_head`: durable marker), so `eth_blockNumber` moves in checkpoint
  steps. The
  step 1 bench stall (section 45: the in-flight tail polls `eth_blockNumber` on val2) is NOT
  removed by step 2; owner options in the step 2 report.

### Step 3: D13 trade-history retention (RPC node)

- First read the key layout of `trade_rows.rs` (:167) and the trade index CFs.
- Test first: `pruner_prunes_trades_below_retention` (rows below the cutoff are gone, rows
  above stay, the RPC trade-history methods return the kept range).
- Code: knob `--trade-retention-blocks` (unset = keep all, today); the pruner deletes trade rows
  below `current - retention` (`pruner.rs:207-235` pattern).
- Verify: `cargo nextest run -p torus-state -p torus-rpc $F -E 'test(trades)'`.
- **Implemented** on `feat/trade-retention` (s111, standalone task after the core was shelved):
  cutoff `min(current - N, applied)`, both trade CFs, RPC serves only `[cutoff, ..]`
  (`DataPruned` for a block / range entirely below). Details: `docs/pruning-guide.md`.

### Step 4: gate campaign (archy)

- tw vs tw + 15 s (+ tw + 30 s if step 0 says it pays). Classic, node from the step 1-3 head,
  n = 4 interleaved, standard 300-market shape.
- Report: node CPU-s/1M, matched/s, state-row bytes written (iostat + RocksDB counters),
  checkpoint wall p50 / p99 and the E wait on a second checkpoint, block-time p99 around
  checkpoints and epoch boundaries, RSS.
- Plus multi-crash restart cells: SIGKILL val1 at random points, n = 3; restart -> ready.
- **Measured for step 2** (campaign `ozarchy-p3s2g`, results doc section 47, plan-2 row 52; 18c s112): step 2
  `2296be67` vs main `c97f56f9`, both with the bench in-flight tail on committed bodies. CPU-s/1M -7.4% (met),
  device writes -53% (met), matched/s -2.2% (tie, not up), native blocks/s -9.4% (borderline), RSS +1.5 GB peak
  (RAM gate not met: bound ~270 MB). Restart cells: not in this campaign. Owner decision.
- **Follow-up (18c s112, after the p3s2g gate).** Both branches now merge main `8efcb07a`
  (step 1 `b1605417`, step 2 `303f2eb1`): the next gate has no 16-commit confound.
  - **Untimed exec time (47.1, +36 ms per block).** Cause: `checkpoint_upkeep` spawns the merge of
    block n-1 into L at the end of block n, and `checkpoint_read_layers` joins it at the start of
    block n+1, before the overlay is built. Nothing runs in between, so E waits for the whole merge
    thread, and no phase timer covers that wait (the pipelined path never observes its
    `flush_timer`). The join also waited for step 2's ingress `slice` and, in step 1, for the drop of
    block n-1. E itself freed the sets a slice replaces (step 2: in the mempool's view swap, under its
    lock), C once durable, and the block handles of a checkpoint-time merge. Ranking (throwaway
    probe, synthetic blocks of section-41 size x2, thread CPU time; **18c-local, loaded host,
    ranking only**, not for sizing): merge first; then the ingress slice and freeing a block's full
    set, about equal; then freeing L at a checkpoint (per block, smallest).
    New series: `torus_exec_checkpoint_settle_wait_seconds` (the join) and
    `torus_exec_checkpoint_upkeep_seconds` (end-of-block upkeep), each once per pipelined block, and
    the counter `torus_exec_checkpoint_drop_inline` (items E freed itself because the free thread's
    queue was full; 0 while it keeps up). The harness counts both series in `residual_untimed`
    until `summarize.py` `PHASES` lists them.
    Built (no change to L, slices, the view's answers, state or replay): the merge thread hands L
    back first, then slices and lets go of the block. Slices are installed at the end of the next
    block (`install_slices`). A `torus-ckpt-drop` thread (queue 2, past it E frees inline) frees C,
    replaced sets and the previous view. Expected effect (qualitative; archy measures it): the
    settle wait drops by the slice (step 2) and the drop of n-1 (step 1), and E no longer frees
    full sets, old views or C (`upkeep` / settle smaller) unless `drop_inline` counts. Left: the
    merge itself, the largest item. It overlaps block n+1 only if L can be read while it is merged. Option: per-CF `RwLock` on L with
    batched merges, block n-1 read as its own layer above L (a read is then correct at any point of
    the merge). Read path: consensus-adjacent, needs its own tests. Other options: move the
    entries instead of cloning when the frozen set is unique (step 1 only), or a persistent map.
  - **Cancel-all expiry (47.3).** Not a DB lag: nothing on the pool's residency path reads state
    (eviction by nonce age, removal at commit on the consensus thread, a height-bounded in-flight
    ledger, selection reads no state). The item C walk takes at most `ceil(limit x 25%)` cancels in
    `(sender, nonce)` order, and leftover slots reach the other cancels only when phase 2 neither
    fills the block nor stops at a byte or order budget. With the cancel demand per block at the
    share, a younger cancel of a lower-address sender passes an older one of a higher-address sender
    every block. Phase 3 only moved the operating point (fewer, larger blocks). Repro:
    `native_pool.rs` `cancel_share_never_starves_an_older_cancel` (ignored, fails today). **Owner
    options:** (a) oldest-first cancel phases (a cancel index by nonce; each sender stays in nonce
    order; changes block content, bench it, see the s66 note on `SortKey`); (b) a higher
    `TORUS_CANCEL_BLOCK_SHARE_PCT` (weaker item C); (c) per-sender round-robin in the cancel
    share; (d) accept it (the bench's 36.7% cancel-all mix is above the 25% share; the in-flight
    estimate stays conservative).

### Step 5 (only if the owner asks): RPC readers on the view (Option A)

About 40 call sites in `torus-rpc` (raw `.inner()` iterators need merged-iterator versions; revm
`DatabaseRef` over the view). Separate plan.

### Step 6: compiled default (owner, after the gate)

Validators get 15 s (or the gate's pick); `--rpc-only` nodes get 0. Same pattern as 9.13 compiled
defaults.

## 5. Not building

- Change-set log file (D5 c); own snapshot format (D7 b); LtHash snapshot trust (D8 b); P2P
  snapshots (D9 b); automatic snapshot creation (`SnapshotManager` stays unwired); nonce windowing
  (D12 b); in-memory EVM (D11 c).
- Interval with modes 2/3 deferred book save (Classic on testnet; mode 3 revisit before
  mainnet, 9.13).

## 6. Open decisions

1. Decided (owner s109): interval > 0 with book mode 2/3 refuses to start (task 1.2). If mode 3
   is chosen before mainnet (9.13), W's pass 2 must read through the layer (separate task then).
2. Who builds steps 1-3: 18c builder or archy? Step 1 is L-sized and consensus-adjacent.

## 7. Rollback

Everything is behind `TORUS_CHECKPOINT_INTERVAL_MS`: unset = today's per-block path byte for byte.
Rollback = unset the knob, no data migration. A node restarted with the knob unset replays
(S, committed] serially as today. Task 1.1 (pruner clamp) stays either way.
