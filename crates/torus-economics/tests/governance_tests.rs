//! Integration tests for on-chain governance (task 2.8).

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;
use torus_economics::governance::{
    ExecutionPayload, GovernanceManager, GovernanceParams, ProposalOutcome, ProposalStatus,
    ProposalType,
};
use torus_economics::{EconomicsError, StakingManager, MIN_SELF_DELEGATION};
use torus_state::cf::{CF_FEE_CONFIG, CF_GOVERNANCE_PROPOSALS, CF_NATIVE_MARKETS};
use torus_state::{StateBackend, StateDb};
use torus_types::FixedPoint;

// ============================================================================
// Helpers
// ============================================================================

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn fund(db: &StateDb, a: &Address, amount: U256) {
    let info = AccountInfo {
        balance: amount,
        ..Default::default()
    };
    db.put_account(a, &info).unwrap();
}

/// Create a GovernanceManager + StakingManager from the same DB with test params.
fn setup() -> (tempfile::TempDir, GovernanceManager, StakingManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let gov = GovernanceManager::new(db.clone());
    let staking = StakingManager::new(db);

    let params = GovernanceParams {
        voting_period_blocks: 100,
        quorum_bps: 3300, // 33%
        min_proposal_stake: wei(100),
        permanent_weight_multiplier_num: 3,
        permanent_weight_multiplier_den: 2,
        treasury_address: addr(99),
        timelock_blocks: 10, // short for tests
        permanent_unlock_threshold_bps: 8000,
    };
    gov.set_governance_params(&params).unwrap();

    (dir, gov, staking)
}

/// Register a validator and return its address.
fn setup_validator(staking: &StakingManager, n: u8) -> Address {
    let validator = addr(n);
    fund(staking.state(), &validator, wei(100_000));
    staking
        .register_validator(validator, [n; 32], 500, MIN_SELF_DELEGATION)
        .unwrap();
    validator
}

/// Fund a voter and delegate + optionally permanent stake.
fn setup_voter(
    staking: &StakingManager,
    voter_n: u8,
    validator: Address,
    del_amount: U256,
    perm_amount: U256,
) {
    let voter = addr(voter_n);
    fund(staking.state(), &voter, del_amount + perm_amount);
    if !del_amount.is_zero() {
        staking.delegate(voter, validator, del_amount).unwrap();
    }
    if !perm_amount.is_zero() {
        staking.permanent_stake(voter, perm_amount, 0).unwrap();
    }
}

// ============================================================================
// 2.8.1: Proposal submission
// ============================================================================

#[test]
fn submit_proposal_sufficient_stake() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(
            addr(2),
            "Test Proposal".into(),
            "Description".into(),
            None,
            0,
        )
        .unwrap();

    assert_eq!(id, 1);
    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.proposer, addr(2));
    assert_eq!(proposal.title, "Test Proposal");
    assert_eq!(proposal.status, ProposalStatus::Active);
    assert_eq!(proposal.proposal_type, ProposalType::TextProposal);
    assert_eq!(proposal.start_block, 0);
    assert_eq!(proposal.end_block, 100);
    assert_eq!(proposal.votes_for, U256::ZERO);
    assert_eq!(proposal.votes_against, U256::ZERO);
}

#[test]
fn submit_proposal_insufficient_stake() {
    let (_dir, gov, _staking) = setup();
    // addr(5) has no stake at all.
    let result = gov.submit_proposal(addr(5), "Bad Proposal".into(), "No stake".into(), None, 0);
    assert!(matches!(
        result,
        Err(EconomicsError::InsufficientProposalStake { .. })
    ));
}

#[test]
fn submit_proposal_title_too_long() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let long_title = "x".repeat(129);
    let result = gov.submit_proposal(addr(2), long_title, "Desc".into(), None, 0);
    assert!(matches!(result, Err(EconomicsError::TitleTooLong { .. })));
}

#[test]
fn submit_proposal_auto_incrementing_ids() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id1 = gov
        .submit_proposal(addr(2), "P1".into(), "D".into(), None, 0)
        .unwrap();
    let id2 = gov
        .submit_proposal(addr(2), "P2".into(), "D".into(), None, 0)
        .unwrap();
    let id3 = gov
        .submit_proposal(addr(2), "P3".into(), "D".into(), None, 0)
        .unwrap();

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(id3, 3);
}

