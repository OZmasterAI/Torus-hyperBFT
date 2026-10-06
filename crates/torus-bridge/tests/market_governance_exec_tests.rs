//! Executor-level tests for market listing / governance wiring (parity audit BUG 4).
//!
//! (a) Direct admin actions (`ListMarket`, `DelistMarket`, `UpdateMarketParams`)
//!     have no authority check, so they must be rejected — markets change only
//!     through a governance proposal.
//! (b) `SubmitProposal(ListMarket)` must map the listing's margin fields
//!     correctly into the governance `ExecutionPayload`.

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_economics::governance::{ExecutionPayload, GovernanceParams};
use torus_economics::MIN_SELF_DELEGATION;
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::{StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketListing, MarketParams, NativeAction, Proposal, ProposalAction,
};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn make_ctx(db: &StateDb) -> NativeExecContext {
    NativeExecContext::new(
        db.clone(),
        1,         // block_height
        1000,      // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
        addr(100), // treasury
        addr(101), // dev_pool
    )
}

fn markets_snapshot(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.iterate_cf(CF_NATIVE_MARKETS, None).unwrap()
}

fn sample_listing(max_leverage: u32, maintenance_margin_bps: u32) -> MarketListing {
    MarketListing {
        base_asset: "ETH".into(),
        quote_asset: "USDC".into(),
        tick_size: FixedPoint::from_raw(1_000_000),
        lot_size: FixedPoint::from_raw(10_000_000),
        max_leverage,
        maintenance_margin_bps,
    }
}

#[test]
fn direct_admin_market_actions_are_rejected_and_change_nothing() {
    let (_dir, db) = open_test_db();
    db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), b"genesis-market")
        .unwrap();
    let before = markets_snapshot(&db);

    let actions = [
        NativeAction::ListMarket(sample_listing(20, 300)),
        NativeAction::DelistMarket { market_id: 1 },
        NativeAction::UpdateMarketParams {
            market_id: 1,
            params: MarketParams {
                tick_size: FixedPoint::ONE,
                lot_size: FixedPoint::ONE,
                max_leverage: 10,
                maintenance_margin_bps: 500,
                max_funding_rate_bps: 100,
            },
        },
    ];
    let mut ctx = make_ctx(&db);
    for action in &actions {
        let r = NativeExecutor::execute(&mut ctx, &addr(7), action);
        assert!(!r.success, "{} must be rejected", r.action_type);
        assert!(
            r.error.as_deref().unwrap_or("").contains("governance-only"),
            "unexpected error for {}: {:?}",
            r.action_type,
            r.error
        );
    }
    assert_eq!(markets_snapshot(&db), before, "CF_NATIVE_MARKETS unchanged");
}

/// Proposer with enough stake to submit governance proposals.
fn setup_proposer(db: &StateDb) -> Address {
    let ctx = make_ctx(db);
    ctx.governance
        .set_governance_params(&GovernanceParams {
            voting_period_blocks: 100,
            quorum_bps: 3300,
            min_proposal_stake: wei(100),
            permanent_weight_multiplier_num: 3,
            permanent_weight_multiplier_den: 2,
            treasury_address: addr(100),
            timelock_blocks: 10,
            permanent_unlock_threshold_bps: 8000,
        })
        .unwrap();
    let val = addr(1);
    db.put_account(
        &val,
        &AccountInfo {
            balance: MIN_SELF_DELEGATION,
            ..Default::default()
        },
    )
    .unwrap();
    ctx.staking
        .register_validator(val, [1; 32], 500, MIN_SELF_DELEGATION)
        .unwrap();
    let proposer = addr(2);
    db.put_account(
        &proposer,
        &AccountInfo {
            balance: wei(500),
            ..Default::default()
        },
    )
    .unwrap();
    ctx.staking.delegate(proposer, val, wei(500)).unwrap();
    proposer
}

fn submit_listing(db: &StateDb, proposer: &Address, listing: MarketListing) -> bool {
    let mut ctx = make_ctx(db);
    let r = NativeExecutor::execute(
        &mut ctx,
        proposer,
        &NativeAction::SubmitProposal(Proposal {
            title: "List ETH".into(),
            description: "d".into(),
            action: ProposalAction::ListMarket(listing),
        }),
    );
    r.success
}

#[test]
fn submit_listing_maps_initial_margin_from_max_leverage() {
    let (_dir, db) = open_test_db();
    let proposer = setup_proposer(&db);

    // 20x max leverage => 5% initial margin, stored in the genesis convention
    // (percent, FixedPoint: "5.0"). maintenance_margin_bps (300) must NOT leak
    // into the initial-margin field.
    assert!(submit_listing(&db, &proposer, sample_listing(20, 300)));
    let ctx = make_ctx(&db);
    let p = ctx.governance.get_proposal(1).unwrap().unwrap();
    match p.execution_payload {
        Some(ExecutionPayload::MarketListing {
            market_id,
            initial_margin,
            ..
        }) => {
            assert_eq!(market_id, 0, "id is assigned at execution time");
            assert_eq!(initial_margin, FixedPoint::from_raw(5 * FixedPoint::SCALE));
        }
        other => panic!("expected MarketListing payload, got {other:?}"),
    }
}

