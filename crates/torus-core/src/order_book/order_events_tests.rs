//! s139: the book's order event log (`orderUpdates` stream). Off by default
//! (nothing logged); on, every public mutation logs what happened to each
//! order it touched, in execution order. Logging never changes the book.

use super::*;
use torus_state::order_events::{OrderEvent, OrderEventReason as R, OrderEventStatus as S};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

const A: u8 = 0xa1;
const B: u8 = 0xb2;

fn book() -> OrderBook {
    let mut b = OrderBook::new(1, fp(1), fp(1));
    b.set_order_events(true);
    b
}

fn params(is_buy: bool, price: i64, qty: i64, tif: TimeInForce, cloid: Option<u128>) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn gtc(is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    params(is_buy, price, qty, TimeInForce::GTC, None)
}

/// `(order id, trader byte, status, reason, filled, remaining)` of each event.
fn summary(ev: &[OrderEvent]) -> Vec<(Option<OrderId>, u8, S, R, i64, i64)> {
    ev.iter()
        .map(|e| {
            (
                e.order_id,
                e.trader.0[0],
                e.status,
                e.reason,
                (e.filled.raw() / FixedPoint::SCALE) as i64,
                (e.remaining.raw() / FixedPoint::SCALE) as i64,
            )
        })
        .collect()
}

#[test]
fn order_events_off_by_default_logs_nothing() {
    let mut b = OrderBook::new(1, fp(1), fp(1));
    b.place_order(gtc(false, 100, 5), addr(A), 1);
    b.place_order(gtc(true, 100, 2), addr(B), 1);
    b.cancel_all(addr(A), None);
    assert!(b.take_order_events().is_empty());
}

#[test]
fn order_events_rest_then_cancel() {
    let mut b = book();
    let r = b.place_order(params(false, 100, 5, TimeInForce::GTC, Some(7)), addr(A), 1);
    b.cancel_order(r.order_id).unwrap();
    let ev = b.take_order_events();
    assert_eq!(
        summary(&ev),
        vec![
            (Some(r.order_id), A, S::Open, R::None, 0, 5),
            (Some(r.order_id), A, S::Canceled, R::User, 0, 5),
        ]
    );
    assert!(ev.iter().all(|e| e.cloid == Some(7) && e.market == 1 && e.side == Side::Sell));
    assert_eq!(ev[0].price, fp(100));
    assert_eq!(ev[0].orig_qty, fp(5));
    // Drained.
    assert!(b.take_order_events().is_empty());
}

#[test]
fn order_events_maker_partial_fill_and_taker_filled() {
    let mut b = book();
    let m = b.place_order(gtc(false, 100, 5), addr(A), 1).order_id;
    b.take_order_events();
    let t = b.place_order(gtc(true, 100, 2), addr(B), 1).order_id;
    assert_eq!(
        summary(&b.take_order_events()),
        vec![
            (Some(m), A, S::PartiallyFilled, R::None, 2, 3),
            (Some(t), B, S::Filled, R::None, 2, 0),
        ]
    );
}

#[test]
fn order_events_taker_partial_rests_and_maker_filled() {
    let mut b = book();
    let m = b.place_order(gtc(false, 100, 2), addr(A), 1).order_id;
    b.take_order_events();
    let t = b.place_order(gtc(true, 100, 5), addr(B), 1).order_id;
    assert_eq!(
        summary(&b.take_order_events()),
        vec![
            (Some(m), A, S::Filled, R::None, 2, 0),
            (Some(t), B, S::PartiallyFilled, R::None, 2, 3),
        ]
    );
}

#[test]
fn order_events_ioc_partial_fill_cancels_remainder() {
    let mut b = book();
    b.place_order(gtc(false, 100, 2), addr(A), 1);
    b.take_order_events();
    let t = b.place_order(params(true, 100, 5, TimeInForce::IOC, None), addr(B), 1).order_id;
    let ev = b.take_order_events();
    assert_eq!(
        summary(&ev)[1..],
        [
            (Some(t), B, S::PartiallyFilled, R::None, 2, 3),
            (Some(t), B, S::Canceled, R::Unfilled, 0, 3),
        ]
    );
}

