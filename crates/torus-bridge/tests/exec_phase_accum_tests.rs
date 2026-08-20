//! r6 engine-untimed-attribution: the per-block sub-phase accumulator that
//! decomposes the ~390 ms/blk of `exec_engine_seconds` that no existing
//! histogram accounts for.
//!
//! `execute_batch` already timed Phase 2 (margin), Phase 3 (match) and
//! Phase 4 (settle). This adds the missing spans — the Phase-1 non-PlaceOrder
//! action loop, the settle pass-A / pass-B split, and the end-of-call
//! position/balance cache flush — as *nanosecond accumulators on the context*
//! (not per-call histograms), so `app.rs` can observe exactly one sample per
//! block and derive `engine_untimed = engine_total - timed - tail`.
//!
//! Contract under test:
//!   * the accumulator is ADDITIVE across the two `execute_batch` calls a
//!     block makes (pre-EVM + post-EVM);
//!   * the settle sub-spans are strictly NESTED inside `settle_ns`
//!     (pass_a + pass_b + cache_flush <= settle_ns);
//!   * parallel settle records pass A **and** pass B; the canonical sequential
//!     loop records pass B only (pass_a stays 0);
//!   * timing is node-local: it must not perturb state — the parallel and
//!     sequential runs still fingerprint identically.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{ExecPhaseAccum, NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

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
    NativeExecContext::new(state_db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101))
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
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

/// One signed action as `execute_batch` takes them.
type SignedAction = (Address, NativeAction);

fn place(sender: Address, p: PlaceOrderParams) -> SignedAction {
    (sender, NativeAction::PlaceOrder(p))
}

/// Three markets, resting ladder then a crossing storm — enough fills that the
/// parallel settle path engages when forced.
fn batches() -> (Vec<SignedAction>, Vec<SignedAction>) {
    let mut seed = Vec::new();
    for m in 1..=3u64 {
        for lvl in 0..4i64 {
            seed.push(place(addr(1 + lvl as u8), gtc(m, true, 97 + lvl, 5)));
            seed.push(place(addr(5 + lvl as u8), gtc(m, false, 103 - lvl, 5)));
        }
    }
    let mut cross = Vec::new();
    for m in 1..=3u64 {
        cross.push(place(addr(11), gtc(m, true, 110, 20)));
        cross.push(place(addr(12), gtc(m, false, 90, 20)));
    }
    // Phase-1 (non-PlaceOrder) work: cancels are executed by the Phase-1 loop
    // whether or not the id still rests.
    cross.push((addr(11), NativeAction::CancelOrder { order_id: 1 }));
    cross.push((addr(12), NativeAction::CancelOrder { order_id: 2 }));
    (seed, cross)
}

fn run(parallel: bool) -> (ExecPhaseAccum, B256) {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    let (seed, cross) = batches();
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &seed, parallel);
    let after_first = ctx.phase_accum;
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &cross, parallel);
    assert!(ctx.fatal_error.is_none(), "fatal: {:?}", ctx.fatal_error);

    let a = ctx.phase_accum;
    // Additive across the two calls a block makes.
    assert!(
        a.margin_ns >= after_first.margin_ns,
        "margin_ns must accumulate"
    );
    assert!(
        a.match_ns >= after_first.match_ns,
        "match_ns must accumulate"
    );
    assert!(
        a.settle_ns >= after_first.settle_ns,
        "settle_ns must accumulate"
    );

    ctx.save_order_books();
    let root = compute_native_state_root(&ctx.state).expect("state root");
    (a, root)
}

#[test]
fn accum_spans_are_nested_and_additive_parallel() {
    let (a, _) = run(true);

    assert!(a.margin_ns > 0, "Phase 2 must be timed");
    assert!(a.match_ns > 0, "Phase 3 must be timed");
    assert!(a.settle_ns > 0, "Phase 4 must be timed");
    assert!(a.cache_flush_ns > 0, "pos/bal cache flush must be timed");
    assert!(a.settle_pass_a_ns > 0, "parallel settle must record pass A");
    assert!(a.settle_pass_b_ns > 0, "parallel settle must record pass B");

    // Strict nesting: pass A, pass B and the cache flush are disjoint spans
    // inside the Phase-4 timer.
    assert!(
        a.settle_pass_a_ns + a.settle_pass_b_ns + a.cache_flush_ns <= a.settle_ns,
        "settle sub-spans {} + {} + {} must nest inside settle_ns {}",
        a.settle_pass_a_ns,
        a.settle_pass_b_ns,
        a.cache_flush_ns,
        a.settle_ns
    );

    // The top-level "timed" total is the disjoint sum app.rs subtracts from
    // the engine wall clock.
    assert_eq!(
        a.timed_total_ns(),
        a.phase1_actions_ns + a.margin_ns + a.match_ns + a.settle_ns
    );
}

#[test]
fn sequential_settle_records_pass_b_only() {
    let (a, _) = run(false);

    assert_eq!(
        a.settle_pass_a_ns, 0,
        "the canonical sequential loop has no pass A"
    );
    assert!(a.settle_pass_b_ns > 0, "sequential settle is pass B");
    assert!(a.cache_flush_ns > 0, "cache flush is timed on both paths");
}

/// The accumulator is node-local instrumentation: parallel and sequential must
/// still agree byte-for-byte on the native state root.
#[test]
fn accum_does_not_perturb_state() {
    let (_, par) = run(true);
    let (_, seq) = run(false);
    assert_eq!(par, seq, "timing must not change consensus state");
}
