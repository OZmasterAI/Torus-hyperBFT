//! O3 + s77: trade history (CF_NATIVE_TRADES / CF_NATIVE_USER_TRADES).
//!
//! These CFs are node-local (NOT in the native consensus root). Execution
//! records one `TradeFill` per fill; the block's packed rows are built from
//! all of its fills (`trade_rows::encode_block`). With `ctx.defer_trades` set,
//! the caller takes the fills and writes the rows (background writer);
//! otherwise each `execute_batch` writes them inline. Both give the same rows.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
use torus_state::trade_rows::{decode_trade_row, decode_user_row, encode_block, TradeFill};
use torus_state::{PackedCfBatch, StateBackend, StateDb};
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

/// Crossing flow on two markets: 3 fills total (2 on market 1, 1 on market 2).
fn crossing_actions() -> Vec<(Address, NativeAction)> {
    vec![
        (addr(1), NativeAction::PlaceOrder(limit(1, false, 100, 5))),
        (addr(2), NativeAction::PlaceOrder(limit(1, false, 101, 4))),
        // Buy 9 @ 101 crosses both resting sells -> 2 fills on market 1.
        (addr(3), NativeAction::PlaceOrder(limit(1, true, 101, 9))),
        (addr(1), NativeAction::PlaceOrder(limit(2, false, 200, 2))),
        // Buy 2 @ 200 crosses -> 1 fill on market 2.
        (addr(4), NativeAction::PlaceOrder(limit(2, true, 200, 2))),
    ]
}

fn run_batch(ctx: &mut NativeExecContext) {
    let actions = crossing_actions();
    for t in 1..=4u8 {
        fund_native(ctx, &addr(t), fp(1_000_000));
    }
    let result = NativeExecutor::execute_batch(ctx, &actions);
    for (i, r) in result.results.iter().enumerate() {
        assert!(r.success, "action {i} failed: {:?}", r.error);
    }
}

/// All rows of one CF, straight from the backend.
fn cf_rows<T: StateBackend>(state: &T, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    state.iterate_cf(cf, None).expect("iterate cf")
}

fn trade_indices(rows: &[(Vec<u8>, Vec<u8>)]) -> Vec<Vec<u32>> {
    rows.iter()
        .map(|(_, v)| decode_trade_row(v).unwrap().1.iter().map(|f| f.trade_index).collect())
        .collect()
}

/// Write the block's rows the way the node's writer does.
fn write_rows(db: &StateDb, fills: &[TradeFill]) {
    let mut rows = PackedCfBatch::default();
    encode_block(1, 1000, fills, &mut rows);
    let writer = torus_state::BackgroundCfWriter::spawn_with_policy(
        db.clone(),
        "packed-trade-test",
        1,
        torus_state::BgWriterPolicy { chunk_kvs: 2, low_pri: false },
    );
    writer.send_packed(rows).unwrap();
    drop(writer);
}

// ============================================================================
// Baseline: default (defer off) writes packed rows inline.
// ============================================================================

#[test]
fn defer_off_writes_packed_rows_inline() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    run_batch(&mut ctx);

    let trades = cf_rows(&db, CF_NATIVE_TRADES);
    assert_eq!(trades.len(), 2, "one row per market (block 1, chunk 0)");
    assert_eq!(trade_indices(&trades), vec![vec![0, 1], vec![2]]);
    let user_trades = cf_rows(&db, CF_NATIVE_USER_TRADES);
    let entries: Vec<(u8, Vec<(u32, u8)>)> = user_trades
        .iter()
        .map(|(k, v)| {
            let e = decode_user_row(v).unwrap().1;
            (k[0], e.iter().map(|e| (e.trade_index, e.role)).collect())
        })
        .collect();
    assert_eq!(
        entries,
        vec![
            (1, vec![(0, 0), (2, 0)]), // maker on both markets
            (2, vec![(1, 0)]),
            (3, vec![(0, 1), (1, 1)]), // taker of both market-1 fills
            (4, vec![(2, 1)]),
        ]
    );
    assert_eq!(ctx.trade_index, 3);
}

// ============================================================================
// Deferred: nothing hits the backend; the fills encode to the inline rows.
// ============================================================================

