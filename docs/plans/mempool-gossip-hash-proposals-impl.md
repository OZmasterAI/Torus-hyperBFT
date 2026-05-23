# Implementation Plan: Mempool Gossip + Hash-Based Consensus Proposals

## Design Decision
Gossip native actions to all validators via gossipsub on submission. Consensus
proposals carry action hashes (B256) instead of full SignedNativeAction payloads.
Validators reconstruct blocks from their local mempool. Fallback to proposer
fetch when a validator is missing an action.

EVM transactions stay as full content (small, few per block).

## Success Criteria
1. Native actions gossiped to all validators on RPC submission
2. Consensus datum contains action hashes, not full actions (~3x smaller than bincode, ~10x smaller than JSON)
3. Validators reconstruct full TorusBlock from local mempool + hashes
4. Fallback fetch when validator is missing an action referenced by hash
5. `bench-throughput consensus --senders 100 --duration 30` shows >2,000 included/sec (vs ~800 baseline)
6. All existing tests pass: `cargo test -p torus-consensus -p torus-bridge -p torus-mempool -p torus-network`
7. Attestation: proposer computes sig_attestation over full TorusBlock before
   compacting; validators re-derive the same hash after reconstruction

## Tasks

### Task 1: Move compute_action_hash to torus-types

**Test first**: `cargo test -p torus-types -- compute_action_hash`
```rust
#[test]
fn compute_action_hash_deterministic() {
    let action = make_test_action();
    let h1 = compute_action_hash(&action);
    let h2 = compute_action_hash(&action);
    assert_eq!(h1, h2);
    assert_ne!(h1, B256::ZERO);
}
```

**Implementation**:
- `crates/torus-types/src/lib.rs` — add pub fn:
```rust
pub fn compute_action_hash(action: &SignedNativeAction) -> B256 {
    let mut data = action.action.canonical_bytes();
    data.extend_from_slice(&action.nonce.to_be_bytes());
    alloy_primitives::keccak256(&data)
}
```
- `crates/torus-mempool/src/native_pool.rs:172` — replace body with
  `torus_types::compute_action_hash(action)`, keep the private fn as a wrapper
  or just call torus_types directly.

**Verify**: `cargo test -p torus-types -p torus-mempool`
**Depends on**: none

---

### Task 2: Add CompactBlock type to torus-types

**Test first**: `cargo test -p torus-types -- compact_block_roundtrip`
```rust
#[test]
fn compact_block_roundtrip() {
    let cb = CompactBlock {
        header: test_header(),
        native_action_hashes: vec![B256::repeat_byte(0x42); 10],
        evm_transactions: vec![vec![1, 2, 3]],
        core_writer_actions: vec![],
    };
    let encoded = bincode::serialize(&cb).unwrap();
    let decoded: CompactBlock = bincode::deserialize(&encoded).unwrap();
    assert_eq!(decoded.native_action_hashes.len(), 10);
}
```

**Implementation** — `crates/torus-types/src/lib.rs` after TorusBlock:
```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactBlock {
    pub header: TorusBlockHeader,
    pub native_action_hashes: Vec<B256>,
    pub evm_transactions: Vec<Vec<u8>>,
    pub core_writer_actions: Vec<CoreWriterAction>,
}

impl CompactBlock {
    pub fn from_block(block: &TorusBlock) -> Self {
        let hashes = block.native_actions.iter()
            .map(|a| compute_action_hash(a))
            .collect();
        Self {
            header: block.header.clone(),
            native_action_hashes: hashes,
            evm_transactions: block.evm_transactions.clone(),
            core_writer_actions: block.core_writer_actions.clone(),
        }
    }
}
```

**Verify**: `cargo test -p torus-types -- compact_block`
**Depends on**: T1

---

### Task 3: Wire native action gossip — outbound

**Test first**: Submit a native action via RPC on validator-0. Check logs on
validator-1 for receipt of the gossiped message.

**Implementation**:
- `crates/torus-network/src/behaviour.rs` — add constant:
  ```rust
  pub const NATIVE_ACTION_TOPIC: &str = "/torus/native-actions/1.0";
  ```
  Subscribe to this topic in `TorusBehaviour::new` alongside existing topics.

