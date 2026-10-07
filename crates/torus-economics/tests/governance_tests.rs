//! Integration tests for on-chain governance (task 2.8).

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;
use torus_economics::governance::{
    ExecutionPayload, GovernanceManager, GovernanceParams, ProposalOutcome, ProposalStatus,
    ProposalType,
};
use torus_economics::{
    EconomicsError, PermanentStakeInfo, StakingManager, ValidatorWhitelistEntry,
    MIN_SELF_DELEGATION, WHITELIST_EXPIRY_BLOCKS,
};
use torus_state::cf::{
    ALL_CF_NAMES, CF_ACCOUNTS, CF_CONSENSUS_META, CF_FEE_CONFIG, CF_GOVERNANCE_PROPOSALS,
    CF_NATIVE_MARKETS, CF_STAKING_PERMANENT, CF_STAKING_REWARDS,
};
use torus_state::error::StateError;
use torus_state::{AtomicWriteOp, StateBackend, StateDb};
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
        param_key: "voting_period_blocks".into(),
        new_value: "2000".into(),
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();

    // FIX 15: finalize sets Passed with timelock, execute_proposal runs after timelock
    let outcome = gov.finalize_proposal(id, 101).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(id));

    let outcome = gov.execute_proposal(id, 120).unwrap(); // after timelock (101 + 10)
    assert_eq!(outcome, ProposalOutcome::Executed(id));

    // The vote lands in the GovernanceParams record (the one governance
    // reads), not in a raw CF_FEE_CONFIG row of its own.
    assert_eq!(
        gov.get_governance_params().unwrap().voting_period_blocks,
        2000
    );
    assert_eq!(
        gov.state()
            .get_cf_raw(CF_FEE_CONFIG, b"voting_period_blocks")
            .unwrap(),
        None
    );

    let proposal = gov.get_proposal(id).unwrap().unwrap();
    assert_eq!(proposal.status, ProposalStatus::Executed);
    assert_eq!(proposal.proposal_type, ProposalType::ParameterChange);
}

/// s99: `maintenance_margin_bps`, `max_leverage` and `liquidation_penalty_bps`
/// had nothing reading them (MM is half the IM, leverage is per market, no
/// liquidation penalty by design): a vote "passed" and changed nothing. They
/// are refused at submission, for any value.
#[test]
fn dead_parameter_keys_are_refused_at_submission() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    for (key, value) in [
        ("maintenance_margin_bps", "500"),
        ("max_leverage", "50"),
        ("liquidation_penalty_bps", "100"),
    ] {
        let payload = ExecutionPayload::ParameterChange {
            param_key: key.into(),
            new_value: value.into(),
        };
        let r = gov.submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0);
        assert!(
            matches!(&r, Err(EconomicsError::ParameterNotModifiable(k)) if k == key),
            "{key}: {r:?}"
        );
    }
}

/// Submit a ParameterChange from addr(2) at `block`, vote yes with every
/// voter in `yes`, finalize after the voting period and execute after the
/// timelock (both read from the current params). Returns the proposal id.
fn pass_param(gov: &GovernanceManager, key: &str, value: &str, yes: &[u8], block: u64) -> u64 {
    let payload = ExecutionPayload::ParameterChange {
        param_key: key.into(),
        new_value: value.into(),
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), block)
        .unwrap();
    for v in yes {
        gov.cast_vote(addr(*v), id, true, block + 1).unwrap();
    }
    let end = gov.get_proposal(id).unwrap().unwrap().end_block;
    assert_eq!(
        gov.finalize_proposal(id, end + 1).unwrap(),
        ProposalOutcome::Passed(id)
    );
    let after = gov.get_proposal(id).unwrap().unwrap().executable_after;
    assert_eq!(
        gov.execute_proposal(id, after).unwrap(),
        ProposalOutcome::Executed(id)
    );
    id
}

/// s99: a passed `voting_period_blocks` vote sets the voting period of the
/// next proposal.
#[test]
fn voting_period_vote_sets_next_proposals_end_block() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    pass_param(&gov, "voting_period_blocks", "2000", &[2], 0);
    let next = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 500)
        .unwrap();
    assert_eq!(gov.get_proposal(next).unwrap().unwrap().end_block, 2500);
}

