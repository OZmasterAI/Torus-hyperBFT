# Implementation Plan: Consensus Fast-Path (Activate Header-First Proposals)

## Design Decision
The hybrid pipelining plan (Option A) was partially implemented: the receive side
(header validation, body fetch, retry, view-filter bypass) is fully wired. The
send side still broadcasts full `HotStuffMessage::Proposal` at all 5 proposal
sites. This plan flips the send side, reduces gossipsub heartbeat, and raises
the block cap — activating the fast-path end-to-end.

Design doc: `docs/plans/hybrid-pipelining.md`
Prior impl plan: `docs/plans/hybrid-pipelining-impl.md` (Tasks 1,2,4,5,5b,6,7,9 done)

## Success Criteria
1. All 5 proposal broadcast sites send `ProposalHeader` via gossip (not full `Proposal`)
2. Full block stored locally + served via `BlockDataRequest`/`BlockDataResponse`
3. Gossipsub heartbeat reduced from 500ms to 100ms
4. `NATIVE_PER_BLOCK_CAP` raised from 16 to 2000
5. All existing tests pass (`cargo test -p hotstuff_rs`, `cargo test -p torus-network`)
6. Devnet 4-validator cluster: blocks < 30ms average, zero gossip size errors
7. Legacy `Proposal` path still accepted (backward compat already in place)

## What's Already Done (receive side)
- `ProposalHeader`, `BlockDataRequest`, `BlockDataResponse` structs + tests (messages.rs:407-471)
- `on_receive_proposal_header` — votes before body, requests body (implementation.rs:1465-1627)
- `on_receive_block_data_response` → `try_insert_body` → `drain_deferred_bodies` (implementation.rs:1653-1677)
- `poll_block_data_responses` + `tick_pending_body_retries` in algorithm loop (algorithm.rs:177-191)
- Block-data transport protocol in swarm (serve + fetch + separate inbound queue)
- `is_block_data_msg()` view-filter bypass in receiving.rs:186
- `pending_bodies`, `pending_headers`, `body_fetch_tracker`, `deferred_bodies` fields (implementation.rs:92-101)
- Legacy `Proposal` path still handled in `on_receive_msg` (implementation.rs:640)

## Tasks

### Task 1: Extract helper to broadcast proposal as header

**Test first:**
```rust
// crates/hotstuff_rs/src/hotstuff/implementation.rs — unit test
#[test]
fn broadcast_proposal_as_header_sends_header_not_proposal() {
    // Setup: create a Proposal with block data
    // Act: call broadcast_proposal_as_header()
    // Assert: sender_handle.broadcast received HotStuffMessage::ProposalHeader
    // Assert: sender_handle.store_block_for_serving called with full block
    // Assert: pending_bodies contains the block
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/implementation.rs` — add private helper to `HotStuff`:
```rust
fn broadcast_proposal_as_header(&mut self, proposal: &Proposal) {
    self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
    self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
    let header = ProposalHeader::from_proposal(proposal);
    self.sender_handle.broadcast::<HotStuffMessage>(header.into());
}
```

**Verify:** `cargo test -p hotstuff_rs`
**Depends on:** None

---

### Task 2: Switch all 5 proposal broadcast sites to use header

**Test first:**
```rust
#[test]
fn enter_view_proposer_broadcasts_header() {
    // Setup: node is proposer, produce_block returns a block
    // Act: enter_view()
    // Assert: broadcast message is ProposalHeader, not Proposal
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/implementation.rs` — replace broadcast at all 5 sites:

**Site 1** (L267-269): Deferred proposal from pending body
```rust
// Before:
self.sender_handle.broadcast::<HotStuffMessage>(proposal.clone().into());
// After:
self.broadcast_proposal_as_header(&proposal);
```

**Site 2** (L361-364): Reproposal (validator-set-updating block)
```rust
// Same pattern — replace broadcast line
self.broadcast_proposal_as_header(&proposal);
```

**Site 3** (L381-384): TC-based proposal
```rust
self.broadcast_proposal_as_header(&proposal);
```

**Site 4** (L453-456): Normal new proposal (produce_block path)
```rust
self.broadcast_proposal_as_header(&proposal);
```

**Site 5** (L1295-1297): NEC recovery reproposal (on_receive_proposal_response)
```rust
self.broadcast_proposal_as_header(&proposal);
```

Each site already calls `pending_bodies.insert()` and `store_block_for_serving()` —
the helper consolidates this and switches the broadcast to header.

**Verify:** `cargo test -p hotstuff_rs`
**Depends on:** Task 1

---

### Task 3: Reduce gossipsub heartbeat from 500ms to 100ms

**Test first:**
```bash
# Build and verify compilation
cargo build -p torus-network
cargo test -p torus-network
```

**Implementation:**
- `crates/torus-network/src/config.rs:78` — change default:
```rust
// Before:
gossipsub_heartbeat_ms: 500,
// After:
gossipsub_heartbeat_ms: 100,
```

This reduces the worst-case gossip propagation delay for ProposalHeader
messages from 500ms to 100ms. Since headers are <1KB, the higher heartbeat
rate has negligible bandwidth cost.

**Verify:** `cargo test -p torus-network`
**Depends on:** None

---

### Task 4: Raise NATIVE_PER_BLOCK_CAP from 16 to 2000

**Test first:**
```bash
cargo test -p torus-mempool
```

**Implementation:**
- `crates/torus-mempool/src/rate_limit.rs:33`:
```rust
// Before:
pub const NATIVE_PER_BLOCK_CAP: usize = 16;
// After:
pub const NATIVE_PER_BLOCK_CAP: usize = 2000;
```

With proposals now header-only via gossip (< 1KB), the 256KB gossip limit
no longer constrains block size. Bodies travel via direct request-response
(4MB limit). 2000 native actions × ~200 bytes each = ~400KB body, well
within the 4MB direct message cap.

**Verify:** `cargo test -p torus-mempool`
**Depends on:** None

---

### Task 5: Integration test — devnet benchmark

**Test first:**
```bash
# Full build
cargo build --release -p torus-node

# Start devnet
cd devnet && ./start.sh --build

# Benchmark
../target/release/bench-throughput consensus \
  --duration 60 --senders 100 \
  --rpc-urls "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548,http://localhost:8549"
```

**Verify — success criteria:**
- Block time: < 30ms average (currently ~52ms)
- No gossip size errors in validator logs
- All 4 validators stay in sync
- Native actions included in blocks (not rejected)
- `grep "NoPeersSubscribedToTopic" logs` — zero after mesh forms
- Kill one validator → others continue; restart → syncs via block-sync

**Depends on:** Tasks 1-4

## Verification (end-to-end)
```bash
cargo test -p hotstuff_rs
cargo test -p torus-network
cargo test -p torus-mempool
cargo build --release -p torus-node
cd devnet && ./start.sh --build
# Monitor: block time, gossip message sizes, validator sync
# Load test: bench-throughput with high concurrency
```

## Rollback
- Legacy `Proposal` receive path is still in the codebase (implementation.rs:640)
- To revert: change `broadcast_proposal_as_header` to broadcast full Proposal again
- `NATIVE_PER_BLOCK_CAP` and `gossipsub_heartbeat_ms` are single-line reverts
