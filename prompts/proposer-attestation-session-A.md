# Session A: Proposer Attestation — Tasks 1-4 (Foundation)

## Task
Implement Tasks 1-4 of the proposer attestation + ed25519 batch verification plan.
Branch: `cte-architecture`. `git checkout cte-architecture` before starting.

**Read the full plan first:** `docs/plans/proposer-attestation-impl.md` — it has the
complete design, success criteria, task ordering, code snippets, and migration notes.
This prompt summarizes your tasks but the plan is the source of truth.

These are the foundation tasks — types, helpers, and plumbing. All in different crates,
all independent. Tasks 5-8 (integration in app.rs) will be done in a separate session.

## Context
The CTE (consensus-then-execute) architecture separates consensus from execution.
`produce_block` and `validate_block` are lightweight (no EVM), and `on_committed_block`
sends blocks to a background execution thread via `SyncSender`. Execution happens
post-commit on a dedicated thread (CTE8 pipelining).

Currently, `validate_block` verifies every native action's secp256k1 signature in the
consensus hot path. With 28 actions at ~1ms each (even with rayon parallel), this
dominates block time. The proposer attestation plan removes per-action verification
from validators — the proposer batch-verifies and attests, validators check one sig.

Ed25519 session keys already exist: `ActionSignature::Session` variant,
`resolve_sender(timestamp, lookup_fn)`, `CF_SESSIONS` storage,
`CreateSession`/`RevokeSession` actions, `SessionScope::Trading`.

## Tasks to implement

### Task 1: Add `sig_attestation` field to TorusBlockHeader
**File:** `crates/torus-types/src/lib.rs`

Add a `pub sig_attestation: [u8; 64]` field after `validator_set_hash` (line 339).
Use `#[serde(default)]` for backward-compatible deserialization of old blocks.
Exclude from `canonical_header_bytes()` — it's metadata, not block content.
Update ALL TorusBlockHeader constructors across the workspace to include
`sig_attestation: [0u8; 64]`. Search for `TorusBlockHeader {` to find them all —
they're in app.rs, test files, and bridge crates.

**Test:** Serialize/deserialize roundtrip with non-zero attestation.
**Verify:** `cargo test -p torus-types`

### Task 2: Batch signature verification utility
**File:** `crates/torus-types/src/eip712.rs`

Add `batch_verify_native_actions(actions, timestamp, session_lookup) -> Vec<usize>`
that returns indices of invalid actions. For `ActionSignature::Session` (ed25519):
collect into a batch and use `ed25519_dalek::verify_batch()`. For
`ActionSignature::Eip712` (secp256k1): verify individually via `recover_sender()`.
On batch failure, fall back to individual ed25519 verification to find which failed.

Key types to reference:
- `ActionSignature` enum in `eip712.rs` has `Eip712` and `Session` variants
- `SignedNativeAction` has `.signature` field and `.recover_sender()` method
- `SessionData` has `.is_valid(timestamp)` method
- `resolve_sender(timestamp, lookup_fn)` handles both paths

**Test:** Mix of valid/invalid ed25519 + secp256k1 sigs, verify correct indices returned.
**Verify:** `cargo test -p torus-types -- batch_verify`

### Task 3: Attestation generation and verification helpers
**File:** `crates/torus-bridge/src/proposer.rs`

Add two functions:
- `generate_sig_attestation(actions: &[SignedNativeAction], key: &SigningKey) -> [u8; 64]`
  — SHA256 hash all actions, sign with proposer's ed25519 key. Return `[0u8; 64]` for empty.
- `verify_sig_attestation(actions, attestation, pubkey) -> bool`
  — Recompute hash, verify the ed25519 signature. Zero attestation + empty actions = valid.

Use `sha2::Sha256` for the digest. Serialize each action with `serde_json::to_vec`.

**Test:** Generate → verify roundtrip. Tamper with actions → verify fails. Empty → zero attestation.
**Verify:** `cargo test -p torus-bridge -- attestation`
**Depends on:** Task 1 (needs the sig_attestation field to exist for integration)

### Task 4: Pass validator signing key into TorusApp
**Files:** `crates/torus-consensus/src/app.rs`, `crates/torus-node/src/main.rs`

Add `signing_key: Option<ed25519_dalek::SigningKey>` parameter to `TorusApp::new()`.
Store as field on `TorusApp` (add `#[allow(dead_code)]` for now — Task 5 will use it).

In `main.rs`, the validator's `ed25519_dalek::SigningKey` is already loaded for the
HotStuff replica configuration. Find where it's created (search for `SigningKey` in
main.rs), clone it, and pass to `TorusApp::new()`.

Update `stub()` to generate a random key:
```rust
let signing_key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
```

Pass `Some(signing_key)` to `Self::new()` in stub.

**Verify:** `cargo check -p torus-consensus -p torus-node`
**Depends on:** none

## What NOT to do
- Don't modify `produce_block`, `validate_block`, or `on_committed_block` — that's Session B
- Don't change the drain_for_block cap — that's Task 5
- Don't add rayon — it's already added
- Don't change the execution pipeline — it already works (CTE8)

## Completion
When all 4 tasks pass their verify commands, commit with:
```
feat(attestation): add sig_attestation header field, batch verify utility, attestation helpers, signing key plumbing
```