/// s99: a passed `quorum_bps` vote moves the quorum. 25% participation
/// fails the 33% quorum and passes the 10% one.
#[test]
fn quorum_vote_changes_the_outcome_at_the_boundary() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    // Total staked 2,000: addr(2) alone is 25%.
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    setup_voter(&staking, 3, validator, wei(1_500), U256::ZERO);

    // Under the 33% quorum, 25% participation is rejected.
    let before = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 0)
        .unwrap();
    gov.cast_vote(addr(2), before, true, 1).unwrap();
    assert_eq!(
        gov.finalize_proposal(before, 101).unwrap(),
        ProposalOutcome::Rejected(before)
    );

    pass_param(&gov, "quorum_bps", "1000", &[2, 3], 200);
    assert_eq!(gov.get_governance_params().unwrap().quorum_bps, 1000);

    let after = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 400)
        .unwrap();
    gov.cast_vote(addr(2), after, true, 401).unwrap();
    assert_eq!(
        gov.finalize_proposal(after, 501).unwrap(),
        ProposalOutcome::Passed(after)
    );
}

/// s99: a passed `timelock_blocks` vote sets when the next passed proposal
/// can execute.
#[test]
fn timelock_vote_sets_when_a_passed_proposal_executes() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    pass_param(&gov, "timelock_blocks", "50", &[2], 0);

    let payload = ExecutionPayload::ParameterChange {
        param_key: "price_band_bps".into(),
        new_value: "1000".into(),
    };
    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 300)
        .unwrap();
    gov.cast_vote(addr(2), id, true, 301).unwrap();
    assert_eq!(
        gov.finalize_proposal(id, 401).unwrap(),
        ProposalOutcome::Passed(id)
    );
    assert_eq!(gov.get_proposal(id).unwrap().unwrap().executable_after, 451);
    // The old 10-block timelock would allow it at 411.
    assert!(matches!(
        gov.execute_proposal(id, 411),
        Err(EconomicsError::TimelockNotExpired {
            executable_after: 451,
            ..
        })
    ));
    assert_eq!(
        gov.execute_proposal(id, 451).unwrap(),
        ProposalOutcome::Executed(id)
    );
}

/// s99: a passed `permanent_unlock_threshold_bps` vote sets the
/// supermajority a PermanentUnlock needs: 60% for fails 80%, passes 60%.
#[test]
fn unlock_threshold_vote_changes_the_permanent_unlock_outcome() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(600), U256::ZERO);
    setup_voter(&staking, 3, validator, wei(400), U256::ZERO);
    setup_voter(&staking, 5, validator, U256::ZERO, wei(10));
    let unlock = || ExecutionPayload::PermanentUnlock {
        staker: addr(5),
        amount: wei(10),
    };
    // 600 for, 400 against: 60%.
    let vote_60 = |id: u64, block: u64| {
        gov.cast_vote(addr(2), id, true, block).unwrap();
        gov.cast_vote(addr(3), id, false, block).unwrap();
    };

    let before = gov
        .submit_proposal(addr(2), "U".into(), "D".into(), Some(unlock()), 0)
        .unwrap();
    vote_60(before, 1);
    assert_eq!(
        gov.finalize_proposal(before, 101).unwrap(),
        ProposalOutcome::Rejected(before)
    );

    pass_param(&gov, "permanent_unlock_threshold_bps", "6000", &[2, 3], 200);
    assert_eq!(
        gov.get_governance_params()
            .unwrap()
            .permanent_unlock_threshold_bps,
        6000
    );

    let after = gov
        .submit_proposal(addr(2), "U".into(), "D".into(), Some(unlock()), 400)
        .unwrap();
    vote_60(after, 401);
    assert_eq!(
        gov.finalize_proposal(after, 501).unwrap(),
        ProposalOutcome::Passed(after)
    );
}

/// s99: a passed `permanent_weight_multiplier_num` vote changes the weight
/// of permanent stake: 100 permanent weighs 150 at 3/2, 250 at 5/2.
#[test]
fn multiplier_vote_changes_permanent_stake_weight() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    setup_voter(&staking, 4, validator, U256::ZERO, wei(100));

    pass_param(&gov, "permanent_weight_multiplier_num", "5", &[2], 0);

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 300)
        .unwrap();
    gov.cast_vote(addr(4), id, true, 301).unwrap();
    assert_eq!(
        gov.get_vote(id, &addr(4)).unwrap().unwrap().weight,
        wei(250)
    );
}