- `crates/torus-mempool/src/lib.rs` — add a gossip callback field:
  ```rust
  pub struct Mempool {
      // ... existing fields ...
      native_gossip_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
  }
  ```
  In `add_native_action` (L208), after successful insert, serialize the action
  with bincode and send on `native_gossip_tx` if Some.

- `crates/torus-network/src/swarm.rs` — in the swarm event loop, receive from
  the native gossip channel and publish to `NATIVE_ACTION_TOPIC`.

- `crates/torus-node/src/main.rs` — wire the gossip channel between mempool
  and network at startup.

**Verify**: Start devnet, submit native action, check logs on non-proposer for
`"received native action gossip"`.
**Depends on**: T1

---

### Task 4: Wire native action gossip — inbound

**Test first**: Submit native action on validator-0, check that validator-1's
mempool contains it (via torus_getOpenOrders or native pool size log).

**Implementation**:

Note: The existing TX_TOPIC handler (swarm.rs:284) only does dedup tracking
via `TxGossipState::should_accept` — it never delivers received transactions
to the Mempool. The native action inbound path must be built from scratch
with a complete delivery pipeline: gossipsub → deserialize → channel → mempool
insertion.

- `crates/torus-network/src/swarm.rs` — add handler for `NATIVE_ACTION_TOPIC`
  messages. Deserialize `SignedNativeAction` with bincode, send to a new
  `native_action_rx: mpsc::UnboundedReceiver<SignedNativeAction>` channel.
  Do NOT copy the TX_TOPIC pattern (which is receive-deaf).

- `crates/torus-node/src/main.rs` — spawn a task that reads from
  `native_action_rx` and calls `mempool.add_native_action(action)`.
  Use `add_native_action` (not `submit_native_action`) for safety —
  re-validates sig + nonce on the receiving side.

- Dedup: NativePool already deduplicates by `(sender, action_hash)` in the
  `seen` HashSet (native_pool.rs:60), so duplicate gossip messages are
  no-ops.

- Expected per-message size: ~200-500 bytes (bincode-encoded SignedNativeAction).
  Well within gossipsub `max_transmit_size: 256 KB`.

**Verify**: `cargo test -p torus-mempool -p torus-network` + devnet manual test.
**Depends on**: T3

---

### Task 5: Produce CompactBlock in consensus

**Test first**: `cargo test -p torus-consensus -- produce_compact_block`
```rust
#[test]
fn produce_compact_block_has_hashes() {
    // ... setup stub app ...
    // produce a block with native actions
    // deserialize datum as CompactBlock
    // assert native_action_hashes.len() == expected
    // assert native_action_hashes[0] == compute_action_hash(&action)
}
```

**Implementation** — `crates/torus-consensus/src/app.rs` in `produce_block`:
- After building `block: TorusBlock` (L788), store it in a local cache keyed
  by height (for later reconstruction requests).
- Compute `sig_attestation` over the full `TorusBlock` BEFORE compacting.
  The attestation hash must cover full action content, not just hashes.
  Validators will re-derive the same hash after reconstructing the full block.
- Serialize `CompactBlock::from_block(&block)` instead of the full block:
  ```rust
  let compact = CompactBlock::from_block(&block);
  let encoded = bincode::serialize(&compact).expect("serialize CompactBlock");
  ```
- The proposer still has full actions locally (just drained from mempool).
  Store them in a `pending_proposals: HashMap<u64, TorusBlock>` field on
  TorusApp for fallback fetch.

**Verify**: `cargo test -p torus-consensus`
**Depends on**: T1

---

### Task 6: Reconstruct TorusBlock from CompactBlock in validate_block

**Test first**: `cargo test -p torus-consensus -- validate_reconstructs_block`

**Implementation** — `crates/torus-consensus/src/app.rs` in `validate_block`:
- Deserialize datum as `CompactBlock` (not TorusBlock):
  ```rust
  let compact: CompactBlock = match bincode::deserialize(datum_bytes) { ... };
  ```
