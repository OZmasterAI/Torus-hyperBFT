Implement the direct-to-leader plan for Torus-hyperBFT (all 7 tasks, both steps).

## What this does
Step 1 (Tasks 1-4): Changes consensus from CompactBlock (action hashes) to full TorusBlock in the consensus datum. Eliminates block reconstruction failures under load.
Step 2 (Tasks 5-7): Adds leader forwarding — non-leader nodes forward native action submissions to the current HotStuff leader. Clients don't change. Gossip becomes catch-up fallback only.

## Plan location
Read `docs/plans/direct-to-leader-impl.md` for the full plan with exact line numbers and code snippets.
Read `PRPs/direct-to-leader.tasks.json` for the task dependency graph.

## Current branch: cte-architecture

## Architecture decisions (already made, do not revisit)
- **Leader discovery**: Any-node forwarding. Client API unchanged. Non-leader nodes forward `torus_submitNativeAction` to the current leader via the existing `NetworkCommand::Send` direct-message protocol.
- **Leader rotation**: Drop in-flight + mempool fallback. Old leader's mempool gossips unincluded actions to the new leader. Clients retry time-sensitive orders. No forwarding protocol needed.
- **Gossip**: Keep running. After Step 2 it carries stragglers and catch-up only — not the primary order flow. Do NOT delete gossip code.

---

## STEP 1: Full Blocks (Tasks 1-3, one file: crates/torus-consensus/src/app.rs)

### Task 1: produce_block serializes TorusBlock instead of CompactBlock
- Location: `produce_block` function (~L803-808)
- Currently: builds TorusBlock → converts to CompactBlock → `bincode::serialize(&compact)`
- Change to: `bincode::serialize(&block)` directly
- Keep: `pending_proposals.insert(height, block)` cache — still needed for proposer fast-path in on_committed_block. You'll need `block.clone()` since pending_proposals takes ownership.
- Keep: `pending_proposals.retain` eviction
- The parent header extraction (L719-735) reads the PARENT block's datum — it must handle BOTH formats since parent blocks may be in old CompactBlock format. This code already handles both (L724-727). Do not change it.

### Task 2: validate_block deserializes TorusBlock directly
- Location: `validate_block` function (~L844-963)
- Currently: deserializes CompactBlock (L844), reconstructs full block from mempool with 500ms retry loop (L867-908)
- Change to: deserialize TorusBlock directly. All actions are in the block — no mempool lookup needed.
- Keep UNCHANGED: data_hash verification (L838-842), EVM tx validation (L859-864), sig attestation check (L910-946), pending_proposals cache insertion (L949-957)
- Remove ENTIRELY: the mempool reconstruction block (L867-908) including the 500ms retry loop — this is the code that causes chain stalls
- Backward compat: if TorusBlock deserialization fails, try CompactBlock as fallback (for blocks produced before upgrade). If CompactBlock succeeds, do the mempool reconstruction as before.
- Access native_actions from the deserialized TorusBlock struct directly instead of reconstructing them.

### Task 3: on_committed_block deserializes TorusBlock directly  
- Location: `on_committed_block` function (~L975-1065)
- Currently (L986-990): tries CompactBlock first, falls back to TorusBlock
- Change: flip the order — try TorusBlock first, CompactBlock fallback
- Keep: `pending_proposals.remove(&height)` fast path (L1015) — this is the proposer's cached block, still correct
- Change: after the `if let Some(cached)` and the new TorusBlock deserialization branches, compute action hashes for `remove_committed_native` from the full block's `native_actions` using `torus_mempool::native_pool::compute_action_hash` (or the equivalent in torus-types). Previously used `compact.native_action_hashes` which won't exist for TorusBlock.

### Task 4: Devnet benchmark verification
- `cargo build --release -p torus-node`
- Rebuild devnet: `cd devnet && ./start.sh down && ./start.sh --build`
- Run: `bench-throughput consensus --duration 60 --senders 100 --concurrency 512 --rpc-urls "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548,http://localhost:8549"`
- Target: chain stays healthy, >1000 included/sec sustained
- Previously: chain stalled at 3 included actions total in 60s

---

## STEP 2: Leader Forwarding (Tasks 5-7, multiple files)

