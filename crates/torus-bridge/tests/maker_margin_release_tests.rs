//! A5 (perf/funnel-truth): maker-fill margin release tests.
//!
//! Phase 2 reserves `price*qty/20` of order margin for every limit order. The
//! pre-A5 executor released that reservation only on the ORDER'S OWN settlement
//! (taker-side fills / IOC-cancel / explicit cancel of the *remaining* qty) —
//! when a resting order was consumed as MAKER by a later taker, or auto-
//! cancelled by self-trade prevention, its reservation was stranded in
//! `order_margin` forever. These tests pin the fixed accounting identity:
//!
//!     over any order's lifetime, Σ margin released == margin reserved
//!
//! exactly (FixedPoint-truncation dust included), across:
//!   (a) full maker fill,
//!   (b) partial maker fill + CancelOrder of the remainder (incl. a
//!       truncation-dust quantity),
//!   (c) STP maker-cancel,
//!   (d) determinism: same batch → same balances,
//! plus in-batch maker consumption by multiple takers (aggregation, no
//! double-release) and single-action-path parity.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers (same idiom as funnel_metrics_tests.rs) ----

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

fn limit(market_id: MarketId, is_buy: bool, price: i64, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn bal(ctx: &NativeExecContext, trader: &Address) -> NativeBalance {
    ctx.positions.get_native_balance(trader).unwrap()
}

fn assert_bal(ctx: &NativeExecContext, trader: &Address, avail: FixedPoint, margin: FixedPoint, what: &str) {
    let b = bal(ctx, trader);
    assert_eq!(b.available, avail, "{what}: available");
    assert_eq!(b.order_margin, margin, "{what}: order_margin");
}

const FUNDING: i64 = 1_000;

// ============================================================================
// (a) Maker fully filled → full reservation released
// ============================================================================

#[test]
fn maker_full_fill_releases_full_reservation() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let maker = addr(1);
    let taker = addr(2);
    fund_native(&ctx, &maker, fp(FUNDING));
    fund_native(&ctx, &taker, fp(FUNDING));

    // Batch 1: maker rests buy@100 qty4 → reserve 100*4/20 = 20.
    let r1 = NativeExecutor::execute_batch(&mut ctx, &[place(maker, limit(1, true, 100, fp(4)))]);
    assert!(r1.results[0].success, "{:?}", r1.results[0].error);
    assert_bal(&ctx, &maker, fp(FUNDING - 20), fp(20), "maker post-rest");

    // Batch 2: taker sells 4 into it → maker fully consumed AS MAKER.
    let r2 = NativeExecutor::execute_batch(&mut ctx, &[place(taker, limit(1, false, 100, fp(4)))]);
    assert!(r2.results[0].success, "{:?}", r2.results[0].error);

    // No fees, no realized PnL (both merely open positions): every reservation
    // must come back — the pre-A5 code stranded the maker's 20 forever.
    assert_bal(&ctx, &maker, fp(FUNDING), FixedPoint::ZERO, "maker post-fill");
    assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker post-fill");
}

// ============================================================================
// (b) Partial maker fill → proportional release; CancelOrder releases exactly
//     the remainder (total released == total reserved)
// ============================================================================

#[test]
fn maker_partial_fill_then_cancel_releases_exactly_total() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let maker = addr(1);
    let taker = addr(2);
    fund_native(&ctx, &maker, fp(FUNDING));
    fund_native(&ctx, &taker, fp(FUNDING));

    let maker_order_id = ctx.next_global_order_id;
    let r1 = NativeExecutor::execute_batch(&mut ctx, &[place(maker, limit(1, true, 100, fp(4)))]);
    assert!(r1.results[0].success);
    assert_bal(&ctx, &maker, fp(FUNDING - 20), fp(20), "maker post-rest");

    // Taker consumes 1 of 4 → maker release = reserve(4) - reserve(3) = 20 - 15 = 5.
    let r2 = NativeExecutor::execute_batch(&mut ctx, &[place(taker, limit(1, false, 100, fp(1)))]);
    assert!(r2.results[0].success);
    assert_bal(&ctx, &maker, fp(FUNDING - 15), fp(15), "maker post-partial-fill");
    assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker restored");

    // Cancel the remainder → releases exactly reserve(3) = 15. No dust, no
    // double-release: maker lands PRECISELY back on funding.
    let rc = NativeExecutor::execute_batch(
        &mut ctx,
        &[(maker, NativeAction::CancelOrder { order_id: maker_order_id })],
    );
    assert!(rc.results[0].success, "{:?}", rc.results[0].error);
    assert_bal(&ctx, &maker, fp(FUNDING), FixedPoint::ZERO, "maker post-cancel");
}

