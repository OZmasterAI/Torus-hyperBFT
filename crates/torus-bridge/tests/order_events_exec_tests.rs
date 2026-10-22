//! s139: order events through the executor (`orderUpdates` stream).
//!
//! `NativeExecContext::set_order_events(true)` turns every book's event log
//! on for the block; `take_order_events` drains them (markets ascending,
//! execution order within a market) plus the executor's own pre-book
//! rejections. Off (the default), nothing is recorded. The events never
//! change state, and the sequential and parallel engines record the same
//! events.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::order_events::{OrderEvent, OrderEventReason as R, OrderEventStatus as S};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    // Owner s138: a fixture from before the never-marked rule (test-only
    // seam, as main's pre-s138 fixtures): it trades 90..=200 on placeholder
    // markets, which no one mark keeps inside the price band.
    torus_core::oracle::test_treat_unmarked_as_marked();
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    for m in 1..=8u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .expect("list market");
    }
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(state_db, height, 1000 + height, 0, 100, 10, addr(99), addr(100), addr(101))
}

fn fund(ctx: &NativeExecContext, trader: &Address) {
    let bal = NativeBalance {
        available: fp(1_000_000),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn order(market_id: MarketId, is_buy: bool, price: i64, qty: i64, tif: TimeInForce, cloid: Option<u128>) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, price, qty, TimeInForce::GTC, None)
}

/// `(market, trader byte, status, reason, filled, remaining)` of each event.
fn summary(ev: &[OrderEvent]) -> Vec<(u64, u8, S, R, i64, i64)> {
    ev.iter()
        .map(|e| {
            (
                e.market,
                e.trader.0[0],
                e.status,
                e.reason,
                (e.filled.raw() / FixedPoint::SCALE) as i64,
                (e.remaining.raw() / FixedPoint::SCALE) as i64,
            )
        })
        .collect()
}

/// Two markets: rests, a crossing fill, an IOC with no fill (a book
/// reject), and a placement refused before the book (off tick).
fn actions() -> Vec<(Address, NativeAction)> {
    vec![
        (addr(1), NativeAction::PlaceOrder(order(1, false, 100, 5, TimeInForce::GTC, Some(0xc1)))),
        (addr(2), NativeAction::PlaceOrder(gtc(1, true, 100, 2))),
        (addr(3), NativeAction::PlaceOrder(gtc(2, false, 200, 1))),
        (addr(4), NativeAction::PlaceOrder(order(2, true, 150, 1, TimeInForce::IOC, Some(0xc4)))),
    ]
}

/// The book CF's rows.
type BookRows = Vec<(Vec<u8>, Vec<u8>)>;

fn run(record: bool, threads: Option<usize>) -> (Vec<OrderEvent>, BookRows) {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1);
    for t in 1..=4u8 {
        fund(&ctx, &addr(t));
    }
    ctx.set_order_events(record);
    let list = actions();
    let result = match threads {
        Some(n) => NativeExecutor::execute_batch_engine_mode(&mut ctx, &list, n),
        None => NativeExecutor::execute_batch(&mut ctx, &list),
    };
    assert_eq!(result.results.len(), 4);
    let events = ctx.take_order_events();
    ctx.save_order_books();
    let books = ctx
        .state
        .iterate_cf(torus_state::cf::CF_NATIVE_ORDER_BOOKS, None)
        .unwrap();
    (events, books)
}

#[test]
fn exec_records_no_order_events_without_a_subscriber() {
    let (events, _) = run(false, None);
    assert!(events.is_empty());
}

#[test]
fn exec_order_events_markets_ascending_in_execution_order() {
    let (events, _) = run(true, None);
    assert_eq!(
        summary(&events),
        vec![
            (1, 1, S::Open, R::None, 0, 5),
            (1, 1, S::PartiallyFilled, R::None, 2, 3),
            (1, 2, S::Filled, R::None, 2, 0),
            (2, 3, S::Open, R::None, 0, 1),
            (2, 4, S::Rejected, R::Reject(torus_state::action_status::FailureReason::IocCancel), 0, 0),
        ]
    );
    // The maker's events carry its id and cloid; the IOC's its cloid and id.
    assert_eq!(events[0].cloid, Some(0xc1));
    assert_eq!(events[0].order_id, events[1].order_id);
    assert!(events[4].order_id.is_some());
    assert_eq!(events[4].cloid, Some(0xc4));
}