/// s99: two passed parameter changes executing in the same block both land
/// (each reloads the record the one before it wrote): num 5 and den 4 give
/// 5/4, so 100 permanent weighs 125.
#[test]
fn two_parameter_changes_in_one_block_both_land() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    setup_voter(&staking, 4, validator, U256::ZERO, wei(100));

    for (key, value) in [
        ("permanent_weight_multiplier_num", "5"),
        ("permanent_weight_multiplier_den", "4"),
    ] {
        submit_and_pass(
            &gov,
            ExecutionPayload::ParameterChange {
                param_key: key.into(),
                new_value: value.into(),
            },
        );
    }
    gov.process_pending_proposals(101).unwrap();
    assert_eq!(
        gov.process_pending_proposals(111).unwrap(),
        vec![ProposalOutcome::Executed(1), ProposalOutcome::Executed(2)]
    );
    let params = gov.get_governance_params().unwrap();
    assert_eq!(
        (
            params.permanent_weight_multiplier_num,
            params.permanent_weight_multiplier_den
        ),
        (5, 4)
    );

    let id = gov
        .submit_proposal(addr(2), "P".into(), "D".into(), None, 300)
        .unwrap();
    gov.cast_vote(addr(4), id, true, 301).unwrap();
    assert_eq!(
        gov.get_vote(id, &addr(4)).unwrap().unwrap().weight,
        wei(125)
    );
}

/// s94 option 2: the placement price band is a ParameterChange key
/// (`price_band_bps`, 100..=9,000): a valid value executes into
/// CF_FEE_CONFIG (the executor and RPC read it there); out of range or not a
/// number is refused at submission.
#[test]
fn execute_price_band_parameter_change() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    for bad in ["99", "9001", "0", "abc"] {
        let payload = ExecutionPayload::ParameterChange { param_key: "price_band_bps".into(), new_value: bad.into() };
        let r = gov.submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0);
        assert!(matches!(r, Err(EconomicsError::InvalidParameterValue { .. })), "{bad}: {r:?}");
    }
    let payload = ExecutionPayload::ParameterChange { param_key: "price_band_bps".into(), new_value: "1000".into() };
    let id = gov.submit_proposal(addr(2), "P".into(), "D".into(), Some(payload), 0).unwrap();
    gov.cast_vote(addr(2), id, true, 10).unwrap();
    assert_eq!(gov.finalize_proposal(id, 101).unwrap(), ProposalOutcome::Passed(id));
    assert_eq!(gov.execute_proposal(id, 120).unwrap(), ProposalOutcome::Executed(id));
    let data = gov.state().get_cf_raw(CF_FEE_CONFIG, b"price_band_bps").unwrap().unwrap();
    assert_eq!(&data, b"1000");
    assert_eq!(torus_types::price_band_bps(Some(&data)), 1_000);
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

        // A direct call: error, no market row, still Passed.
        assert_not_positive(gov.execute_proposal(id, 120), field);
        assert!(market_ids(gov.state()).is_empty(), "no market row ({field})");
        assert_eq!(
            gov.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Passed
        );
        // The per-block path marks it Failed with the same reason.
        assert_eq!(
            gov.process_pending_proposals(120).unwrap(),
            vec![ProposalOutcome::Failed(
                id,
                format!("market listing {field} must be > 0")
            )]
        );
        assert!(market_ids(gov.state()).is_empty(), "no market row ({field})");
        assert_eq!(
            gov.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Failed
        );
    }
}

// ============================================================================
// s94: a passed proposal whose execution fails becomes Failed (terminal) and
// no longer blocks later proposals. Storage errors still abort the step.
// ============================================================================

/// Overwrite a stored proposal's payload (bypassing submission checks).
fn overwrite_payload(gov: &GovernanceManager, id: u64, payload: ExecutionPayload) {
    let mut p = gov.get_proposal(id).unwrap().unwrap();
    p.execution_payload = Some(payload);
    gov.state()
        .put_cf_raw(
            CF_GOVERNANCE_PROPOSALS,
            &id.to_be_bytes(),
            &borsh::to_vec(&p).unwrap(),
        )
        .unwrap();
}

/// Every column family except the proposals CF, in a comparable form.
fn dump_all_but_proposals(db: &StateDb) -> Vec<(&'static str, Vec<(Vec<u8>, Vec<u8>)>)> {
    ALL_CF_NAMES
        .iter()
        .filter(|cf| **cf != CF_GOVERNANCE_PROPOSALS)
        .map(|cf| (*cf, db.iterate_cf(cf, None).unwrap()))
        .collect()
}

