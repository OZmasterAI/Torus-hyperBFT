# Design: Native Overlay Everywhere (Deferred Native Commit)

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
| Fork sibling (use_overlay) | do_validate L983-1008 | NativeStateOverlay → PendingExec | YES |
| Linear (no fork) | do_validate L1009-1030 | Disk via execute_native_post_commit | NO |
| Parent catch-up | ensure_native_committed_through L262 | Disk directly | NO |
| EVM catch-up | do_validate L860-913 | Disk via execute_native_post_commit | NO |

## Options

### Option A: Unify all paths to overlay (Recommended)

Remove the linear/fork distinction. ALL do_validate calls store results in PendingExec.
Flush to disk ONLY via flush_committed_bundles when consensus commits.

**Changes:**

1. **do_validate** (app.rs ~L962-1040): Remove the `if use_overlay { } else { }` branch.
   Always create native overlay, always store in PendingExec. ~30 lines removed, ~5 added.

2. **produce_block** (app.rs ~L1040-1209): Before computing native_root:
   - Call flush_committed_bundles (need committed hash from block tree)
   - Remove ensure_native_committed_through call
   - After building the block: store EVM bundle + native overlay in PendingExec
     (currently commits EVM to disk immediately at L1165)
   ~20 lines changed.

3. **catch-up path** (app.rs ~L860-913): Run native on overlay per ancestor,
   flush immediately since these are already-committed blocks. Skip nonce check
   (already consensus-committed). ~15 lines changed.

4. **Remove ensure_native_committed_through** (app.rs L262-316): No longer needed.
   All parent state arrives via flush_committed_bundles. ~55 lines removed.

5. **Remove execute_native_post_commit** (app.rs L632-673): Replace all callers
   with execute_native_on_overlay. Only flush path is flush_committed_bundles.
   ~40 lines removed.

6. **flush_committed_bundles** (app.rs L578-629): Already handles native overlay
   flush (L614-618). No change needed — it already works for the fork path.

7. **Crash recovery / replay_native_post_commit_if_needed** (app.rs L143-260):
   On startup, if native_applied_height < committed height, replay using overlay
   + immediate flush. Similar to catch-up. ~20 lines changed.

8. **proposer.rs build_block_with_native** (L156-235): Currently commits EVM with
   `execute_block(state_db, ..., true)`. Change to `false` (don't commit), return
   BundleState for deferred commit. ~3 lines changed.

**Files:** app.rs (~150 lines changed), proposer.rs (~3 lines), validator.rs (0 — already returns overlay data)

**Effort:** Medium (1-2 sessions). Machinery exists, it's wiring.
**Risk:** Low — we're REMOVING code paths and unifying to one that already works.

### Option B: Fix nonce check + keep two paths

Skip nonce check in validate_block_with_native_for_catchup. Keep the linear/fork
distinction.

**Changes:** validator.rs ~5 lines (add skip_nonce_check parameter).

**Effort:** Small (30 min)
**Risk:** High — the linear path still writes to disk, future bugs guaranteed.

### Option C: Overlay everywhere + chain overlays (no flush before produce)

Like Option A, but instead of flushing parent overlay to disk before produce_block,
read THROUGH chained overlays (parent overlay on top of disk). Avoids any disk writes
until final commit.

**Effort:** Large — NativeStateOverlay doesn't support chaining, compute_native_state_root
would need to read from overlay instead of disk.
**Risk:** Medium — more complex overlay management, harder to debug.

## Recommendation

**Option A.** It's the minimal change that eliminates the entire class of bugs. We're
removing the special "linear" and "ensure_native_committed_through" code paths and
unifying to the overlay path that already works for fork siblings. The machinery
(NativeStateOverlay, StateBackend, PendingExec with native_overlay) is all built.

Option B is a band-aid. Option C is over-engineering for now (but becomes useful
for speculative pipelining later).

## Task Order

1. Remove linear path in do_validate — always use overlay + PendingExec
2. Update produce_block — flush parent before native_root, defer own block to PendingExec
3. Update catch-up path — overlay + immediate flush, skip nonce check
4. Remove ensure_native_committed_through and execute_native_post_commit
5. Update crash recovery (replay_native_post_commit_if_needed)
6. Update proposer.rs — don't commit EVM in build_block_with_native
7. Test: rebuild devnet, run tx-loop + native-order-flood, verify zero mismatches

## Open Questions

1. Does flush_committed_bundles have access to the right committed_hash in produce_block?
   → Yes, ProduceBlockRequest has block_tree() which gives highest_committed_block().
2. Does build_block_with_native need the EVM BundleState committed to compute state_root?
   → No, compute_full_composite_root takes BundleState as input without needing it on disk.
3. Memory overhead of holding overlays for pending blocks?
   → Minimal. Each overlay is a BTreeMap of CF writes. At most 2-3 pending blocks.
