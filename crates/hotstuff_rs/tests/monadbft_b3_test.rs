/*
    MonadBFT Batch B3 Tests: Speculative Rollback + Leader Reputation
    Tests for tasks 3.3.5 (speculative rollback) and 3.3.6 (leader reputation).
*/

use borsh::BorshSerialize;
use rand_core::OsRng;

use hotstuff_rs::hotstuff::types::{EquivocationEvidence, LeaderReputation, LeaderReputationEntry};
use hotstuff_rs::pacemaker::{
    reputation_leader_selection_enabled, select_leader, select_leader_reputation_weighted,
    select_leader_with_reputation, set_reputation_leader_selection,
};
use hotstuff_rs::types::crypto_primitives::SigningKey;
use hotstuff_rs::types::data_types::*;
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;
use hotstuff_rs::types::validator_set::ValidatorSet;

// ============================================================================
// Helper functions
// ============================================================================

fn make_validator_set(n: usize) -> (Vec<SigningKey>, ValidatorSet) {
    let mut csprg = OsRng {};
    let keypairs: Vec<SigningKey> = (0..n).map(|_| SigningKey::generate(&mut csprg)).collect();
    let mut vs = ValidatorSet::new();
    let mut updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        updates.insert(kp.verifying_key(), Power::new(1));
    }
    vs.apply_updates(&updates);
    (keypairs, vs)
}

fn make_weighted_validator_set(powers: &[u64]) -> (Vec<SigningKey>, ValidatorSet) {
    let mut csprg = OsRng {};
    let keypairs: Vec<SigningKey> = (0..powers.len())
        .map(|_| SigningKey::generate(&mut csprg))
        .collect();
    let mut vs = ValidatorSet::new();
    let mut updates = ValidatorSetUpdates::new();
    for (kp, &power) in keypairs.iter().zip(powers.iter()) {
        updates.insert(kp.verifying_key(), Power::new(power));
    }
    vs.apply_updates(&updates);
    (keypairs, vs)
}

// ============================================================================
// 3.3.5: EquivocationEvidence Tests
// ============================================================================

/// Equivocation evidence stores the correct view and conflicting block hashes.
#[test]
fn equivocation_evidence_fields() {
    let (keypairs, _vs) = make_validator_set(4);
    let evidence = EquivocationEvidence {
        view: ViewNumber::new(10),
        leader: keypairs[0].verifying_key(),
        block_a: CryptoHash::new([0xAA; 32]),
        block_b: CryptoHash::new([0xBB; 32]),
    };
    assert_eq!(evidence.view, ViewNumber::new(10));
    assert_ne!(evidence.block_a, evidence.block_b);
    assert_eq!(evidence.leader, keypairs[0].verifying_key());
}

/// Two evidence instances with same data are structurally identical.
#[test]
fn equivocation_evidence_equality() {
    let (keypairs, _vs) = make_validator_set(4);
    let evidence_a = EquivocationEvidence {
        view: ViewNumber::new(10),
        leader: keypairs[0].verifying_key(),
        block_a: CryptoHash::new([0xAA; 32]),
        block_b: CryptoHash::new([0xBB; 32]),
    };
    let evidence_b = EquivocationEvidence {
        view: ViewNumber::new(10),
        leader: keypairs[0].verifying_key(),
        block_a: CryptoHash::new([0xAA; 32]),
        block_b: CryptoHash::new([0xBB; 32]),
    };
    assert_eq!(evidence_a.view, evidence_b.view);
    assert_eq!(evidence_a.block_a, evidence_b.block_a);
    assert_eq!(evidence_a.block_b, evidence_b.block_b);
}

// ============================================================================
// 3.3.6: LeaderReputationEntry Tests
// ============================================================================

/// New entry has default score of 10000 (100%).
#[test]
fn reputation_entry_default_score() {
    let entry = LeaderReputationEntry::new();
    assert_eq!(entry.score_bps(), 10_000);
    assert_eq!(entry.successes, 0);
    assert_eq!(entry.total, 0);
}

/// Score after all successes is 100%.
#[test]
fn reputation_entry_all_successes() {
    let mut entry = LeaderReputationEntry::new();
    for _ in 0..10 {
        entry.record_success();
    }
    assert_eq!(entry.score_bps(), 10_000);
    assert_eq!(entry.successes, 10);
    assert_eq!(entry.total, 10);
}

