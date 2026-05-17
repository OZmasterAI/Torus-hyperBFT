# Implementation Plan: Hybrid Block Propagation + Pipelining (Option A)

## Design Decision
Header-first gossip + direct body fetch + early QC-triggered view advancement.
Design doc: `docs/plans/hybrid-pipelining.md`

## Success Criteria
1. Proposals split into header (gossip, <1KB) and body (direct fetch, up to 4MB)
2. Validators vote on header before body arrives
3. Views advance immediately when QC forms (no deadline wait)
4. All existing tests pass (`cargo test -p hotstuff_rs`)
5. Devnet 4-validator cluster produces blocks at <20ms under load
6. NATIVE_PER_BLOCK_CAP raised to 2000+ without gossip size errors

## Tasks

### Task 1: Define ProposalHeader and BlockBody message types

**Test first:**
```rust
// crates/hotstuff_rs/src/hotstuff/messages.rs — unit test
#[test]
fn proposal_header_roundtrip_borsh() {
    let header = ProposalHeader { /* ... */ };
    let bytes = borsh::to_vec(&header).unwrap();
    assert!(bytes.len() < 1024); // must fit in gossip comfortably
    let decoded: ProposalHeader = borsh::from_slice(&bytes).unwrap();
    assert_eq!(header, decoded);
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/messages.rs` — add:
```rust
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct ProposalHeader {
    pub chain_id: ChainID,
    pub view: ViewNumber,
    pub block_hash: CryptoHash,
    pub height: BlockHeight,
    pub data_hash: CryptoHash,
    pub justify: PhaseCertificate,
    pub tc: Option<TimeoutCertificate>,
    pub nec: Option<NoEndorsementCertificate>,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct BlockDataRequest {
    pub chain_id: ChainID,
    pub block_hash: CryptoHash,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct BlockDataResponse {
    pub block: Block,
}
```
- Add `ProposalHeader` variant to `HotStuffMessage` enum
- Add conversion: `impl From<&Proposal> for ProposalHeader`

**Verify:** `cargo test -p hotstuff_rs messages`
**Depends on:** None

---

### Task 2: Add block data fetch protocol to network layer

**Test first:**
```rust
// crates/torus-network/src/lib.rs or integration test
#[test]
fn block_data_request_response_codec() {
    let req = BlockDataRequest { chain_id, block_hash };
    let encoded = borsh::to_vec(&req).unwrap();
    assert!(encoded.len() < 100);
    // Response with a large block
    let resp = BlockDataResponse { block: large_block(5000) };
    let encoded = borsh::to_vec(&resp).unwrap();
    assert!(encoded.len() < 4 * 1024 * 1024); // fits in direct msg
}
```

**Implementation:**
- `crates/torus-network/src/behaviour.rs` — add new request-response protocol:
```rust
pub block_data: request_response::Behaviour<BorshCodec>, // /torus/block-data/1.0
```
- `crates/torus-network/src/swarm.rs` — handle `block_data` events:
  - On request: look up block in local store, respond with BlockDataResponse
  - On response: forward to algorithm thread via channel
- `crates/hotstuff_rs/src/networking/network.rs` — add trait methods:
```rust
fn request_block_data(&mut self, peer: VerifyingKey, request: BlockDataRequest);
fn recv_block_data(&mut self) -> Option<(VerifyingKey, BlockDataResponse)>;
```

**Verify:** `cargo test -p torus-network`
**Depends on:** Task 1

---

### Task 3: Proposer broadcasts header via gossip, stores body locally

**Test first:**
```rust
// crates/hotstuff_rs/src/hotstuff/tests.rs
#[test]
fn proposer_broadcasts_header_not_full_proposal() {
    // Setup: node is proposer for current view
    // Act: enter_view triggers proposal
    // Assert: broadcast called with HotStuffMessage::ProposalHeader, NOT Proposal
    // Assert: full block stored in local pending_bodies map
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/implementation.rs` — modify `enter_view` (L270-306):
  - After `app.produce_block()`, create `ProposalHeader` from the block
  - `self.sender_handle.broadcast::<HotStuffMessage>(header.into())`
  - Store full `Block` in new field: `pending_bodies: HashMap<CryptoHash, Block>`
- Add `pending_bodies` field to `HotStuff` struct (L74-88)

