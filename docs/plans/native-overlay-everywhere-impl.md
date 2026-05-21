# Implementation Plan: Native Overlay Everywhere (Chained Overlays)

## Design Decision
Chained overlays — zero disk writes until commit. NativeStateOverlay becomes generic
over its fallback, enabling overlay-on-overlay chains. All native execution paths use
overlays, stored in PendingExec, flushed only in flush_committed_bundles.

## Success Criteria
1. Zero state root mismatches after 10k+ blocks with tx-loop + native-order-flood
2. `ensure_native_committed_through` and `execute_native_post_commit` removed entirely
3. No direct native CF writes outside of flush_committed_bundles and crash recovery
4. All existing tests pass (`cargo test -p torus-consensus -p torus-bridge -p torus-state`)
5. Speculative pipelining unblocked (overlay chains work for speculative parents)

## Tasks

### Task 1: Make NativeStateOverlay generic over fallback

**Test first:**
```rust
// crates/torus-state/src/backend.rs — add test
#[test]
fn chained_overlay_reads_through_parent() {
    let db = test_state_db();
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key1", b"disk_val").unwrap();

    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key1", b"parent_val").unwrap();
    parent.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key2", b"parent_only").unwrap();

    let child = NativeStateOverlay::new(parent.clone());
    child.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key2", b"child_override").unwrap();

    // Child reads: key1 from parent pending, key2 from child pending
    assert_eq!(child.get_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key1").unwrap(), Some(b"parent_val".to_vec()));
    assert_eq!(child.get_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key2").unwrap(), Some(b"child_override".to_vec()));

    // iterate_cf merges all layers
    let entries = child.iterate_cf(CF_NATIVE_ORDER_BOOKS, None).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], (b"key1".to_vec(), b"parent_val".to_vec()));
    assert_eq!(entries[1], (b"key2".to_vec(), b"child_override".to_vec()));
}

#[test]
fn chained_overlay_tombstone_hides_parent_write() {
    let db = test_state_db();
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key1", b"val").unwrap();

    let child = NativeStateOverlay::new(parent.clone());
    child.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key1").unwrap();

    assert_eq!(child.get_cf_raw(CF_NATIVE_ORDER_BOOKS, b"key1").unwrap(), None);
    let entries = child.iterate_cf(CF_NATIVE_ORDER_BOOKS, None).unwrap();
    assert_eq!(entries.len(), 0);
}
```

**Implementation:**
- `crates/torus-state/src/backend.rs`:
  - Change `struct NativeStateOverlay` to `struct NativeStateOverlay<T: StateBackend = StateDb>`
  - Field `db: StateDb` → `db: T`
  - `impl NativeStateOverlay<T>` for `new(db: T)`, `flush()`, `seed_from_bundle()`, etc.
  - `impl<T: StateBackend> StateBackend for NativeStateOverlay<T>` — all methods unchanged
    (they already delegate to `self.db` via trait, not concrete type)
  - `flush(&self, target: &StateDb)` signature unchanged (always flushes to real DB)
  - Update `pub use` in `crates/torus-state/src/lib.rs`

**Verify:** `cargo test -p torus-state chained_overlay`
**Depends on:** None

---

### Task 2: Make compute_native_state_root generic

**Test first:**
```rust
// crates/torus-bridge/src/state_root.rs — add test
#[test]
fn native_state_root_from_overlay_matches_disk() {
    let db = test_state_db();
    // Write some order book data to disk
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"market_0", b"orderbook_data").unwrap();
    let root_from_disk = compute_native_state_root(&db).unwrap();

    // Same data via overlay
    let db2 = test_state_db();
    let overlay = NativeStateOverlay::new(db2);
    overlay.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"market_0", b"orderbook_data").unwrap();
    let root_from_overlay = compute_native_state_root(&overlay).unwrap();

    assert_eq!(root_from_disk, root_from_overlay);
}
```

**Implementation:**
- `crates/torus-bridge/src/state_root.rs`:
  - Change `compute_native_state_root(state_db: &StateDb)` to
    `compute_native_state_root(db: &impl StateBackend)`
  - Replace `db.inner()` + raw RocksDB iterator with `db.iterate_cf(cf_name, None)?`
  - The iterate_cf return type `Vec<(Vec<u8>, Vec<u8>)>` replaces the raw iterator
  - `compute_full_composite_root` signature unchanged (takes pre-computed native_root)
  - Update callers: proposer.rs L234, validator.rs L416 (pass `state_db` or overlay)

