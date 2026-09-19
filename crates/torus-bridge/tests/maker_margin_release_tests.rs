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

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::margin::{effective_max_leverage, MarginTier, MarketMarginConfig};
use torus_core::order_book::OrderBook;
use torus_core::position::NativeBalance;
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::{StateBackend, StateDb};
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

// Frozen f890e06/080c4fa oracle (identical executor code): retain per-order config
// lookup and general FixedPoint division, plus mutation and clamp ordering.
fn legacy_cancel_all_margin(
    ctx: &mut NativeExecContext,
    sender: &Address,
    market_id: Option<MarketId>,
) -> NativeActionResult {
    // FIX 2 (ECON-FIND-05): Compute total margin to release from cancelled orders.
    let mut total_margin_release = FixedPoint::ZERO;

    match market_id {
        Some(mid) => {
            if let Some(book) = ctx.order_books.get_mut(&mid) {
                let cancelled = book.cancel_all(*sender, Some(mid));
                if !cancelled.is_empty() {
                    ctx.dirty_books.insert(mid);
                }
                for order in &cancelled {
                    let notional = order.price * order.remaining_qty;
                    let max_lev = ctx
                        .margin_configs
                        .get(&mid)
                        .map(|c| effective_max_leverage(&c.tiers, notional))
                        .unwrap_or(20);
                    let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                    total_margin_release += notional / lev_fp;
                }
            }
        }
        None => {
            let market_ids: Vec<MarketId> = ctx.order_books.keys().copied().collect();
            for mid in market_ids {
                if let Some(book) = ctx.order_books.get_mut(&mid) {
                    let cancelled = book.cancel_all(*sender, None);
                    if !cancelled.is_empty() {
                        ctx.dirty_books.insert(mid);
                    }
                    for order in &cancelled {
                        let notional = order.price * order.remaining_qty;
                        let max_lev = ctx
                            .margin_configs
                            .get(&mid)
                            .map(|c| effective_max_leverage(&c.tiers, notional))
                            .unwrap_or(20);
                        let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                        total_margin_release += notional / lev_fp;
                    }
                }
            }
        }
    }

    if total_margin_release > FixedPoint::ZERO {
        if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
            let release = total_margin_release.min(bal.order_margin);
            bal.order_margin -= release;
            bal.available += release;
            let _ = ctx.positions.put_native_balance(sender, &bal);
        }
    }

    NativeActionResult {
        action_type: "cancel_all",
        success: true,
        error: None,
        gas_used: 500,
    }
}