#[test]
fn order_events_book_rejects_carry_the_id_and_reason() {
    let mut b = book();
    let ioc = b.place_order(params(true, 100, 1, TimeInForce::IOC, Some(9)), addr(B), 1).order_id;
    b.place_order(gtc(false, 100, 1), addr(A), 1);
    let alo = b.place_order(params(true, 100, 1, TimeInForce::PostOnly, None), addr(B), 1).order_id;
    let ev = b.take_order_events();
    let s = summary(&ev);
    assert_eq!(s[0], (Some(ioc), B, S::Rejected, R::Reject(FailureReason::IocCancel), 0, 0));
    assert_eq!(ev[0].cloid, Some(9));
    assert_eq!(s[2], (Some(alo), B, S::Rejected, R::Reject(FailureReason::BadAloPx), 0, 0));
}

#[test]
fn order_events_self_trade_cancels_the_maker() {
    let mut b = book();
    let m = b.place_order(gtc(false, 100, 2), addr(A), 1).order_id;
    b.take_order_events();
    let t = b.place_order(gtc(true, 100, 1), addr(A), 1).order_id;
    assert_eq!(
        summary(&b.take_order_events()),
        vec![
            (Some(m), A, S::Canceled, R::SelfTrade, 0, 2),
            (Some(t), A, S::Open, R::None, 0, 1),
        ]
    );
}

#[test]
fn order_events_stop_pending_then_triggered() {
    let mut b = book();
    // Last trade 100.
    b.place_order(gtc(false, 100, 1), addr(A), 1);
    b.place_order(gtc(true, 100, 1), addr(B), 1);
    b.take_order_events();
    let stop = PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(105) },
        ..params(true, 110, 1, TimeInForce::GTC, Some(3))
    };
    let sid = b.place_order(stop, addr(B), 1).order_id;
    let ev = b.take_order_events();
    assert_eq!(summary(&ev), vec![(Some(sid), B, S::TriggerPending, R::None, 0, 1)]);
    assert_eq!(ev[0].trigger_price, Some(fp(105)));
    assert_eq!(ev[0].cloid, Some(3));
    // A trade at 105 fires it.
    b.place_order(gtc(false, 105, 1), addr(A), 1);
    let r = b.place_order(gtc(true, 105, 1), addr(0xcc), 1);
    assert_eq!(r.triggered_stops.len(), 1);
    let ev = b.take_order_events();
    let last = ev.last().unwrap();
    assert_eq!((last.order_id, last.status, last.trigger_price), (Some(sid), S::Triggered, Some(fp(105))));
}

#[test]
fn order_events_cancel_all_logs_orders_and_stops_with_the_cause() {
    let mut b = book();
    b.place_order(gtc(false, 100, 1), addr(A), 1);
    b.place_order(gtc(true, 100, 1), addr(B), 1);
    let o1 = b.place_order(gtc(true, 90, 2), addr(A), 1).order_id;
    let stop = PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(80) },
        ..params(false, 70, 1, TimeInForce::GTC, None)
    };
    let s1 = b.place_order(stop, addr(A), 1).order_id;
    b.take_order_events();
    let prev = b.set_cancel_cause(R::Liquidation);
    assert_eq!(prev, R::User);
    b.cancel_all(addr(A), None);
    b.set_cancel_cause(prev);
    let mut s = summary(&b.take_order_events());
    s.sort_by_key(|e| e.0);
    assert_eq!(
        s,
        vec![
            (Some(o1), A, S::Canceled, R::Liquidation, 0, 2),
            (Some(s1), A, S::Canceled, R::Liquidation, 0, 1),
        ]
    );
}

#[test]
fn order_events_take_pending_stops_and_cancel_stop() {
    let mut b = book();
    let stop = |trigger| PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(trigger) },
        ..params(true, 200, 1, TimeInForce::GTC, None)
    };
    let s1 = b.place_order(stop(150), addr(A), 1).order_id;
    let s2 = b.place_order(stop(160), addr(B), 1).order_id;
    b.take_order_events();
    assert_eq!(b.take_pending_stops(&addr(A)).len(), 1);
    b.cancel_stop(s2).unwrap();
    assert_eq!(
        summary(&b.take_order_events()),
        vec![
            (Some(s1), A, S::Canceled, R::User, 0, 1),
            (Some(s2), B, S::Canceled, R::User, 0, 1),
        ]
    );
}

#[test]
fn order_events_modify_logs_the_new_size_and_price() {
    let mut b = book();
    let id = b.place_order(gtc(true, 90, 5), addr(A), 1).order_id;
    b.take_order_events();
    b.modify_order(id, None, Some(fp(3))).unwrap();
    b.modify_order(id, Some(fp(91)), None).unwrap();
    let ev = b.take_order_events();
    assert_eq!(
        summary(&ev),
        vec![
            (Some(id), A, S::Modified, R::User, 0, 3),
            (Some(id), A, S::Modified, R::User, 0, 3),
        ]
    );
    assert_eq!(ev[1].price, fp(91));
}