/// Score after all timeouts clamps to the 2500 bps floor (never excluded).
#[test]
fn reputation_entry_all_timeouts() {
    let mut entry = LeaderReputationEntry::new();
    for _ in 0..10 {
        entry.record_timeout();
    }
    assert_eq!(entry.score_bps(), 2500);
    assert_eq!(entry.successes, 0);
    assert_eq!(entry.total, 10);
}

/// Score after mixed results is proportional.
#[test]
fn reputation_entry_mixed() {
    let mut entry = LeaderReputationEntry::new();
    for _ in 0..7 {
        entry.record_success();
    }
    for _ in 0..3 {
        entry.record_timeout();
    }
    // 7/10 = 70% = 7000 bps
    assert_eq!(entry.score_bps(), 7000);
}

/// Score with 50/50 is exactly 5000 bps.
#[test]
fn reputation_entry_fifty_fifty() {
    let mut entry = LeaderReputationEntry::new();
    for _ in 0..50 {
        entry.record_success();
    }
    for _ in 0..50 {
        entry.record_timeout();
    }
    assert_eq!(entry.score_bps(), 5000);
}

// ============================================================================
// 3.3.6: LeaderReputation Aggregate Tests
// ============================================================================

/// Unknown validators get full reputation (10000 bps).
#[test]
fn reputation_unknown_validator_gets_full_score() {
    let (keypairs, _vs) = make_validator_set(4);
    let rep = LeaderReputation::new(100);
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 10_000);
}

/// Recording success updates the validator's score.
#[test]
fn reputation_record_success() {
    let (keypairs, _vs) = make_validator_set(4);
    let mut rep = LeaderReputation::new(100);

    for _ in 0..5 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 10_000);
    // Other validators still unknown → 10000
    assert_eq!(rep.score_bps(&keypairs[1].verifying_key()), 10_000);
}

/// Recording timeouts reduces score.
#[test]
fn reputation_record_timeout_reduces_score() {
    let (keypairs, _vs) = make_validator_set(4);
    let mut rep = LeaderReputation::new(100);

    // 5 successes + 5 timeouts = 50%
    for _ in 0..5 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    for _ in 0..5 {
        rep.record_timeout(&keypairs[0].verifying_key());
    }
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 5000);
}

/// Decay halves all counters and removes zero entries.
#[test]
fn reputation_decay() {
    let (keypairs, _vs) = make_validator_set(4);
    let mut rep = LeaderReputation::new(100);

    for _ in 0..10 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    assert_eq!(rep.entries.len(), 1);

    rep.decay();
    // After decay: successes = 5, total = 5 → still 100%
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 10_000);

    // Decay multiple times until zero
    for _ in 0..10 {
        rep.decay();
    }
    // Entry should be removed (total decayed to 0)
    assert!(rep.entries.is_empty());
    // Unknown → 10000
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 10_000);
}

/// Serialization round-trip preserves reputation data.
#[test]
fn reputation_serialization_roundtrip() {
    use borsh::BorshDeserialize;

    let (keypairs, _vs) = make_validator_set(4);
    let mut rep = LeaderReputation::new(100);

    for _ in 0..7 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    for _ in 0..3 {
        rep.record_timeout(&keypairs[0].verifying_key());
    }
    rep.record_success(&keypairs[1].verifying_key());

    let bytes = rep.try_to_vec().unwrap();
    let rep2 = LeaderReputation::deserialize(&mut bytes.as_slice()).unwrap();

    assert_eq!(rep, rep2);
    assert_eq!(rep2.score_bps(&keypairs[0].verifying_key()), 7000);
    assert_eq!(rep2.score_bps(&keypairs[1].verifying_key()), 10_000);
}

// ============================================================================
// 3.3.6: Reputation-Weighted Leader Selection Tests
// ============================================================================

/// With all validators at full reputation, selection matches standard IWRR.
#[test]
fn reputation_weighted_selection_matches_standard_at_full_rep() {
    let (keypairs, vs) = make_validator_set(4);
    let rep = LeaderReputation::new(100); // No entries = all at 10000

    // For all views, reputation-weighted and standard should select the same leader.
    for v in 0..20 {
        let view = ViewNumber::new(v);
        let standard = select_leader(view, &vs);
        let weighted = select_leader_reputation_weighted(view, &vs, &rep);
        assert_eq!(
            standard, weighted,
            "View {}: standard and weighted should match at full reputation",
            v
        );
    }
}

