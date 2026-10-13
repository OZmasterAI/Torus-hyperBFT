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
| Phase gate, restart | restart -> ready <= 60 s (SIGKILL val1, multi-crash cells). **Final gate (section 62, `c88d3073`): SIGKILL val1, 4 kills per arm: kill -> val1 first commit 29.9 s mean / 39.5 s max (main 27.2 / 31.95), replay complete from the marker, state hashes agree: met** (section 61: SIGTERM 35.0 s) | step 4 |
| Phase gate, throughput | matched/s UP vs tw beyond noise; native blocks/s no regression (owner s111 after p3t, section 43: on tmpfs the prototype's blocks/s gain is +1.6%, not resolved, so "up" is not required) | step 4 |
| correctness | per-block write sets, `h_n` and full CF dump identical: interval 15 s vs 0 vs 1 block, crash at random points (incl. mid-checkpoint), 3 replicas with different intervals | tests, step 1 |
| RAM | a 10 min soak shows RSS levelling off, with a Phase 3 delta <= ~1 GB per node; RSS still growing at the end of the soak blocks the merge (owner s114; was "RSS growth within archy's layer-size figure + 25%", ~270 MB). Which allocator the soak is judged under (glibc fails, jemalloc levels off; sections 53, 53b, 53d) is decided after archy's benches finish. Je vs je (section 57, both arms jemalloc compiled in, no exec-backlog confound): Phase 3 levels off at +0.94 GB per node mean / +1.00 at 10 min / +1.27 peak: met on the level and the mean, not at the peak. Shrinking the layer (~2.1x its logical size) and the view is a follow-up, reconsidered at Phase 4. **Final gate (section 61, step 2 `c88d3073` vs main `de1a97c1`, both je): levels off, but +1.43 GB per node mean / +1.80 peak in the soak: not met** (live heap +0.50-0.56 GB as in 57; the rest is jemalloc-resident slack). **Owner s118: judged on the live heap, accepted.** Section 62: `MALLOC_CONF=background_thread:true,dirty_decay_ms:1000` brings s2's soak RSS to main's level (-1.72 GiB per node; +4.6% CPU-s/1M, n = 1, env setting only) | soak cells, step 4 |

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
`docs/validator-home-network.md`).

- Test first: `ingress_view_matches_interval_zero`. A session created at n, a balance credited
  at n, an EVM credit at n; gossip admission at n+1 accepts with interval "never" exactly as with
  interval 0. Fails before (session not found).
- Verify: `cargo nextest run -p torus-mempool -p torus-consensus $F -E 'test(ingress_view)'`.

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
- **RSS split** (campaign `ozarchy-p3rss`, results doc section 48, plan-2 row 53): of step 2's +1.55 GB peak, the
  commit-mode exec backlog (not Phase 3) is +0.83 GB, the step 1 layer +0.88 GB and the step 2 view +0.68 GB (vs control
  `2725e321`); about half of each Phase 3 delta is glibc retention (jemalloc live heap +0.51 / +0.45 / +0.33 GB). RSS
  still climbs at 120 s. Next: allocator A/B and a ~30 min soak.
- **Gate rerun** (campaign `ozarchy-p3g2`, results doc section 49, plan-2 row 54): step 2 `51a67f57` vs main `8efcb07a`
  (clean control), commit tail, n = 4. CPU-s/1M -7.0% (met), writes -51% (met), matched/s +2.9% (tie), native blocks/s
  -5.5% (not met), RSS +1.55 GB peak (not met), exec ms per block flat, block-time p99 +12.7%. New timers: E waits
  23.9 ms per block on the previous layer merge (`checkpoint_settle_wait`, p99 95 ms), upkeep 1.0 ms; free-off-exec
  confirmed (`torus-ckpt-drop`). Also valid for `6d9b78af`. Restart cells: SIGTERM cells on the next step 2 head.
- **Allocator A/B** (campaign `ozarchy-p3alloc`, results doc section 51, plan-2 row 56): step 2 `7b006739` vs main
  `219f0c20`, glibc vs jemalloc 5.3.1 (LD_PRELOAD, defaults), commit tail, n = 4. Phase 3 RSS delta +1.55 GB peak (glibc)
  / +0.97 GB (jemalloc): the RAM gate (~270 MB) is not met under either. jemalloc on step 2: peak -0.32 GB, growth
  1.64 -> 1.01 GB/min, matched/s +3.6%, but CPU at equal fills +4.0% (je commits +15-18% faster, so the closed-loop
  bench submits +13-14% more: offered load differs). CPU-s/1M -6.5% / -8.9% and writes
  -54% / -51% (Phase 3, glibc / jemalloc). Soak (600 s per arm) on glibc next.