fn cancel_integer_fixture(clamp: bool) -> (tempfile::TempDir, NativeExecContext) {
    let (dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let maker = addr(1);
    let taker = addr(2);
    let other = addr(3);
    for trader in [maker, taker, other] {
        fund_native(&ctx, &trader, fp(1_000_000));
    }
    for mid in 1..=3 {
        let mut book = OrderBook::new(mid, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
        book.set_level_hash_chunked(true);
        ctx.order_books.insert(mid, book);
    }
    let mut tiered = MarketMarginConfig::new(1, 999);
    // The config's scalar max_leverage is not the selected tier leverage.
    tiered.tiers = vec![
        MarginTier {
            max_notional: fp(100),
            max_leverage: 3,
        },
        MarginTier {
            max_notional: fp(200),
            max_leverage: 7,
        },
        MarginTier {
            max_notional: FixedPoint::MAX,
            max_leverage: 31,
        },
    ];
    ctx.margin_configs.insert(1, tiered);
    let mut empty = MarketMarginConfig::new(2, 999);
    empty.tiers.clear(); // explicit empty config =>1x; absent market3=>20x
    ctx.margin_configs.insert(2, empty);

    let orders = [
        limit(
            1,
            true,
            1,
            FixedPoint::from_raw(100 * FixedPoint::SCALE - 1),
        ),
        limit(1, true, 1, fp(100)),
        limit(
            1,
            true,
            1,
            FixedPoint::from_raw(100 * FixedPoint::SCALE + 1),
        ),
        limit(
            1,
            true,
            1,
            FixedPoint::from_raw(201 * FixedPoint::SCALE + 3),
        ),
        limit(2, true, 2, FixedPoint::from_raw(2 * FixedPoint::SCALE + 3)),
        limit(3, true, 3, FixedPoint::from_raw(3 * FixedPoint::SCALE + 7)),
    ];
    let actions: Vec<_> = orders.into_iter().map(|p| place(maker, p)).collect();
    let result = NativeExecutor::execute_batch_settle_mode(&mut ctx, &actions, false);
    assert!(
        result.results.iter().all(|r| r.success),
        "{:?}",
        result.results
    );
    let rest = NativeExecutor::execute_batch_settle_mode(
        &mut ctx,
        &[place(other, limit(1, true, 1, fp(5)))],
        false,
    );
    assert!(rest.results[0].success);
    let mut consume = limit(1, false, 1, FixedPoint::from_raw(FixedPoint::SCALE + 1));
    consume.time_in_force = TimeInForce::IOC;
    let fill = NativeExecutor::execute_batch_settle_mode(&mut ctx, &[place(taker, consume)], false);
    assert!(fill.results[0].success);
    assert!(ctx.order_books[&1]
        .orders_for_trader(&maker)
        .iter()
        .any(|order| order.remaining_qty < order.original_qty));
    if clamp {
        let mut balance = bal(&ctx, &maker);
        balance.order_margin = FixedPoint::from_raw(7);
        ctx.positions.put_native_balance(&maker, &balance).unwrap();
    }
    for book in ctx.order_books.values_mut() {
        let _ = book.take_row_ops();
        let _ = book.take_level_ops();
    }
    ctx.dirty_books.clear();
    (dir, ctx)
}

fn assert_cancel_integer_state(actual: &mut NativeExecContext, expected: &mut NativeExecContext) {
    // Real stored bytes, including every funded trader rather than just the
    // cancel sender. No re-encoding a decoded balance hides field differences.
    let balances = |ctx: &NativeExecContext| -> std::collections::BTreeMap<_, _> {
        ctx.state
            .iterate_cf(CF_NATIVE_BALANCES, None)
            .unwrap()
            .into_iter()
            .collect()
    };
    assert_eq!(balances(actual), balances(expected));
    assert_eq!(actual.dirty_books, expected.dirty_books);
    let mut mids: Vec<_> = actual.order_books.keys().copied().collect();
    mids.sort_unstable();
    let mut expected_mids: Vec<_> = expected.order_books.keys().copied().collect();
    expected_mids.sort_unstable();
    assert_eq!(mids, expected_mids);
    for mid in mids {
        let a = actual.order_books.get_mut(&mid).unwrap();
        let e = expected.order_books.get_mut(&mid).unwrap();
        assert_eq!(a.stop_rows(), e.stop_rows());
        // Drain before full_row_ops, which clears the journal as a full save.
        assert_eq!(
            a.take_row_ops(),
            e.take_row_ops(),
            "row journal market={mid}"
        );
        assert_eq!(a.full_row_ops(), e.full_row_ops(), "rows market={mid}");
        let levels = |book: &mut OrderBook| {
            book.take_level_ops()
                .into_iter()
                .map(|(key, row)| (key, row.map(|row| row.encode().to_vec())))
                .collect::<Vec<_>>()
        };
        assert_eq!(levels(a), levels(e), "level journal market={mid}");
        let full_levels = |book: &mut OrderBook| {
            book.full_level_ops()
                .into_iter()
                .map(|(key, row)| (key, row.encode().to_vec()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            full_levels(a),
            full_levels(e),
            "mode3 commitments market={mid}"
        );
        assert_eq!(a.order_count(), e.order_count());
        assert_eq!(a.next_order_id(), e.next_order_id());
        assert_eq!(a.next_seq(), e.next_seq());
    }
}

#[test]
fn cancel_all_integer_margin_matches_legacy_on_deep_compacted_book() {
    fn fixture() -> (tempfile::TempDir, NativeExecContext, FixedPoint) {
        let (dir, db) = open_test_db();
        let mut ctx = make_ctx(db);
        let sender = addr(1);
        fund_native(&ctx, &sender, fp(1_000_000));
        fund_native(&ctx, &addr(2), fp(1_000_000));
        let mut book = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
        book.set_level_hash_chunked(true);
        book.set_next_seq(10_000);
        book.set_next_order_id(10_000);
        let mut config = MarketMarginConfig::new(1, 999);
        config.tiers = vec![
            MarginTier { max_notional: fp(200), max_leverage: 3 },
            MarginTier { max_notional: FixedPoint::MAX, max_leverage: 7 },
        ];
        let mut reserved = FixedPoint::ZERO;
        // A restored 2048-deep queue, with 80 dispersed targets: current core
        // admission accepts it and the movement bound selects compaction.
        for i in 0..2048u64 {
            let target = i < 2000 && i % 25 == 0;
            let quantity = FixedPoint::from_raw((1 + (i % 3) as i128) * FixedPoint::SCALE + 11);
            let order = torus_core::order_book::Order {
                id: (i + 1) as torus_types::OrderId,
                trader: if target { sender } else { addr(2) },
                side: torus_types::Side::Buy,
                price: fp(100),
                remaining_qty: quantity,
                original_qty: quantity,
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                timestamp: i,
                reduce_only: false,
                client_order_id: Some(i),
            };
            if target {
                let notional = order.price * order.remaining_qty;
                let leverage = effective_max_leverage(&config.tiers, notional);
                reserved += notional / FixedPoint::from_raw(i128::from(leverage) * FixedPoint::SCALE);
            }
            book.insert_loaded_order(order, i + 1);
        }
        assert_eq!(book.orders_for_trader(&sender).len(), 80);
        book.full_row_ops();
        book.full_level_ops();
        let mut balance = bal(&ctx, &sender);
        balance.available -= reserved;
        balance.order_margin = reserved;
        ctx.positions.put_native_balance(&sender, &balance).unwrap();
        ctx.margin_configs.insert(1, config);
        ctx.order_books.insert(1, book);
        (dir, ctx, reserved)
    }
    let (_actual_dir, mut actual, reserved) = fixture();
    let (_expected_dir, mut expected, _) = fixture();
    assert!(reserved > FixedPoint::ZERO);
    let result = NativeExecutor::execute(
        &mut actual, &addr(1), &NativeAction::CancelAllOrders { market_id: Some(1) },
    );
    assert!(result.success);
    assert!(legacy_cancel_all_margin(&mut expected, &addr(1), Some(1)).success);
    assert_eq!(actual.order_books[&1].order_count(), 2048 - 80);
    assert_eq!(actual.order_books[&1].journaled_rows(), 80);
    assert_bal(&actual, &addr(1), fp(1_000_000), FixedPoint::ZERO, "deep cancel release");
    assert_cancel_integer_state(&mut actual, &mut expected);
}

#[test]
fn cancel_all_integer_margin_real_state_matches_legacy() {
    for market in [Some(1), Some(2), Some(3), Some(999), None] {
        for clamp in [false, true] {
            let (_actual_dir, mut actual) = cancel_integer_fixture(clamp);
            let (_expected_dir, mut expected) = cancel_integer_fixture(clamp);
            let sender = addr(1);
            let old_balance = bal(&actual, &sender);
            let result = NativeExecutor::execute(
                &mut actual,
                &sender,
                &NativeAction::CancelAllOrders { market_id: market },
            );
            let reference = legacy_cancel_all_margin(&mut expected, &sender, market);
            assert_eq!(
                (
                    result.action_type,
                    result.success,
                    result.error,
                    result.gas_used
                ),
                (
                    reference.action_type,
                    reference.success,
                    reference.error,
                    reference.gas_used
                )
            );
            if clamp && market != Some(999) {
                assert_bal(
                    &actual,
                    &sender,
                    old_balance.available + FixedPoint::from_raw(7),
                    FixedPoint::ZERO,
                    "clamped cancel-all",
                );
            }
            assert_cancel_integer_state(&mut actual, &mut expected);
            // Repeating an empty cancellation leaves balances and books equal.
            NativeExecutor::execute(
                &mut actual,
                &sender,
                &NativeAction::CancelAllOrders { market_id: market },
            );
            legacy_cancel_all_margin(&mut expected, &sender, market);
            assert_cancel_integer_state(&mut actual, &mut expected);
        }
    }
}

#[test]
fn cancel_all_integer_margin_zero_preserves_mutation_boundary() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let (_actual_dir, mut actual) = cancel_integer_fixture(false);
    let (_expected_dir, mut expected) = cancel_integer_fixture(false);
    for ctx in [&mut actual, &mut expected] {
        ctx.margin_configs.get_mut(&1).unwrap().tiers = vec![MarginTier {
            max_notional: FixedPoint::MAX,
            max_leverage: 0,
        }];
    }
    let sender = addr(1);
    let before = actual
        .state
        .get_cf_raw(CF_NATIVE_BALANCES, sender.as_slice())
        .unwrap();
    let new = catch_unwind(AssertUnwindSafe(|| {
        NativeExecutor::execute(
            &mut actual,
            &sender,
            &NativeAction::CancelAllOrders { market_id: Some(1) },
        )
    }))
    .unwrap_err();
    let old = catch_unwind(AssertUnwindSafe(|| {
        legacy_cancel_all_margin(&mut expected, &sender, Some(1))
    }))
    .unwrap_err();
    let panic_text = |payload: &Box<dyn std::any::Any + Send>| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap()
    };
    assert_eq!(panic_text(&new), panic_text(&old));
    assert!(panic_text(&new).starts_with("FixedPoint division error"));
    assert!(actual.order_books[&1].orders_for_trader(&sender).is_empty());
    assert!(actual.dirty_books.contains(&1));
    assert_eq!(
        actual
            .state
            .get_cf_raw(CF_NATIVE_BALANCES, sender.as_slice())
            .unwrap(),
        before
    );
    assert_cancel_integer_state(&mut actual, &mut expected);
}
