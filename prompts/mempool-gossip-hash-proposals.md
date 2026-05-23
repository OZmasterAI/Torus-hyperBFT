Implement the mempool gossip + hash-based consensus proposals plan for Torus-hyperBFT.

## Context
The matching engine handles 660k orders/sec isolated, but the consensus pipeline tops out at ~800 included/sec because full block bodies (218KB bincode for 1,600 actions) bottleneck P2P transfer. This plan gossips native actions to all validators via gossipsub so consensus proposals only carry action hashes (~50KB), and validators reconstruct blocks from their local mempool.

## Plan location
Read `docs/plans/mempool-gossip-hash-proposals-impl.md` for the full 8-task TDD plan with exact file paths, line numbers, and code snippets. Read `PRPs/mempool-gossip-hash-proposals.tasks.json` for the task dependency graph.

## Current branch: cte-architecture

## Key architecture facts (verified this session)
- `compute_action_hash` exists at `crates/torus-mempool/src/native_pool.rs:172` — private fn, uses `keccak256(canonical_bytes + nonce)`. Must move to torus-types (avoid circular dep with CompactBlock).
- `NativePool` already deduplicates by `(sender, action_hash)` in a `seen: HashSet` (native_pool.rs:60), so gossip duplicates are free no-ops.
- Two gossipsub topics exist: `CONSENSUS_TOPIC` and `TX_TOPIC` (behaviour.rs:12-13). TX_TOPIC inbound is receive-deaf (swarm.rs:283-288 — does dedup tracking only, never delivers to mempool). Add a new `NATIVE_ACTION_TOPIC` with a complete inbound delivery pipeline: gossipsub → deserialize → channel → `mempool.add_native_action()`. Do NOT copy the TX_TOPIC pattern.
- `TxGossipHandle::submit_tx` (tx_gossip.rs:74) sends outbound on TX_TOPIC via mpsc channel. Use the same pattern for native action gossip outbound.
- `TorusBlock` (torus-types/lib.rs:342) always carries full `Vec<SignedNativeAction>`. The new `CompactBlock` type carries `Vec<B256>` hashes instead.
- Consensus datum serialization already uses bincode (swapped from JSON this session). `produce_block` at app.rs:795, `validate_block` at app.rs:831, `on_committed_block` at app.rs:919.
- `drain_for_block(4096, ...)` at app.rs:743 — proposer drains mempool then builds block.
- Attestation: `sig_attestation` must be computed over full `TorusBlock` BEFORE compacting to `CompactBlock`. Validators re-derive the same hash after reconstructing the full block and verify it matches.
- Gossipsub max message: 256KB. Single SignedNativeAction ~140 bytes bincode. Plenty of headroom.
- `Mempool::add_native_action` (lib.rs:208) validates sig + nonce window. Use this for inbound gossip (re-validates for safety).
- `NativePoolEntry` stores `action_hash: B256` already (native_pool.rs:17). Add `hash_index: HashMap<B256, usize>` to NativePool for O(1) `get_by_hash` lookup — linear scan is too slow for block reconstruction at scale.
- `validate_block_for_sync` cannot rely on mempool (actions may be evicted). Sync path must use full block bytes from `CF_BLOCK_BODIES`.

## Execution rules
1. Follow the plan task-by-task in dependency order. Tasks 1-2 first, then T3-4 and T5 can be parallel tracks, then T6-7, then T8.
2. Write failing tests BEFORE implementation for each task (TDD).
3. Run `cargo test` for affected crates after each task. Do not proceed if tests fail.
4. Do NOT change the RPC layer — it stays JSON. Do NOT change storage format (CF_BLOCK_BODIES/CF_BLOCK_HEADERS stay serde_json).
5. Do NOT change EVM transaction handling — they stay as full content in the block.
6. Attestation: proposer computes `sig_attestation` over full `TorusBlock` before compacting. Validators re-derive and verify after reconstruction.
7. For Task 7 (fallback), implement a 200ms retry window (4x50ms polls) before rejecting on missing hashes. Pure rejection risks chain stall. Dedicated fetch protocol is a follow-up.
8. After all tasks, rebuild devnet (`cd devnet && ./start.sh --build`) and run `./target/release/bench-throughput consensus --senders 100 --duration 30` to measure improvement.
