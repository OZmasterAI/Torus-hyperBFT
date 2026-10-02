# Consensus bug (b): epoch rotation runs on the consensus thread

Status: fixed on `fix/consensus-determinism`, always on from genesis (owner,
s84: no live chain, every chain starts from a fresh genesis, so the activation
switch and the legacy consensus-thread path were removed). `cf_staking_validators`
is hashed by the running state hash again. Line numbers in Problem / Evidence
are for `main` @ d623d3c.

## Problem

At every epoch-boundary height H, the consensus thread (propose / validate of
block H, long before H executes):

1. writes the DB: `apply_pending_rotations` (validator pubkey + deletes the
   `pending_rotation:` row) and `update_validator_statuses` (Active /
   Candidate on every validator row);
2. computes the hotstuff `ValidatorSetUpdates` for H from the staking rows it
   READS at that moment, diffed against an in-memory `last_validator_set`.

The execution thread lags consensus by a variable number of blocks (exec queue,
deferred holes, restart replay). So:

- (b1) **write race**: execution read-modify-writes whole validator rows
  (stake, commission, jail, slash, rewards) through its overlay; the consensus
  thread's direct writes land at a timing-dependent point in between. A row
  written by execution after the consensus write carries or drops the rotated
  pubkey / status depending on timing. Two writers of statuses: execution's
  `process_epoch_boundary` also recomputes and writes statuses at H.
- (b2) **read race -> divergent validator sets**: the set returned to hotstuff
  depends on how far each replica's execution has progressed. hotstuff_rs takes
  each replica's OWN `validate_block` answer (no cross-check), so replicas can
  install different validator sets: a safety failure, not only a state
  divergence.
- (b3) `last_validator_set` (the diff base) is in memory, initialised at boot
  from the staking rows of whatever height the DB is at
  (`compute_new_validator_set` at boot) - not the set hotstuff actually runs.
  A restarted replica diffs against a different base.

`cf_staking_validators` was EXCLUDED from the running state hash because of
(b1) (`crates/torus-state/src/running_hash.rs:29-37` on `main`, id 8 reserved);
it is hashed again since the fix (see below).

## Evidence (main @ d623d3c)

- `crates/torus-consensus/src/app.rs:4181-4291` `epoch_validator_set_updates`:
  `apply_pending_rotations` at `:4195`, `compute_new_validator_set` at `:4199`,
  diff vs `self.last_validator_set` at `:4216-4229`, `update_validator_statuses`
  at `:4239`.
- Called from `finish_validate` (`app.rs:4823`) and `build_proposal`
  (`app.rs:4946`) - consensus thread, proposal / vote time.
- `app.rs:3397-3402` + `:3557`: `last_validator_set` = staking rows at boot.
- `crates/torus-bridge/src/native_executor.rs:6061-6139`
  `process_epoch_boundary`: execution ALSO computes a set and writes statuses
  (second writer), but never applies key rotations.
- `crates/hotstuff_rs/src/hotstuff/implementation.rs:1390-1440`: the replica's
  own `validator_set_updates` go into its block tree; vote phase depends on
  `is_some()`.
- Existing test documenting the race:
  `app::crash_recovery_tests::running_hash_consensus_thread_epoch_write_vs_lagging_exec_equal_across_nodes`.

## Options

1. **Exec plans, consensus applies one epoch later** (chosen). All writes move
   to execution, inside the boundary block's flush batch. Execution of boundary
   H computes the rotation for the NEXT boundary H+L from its own (deterministic)
   post-state and stores it as a consensus row ("plan"). Consensus at boundary
   H+L only READS that plan (after its execution has applied H) and turns it
   into hotstuff updates; execution of H+L applies the same plan to the staking
   rows (statuses + key rotations). hotstuff and staking switch at the same
   height, from the same bytes.
   + Deterministic: every input is execution state at a fixed height.
   + No new block field; hotstuff_rs unchanged.
   - Validator-set changes take effect one epoch later than today (stake at
     boundary H decides the set from H+L on).
   - Consensus at a boundary needs `applied >= H-L`. If execution lags more
     than one epoch the boundary block is refused (`MissingData` on validate,
     a datum-less refusal on propose) until it catches up: a liveness pause at
     boundaries, never a safety issue. Testnet L = 100 vs exec queue <= 64.
2. **Snapshot at H-D, small D**: same as 1 with the plan computed D blocks
   before the boundary (D > pipeline depth). Faster changes, but stalls the
   boundary whenever exec lag > D, which the perf regime hits routinely.
3. **Plan in the block**: the proposer puts the set in the block (header field
   or action), validators verify against their own deterministic computation.
   Same determinism requirement as 1 plus a wire change; no gain.
4. **Block consensus until exec reaches H-1**: impossible - H-1 commits only
   after H is voted (pipelined HotStuff), deadlock.

## Design (option 1, as implemented)

Rows (CF_CONSENSUS_META, hashed prefix `epoch_vset:`, written only by
execution, plus the genesis record of `current`):

- `epoch_vset:plan:` ++ H (8 BE): borsh `EpochRotationPlan { boundary, members
  [(address, pubkey, power, commission)], deletes [pubkey], rotations
  [(address, new_pubkey)], changed }` - written at exec of H-L, applied at
  exec of H, pruned at exec of H+L (kept one boundary longer so a late
  re-validation of H still reads it).