/// Truncation-dust variant of (b): quantity 3.00000001 at price 1 reserves
/// raw(300000001)/20 = raw 15_000_000 (0.05 raw units truncated). A 1.0-qty
/// maker fill and the follow-up cancel must still telescope to EXACTLY the
/// reserved amount — the naive per-quantity recomputation would strand dust.
#[test]
fn maker_partial_fill_truncation_dust_is_zero() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let maker = addr(1);
    let taker = addr(2);
    fund_native(&ctx, &maker, fp(FUNDING));
    fund_native(&ctx, &taker, fp(FUNDING));

    let qty = FixedPoint::from_raw(3 * FixedPoint::SCALE + 1); // 3.00000001
    let maker_order_id = ctx.next_global_order_id;
    let r1 = NativeExecutor::execute_batch(&mut ctx, &[place(maker, limit(1, true, 1, qty))]);
    assert!(r1.results[0].success, "{:?}", r1.results[0].error);

    // reserve = trunc(3.00000001 / 20) = raw 15_000_000
    let reserved = FixedPoint::from_raw(15_000_000);
    assert_bal(&ctx, &maker, fp(FUNDING) - reserved, reserved, "maker post-rest");

    // Fill 1.0 → remaining 2.00000001, reserve(rem) = trunc = raw 10_000_000.
    // Maker release = 15_000_000 - 10_000_000 = 5_000_000 (telescoped).
    let r2 = NativeExecutor::execute_batch(&mut ctx, &[place(taker, limit(1, false, 1, fp(1)))]);
    assert!(r2.results[0].success, "{:?}", r2.results[0].error);
    let after_fill = FixedPoint::from_raw(10_000_000);
    assert_bal(
        &ctx,
        &maker,
        fp(FUNDING) - after_fill,
        after_fill,
        "maker post-dusty-partial",
    );

    // Cancel releases reserve(2.00000001) = raw 10_000_000 → exact funding.
    let rc = NativeExecutor::execute_batch(
        &mut ctx,
        &[(maker, NativeAction::CancelOrder { order_id: maker_order_id })],
    );
    assert!(rc.results[0].success, "{:?}", rc.results[0].error);
    assert_bal(&ctx, &maker, fp(FUNDING), FixedPoint::ZERO, "maker exact after cancel");
    assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker exact");
}

// ============================================================================
// (c) STP maker-cancel → full remaining reservation released
// ============================================================================

#[test]
fn stp_cancel_releases_remaining_reservation() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let a = addr(1);
    fund_native(&ctx, &a, fp(FUNDING));

    // A rests buy@100 q5 (reserve 25), then A sells @100 q5: STP cancels the
    // resting buy (release 25); the sell rests (reserve 25).
    let r = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            place(a, limit(1, true, 100, fp(5))),
            place(a, limit(1, false, 100, fp(5))),
        ],
    );
    assert!(r.results.iter().all(|x| x.success), "{:?}", r.results);
    // Pre-A5 this was avail 950 / margin 50 (the cancelled buy's 25 stranded).
    assert_bal(&ctx, &a, fp(FUNDING - 25), fp(25), "post-STP");

    // Cancel-all releases the resting sell's 25 → exact funding.
    let rc = NativeExecutor::execute_batch(
        &mut ctx,
        &[(a, NativeAction::CancelAllOrders { market_id: None })],
    );
    assert!(rc.results[0].success);
    assert_bal(&ctx, &a, fp(FUNDING), FixedPoint::ZERO, "post-cancel-all");
}

/// STP after the maker was partially consumed: the fills release their slice,
/// the STP cancel releases exactly what is left — never the original total.
#[test]
fn stp_after_partial_consumption_releases_remainder_only() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let a = addr(1);
    let b = addr(2);
    fund_native(&ctx, &a, fp(FUNDING));
    fund_native(&ctx, &b, fp(FUNDING));

    // A rests buy@100 q5 (reserve 25).
    let r1 = NativeExecutor::execute_batch(&mut ctx, &[place(a, limit(1, true, 100, fp(5)))]);
    assert!(r1.results[0].success);

    // B sells 2 → maker A releases reserve(5)-reserve(3) = 25-15 = 10.
    let r2 = NativeExecutor::execute_batch(&mut ctx, &[place(b, limit(1, false, 100, fp(2)))]);
    assert!(r2.results[0].success);
    assert_bal(&ctx, &a, fp(FUNDING - 15), fp(15), "A post-partial");

    // A sells @100 q5: STP cancels A's remaining buy (3 left → release 15),
    // then the sell rests (reserve 25). A's long position stays open (no PnL).
    let r3 = NativeExecutor::execute_batch(&mut ctx, &[place(a, limit(1, false, 100, fp(5)))]);
    assert!(r3.results[0].success, "{:?}", r3.results[0].error);
    assert_bal(&ctx, &a, fp(FUNDING - 25), fp(25), "A post-STP-of-partial");

    let rc = NativeExecutor::execute_batch(
        &mut ctx,
        &[(a, NativeAction::CancelAllOrders { market_id: None })],
    );
    assert!(rc.results[0].success);
    assert_bal(&ctx, &a, fp(FUNDING), FixedPoint::ZERO, "A exact after cancel-all");
}

// ============================================================================
// In-batch aggregation: one maker consumed by several takers in ONE batch
// must release once from the aggregated quantity (no per-result double count)
// ============================================================================

