//! Owner s138 (pre-genesis consensus rule): a market that has NEVER had an
//! oracle mark (no aggregate price was ever recorded for it) has no price
//! band, so an order there could trade at any price and the first mark could
//! liquidate the positions at once. Until its first mark lands, every
//! placement there is REJECTED in consensus with reason `MarketNeverMarked`
//! (code 18) and the text `market N has no oracle mark yet: orders open after
//! its first mark`, before the cloid / open-order / margin checks: nothing is
//! claimed, written or charged, and every exec path (single action, serial
//! batch, sharded engine, cancel batching, streamed settle on / off,
//! CoreWriter) gives the same result. Modify is refused the same way; the
//! cancels keep working. Once the first mark exists the normal band applies,
//! and a later STALE mark keeps the band's stale fallback (unchanged).

use alloy_primitives::Address;

use torus_bridge::native_executor::{with_settle_streamed, NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::action_status::FailureReason;
use torus_state::cf::{ALL_CF_NAMES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Listed, fresh mark 100 at `NOW`.
const MARKED: MarketId = 1;
/// Listed, no aggregate row ever.
const UNMARKED: MarketId = 2;
/// Listed, aggregate 100 written long ago (stale at `NOW`).
const STALE: MarketId = 3;
const NOW: u64 = 1_000_000;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn agg_key(m: MarketId) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8).
fn agg_row(price: FixedPoint, ts: u64) -> Vec<u8> {
    [price.raw().to_be_bytes().as_slice(), &1u64.to_be_bytes(), &3u32.to_be_bytes(), &ts.to_be_bytes()].concat()
}

/// Markets 1..=3 listed (placeholder rows: tick / lot 1), market 1 marked
/// 100 at `NOW`, market 3 marked 100 long ago (stale), market 2 never;
/// traders 1..=4 funded; CoreWriter order delay 1.
fn world() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    for m in [MARKED, UNMARKED, STALE] {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(MARKED), &agg_row(fp(100), NOW)).unwrap();
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(STALE), &agg_row(fp(100), NOW - 10_000)).unwrap();
    db.put_cf_raw(torus_state::cf::CF_FEE_CONFIG, b"core_writer_order_delay", b"1").unwrap();
    let c = ctx(&db, 1);
    for t in 1..=4 {
        c.positions
            .put_native_balance(&addr(t), &NativeBalance { available: fp(1_000_000), order_margin: FixedPoint::ZERO })
            .unwrap();
    }
    (dir, db)
}

fn ctx(db: &StateDb, height: u64) -> NativeExecContext<StateDb> {
    NativeExecContext::new(db.clone(), height, NOW, 0, 100, 10, addr(99), addr(100), addr(101))
}

fn limit(market_id: MarketId, is_buy: bool, price: i64, cloid: Option<u128>) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    }
}

/// Every row of every CF (the whole DB).
fn dump(db: &StateDb) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in ALL_CF_NAMES {
        for (k, v) in db.iterate_cf(cf, None).unwrap() {
            out.push((*cf, k, v));
        }
    }
    out
}

fn want_text(m: MarketId) -> String {
    format!("market {m} has no oracle mark yet: orders open after its first mark")
}

fn assert_never_marked(r: &NativeActionResult, market: MarketId) {
    assert!(!r.success, "{} on market {market} must be rejected: {r:?}", r.action_type);
    assert_eq!(r.reason, FailureReason::MarketNeverMarked, "{r:?}");
    assert_eq!(r.gas_used, 0, "a rejected action is charged nothing: {r:?}");
    assert_eq!(r.error.as_deref(), Some(want_text(market).as_str()), "{r:?}");
}

/// Every placement kind on the never-marked market.
fn unmarked_placements() -> Vec<PlaceOrderParams> {
    let l = limit(UNMARKED, true, 100, None);
    vec![
        l.clone(),
        PlaceOrderParams { time_in_force: TimeInForce::IOC, ..l.clone() },
        PlaceOrderParams { time_in_force: TimeInForce::FOK, ..l.clone() },
        PlaceOrderParams { time_in_force: TimeInForce::PostOnly, ..l.clone() },
        PlaceOrderParams { order_type: OrderType::Market, time_in_force: TimeInForce::IOC, ..l.clone() },
        PlaceOrderParams { order_type: OrderType::StopLimit { trigger: fp(120), limit: fp(121) }, price: fp(121), ..l.clone() },
        PlaceOrderParams { order_type: OrderType::StopMarket { trigger: fp(120) }, price: fp(130), ..l.clone() },
        PlaceOrderParams { reduce_only: true, is_buy: false, ..l.clone() },
        PlaceOrderParams { client_order_id: Some(77), ..l },
    ]
}

