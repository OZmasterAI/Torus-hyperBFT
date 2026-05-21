# Design: Native Overlay Everywhere (Chained Overlays, Zero Disk Writes Until Commit)

## Problem
12 sessions (S222-S233) of state root divergence bugs, all from the same root cause:
native execution writes directly to disk (CF_NATIVE_ORDER_BOOKS, CF_NATIVE_NONCES)
during produce_block, do_validate, or catch-up. When proposals fail, view-change, or
catch-up retries, the disk is poisoned with partial state.

The EVM side already solved this with deferred commits (PendingExec + flush_committed_bundles).
Native has the overlay machinery (NativeStateOverlay, StateBackend trait, execute_native_on_overlay)
but it's only used for the fork-sibling path (`use_overlay=true`). The other 3 paths write to disk.

## Current State — 4 native execution paths

| Path | Where | Writes to | Bug-free? |
|------|-------|-----------|-----------|
| Fork sibling (use_overlay) | do_validate L983-1008 | NativeStateOverlay -> PendingExec | YES |
| Linear (no fork) | do_validate L1009-1030 | Disk via execute_native_post_commit | NO |
| Parent catch-up | ensure_native_committed_through L262 | Disk directly | NO |
| EVM catch-up | do_validate L860-913 | Disk via execute_native_post_commit | NO |

## Design: Chained Overlays

Remove ALL direct disk writes from native execution. Every path uses NativeStateOverlay.
Disk is ONLY written in flush_committed_bundles when consensus commits a block. Overlays
chain through parent overlays so produce_block can read uncommitted parent state without
flushing to disk.

### Key insight
NativeStateOverlay already implements iterate_cf with merge semantics (L304-341 in
backend.rs): pending writes merge with fallback, tombstones handled, sorted output.
compute_native_state_root can run on an overlay instead of disk.

### Chaining mechanism
Make NativeStateOverlay generic over its fallback:

```rust
// Before:
pub struct NativeStateOverlay { db: StateDb, pending: Arc<RwLock<PendingState>> }

// After:
pub struct NativeStateOverlay<T: StateBackend = StateDb> { db: T, pending: Arc<RwLock<PendingState>> }
```

This gives: `NativeStateOverlay<NativeStateOverlay<StateDb>>` = child -> parent -> disk.
Reads check child pending -> parent pending -> disk. Zero cost if parent is already flushed.

## Changes

### 1. Make NativeStateOverlay generic (backend.rs)
- `struct NativeStateOverlay<T: StateBackend = StateDb>` — generic over fallback
- All `impl` blocks become `impl<T: StateBackend> NativeStateOverlay<T>`
- `new()` takes `T` instead of `StateDb`
- `flush()` writes pending to a `&StateDb` target (unchanged)
- ~30 lines changed, no logic changes

### 2. compute_native_state_root generic (state_root.rs or wherever it lives)
- Change signature: `fn compute_native_state_root(db: &impl StateBackend)` instead of `&StateDb`
- Same for compute_full_composite_root if it reads native CFs
- ~5 lines changed

### 3. do_validate — always use overlay + PendingExec (app.rs ~L962-1040)
- Remove the `if use_overlay { } else { }` branch entirely
- ALL paths: create NativeStateOverlay, run execute_native_on_overlay, store in PendingExec
- For the overlay case (fork sibling): overlay chains through parent's pending overlay
  if parent is in pending_bundles, otherwise falls back to disk
- ~30 lines removed, ~10 added

### 4. produce_block — read native_root from overlay chain (app.rs ~L1040-1209)
- Remove ensure_native_committed_through call
- Build overlay chain: if parent has a pending native_overlay in pending_bundles,
  create new overlay wrapping it. Otherwise wrap disk.
- compute_native_state_root(&overlay) instead of compute_native_state_root(&state_db)
- After building block: store EVM bundle + native overlay in PendingExec
  (currently commits EVM to disk immediately at L1165 — defer instead)
- ~25 lines changed

### 5. catch-up path — overlay + immediate flush (app.rs ~L860-913)
- Run native on overlay per ancestor block
- Flush overlay immediately after each block (already consensus-committed)
- Skip nonce check (blocks are already committed, nonce replay is not a concern)
- ~15 lines changed

### 6. Remove dead code (app.rs)
- Remove `ensure_native_committed_through` (L262-316) — ~55 lines
- Remove `execute_native_post_commit` (L632-673) — ~40 lines
- Remove `read_native_applied_height` / `write_native_applied_height` if no longer needed
  (flush_committed_bundles can track via evm_committed_height or a unified committed_height)

### 7. Crash recovery (app.rs L143-260)
- replay_native_post_commit_if_needed: use overlay + immediate flush
- Same pattern as catch-up: create overlay, execute, flush to disk
- ~20 lines changed

### 8. proposer.rs build_block_with_native (L156-235)
- Change `evm_executor.execute_block(state_db, ..., true)` to `false`
- Don't commit EVM to disk — return BundleState for deferred commit
- ~3 lines changed

## Summary

| What | Lines |
|------|-------|
| NativeStateOverlay generic | ~30 changed |
| compute_native_state_root generic | ~5 changed |
| do_validate: always overlay | -30, +10 |
| produce_block: overlay chain, defer | ~25 changed |
| catch-up: overlay + flush + skip nonce | ~15 changed |
| Remove ensure_native_committed_through | -55 |
| Remove execute_native_post_commit | -40 |
| Crash recovery | ~20 changed |
| proposer.rs | ~3 changed |
| **Net** | **~110 changed, ~95 removed** |

**Files:** backend.rs, app.rs, proposer.rs, state_root computation (3-4 files)

## Speculative Pipelining Readiness
With chained overlays, speculative block production (Task C3) becomes trivial:
- Speculative produce_block creates overlay wrapping uncommitted parent overlay
- If speculation confirmed: overlay stays in PendingExec chain
- If speculation discarded: drop the overlay (zero disk cleanup)
- No on_speculative_rollback state repair needed

## Verification
1. `cargo test -p torus-consensus` — existing tests pass
2. `cargo test -p torus-bridge` — native executor tests pass
3. Rebuild devnet, run tx-loop + native-order-flood simultaneously
4. Verify zero state root mismatches after 10k+ blocks with sustained native load
5. Kill and restart a validator mid-run — verify crash recovery works
