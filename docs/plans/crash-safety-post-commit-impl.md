# Implementation Plan: Crash-Safety for Native Post-Commit Execution

## Design Decision
Add a height-keyed "post-commit applied" flag to RocksDB. On startup, detect
if the last committed block's native post-commit was interrupted, and replay it.
This closes the gap between `BlockCommitter::commit_block()` and
`execute_native_post_commit()` in `app.rs:431-452`.

## Why This Works
- `execute_native_post_commit` is deterministic: same block data always produces
  same state writes (fees, epoch boundary, governance, order books, nonces)
- The block body (including native actions) is already persisted by `commit_block`
  before native execution runs, so we can re-derive inputs on restart
- A single key in `CF_CONSENSUS_META` tracks the last height where native
  post-commit completed — compare with last committed block height on startup

## What's At Risk Without This
A crash between lines 436 and 452 of `app.rs` silently drops:
- Staking reward distribution (`distribute_fees`)
- Epoch boundary processing (`process_epoch_boundary`)
- Governance proposal execution (`process_governance`)
- Order book persistence (`save_order_books`)
- Native nonce tracking (`CF_NATIVE_NONCES` writes)

## Success Criteria
1. `cargo check --workspace` passes
2. `cargo test --workspace` passes (all existing tests)
3. New unit test: simulates crash (write block, skip native exec, restart, verify replay)
4. `execute_native_post_commit` is idempotent — calling it twice for the same height produces identical state
5. Startup detects and replays missed post-commit within <100ms

## Tasks

### Task 1: Add NATIVE_APPLIED_HEIGHT key to CF_CONSENSUS_META
**Files:** `crates/torus-state/src/cf.rs`, `crates/torus-consensus/src/app.rs`

Add a well-known key constant for tracking native post-commit completion:
```rust
// In cf.rs (or as a constant in app.rs)
pub const META_NATIVE_APPLIED_HEIGHT: &[u8] = b"native_applied_height";
```

After `execute_native_post_commit` returns Ok, persist the height:
```rust
// In app.rs, after line 452 (successful native execution)
self.state_db.put_cf_raw(
    CF_CONSENSUS_META,
    META_NATIVE_APPLIED_HEIGHT,
    &torus_block.header.height.to_be_bytes(),
);
```

**Test:** `cargo check -p torus-consensus`
**Depends on:** none

### Task 2: Add startup replay check to TorusApp::new()
**Files:** `crates/torus-consensus/src/app.rs`

In `TorusApp::new()`, after loading `last_header`, check for the gap:
1. Read last committed block height from `CF_BLOCK_HEADERS` (scan for highest key)
2. Read `META_NATIVE_APPLIED_HEIGHT` from `CF_CONSENSUS_META`
3. If committed > applied (or applied key missing and committed > 0):
   a. Load the block body from `CF_BLOCK_BODIES` for that height
   b. Deserialize to `TorusBlock`
   c. Re-extract native actions (signature recovery + nonce extraction)
   d. Call `execute_native_post_commit`
   e. Write the applied height flag

Edge cases:
- Height 0 (genesis): skip — no native actions
- Block has no native actions: write flag anyway (marks height as processed)
- Applied == committed: no-op (normal case)

**Test:** `cargo check -p torus-consensus`
**Depends on:** Task 1

### Task 3: Make execute_native_post_commit idempotent
**Files:** `crates/torus-consensus/src/app.rs`

Verify idempotency of each sub-operation:
- `NativeExecutor::execute_batch` — writes final state (not incremental), idempotent
- `drain_core_writer` — drains a queue, needs guard if already drained
- `process_governance` — applies proposals, needs height guard
- `distribute_fees` — writes absolute amounts, idempotent
- `process_epoch_boundary` — validator set recomputation, idempotent if input unchanged
- `save_order_books` — writes full snapshot, idempotent
- Nonce writes — puts are idempotent (same key/value)

If any operation is NOT idempotent (e.g., `drain_core_writer` double-processes), add a
guard: skip if `META_NATIVE_APPLIED_HEIGHT >= current_height`.