/// Single-action path (`execute`: signed actions, session-key actions after
/// sender resolution, crash replay): each placement kind and a batch are
/// rejected, no book appears, the order-id counter does not move and the DB
/// is byte-identical to a run without them. With and without the block mark
/// table.
#[test]
fn every_placement_kind_on_a_never_marked_market_is_rejected_on_the_single_path() {
    for table in [false, true] {
        let (_d0, base) = world();
        let mut c0 = ctx(&base, 2);
        c0.save_order_books();

        let (_d1, db) = world();
        let mut c = ctx(&db, 2);
        if table {
            NativeExecutor::fill_block_marks(&mut c, &[MARKED, UNMARKED, STALE]);
        }
        let next_id = c.next_global_order_id;
        for p in unmarked_placements() {
            let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::PlaceOrder(p));
            assert_never_marked(&r, UNMARKED);
        }
        let batch = NativeAction::PlaceOrderBatch(vec![limit(UNMARKED, true, 100, None), limit(UNMARKED, false, 101, None)]);
        let r = NativeExecutor::execute(&mut c, &addr(1), &batch);
        assert!(!r.success, "{r:?}");
        assert_eq!(r.reason, FailureReason::MarketNeverMarked, "the batch summary carries it: {r:?}");
        assert!(!c.order_books().contains_key(&UNMARKED), "table={table}: no book");
        assert_eq!(c.next_global_order_id, next_id, "table={table}: no order id allocated");
        c.save_order_books();
        assert_eq!(dump(&db), dump(&base), "table={table}: a never-marked placement writes nothing");
    }
}

