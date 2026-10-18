//! Merge of main's s58 bundle (D `TORUS_SETTLE_DEFER_RESULTS`, M
//! `TORUS_PARALLEL_MATCH_SETUP`, T `TORUS_FAST_SELL_TOP_UPS`, all on by default)
//! into item 7 step 2. One block's native phase as `execute_committed_block`
//! runs it: multi-market batches on the forced parallel settle (pass B, where D
//! defers the placed results; >= 2 markets, where M builds the match inputs in
//! the pool jobs; T's stitch-recorded sell top-ups), `EvmUserModify` lane changes
//! (tag 30) between the orders, then the CoreWriter drain of rows queued K = 10
//! blocks earlier (order kinds, C7), then the book save with the B16 bbo rows.
//!
//! The bundle on (pinned per thread) must equal it off in every result field,
//! every drained CoreWriter row, the dirty markets, every native CF (bbo rows,
//! lane rows and the CoreWriter queue included) and the native state root; and
//! under the bundle every dirty market's bbo row is its book's top.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    with_fast_sell_top_ups, with_parallel_match_setup, with_settle_defer_results,
    NativeActionResult, NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::action_status::FailureReason;
use torus_state::cf::{
    evm_lane_key, CF_CORE_WRITER_QUEUE, CF_NATIVE_BALANCES, CF_NATIVE_BBO, CF_NATIVE_MARKETS,
    CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS, CF_NATIVE_TRADES,
    CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// The block: a CoreWriter order row queued at 1 targets 1 + K (default K = 10).
const HEIGHT: u64 = 11;
const MARKETS: [MarketId; 4] = [1, 2, 3, 4];

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn place(
    sender: Address,
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
    tif: TimeInForce,
) -> (Address, NativeAction) {
    (
        sender,
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: tif,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

fn lane(sender: Address, using_big_blocks: bool) -> (Address, NativeAction) {
    (sender, NativeAction::EvmUserModify { using_big_blocks })
}

/// Batch 1 rests a ladder on every market and sets two lanes; batch 2 crosses
/// it on every market (fills on both sides, IOCs), with lane changes between
/// the orders (one sender back to the small lane, one unchanged).
fn batches() -> Vec<Vec<(Address, NativeAction)>> {
    let mut b1 = vec![lane(addr(1), true), lane(addr(2), true)];
    for &m in &MARKETS {
        for lvl in 0..4i64 {
            b1.push(place(
                addr(1 + (m as u8 + lvl as u8) % 12),
                m,
                true,
                96 + lvl,
                2,
                TimeInForce::GTC,
            ));
            b1.push(place(
                addr(13 + (m as u8 + lvl as u8) % 12),
                m,
                false,
                101 + lvl,
                2,
                TimeInForce::GTC,
            ));
        }
    }
    let mut b2 = Vec::new();
    for i in 0..96u64 {
        let m = MARKETS[(i % 4) as usize];
        let sender = addr(1 + ((i * 7) % 24) as u8);
        let is_buy = i % 2 == 0;
        let price = if is_buy {
            101 + (i % 3) as i64
        } else {
            99 - (i % 3) as i64
        };
        let tif = if i % 5 == 0 {
            TimeInForce::IOC
        } else {
            TimeInForce::GTC
        };
        b2.push(place(sender, m, is_buy, price, 1 + (i % 3) as i64, tif));
        match i {
            10 => b2.push(lane(addr(2), false)),
            50 => b2.push(lane(addr(3), false)),
            70 => b2.push(lane(addr(1), true)),
            _ => {}
        }
    }
    vec![b1, b2]
}

type ResultRow = (&'static str, bool, Option<String>, u64, FailureReason);

fn row(a: &NativeActionResult) -> ResultRow {
    (
        a.action_type,
        a.success,
        a.error.clone().map(String::from),
        a.gas_used,
        a.reason,
    )
}

#[derive(PartialEq, Eq, Debug)]
struct Fingerprint {
    results: Vec<Vec<ResultRow>>,
    total_gas: Vec<u64>,
    drained: Vec<(u64, String, Address, u64, ResultRow, Option<u128>)>,
    dirty: Vec<MarketId>,
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    next_global_order_id: u128,
    state_root: B256,
}

/// One block's native phase with the bundle pinned `on` (or `None`: the env,
/// i.e. the shipped default) and the pass-A cap pinned.
fn run(on: Option<bool>, cap: usize) -> Fingerprint {
    match on {
        Some(on) => with_settle_defer_results(on, || {
            with_parallel_match_setup(on, || with_fast_sell_top_ups(on, || block(cap)))
        }),
        None => block(cap),
    }
}

fn block(cap: usize) -> Fingerprint {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let mut ctx = NativeExecContext::new(
        db.clone(),
        HEIGHT,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    for n in 1..=32u8 {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }
    // Two order-kind rows queued at block 1 by an EVM block: both target 1 + K = 11.
    let queued = |trader: Address, kind: QueuedActionKind| QueuedAction {
        trader,
        kind,
        block_queued: 1,
    };
    let place_row = QueuedActionKind::PlaceOrder {
        market_id: 2,
        side: 0,
        order_type: 0,
        price: fp(97),
        quantity: fp(1),
        time_in_force: 0,
        cloid: None,
    };
    CoreWriterQueue::enqueue(&db, &queued(addr(30), place_row)).unwrap();
    CoreWriterQueue::enqueue(
        &db,
        &queued(addr(5), QueuedActionKind::CancelAll { market_id: 3 }),
    )
    .unwrap();
    assert_eq!(
        CoreWriterQueue::pending_count(&db, HEIGHT).unwrap(),
        2,
        "both rows target 11"
    );

    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    for batch in batches() {
        let r = NativeExecutor::execute_batch_settle_workers(&mut ctx, &batch, true, Some(cap));
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        results.push(r.results.iter().map(row).collect::<Vec<_>>());
        total_gas.push(r.total_gas);
    }
    assert!(
        ctx.phase_accum.settle_pass_a_ns > 0,
        "the parallel settle never ran"
    );
    assert_eq!(
        ctx.phase_accum.settle_fallbacks, 0,
        "a parallel settle fell back"
    );
    let lane_results: Vec<_> = results
        .iter()
        .flatten()
        .filter(|r| r.0 == "evm_user_modify")
        .collect();
    assert_eq!(lane_results.len(), 5);
    assert!(
        lane_results.iter().all(|r| r.1),
        "every lane change succeeds"
    );

    let drained: Vec<_> = NativeExecutor::drain_core_writer_entries(&mut ctx)
        .expect("drain")
        .into_iter()
        .map(|e| {
            (
                e.seq,
                format!("{:?}", e.kind),
                e.trader,
                e.queued_block,
                row(&e.result),
                e.oid,
            )
        })
        .collect();
    assert_eq!(drained.len(), 2, "both K-delayed rows drain at 11");
    assert!(
        drained.iter().all(|d| d.4 .1),
        "both rows execute: {drained:?}"
    );
    assert!(drained[0].5.is_some(), "the placed row has its order id");

    let mut dirty: Vec<MarketId> = ctx.dirty_books.iter().copied().collect();
    dirty.sort_unstable();
    assert_eq!(dirty, MARKETS.to_vec(), "every market's book changed");
    ctx.save_order_books();
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);

    // B16: each dirty market's bbo row is its book's top (none for an empty book).
    for &m in &dirty {
        let top = ctx.order_books()[&m].bbo_row();
        let want = (top != [0u8; 32]).then(|| top.to_vec());
        let got = StateBackend::get_cf_raw(&db, CF_NATIVE_BBO, &m.to_be_bytes()).unwrap();
        assert_eq!(got, want, "market {m}: bbo row");
    }
    // Lane rows: #1 big, #2 back to small, #3 never set.
    let lane_row =
        |n: u8| StateBackend::get_cf_raw(&db, CF_NATIVE_ORACLE, &evm_lane_key(&addr(n))).unwrap();
    assert_eq!(lane_row(1), Some(vec![1]));
    assert_eq!(lane_row(2), None);
    assert_eq!(lane_row(3), None);
    assert_eq!(CoreWriterQueue::pending_count(&db, HEIGHT).unwrap(), 0);

    let mut cf_dump = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        CF_NATIVE_BBO,
        CF_NATIVE_ORACLE,
        CF_CORE_WRITER_QUEUE,
    ] {
        for (k, v) in StateBackend::iterate_cf(&db, cf, None).unwrap() {
            cf_dump.push((cf.to_string(), k, v));
        }
    }
    assert!(
        cf_dump.iter().any(|(cf, _, _)| cf == CF_NATIVE_BBO),
        "bbo rows written"
    );
    Fingerprint {
        results,
        total_gas,
        drained,
        dirty,
        cf_dump,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
    }
}

/// D + M + T on (pinned and as shipped) equals all three off, at pass-A caps
/// 1, 2 and 8.
#[test]
fn s58_bundle_keeps_lanes_corewriter_drain_and_bbo_rows() {
    for cap in [1, 2, 8] {
        let off = run(Some(false), cap);
        let on = run(Some(true), cap);
        assert_eq!(on, off, "cap {cap}: D + M + T on vs off");
        let shipped = run(None, cap);
        assert_eq!(shipped, off, "cap {cap}: the env default vs off");
        let fills = off
            .cf_dump
            .iter()
            .filter(|(cf, _, _)| cf == CF_NATIVE_TRADES)
            .count();
        assert!(fills > 0, "cap {cap}: the batch must fill");
    }
}
