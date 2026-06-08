# Implementation Plan: Native-DA Big-Body Dissemination Fix (Option C)

**Branch:** `fix/native-da-bigbody` (off `phase-a-incremental-root`) · **Created:** 2026-06-08
**Design:** `docs/plans/native-da-bigbody-dissemination-fix.md` (Option C chosen)

## Design Decision
Make pull-DA the reliable big-body path: **per-protocol codec caps** (not one shared 4 MB) +
**client-side chunking** of the native-DA fetch + **widened pull budget**. Fixes the liveness wedge
where blocks >4 MB can't be reconstructed. PUSH sub-stream fix is out of scope (#4).

## Success Criteria
- A `NativeDaNetResponse` carrying >4 MB of bodies round-trips through `NativeDaCodec` (today: fails at 4 MB).
- A `BlockDataNetResponse` for a >4 MB block round-trips through `BlockDataCodec`.
- `fetch_native_actions_from_validators(N hashes)` issues `ceil(N / CHUNK)` requests per validator (today: 1 oversized request).
- `pull_missing_bodies` budget ≥ ~1 s (today: 80 ms), bodies arriving at ~500 ms are still recovered.
- All existing tests green: `torus-network`, `torus-consensus` (incl `four_node_consensus`), `torus-mempool`.
- The shared `MAX_DIRECT_MSG_SIZE` for the push/`/torus/direct` path is UNCHANGED (push fix = #4).

## Sizing (documented, conservative)
- Max single action = `PlaceOrderBatch` of `NATIVE_ORDERS_PER_BATCH_CAP=1024` orders ≈ ~75 KB (bincode envelope incl. sig).
- `NATIVE_DA_FETCH_CHUNK = 16` hashes/request → ≤ ~1.2 MB/response (comfortable headroom).
- `MAX_NATIVE_DA_MSG_SIZE = 8 MB` (headroom; chunking keeps actual ≪ this).
- `MAX_BLOCK_DATA_MSG_SIZE = 16 MB` (a full big block during sync; `NATIVE_ORDERS_PER_BLOCK_CAP=50_000` × ~order bytes + framing).
- `MAX_DIRECT_MSG_SIZE = 4 MB` UNCHANGED.

## Tasks (TDD — failing test first)

### Task 1: Per-protocol codec caps
- **Test first** (`codec.rs` tests, extend the existing module ~L256): `native_da_response_over_4mb_roundtrips` — build a `NativeDaNetResponse` whose serialized length is ~5 MB (e.g. 8 bodies × 700 KB), write+read it through `NativeDaCodec` via an in-memory duplex (`tokio::io::duplex`), assert equality. Add `block_data_response_over_4mb_roundtrips` similarly. Both MUST fail today ("message too large").
- **Implementation** (`crates/torus-network/src/codec.rs`):
  - Make `read_length_prefixed_borsh<T, D>(io, max_size: usize)` take an explicit cap (line 78–95); replace the hardcoded `MAX_DIRECT_MSG_SIZE` check at L86 with `max_size`.
  - Add `const MAX_NATIVE_DA_MSG_SIZE: usize = 8 * 1024 * 1024;` and `const MAX_BLOCK_DATA_MSG_SIZE: usize = 16 * 1024 * 1024;` keep `MAX_DIRECT_MSG_SIZE = 4MB`.
  - Each codec's `read_request`/`read_response` passes its own cap: `BorshCodec`→`MAX_DIRECT_MSG_SIZE`, `BlockDataCodec`→`MAX_BLOCK_DATA_MSG_SIZE`, `NativeDaCodec`→`MAX_NATIVE_DA_MSG_SIZE`.
- **Verify:** `cargo test -p torus-network codec`
- **Depends on:** —

### Task 2: Chunk the native-DA client fetch
- **Test first** (`bridge.rs` tests): `fetch_chunks_by_size` — drive `fetch_native_actions_from_validators` with 100 hashes against a 2-validator set; assert it enqueues `ceil(100/16)=7` `FetchNativeActions` commands **per validator** (=14), each with ≤16 hashes. (Use a test seam over `command_tx`/a counting receiver.) MUST fail today (1 request/validator).
- **Implementation** (`crates/torus-network/src/bridge.rs:212` `fetch_native_actions_from_validators`):
  - Add `pub const NATIVE_DA_FETCH_CHUNK: usize = 16;`
  - Replace the single `for target { send(all hashes) }` with `for target { for chunk in hashes.chunks(NATIVE_DA_FETCH_CHUNK) { send(chunk.to_vec()) } }`.
  - Server (`serve_native_da_bodies`) + `absorb_fetched_bodies` (by-hash) already handle partial/unordered — no change.
- **Verify:** `cargo test -p torus-network bridge` (or the fetch test module)
- **Depends on:** —

### Task 3: Widen the pull budget
- **Test first** (`app.rs` tests or a focused unit): assert the effective pull budget (`PULL_RETRIES * PULL_DELAY`) ≥ 1 s; if a behavioral test exists for `pull_missing_bodies`, assert a body delivered after ~500 ms is still recovered. MUST fail today (80 ms).
- **Implementation** (`crates/torus-consensus/src/app.rs:859`): `PULL_RETRIES: usize = 20`, `PULL_DELAY = 50ms` (→ 1.0 s). Keep the loop/absorb logic. (Sync path — blocking ≤1 s is safe, not the consensus hot path.)
- **Verify:** `cargo test -p torus-consensus`
- **Depends on:** —

### Task 4: End-to-end reconstruction proof (the headline test)
- **Test first** (integration test, `torus-network` or `torus-consensus`): seed a DA store with 100 bodies summing >4 MB; via the codec + chunked fetch + serve path, reconstruct all 100 by-hash; assert 0 missing. This is the test that would have caught the wedge.
- **Implementation:** none beyond Tasks 1–3 (wiring test only).
- **Verify:** `cargo test -p torus-network native_da_bigbody_reconstruct`
- **Depends on:** 1, 2

### Cross-cutting: regression gate
- **Verify:** `cargo test -p torus-network -p torus-consensus -p torus-mempool` and `cargo test -p torus-consensus four_node_consensus` all green.

## Verification (end-to-end)
`cargo build --release -p torus-node` then a LOCAL docker devnet bs=500→1000 sweep (NOT the live testnet) showing no body-fetch exhaustion / no wedge at >4 MB blocks.

## Rollback
All changes are additive/parameterizing; revert the branch. The shared-cap behavior is preserved for `/torus/direct`. No consensus-format or determinism change (bodies are content-addressed by hash; caps/chunking are transport-only).