#[test]
fn failed_execution_does_not_block_a_later_proposal_in_the_same_block() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let p1 = submit_and_pass(&gov, listing(7, "X"));
    let p2 = submit_and_pass(&gov, listing(8, "Y"));
    assert_eq!(
        gov.process_pending_proposals(101).unwrap(),
        vec![ProposalOutcome::Passed(p1), ProposalOutcome::Passed(p2)]
    );
    // Market 7 appears during the timelock: p1 (executed first) now fails.
    seed_market(gov.state(), 7);

    assert_eq!(
        gov.process_pending_proposals(120).unwrap(),
        vec![
            ProposalOutcome::Failed(p1, "market id 7 already exists".into()),
            ProposalOutcome::Executed(p2),
        ]
    );
    assert_eq!(gov.get_proposal(p1).unwrap().unwrap().status, ProposalStatus::Failed);
    assert_eq!(gov.get_proposal(p2).unwrap().unwrap().status, ProposalStatus::Executed);
    assert_eq!(market_ids(gov.state()), vec![7, 8]);
    assert_eq!(
        gov.state()
            .get_cf_raw(CF_NATIVE_MARKETS, &7u64.to_be_bytes())
            .unwrap()
            .unwrap(),
        b"genesis-market".to_vec(),
        "the failed listing must not overwrite market 7"
    );
    assert_eq!(
        gov.get_proposals_by_status(ProposalStatus::Failed)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        vec![p1]
    );
}

#[test]
fn failed_proposal_is_terminal_and_not_retried() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let id = submit_and_pass(&gov, listing(7, "X"));
    gov.finalize_proposal(id, 101).unwrap();
    seed_market(gov.state(), 7);
    assert_eq!(
        gov.process_pending_proposals(120).unwrap(),
        vec![ProposalOutcome::Failed(id, "market id 7 already exists".into())]
    );

    // Remove the conflict: a retry would now succeed, so a no-op proves
    // that Failed is never retried.
    gov.state()
        .delete_cf_raw(CF_NATIVE_MARKETS, &7u64.to_be_bytes())
        .unwrap();
    let state_before = dump_all_but_proposals(gov.state());
    let proposals_before = gov.state().iterate_cf(CF_GOVERNANCE_PROPOSALS, None).unwrap();
    for block in [121, 500, 10_000] {
        assert!(gov.process_pending_proposals(block).unwrap().is_empty());
    }
    assert_eq!(dump_all_but_proposals(gov.state()), state_before);
    assert_eq!(
        gov.state().iterate_cf(CF_GOVERNANCE_PROPOSALS, None).unwrap(),
        proposals_before
    );
    // A direct call refuses it too.
    assert!(matches!(
        gov.execute_proposal(id, 10_001),
        Err(EconomicsError::ProposalNotPassed(p)) if p == id
    ));
}