/// The events do not change state: the books are byte-identical with
/// recording on and off.
#[test]
fn exec_order_events_never_change_state() {
    assert_eq!(run(true, None).1, run(false, None).1);
}

/// The parallel engine records exactly the sequential engine's events.
#[test]
fn exec_order_events_identical_across_engine_modes() {
    let (seq, _) = run(true, None);
    for n in [2, 4] {
        let (par, _) = run(true, Some(n));
        assert_eq!(par, seq, "engine threads {n}");
    }
}

/// A user cancel and a cancel-all are logged as `user` cancels.
#[test]
fn exec_order_events_user_cancels() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, 1);
    fund(&ctx, &addr(1));
    ctx.set_order_events(true);
    NativeExecutor::execute_batch(
        &mut ctx,
        &[
            (addr(1), NativeAction::PlaceOrder(gtc(1, true, 90, 1))),
            (addr(1), NativeAction::PlaceOrder(gtc(2, true, 90, 2))),
        ],
    );
    let placed = ctx.take_order_events();
    let id = placed[0].order_id.unwrap();
    let r = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            (addr(1), NativeAction::CancelOrder { order_id: id }),
            (addr(1), NativeAction::CancelAllOrders { market_id: None }),
        ],
    );
    assert!(r.results.iter().all(|r| r.success), "{:?}", r.results);
    assert_eq!(
        summary(&ctx.take_order_events()),
        vec![
            (1, 1, S::Canceled, R::User, 0, 1),
            (2, 1, S::Canceled, R::User, 0, 2),
        ]
    );
}

/// Turning recording off drops what a book still logged.
#[test]
fn exec_order_events_off_drops_the_logs() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, 1);
    fund(&ctx, &addr(1));
    ctx.set_order_events(true);
    NativeExecutor::execute_batch(&mut ctx, &[(addr(1), NativeAction::PlaceOrder(gtc(1, true, 90, 1)))]);
    ctx.set_order_events(false);
    assert!(ctx.take_order_events().is_empty());
    ctx.set_order_events(true);
    assert!(ctx.take_order_events().is_empty());
}

/// The open orders and pending stops rebuilt from the events alone, as a
/// stream client applies them: `id -> (market, remaining, price)`.
#[derive(Default, Debug, PartialEq)]
struct Mirror {
    open: std::collections::BTreeMap<u128, (u64, i128, i128)>,
    stops: std::collections::BTreeSet<u128>,
}

impl Mirror {
    fn apply(&mut self, e: &OrderEvent) {
        let Some(id) = e.order_id else { return };
        match e.status {
            S::Open | S::Modified | S::PartiallyFilled => {
                if e.remaining > FixedPoint::ZERO {
                    self.open.insert(id, (e.market, e.remaining.raw(), e.price.raw()));
                } else {
                    self.open.remove(&id);
                }
            }
            S::Filled | S::Canceled | S::Rejected => {
                self.open.remove(&id);
                self.stops.remove(&id);
            }
            S::TriggerPending => {
                self.stops.insert(id);
            }
            S::Triggered => {
                self.stops.remove(&id);
            }
        }
    }

    /// What the books hold (what `torus_getOpenOrders` returns once saved).
    fn of_books(ctx: &NativeExecContext) -> Self {
        let mut m = Mirror::default();
        for (&mid, book) in ctx.order_books() {
            for (_, q) in book.bid_queues().chain(book.ask_queues()) {
                for o in q {
                    m.open.insert(o.id, (mid, o.remaining_qty.raw(), o.price.raw()));
                }
            }
            for s in book.pending_stops() {
                m.stops.insert(s.id);
            }
        }
        m
    }
}

