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

// ---------------------------------------------------------------------------
// bl4 phase1-actions-drift-attribution
//
// `phase1_actions_ns` alone is unreadable: it swings 55 -> 790 ms/blk WITHIN a
// single 300 s cell (val0, bl3-merged-confirm-10m) while the action count per
// block is pinned at the ~200-action block cap. The span therefore has to be
// divided by the work it actually did, and the unit of that work is a
// CANCELLED ORDER, not an action: `CancelAllOrders { market_id: None }` walks
// every book and removes every one of the sender's resting orders, so one
// action costs O(that sender's resting depth) and gets more expensive as the
// book grows. These two counters are the numerator's denominators.
//
// Contract:
//   * `phase1_action_count` counts EVERY non-PlaceOrder entry the Phase-1
//     loop executed, whether it succeeded or not (a cancel for a vanished id
//     still walks all books — that is the cost being attributed);
//   * `phase1_orders_cancelled` counts orders actually REMOVED from a book by
//     CancelOrder / CancelAllOrders, so it grows with depth under a fixed
//     action mix;
//   * both are additive across the two `execute_batch` calls a block makes;
//   * both are node-local: adding them must not move the state root.
// ---------------------------------------------------------------------------

/// Seed a two-market ladder for `who`, then return the context so a Phase-1
/// batch can be measured against a KNOWN resting depth.
fn seeded_ctx(levels: i64) -> (tempfile::TempDir, NativeExecContext) {
    let (dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(10_000_000));
    }
    let mut seed = Vec::new();
    for m in 1..=2u64 {
        for lvl in 0..levels {
            // All resting orders belong to addr(1) so one CancelAllOrders
            // removes exactly `2 * levels` of them.
            seed.push(place(addr(1), gtc(m, true, 90 - lvl, 5)));
        }
    }
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &seed, false);
    assert!(ctx.fatal_error.is_none(), "fatal: {:?}", ctx.fatal_error);
    ctx.phase_accum = ExecPhaseAccum::default();
    (dir, ctx)
}

#[test]
fn phase1_counts_every_non_place_action() {
    let (_dir, mut ctx) = seeded_ctx(4);
    // 3 Phase-1 actions: two cancels for ids that do NOT rest (still walks
    // every book — that walk is the cost) and one that does nothing else.
    let batch: Vec<SignedAction> = vec![
        (addr(11), NativeAction::CancelOrder { order_id: 99_001 }),
        (addr(11), NativeAction::CancelOrder { order_id: 99_002 }),
        (addr(11), NativeAction::CancelAllOrders { market_id: None }),
    ];
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch, false);
    let a = ctx.phase_accum;
    assert_eq!(
        a.phase1_action_count, 3,
        "every non-PlaceOrder entry must be counted, hit or miss"
    );
    assert_eq!(
        a.phase1_orders_cancelled, 0,
        "addr(11) rests nothing, so nothing was removed"
    );
}

#[test]
fn phase1_cancelled_orders_track_book_depth() {
    // Same ACTION count, different resting depth: the cancelled-order counter
    // is what separates the two, and it is the only column that can tell a
    // deepening book from a heavier action mix.
    let mut counts = Vec::new();
    for levels in [4i64, 12] {
        let (_dir, mut ctx) = seeded_ctx(levels);
        let batch: Vec<SignedAction> =
            vec![(addr(1), NativeAction::CancelAllOrders { market_id: None })];
        NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch, false);
        let a = ctx.phase_accum;
        assert_eq!(a.phase1_action_count, 1, "one Phase-1 action either way");
        counts.push(a.phase1_orders_cancelled);
    }
    assert_eq!(counts[0], 8, "2 markets x 4 levels resting for addr(1)");
    assert_eq!(counts[1], 24, "2 markets x 12 levels resting for addr(1)");
}

#[test]
fn phase1_counters_count_single_order_cancels() {
    let (_dir, mut ctx) = seeded_ctx(4);
    // Order ids are assigned from 1 in placement order, so 1..=3 rest.
    let batch: Vec<SignedAction> = vec![
        (addr(1), NativeAction::CancelOrder { order_id: 1 }),
        (addr(1), NativeAction::CancelOrder { order_id: 2 }),
        (addr(1), NativeAction::CancelOrder { order_id: 1 }),
    ];
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch, false);
    let a = ctx.phase_accum;
    assert_eq!(a.phase1_action_count, 3);
    assert_eq!(
        a.phase1_orders_cancelled, 2,
        "the repeat cancel of id 1 removes nothing the second time"
    );
}

#[test]
fn phase1_counters_are_additive_across_calls() {
    let (_dir, mut ctx) = seeded_ctx(4);
    let first: Vec<SignedAction> = vec![(addr(1), NativeAction::CancelOrder { order_id: 1 })];
    let second: Vec<SignedAction> = vec![
        (addr(1), NativeAction::CancelOrder { order_id: 2 }),
        (addr(1), NativeAction::CancelAllOrders { market_id: None }),
    ];
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &first, false);
    let after_first = ctx.phase_accum;
    assert_eq!(after_first.phase1_action_count, 1);
    assert_eq!(after_first.phase1_orders_cancelled, 1);
    NativeExecutor::execute_batch_settle_mode(&mut ctx, &second, false);
    let a = ctx.phase_accum;
    assert_eq!(a.phase1_action_count, 3, "counts must ACCUMULATE, not reset");
    assert_eq!(
        a.phase1_orders_cancelled, 8,
        "1 + 1 + the 6 still resting when the cancel-all ran"
    );
}

/// The counters are node-local instrumentation, exactly like the ns spans:
/// a batch that cancels must still produce the same native state root as the
/// same batch executed on the other settle path.
#[test]
fn phase1_counters_do_not_perturb_state() {
    let mut roots = Vec::new();
    for parallel in [false, true] {
        let (_dir, mut ctx) = seeded_ctx(6);
        let batch: Vec<SignedAction> = vec![
            (addr(1), NativeAction::CancelOrder { order_id: 3 }),
            (addr(1), NativeAction::CancelAllOrders { market_id: Some(1) }),
        ];
        NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch, parallel);
        assert!(ctx.fatal_error.is_none(), "fatal: {:?}", ctx.fatal_error);
        assert!(ctx.phase_accum.phase1_orders_cancelled > 0);
        ctx.save_order_books();
        roots.push(compute_native_state_root(&ctx.state).expect("state root"));
    }
    assert_eq!(
        roots[0], roots[1],
        "Phase-1 workload counting must not change consensus state"
    );
}