- Look up each hash in the local mempool:
  ```rust
  let mut native_actions = Vec::with_capacity(compact.native_action_hashes.len());
  let mut missing = Vec::new();
  for hash in &compact.native_action_hashes {
      match mempool.get_by_hash(hash) {
          Some(action) => native_actions.push(action),
          None => missing.push(*hash),
      }
  }
  ```
- If `missing` is not empty, trigger fallback (T7) before rejecting.
- Reconstruct full `TorusBlock` from compact + resolved actions.
- Re-derive the attestation hash from the reconstructed full block and verify
  it matches `sig_attestation` in the header.
- Run existing validation logic on the reconstructed block.

- `crates/torus-mempool/src/native_pool.rs` — add `hash_index: HashMap<B256, usize>`
  for O(1) lookup instead of linear scan. Update on insert/remove:
  ```rust
  pub fn get_by_hash(&self, hash: &B256) -> Option<SignedNativeAction> {
      self.hash_index.get(hash)
          .and_then(|&idx| self.entries.get(idx))
          .map(|e| e.action.clone())
  }
  ```
  Expose via `Mempool::get_native_by_hash` in lib.rs. The O(1) index is
  required to hit the 2,000 tx/sec target — linear scan over thousands of
  entries per hash would be O(n*m) for full block reconstruction.

- `on_committed_block`: same CompactBlock deserialization.

- `validate_block_for_sync`: This path CANNOT rely on mempool gossip state
  (the mempool may have evicted actions long ago). Sync blocks must carry
  full action content. Options:
  (a) Sync messages embed full `TorusBlock` bytes alongside the compact datum.
  (b) The sync protocol fetches full blocks from peers separately.
  For v1, use option (a): store full block bytes in `CF_BLOCK_BODIES` on
  commit, and use those for sync instead of the compact datum.

**Verify**: `cargo test -p torus-consensus -p torus-mempool`
**Depends on**: T4, T5

---

### Task 7: Fallback fetch for missing actions

**Test first**: Validator that hasn't received gossip for an action can still
validate the block after a short retry window.

**Implementation**:
- When `validate_block` finds missing hashes, do NOT immediately reject.
  Instead, wait up to 200ms with 50ms polling intervals for the missing
  actions to arrive via gossip:
  ```rust
  if !missing.is_empty() {
      for _ in 0..4 {
          tokio::time::sleep(Duration::from_millis(50)).await;
          missing.retain(|hash| mempool.get_by_hash(hash).is_none());
          if missing.is_empty() { break; }
      }
  }
  ```
  If still missing after retry, reject with `Invalid` and log the missing
  hashes at WARN level. Pure rejection without retry risks chain stall under
  network jitter, even with a 4-validator set.

- Future enhancement: add a `NativeActionFetchRequest` message to the
  block-data protocol to request specific actions by hash from the proposer.
  The proposer already caches full blocks in `pending_proposals` (T5).
  Defer to a follow-up task.

**Verify**: Devnet test — flood with bench-throughput, check that missing-hash
rejections are rare (<1%).
**Depends on**: T6

---

### Task 8: Integration test + benchmark

**Test first**: `bench-throughput consensus --senders 100 --duration 30`

**Implementation**:
- Rebuild devnet with all changes.
- Run bench-throughput in consensus mode.
- Compare included/sec against ~800/sec baseline.
- Monitor chain logs for missing-hash warnings.
- Run `cargo test -p torus-consensus -p torus-bridge -p torus-mempool` to
  confirm all existing tests pass.

**Verify**: Included/sec > 2,000. Zero missing-hash rejections under moderate
load.
**Depends on**: T7

---

## Verification (end-to-end)
```bash
cargo test -p torus-types -p torus-mempool -p torus-consensus -p torus-bridge
cd devnet && ./start.sh --build
# Wait for chain to stabilize
./target/release/bench-throughput consensus --senders 100 --duration 30
# Expect >2,000 included/sec
```

## Rollback
- Revert CompactBlock changes: switch produce_block back to serializing full
  TorusBlock (the bincode serialization from the prior change).
- Gossip can stay — it's additive and doesn't break anything if disabled.
- The compute_action_hash move to torus-types is safe to keep regardless.
