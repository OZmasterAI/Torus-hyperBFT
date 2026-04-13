//! Double-sign detection and downtime tracking (Phase 3: 3.1.1, 3.1.2).
//!
//! Builds detection *around* hotstuff_rs — does not modify the consensus library.
//!
//! - [`DoubleSignDetector`]: sliding-window tracker for conflicting PhaseVotes
//! - [`DowntimeTracker`]: per-block signing participation tracker

use std::collections::{HashMap, HashSet, VecDeque};

use ed25519_dalek::{Signature as EdSignature, Verifier, VerifyingKey};

// ============================================================================
// Double-Sign Detection (3.1.1)
// ============================================================================

/// A phase vote as observed from hotstuff_rs consensus messages.
/// This is our own representation — we do not modify hotstuff_rs internals.
#[derive(Clone, Debug)]
pub struct ObservedPhaseVote {
    pub chain_id: u64,
    pub view: u64,
    pub block_hash: [u8; 32],
    /// 0 = Prepare, 1 = PreCommit, 2 = Commit
    pub phase: u8,
    /// Ed25519 public key of the signer (32 bytes)
    pub signer: [u8; 32],
    /// Ed25519 signature (64 bytes)
    pub signature: [u8; 64],
}

impl ObservedPhaseVote {
    /// Build the canonical signed message: chain_id(8) || view(8) || block_hash(32) || phase(1).
    pub fn signed_message(&self) -> Vec<u8> {
        build_vote_message(self.chain_id, self.view, &self.block_hash, self.phase)
    }
}

/// One side of a double-sign evidence pair.
#[derive(Clone, Debug)]
pub struct SignedVoteData {
    pub block_hash: [u8; 32],
    pub signature: [u8; 64],
}

/// Cryptographically verifiable evidence of a double-sign.
///
/// Contains two conflicting votes at the same (chain_id, view, phase) by the
/// same signer, each with a different block hash. Anyone with the evidence can
/// verify the double-sign independently using only the public key and signatures.
#[derive(Clone, Debug)]
pub struct DoubleSignEvidence {
    pub chain_id: u64,
    pub view: u64,
    pub phase: u8,
    pub signer: [u8; 32],
    pub vote_a: SignedVoteData,
    pub vote_b: SignedVoteData,
}

impl DoubleSignEvidence {
    /// Independently verify this evidence without any external state.
    ///
    /// Checks:
    /// 1. The two block hashes differ (it *is* a conflict)
    /// 2. Both signatures are valid for the signer's public key
    /// 3. Both signatures cover the correct canonical message
    pub fn verify(&self) -> bool {
        // Must be different block hashes
        if self.vote_a.block_hash == self.vote_b.block_hash {
            return false;
        }

        let vk = match VerifyingKey::from_bytes(&self.signer) {
            Ok(vk) => vk,
            Err(_) => return false,
        };

        // Verify signature A
        let msg_a =
            build_vote_message(self.chain_id, self.view, &self.vote_a.block_hash, self.phase);
        let sig_a = EdSignature::from_bytes(&self.vote_a.signature);
        if vk.verify(&msg_a, &sig_a).is_err() {
            return false;
        }

        // Verify signature B
        let msg_b =
            build_vote_message(self.chain_id, self.view, &self.vote_b.block_hash, self.phase);
        let sig_b = EdSignature::from_bytes(&self.vote_b.signature);
        if vk.verify(&msg_b, &sig_b).is_err() {
            return false;
        }

        true
    }
}

/// Build the canonical vote message: chain_id(8) || view(8) || block_hash(32) || phase(1).
fn build_vote_message(chain_id: u64, view: u64, block_hash: &[u8; 32], phase: u8) -> Vec<u8> {
    let mut msg = Vec::with_capacity(49);
    msg.extend_from_slice(&chain_id.to_be_bytes());
    msg.extend_from_slice(&view.to_be_bytes());
    msg.extend_from_slice(block_hash);
    msg.push(phase);
    msg
}

/// Vote identity key: (chain_id, view, phase, signer).
type VoteKey = (u64, u64, u8, [u8; 32]);
/// Stored vote data: (block_hash, signature).
type VoteData = ([u8; 32], [u8; 64]);

/// Detects double-signing within a sliding window of recent views.
///
/// Maintains a map of recently seen votes. If the same validator submits two
/// different block hashes at the same (chain_id, view, phase), evidence is returned.
/// Votes outside the window are pruned to bound memory.
pub struct DoubleSignDetector {
    votes: HashMap<VoteKey, VoteData>,
    views_in_window: VecDeque<u64>,
    chain_id: u64,
    window_size: u64,
    min_view: u64,
}

impl DoubleSignDetector {
    pub fn new(chain_id: u64, window_size: u64) -> Self {
        Self {
            votes: HashMap::new(),
            views_in_window: VecDeque::new(),
            chain_id,
            window_size,
            min_view: 0,
        }
    }