#[test]
fn maker_consumed_by_multiple_takers_in_one_batch_releases_exactly_once() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let m = addr(1);
    let t1 = addr(2);
    let t2 = addr(3);
    for t in [&m, &t1, &t2] {
        fund_native(&ctx, t, fp(FUNDING));
    }

    // Single batch: M rests buy@100 q4 (reserve 20), then two takers consume
    // 1 + 3. M's own settlement releases nothing (it rested unfilled at ITS
    // match time); the maker pass must release reserve(4) - reserve(0) = 20
    // ONCE — a per-result implementation would release (20-15) twice or worse.
    let r = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            place(m, limit(1, true, 100, fp(4))),
            place(t1, limit(1, false, 100, fp(1))),
            place(t2, limit(1, false, 100, fp(3))),
        ],
    );
    assert!(r.results.iter().all(|x| x.success), "{:?}", r.results);

    assert_bal(&ctx, &m, fp(FUNDING), FixedPoint::ZERO, "maker exact");
    assert_bal(&ctx, &t1, fp(FUNDING), FixedPoint::ZERO, "taker1 exact");
    assert_bal(&ctx, &t2, fp(FUNDING), FixedPoint::ZERO, "taker2 exact");
}

// ============================================================================
// Single-action path (exec_place_order) parity with the batch pipeline
// ============================================================================

#[test]
fn single_action_path_matches_batch_path() {
    let maker = addr(1);
    let taker = addr(2);
    let orders = [
        limit(1, true, 100, fp(4)),  // maker rests
        limit(1, false, 100, fp(1)), // taker partial-fills the maker
    ];

    // Run A: batch pipeline.
    let (_da, dba) = open_test_db();
    let mut ctx_a = make_ctx(dba);
    fund_native(&ctx_a, &maker, fp(FUNDING));
    fund_native(&ctx_a, &taker, fp(FUNDING));
    let actions: Vec<(Address, NativeAction)> = vec![
        place(maker, orders[0].clone()),
        place(taker, orders[1].clone()),
    ];
    let ra = NativeExecutor::execute_batch(&mut ctx_a, &actions);
    assert!(ra.results.iter().all(|x| x.success));

    // Run B: sequential single-action dispatch (exec_place_order path).
    let (_db_, dbb) = open_test_db();
    let mut ctx_b = make_ctx(dbb);
    fund_native(&ctx_b, &maker, fp(FUNDING));
    fund_native(&ctx_b, &taker, fp(FUNDING));
    for (s, a) in &actions {
        let r = NativeExecutor::execute(&mut ctx_b, s, a);
        assert!(r.success, "{:?}", r.error);
    }

    for t in [&maker, &taker] {
        let ba = bal(&ctx_a, t);
        let bb = bal(&ctx_b, t);
        assert_eq!(ba.available, bb.available, "available parity for {t}");
        assert_eq!(ba.order_margin, bb.order_margin, "order_margin parity for {t}");
    }
    // And the absolute values: maker released 5 of 20, taker fully restored.
    assert_bal(&ctx_a, &maker, fp(FUNDING - 15), fp(15), "maker (batch)");
    assert_bal(&ctx_a, &taker, fp(FUNDING), FixedPoint::ZERO, "taker (batch)");
}

// ============================================================================
// (d) Determinism: same batch on fresh state → identical balances
// ============================================================================

#[test]
fn same_batch_is_deterministic_across_runs() {
    // Mixed scenario: partial maker fill, full maker fill, STP cancel, a
    // resting leftover — across two markets, exercising the aggregation maps.
    let run = || -> Vec<(i128, i128)> {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db);
        let a = addr(1);
        let b = addr(2);
        let c = addr(3);
        for t in [&a, &b, &c] {
            fund_native(&ctx, t, fp(FUNDING));
        }
        let actions: Vec<(Address, NativeAction)> = vec![
            place(a, limit(1, true, 100, fp(4))),  // rests
            place(b, limit(1, false, 100, fp(1))), // partial-fills A
            place(b, limit(2, true, 50, fp(2))),   // rests (mkt 2)
            place(c, limit(2, false, 50, fp(2))),  // fully fills B (mkt 2)
            place(a, limit(1, false, 100, fp(1))), // STP-cancels A's resting buy remainder
        ];
        let r = NativeExecutor::execute_batch(&mut ctx, &actions);
        assert!(r.results.iter().all(|x| x.success), "{:?}", r.results);
        [a, b, c]
            .iter()
            .map(|t| {
                let bl = bal(&ctx, t);
                (bl.available.raw(), bl.order_margin.raw())
            })
            .collect()
    };

    let first = run();
    for i in 0..4 {
        assert_eq!(first, run(), "run {i} diverged");
    }

    // Conservation: no reservation may be stranded for fully-settled traders.
    // C's sell fully filled -> C restored exactly.
    assert_eq!(first[2], (fp(FUNDING).raw(), 0), "C fully restored");
}
