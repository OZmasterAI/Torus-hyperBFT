//! C1 — execute_batch settlement through PositionCache + BalanceCache.
//!
//! Phase 4 no longer does per-fill overlay read-modify-writes for positions,
//! and no longer flush+evicts balances before every fill: both rows live in
//! write-back caches during the batch and flush once, in sorted-key order, at
//! batch end. These tests pin the correctness bar: final CF_NATIVE_BALANCES /
//! CF_NATIVE_POSITIONS contents must be byte-identical to the classic
//! sequential (uncached) execution path, and a realized-PnL credit must
//! survive a later margin release for the same trader within one batch (the
//! invariant the old per-fill flush_and_evict protected).

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers (mirrors parallel_matching_tests.rs) ----

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
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

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn limit(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(sender: Address, params: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(params))
}

fn dump_cf(db: &StateDb, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, cf, None).unwrap()
}

// ============================================================================
// Differential: batched (cached) settlement == sequential (uncached) path
// ============================================================================

/// Fills across multiple markets and parties — opens, full closes with PnL
/// on both sides, and a maker fill against an order resting from an earlier
/// batch — must leave byte-identical balances/positions vs. running the same
/// actions one-by-one through the classic `execute` path (which does direct
/// per-fill overlay read-modify-writes and immediate PnL credits).
#[test]
fn batched_settlement_state_matches_sequential_reference() {
    let a = addr(1);
    let b = addr(2);
    let c = addr(3);
    let d = addr(4);
    let e = addr(5);

    // Batch 1: open positions on two markets + leave a resting bid on m1.
    let batch1: Vec<(Address, NativeAction)> = vec![
        place(a, limit(1, true, 100, 5)),  // rests
        place(b, limit(1, false, 100, 5)), // fills vs A @100
        place(c, limit(2, false, 200, 3)), // rests
        place(d, limit(2, true, 200, 3)),  // fills vs C @200
        place(a, limit(1, true, 101, 2)),  // rests (maker for batch 2)
    ];

    // Batch 2: close everything with PnL on both sides + partial-ish flow.
    let batch2: Vec<(Address, NativeAction)> = vec![
        place(b, limit(1, true, 110, 5)),  // rests
        place(a, limit(1, false, 105, 5)), // fills vs B @110: A +50, B -50
        place(d, limit(2, false, 190, 3)), // rests
        place(c, limit(2, true, 190, 3)),  // fills vs D @190: C +30, D -30
        place(e, limit(1, false, 101, 2)), // fills vs A's old resting bid @101
    ];

    // --- Batched execution (Phase 2-4 pipeline with caches) ---
    let (_dir1, db1) = open_test_db();
    let mut ctx1 = make_ctx(db1.clone());
    for t in [a, b, c, d, e] {
        fund_native(&ctx1, &t, fp(1_000_000));
    }
    let r1 = NativeExecutor::execute_batch(&mut ctx1, &batch1);
    assert!(r1.results.iter().all(|r| r.success), "batch1: {:?}", r1.results);
    let r2 = NativeExecutor::execute_batch(&mut ctx1, &batch2);
    assert!(r2.results.iter().all(|r| r.success), "batch2: {:?}", r2.results);

    // --- Sequential reference (classic uncached per-action path) ---
    let (_dir2, db2) = open_test_db();
    let mut ctx2 = make_ctx(db2.clone());
    for t in [a, b, c, d, e] {
        fund_native(&ctx2, &t, fp(1_000_000));
    }
    for (sender, action) in batch1.iter().chain(batch2.iter()) {
        let r = NativeExecutor::execute(&mut ctx2, sender, action);
        assert!(r.success, "sequential action failed: {:?}", r.error);
    }

    assert_eq!(
        dump_cf(&db1, CF_NATIVE_POSITIONS),
        dump_cf(&db2, CF_NATIVE_POSITIONS),
        "positions diverged: batched-cached vs sequential"
    );
    assert_eq!(
        dump_cf(&db1, CF_NATIVE_BALANCES),
        dump_cf(&db2, CF_NATIVE_BALANCES),
        "balances diverged: batched-cached vs sequential"
    );
}

// ============================================================================
// Invariant: PnL credit survives a later same-batch margin release
// ============================================================================

/// The old code flush+evicted both parties' balances before every fill so the
/// direct-to-overlay PnL credit could not be clobbered by a later BalanceCache
/// flush. With the credit now routed THROUGH the balance cache, the same
/// trader can take a PnL credit (order i) and a partial-fill margin release
/// (order j > i) in one batch and both must land. Exact-value check.
#[test]
fn pnl_credit_survives_later_margin_release_in_same_batch() {
    let a = addr(1);
    let b = addr(2);
    let c = addr(3);

    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    fund_native(&ctx, &a, fp(10_000));
    fund_native(&ctx, &b, fp(10_000));
    fund_native(&ctx, &c, fp(10_000));

    // Setup batch: A ends up short 1 @ 100 with a clean balance
    // (reserve fully released on the full fill).
    let setup: Vec<(Address, NativeAction)> = vec![
        place(b, limit(1, true, 100, 1)),  // rests
        place(a, limit(1, false, 100, 1)), // fills vs B @100 -> A short 1 @ 100
    ];
    let rs = NativeExecutor::execute_batch(&mut ctx, &setup);
    assert!(rs.results.iter().all(|r| r.success), "setup: {:?}", rs.results);

    let a_bal0 = ctx.positions.get_native_balance(&a).unwrap();
    assert_eq!(a_bal0.available, fp(10_000), "setup left margin un-released");
    assert_eq!(a_bal0.order_margin, FixedPoint::ZERO);

    // Test batch (single market, submission order = settlement order):
    //   idx0: C sell 1@90  (rests)
    //   idx1: A buy 1@90   (fills vs C @90 -> full close of A's short: PnL +10)
    //   idx2: C sell 1@85  (rests)
    //   idx3: A buy 2@85   (partial fill 1 of 2 -> releases half of A's reserve
    //                       AFTER the idx1 PnL credit, same batch)
    let batch: Vec<(Address, NativeAction)> = vec![
        place(c, limit(1, false, 90, 1)),
        place(a, limit(1, true, 90, 1)),
        place(c, limit(1, false, 85, 1)),
        place(a, limit(1, true, 85, 2)),
    ];
    let rb = NativeExecutor::execute_batch(&mut ctx, &batch);
    assert!(rb.results.iter().all(|r| r.success), "batch: {:?}", rb.results);

    // Expected (default 20x leverage => reserve = notional / 20):
    //   r1 = 90*1/20 = 4.5  (idx1, fully released on full fill)
    //   r3 = 85*2/20 = 8.5  (idx3, half released on 1-of-2 fill)
    //   available = 10_000 - r1 - r3 + r1 + 10 (PnL) + r3/2 = 10_010 - r3/2
    //   order_margin = r3/2
    let r3 = fp(85) * fp(2) / fp(20);
    let expected_avail = fp(10_000) + fp(10) - r3 / fp(2);
    let expected_om = r3 / fp(2);

    let a_bal = ctx.positions.get_native_balance(&a).unwrap();
    assert_eq!(
        a_bal.available, expected_avail,
        "PnL credit was lost or margin release mis-applied (got {}, want {})",
        a_bal.available, expected_avail
    );
    assert_eq!(a_bal.order_margin, expected_om);

    // Position bookkeeping: A's short is gone, replaced by long 1 @ 85.
    let pos = ctx.positions.get_position(&a, 1).unwrap().unwrap();
    assert!(pos.is_long);
    assert_eq!(pos.size, fp(1));
    assert_eq!(pos.entry_price, fp(85));
}