/// Each payload kind that can fail at execution: the step marks the proposal
/// Failed with the reason and writes nothing else (no half-applied payload).
#[test]
fn failed_execution_leaves_no_partial_state_for_any_payload_kind() {
    type Sabotage = fn(&GovernanceManager, u64);
    let cases: Vec<(&str, ExecutionPayload, Sabotage, String)> = vec![
        (
            "param change, value out of range",
            ExecutionPayload::ParameterChange {
                param_key: "voting_period_blocks".into(),
                new_value: "2000".into(),
            },
            |gov, id| {
                overwrite_payload(
                    gov,
                    id,
                    ExecutionPayload::ParameterChange {
                        param_key: "voting_period_blocks".into(),
                        new_value: "0".into(),
                    },
                )
            },
            "invalid parameter value for voting_period_blocks: must be between 1000 and 1000000 blocks"
                .into(),
        ),
        (
            // s99: a dead key is refused at execution too (a proposal stored
            // before the fix).
            "param change, dead key",
            ExecutionPayload::ParameterChange {
                param_key: "price_band_bps".into(),
                new_value: "1000".into(),
            },
            |gov, id| {
                overwrite_payload(
                    gov,
                    id,
                    ExecutionPayload::ParameterChange {
                        param_key: "max_leverage".into(),
                        new_value: "50".into(),
                    },
                )
            },
            "governance parameter not modifiable: max_leverage".into(),
        ),
        (
            // s99: one side of the multiplier changes; the resulting pair is
            // checked (here the stored den is 0, so num 5 / den 0 is refused).
            "param change, multiplier pair invalid",
            ExecutionPayload::ParameterChange {
                param_key: "permanent_weight_multiplier_num".into(),
                new_value: "5".into(),
            },
            |gov, _| {
                let mut params = gov.get_governance_params().unwrap();
                params.permanent_weight_multiplier_den = 0;
                gov.set_governance_params(&params).unwrap();
            },
            "invalid parameter value for permanent_weight_multiplier_den: must be between 1 and 100 (zero disallowed)"
                .into(),
        ),
        (
            "param change, key not modifiable",
            ExecutionPayload::ParameterChange {
                param_key: "voting_period_blocks".into(),
                new_value: "2000".into(),
            },
            |gov, id| {
                overwrite_payload(
                    gov,
                    id,
                    ExecutionPayload::ParameterChange {
                        param_key: "gov_next_id".into(),
                        new_value: "1".into(),
                    },
                )
            },
            "governance parameter not modifiable: gov_next_id".into(),
        ),
        (
            "treasury spend over balance",
            ExecutionPayload::TreasurySpend {
                recipient: addr(50),
                amount: U256::from(1_000u64),
                reason: "r".into(),
            },
            |gov, _| fund(gov.state(), &addr(99), U256::from(999u64)),
            "treasury insufficient balance: have 999, need 1000".into(),
        ),
        (
            "listing id taken",
            listing(7, "X"),
            |gov, _| seed_market(gov.state(), 7),
            "market id 7 already exists".into(),
        ),
        (
            "listing tick 0",
            listing(0, "X"),
            |gov, id| overwrite_payload(gov, id, listing_tick_lot(0, 1)),
            "market listing tick_size must be > 0".into(),
        ),
        (
            "permanent unlock, no stake",
            ExecutionPayload::PermanentUnlock {
                staker: addr(60),
                amount: U256::from(1u64),
            },
            |_, _| {},
            format!("permanent stake not found for {}", addr(60)),
        ),
        (
            "permanent unlock, amount over stake",
            ExecutionPayload::PermanentUnlock {
                staker: addr(3),
                amount: wei(1_000),
            },
            |_, _| {},
            format!(
                "permanent unlock amount {} exceeds stake {}",
                wei(1_000),
                wei(10)
            ),
        ),
        (
            "permanent unlock, amount 0",
            ExecutionPayload::PermanentUnlock {
                staker: addr(3),
                amount: U256::ZERO,
            },
            |_, _| {},
            "invalid parameter value for amount: permanent unlock amount must be > 0".into(),
        ),
    ];

    for (name, payload, sabotage, reason) in cases {
        let (_dir, gov, staking) = setup();
        let validator = setup_validator(&staking, 1);
        setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
        // addr(3): a small permanent staker (unlock cases), never votes.
        setup_voter(&staking, 3, validator, U256::ZERO, wei(10));
        // addr(3)'s accrued rewards: an unlock that half-ran would claim them.
        staking.credit_rewards(addr(3), wei(1)).unwrap();

        let id = submit_and_pass(&gov, payload);
        gov.finalize_proposal(id, 101).unwrap();
        sabotage(&gov, id);
        let state_before = dump_all_but_proposals(gov.state());
        let mut expected = gov.get_proposal(id).unwrap().unwrap();
        assert_eq!(expected.status, ProposalStatus::Passed, "{name}");

        assert_eq!(
            gov.process_pending_proposals(120).unwrap(),
            vec![ProposalOutcome::Failed(id, reason)],
            "{name}"
        );
        assert_eq!(dump_all_but_proposals(gov.state()), state_before, "{name}");
        // The proposal row differs only in its status.
        expected.status = ProposalStatus::Failed;
        assert_eq!(
            gov.state()
                .get_cf_raw(CF_GOVERNANCE_PROPOSALS, &id.to_be_bytes())
                .unwrap()
                .unwrap(),
            borsh::to_vec(&expected).unwrap(),
            "{name}"
        );
    }
}

/// `StateDb` whose writes to one `(cf, key)` fail (an injected storage
/// fault): a plain put / delete of it, and any `atomic_write` containing it,
/// which then applies nothing (as a RocksDB `WriteBatch` does).
#[derive(Clone)]
struct PoisonedKey {
    inner: StateDb,
    cf: &'static str,
    key: Vec<u8>,
}

impl PoisonedKey {
    fn new(inner: StateDb, cf: &'static str, key: &[u8]) -> Self {
        Self {
            inner,
            cf,
            key: key.to_vec(),
        }
    }
    fn hit(&self, cf: &str, key: &[u8]) -> bool {
        cf == self.cf && key == self.key.as_slice()
    }
    fn fail() -> StateError {
        StateError::InvalidData("injected write failure".into())
    }
}