// ============================================================================
// 2.8.2 + 2.8.3: Voting with chain-computed weight
// ============================================================================

#[test]
fn cast_vote_weight_delegated_only() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(100), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    let vote = gov.get_vote(id, &addr(2)).unwrap().unwrap();
    // 100 delegated + 0 permanent -> weight = 100
    assert_eq!(vote.weight, wei(100));
    assert!(vote.support);

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.votes_for, wei(100));
    assert_eq!(proposal.votes_against, U256::ZERO);
}

#[test]
fn cast_vote_weight_permanent_only() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    // Only permanent stake, no delegation.
    setup_voter(&staking, 2, validator, U256::ZERO, wei(100));
    // Need another voter with delegation to submit (permanent only can also submit).
    // Actually, permanent stake counts toward min_proposal_stake.

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    let vote = gov.get_vote(id, &addr(2)).unwrap().unwrap();
    // 0 delegated + 100 permanent * 3/2 = 150
    assert_eq!(vote.weight, wei(150));
}

#[test]
fn cast_vote_weight_both_delegated_and_permanent() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(100), wei(100));

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    let vote = gov.get_vote(id, &addr(2)).unwrap().unwrap();
    // 100 delegated + 100 permanent * 3/2 = 100 + 150 = 250
    assert_eq!(vote.weight, wei(250));
}

#[test]
fn cast_vote_expired_proposal() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    // Voting period is 100 blocks. Try to vote at block 101.
    let result = gov.cast_vote(addr(2), id, true, 101);
    assert!(matches!(result, Err(EconomicsError::ProposalNotActive(_))));
}

#[test]
fn cast_vote_duplicate() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    let result = gov.cast_vote(addr(2), id, false, 20);
    assert!(matches!(result, Err(EconomicsError::AlreadyVoted { .. })));
}

#[test]
fn cast_vote_no_weight() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    // addr(10) has no stake.
    let result = gov.cast_vote(addr(10), id, true, 10);
    assert!(matches!(result, Err(EconomicsError::NoVotingWeight(_))));
}

// ============================================================================
// 2.8.5: Finalization
// ============================================================================

#[test]
fn finalize_passed_quorum_met() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // Total staked = 500. Quorum = 500 * 33% = 165. votes_for = 500 >= 165.
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.status, ProposalStatus::Passed);
}

#[test]
fn finalize_rejected_quorum_not_met() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    // Voter A: 100 tokens.
    setup_voter(&staking, 2, validator, wei(100), U256::ZERO);
    // Voter B: 900 tokens (doesn't vote).
    setup_voter(&staking, 3, validator, wei(900), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // Total staked = 1000. Quorum = 330. votes_for = 100 < 330.
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Rejected(id));

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.status, ProposalStatus::Rejected);
}

#[test]
fn finalize_rejected_votes_against() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(300), U256::ZERO);
    setup_voter(&staking, 3, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();
    gov.cast_vote(addr(3), id, false, 10).unwrap();

    // votes_for = 300, votes_against = 500. 300 < 500 -> rejected.
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Rejected(id));
}

#[test]
fn finalize_voting_not_ended() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // end_block = 100. Try to finalize at block 50.
    let result = gov.finalize_proposal(id, 50);
    assert!(matches!(result, Err(EconomicsError::VotingNotEnded(_))));
}

// ============================================================================
// 2.8.4 + 2.8.5: Execution of proposal types
// ============================================================================

#[test]
fn execute_parameter_change() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let payload = ExecutionPayload::ParameterChange {
        param_key: "max_leverage".into(),
        new_value: "50".into(),
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // FIX 15: finalize sets Passed with timelock, execute_proposal runs after timelock
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));

    let outcome = gov.execute_proposal(id, 120).unwrap(); // after timelock (101 + 5)
    assert_eq!(outcome, ProposalOutcome::Executed(id));

    // Verify parameter updated in CF_FEE_CONFIG.
    let data = gov
        .state()
        .get_cf_raw(CF_FEE_CONFIG, b"max_leverage")
        .unwrap()
        .unwrap();
    assert_eq!(&data, b"50");

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.status, ProposalStatus::Executed);
    assert_eq!(proposal.proposal_type, ProposalType::ParameterChange);
}

