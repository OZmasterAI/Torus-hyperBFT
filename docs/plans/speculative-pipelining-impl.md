# Implementation Plan: Speculative Block Production (Option C)

## Design Decision
Add speculative block production ON TOP of Option A (header-first gossip + DA guard +
early view advancement). The next proposer starts building its block during the current
view's voting phase, so when the QC arrives its proposal is ready instantly.

**Prerequisite:** Option A must be implemented first. This plan assumes Tasks 1-9 + 5b
from `hybrid-pipelining-impl.md` are done.

## Success Criteria
1. Next proposer starts `produce_block` speculatively before QC arrives
2. If QC confirms the speculative parent → proposal broadcasts instantly (0ms delay)
3. If QC is for a DIFFERENT parent (fork/timeout) → speculative block discarded, rebuild
4. `on_speculative_rollback` fires correctly on discard
5. No state corruption: committed state is never affected by discarded speculation
6. Effective block time drops from ~15-20ms to ~10-15ms under normal operation
7. All existing tests pass + new speculation-specific tests

## Architecture

```
View N                              View N+1
┌──────────────────────────┐       ┌─────────────────────────┐
│ Leader N: broadcast hdr  │       │ Leader N+1: broadcast   │
│ Validators: vote         │       │ (proposal was pre-built)│
│                          │       │                         │
│ Leader N+1: speculatively│       │                         │
│   produce_block(parent=N)│       │                         │
│                          │       │                         │
│ QC forms ─── ─── ─── ─── ── ──>│ Instant proposal!       │
└──────────────────────────┘       └─────────────────────────┘

If view N times out (no QC for N's block):
  - Leader N+1 discards speculative block
  - Calls on_speculative_rollback
  - Rebuilds with TC-based parent (normal path)
```

## Key Design Decisions

### D1: Speculation scope = block production only
The speculative worker ONLY calls `app.produce_block()`. It does NOT insert into the
block tree or broadcast anything. The block sits in memory until the QC arrives
confirming the parent. This means:
- Block tree is never polluted with speculative blocks
- No state rollback needed in the block tree (only in the app)
- Other validators never see the speculative block

### D2: Single speculative slot
Only ONE speculative block at a time. No chained speculation (speculating on a
speculation). This bounds complexity and memory usage.

### D3: App-level state isolation
`produce_block` for speculation runs on a CLONED state snapshot. If speculation is
confirmed, the snapshot becomes the real state. If discarded, the snapshot is dropped.
The `on_speculative_rollback` hook (already in App trait) handles any app-specific
cleanup.

## Tasks

### Task C1: Speculative block production worker

**Test first:**
```rust
#[test]
fn speculative_worker_produces_block_in_background() {
    // Setup: algorithm knows next proposer is us for view N+1
    // Act: current view N starts, we start speculative produce_block
    // Assert: speculative block ready before QC arrives
    // Assert: block uses current proposal's block as parent
}

#[test]
fn speculative_worker_cancelled_on_view_timeout() {
    // Setup: speculative production in progress for parent=block_N
    // Act: view N times out (TC formed instead of QC)
    // Assert: speculative block discarded
    // Assert: on_speculative_rollback called with block hash
}
```

**Implementation:**
- `crates/hotstuff_rs/src/algorithm.rs` — add speculative production:
```rust
struct SpeculativeSlot {
    parent_hash: CryptoHash,
    parent_height: BlockHeight,
    result: Option<ProduceBlockResponse>,
    worker: Option<JoinHandle<ProduceBlockResponse>>,
}
```
- After `enter_view` processes a ProposalHeader for view N:
  - Check: am I the proposer for view N+1? (via `is_proposer(view+1)`)
  - If yes: spawn speculative `produce_block` on a background thread
    with `parent_block = current_proposal.block_hash`
  - Store in `speculative_slot: Option<SpeculativeSlot>`
- The worker runs `app.produce_block(SpeculativeRequest{...})` on a cloned state

**Verify:** `cargo test -p hotstuff_rs speculative_worker`
**Depends on:** Option A complete

---

### Task C2: Use speculative result on QC arrival

**Test first:**
```rust
#[test]
fn enter_view_uses_speculative_block_when_parent_matches() {
    // Setup: speculative block ready for parent=block_N
    // Act: QC arrives for block_N, view advances to N+1, we are proposer
    // Assert: enter_view uses speculative ProduceBlockResponse directly
    // Assert: does NOT call app.produce_block again
    // Assert: proposal broadcast happens within <1ms of QC arrival
}

#[test]
fn enter_view_discards_speculation_when_parent_mismatches() {
    // Setup: speculative block ready for parent=block_N
    // Act: TC arrives (timeout, not QC) — different parent for next view
    // Assert: speculative block discarded
    // Assert: app.produce_block called normally with correct TC-based parent
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/implementation.rs` — modify `enter_view` (L270+):
  - Before calling `app.produce_block()`, check `speculative_slot`:
    ```rust
    if let Some(slot) = self.speculative_slot.take() {
        if slot.parent_hash == expected_parent {
            // Use speculative result directly — zero-latency proposal
            let response = slot.result.unwrap();
            // Build Block from response, broadcast header
            return Ok(());
        } else {
            // Parent mismatch — discard and rebuild
            app.on_speculative_rollback(slot.parent_hash, &evidence);
        }
    }
    // Normal path: call app.produce_block()
    ```
