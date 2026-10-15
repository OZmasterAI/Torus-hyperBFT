# Experiment s58: epoch-boundary fast path (`TORUS_EPOCH_FAST_PATH=1`)

Branch `exp/archy-s58-epoch-boundary-fast-path`, base `exp/archy-s58-perf-search`
(= main `a44000e0`). One file of code: `crates/torus-consensus/src/app.rs`.
Default off. Not benched yet.

## What changed

With `TORUS_EPOCH_FAST_PATH=1`, an epoch-boundary block that passes every other
fast-path term (flush worker attached, header and body dispatch-durable, no EVM
tx, no buffered slash) takes the pipelined fast path instead of
`pipeline_barrier`. With a checkpoint policy set, every boundary forces a
checkpoint.

1. `parse_epoch_fast_path(Option<String>) -> bool` (only a trimmed `"1"` turns
   it on) and `epoch_fast_path_enabled()` (a process-wide `OnceLock` over the env,
   logged once). Both sit next to `async_post_flush_enabled`.
2. `ExecutionContext::epoch_fast_path: bool`, set from `epoch_fast_path_enabled()`
   in both struct literals: production `TorusApp::new`, and the test helper
   `make_exec_ctx_with_mempool`. In the test helper it is false unless the env
   is set, so an env rerun covers every test context. Unit tests set the field
   directly.
3. The fast-path predicate in `execute_committed_block_with`. Its last term
   `!is_epoch_boundary(height, epoch_length)` becomes
   `(self.epoch_fast_path || !is_epoch_boundary(..))`. Every other term is
   unchanged. EVM and slash boundaries stay serial, and so does replay (no flush
   worker exists during replay).
4. The `due` expression in `checkpoint_upkeep_with` gains
   `|| (self.epoch_fast_path && is_epoch_boundary(height, epoch_length))`. This
   term is required for liveness; it is not an optimisation (see Risks).
5. Test-only `test_barriers: AtomicU64`, incremented in `pipeline_barrier`
   right after the no-worker early return.

The rest of the boundary path is unchanged:

- `process_epoch_boundary` still writes only through `ctx.staking` on the block
  overlay.
- No raw-`StateDb` reader or writer was added to the boundary tail.
- The h%100 attest checkpoint is still submitted (now asynchronously).
- RPC sees the boundary's rows when its checkpoint lands, like any pipelined
  block.

## Why (evidence, main `a44000e0`)

Bench and testnet genesis use `epoch_length = 100`, so every h%100 block was a
barrier. That barrier did three things:

- `checkpoint_drain` closed L into a forced checkpoint of about 100-130 MB.
- The exec thread waited in `wait_idle` until W had written that checkpoint.
- The block then ran serially, with its own flush of about 55-73 MB on the exec
  thread.

Measured in the logs of `hsdb-300m-r{1,2,3}-B` (memories 865d2bfa, c3c714d2,
7d1b757a), the gap is from `block done` of h-1 to `executing finalized block` of
h:

- The gap before each boundary block is 499-533 ms. For other blocks it is
  0.6 ms.
- The boundary block runs 120-160 ms longer than a pipelined block (serial
  flush).
- Net cost: about 0.47-0.56 s per epoch per node, out of about 32 s, which is
  1.6-2.0% of exec time at 300m 600 s.

The original reasons for the barrier no longer hold:

- **F2 (EVM accounts).** W resyncs the dirty EVM accounts after the checkpoint
  lands (Digest jobs carry `evm_addrs`, exec_pipeline.rs:818/872).
- **F8 (validator-set plan).** Bug (b) removed the consensus-thread staking
  writer. `epoch_validator_set_updates` reads the plan only once the durable
  applied marker reaches H-L, and refuses the boundary with `EpochPlanNotReady`
  until then.

## Tests (all in `mod crash_recovery_tests`)