#[test]
fn execute_treasury_spend() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    // Fund treasury.
    let treasury = addr(99);
    fund(gov.state(), &treasury, wei(10_000));

    let recipient = addr(50);
    let payload = ExecutionPayload::TreasurySpend {
        recipient,
        amount: wei(1_000),
        reason: "Grant".into(),
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // FIX 15: finalize → Passed, then execute after timelock
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));
    let outcome = gov.execute_proposal(id, 120).unwrap();
    assert_eq!(outcome, ProposalOutcome::Executed(id));

    // Verify treasury debited.
    let treasury_bal = gov.state().get_account(&treasury).unwrap().unwrap().balance;
    assert_eq!(treasury_bal, wei(9_000));

    // Verify recipient credited.
    let recipient_bal = gov
        .state()
        .get_account(&recipient)
        .unwrap()
        .unwrap()
        .balance;
    assert_eq!(recipient_bal, wei(1_000));
}

#[test]
fn execute_treasury_spend_insufficient() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    // Treasury has only 100.
    fund(gov.state(), &addr(99), wei(100));

    let payload = ExecutionPayload::TreasurySpend {
        recipient: addr(50),
        amount: wei(1_000),
        reason: "Too much".into(),
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // FIX 15: finalize succeeds (Passed), execution fails at execute_proposal
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));
    let result = gov.execute_proposal(id, 120);
    assert!(matches!(
        result,
        Err(EconomicsError::InsufficientTreasury { .. })
    ));
}

#[test]
fn execute_market_listing() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let payload = ExecutionPayload::MarketListing {
        market_id: 42,
        base_asset: "ETH".into(),
        quote_asset: "USDC".into(),
        lot_size: FixedPoint::from_raw(10_000_000), // 0.1
        tick_size: FixedPoint::from_raw(1_000_000), // 0.01
        initial_margin: FixedPoint::from_raw(500_000_000), // 5.0
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // FIX 15: finalize → Passed, then execute after timelock
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));
    let outcome = gov.execute_proposal(id, 120).unwrap();
    assert_eq!(outcome, ProposalOutcome::Executed(id));

    // Verify market created in CF_NATIVE_MARKETS.
    let market_key = 42u64.to_be_bytes();
    let data = gov
        .state()
        .get_cf_raw(CF_NATIVE_MARKETS, &market_key)
        .unwrap();
    assert!(data.is_some(), "market should exist in CF_NATIVE_MARKETS");

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.proposal_type, ProposalType::MarketListing);
    assert_eq!(proposal.status, ProposalStatus::Executed);
}

#[test]
fn execute_text_proposal_no_state_change() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = gov
        .submit_proposal(addr(2), "Signal Vote".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.status, ProposalStatus::Passed);
    assert_eq!(proposal.proposal_type, ProposalType::TextProposal);
}

// ============================================================================
// 2.8.3: 1.5x weight tests with FixedPoint-precision verification
// ============================================================================

#[test]
fn weight_1_5x_exact_math() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);

    // Voter A: 100 delegated, 0 permanent -> 100
    setup_voter(&staking, 2, validator, wei(100), U256::ZERO);
    // Voter B: 0 delegated, 100 permanent -> 150
    setup_voter(&staking, 3, validator, U256::ZERO, wei(100));
    // Voter C: 100 delegated, 100 permanent -> 250
    setup_voter(&staking, 4, validator, wei(100), wei(100));

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();

    gov.cast_vote(addr(2), id, true, 10).unwrap();
    gov.cast_vote(addr(3), id, true, 10).unwrap();
    gov.cast_vote(addr(4), id, true, 10).unwrap();

    let va = gov.get_vote(id, &addr(2)).unwrap().unwrap();
    let vb = gov.get_vote(id, &addr(3)).unwrap().unwrap();
    let vc = gov.get_vote(id, &addr(4)).unwrap().unwrap();

    assert_eq!(va.weight, wei(100), "100 delegated + 0 permanent = 100");
    assert_eq!(vb.weight, wei(150), "0 delegated + 100 permanent = 150");
    assert_eq!(vc.weight, wei(250), "100 delegated + 100 permanent = 250");

    // Verify total tally.
    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.votes_for, wei(500)); // 100 + 150 + 250
}

// ============================================================================
// Multiple voters with different stakes
// ============================================================================

