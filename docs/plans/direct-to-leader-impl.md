# Implementation Plan: Direct-to-Leader Architecture

## Design Decision
Incremental rollout: Step 1 (full blocks) eliminates compact block reconstruction failures.
Step 2 (leader forwarding) eliminates gossip amplification for order intake.

## Architecture Decisions
- **Leader discovery**: Any-node forwarding (clients unchanged, non-leaders forward internally)
- **Leader rotation**: Drop in-flight + mempool fallback via gossip (already built)
- **Phasing**: Incremental (Step 1 → benchmark → Step 2)

## Success Criteria

> **Baseline established S456** (val2 pre-fix, session-sig, shared 3-val testnet on B). The old
> ">1000 / >10,000 included/sec" targets below were stale — the chain already commits far more.
> Two honest reference numbers replace them:
> - **Sustainable-today (0 drops, healthy blocks):** ~71–80k **committed** orders/s — val2 paced
>   b=100 conc ramp, `send-queue-full` drops = 0 every cell, blk/s ≥ floor. *This is the floor to lift.*
> - **Peak-committed under flood:** ~140–150k committed orders/s — but with **~44% native-action
>   gossip drops** (`torus_native_gossip_dropped_full` / `_published_actions`) and blk/s degraded to
>   2.6–3.6 (from ~15 healthy). *The chain ALREADY commits this; the job is to make it clean.*
>
> **Confirmed bottleneck (S456):** batch-dominated native-action **gossip** send-queue overflow on
> the ingest node (bigger batch → bigger per-action bytes → the byte-bounded libp2p publish queue
> overflows; concurrency is NOT the driver). Step 2 (unicast-to-leader) bypasses that gossip fan-out
> and is the **primary lever** — not Step 1. Body-path zstd (`/torus/*/2.0`) is already on and does
> NOT cover the gossip path (removed s364, DoS), so it does not address these drops.
>
> **Measure with the val2 OFAT ramp (`testnet/bench-ofat-ramp-s447.sh`), node-side `committed`
> (`node_actions_s × batch`) — NOT the tool's submission-derived `orders_s`, which lies under load.**

- **Step 1 (full blocks):** chain does NOT stall under sustained flood; no compact-block
  reconstruction stalls; committed orders/s and blk/s **no worse** than the B baseline (full blocks
  add body bytes — must not regress block health; relies on the already-negotiated `/2.0` body zstd).
- **Step 2 (direct-to-leader):** native-action gossip drop rate (`dropped_full / published`) falls
  from ~44% toward ~0; sustainable-clean throughput rises from ~80k toward the ~140k the chain
  already commits under flood; blk/s holds ≥ floor at that load.
- **Overall win:** hold **≥140k committed orders/s at ~0 drops** with **healthy block times
  (~65–120ms / blk/s ≥ floor)**.
- All existing tests pass (83 tests across torus-network, torus-mempool, torus-consensus).

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
2. Re-run the **identical val2 OFAT baseline** (`bench-ofat-ramp-s447.sh`, b=100 + b=400 conc ramps,
   session-sig) so before/after is same-rig, same-harness. Read node-side `committed` + `blk/s`.
3. Step 1 acceptance: no reconstruction stalls under flood; committed o/s + blk/s **≥ B baseline**
   (no regression from the extra body bytes).
4. Step 2 acceptance: `torus_native_gossip_dropped_full` drop rate → ~0; sustainable-clean throughput
   climbs from ~80k toward ~140k; blk/s ≥ floor.
5. Overall: ≥140k committed orders/s at ~0 drops, block times ~65–120ms.

## Rollback
- CompactBlock deserialization fallback preserved in validate_block and on_committed_block
- If full blocks cause consensus bandwidth issues, revert to CompactBlock with larger retry window
- Gossip infrastructure remains intact as fallback
