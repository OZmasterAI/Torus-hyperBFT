# Implementation Plan: Commit-Then-Execute (Native State Decoupling)

## Design Decision
Path 1 from brainstorm — Hyperliquid model. Move native execution to post-commit.
Consensus agrees on TX ordering, then all nodes execute native exactly once after commit.
Eliminates double-write (proposer + validator both writing to RocksDB).

## Why This Works
- Neither proposer nor validator executes native before computing state_root
- Both call `compute_native_state_root(state_db)` on unmodified DB = same lagged root
- `composite(evm_root_N, native_root_{N-1})` matches on both sides
- After commit, native executes once → DB updated for block N+1

## Success Criteria
1. `cargo check --workspace` passes
2. `cargo test --workspace` passes (all existing tests)
3. Native state written to DB exactly once per block (post-commit in do_validate)
4. State root consistent between proposer and validator (no mismatch)
5. Nonce replay protection still works
6. Speculative rollback still works (pre-existing limitation noted)

## Tasks

### Task 1: Extend ValidatedBlock with native context
**File:** `crates/torus-bridge/src/validator.rs`

Add fields to ValidatedBlock so post-commit execution doesn't re-verify signatures:
```rust
pub struct ValidatedBlock {
    pub receipts: Vec<Receipt>,
    pub bundle: BundleState,
    pub state_root: B256,
    pub native_sender_actions: Vec<(Address, torus_types::NativeAction)>,
    pub native_consumed_nonces: Vec<(Address, u64)>,
}
```

**Test:** `cargo check -p torus-bridge`
**Depends on:** none

### Task 2: Strip native execution from proposer.rs
**File:** `crates/torus-bridge/src/proposer.rs`

In `build_block_with_native()`:
- KEEP lines 162-182: signature recovery + read-only nonce replay check
- REMOVE lines 184-185: sort_native_actions call (not needed without execution)
- REMOVE lines 188-243: NativeExecContext creation + all execution phases (1-7)
- REMOVE lines 246-258: save_order_books + nonce persistence
- KEEP line 261-262: state root computation (reads unmodified DB = lagged native root)
- REMOVE unused imports: NativeExecContext, NativeExecutor, sort_native_actions

**Test:** `cargo check -p torus-bridge`
**Depends on:** none

### Task 3: Strip native execution from validator.rs
**File:** `crates/torus-bridge/src/validator.rs`

In `validate_block_with_native()`:
- KEEP lines 196-220: signature recovery + nonce replay check
- REMOVE lines 222-223: sort_native_actions call
- REMOVE lines 226-314: NativeExecContext creation + all execution phases
- REMOVE lines 316-329: save_order_books + nonce persistence
- KEEP line 332-340: state root verification (reads unmodified DB = same lagged root)
- Populate new ValidatedBlock fields: native_sender_actions = sender_actions, native_consumed_nonces = consumed_nonces
- Update validate_block_inner to set empty vecs for native fields (EVM-only path)
- REMOVE unused imports: NativeExecContext, NativeExecutor, sort_native_actions

**Test:** `cargo check -p torus-bridge`
**Depends on:** Task 1

### Task 4: Add post-commit native execution in app.rs
**File:** `crates/torus-consensus/src/app.rs`

Add method to TorusApp:
```rust
fn execute_native_post_commit(
    &self,
    block: &TorusBlock,
    sender_actions: Vec<(Address, NativeAction)>,
    consumed_nonces: Vec<(Address, u64)>,
) -> Result<(), BridgeError> {
    let (pre_evm, post_evm) = sort_native_actions(&sender_actions);
    let mut ctx = NativeExecContext::new(
        self.state_db.clone(),
        block.header.height,
        block.header.timestamp,
        block.header.epoch,
        self.epoch_length,
        self.max_validators,
        block.header.proposer,
        // treasury + dev_pool from self
    );
    ctx.metrics = self.metrics.clone();
    NativeExecutor::execute_batch(&mut ctx, &pre_evm);
    NativeExecutor::execute_batch(&mut ctx, &post_evm);
    NativeExecutor::drain_core_writer(&mut ctx)?;
    NativeExecutor::process_governance(&mut ctx);
    NativeExecutor::distribute_fees(&mut ctx, block.header.evm_gas_used);
    NativeExecutor::process_epoch_boundary(&mut ctx);
    ctx.save_order_books();
    // Persist consumed nonces
    for (sender, nonce) in &consumed_nonces {
        let mut nonce_key = [0u8; 28];
        nonce_key[..20].copy_from_slice(sender.as_slice());
        nonce_key[20..28].copy_from_slice(&nonce.to_be_bytes());
        let _ = self.state_db.put_cf_raw(
            torus_state::cf::CF_NATIVE_NONCES,
            &nonce_key,
            &block.header.height.to_be_bytes(),
        );
    }
    Ok(())
}
```

In `do_validate()`, after `BlockCommitter::commit_block()` succeeds:
```rust
// Post-commit: execute native actions exactly once
if has_native {
    if let Err(e) = self.execute_native_post_commit(
        &torus_block,
        validated.native_sender_actions,
        validated.native_consumed_nonces,
    ) {
        tracing::error!(%e, "native post-commit execution failed");
    }
}
```

Add imports: `sort_native_actions`, `NativeExecContext`, `NativeExecutor`, `NativeAction`

**Test:** `cargo check -p torus-consensus`
**Depends on:** Tasks 1, 2, 3

### Task 5: Compile and test full workspace
- `cargo check --workspace`
- `cargo test --workspace`
- Verify no remaining references to NativeExecContext/NativeExecutor in proposer.rs or validator.rs
- Verify compute_native_state_root is still called in both proposer.rs and validator.rs (for state root)

**Depends on:** Task 4

## Known Limitations / Follow-ups
1. **Native writes not atomic:** Native state writes go through individual `put_cf_raw()` calls, not WriteBatch. A crash between EVM commit and native execution completion could leave partial native state. Future: batch native writes too.
2. **Speculative rollback:** Pre-existing issue — EVM state committed during do_validate is not reverted on speculative rollback. Same applies to native now. The `on_speculative_rollback` comment at app.rs:556 acknowledges this.
3. **Validator set updates lagged by 1 block:** Epoch boundary processing (staking changes) now happens post-commit, so validator set updates reflect block N-1's native state. Negligible impact (epoch_length=100).

## Rollback
If something breaks: revert all changes — the old double-write model works (just slower and with potential mismatches). `git stash` or `git checkout -- crates/` to restore.