#[test]
fn multiple_voters_different_stakes_correct_tallies() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);

    // Voter A: 200 delegated, votes yes
    setup_voter(&staking, 2, validator, wei(200), U256::ZERO);
    // Voter B: 300 delegated + 100 permanent = 300 + 150 = 450, votes no
    setup_voter(&staking, 3, validator, wei(300), wei(100));
    // Voter C: 0 delegated + 50 permanent = 75, votes yes
    setup_voter(&staking, 4, validator, U256::ZERO, wei(50));

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();

    gov.cast_vote(addr(2), id, true, 10).unwrap();
    gov.cast_vote(addr(3), id, false, 10).unwrap();
    gov.cast_vote(addr(4), id, true, 10).unwrap();

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.votes_for, wei(200) + wei(75)); // 275
    assert_eq!(proposal.votes_against, wei(450));
}

// ============================================================================
// process_pending_proposals
// ============================================================================

#[test]
fn process_pending_proposals_selective() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    // Submit 3 proposals at different blocks.
    // P1: start=0,  end=100
    // P2: start=50, end=150
    // P3: start=100, end=200
    let p1 = gov
        .submit_proposal(addr(2), "P1".into(), "D".into(), None, 0)
        .unwrap();
    let p2 = gov
        .submit_proposal(addr(2), "P2".into(), "D".into(), None, 50)
        .unwrap();
    let _p3 = gov
        .submit_proposal(addr(2), "P3".into(), "D".into(), None, 100)
        .unwrap();

    // Vote on all proposals within their periods.
    gov.cast_vote(addr(2), p1, true, 50).unwrap();
    gov.cast_vote(addr(2), p2, true, 60).unwrap();
    // p3 voted later
    gov.cast_vote(addr(2), _p3, true, 110).unwrap();

    // Process at block 150: only P1 should be finalized (150 > 100).
    // P2: end_block=150, 150 > 150 is false.
    // P3: end_block=200, 150 > 200 is false.
    let outcomes = gov.process_pending_proposals(150).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0], ProposalOutcome::Passed(p1));

    // Verify P2 and P3 are still active.
    let p2_state = gov.get_proposal(p2).unwrap().unwrap();
    assert_eq!(p2_state.status, ProposalStatus::Active);
    let p3_state = gov.get_proposal(_p3).unwrap().unwrap();
    assert_eq!(p3_state.status, ProposalStatus::Active);
}

// ============================================================================
// 2.8.6: Query functions
// ============================================================================

#[test]
fn query_proposals_by_status() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id1 = gov
        .submit_proposal(addr(2), "P1".into(), "D".into(), None, 0)
        .unwrap();
    let _id2 = gov
        .submit_proposal(addr(2), "P2".into(), "D".into(), None, 0)
        .unwrap();

    // Finalize P1.
    gov.cast_vote(addr(2), id1, true, 10).unwrap();
    gov.finalize_proposal(id1, 101).unwrap();

    let active = gov.get_proposals_by_status(ProposalStatus::Active).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].title, "P2");

    let passed = gov.get_proposals_by_status(ProposalStatus::Passed).unwrap();
    assert_eq!(passed.len(), 1);
    assert_eq!(passed[0].title, "P1");
}

#[test]
fn query_voter_history() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id1 = gov
        .submit_proposal(addr(2), "P1".into(), "D".into(), None, 0)
        .unwrap();
    let id2 = gov
        .submit_proposal(addr(2), "P2".into(), "D".into(), None, 0)
        .unwrap();

    gov.cast_vote(addr(2), id1, true, 10).unwrap();
    gov.cast_vote(addr(2), id2, false, 20).unwrap();

    let history = gov.get_voter_history(&addr(2)).unwrap();
    assert_eq!(history.len(), 2);
}

#[test]
fn query_governance_params_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let gov = GovernanceManager::new(db);

    // FIX MED-NEW-15: No params stored -> returns error (not silent zero-address default).
    assert!(gov.get_governance_params().is_err());
}

// ============================================================================
// Market listing id assignment (parity audit BUG 4b)
// ============================================================================

fn listing(market_id: u64, base: &str) -> ExecutionPayload {
    ExecutionPayload::MarketListing {
        market_id,
        base_asset: base.into(),
        quote_asset: "USDC".into(),
        lot_size: FixedPoint::from_raw(10_000_000),
        tick_size: FixedPoint::from_raw(1_000_000),
        initial_margin: FixedPoint::from_raw(500_000_000),
    }
}