/// The batch paths: serial prepare, sharded engine (4 workers), cancel-all
/// batching on / off, streamed settle on / off — all reject the never-marked
/// entries with the same result, execute the rest, and leave a DB
/// byte-identical to the block without the never-marked entries. A rejected
/// order's cloid claims nothing.
#[test]
fn batch_paths_reject_never_marked_markets_identically_and_write_nothing_for_them() {
    let mixed: Vec<(Address, NativeAction)> = vec![
        (addr(1), NativeAction::PlaceOrder(limit(UNMARKED, true, 100, Some(42)))),
        (
            addr(2),
            NativeAction::PlaceOrderBatch(vec![
                limit(MARKED, false, 110, None),
                limit(UNMARKED, false, 110, None),
                limit(STALE, false, 101, None),
            ]),
        ),
        (
            addr(3),
            NativeAction::PlaceOrder(PlaceOrderParams {
                order_type: OrderType::StopLimit { trigger: fp(120), limit: fp(121) },
                price: fp(121),
                ..limit(UNMARKED, true, 121, None)
            }),
        ),
        (addr(1), NativeAction::PlaceOrder(limit(MARKED, true, 100, Some(42)))),
        (addr(4), NativeAction::CancelAllOrders { market_id: Some(UNMARKED) }),
        (addr(4), NativeAction::PlaceOrder(limit(MARKED, true, 99, None))),
        (addr(4), NativeAction::CancelAllOrders { market_id: Some(MARKED) }),
        (addr(4), NativeAction::CancelAllOrders { market_id: None }),
        (
            addr(3),
            NativeAction::PlaceOrder(PlaceOrderParams {
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                ..limit(UNMARKED, false, 50, None)
            }),
        ),
        (addr(3), NativeAction::PlaceOrder(limit(MARKED, true, 105, None))),
        // Crosses the resting ask 110 of trader 2: a fill on a marked market.
        (addr(1), NativeAction::PlaceOrder(limit(MARKED, true, 110, None))),
    ];
    let marked_only: Vec<(Address, NativeAction)> = vec![
        (addr(2), NativeAction::PlaceOrderBatch(vec![limit(MARKED, false, 110, None), limit(STALE, false, 101, None)])),
        (addr(1), NativeAction::PlaceOrder(limit(MARKED, true, 100, Some(42)))),
        (addr(4), NativeAction::CancelAllOrders { market_id: Some(UNMARKED) }),
        (addr(4), NativeAction::PlaceOrder(limit(MARKED, true, 99, None))),
        (addr(4), NativeAction::CancelAllOrders { market_id: Some(MARKED) }),
        (addr(4), NativeAction::CancelAllOrders { market_id: None }),
        (addr(3), NativeAction::PlaceOrder(limit(MARKED, true, 105, None))),
        (addr(1), NativeAction::PlaceOrder(limit(MARKED, true, 110, None))),
    ];

    let (_db_ref, reference) = world();
    let mut c = ctx(&reference, 2);
    let r = NativeExecutor::execute_batch_engine_mode(&mut c, &marked_only, 1);
    assert!(r.results.iter().all(|r| r.success), "{:?}", r.results);
    c.save_order_books();
    let want_db = dump(&reference);

    type Run = fn(&mut NativeExecContext<StateDb>, &[(Address, NativeAction)]) -> Vec<NativeActionResult>;
    let runs: [(&str, Run); 6] = [
        ("serial", |c, a| NativeExecutor::execute_batch_engine_mode(c, a, 1).results),
        ("engine x4", |c, a| NativeExecutor::execute_batch_engine_mode(c, a, 4).results),
        ("cancel batch on", |c, a| NativeExecutor::execute_batch_cancel_mode(c, a, true).results),
        ("cancel batch off", |c, a| NativeExecutor::execute_batch_cancel_mode(c, a, false).results),
        ("streamed settle on, engine x4", |c, a| {
            with_settle_streamed(true, || NativeExecutor::execute_batch_engine_mode(c, a, 4).results)
        }),
        ("streamed settle off, engine x4", |c, a| {
            with_settle_streamed(false, || NativeExecutor::execute_batch_engine_mode(c, a, 4).results)
        }),
    ];
    let mut first: Option<Vec<String>> = None;
    for (name, run) in runs {
        let (_d, db) = world();
        let mut c = ctx(&db, 2);
        let results = run(&mut c, &mixed);
        // Flattened: the batch's 3 orders are 3 results.
        assert_eq!(results.len(), mixed.len() + 2, "{name}");
        let never_marked_idx = [0usize, 2, 4, 10];
        for (i, r) in results.iter().enumerate() {
            if never_marked_idx.contains(&i) {
                assert_never_marked(r, UNMARKED);
            } else {
                assert!(r.success, "{name}: result {i} must succeed: {r:?}");
            }
        }
        assert!(!c.order_books().contains_key(&UNMARKED), "{name}: no book for a never-marked market");
        c.save_order_books();
        assert_eq!(dump(&db), want_db, "{name}: same DB as the block without the never-marked entries");
        let shown: Vec<String> = results.iter().map(|r| format!("{r:?}")).collect();
        match &first {
            None => first = Some(shown),
            Some(f) => assert_eq!(&shown, f, "{name}: same results on every path"),
        }
    }
}

/// CoreWriter (EVM precompile) placements on a never-marked market fail at
/// the drain with the same result; a CancelAll there still runs.
#[test]
fn core_writer_placements_on_a_never_marked_market_are_rejected_at_the_drain() {
    let (_d, db) = world();
    for (order_type, cloid) in [(0u8, None), (1, Some(9u128))] {
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind: QueuedActionKind::PlaceOrder {
                    market_id: UNMARKED,
                    side: 0,
                    order_type,
                    price: fp(100),
                    quantity: fp(1),
                    time_in_force: if order_type == 1 { 1 } else { 0 },
                    cloid,
                },
                block_queued: 1,
            },
        )
        .unwrap();
    }
    CoreWriterQueue::enqueue(
        &db,
        &QueuedAction { trader: addr(1), kind: QueuedActionKind::CancelAll { market_id: UNMARKED }, block_queued: 1 },
    )
    .unwrap();
    let mut c = ctx(&db, 2);
    let results = NativeExecutor::drain_core_writer(&mut c).unwrap();
    assert_eq!(results.len(), 3);
    assert_never_marked(&results[0], UNMARKED);
    assert_never_marked(&results[1], UNMARKED);
    assert!(results[2].success, "cancel-all still runs: {:?}", results[2]);
    assert!(!c.order_books().contains_key(&UNMARKED));
}

