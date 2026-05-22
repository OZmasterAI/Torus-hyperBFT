# Bench-Throughput — Rust benchmarking tool for Torus-hyperBFT

Copy-paste this into a fresh Claude session to build the tool.

---

## Task: Build a Rust benchmarking tool for Torus-hyperBFT order throughput

### What this is
A CLI tool at `tools/bench-throughput/` that measures the real end-to-end throughput
of the Torus devnet — how many native orders (PlaceOrder) and EVM transfers land in
committed blocks per second under sustained load.

### Why
We need a real number to compare against Hyperliquid's claimed 100-200k orders/sec.
The current Python native-order-flood.py only pushes ~150 ops/sec (script-limited).
We need a Rust tool that can saturate the chain and measure the INCLUSION rate.

### Current performance context
The `cte-architecture` branch uses consensus-then-execute with execution pipelining
(CTE8). `on_committed_block` sends blocks to a background execution thread via a
bounded `SyncSender` channel, so consensus proceeds at ~43ms/block regardless of
execution load. Prior to pipelining, combined EVM+native load pushed block times to
238-357ms. The benchmark should confirm block times stay under 80ms under load.

### Architecture
Three modes, each answering a different question:

1. `bench-throughput matching-engine` — Isolated benchmark of the native matching
   engine (no consensus, no network). Instantiates NativeExecutor + StateDb directly,
   feeds PlaceOrder actions in batches, measures orders/sec. This tests the theoretical
   max of execute_batch with parallel per-market matching.

2. `bench-throughput consensus` — Floods the running devnet with native PlaceOrder
   actions via RPC, then polls committed blocks to count how many actually landed.
   Reports: submitted/sec, included/sec, latency p50/p95/p99, block time stats.

3. `bench-throughput combined` — Same as consensus but sends BOTH EVM transfers
   AND native PlaceOrder actions simultaneously. Measures both pipelines independently.

### Key files to reference
- `crates/torus-bridge/src/native_executor.rs` — NativeExecutor,
  parallel per-market matching, PlaceOrder handling
- `crates/torus-types/src/lib.rs` — NativeAction::PlaceOrder, SignedNativeAction,
  TorusBlock, TorusBlockHeader
- `crates/torus-types/src/eip712.rs` — `sign_native_action(action, nonce, &SigningKey)`
  uses **k256::ecdsa::SigningKey** (secp256k1), NOT ed25519
- `crates/torus-state/src/db.rs` — StateDb for isolated benchmarks
- `crates/torus-genesis/src/lib.rs` — genesis state setup with market seeding
- `devnet/scripts/native-order-flood.py` — reference for RPC submission format
  (study how it signs and submits native actions via EIP-712)
- `devnet/scripts/tx-loop.sh` — reference for EVM tx submission
- `crates/torus-rpc/src/torus.rs` — RPC endpoints:
  - `torus_submitNativeAction` (line 127) — submit a signed native action (hex-encoded JSON)
  - `torus_getBlockBody` (line 145) — get block body with native_action_count
  - Standard eth_* endpoints: eth_sendRawTransaction, eth_getBlockByNumber, eth_blockNumber

### Implementation details

**Mode 1 (matching-engine):**
- Create an in-memory StateDb (use TempDir)
- Initialize genesis state with markets (use torus-genesis)
- Pre-generate N PlaceOrder actions as resolved `(Address, NativeAction)` tuples —
  mode 1 bypasses signing entirely and feeds pre-resolved pairs directly to
  `NativeExecutor::execute_batch`. Random prices around a mid-price, random quantities.
- Warm up with 1000 orders, then measure batches of 10k/50k/100k
- Use std::time::Instant for timing
- Report: orders/sec, with market count (test 1, 4, 16, 64 markets)
- This is a pure Rust benchmark — no network, no consensus

**Mode 2 (consensus):**
- Connect to devnet RPC (default http://localhost:8545)
- Use the devnet Hardhat mnemonic accounts (same keys as native-order-flood.py —
  derive via `cast wallet derive-private-key` or pre-load from devnet/accounts.csv)
- Sign native actions with `torus_types::eip712::sign_native_action` using
  `k256::ecdsa::SigningKey` (NOT ed25519 — ed25519 is for consensus validator keys)
- Spawn N sender tasks (tokio) each submitting PlaceOrder at max rate via
  `torus_submitNativeAction` JSON-RPC
- Separately, poll eth_blockNumber + `torus_getBlockBody` every 500ms
- Count native_action_count in each new block
- Run for --duration seconds (default 60)
- Report: submit_rate, inclusion_rate, latency percentiles, block_time_avg

**Mode 3 (combined):**
- Same as mode 2 but also spawn EVM transfer senders (using alloy/ethers)
- Report both native and EVM throughput separately

### Output format
```
=== Torus Throughput Benchmark ===
Mode: consensus
Duration: 60s
Senders: 16

Submitted:  45,230 native actions (753/s)
Included:   38,400 native actions (640/s)  ← this is the real number
Drop rate:  15.1%
Block time: 245ms avg (182ms p50, 412ms p95)
Blocks:     244 total, 157 avg native/block

Peak:       892/s (block #12045, 218 actions)
Sustained:  640/s (over 60s)
```

### Cargo.toml dependencies
- tokio (async runtime)
- clap (CLI parsing)
- reqwest (HTTP RPC calls)
- serde/serde_json (JSON-RPC)
- alloy-primitives, alloy-signer (for EVM tx signing in mode 3)
- k256 (for native action signing — secp256k1 EIP-712)
- torus-types, torus-bridge, torus-state, torus-genesis (for mode 1 isolated bench)

### Testing
- `cargo build -p bench-throughput` must succeed
- Mode 1 should run without a devnet (isolated)
- Modes 2/3 require a running devnet

### What NOT to do
- Don't use the Python flood script as a subprocess — this is pure Rust
- Don't measure submission rate as throughput — measure INCLUSION rate
- Don't run for <30 seconds — need sustained measurement
- Don't hardcode keys — read from the same Hardhat mnemonic genesis accounts
- Don't use ed25519 for native action signing — native uses k256/secp256k1 via EIP-712.
  ed25519 is only for consensus validator keys.
