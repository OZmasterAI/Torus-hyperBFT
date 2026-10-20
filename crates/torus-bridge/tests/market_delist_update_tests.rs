//! Pre-testnet P1 (roadmap P1 item 2): governance `DelistMarket` and
//! `UpdateMarketParams` take effect when their proposal executes (they were
//! text-only: nothing happened). Consensus change, genesis-bound.
//!
//! Delist (the minimal safe wind-down; the roadmap names no settlement rule):
//! the market goes reduce-only (new orders that are not reduce-only are
//! rejected, `MarketDelisted`), every resting order and pending stop in it is
//! cancelled with its reserved margin refunded, and open positions are left
//! as they are: holders close them with reduce-only orders, liquidation keeps
//! running (the market stays in the registry, so it keeps its oracle mark).
//! How remaining positions are settled is an open owner question (roadmap P1).
//!
//! UpdateMarketParams: tick, lot and max leverage (initial margin = 1 / max
//! leverage, as for listings) are validated and written to the market row in
//! the proposal's atomic write at its execution height. When tick, lot or
//! max leverage changes, every resting order and pending stop in the market
//! is first cancelled and refunded under the old config (Codex P1: a
//! reservation is never re-priced under a config it was not placed with);
//! then the live book takes the new tick / lot and the margin config follows
//! the row.
//!
//! The direct (non-governance) actions stay rejected.

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_economics::governance::{GovernanceParams, ProposalStatus};
use torus_economics::MIN_SELF_DELEGATION;
use torus_state::action_status::FailureReason;
use torus_state::cf::{ALL_CF_NAMES, CF_NATIVE_MARKETS};
use torus_state::{StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, MarketParams, NativeAction, OrderType, PlaceOrderParams, Proposal,
    ProposalAction, TimeInForce,
};

const M: MarketId = 1;
const OTHER: MarketId = 2;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

/// A market row in the genesis / governance layout.
fn market_row(lot: i128, tick: i128, im: i128) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USD".to_string(), lot, tick, im)).unwrap()
}

fn decoded_row(db: &StateDb, m: MarketId) -> (String, String, i128, i128, i128) {
    let row = db
        .get_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes())
        .unwrap()
        .unwrap();
    borsh::from_slice(&row).unwrap()
}