- **SIGTERM restart cells** (campaign `ozarchy-p3term`, results doc section 52, plan-2 row 57): step 2 `7b006739` at 15 s vs
  main `219f0c20`, 8 + 4 SIGTERM kills: replay 0 every time, no forced stop, state agrees; exit 7.0 s mean / 10.7 s max
  (exec-queue drain; checkpoint flush share ~0.15 s); kill -> first commit max 59.8 s (gate 60 s met; the slow kill is
  the gossipsub resubscribe stall, both arms).
- **Soak** (campaign `ozarchy-p3soak`, results doc section 53, plan-2 row 58): step 2 `7b006739` vs main `219f0c20`, glibc,
  600 s each: step 2 RSS still +40-67 MB/min per node at 10 min (main +3-11), Phase 3 delta +1.98 GB per node at 10 min:
  no plateau (growth finding). Cancel-all expiry hits both arms from minute 2 (~18k per node), matched/s -30% from minute 6.
- **Soak on jemalloc** (campaign `ozarchy-p3soakje`, results doc section 53b, plan-2 row 59): the same pair on jemalloc 5.3.1
  (defaults): step 2 RSS flat after ~2 min, Phase 3 delta +0.68 GB per node (minutes 5-10 mean; +1.01 GB peak). So the
  glibc growth is consistent with allocator retention (one pair; slow growth not ruled out). Layer size and resting orders do not grow. The minute-6 matched/s drop is margin
  rejections in the econ workload.