### Task 5: Expose current leader from consensus to node
- hotstuff_rs already has `select_leader(view: ViewNumber, validator_set: &ValidatorSet) -> VerifyingKey` (pacemaker/implementation.rs:L785) — it's a pure function
- The node needs: current view number + validator set to compute the leader
- Add a shared `Arc<AtomicU64>` for current view, updated by `on_committed_block` or a consensus event callback
- Add a method to compute the current leader: call `select_leader` with the view and the validator set from the last known state
- Files: `crates/torus-consensus/src/app.rs`, `crates/torus-node/src/main.rs`

### Task 6: Non-leader RPC forwards native actions to leader
- In `submit_native_action` RPC handler (`crates/torus-rpc/src/torus.rs`):
  - Check if this node is the current leader (compare own address with leader address from Task 5)
  - If leader: process normally (mempool insert + gossip, as today)
  - If NOT leader: validate the action (ECDSA verify — must still happen on first contact), insert into local mempool (fallback), then forward the serialized action to the leader via `NetworkCommand::Send` using the leader's VerifyingKey
- The leader receives the forwarded action via the direct-message protocol, deserializes, and inserts into its mempool
- Need a new `NetworkCommand` variant or reuse the direct-message protocol with a new message type
- Files: `crates/torus-rpc/src/torus.rs`, `crates/torus-node/src/main.rs`, `crates/torus-network/src/bridge.rs`

### Task 7: torus_getLeader RPC endpoint
- New RPC method: `torus_getLeader() -> { address: Address, peer_id: String, view: u64 }`
- Returns the current leader's Ethereum address, libp2p peer ID, and the consensus view
- Optional — for monitoring, explorers, and advanced clients that want direct connection
- File: `crates/torus-rpc/src/torus.rs`

---

## Key context about the codebase

### Consensus datum format
- hotstuff_rs blocks carry a `Data` field containing one or more `Datum` byte arrays
- `produce_block` returns `ProduceBlockResponse` with `data: Data::new(vec![Datum::new(encoded)])`
- `validate_block` reads `block.data.vec()[0].bytes()` to get the datum bytes
- Currently serialized as CompactBlock (bincode). After this change: TorusBlock (bincode).

### CompactBlock vs TorusBlock (crates/torus-types/src/lib.rs)
- `TorusBlock`: header + `Vec<SignedNativeAction>` + evm_transactions + core_writer_actions (~140 bytes per action)
- `CompactBlock`: header + `Vec<B256>` hashes + evm_transactions + core_writer_actions (~32 bytes per hash)
- At 1600 actions: TorusBlock ~224KB, CompactBlock ~51KB. Both under gossipsub 256KB limit.

### Block body persistence
- `execute_committed_block` (L317-324) stores `torus_block.body()` in CF_BLOCK_BODIES via serde_json
- This already uses the full TorusBlock — no change needed for persistence

### Existing gossip infrastructure (built this session, keep intact)
- Batched gossip: 50ms intervals, 0xFF marker wire format (swarm.rs)
- Bounded channel (8192) with backpressure (tx_gossip.rs, bridge.rs)
- Skip ECDSA on gossip inbound: sender Address in wire format (mempool/lib.rs, swarm.rs, main.rs)
- Separate swarm runtime: dedicated OS thread (bridge.rs)
- RPC max_connections=64 (torus-rpc/lib.rs)

### Leader selection
- `hotstuff_rs::pacemaker::select_leader(view, validator_set) -> VerifyingKey`
- `stable_leader` with `leader_tenure` already implemented — leader stays for N consecutive views
- VerifyingKey → PeerId: `torus_network::bridge::peer_id_from_verifying_key(&vk)`
- VerifyingKey → Address: derive from the ed25519 key (or look up from staking state)

### Test suites that must pass
- `cargo test -p torus-consensus` (2 integration + unit tests)
- `cargo test -p torus-network` (25 tests)
- `cargo test -p torus-mempool` (41 tests)
- `cargo test -p torus-rpc` (existing tests)

## Execution rules
1. Follow tasks in dependency order. Tasks 2 and 3 can run in parallel after Task 1.
2. Write failing tests BEFORE implementation for each task (TDD).
3. Run `cargo test` for affected crates after each task. Do not proceed if tests fail.
4. After Step 1 (Tasks 1-3): full test suite + devnet benchmark before starting Step 2.
5. CompactBlock backward compatibility: always try TorusBlock first, fall back to CompactBlock in both validate_block and on_committed_block.
6. Do NOT delete CompactBlock type or gossip code — they remain as fallback infrastructure.
7. Do NOT change the RPC API — clients must not need any changes.
8. After all tasks: rebuild devnet, run benchmark, target >1000 included/sec (Step 1) then >10,000 included/sec (Step 2).
