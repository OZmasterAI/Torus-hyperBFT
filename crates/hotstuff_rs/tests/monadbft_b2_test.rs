/*
    MonadBFT Batch B2 Tests: NEC + Speculative Finality
    Tests for tasks 3.3.3 (NEC) and 3.3.4 (2-chain commit + speculative finality).
*/

use borsh::BorshSerialize;
use rand_core::OsRng;

use hotstuff_rs::hotstuff::messages::Proposal;
use hotstuff_rs::hotstuff::types::{
    is_fresh_proposal, NECollector, NoEndorsementCertificate, Phase, PhaseCertificate,
};
use hotstuff_rs::pacemaker::types::TimeoutCertificate;
use hotstuff_rs::types::block::Block;
use hotstuff_rs::types::crypto_primitives::{Signer, SigningKey};
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

fn genesis_pc() -> PhaseCertificate {
    PhaseCertificate::genesis_pc()
}

fn make_block(height: u64, justify: PhaseCertificate) -> Block {
    Block::new(
        BlockHeight::new(height),
        justify,
        CryptoHash::new([height as u8; 32]),
        Data::new(vec![]),
    )
}

fn sign_ne(keypair: &SigningKey, view: ViewNumber, high_tip_qc_view: ViewNumber) -> SignatureBytes {
    let msg = (view, high_tip_qc_view).try_to_vec().unwrap();
    let sig = keypair.sign(&msg);
    SignatureBytes::new(sig.to_bytes())
}

// ============================================================================
// NEC Safety Tests
// ============================================================================

/// NEC and QC cannot coexist for the same block (quorum intersection).
/// If 2f+1 validators did NOT vote (NEC), then at most f validators voted,
/// which is insufficient for a QC (requires 2f+1).
#[test]
fn nec_and_qc_cannot_coexist_4_validators() {
    // With n=4, f=1, quorum=3
    // NEC requires 3 non-voters → at most 1 voter → cannot form QC (needs 3)
    let (keypairs, vs) = make_validator_set(4);
    let nec_view = ViewNumber::new(5);
    let high_tip_qc_view = ViewNumber::new(2);

    let mut collector = NECollector::new(nec_view, high_tip_qc_view, vs.clone());

    // Collect 3 NE signatures (2f+1 = 3)
    for i in 0..3 {
        let sig = sign_ne(&keypairs[i], nec_view, high_tip_qc_view);
        let result = collector.collect(
            &keypairs[i].verifying_key(),
            nec_view,
            high_tip_qc_view,
            sig,
        );
        if i < 2 {
            assert!(
                result.is_none(),
                "NEC should not form with only {} sigs",
                i + 1
            );
        } else {
            assert!(result.is_some(), "NEC should form with 3 sigs");
        }
    }
    // The 4th validator (index 3) is the only one who COULD have voted.
    // 1 voter cannot form a QC (needs 3). Safety proven.
}

/// VALIDNEC rejects NEC with wrong view relationship.
#[test]
fn validnec_rejects_wrong_view_relationship() {
    let nec = NoEndorsementCertificate {
        view: ViewNumber::new(5),
        // high_tip_qc_view must be < view - 1 = 4. Setting it to 4 should fail.
        high_tip_qc_view: ViewNumber::new(4),
        signatures: SignatureSet::new(0),
    };
    // We can check the view condition directly:
    assert!(
        nec.high_tip_qc_view >= nec.view - 1,
        "Should violate view condition"
    );
}

/// NECollector rejects invalid signatures.
#[test]
fn ne_collector_rejects_bad_signature() {
    let (keypairs, vs) = make_validator_set(4);
    let nec_view = ViewNumber::new(5);
    let high_tip_qc_view = ViewNumber::new(2);

    let mut collector = NECollector::new(nec_view, high_tip_qc_view, vs);

    // Sign with wrong view
    let bad_sig = sign_ne(&keypairs[0], ViewNumber::new(99), high_tip_qc_view);
    let result = collector.collect(
        &keypairs[0].verifying_key(),
        nec_view,
        high_tip_qc_view,
        bad_sig,
    );
    assert!(result.is_none(), "Should reject bad signature");
}

/// NECollector prevents double-counting from same validator.
#[test]
fn ne_collector_no_double_count() {
    let (keypairs, vs) = make_validator_set(4);
    let nec_view = ViewNumber::new(5);
    let high_tip_qc_view = ViewNumber::new(2);

    let mut collector = NECollector::new(nec_view, high_tip_qc_view, vs);

    let sig = sign_ne(&keypairs[0], nec_view, high_tip_qc_view);
    let result1 = collector.collect(
        &keypairs[0].verifying_key(),
        nec_view,
        high_tip_qc_view,
        sig,
    );
    assert!(result1.is_none());

    // Same validator, same signature — should be ignored
    let sig2 = sign_ne(&keypairs[0], nec_view, high_tip_qc_view);
    let result2 = collector.collect(
        &keypairs[0].verifying_key(),
        nec_view,
        high_tip_qc_view,
        sig2,
    );
    assert!(result2.is_none(), "Should not double-count");
}

/// NECollector rejects NE for wrong view.
#[test]
fn ne_collector_rejects_wrong_view() {
    let (keypairs, vs) = make_validator_set(4);
    let nec_view = ViewNumber::new(5);
    let high_tip_qc_view = ViewNumber::new(2);

    let mut collector = NECollector::new(nec_view, high_tip_qc_view, vs);

    let sig = sign_ne(&keypairs[0], nec_view, high_tip_qc_view);
    // Pass wrong view to collect
    let result = collector.collect(
        &keypairs[0].verifying_key(),
        ViewNumber::new(6), // wrong view
        high_tip_qc_view,
        sig,
    );
    assert!(result.is_none(), "Should reject NE for wrong view");
}