#[test]
fn defer_on_buffers_fills_and_rows_match_inline() {
    let (_dir_a, db_a) = open_test_db();
    let mut ctx_a = make_ctx(db_a.clone());
    run_batch(&mut ctx_a);

    let (_dir_b, db_b) = open_test_db();
    let mut ctx_b = make_ctx(db_b.clone());
    ctx_b.defer_trades = true;
    run_batch(&mut ctx_b);

    assert!(cf_rows(&db_b, CF_NATIVE_TRADES).is_empty(), "deferred: no trade rows during exec");
    assert!(cf_rows(&db_b, CF_NATIVE_USER_TRADES).is_empty(), "deferred: no user rows during exec");

    let fills = ctx_b.take_pending_trade_fills();
    let idx: Vec<u32> = fills.iter().map(|f| f.trade_index).collect();
    assert_eq!(idx, vec![0, 1, 2]);
    assert_eq!((fills[0].market, fills[0].maker, fills[0].taker), (1, addr(1), addr(3)));
    assert_eq!(fills[0].price_raw, fp(100).raw());
    assert_eq!(fills[0].qty_raw, fp(5).raw());
    assert_eq!(fills[0].taker_side, 0, "taker bought");
    assert!(ctx_b.take_pending_trade_fills().is_empty(), "take must drain");

    write_rows(&db_b, &fills);
    for cf in [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES] {
        assert_eq!(cf_rows(&db_b, cf), cf_rows(&db_a, cf), "{cf}: deferred rows == inline rows");
    }
}

// ============================================================================
// Fills accumulate across execute_batch calls (app.rs runs pre_evm and
// post_evm batches on one ctx); the block's rows hold both phases' fills.
// ============================================================================

fn two_phase_block(ctx: &mut NativeExecContext) {
    for t in 1..=2u8 {
        fund_native(ctx, &addr(t), fp(1_000_000));
    }
    let batch1: Vec<(Address, NativeAction)> = vec![
        (addr(1), NativeAction::PlaceOrder(limit(1, false, 100, 5))),
        (addr(2), NativeAction::PlaceOrder(limit(1, true, 100, 5))),
    ];
    let batch2: Vec<(Address, NativeAction)> = vec![
        (addr(1), NativeAction::PlaceOrder(limit(1, false, 100, 3))),
        (addr(2), NativeAction::PlaceOrder(limit(1, true, 100, 3))),
    ];
    let r1 = NativeExecutor::execute_batch(ctx, &batch1);
    let r2 = NativeExecutor::execute_batch(ctx, &batch2);
    assert!(r1.results.iter().all(|r| r.success));
    assert!(r2.results.iter().all(|r| r.success));
}

#[test]
fn fills_accumulate_across_batches() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    ctx.defer_trades = true;
    two_phase_block(&mut ctx);
    let fills = ctx.take_pending_trade_fills();
    let idx: Vec<u32> = fills.iter().map(|f| f.trade_index).collect();
    assert_eq!(idx, vec![0, 1], "trade_index keeps advancing across calls");

    // Inline: the second call's rows must not drop the first call's fill.
    let (_dir_i, db_i) = open_test_db();
    let mut ctx_i = make_ctx(db_i.clone());
    two_phase_block(&mut ctx_i);
    assert_eq!(trade_indices(&cf_rows(&db_i, CF_NATIVE_TRADES)), vec![vec![0, 1]]);

    write_rows(&db, &fills);
    for cf in [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES] {
        assert_eq!(cf_rows(&db, cf), cf_rows(&db_i, cf), "{cf}");
    }
}

// ============================================================================
// s77: trade_index counts every fill in both settle paths, history on or off.
// ============================================================================

#[test]
fn trade_index_counts_fills_in_both_settle_paths_with_history_on_and_off() {
    for history in [true, false] {
        for parallel in [false, true] {
            let (_dir, db) = open_test_db();
            let mut ctx = make_ctx(db.clone());
            ctx.defer_trades = true;
            ctx.trade_history = history;
            for t in 1..=4u8 {
                fund_native(&ctx, &addr(t), fp(1_000_000));
            }
            let r = NativeExecutor::execute_batch_settle_mode(&mut ctx, &crossing_actions(), parallel);
            assert!(r.results.iter().all(|r| r.success));
            let case = format!("history={history} parallel={parallel}");
            assert_eq!(ctx.trade_index, 3, "{case}");
            let fills = ctx.take_pending_trade_fills();
            assert_eq!(fills.len(), if history { 3 } else { 0 }, "{case}");
            assert!(fills.iter().map(|f| f.trade_index).eq(0..fills.len() as u32), "{case}");
        }
    }
}