**Verify:** `cargo test -p torus-bridge native_state_root`
**Depends on:** Task 1

---

### Task 3: do_validate — always use overlay + PendingExec

**Test first:**
```rust
// crates/torus-consensus/src/app.rs — modify existing test or add
#[test]
fn validate_block_with_native_stores_overlay_in_pending() {
    // Setup: app with state_db, produce a block with native actions
    // Act: validate the block
    // Assert: pending_bundles contains entry with native_overlay = Some(...)
    // Assert: state_db CF_NATIVE_ORDER_BOOKS is UNCHANGED (no disk write)
}
```

**Implementation:**
- `crates/torus-consensus/src/app.rs` do_validate (~L962-1040):
  - Remove the `if use_overlay { ... } else { ... }` branch
  - Always create native overlay:
    ```rust
    let native_overlay = if has_native || torus_block.header.evm_fee_revenue > 0 {
        match self.execute_native_on_overlay(&torus_block, validated.native_sender_actions, validated.native_consumed_nonces, &validated.bundle) {
            Ok(ov) => Some(ov),
            Err(e) => { tracing::error!(%e, "native overlay execution failed"); None }
        }
    } else { None };
    self.pending_bundles.insert(block_crypto_hash, PendingExec {
        bundle: validated.bundle,
        native_overlay,
        parent_hash,
        height: torus_block.header.height,
    });
    ```
  - Remove direct `execute_native_post_commit` call (L1021)
  - Remove direct `commit_pending_bundle` call (L1011)
  - Remove `self.evm_committed_height = ...` in the linear path (L1018)
  - Remove `ensure_native_committed_through` call at L765

**Verify:** `cargo test -p torus-consensus validate`
**Depends on:** Task 1

---

### Task 4: produce_block — read native_root via overlay chain, defer commit

**Test first:**
```rust
#[test]
fn produce_block_does_not_write_native_to_disk() {
    // Setup: app with seeded state, parent block committed
    // Act: produce a block with native actions in mempool
    // Assert: CF_NATIVE_ORDER_BOOKS on disk unchanged
    // Assert: pending_bundles contains the produced block's overlay
    // Assert: returned block has correct native_root (from overlay chain)
}
```

**Implementation:**
- `crates/torus-consensus/src/app.rs` produce_block (~L1040-1209):
  - Remove `self.ensure_native_committed_through(parent_header.height)` (L1088)
  - Before computing native_root, build overlay chain:
    ```rust
    let committed_hash = request.block_tree().highest_committed_block().ok().flatten();
    self.flush_committed_bundles(committed_hash);
    ```
  - If parent hash is in pending_bundles and has native_overlay, chain:
    ```rust
    let native_root = if let Some(pending) = parent_hash.and_then(|ph| self.pending_bundles.get(&ph)) {
        if let Some(ref parent_overlay) = pending.native_overlay {
            compute_native_state_root(parent_overlay)?
        } else {
            compute_native_state_root(&self.state_db)?
        }
    } else {
        compute_native_state_root(&self.state_db)?
    };
    ```
  - After building block: store in PendingExec instead of committing to disk (L1157-1190)
    ```rust
    self.pending_bundles.insert(block_hash, PendingExec {
        bundle: exec_result.bundle,
        native_overlay: Some(overlay),
        parent_hash: Some(parent_crypto_hash),
        height: block.header.height,
    });
    ```
  - Pass parent overlay into build_block_with_native for native_root computation
    (or compute native_root before calling build_block_with_native and pass it in)