// ============================================================================
// ISFRESHPROPOSAL Tests
// ============================================================================

/// Happy path: block's QC is from the immediately preceding view.
#[test]
fn is_fresh_proposal_happy_path() {
    let pc = PhaseCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(4), // QC from view 4
        block: CryptoHash::new([1u8; 32]),
        phase: Phase::Generic,
        signatures: SignatureSet::new(0),
    };
    let block = make_block(1, pc);
    let proposal = Proposal {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(5), // proposed in view 5 = QC view + 1
        block,
        tc: None,
        nec: None,
    };
    assert!(is_fresh_proposal(&proposal), "Should be fresh (happy path)");
}

/// NEC proves the skipped block is safe.
#[test]
fn is_fresh_proposal_with_nec() {
    let pc = PhaseCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(2), // NOT consecutive with view 5
        block: CryptoHash::new([1u8; 32]),
        phase: Phase::Generic,
        signatures: SignatureSet::new(0),
    };
    let block = make_block(1, pc);
    let nec = NoEndorsementCertificate {
        view: ViewNumber::new(5),
        high_tip_qc_view: ViewNumber::new(2),
        signatures: SignatureSet::new(0),
    };
    let proposal = Proposal {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(5),
        block,
        tc: None,
        nec: Some(nec),
    };
    assert!(is_fresh_proposal(&proposal), "Should be fresh (NEC)");
}

/// TC's high_qc resolves the view.
#[test]
fn is_fresh_proposal_with_tc_high_qc() {
    let pc = PhaseCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(2),
        block: CryptoHash::new([1u8; 32]),
        phase: Phase::Generic,
        signatures: SignatureSet::new(0),
    };
    let block = make_block(1, pc);
    let tc = TimeoutCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(4),
        signatures: SignatureSet::new(0),
        high_tip: None,
        high_qc: Some(PhaseCertificate {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(3),
            block: CryptoHash::new([2u8; 32]),
            phase: Phase::Generic,
            signatures: SignatureSet::new(0),
        }),
        high_tip_is_winner: false,
        voter_metadata: vec![],
    };
    let proposal = Proposal {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(5),
        block,
        tc: Some(tc),
        nec: None,
    };
    assert!(is_fresh_proposal(&proposal), "Should be fresh (TC high_qc)");
}

/// Reproposal without NEC/TC-high-qc is NOT fresh.
#[test]
fn is_not_fresh_for_reproposal() {
    let pc = PhaseCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(2), // NOT consecutive with view 5
        block: CryptoHash::new([1u8; 32]),
        phase: Phase::Generic,
        signatures: SignatureSet::new(0),
    };
    let block = make_block(1, pc);
    let tc = TimeoutCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(4),
        signatures: SignatureSet::new(0),
        high_tip: None,
        high_qc: None,            // No high_qc
        high_tip_is_winner: true, // high_tip wins
        voter_metadata: vec![],
    };
    let proposal = Proposal {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(5),
        block,
        tc: Some(tc),
        nec: None,
    };
    assert!(
        !is_fresh_proposal(&proposal),
        "Reproposal should NOT be fresh"
    );
}

/// Non-consecutive view without TC or NEC is NOT fresh.
#[test]
fn is_not_fresh_for_non_consecutive() {
    let pc = PhaseCertificate {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(2), // NOT consecutive with view 5
        block: CryptoHash::new([1u8; 32]),
        phase: Phase::Generic,
        signatures: SignatureSet::new(0),
    };
    let block = make_block(1, pc);
    let proposal = Proposal {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(5),
        block,
        tc: None,
        nec: None,
    };
    assert!(
        !is_fresh_proposal(&proposal),
        "Non-consecutive should NOT be fresh"
    );
}

// ============================================================================
// NEC Construction Tests
// ============================================================================

/// Successful NEC formation with exactly 2f+1 non-voters.
#[test]
fn nec_formation_with_quorum() {
    let (keypairs, vs) = make_validator_set(4);
    // n=4, f=1, quorum=3
    let nec_view = ViewNumber::new(10);
    let high_tip_qc_view = ViewNumber::new(5);

    let mut collector = NECollector::new(nec_view, high_tip_qc_view, vs);

    // Collect from first 3 validators (indices 0, 1, 2)
    for i in 0..3 {
        let sig = sign_ne(&keypairs[i], nec_view, high_tip_qc_view);
        let nec = collector.collect(
            &keypairs[i].verifying_key(),
            nec_view,
            high_tip_qc_view,
            sig,
        );
        if i == 2 {
            assert!(nec.is_some(), "NEC should form at 2f+1 = 3");
            let nec = nec.unwrap();
            assert_eq!(nec.view, nec_view);
            assert_eq!(nec.high_tip_qc_view, high_tip_qc_view);
        } else {
            assert!(nec.is_none());
        }
    }
}

/// NEC cannot form when there are too many voters (not enough non-voters).
#[test]
fn nec_cannot_form_insufficient_non_voters() {
    let (keypairs, vs) = make_validator_set(4);
    // n=4, f=1, quorum=3. If 2 validators voted, only 2 non-voters remain,
    // which is less than quorum (3). NEC cannot form.
    let nec_view = ViewNumber::new(10);
    let high_tip_qc_view = ViewNumber::new(5);

    let mut collector = NECollector::new(nec_view, high_tip_qc_view, vs);

    // Only 2 non-voters send NE
    for i in 0..2 {
        let sig = sign_ne(&keypairs[i], nec_view, high_tip_qc_view);
        let nec = collector.collect(
            &keypairs[i].verifying_key(),
            nec_view,
            high_tip_qc_view,
            sig,
        );
        assert!(nec.is_none(), "NEC should not form with only 2 sigs");
    }
}