    /// Record an observed phase vote. Returns evidence if a double-sign is detected.
    pub fn record_vote(&mut self, vote: &ObservedPhaseVote) -> Option<DoubleSignEvidence> {
        if vote.chain_id != self.chain_id {
            return None;
        }

        self.prune(vote.view);

        let key = (vote.chain_id, vote.view, vote.phase, vote.signer);

        if let Some((existing_hash, existing_sig)) = self.votes.get(&key) {
            if *existing_hash != vote.block_hash {
                // DOUBLE SIGN DETECTED
                return Some(DoubleSignEvidence {
                    chain_id: vote.chain_id,
                    view: vote.view,
                    phase: vote.phase,
                    signer: vote.signer,
                    vote_a: SignedVoteData {
                        block_hash: *existing_hash,
                        signature: *existing_sig,
                    },
                    vote_b: SignedVoteData {
                        block_hash: vote.block_hash,
                        signature: vote.signature,
                    },
                });
            }
            // Same block hash = duplicate/replay, not a double-sign
            return None;
        }

        // New vote — store it
        self.votes.insert(key, (vote.block_hash, vote.signature));

        if self.views_in_window.back() != Some(&vote.view) {
            self.views_in_window.push_back(vote.view);
        }

        None
    }

    /// Prune votes older than the sliding window.
    fn prune(&mut self, current_view: u64) {
        let cutoff = current_view.saturating_sub(self.window_size);
        if cutoff <= self.min_view {
            return;
        }

        self.votes.retain(|&(_, v, _, _), _| v >= cutoff);

        while let Some(&oldest) = self.views_in_window.front() {
            if oldest < cutoff {
                self.views_in_window.pop_front();
            } else {
                break;
            }
        }

        self.min_view = cutoff;
    }

    /// Number of votes currently tracked.
    pub fn tracked_vote_count(&self) -> usize {
        self.votes.len()
    }
}

// ============================================================================
// Downtime Tracking (3.1.2)
// ============================================================================

/// Tracks validator participation (signing) across recent blocks for downtime detection.
///
/// At epoch boundaries, call [`detect_downtime`] to find validators that have
/// signed fewer than the threshold percentage of blocks in the window.
pub struct DowntimeTracker {
    /// Block height -> set of signer pubkeys that participated.
    block_signers: VecDeque<(u64, HashSet<[u8; 32]>)>,
    window_size: u64,
}

impl DowntimeTracker {
    pub fn new(window_size: u64) -> Self {
        Self {
            block_signers: VecDeque::new(),
            window_size,
        }
    }

    /// Record that a validator signed for a given block height.
    pub fn record_signer(&mut self, block_height: u64, signer: [u8; 32]) {
        if let Some(entry) = self.block_signers.back_mut() {
            if entry.0 == block_height {
                entry.1.insert(signer);
                return;
            }
        }

        let mut signers = HashSet::new();
        signers.insert(signer);
        self.block_signers.push_back((block_height, signers));

        // Prune old blocks
        let cutoff = block_height.saturating_sub(self.window_size);
        while let Some(&(h, _)) = self.block_signers.front() {
            if h < cutoff {
                self.block_signers.pop_front();
            } else {
                break;
            }
        }
    }

    /// Check which validators from the given set are below the signing threshold.
    /// Returns pubkeys of validators that signed fewer than `threshold_pct`% of blocks.
    pub fn detect_downtime(
        &self,
        validators: &[[u8; 32]],
        threshold_pct: u64,
    ) -> Vec<[u8; 32]> {
        let total_blocks = self.block_signers.len() as u64;
        if total_blocks == 0 {
            return vec![];
        }

        let threshold = total_blocks * threshold_pct / 100;

        validators
            .iter()
            .filter(|pubkey| {
                let signed_count = self
                    .block_signers
                    .iter()
                    .filter(|(_, signers)| signers.contains(*pubkey))
                    .count() as u64;
                signed_count < threshold
            })
            .copied()
            .collect()
    }