- `crates/torus-bridge/src/proposer.rs` build_block_with_native (L156-235):
  - Change `evm_executor.execute_block(state_db, &block_cfg, tx_envs, true)` to `false`
    at L213 (don't commit EVM to disk)
  - Accept `native_root: B256` as parameter instead of computing it internally (L234)
  - Remove `compute_native_state_root(state_db)` call at L234

**Verify:** `cargo test -p torus-consensus produce` and `cargo test -p torus-bridge proposer`
**Depends on:** Task 2, Task 3

---

### Task 5: catch-up path — overlay + immediate flush, skip nonce check

**Test first:**
```rust
#[test]
fn catchup_with_native_uses_overlay_and_flushes() {
    // Setup: app behind by 2 blocks, both have native actions, nonces already in DB
    // Act: do_validate triggers catch-up walk
    // Assert: catch-up succeeds (no replayed nonce error)
    // Assert: after catch-up, disk state reflects all ancestors' native execution
}
```

**Implementation:**
- `crates/torus-consensus/src/app.rs` catch-up path (~L860-913):
  - Replace `self.execute_native_post_commit(tb, ...)` (L880) with:
    ```rust
    let overlay = NativeStateOverlay::new(self.state_db.clone());
    // ... run native on overlay, flush immediately
    overlay.flush(&self.state_db)?;
    ```
  - Or simpler: keep execute_native_on_overlay + immediate flush
  - The key change: use overlay so order books are written atomically

- `crates/torus-bridge/src/validator.rs` validate_block_with_native_inner (~L323):
  - Add `skip_nonce_check: bool` parameter
  - When `skip_nonce_check`, skip the nonce replay check at L344-356
  - `validate_block_with_native_for_catchup` passes `skip_nonce_check=true`
  - `validate_block_with_native` passes `skip_nonce_check=false`

**Verify:** `cargo test -p torus-bridge validator` and `cargo test -p torus-consensus catchup`
**Depends on:** Task 1

---

### Task 6: Remove dead code

**Test first:** Existing tests must still pass after removal.

**Implementation:**
- `crates/torus-consensus/src/app.rs`:
  - Remove `ensure_native_committed_through` (L262-316)
  - Remove `execute_native_post_commit` (L632-673)
  - Keep `read_native_applied_height` and `write_native_applied_height` — still used by
    flush_committed_bundles (L626) and crash recovery
  - Remove any remaining callers of the removed functions

**Verify:** `cargo test -p torus-consensus` — all tests pass, `cargo build` — no dead code warnings
**Depends on:** Task 3, Task 4, Task 5

---

### Task 7: Crash recovery — overlay + immediate flush

**Test first:**
```rust
#[test]
fn crash_recovery_replays_native_via_overlay() {
    // Setup: state_db has committed block at height 5, native_applied_height = 3
    // Act: replay_native_post_commit_if_needed runs on startup
    // Assert: native state for blocks 4 and 5 applied to disk
    // Assert: native_applied_height = 5
}
```

**Implementation:**
- `crates/torus-consensus/src/app.rs` replay_native_post_commit_if_needed (L143-260):
  - Replace `execute_native_post_commit` calls with overlay + immediate flush pattern
  - For each block needing replay: create overlay, execute native, flush, advance height
  - Skip nonce check during replay (blocks are already committed)

**Verify:** `cargo test -p torus-consensus replay_native`
**Depends on:** Task 1, Task 6

---

### Task 8: Integration test — devnet verification

**Test first:**
```bash
# Rebuild devnet with all changes
cd devnet && docker compose down && docker compose up --build -d
# Wait for chain to stabilize
sleep 30
# Run native-order-flood + tx-loop simultaneously
python3 devnet/scripts/native-order-flood.py &
bash devnet/scripts/tx-loop.sh &
# Monitor for 5+ minutes — zero state root mismatches
docker logs -f devnet-validator-0-1 2>&1 | grep -i 'mismatch\|ERROR\|panic'
```

**Implementation:** No code changes — just verification.

**Verify:** 10k+ blocks with sustained native+EVM load, zero mismatches across all 4 validators
**Depends on:** Tasks 1-7

## Verification (end-to-end)
```bash
cargo test -p torus-state
cargo test -p torus-bridge
cargo test -p torus-consensus
# Devnet integration:
cd devnet && docker compose up --build -d
# Run both floods, watch for zero mismatches for 10 min
```

## Rollback
- Each task is independently compilable — if task N breaks, revert just that task
- The removed code (ensure_native_committed_through, execute_native_post_commit) is in git
- If overlay approach fails catastrophically, revert to pre-overlay commit + apply nonce-check band-aid (5 lines in validator.rs)