/// Consistency with the open-orders snapshot: a client that applies every
/// event ends with exactly the books' open orders and pending stops, after
/// every block of a pseudo-random flow (rests, crosses, IOC, post-only,
/// stops, cancels, cancel-alls, modifies, self-trades, off-tick rejects), on
/// the sequential and the parallel engine alike.
#[test]
fn exec_order_events_rebuild_the_open_orders() {
    for threads in [None, Some(4)] {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db, 1);
        for t in 1..=3u8 {
            fund(&ctx, &addr(t));
        }
        ctx.set_order_events(true);
        let mut mirror = Mirror::default();
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let mut seed: u64 = 0x5eed;
        let mut rnd = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for block in 0..40 {
            let mut list = Vec::new();
            for _ in 0..12 {
                let t = addr(1 + rnd(3) as u8);
                let m = 1 + rnd(2);
                let is_buy = rnd(2) == 0;
                let px = 98 + rnd(5) as i64;
                let qty = 1 + rnd(4) as i64;
                let known: Vec<u128> = mirror.open.keys().copied().collect();
                let action = match rnd(20) {
                    0..=8 => NativeAction::PlaceOrder(gtc(m, is_buy, px, qty)),
                    9 | 10 => NativeAction::PlaceOrder(order(m, is_buy, px, qty, TimeInForce::IOC, None)),
                    11 => NativeAction::PlaceOrder(order(m, is_buy, px, qty, TimeInForce::PostOnly, None)),
                    12 => NativeAction::PlaceOrder(PlaceOrderParams {
                        order_type: OrderType::StopMarket {
                            trigger: fp(if is_buy { 101 } else { 99 }),
                        },
                        price: fp(if is_buy { 110 } else { 90 }),
                        ..gtc(m, is_buy, px, qty)
                    }),
                    13 => NativeAction::PlaceOrder(PlaceOrderParams {
                        // Off tick: refused before the book.
                        price: FixedPoint::from_raw(fp(px).raw() + 1),
                        ..gtc(m, is_buy, px, qty)
                    }),
                    14 | 15 if !known.is_empty() => NativeAction::CancelOrder {
                        order_id: known[rnd(known.len() as u64) as usize],
                    },
                    16 if !known.is_empty() => NativeAction::ModifyOrder {
                        order_id: known[rnd(known.len() as u64) as usize],
                        new_price: (rnd(2) == 0).then(|| fp(px)),
                        new_qty: Some(fp(qty)),
                    },
                    17 => NativeAction::CancelAllOrders {
                        market_id: (rnd(2) == 0).then_some(m),
                    },
                    _ => NativeAction::PlaceOrder(gtc(m, is_buy, px, qty)),
                };
                list.push((t, action));
            }
            match threads {
                Some(n) => NativeExecutor::execute_batch_engine_mode(&mut ctx, &list, n),
                None => NativeExecutor::execute_batch(&mut ctx, &list),
            };
            let events = ctx.take_order_events();
            for e in &events {
                mirror.apply(e);
                seen.insert(format!("{:?}/{:?}", e.status, e.reason));
            }
            assert_eq!(mirror, Mirror::of_books(&ctx), "block {block}, threads {threads:?}");
        }
        assert!(!mirror.open.is_empty(), "the flow leaves resting orders");
        // The flow is not vacuous: every kind of transition happened.
        for want in [
            "Open/None",
            "PartiallyFilled/None",
            "Filled/None",
            "TriggerPending/None",
            "Triggered/None",
            "Modified/User",
            "Canceled/User",
            "Canceled/SelfTrade",
            "Canceled/Unfilled",
            // (The off-tick placements are refused before the book: the
            // committer maps those from the results, `order_rejections`.)
            "Rejected/Reject(BadTriggerPx)",
            "Rejected/Reject(IocCancel)",
            "Rejected/Reject(BadAloPx)",
        ] {
            assert!(seen.contains(want), "{want} never happened: {seen:?}");
        }
    }
}