    /// Number of blocks currently in the window.
    pub fn window_block_count(&self) -> usize {
        self.block_signers.len()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn make_keypair(seed: u8) -> (SigningKey, VerifyingKey) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn sign_vote(
        sk: &SigningKey,
        chain_id: u64,
        view: u64,
        block_hash: [u8; 32],
        phase: u8,
    ) -> ObservedPhaseVote {
        use ed25519_dalek::Signer;
        let msg = build_vote_message(chain_id, view, &block_hash, phase);
        let sig = sk.sign(&msg);
        ObservedPhaseVote {
            chain_id,
            view,
            block_hash,
            phase,
            signer: sk.verifying_key().to_bytes(),
            signature: sig.to_bytes(),
        }
    }

    // ---- Double-Sign Detection ----

    #[test]
    fn detect_double_sign() {
        let (sk, _vk) = make_keypair(1);
        let mut detector = DoubleSignDetector::new(7777, 100);

        let vote_a = sign_vote(&sk, 7777, 10, [0xAA; 32], 0);
        let vote_b = sign_vote(&sk, 7777, 10, [0xBB; 32], 0);

        assert!(detector.record_vote(&vote_a).is_none());

        let evidence = detector.record_vote(&vote_b);
        assert!(evidence.is_some());

        let ev = evidence.unwrap();
        assert_eq!(ev.view, 10);
        assert_eq!(ev.signer, sk.verifying_key().to_bytes());
        assert!(ev.verify());
    }

    #[test]
    fn same_vote_no_false_positive() {
        let (sk, _vk) = make_keypair(2);
        let mut detector = DoubleSignDetector::new(7777, 100);

        let vote = sign_vote(&sk, 7777, 10, [0xAA; 32], 0);

        assert!(detector.record_vote(&vote).is_none());
        assert!(detector.record_vote(&vote).is_none());
    }

    #[test]
    fn different_view_no_false_positive() {
        let (sk, _vk) = make_keypair(3);
        let mut detector = DoubleSignDetector::new(7777, 100);

        let vote_a = sign_vote(&sk, 7777, 10, [0xAA; 32], 0);
        let vote_b = sign_vote(&sk, 7777, 11, [0xBB; 32], 0);

        assert!(detector.record_vote(&vote_a).is_none());
        assert!(detector.record_vote(&vote_b).is_none());
    }

    #[test]
    fn old_votes_pruned() {
        let (sk, _vk) = make_keypair(4);
        let mut detector = DoubleSignDetector::new(7777, 100);

        let vote_old = sign_vote(&sk, 7777, 10, [0xAA; 32], 0);
        assert!(detector.record_vote(&vote_old).is_none());

        let vote_new = sign_vote(&sk, 7777, 200, [0xCC; 32], 0);
        assert!(detector.record_vote(&vote_new).is_none());

        // Old view pruned — conflicting vote won't be detected
        let vote_conflict = sign_vote(&sk, 7777, 10, [0xBB; 32], 0);
        assert!(detector.record_vote(&vote_conflict).is_none());

        assert!(detector.tracked_vote_count() <= 2);
    }

    #[test]
    fn evidence_with_invalid_signer_fails_verification() {
        let evidence = DoubleSignEvidence {
            chain_id: 7777,
            view: 10,
            phase: 0,
            signer: [0xFF; 32],
            vote_a: SignedVoteData {
                block_hash: [0xAA; 32],
                signature: [0; 64],
            },
            vote_b: SignedVoteData {
                block_hash: [0xBB; 32],
                signature: [0; 64],
            },
        };
        assert!(!evidence.verify());
    }

    #[test]
    fn evidence_same_block_hash_fails_verification() {
        let (sk, _) = make_keypair(5);
        use ed25519_dalek::Signer;
        let msg = build_vote_message(7777, 10, &[0xAA; 32], 0);
        let sig = sk.sign(&msg);

        let evidence = DoubleSignEvidence {
            chain_id: 7777,
            view: 10,
            phase: 0,
            signer: sk.verifying_key().to_bytes(),
            vote_a: SignedVoteData {
                block_hash: [0xAA; 32],
                signature: sig.to_bytes(),
            },
            vote_b: SignedVoteData {
                block_hash: [0xAA; 32],
                signature: sig.to_bytes(),
            },
        };
        assert!(!evidence.verify());
    }

    // ---- Downtime Tracking ----

    #[test]
    fn downtime_detection_below_threshold() {
        let mut tracker = DowntimeTracker::new(1000);

        let signer_a = [1u8; 32];
        let signer_b = [2u8; 32];

        for h in 1..=100 {
            tracker.record_signer(h, signer_a);
        }

        let down = tracker.detect_downtime(&[signer_a, signer_b], 50);
        assert!(down.contains(&signer_b));
        assert!(!down.contains(&signer_a));
    }

    #[test]
    fn downtime_tracker_prunes_old_blocks() {
        let mut tracker = DowntimeTracker::new(10);

        for h in 1..=20 {
            tracker.record_signer(h, [1u8; 32]);
        }

        assert!(tracker.window_block_count() <= 11);
    }

    #[test]
    fn downtime_empty_window_returns_empty() {
        let tracker = DowntimeTracker::new(1000);
        let down = tracker.detect_downtime(&[[1u8; 32]], 50);
        assert!(down.is_empty());
    }

    #[test]
    fn downtime_partial_signing() {
        let mut tracker = DowntimeTracker::new(1000);
        let signer = [3u8; 32];

        for h in 1..=100 {
            if h <= 40 {
                tracker.record_signer(h, signer);
            } else {
                tracker.record_signer(h, [99u8; 32]);
            }
        }

        let down = tracker.detect_downtime(&[signer], 50);
        assert!(down.contains(&signer));
    }
}
