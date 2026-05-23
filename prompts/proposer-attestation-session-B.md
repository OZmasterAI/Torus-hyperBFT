# Session B: Proposer Attestation — Tasks 5-8 (Integration)

## Task
Implement Tasks 5-8 of the proposer attestation plan. Tasks 1-4 are already done.
Branch: `cte-architecture`. `git checkout cte-architecture` before starting.

**Read the full plan first:** `docs/plans/proposer-attestation-impl.md` — it has the
complete design, success criteria, task ordering, code snippets, and migration notes.
This prompt summarizes your tasks but the plan is the source of truth.

These are the integration tasks — all changes are in `crates/torus-consensus/src/app.rs`
plus devnet verification. They wire up the foundation from Tasks 1-4.

## Context
The CTE (consensus-then-execute) architecture with CTE8 execution pipelining is live.
`on_committed_block` sends blocks to a background execution thread via `SyncSender(64)`.
Execution happens post-commit. `validate_block` currently uses rayon parallel sig
verification (secp256k1 ECDSA recovery).

Tasks 1-4 added:
- `TorusBlockHeader.sig_attestation: [u8; 64]` — `#[serde(default)]`, excluded from
  canonical_header_bytes (Task 1)
- `batch_verify_native_actions(actions, timestamp, session_lookup) -> Vec<usize>` in
  `crates/torus-types/src/eip712.rs` — ed25519 batch + secp256k1 individual (Task 2)
- `generate_sig_attestation()` / `verify_sig_attestation()` in
  `crates/torus-bridge/src/proposer.rs` — SHA256 digest + ed25519 sign/verify (Task 3)
- `signing_key: Option<ed25519_dalek::SigningKey>` field on `TorusApp`, passed from
  main.rs (Task 4)

## Architecture of app.rs (current state)

`TorusApp` struct has these key fields:
- `signing_key: Option<SigningKey>` — proposer's ed25519 key (Task 4)
- `staking: StakingManager` — for validator lookups and slashing
- `pending_slashes: Vec<PendingSlash>` — drained in on_committed_block, sent to exec thread
- `exec_tx: Option<SyncSender<CommittedBlockMsg>>` — channel to execution thread
- `mempool: Option<Arc<Mempool>>` — drained in produce_block

`ExecutionContext` struct (owns execution thread state):
- `state_db`, `validator`, `evm_executor`, `staking`, config fields
- `execute_committed_block(&self, block, pending_slashes)` — the execution entry point

Key methods:
- `produce_block()` — drains mempool, builds TorusBlock, NO execution.
  Currently: `mempool.drain_for_block(256, gas_limit, parent_header.state_root)`
- `validate_block()` — structural checks + rayon parallel sig recovery. NO execution.
- `on_committed_block()` — deserializes block, drains pending_slashes, sends
  `CommittedBlockMsg { torus_block, pending_slashes }` through exec_tx channel.
- `ExecutionContext::execute_committed_block()` — EVM + native execution on background thread.

## Tasks to implement

### Task 5: produce_block — batch verify + attest + raise cap
**File:** `crates/torus-consensus/src/app.rs`

In `produce_block`, after draining mempool and before building the TorusBlock:

1. Call `batch_verify_native_actions()` on the native_actions. Remove any invalid
   actions (iterate indices in reverse to preserve ordering).
2. Call `generate_sig_attestation(&native_actions, key)` if `self.signing_key` is Some.
   Otherwise `[0u8; 64]`.
3. Set `header.sig_attestation = sig_attestation` on the TorusBlock.
4. Change `drain_for_block(256, ...)` → `drain_for_block(4096, ...)`.

Import `batch_verify_native_actions` from torus_types and `generate_sig_attestation`
from torus_bridge.

**Test:** Integration test: create TorusApp with signing key + mempool, submit native
actions, call produce_block, verify attestation is non-zero and valid.
**Verify:** `cargo test -p torus-consensus`

### Task 6: validate_block — check attestation only
**File:** `crates/torus-consensus/src/app.rs`

Replace the current rayon parallel sig verification with a two-path check:

```rust
if !torus_block.native_actions.is_empty() {
    if torus_block.header.sig_attestation == [0u8; 64] {
        // Fallback: pre-upgrade block, verify individually (rayon)
        use rayon::prelude::*;
        let all_valid = torus_block.native_actions.par_iter().enumerate().all(|(i, sa)| {
            // ... existing rayon check ...
        });
        if !all_valid { return ValidateBlockResponse::Invalid; }
    } else {
        // Fast path: check proposer's attestation only
        let proposer_pubkey = /* resolve from staking */;
        if !verify_sig_attestation(&torus_block.native_actions,
            &torus_block.header.sig_attestation, &proposer_pubkey) {
            return ValidateBlockResponse::Invalid;
        }
    }
}
```

To resolve the proposer's ed25519 VerifyingKey:
- `torus_block.header.proposer` gives the proposer Address
- `self.staking.get_validator(&proposer_addr)` returns the validator record
- The validator record has a `pubkey: [u8; 32]` field (ed25519)
- `ed25519_dalek::VerifyingKey::from_bytes(&pubkey)` gives the key

Import `verify_sig_attestation` from torus_bridge::proposer.

**Test:** Build block with valid attestation → passes. Tamper attestation → rejected.
Zero attestation → falls back to rayon per-action check.
**Verify:** `cargo test -p torus-consensus`
**Depends on:** Task 5 (needs produce_block to generate attestations for testing)

### Task 7: Execution thread — re-verify + slash proposer
**File:** `crates/torus-consensus/src/app.rs` (in `ExecutionContext::execute_committed_block`)

Before the native execution section (where `sender_actions` is built), add:

1. Call `batch_verify_native_actions()` on the block's native actions.
2. If any invalid indices returned: skip those actions (don't add to sender_actions).
3. Log at ERROR level with proposer address and count.
4. Queue a 100% slash + tombstone for the proposer. Use the existing `PendingSlash`
   mechanism — but since the execution thread doesn't have `pending_slashes` on TorusApp,
   directly call `self.staking.slash(proposer, 10000, SlashReason::InvalidAttestation, 0)`
   and `self.staking.tombstone_validator(&proposer)`. You may need to add
   `SlashReason::InvalidAttestation` variant to the SlashReason enum in torus-economics.

Import `batch_verify_native_actions` from torus_types.

**Test:** Create a block with one corrupted signature + valid attestation (simulating
malicious proposer). Execute through ExecutionContext. Verify: the bad action is skipped,
slash is applied.
**Verify:** `cargo test -p torus-consensus`
**Depends on:** Task 5 (needs attested blocks)

### Task 8: Devnet integration test
No code changes — this is rebuild + verify.

1. `docker compose build`
2. `docker compose up -d`
3. Wait for chain to start producing blocks
4. Run `python3 devnet/scripts/native-order-flood.py` with 100 senders
5. Verify:
   - Block time stays <80ms under load (check validator logs)
   - Blocks contain native actions (check `drained mempool` logs)
   - No consensus rejections
   - Execution thread processes blocks (check `execution pipeline: block done` logs)
   - Clean shutdown with `docker compose down`

**Verify:** Manual observation
**Depends on:** Tasks 5, 6, 7

## Key files to read first
- `crates/torus-consensus/src/app.rs` — THE file. Read the full thing.
- `crates/torus-bridge/src/proposer.rs` — where generate/verify_sig_attestation live
- `crates/torus-types/src/eip712.rs` — where batch_verify_native_actions lives
- `crates/torus-economics/src/staking.rs` — StakingManager, SlashReason enum

## What NOT to do
- Don't modify Tasks 1-4 files unless fixing a bug discovered during integration
- Don't remove the rayon fallback — it's needed for backward compatibility
- Don't change the execution pipeline architecture (SyncSender, ExecutionContext)
- Don't change on_committed_block — it already works correctly
- Don't skip the re-verification in the execution thread (Task 7) — it's the safety net

## Completion
When all tasks pass, commit with:
```
feat(attestation): proposer attestation in produce/validate_block + execution re-verify + 4096 action cap
```