/// Seed a raw market row the way genesis does (8-byte BE key).
fn seed_market(db: &StateDb, market_id: u64) {
    db.put_cf_raw(
        CF_NATIVE_MARKETS,
        &market_id.to_be_bytes(),
        b"genesis-market",
    )
    .unwrap();
}

/// 8-byte market keys currently present in CF_NATIVE_MARKETS, ascending.
fn market_ids(db: &StateDb) -> Vec<u64> {
    db.iterate_cf(CF_NATIVE_MARKETS, None)
        .unwrap()
        .into_iter()
        .filter(|(k, _)| k.len() == 8)
        .map(|(k, _)| u64::from_be_bytes(k[..8].try_into().unwrap()))
        .collect()
}

/// Submit + vote yes at block 10 (voting ends 100, timelock 10).
fn submit_and_pass(gov: &GovernanceManager, payload: ExecutionPayload) -> u64 {
    let id = gov
        .submit_proposal(addr(2), "List".into(), "D".into(), Some(payload), 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();
    id
}

#[test]
fn two_auto_id_listings_get_distinct_ids() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let p1 = submit_and_pass(&gov, listing(0, "ETH"));
    let p2 = submit_and_pass(&gov, listing(0, "SOL"));

    // Both finalize at 101, both execute in the same block after the timelock.
    gov.process_pending_proposals(101).unwrap();
    let outcomes = gov.process_pending_proposals(120).unwrap();
    assert!(outcomes.contains(&ProposalOutcome::Executed(p1)));
    assert!(outcomes.contains(&ProposalOutcome::Executed(p2)));

    assert_eq!(
        market_ids(gov.state()),
        vec![1, 2],
        "auto-assigned listings must not clobber each other (or key 0)"
    );
}

#[test]
fn auto_id_listing_after_genesis_markets_is_max_plus_one() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    for mid in 1..=4 {
        seed_market(gov.state(), mid);
    }
    // Non-market metadata rows that share the CF must be ignored.
    gov.state()
        .put_cf_raw(CF_NATIVE_MARKETS, b"__book_mode__", &[1])
        .unwrap();
    gov.state()
        .put_cf_raw(
            CF_NATIVE_MARKETS,
            b"__next_global_order_id__",
            &u128::MAX.to_be_bytes(),
        )
        .unwrap();

    let id = submit_and_pass(&gov, listing(0, "ARB"));
    gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(
        gov.execute_proposal(id, 120).unwrap(),
        ProposalOutcome::Executed(id)
    );

    assert_eq!(market_ids(gov.state()), vec![1, 2, 3, 4, 5]);
    let row = gov
        .state()
        .get_cf_raw(CF_NATIVE_MARKETS, &5u64.to_be_bytes())
        .unwrap()
        .unwrap();
    assert_ne!(row, b"genesis-market".to_vec());
}

#[test]
fn explicit_listing_id_colliding_at_submit_is_rejected() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    seed_market(gov.state(), 3);

    let err = gov
        .submit_proposal(addr(2), "List".into(), "D".into(), Some(listing(3, "X")), 0)
        .unwrap_err();
    assert!(
        err.to_string().contains("market id 3"),
        "unexpected error: {err}"
    );
}

#[test]
fn explicit_listing_id_colliding_at_execution_is_rejected() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = submit_and_pass(&gov, listing(7, "X"));
    gov.finalize_proposal(id, 101).unwrap();
    // Market 7 appears (e.g. another listing executed) during the timelock.
    seed_market(gov.state(), 7);

    let err = gov.execute_proposal(id, 120).unwrap_err();
    assert!(
        err.to_string().contains("market id 7"),
        "unexpected error: {err}"
    );
    assert_eq!(
        gov.state()
            .get_cf_raw(CF_NATIVE_MARKETS, &7u64.to_be_bytes())
            .unwrap()
            .unwrap(),
        b"genesis-market".to_vec(),
        "existing market row must not be overwritten"
    );
}

#[test]
fn explicit_free_listing_id_is_honoured() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    seed_market(gov.state(), 1);

    let id = submit_and_pass(&gov, listing(42, "X"));
    gov.finalize_proposal(id, 101).unwrap();
    gov.execute_proposal(id, 120).unwrap();
    assert_eq!(market_ids(gov.state()), vec![1, 42]);
}