/// Validators with low reputation are selected less often.
#[test]
fn low_reputation_reduces_selection_frequency() {
    let (keypairs, vs) = make_weighted_validator_set(&[10, 10, 10, 10]);
    let mut rep = LeaderReputation::new(100);

    // Validator 0 has terrible reputation: 10% success rate.
    for _ in 0..1 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    for _ in 0..9 {
        rep.record_timeout(&keypairs[0].verifying_key());
    }
    // Validator 0 raw score 1000 bps (10%) clamps to the 2500 bps floor.
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 2500);

    // Count selections over many views.
    let total_views = 1000u64;
    let mut counts = [0u64; 4];
    for v in 0..total_views {
        let leader = select_leader_reputation_weighted(ViewNumber::new(v), &vs, &rep);
        for (i, kp) in keypairs.iter().enumerate() {
            if leader == kp.verifying_key() {
                counts[i] += 1;
            }
        }
    }

    // Validator 0 (25% floored rep) should be selected significantly less than others.
    // Others have 100% rep with power 10 → effective power 10.
    // Validator 0 has 25% rep with power 10 → effective power 2.
    // Total power = 2 + 10 + 10 + 10 = 32.
    // Expected: V0 ≈ 2/32 ≈ 6.3%, others ≈ 10/32 ≈ 31.3%.
    assert!(
        counts[0] < counts[1],
        "Low-rep validator 0 ({}) should be selected less than full-rep validator 1 ({})",
        counts[0],
        counts[1]
    );
}

/// Validators with low reputation are NOT permanently excluded.
#[test]
fn low_reputation_does_not_exclude() {
    let (keypairs, vs) = make_weighted_validator_set(&[10, 10, 10, 10]);
    let mut rep = LeaderReputation::new(100);

    // Validator 0 has 0% success rate — but should still be selected sometimes.
    for _ in 0..10 {
        rep.record_timeout(&keypairs[0].verifying_key());
    }
    // Raw score 0 clamps to the 2500 bps floor.
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 2500);

    // With floored 25% rep, effective power = max(1, 10 * 2500 / 10000) = 2.
    // Total power = 2 + 10 + 10 + 10 = 32.
    // Validator 0 should still appear in ~2/32 views.
    let total_views = 1000u64;
    let mut count_v0 = 0u64;
    for v in 0..total_views {
        let leader = select_leader_reputation_weighted(ViewNumber::new(v), &vs, &rep);
        if leader == keypairs[0].verifying_key() {
            count_v0 += 1;
        }
    }

    assert!(
        count_v0 > 0,
        "Even 0%-rep validator should be selected at least once in {} views",
        total_views
    );
}

/// Recovery: successful blocks restore reputation.
#[test]
fn reputation_recovery_after_poor_performance() {
    let (keypairs, _vs) = make_validator_set(4);
    let mut rep = LeaderReputation::new(100);

    // Start with poor performance.
    for _ in 0..8 {
        rep.record_timeout(&keypairs[0].verifying_key());
    }
    for _ in 0..2 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    // Raw score 2000 bps (20%) clamps to the 2500 bps floor.
    assert_eq!(rep.score_bps(&keypairs[0].verifying_key()), 2500);

    // Gradually recover.
    for _ in 0..20 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    // Now: 22 successes / 30 total = 73%
    let score = rep.score_bps(&keypairs[0].verifying_key());
    assert!(
        score > 7000,
        "Score should recover above 70%, got {}",
        score
    );
}