- **Soak with the cancel-share fix** (campaign `ozarchy-p3cssoak`, results doc section 53c, plan-2 row 60): cs2 `41a95355`
  (`de97682f` on step 2) vs step 2 `7b006739`, glibc, 600 s each: cancel-all expiry 0 (step 2 16k per node) and no margin
  collapse (the margin user is positions, concentrated on low-address senders by the old cancel order). Step 2 + fix RSS
  still grows (+129-178 MB/min, +2.76 GB vs section 53's main), against a confounded reference: the fixed arm keeps ~2x the
  book and layer size for 10 min. Next: cs2 vs main + fix `de97682f`.
- **Fair RAM soak, fix in every arm** (campaign `ozarchy-p3fsoak`, results doc section 53d, plan-2 row 61): main + fix
  `de97682f` (glibc, control) vs `41a95355` on glibc and on jemalloc, 600 s each, full workload in all three (0 expiries,
  0 margin rejections). Glibc: Phase 3 delta +2.62 GB per node (minutes 5-10 mean), still +120-158 MB/min: rule not met.
  jemalloc: flat, +1.10 GB per node mean / +1.49 GB peak (val2 +0.95 / +1.34): met on the level, borderline on the mean,
  not at the peak. Same book and layer, so the glibc growth is consistent with retention, not data.
- **Merge-wait A/B** (campaign `ozarchy-p3mw`, results doc section 54, plan-2 row 62): `perf/p3-merge-wait` `3cf8f72d` vs
  step 2 `7b006739`, both 15 s, glibc, commit tail, n = 4 ABBA. E's `checkpoint_settle_wait` 23.4 -> 3.1 ms per native
  block (-20.3 [-20.9, -19.6]), deferred read waits 0, deferred join 0.19 ms per block (inside upkeep); `block_ms` -6.9%,
  native blocks/s +11.3% [+8.4, +14.1]; matched/s +1.8% and block time p99 -2.9% (ties); RSS / writes neutral; CPU-s/1M
  +1.35% [+0.09, +2.61] (not resolved at equal fills). Three REJECTs, all the mark-only digest artifact.
- **Checkpoint interval 5 s vs 15 s** (campaign `ozarchy-p3iv`, results doc section 55, plan-2 row 63): the same step 2
  `7b006739` binary, glibc, commit tail, n = 4 ABBA, 120 s. Interval confirmed per cell (4.4 vs 11.6 checkpoints per node
  per minute in the bench window; layer per checkpoint 114 -> 74 MB). RSS -0.33 GB peak [-0.65, -0.01] / -0.28 GB mean
  [-0.50, -0.06] per node (~6%), growth rate unchanged; CPU-s/1M +1.10% [+0.44, +1.75]; device writes +10.8% [+7.0, +14.6]
  (WAL path); blocks/s, matched/s, block time p99 ties. Minutes 0-2 only, not the soak plateau. Three REJECTs (g2), all
  the mark-only digest artifact.
- **jemalloc compiled in (RAM-gate dependency)** (campaign `ozarchy-p3je`, results doc section 56, plan-2 row 64):
  `feat/jemalloc-allocator` `88ddc4c2` vs main `deeacdb3` (glibc), no Phase 3 in either arm, commit tail, n = 4 ABBA,
  120 s, saturated and at a fixed 907 actions/s. RSS growth over minutes 0-2 is about halved (-44% / -54%), the
  fixed-load RSS mean -0.22 GB [-0.30, -0.14] per node and end-of-window -0.36 GB, at CPU-s/1M +2.7% (saturated) /
  +4.3% (fixed load, resolved). matched/s is not higher (tie saturated); blocks/s +10-15%, block time p99 -7 to -9%.
  Part B (10 min soaks ja / jb and je-vs-je jcs `e7be8460` / jmf `0360c840`) decides the RAM gate under jemalloc;
  its binaries are staged in `ozarchy-p3je-stage/{jcs,jmf}-run`.
- **RAM gate, je vs je soak** (campaign `ozarchy-p3jesoak`, results doc section 57, plan-2 row 65): jcs `e7be8460` (step 2
  + fix + jemalloc, 15 s) vs jmf `0360c840` (main + fix + jemalloc), 600 s each, full workload in both (0 expiries, 0 margin
  rejections), exec queue at the cap in both. Phase 3 delta +0.94 GB per node (minutes 5-10 mean; +0.86 to +0.98 per node),
  +1.00 GB at 10 min, +1.27 GB peak; difference slopes over the last 5 min +43 [-4, +90] / -80 [-134, -27] / +8 [-49, +65]
  MB/min: levels off. ~+0.55 GB live allocations, ~+0.45 GB jemalloc-resident slack. **RAM rule: met on the level and the
  mean, at the bound at 10 min, not met at the peak (1.05-1.55 GB).** One cell per arm. Secondary ja vs jb (main, no fix):
  no RSS saving at 10 min (+0.15 GB), matched/s minutes 6-10 vs 0-2 -19.9% vs -35.1%.
- **FINAL Phase 3 gate** (campaign `ozarchy-p3gate`, results doc section 61, plan-2 row 69): s2 = `c88d3073` (step 1 +
  step 2 + merge-wait, merged with main `de1a97c1`, 15 s) vs m = main `de1a97c1`, both jemalloc compiled in, node and bench
  built per arm, CLOID_SHARE=0, full workload, commit tail. SAT n = 4 ABBA 120 s: CPU-s/1M -9.72% [-10.46, -8.98] (equal
  fills -8.39%), writes -54.8% [-56.9, -52.7], matched/s +8.8% [+5.2, +12.4], native blocks/s +20.1% [+10.3, +30.0], block
  time p99 +17.7% [+6.7, +28.7]; RSS mean +0.99 [+0.67, +1.30] / peak +1.28 [+0.82, +1.74] GB. Soak (600 s, one per arm):
  CPU -12.4%, writes -64%, matched/s +13%; RSS levels off (s2 last-5-min slopes -77 / -66 / -10 MB/min, difference slope
  CIs include 0) at **+1.43 GB per node (minutes 5-10 mean), +1.80 peak**; live heap +0.50-0.56 GB (= section 57), the rest
  jemalloc-resident slack. SIGTERM restart: kill -> first chain commit 35.0 s (m 37.3 s), replay 0; no SIGKILL multi-crash cells on this
  head (last: section 46, step 1). **Gates: CPU, disk, throughput, restart (SIGTERM only), RAM levels off PASS; RAM size (<= ~1 GB) FAIL in the soak (mean and peak).** Owner decision pending;
  suggestions (not measured): judge on the live heap, jemalloc decay tuning + soak rerun, n = 2-3 soaks.
  **Owner (s118, via 18c): RAM accepted on the live heap; Phase 3 merges once the SIGKILL restart gate passes.**
- **SIGKILL restart gate + jemalloc decay soak** (campaign `ozarchy-p3kd`, results doc section 62, plan-2 row 70): same
  binaries as the final gate. SIGKILL val1 under load, 2 cells x 2 kills per arm (ABBA), state hash on: kill -> val1 first
  commit **s2 29.9 s mean / 39.5 s max** (m 27.2 / 31.95); replay from the marker + 1, contiguous (s2 72-113 blocks from the
  last 100-block checkpoint, m 61-65 = the exec queue); 121 state-hash checkpoints on all 3 validators, 0 disagreements.
  ~2x section 46's times in both arms (replay 2.4-4.0 blocks/s at this workload). **Restart gate (SIGKILL) PASS; with the
  live-heap RAM decision every Phase 3 gate is met.** Soak pair on s2: `MALLOC_CONF=background_thread:true,dirty_decay_ms:1000`
  cuts RSS 6.25 -> 4.53 GiB per node (minutes 5-10 mean; peak 6.91 -> 5.19), live heap unchanged, for +4.6% CPU-s/1M and
  -2.5% matched/s (n = 1 per arm). An env setting only; a compiled default is a suggestion, not done.

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