/// Item 2: the oracle aggregates exactly the listed markets — the 8-byte keys
/// of CF_NATIVE_MARKETS, ascending; metadata rows (other key lengths) skipped.
#[test]
fn listed_market_ids_are_the_8_byte_keys_ascending() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let gov = GovernanceManager::new(db.clone());
    for id in [9u64, 1, 300] {
        db.put_cf_raw(CF_NATIVE_MARKETS, &id.to_be_bytes(), b"m").unwrap();
    }
    db.put_cf_raw(CF_NATIVE_MARKETS, b"__book_mode__", &[1]).unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, b"__next_global_order_id__", &7u128.to_be_bytes())
        .unwrap();
    assert_eq!(gov.listed_market_ids().unwrap(), vec![1, 9, 300]);
    assert!(gov.market_exists(9).unwrap());
    assert!(!gov.market_exists(2).unwrap());
}

// ============================================================================
// Row 43: a listing needs tick_size > 0 and lot_size > 0. Books are built from
// the market row; a lot of 0 accepts zero-quantity orders and a tick of 0 turns
// the tick check off.
// ============================================================================

fn listing_tick_lot(tick_raw: i128, lot_raw: i128) -> ExecutionPayload {
    ExecutionPayload::MarketListing {
        market_id: 0,
        base_asset: "ETH".into(),
        quote_asset: "USDC".into(),
        lot_size: FixedPoint::from_raw(lot_raw),
        tick_size: FixedPoint::from_raw(tick_raw),
        initial_margin: FixedPoint::from_raw(500_000_000),
    }
}

fn assert_not_positive<T: std::fmt::Debug>(r: Result<T, EconomicsError>, field: &str) {
    match r {
        Err(e @ EconomicsError::MarketListingNotPositive { .. }) => {
            assert_eq!(e.to_string(), format!("market listing {field} must be > 0"));
        }
        other => panic!("expected MarketListingNotPositive({field}), got {other:?}"),
    }
}

#[test]
fn submit_market_listing_rejects_zero_or_negative_tick_or_lot() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    let submit = |p| gov.submit_proposal(addr(2), "List".into(), "D".into(), Some(p), 0);

    assert_not_positive(submit(listing_tick_lot(0, 1_000_000)), "tick_size");
    assert_not_positive(submit(listing_tick_lot(-1, 1_000_000)), "tick_size");
    assert_not_positive(submit(listing_tick_lot(1_000_000, 0)), "lot_size");
    assert_not_positive(submit(listing_tick_lot(1_000_000, -1)), "lot_size");
    assert_not_positive(submit(listing_tick_lot(0, 0)), "tick_size");

    // Refused proposals are not stored and consume no proposal id.
    assert!(gov.get_proposal(1).unwrap().is_none());
    assert_eq!(submit(listing_tick_lot(1, 1)).unwrap(), 1);
    assert_eq!(
        gov.get_proposal(1).unwrap().unwrap().proposal_type,
        ProposalType::MarketListing
    );
}

#[test]
fn execute_market_listing_rejects_zero_tick_or_lot_without_writing_a_market() {
    for (tick, lot, field) in [(0, 1, "tick_size"), (1, 0, "lot_size")] {
        let (_dir, gov, staking) = setup();
        let validator = setup_validator(&staking, 1);
        setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

        // Pass a valid listing, then overwrite the stored payload (bypassing
        // submission) so a bad listing reaches execution.
        let id = submit_and_pass(&gov, listing(0, "ETH"));
        gov.finalize_proposal(id, 101).unwrap();
        let mut p = gov.get_proposal(id).unwrap().unwrap();
        p.execution_payload = Some(listing_tick_lot(tick, lot));
        gov.state()
            .put_cf_raw(
                CF_GOVERNANCE_PROPOSALS,
                &id.to_be_bytes(),
                &borsh::to_vec(&p).unwrap(),
            )
            .unwrap();

        // Same as any failed execution: error, no market row, still Passed.
        assert_not_positive(gov.execute_proposal(id, 120), field);
        assert!(market_ids(gov.state()).is_empty(), "no market row ({field})");
        assert_eq!(
            gov.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Passed
        );
        // The per-block path reports the same error.
        assert_not_positive(gov.process_pending_proposals(120), field);
        assert!(market_ids(gov.state()).is_empty(), "no market row ({field})");
    }
}