/// A resting order on a market without an aggregate row (only reachable
/// from state written before this rule; simulated by deleting the row):
/// a modify is refused with `MarketNeverMarked`; cancel by id, by cloid and
/// cancel-all keep working.
#[test]
fn modify_is_refused_and_cancels_still_work_on_a_never_marked_market() {
    let (_d, db) = world();
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(UNMARKED), &agg_row(fp(100), NOW)).unwrap();
    let mut c = ctx(&db, 2);
    let mut ids = Vec::new();
    for (cloid, px) in [(Some(5u128), 90), (Some(6), 91), (None, 92)] {
        let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::PlaceOrder(limit(UNMARKED, true, px, cloid)));
        assert!(r.success, "{r:?}");
        ids.push(c.next_global_order_id - 1);
    }
    c.save_order_books();
    drop(c);
    db.delete_cf_raw(CF_NATIVE_ORACLE, &agg_key(UNMARKED)).unwrap();

    let mut c = ctx(&db, 3);
    for (new_price, new_qty) in [(Some(fp(93)), None), (None, Some(fp(2))), (Some(fp(94)), Some(fp(3)))] {
        let r = NativeExecutor::execute(
            &mut c,
            &addr(1),
            &NativeAction::ModifyOrder { order_id: ids[0], new_price, new_qty },
        );
        assert!(!r.success, "{r:?}");
        assert_eq!(r.reason, FailureReason::MarketNeverMarked, "{r:?}");
        assert_eq!(r.gas_used, 0, "{r:?}");
        assert_eq!(r.error.as_deref(), Some(want_text(UNMARKED).as_str()), "{r:?}");
    }
    let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::CancelOrder { order_id: ids[0] });
    assert!(r.success, "cancel by id: {r:?}");
    let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::CancelByCloid { market_id: UNMARKED, cloid: 6 });
    assert!(r.success, "cancel by cloid: {r:?}");
    let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::CancelAllOrders { market_id: Some(UNMARKED) });
    assert!(r.success, "cancel-all: {r:?}");
    assert!(
        c.order_books().get(&UNMARKED).is_none_or(|b| b.orders_for_trader(&addr(1)).is_empty()),
        "every order cancelled"
    );
}

/// After the first mark the market trades under the normal band; a STALE
/// mark (an aggregate exists, too old to use) keeps today's behaviour: the
/// band's stale fallback, no `MarketNeverMarked`.
#[test]
fn the_first_mark_opens_the_market_and_a_stale_mark_keeps_the_fallback_band() {
    let (_d, db) = world();
    let mut c = ctx(&db, 2);
    let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::PlaceOrder(limit(UNMARKED, true, 100, None)));
    assert_never_marked(&r, UNMARKED);

    // The first mark lands (aggregate written by the oracle step).
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        c.oracle.submit_price(v, UNMARKED, fp(100), c.block_height, NOW).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    assert_eq!(c.oracle.aggregate_price(UNMARKED, c.block_height, NOW, &stakes).unwrap(), fp(100));
    let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::PlaceOrder(limit(UNMARKED, true, 100, None)));
    assert!(r.success, "after the first mark: {r:?}");
    // The normal band (±50% default) applies.
    let r = NativeExecutor::execute(&mut c, &addr(1), &NativeAction::PlaceOrder(limit(UNMARKED, true, 151, None)));
    assert_eq!(r.reason, FailureReason::PriceBand, "{r:?}");

    // A stale mark: accepted inside the fallback band, PriceBand outside.
    let market_sell =
        PlaceOrderParams { order_type: OrderType::Market, time_in_force: TimeInForce::IOC, ..limit(STALE, false, 50, None) };
    for (t, p) in [(2u8, limit(STALE, true, 100, None)), (3, market_sell)] {
        let r = NativeExecutor::execute(&mut c, &addr(t), &NativeAction::PlaceOrder(p));
        assert_ne!(r.reason, FailureReason::MarketNeverMarked, "stale is not never-marked: {r:?}");
    }
    let r = NativeExecutor::execute(&mut c, &addr(2), &NativeAction::PlaceOrder(limit(STALE, true, 100, None)));
    assert!(r.success, "{r:?}");
    let r = NativeExecutor::execute(&mut c, &addr(2), &NativeAction::PlaceOrder(limit(STALE, true, 200, None)));
    assert_eq!(r.reason, FailureReason::PriceBand, "the stale fallback band: {r:?}");
}
