# Implementation Plan: Proposer Attestation + Ed25519 Batch Verification (B+C)

## Design Decision
Combine proposer attestation with ed25519 batch verification to remove per-action
signature verification from the consensus critical path. Target: ~95-190k native
ops/sec (4096-8192 actions/block at ~43ms block time).

## Why This Works
- CTE8 execution pipelining already runs execution post-commit on a background thread
- Ed25519 session keys already exist (ActionSignature::Session, resolve_sender, CF_SESSIONS)
- Proposer batch-verifies sigs (~5ms for 4096 ed25519), attests with a single signature
- Validators check one attestation sig (~0.05ms) instead of N action sigs
- Execution thread re-verifies post-commit as safety net, slashes proposer if invalid
- Economic security: proposer's full stake at risk for one block of invalid actions

## Success Criteria
1. `cargo test --workspace` passes
2. `validate_block` checks only attestation signature — O(1) regardless of block size
3. `produce_block` batch-verifies all native sigs + generates attestation
4. Execution thread re-verifies all sigs, slashes proposer if any invalid
5. Blocks with 4096 native actions produce and validate in <50ms total
6. Backward compatible: zero attestation = verify individually (rolling upgrade)
7. EIP-712 (secp256k1) actions still accepted, verified individually by proposer
8. Devnet stable under native-order-flood with 4096-action blocks

## Tasks

### Task 1: Add `sig_attestation` field to TorusBlockHeader
**Files:** `crates/torus-types/src/lib.rs`

**Test first:**
```rust
#[test]
fn header_with_attestation_serializes() {
    let mut header = /* existing test header */;
    header.sig_attestation = [42u8; 64];
    let json = serde_json::to_vec(&header).unwrap();
    let decoded: TorusBlockHeader = serde_json::from_slice(&json).unwrap();
    assert_eq!(decoded.sig_attestation, [42u8; 64]);
}
```

**Implementation:**
Add after `validator_set_hash` field (line 339 of `crates/torus-types/src/lib.rs`):
```rust
    /// Proposer's ed25519 attestation over native action signatures.
    /// Zero-filled for blocks with no native actions or pre-upgrade blocks.
    #[serde(default)]
    pub sig_attestation: [u8; 64],
```

`#[serde(default)]` ensures backward-compatible deserialization of old blocks
that lack this field (deserializes as `[0u8; 64]`).

Exclude `sig_attestation` from `canonical_header_bytes()` — it's metadata about
the block, not content that should affect block hash.

Update all TorusBlockHeader constructors across the codebase to include
`sig_attestation: [0u8; 64]`.

**Verify:** `cargo test -p torus-types`
**Depends on:** none

### Task 2: Batch signature verification utility
**Files:** `crates/torus-types/src/eip712.rs`

**Test first:**
```rust
#[test]
fn batch_verify_mixed_sigs() {
    let key = k256::ecdsa::SigningKey::random(&mut rand::thread_rng());
    let ed_key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
    // Create session, sign actions with both key types
    let actions = vec![
        sign_native_action(NativeAction::ClaimRewards, 1, &key),      // eip712
        sign_native_action_session(NativeAction::ClaimRewards, 2, &ed_key), // session
    ];
    let invalid = batch_verify_native_actions(&actions, now, |_| None);
    // eip712 valid, session invalid (no session in DB) → [1]
}
```

**Implementation:**
```rust
pub fn batch_verify_native_actions(
    actions: &[SignedNativeAction],
    timestamp: u64,
    session_lookup: impl Fn(&[u8; 32]) -> Option<SessionData>,
) -> Vec<usize> {
    let mut invalid = Vec::new();
    let mut ed25519_batch: Vec<(usize, &[u8], ed25519_dalek::Signature, ed25519_dalek::VerifyingKey)> = Vec::new();

    for (i, action) in actions.iter().enumerate() {
        match &action.signature {
            ActionSignature::Eip712 { .. } => {
                // secp256k1: verify individually (~1ms each, but rare)
                if action.recover_sender().is_err() {
                    invalid.push(i);
                }
            }
            ActionSignature::Session { pubkey, sig, .. } => {
                // Collect for batch verification
                match session_lookup(pubkey) {
                    Some(session) if session.is_valid(timestamp) => {
                        // parse sig + pubkey, push to ed25519_batch
                    }
                    _ => invalid.push(i), // no valid session
                }
            }
        }
    }

    // Batch verify all ed25519 sigs at once
    if !ed25519_batch.is_empty() {
        // ed25519_dalek::verify_batch(messages, signatures, verifying_keys)
        // On failure: fall back to individual verification to find which failed
    }

    invalid
}
```

**Verify:** `cargo test -p torus-types -- batch_verify`
**Depends on:** none

### Task 3: Attestation generation and verification helpers
**Files:** `crates/torus-bridge/src/proposer.rs`

