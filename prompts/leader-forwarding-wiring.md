## Task: Wire leader forwarding + spawn_blocking for RPC ECDSA

### Context
We're implementing direct-to-leader for Torus-hyperBFT on branch `cte-architecture`. Tasks 1-3 (TorusBlock in consensus datum), Task 5 (LeaderState), Task 6 partial (network + RPC), and Task 7 (getLeader) are done. Two things remain:

### 1. `spawn_blocking` for RPC ECDSA in `submit_native_action`

**File**: `crates/torus-rpc/src/torus.rs` — `submit_native_action` method (~L592)

**Problem**: ECDSA signature verification (`action.validate_with_sessions(...)`) runs on the async tokio runtime. Under load (512 concurrent RPC connections), this saturates the runtime and starves the libp2p swarm (which shares the same runtime via `tokio::spawn`). Chain stalls at 204 included actions in 60s.

**Fix**: Wrap the ECDSA verify + serde deserialization in `tokio::task::spawn_blocking` so it runs on the blocking thread pool instead of the async runtime. The mempool insert and leader forwarding can stay async.

**Current flow** (all on async runtime):
1. `parse_bytes` → decode hex
2. `serde_json::from_slice` → deserialize SignedNativeAction  
3. `action.validate_with_sessions(...)` → ECDSA verify (EXPENSIVE)
4. `serde_json::to_vec(&action)` → serialize for hash
5. `keccak256` → compute hash
6. `mempool.add_native_action_presigned(sender, action)` → mempool insert
7. Leader forwarding (if not leader)

Steps 2-5 should move into `spawn_blocking`. Return `(sender, action, action_bytes, hash)` from the blocking task. Steps 6-7 stay async.

**Constraint**: `self` (RpcState) fields needed inside the blocking closure: `self.state` (StateDb, Clone), `self.chain_id` (u64, Copy). The `action` must be cloned before the mempool insert since forwarding also needs it.

### 2. Wire Task 6 in `main.rs`

**File**: `crates/torus-node/src/main.rs`

**What exists**:
- `TorusApp::leader_state() -> Arc<LeaderState>` — returns shared consensus state (current view + validator set)
- `LeaderState::current_leader() -> Option<VerifyingKey>` — computes current leader via `select_leader`
- `LibP2PNetwork::forward_native_action(target: VerifyingKey, payload: Vec<u8>)` — sends action to leader peer
- `RpcServer::set_leader_forwarding(own_vk, leader_vk_fn, forward_tx)` — configures RPC forwarding
- RPC forwarding payload format: sender_address(20 bytes) + serde_json(SignedNativeAction)

**What to wire** (in `main.rs::run()`, between app creation and replica start):

1. Get `leader_state` from app before replica consumes it:
```rust
let leader_state = app.leader_state(); // Arc<LeaderState>
```

2. Clone the network before replica consumes it:
```rust
let network_for_fwd = network.clone(); // LibP2PNetwork is Clone
```

3. After RPC server creation, set up forwarding:
```rust
let own_vk = signing_key.verifying_key().to_bytes();
let leader_state_for_rpc = leader_state.clone();
let leader_vk_fn = Arc::new(move || {
    leader_state_for_rpc.current_leader().map(|vk| vk.to_bytes())
});
let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel();
rpc_server.set_leader_forwarding(own_vk, leader_vk_fn, fwd_tx);
```

4. Spawn the forwarding bridge task (after RPC server start):
```rust
tokio::spawn(async move {
    while let Some((target_vk_bytes, payload)) = fwd_rx.recv().await {
        if let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&target_vk_bytes) {
            network_for_fwd.forward_native_action(vk, payload);
        }
    }
});
```

**Key imports needed**: `Arc` (already imported), `ed25519_dalek::VerifyingKey` (check if imported)

### Verification
1. `cargo test -p torus-consensus -p torus-rpc -p torus-network` — all must pass
2. `cargo build --release -p torus-node` — must compile
3. Rebuild devnet: `cd devnet && docker compose up -d --build`
4. Check block speed: should be 80-150ms/block idle
5. Run benchmark: `target/release/bench-throughput consensus --duration 60 --senders 100 --concurrency 512 --rpc-urls "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548,http://localhost:8549"`
6. Target: >1000 included/sec sustained (was 204 total before spawn_blocking)

### Rules
- Read the current file state before editing — don't assume line numbers
- Don't add features beyond what's described
- Run tests after each change
- The working-summary is at `~/.claude/rules/working-summary.md` — update it when done
