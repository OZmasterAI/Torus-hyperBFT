# Implementation Plan: Block Validation Worker Thread

## Design Decision
Move block validation (EVM execution + native sig recovery) off the consensus
algorithm's main loop into a dedicated worker thread. The consensus loop sends
validation requests and polls for results, never blocking on heavy computation.

This follows the same pattern as the BlockSyncWorker (`block_sync/worker.rs`):
command/result channels with non-blocking polling.

## Why This Matters
Currently `do_validate` in `app.rs` runs EVM execution inline. For blocks with
many transactions, this can take 100ms+ and stall the consensus round. Moving
validation to a worker means the algorithm loop stays responsive for voting,
view changes, and timeouts — even during heavy blocks.

## Why This Is Complex
Unlike block sync (pure I/O), validation touches shared state:
- Reads from `state_db` (RocksDB) — safe, reads are concurrent
- Writes to `state_db` via `commit_block` + `execute_native_post_commit` — must
  be serialized (only one block committed at a time, in height order)
- Updates `self.last_header`, `self.cached_vs_updates`, etc. — main thread state
- Returns `ValidateBlockResponse` to hotstuff_rs — must arrive before vote deadline

The `App` trait in hotstuff_rs calls `validate_block()` synchronously and expects
an immediate response. This is the core constraint.

## Architecture Options

### Option A: Async validation with speculative accept (Hyperliquid-style)
- Worker validates in background
- Consensus loop returns `Valid` optimistically
- If validation fails later, trigger rollback
- **Pro:** Zero blocking. **Con:** Requires rollback machinery, complex.

### Option B: Worker thread with bounded wait
- Consensus loop sends block to worker, then polls with a deadline
- If result arrives before vote timeout: use it
- If timeout: return Invalid (miss the vote, catch up via sync)
- **Pro:** Simpler, no rollback needed. **Con:** Could miss votes on slow blocks.

### Option C: Pre-validation pipeline
- When a proposal arrives, immediately dispatch to worker
- By the time consensus asks for validation, result is likely ready
- Falls back to inline if result not ready
- **Pro:** Hides latency without changing semantics. **Con:** Needs proposal interception.

**Recommended: Option B** — simplest, no semantic changes to consensus, follows
the existing BlockSyncWorker pattern. Option C is a follow-up optimization.

## Success Criteria
1. `cargo check --workspace` passes
2. `cargo test --workspace` passes (all existing tests)
3. Consensus test still passes at ~15s
4. `do_validate` never blocks the algorithm thread for more than 10ms (channel ops only)
5. Heavy blocks validated correctly (state matches inline validation)
6. Graceful shutdown of worker thread

## Tasks

### Task 1: Define ValidationCommand and ValidationResult types
**File:** `crates/torus-consensus/src/validation_worker.rs` (new)

```rust
pub(crate) enum ValidationCommand {
    Validate {
        block: hotstuff_rs::types::block::Block,
        // Serialized TorusBlock + all context needed for validation
    },
}

pub(crate) enum ValidationResult {
    Valid {
        block_height: u64,
        app_state_updates: Option<AppStateUpdates>,
        validator_set_updates: Option<ValidatorSetUpdates>,
    },
    Invalid {
        block_height: u64,
    },
    Error {
        block_height: u64,
        message: String,
    },
}
```

**Test:** `cargo check -p torus-consensus`
**Depends on:** none

### Task 2: Implement ValidationWorker
**File:** `crates/torus-consensus/src/validation_worker.rs`

Worker thread that:
1. Receives ValidationCommand via `mpsc::Receiver`
2. Runs the full validation pipeline (deserialize, validate, commit, native exec)
3. Sends ValidationResult via `mpsc::Sender`

Key design: the worker owns its own `TorusApp`-like state (state_db clone,
validator, evm_executor) so it can operate independently. The state_db is
shared (Arc) — RocksDB supports concurrent reads, and writes are serialized
by the single worker thread.

Follow the BlockSyncWorker pattern:
- `new()` + `spawn()` returning JoinHandle
- Shutdown signal channel
- Response timeout handling

**Test:** `cargo check -p torus-consensus`
**Depends on:** Task 1

### Task 3: Integrate worker into TorusApp
**File:** `crates/torus-consensus/src/app.rs`

Add to `TorusApp`:
```rust
validation_cmd_tx: Sender<ValidationCommand>,
validation_result_rx: Receiver<ValidationResult>,
pending_validation: Option<u64>, // height being validated
```

Modify `do_validate`:
1. If no pending validation: send block to worker, set pending_validation
2. Poll `validation_result_rx.try_recv()`
3. If result ready and matches current block: return it
4. If not ready: return Invalid (conservative — miss this vote, catch up later)

Modify `TorusApp::new()` to spawn the worker thread.

**Test:** `cargo check -p torus-consensus`
**Depends on:** Task 2

### Task 4: Handle state synchronization
**File:** `crates/torus-consensus/src/app.rs`, `validation_worker.rs`

After worker commits a block:
- Worker sends back the new `last_header` and any `cached_vs_updates`
- Main thread updates its local state from the result
- `pending_slashes` flush needs coordination (flush before sending to worker,
  or include pending slashes in the command)

**Test:** `cargo check -p torus-consensus`
**Depends on:** Task 3

### Task 5: Spawn and shutdown in main.rs
**File:** `crates/torus-node/src/main.rs`

- Worker thread spawned as part of TorusApp construction
- Graceful shutdown: send shutdown signal, join thread
- Same pattern as BlockSyncWorker in `replica.rs`

**Test:** `cargo check -p torus-node`
**Depends on:** Task 3

### Task 6: Consensus test with validation worker
**File:** `crates/torus-consensus/tests/consensus_test.rs`

Verify 4-node consensus still works with validation on worker thread:
- Blocks produced and committed
- State consistent across nodes
- Test completes in ~15s (no regression)

**Test:** `cargo test -p torus-consensus -- consensus_test`
**Depends on:** Tasks 3, 4, 5

### Task 7: Full workspace compile and test
- `cargo check --workspace`
- `cargo test --workspace`
- Benchmark: compare validation latency inline vs worker

**Depends on:** Task 6

## Key Risks
1. **App trait is synchronous:** hotstuff_rs calls `validate_block()` and expects
   an immediate answer. The worker must complete before the vote deadline, or we
   accept the vote miss. May need to adjust `max_view_time` if validation is slow.
2. **State races:** Two paths write to state_db — the main loop (produce_block)
   and the worker (validate+commit). Must ensure they never overlap. Since hotstuff_rs
   calls produce OR validate (never both simultaneously), this should be safe.
3. **Complexity cost:** The BlockSyncWorker is I/O-only. This worker does state mutations.
   Bugs here could cause state divergence between validators.

## Known Limitations / Follow-ups
1. **Option C pre-pipeline:** Could further reduce latency by starting validation
   when the proposal arrives (before consensus asks). Requires hooking into message
   reception, not just the App trait.
2. **Parallel EVM execution:** Future optimization — execute transactions in parallel
   within the worker using REVM's parallel execution mode.
3. **WriteBatch atomicity:** Currently native post-commit is not batched. Could combine
   with the crash-safety plan (WriteBatch + applied flag) for full atomicity.

## Rollback
Revert to inline validation — remove worker, restore original `do_validate`.
No state format changes, so rollback is clean.