**Verify:** `cargo test -p hotstuff_rs enter_view`
**Depends on:** Task 1

---

### Task 4: Validators receive header, request body, vote on header

**Test first:**
```rust
#[test]
fn validator_votes_on_header_before_body() {
    // Setup: node receives ProposalHeader with valid justify
    // Act: on_receive_proposal_header processes it
    // Assert: PhaseVote is sent (vote on block_hash from header)
    // Assert: BlockDataRequest sent to proposer
    // Assert: block NOT yet in block tree (pending body)
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/implementation.rs` — add `on_receive_proposal_header`:
  - Verify `justify.is_correct()` and `safe_pc()`
  - Compute expected block_hash from (height, justify, data_hash)
  - Send PhaseVote for block_hash
  - Send BlockDataRequest to origin (the proposer)
  - Store header in `pending_headers: HashMap<CryptoHash, ProposalHeader>`
- Route `HotStuffMessage::ProposalHeader` in `on_receive_msg` (L460+)

**Verify:** `cargo test -p hotstuff_rs on_receive_proposal_header`
**Depends on:** Task 2, Task 3

---

### Task 5: Process body when it arrives, insert block into tree

**Test first:**
```rust
#[test]
fn body_arrival_inserts_block_and_updates_tree() {
    // Setup: header received, vote sent, body pending
    // Act: BlockDataResponse arrives with full block
    // Assert: block.hash matches pending header's block_hash
    // Assert: block inserted into block_tree
    // Assert: block_tree.update() called with justify
    // Assert: commit rule triggers if consecutive views
}
```

**Implementation:**
- `crates/hotstuff_rs/src/algorithm.rs` — add body fetch polling:
  - After network recv, check for incoming BlockDataResponse
  - Match response.block.hash against pending_headers
  - Validate block via `app.validate_block()`
  - Insert into block_tree, call `block_tree.update()`
- Handle timeout: if body doesn't arrive within `max_view_time / 2`, request from another peer

**Verify:** `cargo test -p hotstuff_rs algorithm`
**Depends on:** Task 4

---

### Task 5b: Data availability guard — delay commit until body confirmed

**Test first:**
```rust
#[test]
fn commit_delayed_until_body_available() {
    // Setup: header received, vote sent, QC formed (2-chain satisfied)
    // Act: block_tree.update() triggers commit rule
    // Assert: block NOT committed yet (body still pending)
    // Act: body arrives, is validated and inserted
    // Assert: NOW the block is committed and on_commit_block fires
}

#[test]
fn commit_proceeds_immediately_when_body_arrives_before_qc() {
    // Setup: header received, body fetched and inserted
    // Act: QC forms after body already in tree
    // Assert: commit happens immediately (happy path, zero extra latency)
}

#[test]
fn view_timeout_on_missing_body_blames_proposer() {
    // Setup: header received, votes sent, QC formed, body never arrives
    // Act: body fetch timeout fires (max_view_time / 2)
    // Assert: block NOT committed (no stuck state)
    // Assert: chain advances to next view with different leader
}
```

**Implementation:**
- `crates/hotstuff_rs/src/block_tree/accessors/internal.rs` — add:
  - `has_body(&self, hash: &CryptoHash) -> bool` — true if full block data stored
  - Track header-only blocks in `pending_bodies: HashSet<CryptoHash>`