- Pass `speculative_slot` from Algorithm struct into HotStuff via `enter_view` params

**Verify:** `cargo test -p hotstuff_rs speculative_enter_view`
**Depends on:** Task C1

---

### Task C3: App-level state snapshot for speculation

**Test first:**
```rust
#[test]
fn speculative_produce_block_does_not_modify_committed_state() {
    // Setup: committed state has balance=100 for account A
    // Act: speculative produce_block executes a transfer A→B
    // Assert: committed state still shows balance=100 for A
    // Act: speculation discarded
    // Assert: state is unchanged (no trace of the speculative execution)
}

#[test]
fn speculative_state_promoted_on_confirmation() {
    // Setup: speculative produce_block executed, parent confirmed by QC
    // Act: block committed normally
    // Assert: state reflects both the confirmed block AND speculatively pre-computed changes
}
```

**Implementation:**
- `crates/torus-consensus/src/app.rs` — add state snapshot support:
  - `fn snapshot_state(&self) -> StateSnapshot` — cheap CoW clone of current state
  - `fn promote_snapshot(&mut self, snapshot: StateSnapshot)` — make snapshot the real state
  - `fn discard_snapshot(&mut self, snapshot: StateSnapshot)` — drop without applying
- The speculative worker receives the snapshot and produces a block against it
- On confirmation: snapshot is promoted (becomes committed state base for next block)
- On discard: snapshot is dropped, `on_speculative_rollback` called

**Verify:** `cargo test -p torus-consensus speculative_state`
**Depends on:** Task C1

---

### Task C4: Predict next proposer

**Test first:**
```rust
#[test]
fn correctly_predicts_next_view_proposer() {
    // Setup: 4 validators, round-robin rotation
    // Assert: at view N, next_proposer(N+1) returns the correct validator
    // Assert: works across epoch boundaries
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/roles.rs` — add:
  ```rust
  pub fn next_proposer(current_view: ViewNumber, validator_set: &ValidatorSetState) -> VerifyingKey
  ```
- This is just `is_proposer(view + 1, ...)` — trivial with round-robin
- Used by algorithm loop to decide whether to start speculative production

**Verify:** `cargo test -p hotstuff_rs roles::next_proposer`
**Depends on:** None

---

### Task C5: Cancellation and cleanup

**Test first:**
```rust
#[test]
fn speculative_worker_dropped_on_shutdown() {
    // Ensure no dangling threads on node shutdown
}

#[test]
fn rapid_view_changes_dont_accumulate_speculative_slots() {
    // Setup: views advance rapidly (every 10ms)
    // Assert: only ONE speculative slot active at any time
    // Assert: previous speculation cancelled before new one starts
}

#[test]
fn speculation_disabled_when_not_next_proposer() {
    // Setup: we are NOT the next proposer
    // Assert: no speculative production started
    // Assert: no wasted CPU
}
```

**Implementation:**
- `crates/hotstuff_rs/src/algorithm.rs`:
  - On every `enter_view`: if `speculative_slot` exists and doesn't match new parent, discard
  - Only start speculation when `next_proposer(view+1) == me`
  - Worker uses a cancellation token (AtomicBool) — check periodically during produce_block
  - On shutdown: set cancel flag, join worker thread

**Verify:** `cargo test -p hotstuff_rs speculative_cleanup`
**Depends on:** Task C1, C2

---

### Task C6: Integration test

**Test first:**
```bash
# Devnet 4 validators, high order load
# Measure block time WITH speculation vs WITHOUT
# Assert: average block time < 15ms with speculation
# Assert: zero state corruption after 1000+ blocks
# Assert: forced leader crash mid-speculation doesn't break chain
```

**Implementation:**
- Feature flag: `--speculative-production` CLI arg (default off)
- Enable in devnet docker-compose for testing
- Measure: time between consecutive `on_commit_block` events
- Chaos test: kill proposer mid-speculation, verify chain continues

**Verify:** Devnet integration test with speculation enabled
**Depends on:** Tasks C1-C5

## Verification (end-to-end)
```bash
cargo test -p hotstuff_rs
cargo test -p torus-consensus
# Devnet with speculation:
cd devnet && SPECULATION=1 ./start.sh --build
# 1. Measure block time: should be <15ms average
# 2. Kill random validator mid-block: chain continues
# 3. Run tx-flood for 5 minutes: zero state divergence across validators
# 4. Compare block times with/without speculation flag
```

## Rollback
- Feature-flagged: `--speculative-production` (off by default)
- Without the flag, system behaves exactly like Option A
- If bugs found, flip flag off — zero code revert needed

## Interaction with Option A

Option C adds 6 tasks ON TOP of Option A's 10 tasks:
- Total: 16 tasks
- Option A: 2-3 weeks
- Option C additions: 2-3 weeks more
- Combined: 4-6 weeks

The key dependency: Option A's Task 7 (early view advancement) must be done first,
because speculation only helps if the QC triggers immediate view advancement. If views
wait for deadlines, speculation gains nothing.