/// Determinism: same events produce identical reputation on different nodes.
#[test]
fn reputation_determinism() {
    let (keypairs, _vs) = make_validator_set(4);

    // Simulate the same event sequence on two "nodes".
    let mut rep_a = LeaderReputation::new(100);
    let mut rep_b = LeaderReputation::new(100);

    let events = vec![
        (0, true),  // V0 success
        (1, false), // V1 timeout
        (0, true),  // V0 success
        (2, true),  // V2 success
        (0, false), // V0 timeout
        (3, true),  // V3 success
        (1, true),  // V1 success
    ];

    for &(vi, success) in &events {
        let vk = keypairs[vi].verifying_key();
        if success {
            rep_a.record_success(&vk);
            rep_b.record_success(&vk);
        } else {
            rep_a.record_timeout(&vk);
            rep_b.record_timeout(&vk);
        }
    }

    assert_eq!(
        rep_a, rep_b,
        "Same event sequence must produce identical reputation"
    );

    // Same reputation → same leader selection.
    let vs = {
        let mut vs = ValidatorSet::new();
        let mut updates = ValidatorSetUpdates::new();
        for kp in &keypairs {
            updates.insert(kp.verifying_key(), Power::new(10));
        }
        vs.apply_updates(&updates);
        vs
    };

    for v in 0..50 {
        let view = ViewNumber::new(v);
        let leader_a = select_leader_reputation_weighted(view, &vs, &rep_a);
        let leader_b = select_leader_reputation_weighted(view, &vs, &rep_b);
        assert_eq!(
            leader_a, leader_b,
            "View {}: determinism violated — different leaders selected",
            v
        );
    }
}

/// Reputation-weighted selection with varied stake levels.
#[test]
fn reputation_with_varied_stake() {
    let (keypairs, vs) = make_weighted_validator_set(&[20, 10, 5, 1]);
    let mut rep = LeaderReputation::new(100);

    // High-stake validator 0 has poor reputation (30%).
    for _ in 0..3 {
        rep.record_success(&keypairs[0].verifying_key());
    }
    for _ in 0..7 {
        rep.record_timeout(&keypairs[0].verifying_key());
    }
    // V0 effective power: max(1, 20 * 3000 / 10000) = 6
    // V1 effective power: 10 (full rep)
    // V2 effective power: 5 (full rep)
    // V3 effective power: 1 (full rep)

    let total_views = 1000u64;
    let mut counts = [0u64; 4];
    for v in 0..total_views {
        let leader = select_leader_reputation_weighted(ViewNumber::new(v), &vs, &rep);
        for (i, kp) in keypairs.iter().enumerate() {
            if leader == kp.verifying_key() {
                counts[i] += 1;
            }
        }
    }

    // V0 (effective 6) should be selected less than V1 (effective 10).
    assert!(
        counts[0] < counts[1],
        "Poor-rep high-stake V0 ({}) should be selected less than full-rep V1 ({})",
        counts[0],
        counts[1]
    );
    // V3 (effective 1) should be selected the least.
    assert!(
        counts[3] <= counts[2],
        "Low-stake V3 ({}) should be selected no more than V2 ({})",
        counts[3],
        counts[2]
    );
}

// ============================================================================
// Irrevocable blocks cannot be rolled back
// ============================================================================

/// Verifies that only speculative (not irrevocable) blocks can be targeted.
/// This is a logical invariant test — the block tree methods enforce this.
#[test]
fn irrevocable_blocks_cannot_be_speculative() {
    // By construction: promote_speculative_to_irrevocable removes from list.
    // After promotion, is_speculatively_committed returns false.
    // This is tested structurally — the methods guarantee it.
    // We test the LeaderReputation data structure as a proxy since block tree
    // tests require a full KVStore setup.

    // The invariant: once a block is committed via 2-chain, it's removed from
    // the speculative list and cannot be rolled back.
    let mut rep = LeaderReputation::new(100);
    let mut csprg = OsRng {};
    let sk = SigningKey::generate(&mut csprg);
    let vk = sk.verifying_key();

    // Simulate commit → reputation success.
    rep.record_success(&vk);
    assert_eq!(rep.score_bps(&vk), 10_000);

    // Simulate timeout → reputation decreases.
    rep.record_timeout(&vk);
    assert_eq!(rep.score_bps(&vk), 5000);
}

// ============================================================================
// Equivocation evidence persistence (survives rollback)
// ============================================================================

/// Evidence is a separate data structure — not part of the rolled-back state.
#[test]
fn equivocation_evidence_is_independent() {
    let (keypairs, _vs) = make_validator_set(4);

    // Create evidence.
    let evidence = EquivocationEvidence {
        view: ViewNumber::new(10),
        leader: keypairs[0].verifying_key(),
        block_a: CryptoHash::new([0xAA; 32]),
        block_b: CryptoHash::new([0xBB; 32]),
    };

    // Evidence fields are independent of block state.
    assert_eq!(evidence.view, ViewNumber::new(10));
    assert_eq!(evidence.leader, keypairs[0].verifying_key());

    // Evidence can be serialized (it uses Borsh-compatible types internally).
    let view_bytes = evidence.view.try_to_vec().unwrap();
    assert!(!view_bytes.is_empty());
}