- `crates/hotstuff_rs/src/block_tree/invariants.rs` — modify `block_to_commit`:
  - Before returning a block to commit, check `has_body(&block_hash)`
  - If body not available: return `Ok(None)` (defer commit, don't block)
- `crates/hotstuff_rs/src/algorithm.rs` — after body insertion in Task 5:
  - Re-run `block_tree.update()` with the stored justify/QC
  - The commit rule now finds the body available and commits
- Timeout: if body missing at `max_view_time / 2`, retry from other peers.
  If still missing at deadline, view times out naturally.

**Key property:** Happy path (99.9%): body arrives in ~5ms, vote collection
takes ~15ms. By the time the commit check runs, body is already there.
Zero latency cost. Only the withholding attack path pays extra.

**Verify:** `cargo test -p hotstuff_rs commit_body_guard`
**Depends on:** Task 5

---

### Task 6: Proposer serves block data requests

**Test first:**
```rust
#[test]
fn proposer_responds_to_block_data_request() {
    // Setup: proposer has produced a block and stored in pending_bodies
    // Act: BlockDataRequest arrives for that block_hash
    // Assert: BlockDataResponse sent back with full block
}
```

**Implementation:**
- `crates/hotstuff_rs/src/algorithm.rs` — handle incoming BlockDataRequest:
  - Look up in `hotstuff.pending_bodies` or `block_tree`
  - Send BlockDataResponse via direct message
- Any validator that has the block (from sync or prior fetch) can also serve

**Verify:** `cargo test -p hotstuff_rs block_data_serve`
**Depends on:** Task 3

---

### Task 7: Early view advancement on QC formation

**Test first:**
```rust
#[test]
fn view_advances_immediately_on_qc_not_at_deadline() {
    // Setup: pacemaker with 500ms deadline
    // Act: quorum PhaseVotes arrive at t=10ms, forming QC
    // Assert: AdvanceView sent immediately
    // Assert: view advanced to N+1 within 1ms of QC formation
    // Assert: did NOT wait until 500ms deadline
}
```

**Implementation:**
- `crates/hotstuff_rs/src/hotstuff/implementation.rs` — in `on_receive_phase_vote` (L901+):
  - When PhaseVoteCollector reaches quorum and forms a QC:
  - Immediately call `self.sender_handle.broadcast(AdvanceView { cert })` (already done)
  - Verify the pacemaker processes it without delay
- `crates/hotstuff_rs/src/pacemaker/implementation.rs` — in `on_receive_advance_view` (L411):
  - Ensure `update_view()` fires immediately, no batching or delay
  - Verify: the existing code already does this at L485 — confirm no artificial delay exists
- `crates/hotstuff_rs/src/algorithm.rs` — ensure the main loop doesn't park waiting for deadline when AdvanceView arrives mid-wait:
  - The `recv_deadline` in the network poll must be interruptible by AdvanceView

**Verify:** `cargo test -p hotstuff_rs pacemaker`
**Depends on:** None (independent of header/body split)

---

### Task 8: Wire everything together + raise caps

**Test first:**
```bash
# Integration test: devnet with 4 validators, NATIVE_PER_BLOCK_CAP=2000
# Send 100 native orders via tx-flood
# Assert: all orders land in blocks, no gossip size errors
# Assert: block time < 30ms average
# Assert: all 4 validators in sync
```

**Implementation:**
- `crates/torus-mempool/src/rate_limit.rs:33` — raise `NATIVE_PER_BLOCK_CAP` from 16 to 2000
- `crates/torus-network/src/behaviour.rs:42` — keep gossip max_transmit_size at 256KB (headers only now)
- Run full devnet test with load

**Verify:** `./devnet/start.sh --build && devnet/scripts/tx-loop.sh` — verify zero failures, sub-30ms blocks
**Depends on:** Tasks 1-7

---

### Task 9: Backward compatibility and fallback

**Test first:**
```rust
#[test]
fn legacy_full_proposal_still_accepted() {
    // Ensure nodes can still process old-style full Proposal messages
    // for rolling upgrades (not all validators upgrade simultaneously)
}
```

**Implementation:**
- Keep `HotStuffMessage::Proposal` variant (don't remove it)
- `on_receive_msg` handles both `Proposal` (legacy) and `ProposalHeader` (new)
- Legacy path: process as before (insert block directly)
- New path: vote on header, fetch body

**Verify:** `cargo test -p hotstuff_rs backward_compat`
**Depends on:** Task 4

## Verification (end-to-end)
```bash
cargo test -p hotstuff_rs
cargo test -p torus-network
cd devnet && ./start.sh --build
# Wait for startup, then:
# 1. Verify block time: curl localhost:8545 (measure over 10s)
# 2. Verify gossip messages are small: check validator logs for proposal size
# 3. Run tx-flood at high rate: verify 2000 orders/block with no failures
# 4. Kill one validator: verify others continue at same speed
# 5. Restart killed validator: verify it syncs via block sync
```

## Rollback
If something breaks during implementation:
- Revert to full-proposal gossip by setting a feature flag
- `HotStuffMessage::Proposal` path remains functional (Task 9)
- NATIVE_PER_BLOCK_CAP can be lowered back to 16 at any time