#[test]
fn submit_listing_rejects_zero_max_leverage() {
    let (_dir, db) = open_test_db();
    let proposer = setup_proposer(&db);
    assert!(!submit_listing(&db, &proposer, sample_listing(0, 300)));
    let ctx = make_ctx(&db);
    assert!(ctx.governance.get_proposal(1).unwrap().is_none());
}

// ----------------------------------------------------------------------------
// Row 43: tick_size > 0 and lot_size > 0 (consensus). Books are built from the
// market row; a lot of 0 accepts zero-quantity orders, a tick of 0 turns the
// tick check off.
// ----------------------------------------------------------------------------

fn listing_tick_lot(tick_raw: i128, lot_raw: i128) -> MarketListing {
    MarketListing {
        tick_size: FixedPoint::from_raw(tick_raw),
        lot_size: FixedPoint::from_raw(lot_raw),
        ..sample_listing(20, 300)
    }
}

#[test]
fn submit_listing_rejects_zero_tick_or_lot() {
    let (_dir, db) = open_test_db();
    let proposer = setup_proposer(&db);
    for (tick, lot, field) in [
        (0, 1_000_000, "tick_size"),
        (1_000_000, 0, "lot_size"),
        (-1_000_000, 1_000_000, "tick_size"),
    ] {
        let mut ctx = make_ctx(&db);
        let r = NativeExecutor::execute(
            &mut ctx,
            &proposer,
            &NativeAction::SubmitProposal(Proposal {
                title: "List ETH".into(),
                description: "d".into(),
                action: ProposalAction::ListMarket(listing_tick_lot(tick, lot)),
            }),
        );
        assert!(!r.success, "tick {tick} lot {lot} must be refused");
        assert_eq!(
            r.error.as_deref(),
            Some(format!("market listing {field} must be > 0").as_str())
        );
    }
    let ctx = make_ctx(&db);
    assert!(ctx.governance.get_proposal(1).unwrap().is_none(), "nothing stored");
    // Both > 0 is accepted, and gets proposal id 1 (refusals consumed none).
    assert!(submit_listing(&db, &proposer, listing_tick_lot(1, 1)));
    assert!(make_ctx(&db).governance.get_proposal(1).unwrap().is_some());
}

#[test]
fn direct_list_market_with_zero_tick_or_lot_is_rejected_and_changes_nothing() {
    let (_dir, db) = open_test_db();
    let before = markets_snapshot(&db);
    let mut ctx = make_ctx(&db);
    for l in [listing_tick_lot(0, 1), listing_tick_lot(1, 0)] {
        let r = NativeExecutor::execute(&mut ctx, &addr(7), &NativeAction::ListMarket(l));
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap_or("").contains("governance-only"));
    }
    assert_eq!(markets_snapshot(&db), before, "CF_NATIVE_MARKETS unchanged");
}

/// A bad listing that reaches execution anyway (stored payload overwritten,
/// bypassing submission) is not applied: the per-block governance step records
/// the error and writes no market row.
#[test]
fn governance_execution_of_zero_tick_or_lot_listing_writes_no_market() {
    use torus_state::cf::CF_GOVERNANCE_PROPOSALS;
    for (tick, lot, field) in [(0, 1, "tick_size"), (1, 0, "lot_size")] {
        let (_dir, db) = open_test_db();
        let proposer = setup_proposer(&db);
        assert!(submit_listing(&db, &proposer, sample_listing(20, 300)));
        let mut ctx = make_ctx(&db);
        ctx.governance.cast_vote(proposer, 1, true, 10).unwrap();
        ctx.governance.finalize_proposal(1, 102).unwrap();
        let mut p = ctx.governance.get_proposal(1).unwrap().unwrap();
        match p.execution_payload.as_mut() {
            Some(ExecutionPayload::MarketListing {
                tick_size, lot_size, ..
            }) => {
                *tick_size = FixedPoint::from_raw(tick);
                *lot_size = FixedPoint::from_raw(lot);
            }
            other => panic!("expected MarketListing payload, got {other:?}"),
        }
        db.put_cf_raw(CF_GOVERNANCE_PROPOSALS, &1u64.to_be_bytes(), &borsh::to_vec(&p).unwrap())
            .unwrap();
        let before = markets_snapshot(&db);

        ctx.block_height = 200; // past the 10-block timelock
        let results = NativeExecutor::process_governance(&mut ctx);
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert_eq!(results[0].action_type, "governance_process");
        assert_eq!(
            results[0].error.as_deref(),
            Some(format!("market listing {field} must be > 0").as_str())
        );
        assert_eq!(markets_snapshot(&db), before, "no market row ({field})");
    }
}