impl StateBackend for PoisonedKey {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        StateBackend::get_cf_raw(&self.inner, cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        StateBackend::put_cf_raw(&self.inner, cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        StateBackend::delete_cf_raw(&self.inner, cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        StateBackend::iterate_cf(&self.inner, cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        let poisoned = ops.iter().any(|op| match op {
            AtomicWriteOp::Put { cf, key, .. } | AtomicWriteOp::Delete { cf, key } => {
                self.hit(cf, key)
            }
        });
        if poisoned {
            return Err(Self::fail());
        }
        StateBackend::atomic_write(&self.inner, ops)
    }
}

/// A storage error is not an execution failure: the step still returns the
/// error (as before), the proposal stays Passed and later ones are not run.
#[test]
fn storage_error_during_execution_still_aborts_the_step() {
    let (_dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);

    let p1 = submit_and_pass(&gov, listing(7, "X"));
    let p2 = submit_and_pass(&gov, listing(8, "Y"));
    gov.process_pending_proposals(101).unwrap();

    let faulty = GovernanceManager::new(PoisonedKey::new(
        gov.state().clone(),
        CF_NATIVE_MARKETS,
        &7u64.to_be_bytes(),
    ));
    match faulty.process_pending_proposals(120) {
        Err(EconomicsError::State(e)) => assert!(e.to_string().contains("injected")),
        other => panic!("expected the storage error, got {other:?}"),
    }
    assert_eq!(gov.get_proposal(p1).unwrap().unwrap().status, ProposalStatus::Passed);
    assert_eq!(gov.get_proposal(p2).unwrap().unwrap().status, ProposalStatus::Passed);
    assert!(market_ids(gov.state()).is_empty());
}

// ============================================================================
// Row 75 (s94): a payload and its proposal's status change are one atomic
// write. A storage fault anywhere in it leaves every CF unchanged and the
// proposal Passed; the next block applies it exactly once.
// ============================================================================

/// Every column family's rows, per CF name.
type CfDump = Vec<(&'static str, Vec<(Vec<u8>, Vec<u8>)>)>;

/// Every column family, in a comparable form.
fn dump_all(db: &StateDb) -> CfDump {
    ALL_CF_NAMES
        .iter()
        .map(|cf| (*cf, db.iterate_cf(cf, None).unwrap()))
        .collect()
}

fn balance(db: &StateDb, a: &Address) -> U256 {
    db.get_account(a)
        .unwrap()
        .map(|i| i.balance)
        .unwrap_or_default()
}

/// Run block `block` through a manager whose writes to `(cf, key)` fail: the
/// step returns the injected error, no CF changes, proposal `id` stays Passed.
fn assert_faulted_block_changes_nothing(
    gov: &GovernanceManager,
    cf: &'static str,
    key: &[u8],
    block: u64,
    id: u64,
) {
    let before = dump_all(gov.state());
    let faulty = GovernanceManager::new(PoisonedKey::new(gov.state().clone(), cf, key));
    match faulty.process_pending_proposals(block) {
        Err(EconomicsError::State(e)) => assert!(e.to_string().contains("injected"), "{e}"),
        other => panic!("expected the injected storage error, got {other:?}"),
    }
    assert!(
        dump_all(gov.state()) == before,
        "a failed execution must leave no partial write (fault on {cf})"
    );
    assert_eq!(gov.get_proposal(id).unwrap().unwrap().status, ProposalStatus::Passed);
}

/// Governance + staking with a funded treasury (addr 99, 10k tokens),
/// addr(50) holding 3 tokens, and addr(3) holding a permanent stake of 10
/// tokens (locked at block 0) plus 1 token of accrued rewards.
fn setup_row75() -> (tempfile::TempDir, GovernanceManager, StakingManager) {
    let (dir, gov, staking) = setup();
    let validator = setup_validator(&staking, 1);
    setup_voter(&staking, 2, validator, wei(500), U256::ZERO);
    setup_voter(&staking, 3, validator, U256::ZERO, wei(10));
    staking.credit_rewards(addr(3), wei(1)).unwrap();
    fund(gov.state(), &addr(99), wei(10_000));
    fund(gov.state(), &addr(50), wei(3));
    (dir, gov, staking)
}

fn treasury_spend(recipient: Address, tokens: u64) -> ExecutionPayload {
    ExecutionPayload::TreasurySpend {
        recipient,
        amount: wei(tokens),
        reason: "Grant".into(),
    }
}

fn unlock(tokens: u64) -> ExecutionPayload {
    ExecutionPayload::PermanentUnlock {
        staker: addr(3),
        amount: wei(tokens),
    }
}

#[test]
fn treasury_spend_storage_fault_after_first_write_changes_nothing_and_retry_pays_once() {
    let (_dir, gov, _staking) = setup_row75();
    let (treasury, recipient) = (addr(99), addr(50));
    let id = submit_and_pass(&gov, treasury_spend(recipient, 1_000));
    gov.finalize_proposal(id, 101).unwrap();

    // The treasury debit is written before the recipient credit: fail the credit.
    assert_faulted_block_changes_nothing(&gov, CF_ACCOUNTS, recipient.as_slice(), 120, id);
    assert_eq!(balance(gov.state(), &treasury), wei(10_000));

    assert_eq!(
        gov.process_pending_proposals(121).unwrap(),
        vec![ProposalOutcome::Executed(id)]
    );
    for block in [122, 500] {
        assert!(gov.process_pending_proposals(block).unwrap().is_empty());
        assert_eq!(balance(gov.state(), &treasury), wei(9_000), "debited once");
        assert_eq!(balance(gov.state(), &recipient), wei(1_003), "paid once");
    }
    assert_eq!(gov.get_proposal(id).unwrap().unwrap().status, ProposalStatus::Executed);
}

#[test]
fn permanent_unlock_storage_fault_after_first_write_changes_nothing_and_retry_unlocks_once() {
    let (_dir, gov, staking) = setup_row75();
    let staker = addr(3);
    let id = submit_and_pass(&gov, unlock(4));
    gov.finalize_proposal(id, 101).unwrap();

    // Stake row and rewards row are written before the balance credit: fail the credit.
    assert_faulted_block_changes_nothing(&gov, CF_ACCOUNTS, staker.as_slice(), 120, id);
    assert_eq!(staking.get_permanent_stake(&staker).unwrap().unwrap().amount, wei(10));
    assert_eq!(staking.get_pending_rewards(&staker).unwrap().unwrap().amount, wei(1));

    assert_eq!(
        gov.process_pending_proposals(121).unwrap(),
        vec![ProposalOutcome::Executed(id)]
    );
    for block in [122, 500] {
        assert!(gov.process_pending_proposals(block).unwrap().is_empty());
        assert_eq!(staking.get_permanent_stake(&staker).unwrap().unwrap().amount, wei(6));
        assert!(staking.get_pending_rewards(&staker).unwrap().is_none());
        assert_eq!(balance(gov.state(), &staker), wei(5), "4 unlocked + 1 reward, once");
    }
    assert_eq!(gov.get_proposal(id).unwrap().unwrap().status, ProposalStatus::Executed);
}

/// Every executable payload kind.
fn all_payload_kinds() -> Vec<(&'static str, ExecutionPayload)> {
    vec![
        (
            "param change, price band",
            ExecutionPayload::ParameterChange {
                param_key: "price_band_bps".into(),
                new_value: "1000".into(),
            },
        ),
        (
            "param change, governance param",
            ExecutionPayload::ParameterChange {
                param_key: "quorum_bps".into(),
                new_value: "5000".into(),
            },
        ),
        ("treasury spend", treasury_spend(addr(50), 1_000)),
        ("treasury spend to the treasury", treasury_spend(addr(99), 1_000)),
        ("listing, explicit id", listing(42, "ETH")),
        ("listing, auto id", listing(0, "SOL")),
        (
            "validator registration",
            ExecutionPayload::ValidatorRegistration {
                candidate: addr(70),
            },
        ),
        ("permanent unlock, partial", unlock(4)),
        ("permanent unlock, full", unlock(10)),
    ]
}

/// The status write fails (the payload's writes would succeed): nothing of
/// the payload may land, else the still-Passed proposal re-applies it. The
/// retry leaves exactly the state of a run without the fault.
#[test]
fn status_write_fault_rolls_back_the_payload_for_every_kind() {
    for (name, payload) in all_payload_kinds() {
        let (_ref_dir, reference, _) = setup_row75();
        let ref_id = submit_and_pass(&reference, payload.clone());
        reference.finalize_proposal(ref_id, 101).unwrap();
        assert_eq!(
            reference.process_pending_proposals(121).unwrap(),
            vec![ProposalOutcome::Executed(ref_id)],
            "{name}"
        );

        let (_dir, gov, _) = setup_row75();
        let id = submit_and_pass(&gov, payload);
        gov.finalize_proposal(id, 101).unwrap();
        assert_faulted_block_changes_nothing(
            &gov,
            CF_GOVERNANCE_PROPOSALS,
            &id.to_be_bytes(),
            120,
            id,
        );
        assert_eq!(
            gov.process_pending_proposals(121).unwrap(),
            vec![ProposalOutcome::Executed(id)],
            "{name}"
        );
        assert!(gov.process_pending_proposals(122).unwrap().is_empty(), "{name}");
        assert!(
            dump_all(gov.state()) == dump_all(reference.state()),
            "{name}: the retry must apply the payload exactly once"
        );
    }
}

/// Success path of every kind, row by row: exactly these rows change
/// (unchanged by row 75; this test passes before and after it).
#[test]
fn every_payload_kind_writes_exactly_its_rows_on_success() {
    use std::collections::{BTreeMap, BTreeSet};
    type Row = (&'static str, Vec<u8>, Option<Vec<u8>>);

    let account = |a: Address, bal: U256| -> Row {
        let info = AccountInfo {
            balance: bal,
            ..Default::default()
        };
        let bytes = torus_state::db::encode_account_info(&info).to_vec();
        (CF_ACCOUNTS, a.as_slice().to_vec(), Some(bytes))
    };
    let market = |id: u64, base: &str| -> Row {
        let fields = (
            base.to_string(),
            "USDC".to_string(),
            FixedPoint::from_raw(10_000_000).raw(),
            FixedPoint::from_raw(1_000_000).raw(),
            FixedPoint::from_raw(500_000_000).raw(),
        );
        let bytes = borsh::to_vec(&fields).unwrap();
        (CF_NATIVE_MARKETS, id.to_be_bytes().to_vec(), Some(bytes))
    };
    let perm = |amount: Option<U256>| -> Row {
        let bytes = amount.map(|amount| {
            borsh::to_vec(&PermanentStakeInfo {
                staker: addr(3),
                amount,
                locked_at_block: 0,
            })
            .unwrap()
        });
        (CF_STAKING_PERMANENT, addr(3).as_slice().to_vec(), bytes)
    };
    let rewards_gone: Row = (CF_STAKING_REWARDS, addr(3).as_slice().to_vec(), None);

    for (name, payload) in all_payload_kinds() {
        let (_dir, gov, _staking) = setup_row75();
        seed_market(gov.state(), 5);
        let id = submit_and_pass(&gov, payload);
        gov.finalize_proposal(id, 101).unwrap();
        let mut proposal = gov.get_proposal(id).unwrap().unwrap();
        let params_before = gov.get_governance_params().unwrap();
        let before = dump_all(gov.state());
        assert_eq!(
            gov.process_pending_proposals(121).unwrap(),
            vec![ProposalOutcome::Executed(id)],
            "{name}"
        );

        let mut changed: Vec<Row> = Vec::new();
        for ((cf, rows_before), (_, rows_after)) in before.iter().zip(dump_all(gov.state())) {
            let b: BTreeMap<_, _> = rows_before.iter().cloned().collect();
            let a: BTreeMap<_, _> = rows_after.into_iter().collect();
            for k in b.keys().chain(a.keys()).collect::<BTreeSet<_>>() {
                if b.get(k) != a.get(k) {
                    changed.push((cf, k.clone(), a.get(k).cloned()));
                }
            }
        }

        let mut want: Vec<Row> = match name {
            "param change, price band" => {
                vec![(CF_FEE_CONFIG, b"price_band_bps".to_vec(), Some(b"1000".to_vec()))]
            }
            // s99: the whole GovernanceParams record, one field changed.
            "param change, governance param" => {
                let mut params = params_before.clone();
                params.quorum_bps = 5000;
                vec![(CF_FEE_CONFIG, b"gov_params".to_vec(), Some(borsh::to_vec(&params).unwrap()))]
            }
            "treasury spend" => vec![account(addr(50), wei(1_003)), account(addr(99), wei(9_000))],
            // Debit, then credit of the same account: the balance is unchanged.
            "treasury spend to the treasury" => vec![],
            "listing, explicit id" => vec![market(42, "ETH")],
            "listing, auto id" => vec![market(6, "SOL")],
            "validator registration" => {
                let mut key = b"validator_whitelist:".to_vec();
                key.extend_from_slice(addr(70).as_slice());
                let entry = ValidatorWhitelistEntry {
                    candidate: addr(70),
                    approved_at_block: 121,
                    expires_at_block: 121 + WHITELIST_EXPIRY_BLOCKS,
                };
                vec![(CF_CONSENSUS_META, key, Some(borsh::to_vec(&entry).unwrap()))]
            }
            "permanent unlock, partial" => vec![
                account(addr(3), wei(5)),
                perm(Some(wei(6))),
                rewards_gone.clone(),
            ],
            "permanent unlock, full" => {
                vec![account(addr(3), wei(11)), perm(None), rewards_gone.clone()]
            }
            other => unreachable!("{other}"),
        };
        // The proposal row changes in its status only.
        proposal.status = ProposalStatus::Executed;
        want.push((
            CF_GOVERNANCE_PROPOSALS,
            id.to_be_bytes().to_vec(),
            Some(borsh::to_vec(&proposal).unwrap()),
        ));
        let order = |cf: &str| ALL_CF_NAMES.iter().position(|c| *c == cf).unwrap();
        want.sort_by(|a, b| (order(a.0), &a.1).cmp(&(order(b.0), &b.1)));
        assert_eq!(changed, want, "{name}");
    }
}
