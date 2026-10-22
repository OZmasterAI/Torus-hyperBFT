//! Pre-testnet P0 (roadmap "Unknown market ids create books", P1 item 2):
//! an action on a market with no `CF_NATIVE_MARKETS` row (not listed at
//! genesis or by governance) is REJECTED in consensus, not only at RPC, with
//! reason `UnknownMarket` and the RPC's text (`unknown market_id N`). It
//! creates no book and no state, is charged nothing (gas 0, like every
//! rejected action) and gives the same result on every exec path (single
//! action, serial batch, sharded engine, cancel-all batching, CoreWriter).
//! Listed markets are unaffected.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::action_status::FailureReason;
use torus_state::cf::{ALL_CF_NAMES, CF_NATIVE_MARKETS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const LISTED: MarketId = 1;
const UNKNOWN: MarketId = 9;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

/// A DB with market 1 listed (placeholder row: tick / lot 1) and traders
/// 1..=4 funded.
fn listed_db() -> (tempfile::TempDir, StateDb) {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    db.put_cf_raw(CF_NATIVE_MARKETS, &LISTED.to_be_bytes(), b"listed")
        .unwrap();
    // CoreWriter order delay K = 1 (rows queued in block 1 drain in block 2).
    db.put_cf_raw(
        torus_state::cf::CF_FEE_CONFIG,
        b"core_writer_order_delay",
        b"1",
    )
    .unwrap();
    let c = ctx(&db, 1);
    for t in 1..=4 {
        c.positions
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

fn ctx(db: &StateDb, height: u64) -> NativeExecContext<StateDb> {
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

fn stop_limit(market_id: MarketId) -> PlaceOrderParams {
    PlaceOrderParams {
        order_type: OrderType::StopLimit {
            trigger: fp(120),
            limit: fp(121),
        },
        ..limit(market_id, true, 121, None)
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

fn assert_unknown(r: &NativeActionResult, market: MarketId) {
    assert!(
        !r.success,
        "{} on market {market} must be rejected: {r:?}",
        r.action_type
    );
    assert_eq!(r.reason, FailureReason::UnknownMarket, "{r:?}");
    assert_eq!(r.gas_used, 0, "a rejected action is charged nothing: {r:?}");
    let want = format!("unknown market_id {market}");
    assert_eq!(r.error.as_deref(), Some(want.as_str()), "{r:?}");
}

/// The unknown-market actions of every kind that names a market.
fn unknown_actions() -> Vec<NativeAction> {
    vec![
        NativeAction::PlaceOrder(limit(UNKNOWN, true, 100, None)),
        NativeAction::PlaceOrder(stop_limit(UNKNOWN)),
        NativeAction::PlaceOrder(PlaceOrderParams {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            ..limit(UNKNOWN, false, 50, None)
        }),
        NativeAction::PlaceOrder(PlaceOrderParams {
            reduce_only: true,
            ..limit(UNKNOWN, false, 100, None)
        }),
        NativeAction::PlaceOrderBatch(vec![
            limit(UNKNOWN, true, 100, None),
            limit(UNKNOWN, false, 101, None),
        ]),
        NativeAction::CancelAllOrders {
            market_id: Some(UNKNOWN),
        },
        NativeAction::CancelByCloid {
            market_id: UNKNOWN,
            cloid: 7,
        },
    ]
}

/// Single-action path (`execute`, the CoreWriter / crash-replay path): each
/// kind is rejected, no book appears, the order-id counter does not move and
/// the DB (after the book save) is byte-identical to a run without them.
#[test]
fn every_action_kind_on_an_unknown_market_is_rejected_on_the_single_path() {
    let (_d0, base) = listed_db();
    let mut c0 = ctx(&base, 2);
    c0.save_order_books();

    let (_d1, db) = listed_db();
    let mut c = ctx(&db, 2);
    let next_id = c.next_global_order_id;
    for action in unknown_actions() {
        let r = NativeExecutor::execute(&mut c, &addr(1), &action);
        if let NativeAction::PlaceOrderBatch(_) = action {
            // The batch summary carries the first failing order's reason.
            assert!(!r.success, "{r:?}");
            assert_eq!(r.reason, FailureReason::UnknownMarket, "{r:?}");
        } else {
            assert_unknown(&r, UNKNOWN);
        }
    }
    assert!(
        !c.order_books().contains_key(&UNKNOWN),
        "no book for an unknown market"
    );
    assert_eq!(c.next_global_order_id, next_id, "no order id allocated");
    c.save_order_books();
    assert_eq!(
        dump(&db),
        dump(&base),
        "an unknown-market action writes nothing"
    );
}

/// The batch paths: serial prepare, sharded engine (4 workers) and the
/// cancel-all run batching all reject the unknown-market entries with the
/// same result, place the listed ones, and leave a DB byte-identical to a
/// block without the unknown-market entries (so every node agrees). An
/// unknown-market order's cloid claims nothing: a listed order of the same
/// sender with that cloid later in the block rests.
#[test]
fn batch_paths_reject_unknown_markets_identically_and_write_nothing_for_them() {
    let mixed: Vec<(Address, NativeAction)> = vec![
        (
            addr(1),
            NativeAction::PlaceOrder(limit(UNKNOWN, true, 100, Some(42))),
        ),
        (
            addr(2),
            NativeAction::PlaceOrderBatch(vec![
                limit(LISTED, false, 110, None),
                limit(UNKNOWN, false, 110, None),
                limit(LISTED, false, 111, None),
            ]),
        ),
        (addr(3), NativeAction::PlaceOrder(stop_limit(UNKNOWN))),
        (
            addr(1),
            NativeAction::PlaceOrder(limit(LISTED, true, 100, Some(42))),
        ),
        (
            addr(4),
            NativeAction::CancelAllOrders {
                market_id: Some(UNKNOWN),
            },
        ),
        (
            addr(4),
            NativeAction::CancelAllOrders {
                market_id: Some(LISTED),
            },
        ),
        (addr(4), NativeAction::CancelAllOrders { market_id: None }),
        (
            addr(3),
            NativeAction::CancelByCloid {
                market_id: UNKNOWN,
                cloid: 42,
            },
        ),
        (
            addr(3),
            NativeAction::PlaceOrder(limit(LISTED, true, 105, None)),
        ),
    ];
    let listed_only: Vec<(Address, NativeAction)> = vec![
        (
            addr(2),
            NativeAction::PlaceOrderBatch(vec![
                limit(LISTED, false, 110, None),
                limit(LISTED, false, 111, None),
            ]),
        ),
        (
            addr(1),
            NativeAction::PlaceOrder(limit(LISTED, true, 100, Some(42))),
        ),
        (
            addr(4),
            NativeAction::CancelAllOrders {
                market_id: Some(LISTED),
            },
        ),
        (addr(4), NativeAction::CancelAllOrders { market_id: None }),
        (
            addr(3),
            NativeAction::PlaceOrder(limit(LISTED, true, 105, None)),
        ),
    ];

    let (_db_ref, reference) = listed_db();
    let mut c = ctx(&reference, 2);
    let r = NativeExecutor::execute_batch_engine_mode(&mut c, &listed_only, 1);
    assert!(r.results.iter().all(|r| r.success), "{:?}", r.results);
    c.save_order_books();
    let want_db = dump(&reference);

    type Run =
        fn(&mut NativeExecContext<StateDb>, &[(Address, NativeAction)]) -> Vec<NativeActionResult>;
    let runs: [(&str, Run); 4] = [
        ("serial", |c, a| {
            NativeExecutor::execute_batch_engine_mode(c, a, 1).results
        }),
        ("engine x4", |c, a| {
            NativeExecutor::execute_batch_engine_mode(c, a, 4).results
        }),
        ("cancel batch on", |c, a| {
            NativeExecutor::execute_batch_cancel_mode(c, a, true).results
        }),
        ("cancel batch off", |c, a| {
            NativeExecutor::execute_batch_cancel_mode(c, a, false).results
        }),
    ];
    let mut first: Option<Vec<String>> = None;
    for (name, run) in runs {
        let (_d, db) = listed_db();
        let mut c = ctx(&db, 2);
        let results = run(&mut c, &mixed);
        // Flattened: the batch's 3 orders are 3 results.
        assert_eq!(results.len(), mixed.len() + 2, "{name}");
        let unknown_idx = [0usize, 2, 4, 6, 9];
        for (i, r) in results.iter().enumerate() {
            if unknown_idx.contains(&i) {
                assert_unknown(r, UNKNOWN);
            } else {
                assert!(r.success, "{name}: result {i} must succeed: {r:?}");
            }
        }
        assert!(
            !c.order_books().contains_key(&UNKNOWN),
            "{name}: no book for an unknown market"
        );
        c.save_order_books();
        assert_eq!(
            dump(&db),
            want_db,
            "{name}: same DB as the block without the unknown-market entries"
        );
        let shown: Vec<String> = results.iter().map(|r| format!("{r:?}")).collect();
        match &first {
            None => first = Some(shown),
            Some(f) => assert_eq!(&shown, f, "{name}: same results on every path"),
        }
    }
}

/// CoreWriter: a PlaceOrder / CancelAll queued from the EVM for an unknown
/// market fails at the drain with the same result and creates no book.
#[test]
fn core_writer_actions_on_an_unknown_market_are_rejected_at_the_drain() {
    let (_d, db) = listed_db();
    for kind in [
        QueuedActionKind::PlaceOrder {
            market_id: UNKNOWN,
            side: 0,
            order_type: 0,
            price: fp(100),
            quantity: fp(1),
            time_in_force: 0,
            cloid: None,
        },
        QueuedActionKind::CancelAll { market_id: UNKNOWN },
    ] {
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind,
                block_queued: 1,
            },
        )
        .unwrap();
    }
    let mut c = ctx(&db, 2);
    let results = NativeExecutor::drain_core_writer(&mut c).unwrap();
    assert_eq!(results.len(), 2);
    for r in &results {
        assert_unknown(r, UNKNOWN);
    }
    assert!(!c.order_books().contains_key(&UNKNOWN));
}

/// Listed markets are unaffected: orders rest, a cancel-all and a
/// cancel-by-cloid of a listed market without a book behave as before.
#[test]
fn listed_markets_are_unaffected() {
    let (_d, db) = listed_db();
    let mut c = ctx(&db, 2);
    let r = NativeExecutor::execute(
        &mut c,
        &addr(1),
        &NativeAction::CancelAllOrders {
            market_id: Some(LISTED),
        },
    );
    assert!(r.success, "{r:?}");
    let r = NativeExecutor::execute(
        &mut c,
        &addr(1),
        &NativeAction::CancelByCloid {
            market_id: LISTED,
            cloid: 5,
        },
    );
    assert!(!r.success);
    assert_eq!(
        r.reason,
        FailureReason::Other,
        "a listed market's missing book keeps its old result: {r:?}"
    );
    let r = NativeExecutor::execute(
        &mut c,
        &addr(1),
        &NativeAction::PlaceOrder(limit(LISTED, true, 100, Some(5))),
    );
    assert!(r.success, "{r:?}");
    assert!(c.order_books().contains_key(&LISTED));
    let r = NativeExecutor::execute(
        &mut c,
        &addr(1),
        &NativeAction::CancelByCloid {
            market_id: LISTED,
            cloid: 5,
        },
    );
    assert!(r.success, "{r:?}");
}