- `epoch_vset:current`: borsh member list of the set installed in hotstuff:
  the genesis set (written by `Genesis::initialize`,
  `StakingManager::record_genesis_epoch_set`: all genesis validators, ranked
  like a computed plan), then the set of the last applied plan.

Execution of boundary H, after rewards. Every boundary block runs the native
phase, even when empty (on `main` an empty block skips it, so a boundary could
silently skip its epoch processing):
1. if `plan(H)` exists: apply its key rotations (pubkey + delete the pending
   row), set statuses (members Active, others Candidate, Jailed / Tombstoned
   untouched), write `current`; prune `plan(H-L)`.
2. compute `plan(H+L)`: due rotations (`effective_epoch <= epoch(H+L)`)
   substituted in a scratch copy; `compute_new_validator_set`; rotation cap +
   BFT floor vs `current`; minimum-set failure => `changed = false`; `deletes`
   = every registered pubkey (old keys of rotating validators included) not in
   the new member keys. `changed` = members differ from `current`.
3. this is the ONLY writer of epoch statuses and rotated keys (the old Phase B
   status recompute, `apply_pending_rotations` and the consensus-thread
   rotation are deleted).

Consensus at boundary H: no DB writes. If the durable applied height < H-L:
not ready (validate -> `MissingData`, propose -> refuse). Else `plan(H)`
absent or `!changed` -> `None`; else inserts = all members, deletes =
`plan.deletes`; `last_validator_set` := members.
Boot: `last_validator_set` := `current` (the staking rows only on a DB not
built from a genesis, i.e. tests).

Genesis / first boundary: the first boundary L has no plan (no change at L);
execution of L writes `plan(2L)` against the recorded genesis set, so an
unchanged set is no hotstuff update. (Before the genesis record, the first
plan was always `changed`: a spurious validator-set update at 2L on every
fresh chain, and cap / floor used the Active rows at L instead of the set
hotstuff runs.) A DB without a recorded set diffs against an empty set.

## Owner decisions (s84)

- D-b1 ACCEPTED: one-epoch delay of validator-set changes (option 1).
- D-b2 DROPPED: no activation height. No live chain exists and every chain
  starts from a fresh genesis, so the new rules apply from genesis; the
  switch (`consensus.epoch_rotation_activation_height`) and the legacy path
  were removed.
- D-b3 ACCEPTED: rotations submitted during epoch k take effect at k+2.
  Pending rows are applied late, never lost (`<=`).
- D-b4: jailed / tombstoned members stay in hotstuff until the next planned
  boundary (same as the old gap, now deterministic).

## `cf_staking_validators` in the running state hash (done)

Every validator-row write now happens on the execution thread inside the
block's flush (overlay -> flush batch; epoch rotation via
`process_epoch_boundary`), so the CF is deterministic per height and is
hashed whenever the running state hash is active (existing
`state_hash_activation_height`, no new activation). It reuses `cf_id` 8 (the
id reserved for it): no chain ever hashed id 8, and there is no chain history
to preserve. Writer audit (all `StakingManager::put_validator` callers):

- native executor (`NativeExecContext.staking` over the block overlay): all
  staking actions, jail votes, rewards, epoch boundary -> flush batch;
- buffered slashes (`app.rs` slash loop): per-block overlay + one `commit_tx`
  before the flush, recorded by the out-of-batch recorder into the same
  height's hash (not in the flush batch itself; nothing produces pending
  slashes since bug (a));
- genesis (`Genesis::initialize`): before any block, identical by genesis
  file, not part of the hash;
- RPC, state-hash monitor, async validate, consensus thread: read only.

Tests: `running_hash_covers_validator_rows` (torus-state),
`running_hash_epoch_rotation_identical_across_exec_lag_with_validator_rows`
(3 replicas, exec lag 1 / 2 / 4, key rotation + Active -> Candidate at 8 +
commission change at 6: identical updates, write sets and hash; the write sets
carry the DB's validator rows), `running_hash_corrupted_validator_row_diverges`.

## Tests

- RED on main / GREEN after (torus-consensus
  `epoch_rotation_identical_across_exec_lag`): two replicas whose execution
  lags 1 and 3 blocks behind boundaries 4 and 8 (a key rotation in block 2)
  hand hotstuff the same updates, end with the same staking rows and pending
  rotations, the rotation is installed at 8, and the consensus thread writes
  nothing (full-CF dump before / after each boundary call). RED on main: lag 1
  rotated at 4, lag 3 never.
- `epoch_rotation_plan_not_ready_until_previous_boundary_applied`: applied <
  H-L -> `Err` (validate `MissingData`), the leader refuses to build H, ready
  once execution applies H-L.
- torus-economics `epoch_plan` unit tests: plan without a recorded set,
  first plan vs the recorded genesis set (unchanged; floor of the genesis
  set), unchanged plan, due / late rotations, statuses (jailed untouched),
  plan rows hashed.
- torus-genesis `initialize_records_the_genesis_set_as_the_installed_epoch_set`
  (RED before the genesis record: the first plan after genesis was `changed`).
- Running-hash tests listed in the section above.