fn ctx_at(db: &StateDb, height: u64) -> NativeExecContext<StateDb> {
    NativeExecContext::new(
        db.clone(),
        height,
        1_000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

const PROPOSER: u8 = 2;
const TRADERS: [u8; 6] = [10, 11, 12, 13, 14, 15];

/// Markets 1 and 2 listed (tick 1, lot 1, 20x), governance params with a
/// 10-block timelock, a proposer with stake, traders funded.
fn setup() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let s = FixedPoint::SCALE;
    for m in [M, OTHER] {
        db.put_cf_raw(
            CF_NATIVE_MARKETS,
            &m.to_be_bytes(),
            &market_row(s, s, 5 * s),
        )
        .unwrap();
    }
    let ctx = ctx_at(&db, 1);
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
    db.put_account(
        &addr(PROPOSER),
        &AccountInfo {
            balance: wei(500),
            ..Default::default()
        },
    )
    .unwrap();
    ctx.staking.delegate(addr(PROPOSER), val, wei(500)).unwrap();
    for t in TRADERS {
        ctx.positions
            .put_native_balance(
                &addr(t),
                &NativeBalance {
                    available: fp(1_000_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
    }
    (dir, db)
}

fn limit(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(t: u8, p: PlaceOrderParams) -> (Address, NativeAction) {
    (addr(t), NativeAction::PlaceOrder(p))
}

/// Executes `actions` as one block at `height` on `threads` engine workers
/// (1 = serial) and saves the books.
fn block(
    db: &StateDb,
    height: u64,
    threads: usize,
    actions: &[(Address, NativeAction)],
) -> Vec<NativeActionResult> {
    let mut ctx = ctx_at(db, height);
    let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, actions, threads).results;
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    r
}

fn submit(db: &StateDb, height: u64, title: &str, action: ProposalAction) -> NativeActionResult {
    let mut ctx = ctx_at(db, height);
    NativeExecutor::execute(
        &mut ctx,
        &addr(PROPOSER),
        &NativeAction::SubmitProposal(Proposal {
            title: title.into(),
            description: "d".into(),
            action,
        }),
    )
}

/// Votes proposal `id` through and runs the governance step at height 200
/// (past the vote and the timelock), on a context that loaded the books.
fn pass_and_execute(db: &StateDb, id: u64) -> Vec<NativeActionResult> {
    let mut ctx = ctx_at(db, 200);
    ctx.governance
        .cast_vote(addr(PROPOSER), id, true, 10)
        .unwrap();
    ctx.governance.finalize_proposal(id, 104).unwrap();
    let r = NativeExecutor::process_governance(&mut ctx);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    r
}

fn status(db: &StateDb, id: u64) -> ProposalStatus {
    ctx_at(db, 1)
        .governance
        .get_proposal(id)
        .unwrap()
        .unwrap()
        .status
}

fn bal(db: &StateDb, t: u8) -> NativeBalance {
    ctx_at(db, 1)
        .positions
        .get_native_balance(&addr(t))
        .unwrap()
}

fn signed_pos(db: &StateDb, t: u8, m: MarketId) -> FixedPoint {
    match ctx_at(db, 1).positions.get_position(&addr(t), m).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

fn dump(db: &StateDb) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in ALL_CF_NAMES {
        for (k, v) in db.iterate_cf(cf, None).unwrap() {
            out.push((*cf, k, v));
        }
    }
    out
}

/// Resting orders on both sides, a pending stop and a position pair in M; a
/// resting order of trader 10 in the other market.
fn seed_book(db: &StateDb, threads: usize) {
    let stop = PlaceOrderParams {
        order_type: OrderType::StopLimit {
            trigger: fp(120),
            limit: fp(121),
        },
        ..limit(M, true, 121, 1)
    };
    let r = block(
        db,
        2,
        threads,
        &[
            place(10, limit(M, true, 99, 2)),
            place(11, limit(M, false, 110, 3)),
            place(12, stop),
            place(13, limit(M, false, 100, 1)),
            place(14, limit(M, true, 100, 1)), // fills 13: 14 long 1, 13 short 1
            place(10, limit(OTHER, true, 50, 1)),
        ],
    );
    assert!(r.iter().all(|r| r.success), "{r:?}");
    assert_eq!(signed_pos(db, 14, M), fp(1));
    assert_eq!(signed_pos(db, 13, M), fp(-1));
}

/// The delist scenario on `threads` engine workers; returns the final DB dump.
fn delist_scenario(threads: usize) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let (_d, db) = setup();
    seed_book(&db, threads);
    for t in [10, 11, 12] {
        assert!(
            bal(&db, t).order_margin > FixedPoint::ZERO,
            "trader {t} holds a reservation"
        );
    }
    let other_margin = {
        // Trader 10's reservation for its market-2 bid alone.
        let (_d2, db2) = setup();
        block(&db2, 2, 1, &[place(10, limit(OTHER, true, 50, 1))]);
        bal(&db2, 10).order_margin
    };

    let r = submit(
        &db,
        3,
        "Delist BTC",
        ProposalAction::DelistMarket { market_id: M },
    );
    assert!(r.success, "{r:?}");
    // Passed but not executed yet: the market still trades normally.
    let r = block(&db, 4, threads, &[place(15, limit(M, true, 98, 1))]);
    assert!(r[0].success, "before execution: {r:?}");
    let r = block(
        &db,
        5,
        threads,
        &[(
            addr(15),
            NativeAction::CancelAllOrders { market_id: Some(M) },
        )],
    );
    assert!(r[0].success);

    let results = pass_and_execute(&db, 1);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(results[0].success, "{results:?}");
    assert_eq!(status(&db, 1), ProposalStatus::Executed);

    let ctx = ctx_at(&db, 201);
    let book = &ctx.order_books()[&M];
    assert_eq!(book.order_count(), 0, "every resting order cancelled");
    assert!(
        book.pending_stops().is_empty(),
        "every pending stop cancelled"
    );
    assert_eq!(
        ctx.order_books()[&OTHER].order_count(),
        1,
        "the other market is untouched"
    );
    drop(ctx);
    assert!(
        db.get_cf_raw(torus_state::cf::CF_NATIVE_BBO, &M.to_be_bytes())
            .unwrap()
            .is_none(),
        "the emptied book's bbo row is deleted"
    );
    // Reservations refunded: 11 and 12 hold nothing, 10 only its market-2 bid.
    for t in [11, 12] {
        let b = bal(&db, t);
        assert_eq!(b.order_margin, FixedPoint::ZERO, "trader {t}");
        assert_eq!(b.available, fp(1_000_000), "trader {t}: refunded in full");
    }
    assert_eq!(
        bal(&db, 10).order_margin,
        other_margin,
        "trader 10 keeps its market-2 reservation"
    );
    // Positions are left as they are (settlement: open owner question).
    assert_eq!(signed_pos(&db, 14, M), fp(1));
    assert_eq!(signed_pos(&db, 13, M), fp(-1));

    // Reduce-only from now on: new exposure is rejected, on every path...
    let r = block(
        &db,
        201,
        threads,
        &[
            place(10, limit(M, true, 99, 1)),
            place(
                15,
                PlaceOrderParams {
                    order_type: OrderType::Market,
                    time_in_force: TimeInForce::IOC,
                    ..limit(M, true, 200, 1)
                },
            ),
            place(10, limit(OTHER, true, 49, 1)),
        ],
    );
    for i in [0, 1] {
        assert!(!r[i].success, "{r:?}");
        assert_eq!(r[i].reason, FailureReason::MarketDelisted, "{r:?}");
        assert_eq!(
            r[i].error.as_deref(),
            Some("market 1 is delisted: only reduce-only orders")
        );
        assert_eq!(r[i].gas_used, 0);
    }
    assert!(r[2].success, "the other market trades: {r:?}");
    // ...and the holders close their positions with reduce-only orders.
    let r = block(
        &db,
        202,
        threads,
        &[
            place(
                14,
                PlaceOrderParams {
                    reduce_only: true,
                    ..limit(M, false, 100, 1)
                },
            ),
            place(
                13,
                PlaceOrderParams {
                    reduce_only: true,
                    ..limit(M, true, 100, 1)
                },
            ),
        ],
    );
    assert!(r.iter().all(|r| r.success), "{r:?}");
    assert_eq!(signed_pos(&db, 14, M), FixedPoint::ZERO);
    assert_eq!(signed_pos(&db, 13, M), FixedPoint::ZERO);
    let single = {
        let mut ctx = ctx_at(&db, 203);
        NativeExecutor::execute(
            &mut ctx,
            &addr(10),
            &NativeAction::PlaceOrder(limit(M, false, 130, 1)),
        )
    };
    assert_eq!(
        single.reason,
        FailureReason::MarketDelisted,
        "single path: {single:?}"
    );
    dump(&db)
}

#[test]
fn governance_delist_cancels_orders_refunds_margin_and_goes_reduce_only() {
    delist_scenario(1);
}

/// Same chain, serial vs 4 engine workers: byte-identical state.
#[test]
fn governance_delist_is_deterministic_across_exec_paths() {
    assert_eq!(delist_scenario(1), delist_scenario(4));
}

#[test]
fn governance_update_market_params_applies_at_execution() {
    let (_d, db) = setup();
    let s = FixedPoint::SCALE;
    let r = block(
        &db,
        2,
        1,
        &[
            place(10, limit(M, true, 99, 1)),
            place(11, limit(M, false, 101, 3)),
        ],
    );
    assert!(r.iter().all(|r| r.success), "{r:?}");
    let params = MarketParams {
        tick_size: fp(2),
        lot_size: fp(2),
        max_leverage: 10,
        maintenance_margin_bps: 500,
        max_funding_rate_bps: 100,
    };
    let r = submit(
        &db,
        3,
        "BTC params",
        ProposalAction::UpdateMarketParams {
            market_id: M,
            params,
        },
    );
    assert!(r.success, "{r:?}");
    // Not executed yet: the old tick and lot hold.
    let r = block(&db, 4, 1, &[place(12, limit(M, true, 97, 1))]);
    assert!(r[0].success, "{r:?}");
    assert_eq!(
        decoded_row(&db, M),
        ("BTC".into(), "USD".into(), s, s, 5 * s)
    );

    let results = pass_and_execute(&db, 1);
    assert!(results.iter().all(|r| r.success), "{results:?}");
    assert_eq!(status(&db, 1), ProposalStatus::Executed);
    // The row: lot 2, tick 2, initial margin 10% (10x); names kept.
    assert_eq!(
        decoded_row(&db, M),
        ("BTC".into(), "USD".into(), 2 * s, 2 * s, 10 * s)
    );
    let ctx = ctx_at(&db, 201);
    let book = &ctx.order_books()[&M];
    assert_eq!(
        (book.tick_size, book.lot_size),
        (fp(2), fp(2)),
        "the live book took them (persisted)"
    );
    // Codex P1: the tick / lot / leverage change cancelled the resting
    // orders and refunded them under the old config.
    assert_eq!(book.order_count(), 0, "resting orders cancelled");
    for t in [10, 11, 12] {
        assert_eq!(
            bal(&db, t).order_margin,
            FixedPoint::ZERO,
            "trader {t} refunded"
        );
    }
    assert_eq!(ctx.margin_configs[&M].max_leverage, 10);
    assert_eq!(
        ctx.margin_configs[&OTHER].max_leverage, 20,
        "other market untouched"
    );
    drop(ctx);
    let r = block(
        &db,
        201,
        1,
        &[
            place(13, limit(M, true, 95, 2)), // off the new tick 2
            place(13, limit(M, true, 94, 1)), // below the new lot 2
            place(13, limit(M, true, 94, 2)),
        ],
    );
    assert_eq!(r[0].reason, FailureReason::Tick, "{r:?}");
    assert_eq!(r[1].reason, FailureReason::Lot, "{r:?}");
    assert!(r[2].success, "{r:?}");
}

/// Proposals that cannot apply: refused at submission when the market is not
/// listed or the params are invalid; at execution a second delist and an
/// update of a delisted market fail (status `Failed`, nothing written).
#[test]
fn invalid_market_proposals_are_refused_or_fail() {
    let (_d, db) = setup();
    let good = MarketParams {
        tick_size: fp(1),
        lot_size: fp(1),
        max_leverage: 10,
        maintenance_margin_bps: 500,
        max_funding_rate_bps: 100,
    };
    for (action, want) in [
        (
            ProposalAction::DelistMarket { market_id: 9 },
            "market 9 is not listed",
        ),
        (
            ProposalAction::UpdateMarketParams {
                market_id: 9,
                params: good.clone(),
            },
            "market 9 is not listed",
        ),
        (
            ProposalAction::UpdateMarketParams {
                market_id: M,
                params: MarketParams {
                    max_leverage: 0,
                    ..good.clone()
                },
            },
            "market params max_leverage must be > 0",
        ),
        (
            ProposalAction::UpdateMarketParams {
                market_id: M,
                params: MarketParams {
                    tick_size: FixedPoint::ZERO,
                    ..good.clone()
                },
            },
            "market listing tick_size must be > 0",
        ),
    ] {
        let r = submit(&db, 3, "bad", action);
        assert!(!r.success, "{r:?}");
        assert_eq!(r.error.as_deref(), Some(want));
    }
    assert!(
        ctx_at(&db, 1).governance.get_proposal(1).unwrap().is_none(),
        "nothing stored"
    );

    for (title, action) in [
        ("delist", ProposalAction::DelistMarket { market_id: M }),
        (
            "delist again",
            ProposalAction::DelistMarket { market_id: M },
        ),
        (
            "update",
            ProposalAction::UpdateMarketParams {
                market_id: M,
                params: good,
            },
        ),
    ] {
        assert!(submit(&db, 3, title, action).success);
    }
    let mut ctx = ctx_at(&db, 200);
    for id in 1..=3 {
        ctx.governance
            .cast_vote(addr(PROPOSER), id, true, 10)
            .unwrap();
        ctx.governance.finalize_proposal(id, 104).unwrap();
    }
    let results = NativeExecutor::process_governance(&mut ctx);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(results[0].success, "{results:?}");
    assert_eq!(
        results[1].error.as_deref(),
        Some("proposal 2 execution failed: market 1 is delisted")
    );
    assert_eq!(
        results[2].error.as_deref(),
        Some("proposal 3 execution failed: market 1 is delisted")
    );
    drop(ctx);
    assert_eq!(status(&db, 1), ProposalStatus::Executed);
    assert_eq!(status(&db, 2), ProposalStatus::Failed);
    assert_eq!(status(&db, 3), ProposalStatus::Failed);
    let s = FixedPoint::SCALE;
    assert_eq!(
        decoded_row(&db, M),
        ("BTC".into(), "USD".into(), s, s, 5 * s),
        "the failed update wrote nothing"
    );
}

/// The direct (non-governance) actions are still rejected and change nothing.
#[test]
fn direct_delist_and_update_actions_stay_rejected() {
    let (_d, db) = setup();
    let before = dump(&db);
    let mut ctx = ctx_at(&db, 2);
    for action in [
        NativeAction::DelistMarket { market_id: M },
        NativeAction::UpdateMarketParams {
            market_id: M,
            params: MarketParams {
                tick_size: fp(2),
                lot_size: fp(2),
                max_leverage: 10,
                maintenance_margin_bps: 500,
                max_funding_rate_bps: 100,
            },
        },
    ] {
        let r = NativeExecutor::execute(&mut ctx, &addr(10), &action);
        assert!(!r.success);
        assert_eq!(
            r.error.as_deref(),
            Some("governance-only: submit a proposal")
        );
    }
    drop(ctx);
    assert_eq!(dump(&db), before);
}

// ---------------------------------------------------------------------------
// Codex P1 (pre-testnet review): a params update that changes tick, lot or
// max leverage first cancels every resting order and pending stop in the
// market and refunds each reservation under the OLD config (the delist
// cancel / refund path), then applies the new params. Before, resting
// orders kept their reservations while later cancels refunded at the new
// leverage: lowering it released other orders' collateral, raising it
// stranded collateral.
// ---------------------------------------------------------------------------

fn fp_ratio(num: i128, den: i128) -> FixedPoint {
    FixedPoint::from_raw(num * FixedPoint::SCALE / den)
}

fn params(tick: i64, lot: i64, max_leverage: u32) -> MarketParams {
    MarketParams {
        tick_size: fp(tick),
        lot_size: fp(lot),
        max_leverage,
        maintenance_margin_bps: 500,
        max_funding_rate_bps: 100,
    }
}

/// Submits and executes an update of M (proposal 1 on a fresh setup).
fn update_m(db: &StateDb, p: MarketParams) {
    let r = submit(
        db,
        3,
        "BTC params",
        ProposalAction::UpdateMarketParams {
            market_id: M,
            params: p,
        },
    );
    assert!(r.success, "{r:?}");
    let results = pass_and_execute(db, 1);
    assert!(results.iter().all(|r| r.success), "{results:?}");
    assert_eq!(status(db, 1), ProposalStatus::Executed);
}

/// Resting orders in M (two traders), a pending stop in M and a resting
/// order of trader 10 in the other market, all at 20x and unfilled.
fn seed_resting(db: &StateDb) {
    let stop = PlaceOrderParams {
        order_type: OrderType::StopLimit {
            trigger: fp(120),
            limit: fp(121),
        },
        ..limit(M, true, 121, 1)
    };
    let r = block(
        db,
        2,
        1,
        &[
            place(10, limit(M, true, 100, 2)),    // 200 / 20 = 10
            place(10, limit(OTHER, true, 50, 1)), // 50 / 20 = 2.5
            place(11, limit(M, false, 110, 3)),   // 330 / 20 = 16.5
            place(12, stop),
        ],
    );
    assert!(r.iter().all(|r| r.success), "{r:?}");
    assert_eq!(bal(db, 10).order_margin, fp_ratio(25, 2));
    assert_eq!(bal(db, 11).order_margin, fp_ratio(33, 2));
    assert!(bal(db, 12).order_margin > FixedPoint::ZERO);
}

/// No fill happened: every trader's collateral is where it started.
fn assert_collateral_conserved(db: &StateDb) {
    for t in TRADERS {
        let b = bal(db, t);
        assert_eq!(
            b.available + b.order_margin,
            fp(1_000_000),
            "trader {t}: collateral conserved ({b:?})"
        );
    }
}

fn leverage_update_refunds_exactly(new_leverage: u32) {
    let (_d, db) = setup();
    seed_resting(&db);
    update_m(&db, params(1, 1, new_leverage));

    let ctx = ctx_at(&db, 201);
    assert_eq!(ctx.margin_configs[&M].max_leverage, new_leverage);
    let book = &ctx.order_books()[&M];
    assert_eq!(book.order_count(), 0, "resting orders cancelled");
    assert!(book.pending_stops().is_empty(), "pending stops cancelled");
    assert_eq!(
        ctx.order_books()[&OTHER].order_count(),
        1,
        "other market untouched"
    );
    drop(ctx);
    // Refunded at the reservation they were placed with (20x), exactly.
    assert_eq!(
        bal(&db, 10).order_margin,
        fp_ratio(5, 2),
        "only the other market's 2.5"
    );
    for t in [11, 12] {
        assert_eq!(bal(&db, t).order_margin, FixedPoint::ZERO, "trader {t}");
    }
    assert_collateral_conserved(&db);

    // Later cancels release exactly what is still reserved.
    let r = block(
        &db,
        201,
        1,
        &[(addr(10), NativeAction::CancelAllOrders { market_id: None })],
    );
    assert!(r[0].success, "{r:?}");
    assert_eq!(bal(&db, 10).order_margin, FixedPoint::ZERO);
    assert_eq!(bal(&db, 10).available, fp(1_000_000));

    // New orders reserve at the new leverage and release the same on cancel.
    let r = block(&db, 202, 1, &[place(11, limit(M, false, 110, 1))]);
    assert!(r[0].success, "{r:?}");
    assert_eq!(
        bal(&db, 11).order_margin,
        fp_ratio(110, new_leverage as i128)
    );
    let r = block(
        &db,
        203,
        1,
        &[(
            addr(11),
            NativeAction::CancelAllOrders { market_id: Some(M) },
        )],
    );
    assert!(r[0].success, "{r:?}");
    assert_eq!(bal(&db, 11).order_margin, FixedPoint::ZERO);
    assert_collateral_conserved(&db);
}

#[test]
fn leverage_20_to_10_cancels_and_refunds_under_the_old_config() {
    leverage_update_refunds_exactly(10);
}

#[test]
fn leverage_20_to_40_cancels_and_refunds_under_the_old_config() {
    leverage_update_refunds_exactly(40);
}

/// A tick change with resting orders: cancelled and refunded; the emptied
/// book takes the new tick.
#[test]
fn tick_change_cancels_resting_orders_and_refunds() {
    let (_d, db) = setup();
    seed_resting(&db);
    update_m(&db, params(2, 1, 20));
    let ctx = ctx_at(&db, 201);
    let book = &ctx.order_books()[&M];
    assert_eq!((book.tick_size, book.order_count()), (fp(2), 0));
    assert!(book.pending_stops().is_empty());
    drop(ctx);
    assert_eq!(bal(&db, 10).order_margin, fp_ratio(5, 2));
    assert_collateral_conserved(&db);
    let r = block(
        &db,
        201,
        1,
        &[
            place(13, limit(M, true, 99, 1)),
            place(13, limit(M, true, 98, 1)),
        ],
    );
    assert_eq!(r[0].reason, FailureReason::Tick, "{r:?}");
    assert!(r[1].success, "{r:?}");
}

/// An update that changes none of tick, lot and max leverage leaves the
/// resting orders and their reservations alone.
#[test]
fn unchanged_params_keep_resting_orders() {
    let (_d, db) = setup();
    seed_resting(&db);
    update_m(&db, params(1, 1, 20));
    let ctx = ctx_at(&db, 201);
    assert_eq!(ctx.order_books()[&M].order_count(), 2);
    assert_eq!(ctx.order_books()[&M].pending_stops().len(), 1);
    drop(ctx);
    assert_eq!(bal(&db, 10).order_margin, fp_ratio(25, 2));
    assert_eq!(bal(&db, 11).order_margin, fp_ratio(33, 2));
}
