# Abstraction Audit

This document lists every simplification made in the safety arguments
relative to the actual implementation, and argues why each simplification
does not hide a safety bug.

## 1. Message ordering is unspecified

**Abstraction**: The safety arguments assume messages can arrive in any
order (asynchronous model). No ordering guarantees.

**Reality**: The implementation uses TCP-based networking which provides
FIFO ordering per connection, and a message cache (`Cacheable`) that
buffers out-of-order messages.

**Safety impact**: None. The safety argument is STRONGER with unordered
messages. Any attack possible under ordered delivery is also possible
under unordered delivery. The implementation's ordering is a liveness
optimization, not a safety assumption.

## 2. Signature verification abstracted to "correct/incorrect"

**Abstraction**: We treat signatures as either valid (from the claimed
signer) or invalid (forgery). We don't model the cryptographic details
of Ed25519.

**Reality**: The implementation uses `ed25519_dalek` with `VerifyingKey`
for signature verification. Each signed message is verified against the
signer's public key before processing.

**Safety impact**: None, assuming Ed25519 is secure (standard assumption).
The `is_correct(signer)` method on `PhaseVote` and `TimeoutVote` verifies
the signature against the signer's key. If Ed25519 is broken, all BFT
protocols fail regardless.

**Code anchors**:
- `types.rs:149-188` (PhaseCertificate::is_correctly_signed)
- `pacemaker/types.rs:80-116` (TimeoutCertificate::is_correctly_signed)
- `types.rs:444-471` (NEC signature verification)

## 3. Quorum modeled as "2f+1 out of n"

**Abstraction**: We assume quorum = 2f+1 = n-f validators.

**Reality**: The implementation uses weighted voting with `TotalPower`
and `Power` types. Quorum is `ceil(2/3 * total_power) + 1` (or similar).
Different validators can have different powers.

**Safety impact**: Minimal. The weighted quorum satisfies the same
intersection property: any two quorums of power >= 2/3 * total_power
overlap by at least 1/3 * total_power, which includes at least one
honest validator (assuming Byzantine power <= f = 1/3 * total_power).

**Code anchor**: `validator_set.rs` — `ValidatorSet::quorum()` method
computes the quorum threshold.

## 4. Block tree modeled as append-only

**Abstraction**: Once a block is inserted, it stays in the tree forever.

**Reality**: The implementation has pruning logic that can remove old
blocks. Committed blocks below a certain height may be pruned.

**Safety impact**: Pruning only removes blocks that are already committed
(finalized). The safety arguments concern whether the CORRECT blocks are
committed, not whether old committed blocks are retained. Pruning after
commitment is a storage optimization.

## 5. Network partitions modeled as message drops

**Abstraction**: Byzantine behavior includes message withholding.
Network partitions are modeled as the Byzantine adversary choosing to
not deliver messages.

**Reality**: Network partitions are not adversarial — they're infrastructure
failures. But the safety argument treats them equivalently: a message not
delivered is a message not delivered, regardless of cause.

**Safety impact**: None. The safety argument doesn't depend on message
delivery timing (only on eventual delivery for liveness).

## 6. View change timing abstracted away

**Abstraction**: Views simply "time out" and validators move to the next
view. We don't model the exact timing.

**Reality**: The implementation has `max_view_time` configurations, epoch
boundaries, timeout extensions, and the Bracha amplification mechanism
for timeout vote dissemination.

**Safety impact**: None for safety. Timing affects only liveness. The
safety arguments hold regardless of when views change, because:
- `safe_pc` doesn't depend on timing
- Locking is based on view numbers, not wall-clock time
- Commit rules are based on consecutive views, not timing

The one concern: if a validator's clock is wrong and it enters views too
early/late, it might miss proposals. But this is a liveness issue.

## 7. Validator set changes simplified

**Abstraction**: The safety arguments mostly assume a fixed validator set.
Properties 1-4 are stated for a fixed set.

**Reality**: The implementation supports validator set updates via the
phased mode (Prepare/Precommit/Commit/Decide phases). During transitions,
two validator sets may be active simultaneously.

**Safety impact**: Moderate. Validator set transitions are the most
complex part of the protocol. The key safety mechanism is:
- A validator set update must be committed (via Decide PC) before the
  new set takes effect
- During the transition, PCs are validated against BOTH the old and new
  sets (`is_correct` in `types.rs:48-145`)
- The phased mode ensures the transition is committed before the new set
  proposes blocks

The safety arguments for the fixed-set case extend to the dynamic-set
case through the "immediacy" property: the old set remains active until
the update is decided, preventing a gap in safety guarantees.

**Code anchor**: `types.rs:48-145` (PhaseCertificate::is_correct with
validator set state handling)

## 8. Block content abstracted away

**Abstraction**: We treat blocks as opaque containers with a hash.
Block content (transactions, state updates) is not modeled.

**Reality**: Blocks contain application data, app state updates, and
possibly validator set updates. The app validates block content via
`validate_block()`.

**Safety impact**: None for consensus safety. The consensus protocol
guarantees agreement on the SEQUENCE of blocks. Whether the content
of those blocks is valid is an application-layer concern enforced by
`App::validate_block()`. A block that passes consensus may still be
application-invalid, but this is outside the scope of BFT consensus
safety.

## 9. Recovery protocol modeled as synchronous

**Abstraction**: In Trace 3 (NEC recovery), we describe the recovery
as a sequence of steps. In reality, it's asynchronous.

**Reality**: The recovery is state-machine based (`RecoveryState` enum).
The leader sends requests, stores state, and completes the proposal
when responses arrive — all within the same view's timeout.

**Safety impact**: None. The async recovery doesn't affect safety:
- If recovery completes: the resulting proposal is validated normally
- If recovery doesn't complete: the view times out, and the next leader
  tries again
- The recovery state is cleared on view change (`enter_view` resets
  `recovery_state` to `None` at `implementation.rs:182`)

## 10. Leader reputation simplified

**Abstraction**: We treat leader selection as deterministic given the
validator set and view number.

**Reality**: MonadBFT B3 adds reputation-weighted leader selection via
`select_leader_with_reputation()` which adjusts each validator's
effective power based on their track record.

**Safety impact**: None. Leader selection affects liveness (a bad leader
causes timeouts), not safety. The safety arguments depend on:
- Honest validators following `safe_pc`/`safe_block` (regardless of who
  proposes)
- Quorum intersection (independent of leader selection)
- Vote-once-per-view (independent of leader selection)

The reputation system is deterministic: same inputs produce the same
leader on all honest nodes. This ensures consistency.

**Code anchor**: `pacemaker/implementation.rs:780-801`
(`select_leader_with_reputation`)
