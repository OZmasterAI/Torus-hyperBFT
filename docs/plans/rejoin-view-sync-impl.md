# Implementation Plan: Rejoin view sync (round-skip)

## Design Decision

Option A from `rejoin-view-sync.md`: a checked future-view header moves the
replica to that view through the pacemaker. It then votes through the unchanged
current-view path.

The kill switch is `TORUS_ROUND_SKIP`. It is on unless set to `0`, the same
parsing as `TORUS_BODY_SERVE_THREAD`.

## Success Criteria

- A replica at view v-4 that receives leader(v)'s safe header sends exactly
  one PhaseVote, at v, for that block. Its pacemaker and HotStuff are then at v.
- After that, none of these produce another vote:
  - the buffered copy of the header;
  - a retransmission of the header;
  - a header for v-1;
  - a conflicting header at v.
- There is no skip in any of these cases:
  - the header carries a TC or NEC;
  - the replica already voted at ≥ v;
  - the header is for the current view;
  - the kill switch is off;
  - the skip would leave the current epoch.
- Existing tests stay green: `hotstuff_rs` lib, integration tests (with
  `--no-fail-fast`, since `justify_block_livelock_test` is a known flake), and
  the workspace.

## Tasks

### Task 1: HotStuff records a skip request

- **Test first** (`hotstuff/implementation.rs`, module `sync_recovery_tests`):
  - `future_header_requests_view_skip_without_voting`
  - `view_skip_refused_for_tc_nec_voted_current_or_disabled`
  - `round_skip_flag_defaults_on_and_only_zero_disables`
- **Implementation:**
  - Add `round_skip: bool` (from `round_skip_from_env()`) and
    `view_skip: Option<(ProposalHeader, VerifyingKey)>` to `HotStuff`.
  - Add `set_round_skip` (tests) and `take_view_skip()`.
  - In `on_receive_proposal_header`, after `update_locks_only`, record the
    request when all of these hold:
    - `round_skip`;
    - `header.view > view_info.view`;
    - `tc` and `nec` are `None`;
    - `is_phase_voter`;
    - `highest_view_voted < header.view`.
    Keep the highest view.
- **Verify:** `cargo test -p hotstuff_rs --lib sync_recovery_tests`

### Task 2: `Pacemaker::skip_to_view`

- **Test first** (`pacemaker/implementation.rs`):
  - `skip_to_view_enters_future_view_with_bounded_deadline`
  - `skip_to_view_refuses_non_increasing_and_cross_epoch`
- **Implementation:** `pub(crate) fn skip_to_view(w, &block_tree) -> Result<bool>`.
  - It returns `false` (no change) if `w <= cur` or `epoch(cur) != epoch(w)`.
  - Otherwise it calls `update_view(w, vss, highest_pc.view, committed_qc_view)`,
    the same arguments as the AdvanceView path.
- **Verify:** `cargo test -p hotstuff_rs --lib skip_to_view`
- **Depends on:** none.

### Task 3: Algorithm wiring

- **Test first:** `algorithm/rejoin_view_sync_tests.rs`, using an Algorithm
  fixture with the pacemaker and HotStuff at the same view:
  - `rejoining_replica_skips_to_leader_header_and_votes_once`
  - `round_skip_off_keeps_view_and_withholds_vote`
- **Implementation:** in `poll_progress_and_retry`, after `on_receive_msg`,
  run `take_view_skip`, then `skip_to_view`, then `enter_view(query())`, then
  re-dispatch the header to `on_receive_msg`.
- **Verify:** `cargo test -p hotstuff_rs --lib rejoin_view_sync`
- **Depends on:** 1 and 2.

### Task 4: Stateright model

- **Test first/only:** `tests/stateright_rejoin_view_sync.rs`.
- **Model:** 3 replicas. Each has a view, a persisted `highest_voted`, a
  persisted `highest_entered`, and a vote log.
- **Actions:**
  - timeout (view+1);
  - crash-restart (view = `max(highest_entered, highest_voted) + 1`);
  - leader header at w, following the skip rule;
  - vote.
- **Properties:**
  - no replica votes twice in a view;
  - a replica's vote views strictly increase across restarts;
  - after a restart, once a current header is delivered, all replicas vote in
    a common view.
- **Verify:** `cargo test -p hotstuff_rs --test stateright_rejoin_view_sync`

### Task 5: Suites and crash A/B

- **Suites:** `cargo test --release -p hotstuff_rs --no-fail-fast`, then the
  workspace.
- **Crash A/B:** campaign `s74-rv`, same binary, `TORUS_ROUND_SKIP=0` vs
  default, 3 cells each at `--crash-at 60`. Score:
  - the freeze;
  - the time from `state database opened` to val1's first vote;
  - agreement and the crash gate.

## Rollback

Set `TORUS_ROUND_SKIP=0` at runtime, or revert the branch. No schema or state
changes are involved.