**Test:** Write test that calls `execute_native_post_commit` twice for same block, asserts identical DB state
**Depends on:** Task 1

### Task 4: Add crash-recovery integration test
**Files:** `crates/torus-consensus/tests/` (new test or extend consensus_test.rs)

Test scenario:
1. Set up TorusApp with a state_db
2. Produce and commit a block with native actions (staking, governance)
3. Do NOT call `execute_native_post_commit` (simulate crash)
4. Construct a new TorusApp from the same state_db (simulates restart)
5. Assert: replay detected and executed (check native state in DB matches expected)
6. Assert: `META_NATIVE_APPLIED_HEIGHT` == block height

**Test:** `cargo test -p torus-consensus -- crash_recovery`
**Depends on:** Tasks 1, 2, 3

### Task 5: Compile and test full workspace
- `cargo check --workspace`
- `cargo test --workspace`
- Verify META_NATIVE_APPLIED_HEIGHT is written on every successful post-commit path
- Verify startup replay triggers correctly

**Depends on:** Task 4

## Known Limitations / Follow-ups
1. **Single-block replay only:** ~~This handles the common case (crash during the last block).
   Multi-block gaps (theoretically impossible since commit_block is synchronous) are not handled.~~

   > **UPDATE (S442):** No longer single-block. Consensus finality is decoupled from local
   > execution readiness (a committed block whose body isn't yet reconstructable still advances
   > height), so multi-block execution gaps DO occur in practice. `replay_committed`
   > (`app.rs:1511`) now replays the ENTIRE `[applied+1, committed]` range in ascending order
   > via `replay_gap` (`app.rs:360`), failing loud on any hole rather than skipping it.
2. **Native writes still not batched:** ~~Individual `put_cf_raw` calls within `execute_native_post_commit`
   could leave partial state if crash happens mid-execution. Future: wrap in WriteBatch.~~

   > **UPDATE (S442) — OUTDATED, superseded by the current tree.** This described a real
   > past state: native post-commit once issued individual `put_cf_raw()` calls with no
   > enclosing batch. That is no longer how it works. Native state now flushes through
   > `NativeStateOverlay::flush_with_native_trie` (`crates/torus-state/src/backend.rs:454`),
   > which builds a **single atomic `WriteBatch`** covering the native CF writes/deletes AND
   > the incremental native-trie nodes (`flush_with_native_trie_inner`, `backend.rs:482-532`).
   > The renamed function pointing at execution is now `execute_committed_block`
   > (`crates/torus-consensus/src/app.rs:424`), not `execute_native_post_commit`.
   >
   > The crash-safety fix in this plan is folded into that same batch: when the native path
   > runs, `execute_committed_block` calls `flush_with_native_trie_and_marker`
   > (`backend.rs:471`, `app.rs:750`), appending `META_NATIVE_APPLIED_HEIGHT` (big-endian `u64`)
   > to the SAME batch as the native writes. So native state and "this height is applied" now
   > commit together — a hard crash can never leave native state flushed but the height
   > un-marked (which would double-apply fees/epoch rewards on restart), nor the marker written
   > without the state (which would silently drop it). A standalone marker write remains only for
   > blocks that skip the native path entirely — empty / pure-EVM blocks whose re-execution is
   > idempotent (`app.rs:807-808`). Startup replay is no longer single-block: `replay_committed`
   > (`app.rs:1511`) loops the whole `[applied+1, committed]` gap in order via `replay_gap`
   > (`app.rs:360`) and **fails loud** (`ReplayGapOutcome::Hole` → latch `exec_failed`) on any
   > height it cannot reconstruct, instead of silently marking it applied.
3. **EVM state not covered:** EVM commit via `commit_block` uses RocksDB WriteBatch
   (already atomic). This plan only addresses the native post-commit gap.

## Rollback
Revert the changes — the system works without crash-safety (same as today).
The META_NATIVE_APPLIED_HEIGHT key is ignored if the code is reverted.