#[test]
fn order_events_cancel_all_many_logs_every_sender() {
    let mut b = book();
    let mut ids = Vec::new();
    for t in 1..=3u8 {
        ids.push(b.place_order(gtc(true, 90 - t as i64, 1), addr(t), 1).order_id);
        ids.push(b.place_order(gtc(false, 110 + t as i64, 1), addr(t), 1).order_id);
    }
    b.take_order_events();
    b.cancel_all_many(&[addr(1), addr(2), addr(3)]);
    let mut got: Vec<_> = b
        .take_order_events()
        .iter()
        .map(|e| (e.order_id.unwrap(), e.status, e.reason))
        .collect();
    got.sort_by_key(|e| e.0);
    let want: Vec<_> = ids.iter().map(|&id| (id, S::Canceled, R::User)).collect();
    assert_eq!(got, want);
}

/// The log is bookkeeping only: the same placements with it on and off
/// leave byte-identical books (Classic blob, order rows, level rows).
#[test]
fn order_events_never_change_the_book() {
    let run = |on: bool| {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.set_order_events(on);
        b.place_order(gtc(false, 100, 5), addr(A), 1);
        b.place_order(gtc(false, 101, 5), addr(A), 1);
        b.place_order(gtc(true, 101, 7), addr(B), 2);
        b.place_order(params(true, 102, 9, TimeInForce::IOC, None), addr(B), 3);
        let id = b.place_order(gtc(true, 95, 4), addr(A), 4).order_id;
        b.modify_order(id, None, Some(fp(2))).unwrap();
        b.place_order(gtc(true, 96, 4), addr(B), 5);
        b.cancel_all(addr(B), None);
        let rows = b.take_row_ops();
        (borsh::to_vec(&b).unwrap(), rows)
    };
    assert_eq!(run(true), run(false));
}

/// Cost probe (run by hand: `--ignored --nocapture`): ns per placement of a
/// crossing flow (every other order fills one maker) with the log off and on,
/// interleaved rounds, median of each.
#[test]
#[ignore]
fn order_events_cost_ubench() {
    const N: i64 = 200_000;
    let flow = |on: bool| {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.set_order_events(on);
        let t = std::time::Instant::now();
        for i in 0..N {
            let px = 1_000 + (i % 50);
            if i % 2 == 0 {
                b.place_order(gtc(false, px, 1), addr(A), 1);
            } else {
                b.place_order(gtc(true, px, 1), addr(B), 1);
            }
            if i % 1_000 == 999 {
                std::hint::black_box(b.take_order_events());
            }
        }
        t.elapsed().as_nanos() as f64 / N as f64
    };
    let (mut off, mut on) = (Vec::new(), Vec::new());
    for _ in 0..7 {
        off.push(flow(false));
        on.push(flow(true));
    }
    off.sort_by(f64::total_cmp);
    on.sort_by(f64::total_cmp);
    eprintln!(
        "order_events_cost_ubench: ns/placement off {:.1} on {:.1} (median of 7, N={N})",
        off[3], on[3]
    );
}

/// Codex P2 (s139): an accepted reduce-only placement clamped to the position
/// reports the clamped `originalQty` (the stored order's), not the request.
#[test]
fn order_events_reduce_only_clamp_reports_clamped_orig_qty() {
    let mut b = book();
    let mut ro = ReduceOnlyPositions::new();
    // A long 2: a reduce-only sell of 5 rests as 2.
    ro.insert(addr(A), fp(2));
    b.set_reduce_only_positions(ro);
    let mut p = gtc(false, 100, 5);
    p.reduce_only = true;
    let r = b.place_order(p, addr(A), 1);
    assert!(r.reject.is_none());
    let o = b.get_order(r.order_id).expect("rests");
    assert_eq!(o.original_qty, fp(2));
    let ev = b.take_order_events();
    assert_eq!(summary(&ev), vec![(Some(r.order_id), A, S::Open, R::None, 0, 2)]);
    assert_eq!(ev[0].orig_qty, fp(2), "event originalQty == book originalQty");
    // A later cancel reports the same originalQty.
    b.cancel_order(r.order_id).unwrap();
    assert_eq!(b.take_order_events()[0].orig_qty, fp(2));
}