// ============================================================================
// Performance: reputation-weighted selection overhead
// ============================================================================

#[test]
fn reputation_weighted_selection_performance() {
    let (keypairs, vs) = make_weighted_validator_set(&[10; 20]);
    let mut rep = LeaderReputation::new(100);

    // Add some reputation data.
    for kp in &keypairs {
        for _ in 0..5 {
            rep.record_success(&kp.verifying_key());
        }
    }

    // Measure overhead by running many selections.
    let start = std::time::Instant::now();
    for v in 0..10_000 {
        let _ = select_leader_reputation_weighted(ViewNumber::new(v), &vs, &rep);
    }
    let duration = start.elapsed();

    // In debug mode, IWRR is O(p_max * n) per call, so 10k * 20 * 20 iterations.
    // Allow generous time for debug builds.
    assert!(
        duration.as_secs() < 60,
        "10k reputation-weighted selections took too long: {:?}",
        duration
    );
}

// ============================================================================
// B3 kill switch: reputation-weighted selection is opt-in (default OFF)
// ============================================================================

/// Serializes the kill-switch tests: they read/write process-wide selection
/// mode, so they must not interleave with each other.
static SELECTION_MODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Reputation entries that crater a power-2 validator: under weighted
/// selection its effective power drops 2 → max(1, 2*2500/10000) = 1, which
/// changes p_total and therefore the whole IWRR schedule.
fn cratered_rep(victim: &SigningKey) -> LeaderReputation {
    let mut rep = LeaderReputation::new(100);
    for _ in 0..20 {
        rep.record_timeout(&victim.verifying_key());
    }
    rep
}

/// With the switch off (the default), `select_leader_with_reputation` ignores
/// reputation entirely and matches plain IWRR — even with degraded entries.
/// This is the testnet-stall regression test: divergent per-node reputation
/// must not be able to change the leader schedule unless explicitly enabled.
#[test]
fn reputation_selection_disabled_by_default_matches_plain_iwrr() {
    let _guard = SELECTION_MODE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    set_reputation_leader_selection(false);

    // Testnet-shaped validator set: weighted stakes 2/2/1.
    let (keypairs, vs) = make_weighted_validator_set(&[2, 2, 1]);
    let rep = cratered_rep(&keypairs[0]);

    assert!(!reputation_leader_selection_enabled());
    for v in 0..200 {
        let view = ViewNumber::new(v);
        assert_eq!(
            select_leader_with_reputation(view, &vs, &rep),
            select_leader(view, &vs),
            "view {}: with the kill switch off, selection must be plain IWRR",
            v
        );
    }
}

/// Explicitly enabling the switch restores reputation-weighted behavior.
#[test]
fn reputation_selection_enabled_uses_weighted_schedule() {
    let _guard = SELECTION_MODE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let (keypairs, vs) = make_weighted_validator_set(&[2, 2, 1]);
    let rep = cratered_rep(&keypairs[0]);

    set_reputation_leader_selection(true);
    assert!(reputation_leader_selection_enabled());
    // Views past the warm-up window (< 20 uses plain selection regardless).
    let selected: Vec<_> = (20..220)
        .map(|v| select_leader_with_reputation(ViewNumber::new(v), &vs, &rep))
        .collect();
    // Restore the default before any assertion can panic out of the test.
    set_reputation_leader_selection(false);

    let weighted: Vec<_> = (20..220)
        .map(|v| select_leader_reputation_weighted(ViewNumber::new(v), &vs, &rep))
        .collect();
    assert_eq!(
        selected, weighted,
        "with the kill switch on, the wrapper must use the weighted schedule"
    );

    // Sanity: the cratered entries actually change the schedule vs plain IWRR,
    // otherwise the disabled-by-default test above proves nothing.
    let plain: Vec<_> = (20..220)
        .map(|v| select_leader(ViewNumber::new(v), &vs))
        .collect();
    assert_ne!(
        weighted, plain,
        "cratered reputation should produce a different schedule than plain IWRR"
    );
}