**Test first:**
```rust
#[test]
fn attestation_roundtrip() {
    let key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
    let actions = vec![make_test_signed_action(1), make_test_signed_action(2)];
    let att = generate_sig_attestation(&actions, &key);
    assert!(verify_sig_attestation(&actions, &att, &key.verifying_key()));
    // tamper
    let mut bad = actions.clone();
    bad.push(make_test_signed_action(3));
    assert!(!verify_sig_attestation(&bad, &att, &key.verifying_key()));
}

#[test]
fn empty_actions_zero_attestation() {
    let key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
    let att = generate_sig_attestation(&[], &key);
    assert_eq!(att, [0u8; 64]);
    assert!(verify_sig_attestation(&[], &[0u8; 64], &key.verifying_key()));
}
```

**Implementation:**
```rust
use ed25519_dalek::{Signer, Verifier};
use sha2::{Digest, Sha256};

pub fn generate_sig_attestation(
    native_actions: &[SignedNativeAction],
    proposer_key: &ed25519_dalek::SigningKey,
) -> [u8; 64] {
    if native_actions.is_empty() {
        return [0u8; 64];
    }
    let digest = attestation_digest(native_actions);
    proposer_key.sign(&digest).to_bytes()
}

pub fn verify_sig_attestation(
    native_actions: &[SignedNativeAction],
    attestation: &[u8; 64],
    proposer_pubkey: &ed25519_dalek::VerifyingKey,
) -> bool {
    if native_actions.is_empty() {
        return *attestation == [0u8; 64];
    }
    let digest = attestation_digest(native_actions);
    let Ok(sig) = ed25519_dalek::Signature::from_bytes(attestation) else {
        return false;
    };
    proposer_pubkey.verify(&digest, &sig).is_ok()
}

fn attestation_digest(actions: &[SignedNativeAction]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for action in actions {
        hasher.update(&serde_json::to_vec(action).unwrap_or_default());
    }
    hasher.finalize().into()
}
```

**Verify:** `cargo test -p torus-bridge -- attestation`
**Depends on:** Task 1

### Task 4: Pass validator signing key into TorusApp
**Files:** `crates/torus-consensus/src/app.rs`, `crates/torus-node/src/main.rs`

**Test first:**
```rust
// In stub(), verify signing_key is set
#[test]
fn stub_has_signing_key() {
    let app = TorusApp::stub();
    // app should have a signing key (random for tests)
}
```

**Implementation:**
Add `signing_key: Option<ed25519_dalek::SigningKey>` parameter to `TorusApp::new()`.
Store as field on `TorusApp`. Validators pass `Some(key)`, RPC nodes pass `None`.

In `main.rs`, the validator's signing key is already loaded for the HotStuff replica.
Clone it and pass to `TorusApp::new()`.

Update `stub()` to generate a random signing key:
```rust
let signing_key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
```

**Verify:** `cargo check -p torus-consensus -p torus-node`
**Depends on:** none

### Task 5: produce_block — batch verify + attest
**Files:** `crates/torus-consensus/src/app.rs`

**Test first:** Integration test in `tests/consensus_test.rs`:
```rust
#[test]
fn produced_block_has_valid_attestation() {
    // Create TorusApp with signing key
    // Submit native actions to mempool
    // Call produce_block
    // Deserialize TorusBlock from response
    // Verify sig_attestation is non-zero and valid
}
```

**Implementation:**
In `produce_block`, after draining mempool and before serializing the block:

```rust
// Batch-verify native sigs, drop invalid
let invalid = batch_verify_native_actions(&native_actions, timestamp, |pubkey| {
    self.state_db.get_session(pubkey).ok().flatten()
});
if !invalid.is_empty() {
    // Remove invalid actions (iterate in reverse to preserve indices)
    for &idx in invalid.iter().rev() {
        native_actions.remove(idx);
    }
}

// Generate attestation
let sig_attestation = if let Some(ref key) = self.signing_key {
    generate_sig_attestation(&native_actions, key)
} else {
    [0u8; 64]
};
```

Set `header.sig_attestation = sig_attestation` on the TorusBlock.

Also change `drain_for_block(256, ...)` → `drain_for_block(4096, ...)`.

**Verify:** `cargo test -p torus-consensus`
**Depends on:** Tasks 1, 2, 3, 4

### Task 6: validate_block — check attestation only
**Files:** `crates/torus-consensus/src/app.rs`

**Test first:**
```rust
#[test]
fn validate_block_rejects_bad_attestation() {
    // Build a block with valid attestation
    // Tamper with attestation bytes
    // validate_block should return Invalid
}
```

**Implementation:**
Replace the rayon parallel sig verification loop with:

