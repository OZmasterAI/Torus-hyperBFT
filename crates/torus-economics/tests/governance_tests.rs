//! Integration tests for on-chain governance (task 2.8).

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;
use torus_economics::governance::{
    ExecutionPayload, GovernanceManager, GovernanceParams, ProposalOutcome, ProposalStatus,
    ProposalType,
};
use torus_economics::{EconomicsError, StakingManager, MIN_SELF_DELEGATION};
use torus_state::cf::{CF_FEE_CONFIG, CF_NATIVE_MARKETS};
use torus_state::StateDb;
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
