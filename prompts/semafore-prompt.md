Task: Add admission control semaphore to submit_native_action

Problem

Under 512 concurrent RPC connections, submit_native_action floods the tokio runtime and starves the libp2p swarm task, causing the consensus chain to stall (0 blocks in 60s). The tx-loop script at 20 in-flight works fine (median 143ms blocks). We need to cap in-flight submissions.

What to do

Add a tokio::sync::Semaphore with 64 permits to gate submit_native_action. When all permits are taken, immediately return an RPC error (don't queue).

Files to modify

crates/torus-rpc/src/lib.rs:
- Add field pub(crate) submit_semaphore: Arc<tokio::sync::Semaphore> to RpcState (line ~138)
- Initialize it in RpcServer::new() with Arc::new(tokio::sync::Semaphore::new(64)) (line ~168)

crates/torus-rpc/src/torus.rs:
- At the top of submit_native_action (~line 596), before parse_bytes:
let _permit = self.submit_semaphore.try_acquire()
    .map_err(|_| ErrorObjectOwned::from(RpcError::Internal("server overloaded, try again".into())))?;
- The _permit drops automatically at end of function, releasing the slot

Constraints

- Don't touch any other RPC method — only submit_native_action
- Don't add CLI flags or configuration — hardcode 64 for now
- Run cargo test -p torus-rpc after — all existing tests must pass
- Run cargo build -p torus-node to verify it compiles

Verification

After implementing, start devnet (cd devnet && ./start.sh) and run:
target/release/bench-throughput consensus --duration 60 --senders 100 --concurrency 512 --rpc-urls "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548,http://localhost:8549"
The chain should NOT stall — blocks should keep progressing even if most submissions get rejected. Target: >0 included actions and block time staying under 500ms.
