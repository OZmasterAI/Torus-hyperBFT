# Implementation Plan: Direct-to-Leader Architecture

## Design Decision
Incremental rollout: Step 1 (full blocks) eliminates compact block reconstruction failures.
Step 2 (leader forwarding) eliminates gossip amplification for order intake.

## Architecture Decisions
- **Leader discovery**: Any-node forwarding (clients unchanged, non-leaders forward internally)
- **Leader rotation**: Drop in-flight + mempool fallback via gossip (already built)
- **Phasing**: Incremental (Step 1 → benchmark → Step 2)

## Success Criteria
- Step 1: Chain does NOT stall under 3000+/sec native action load
- Step 1: Benchmark measures >1000 included/sec sustained
- Step 2: Benchmark measures >10,000 included/sec sustained
- All existing tests pass (83 tests across torus-network, torus-mempool, torus-consensus)

---

## Step 1: Full Blocks Instead of Compact Blocks

### Task 1: produce_block serializes TorusBlock instead of CompactBlock

**Test first**: `cargo test -p torus-consensus` — existing tests must pass after change.

**Implementation** (`crates/torus-consensus/src/app.rs`):

- L803-808: Replace CompactBlock serialization with TorusBlock serialization
  ```
  // Before:
  let compact = CompactBlock::from_block(&block);
  self.pending_proposals.insert(height, block);
  self.pending_proposals.retain(|&h, _| h + 10 > height);
  let encoded = bincode::serialize(&compact).expect("serialize CompactBlock");

  // After:
  self.pending_proposals.insert(height, block.clone());
  self.pending_proposals.retain(|&h, _| h + 10 > height);
  let encoded = bincode::serialize(&block).expect("serialize TorusBlock");
  ```

**Verify**: `cargo test -p torus-consensus`

**Depends on**: None

---

### Task 2: validate_block deserializes TorusBlock directly

**Test first**: `cargo test -p torus-consensus` — must pass.

**Implementation** (`crates/torus-consensus/src/app.rs`):

- L844-908: Replace CompactBlock deserialization + mempool reconstruction with direct TorusBlock deserialization:
  ```
  // Before: deserialize CompactBlock, reconstruct from mempool with 500ms retry
  // After: deserialize TorusBlock directly — all actions included
  let block: TorusBlock = match bincode::deserialize(datum_bytes) {
      Ok(b) => b,
      Err(e) => {
          // Fallback: try CompactBlock for backward compat during rolling upgrade
          ...
      }
  };
  ```
  - Remove the 500ms retry loop (L882-893)
  - Remove mempool reconstruction (L867-908)
  - Keep sig attestation verification (L910-946)
  - Keep pending_proposals cache insertion (L949-957)

**Verify**: `cargo test -p torus-consensus`

**Depends on**: Task 1

---

### Task 3: on_committed_block deserializes TorusBlock directly

**Test first**: `cargo test -p torus-consensus`

**Implementation** (`crates/torus-consensus/src/app.rs`):

- L986-990: Primary deserialization is now TorusBlock (flip the try order):
  ```
  // Before: try CompactBlock first, fallback to TorusBlock
  // After: try TorusBlock first, fallback to CompactBlock (backward compat)
  let torus_block = if let Some(cached) = self.pending_proposals.remove(&height) {
      cached
  } else if let Ok(full) = bincode::deserialize::<TorusBlock>(datum.bytes()) {
      full
  } else if let Ok(compact) = bincode::deserialize::<CompactBlock>(datum.bytes()) {
      // fallback for blocks produced before upgrade
      ...reconstruct from mempool...
  }
  ```
- L1051-1054: Change remove_committed_native to use action hashes computed from block
  instead of compact.native_action_hashes

**Verify**: `cargo test -p torus-consensus`

**Depends on**: Task 1

---

### Task 4: Devnet benchmark verification

**Test**: Rebuild devnet, run `bench-throughput consensus --duration 60 --senders 100`

**Implementation**: No code changes — just build and test.

**Verify**: Chain stays healthy under load, >1000 included/sec sustained.

**Depends on**: Tasks 1-3

---

## Step 2: Leader Forwarding (Next Session)

### Task 5: Expose current leader from consensus to node

**Implementation**:
- Add `current_view` / `current_leader` query to TorusApp or a shared state
- hotstuff_rs `select_leader(view, validator_set)` is a pure function — node can compute it

**Depends on**: Step 1 complete

---

### Task 6: Non-leader RPC forwards native actions to leader

**Implementation** (`crates/torus-rpc/src/torus.rs`, `crates/torus-node/src/main.rs`):
- `submit_native_action` RPC: if this node is not the current leader, forward to leader
  via the existing `NetworkCommand::Send` direct-message protocol
- Leader processes the action normally (mempool insert + propose)
- Non-leader still inserts into local mempool as fallback (gossip propagation)

**Depends on**: Task 5

---

### Task 7: torus_getLeader RPC endpoint

**Implementation** (`crates/torus-rpc/src/torus.rs`):
- New RPC method returning current leader address + validator peer info
- Optional — for advanced clients that want direct connection

**Depends on**: Task 5

---

## Verification (end-to-end)
1. `cargo test -p torus-consensus -p torus-network -p torus-mempool` — all pass
2. Devnet benchmark: `bench-throughput consensus --duration 60 --senders 100`
3. Step 1 target: >1000 included/sec, chain stable
4. Step 2 target: >10,000 included/sec, chain stable

## Rollback
- CompactBlock deserialization fallback preserved in validate_block and on_committed_block
- If full blocks cause consensus bandwidth issues, revert to CompactBlock with larger retry window
- Gossip infrastructure remains intact as fallback