```rust
if !torus_block.native_actions.is_empty() {
    if torus_block.header.sig_attestation == [0u8; 64] {
        // Pre-upgrade block or RPC-produced: fall back to per-action verify
        use rayon::prelude::*;
        let all_valid = torus_block.native_actions.par_iter().enumerate().all(|(i, sa)| {
            if sa.recover_sender().is_err() {
                tracing::warn!(index = i, "validate_block: REJECTED -- invalid sig");
                return false;
            }
            true
        });
        if !all_valid {
            return ValidateBlockResponse::Invalid;
        }
    } else {
        // Attestation present: verify only the proposer's attestation sig
        let proposer_pubkey = /* get from block/view leader */;
        if !verify_sig_attestation(
            &torus_block.native_actions,
            &torus_block.header.sig_attestation,
            &proposer_pubkey,
        ) {
            tracing::warn!("validate_block: REJECTED -- invalid sig attestation");
            return ValidateBlockResponse::Invalid;
        }
    }
}
```

Need: resolve proposer's ed25519 `VerifyingKey` from the validator set. The
proposer is identified by `torus_block.header.proposer` (Address). The validator
set maps address → pubkey via `StakingManager::get_validator(addr)`.

**Verify:** `cargo test -p torus-consensus`
**Depends on:** Tasks 3, 5

### Task 7: Execution thread — re-verify + slash proposer
**Files:** `crates/torus-consensus/src/app.rs` (ExecutionContext)

**Test first:**
```rust
#[test]
fn execution_skips_invalid_sig_actions() {
    // Create block with one action that has a corrupted signature
    // but valid attestation (simulating malicious proposer)
    // Execute it through ExecutionContext
    // Verify: invalid action skipped, slash queued
}
```

**Implementation:**
In `ExecutionContext::execute_committed_block`, before the native execution section:

```rust
// Re-verify all signatures as safety net
let invalid_indices = batch_verify_native_actions(
    &torus_block.native_actions,
    torus_block.header.timestamp,
    |pubkey| self.state_db.get_session(pubkey).ok().flatten(),
);

let mut sender_actions = Vec::with_capacity(torus_block.native_actions.len());
let mut consumed_nonces = Vec::new();
for (i, signed) in torus_block.native_actions.iter().enumerate() {
    if invalid_indices.contains(&i) {
        tracing::error!(index = i, "INVALID SIG in attested block — skipping action");
        continue; // skip invalid
    }
    match signed.resolve_sender(torus_block.header.timestamp, |pubkey| {
        self.state_db.get_session(pubkey).ok().flatten()
    }) {
        Ok(sender) => {
            consumed_nonces.push((sender, signed.nonce));
            sender_actions.push((sender, signed.action.clone()));
        }
        Err(e) => {
            tracing::warn!(%e, "failed to recover native action sender, skipping");
        }
    }
}

if !invalid_indices.is_empty() {
    tracing::error!(
        count = invalid_indices.len(),
        proposer = %torus_block.header.proposer,
        "SLASHING PROPOSER: attested block contained invalid signatures"
    );
    // Queue 100% slash + tombstone for proposer
    // (push to a slash channel or directly call staking.slash)
}
```

**Verify:** `cargo test -p torus-consensus`
**Depends on:** Tasks 2, 5

### Task 8: Devnet integration test
**Files:** `crates/torus-consensus/src/app.rs` (cap change), devnet config

**Test first:** Manual devnet verification checklist:
- [ ] `docker compose build` succeeds
- [ ] Chain starts and produces blocks
- [ ] native-order-flood with 100 senders: actions land in blocks
- [ ] Block time stays <80ms under sustained load
- [ ] Blocks contain >256 native actions (cap raised)
- [ ] No consensus rejections
- [ ] Execution thread keeps up (no channel backpressure)
- [ ] Shutdown clean (no panics)

**Implementation:**
The `drain_for_block(4096, ...)` change is done in Task 5.
This task is rebuild + run + verify.

**Verify:** Manual devnet test
**Depends on:** Tasks 5, 6, 7

## Task Ordering

```
Task 1 (header field) ─────┐
                            ├── Task 3 (attestation helpers) ──┐
Task 4 (signing key) ──────┘                                   │
                                                                ├── Task 5 (produce_block)
Task 2 (batch verify) ─────────────────────────────────────────┤
                                                                ├── Task 6 (validate_block)
                                                                │
                                                                ├── Task 7 (exec re-verify)
                                                                │
                                                                └── Task 8 (devnet test)
```

Parallel starts: Tasks 1, 2, 4 (independent).
Then: Task 3 (needs 1). Then: Tasks 5, 6, 7 (need 2, 3, 4). Finally: Task 8.

## Migration / Rollback
- **Rolling upgrade**: `#[serde(default)]` on `sig_attestation` means old blocks
  deserialize with `[0u8; 64]`. validate_block falls back to per-action verify
  when attestation is zero. No coordinated upgrade needed.
- **Rollback**: revert to per-action verification. The attestation field is ignored
  if zero. Remove the attestation check from validate_block, restore rayon loop.
- **Client migration**: encourage `CreateSession` → ed25519 signing for trading.
  EIP-712 keeps working, just slower (proposer verifies individually).