- **(a)** `epoch_fast_path_skips_boundary_barriers_and_checkpoints_boundaries`.
  Setup: the barrier fixture (epoch length 4, EVM tx at 6, slash at 10),
  checkpoints never by time.
  - Flag on: barriers only at 6 and 10. Flag off: at 4, 6, 8, 10 and 12.
  - The durable marker after every block equals the flag-off one: the last of
    4/6/8/10/12 at or below h.
  - RED first (barriers at boundaries). It then failed again with the predicate
    changed but the `due` term left out (markers None, None, None, None, None,
    6, ..., 10), and passed once the term was added.
- **(b)** `epoch_fast_path_state_identical_to_flag_off`. Flag on against flag
  off, under per-block flush, every 5 blocks, and never by time. Equal, and
  equal to the serial run:
  - per-height write sets, running hash and full CF dump;
  - the durable marker after each block;
  - CF_ACCOUNTS and CF_HASHED_ACCOUNTS after every block, once W has drained.
- **`epoch_fast_path_keeps_evm_and_slash_boundaries_serial`.** EVM tx at
  boundary 8 and slash at boundary 12. With the flag on they still barrier
  (barriers at 8 and 12; boundary 4 does not barrier). State is equal to flag
  off.
- **(c)** `epoch_rotation_identical_across_exec_lag` and
  `running_hash_epoch_rotation_identical_across_exec_lag_with_validator_rows`
  now also run pipelined nodes with the flag on, under per-block flush and
  checkpoints never by time, at every lag. They must give the same
  ValidatorSetUpdates, staking rows, pending rotations, write sets and running
  hash as the serial nodes. Before reading the plan, a pipelined node waits for
  W, as a live node retries a refused boundary. Without the `due` term the
  never-by-time nodes fail with `EpochPlanNotReady`.
- **(d)** `epoch_fast_path_plan_not_ready_until_boundary_checkpoint_lands`.
  Setup: flag on, W parked (WorkerGate) on boundary 4's checkpoint.
  - E is not held at boundary 4.
  - `epoch_validator_set_updates(8)` returns `Err(EpochPlanNotReady)`, and the
    marker is still None.
  - After release, it returns the flag-off updates (the rotated key).
- **(e)** `epoch_fast_path_checkpoint_crash_replay_matches_uncrashed`. The P2
  run (130 blocks, epoch length 50, checkpoint every 5) with the flag on and
  the 6 standard crashes, plus two crashes whose blocks contain a fast-path
  boundary:
  - crash 49: the boundary's checkpoint 50 is in flight when W fails;
  - crash 99: W is parked on 99's digest while E runs boundary 100.

  Write sets, running hash and CF dump equal the uncrashed flag-off per-block
  run. With the flag off these two crashes cannot run, because the boundary
  barrier waits for the parked W.
- **(f)** `epoch_fast_path_flag_parses_trimmed_one_only`.

New test helpers: `ckpt_barrier_run_with` / `BarrierFixture` / `BarrierRun`,
`p2_run_fast` / `p2_run_full`, `ckpt_crash_replay_with`, and `epoch_exec_ctx` /
`settle_exec`. Existing callers are unchanged.

### Boundary-serial asserts under `TORUS_EPOCH_FAST_PATH=1`

With the env unset, torus-consensus passes in full (397 of 397). With
`TORUS_EPOCH_FAST_PATH=1`, 393 of 397 pass. Each of the 4 failures pins serial
behaviour at a boundary, which is exactly what the flag removes:

1. `exec_pipeline_state_identical_over_sequence` asserts that W received exactly
   heights `[1, 2, 3, 5, 6, 7, 9, 10, 11, 13]` ("boundaries 4/8/12 were serial
   barriers (F2/F8)"). With the flag, W also receives 4, 8 and 12. The state
   equality it checks next is covered by test (b), on the same fixture blocks
   plus an EVM tx at 6 and a slash at 10.
2. `exec_pipeline_deferred_books_end_to_end_match_serial` pins the same
   received-height list (deferred book modes 2 and 3).
3. `ingress_view_follows_checkpoints_and_barriers` asserts that boundaries
   4/8/12 drop the ingress view and are durable at h ("barrier -> DB"). With the
   flag, the boundary hands its forced checkpoint to W and publishes a view
   instead. The ingress dump, which that test checks first at every height,
   still equals the per-block reference at h4.
4. `r01_epoch_boundary_flush_failure_fail_stops_and_restart_matches` injects the
   write fault through `test_fail_flush_at`, which only hits serial flushes,
   at boundary 4. With the flag, block 4's write goes to W, so the seam never
   fires and nothing latches. A failure of W's write at or around a boundary is
   covered by test (e) (crashes 49 and 99) and by the existing CrashAfter and
   CkptCrash runs.

A fifth test failed only through timing, and the test was fixed.
`state_hash_mismatch_differing_vote_reported_on_every_node_and_never_writes`
read boundary 100's checkpoint row right after executing it on a pipelined node,
which relied on the barrier. `node_at_checkpoint` now waits for W before the
read (`settle_exec`). With the flag off this is a no-op, because W is idle after
the barrier.

## Flag

`TORUS_EPOCH_FAST_PATH=1`:

- node-local, read once per process, copied into the execution context at
  construction;
- unset or anything other than a trimmed `1` means today's behaviour.

Validators with different values agree: the same blocks give the same state (test
(b)). Only the timing of durability differs, and consensus already waits for the
durable marker.

## Expected gain (300m 600 s)

- matched/s +1.1 to +1.7% (central +1.4%). Native blk/s about the same.
- exec block_ms -1.4 to -1.7% (residual_untimed plus exec_flush).
- Device writes about -1 GB per node per 600 s, because the serial flush is
  folded into the h%100 checkpoint.
- CPU-s/1M 0 to -0.3%. Block p99: small.
- 120 s gate shape +1.6 to +2.2%; 10m depth 4 about +1%; 10m depth 1 about 0.

The gain needs short epochs. It is about 0 at the node default of 100,000, and
boundaries that carry EVM txs stay serial.

## How to bench it

A/B on the same binary (flag only), 300m 600 s, n >= 4 paired:

```
tools/matched-bench/run-cell.sh <wt> efp-off-300m 300 600 <rate> ''
tools/matched-bench/run-cell.sh <wt> efp-on-300m  300 600 <rate> 'TORUS_EPOCH_FAST_PATH=1'
```

Plus a 120 s gate shape and 10m depth 4. Check:

- AGREE in every cell.
- `pipelined=false` on h%100 in the `executing finalized block` lines drops to 0.
- The gap `block done` h-1 -> `executing finalized block` h at h%100 falls from
  about 500 ms to under 50 ms.
- `exec_flush_seconds_count` about 0; `torus_exec_checkpoints` unchanged.
- Zero `validator-set plan not ready` warnings; consensus timeouts not up.
- matched/s paired-CI lower bound >= 0, or a positive central value with the
  mechanism confirmed.

## Risks

- **Liveness (handled).** The boundary `due` term is mandatory. For an epoch
  length that 100 does not divide, the plan written at H would otherwise stay
  in L. Consensus refuses H+L until the durable marker reaches H. Checkpoint
  triggers run only per executed block (there is no idle checkpoint), so the
  chain would halt. Tests (a), (c) and (d) fail without the term. The fix never
  relies on 100 dividing the epoch length.
- **Plan durability timing.** The plan becomes durable when the boundary's
  checkpoint lands, at about the same wall time as today: a fast exec plus an
  async checkpoint, against drain + serial exec + flush. The exec queue is
  much shorter than L, so boundary refusals should stay at zero. The bench
  check above confirms it.
- **Checkpoint slicing (18c follow-up).** Slicing must keep the applied marker
  in the last slice, so the plan rows are durable no later than the marker.
- **Item 7 step 1 (18c, unpushed)** edits the predicate area: a one-line
  rebase. EVM-lane boundaries stay serial through `!has_evm`.
- **Paused propose-defer branch** edits other app.rs hunks (~:806, :5750,
  :5971, :7577), so no overlap is expected.
