//! Item 6 M1 (review rows 40-42): the placement shape rule shared by the
//! executor (pre-book) and the RPC (intake).

use super::*;

const S: i128 = FixedPoint::SCALE;

fn fpr(raw: i128) -> FixedPoint {
    FixedPoint::from_raw(raw)
}

fn order(price: i128, qty: i128, order_type: OrderType) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fpr(price),
        quantity: fpr(qty),
        order_type,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Lot first (every order type), then the tick for a `Limit` price and for
/// a `StopLimit` limit (row 40); a Market cap and a StopMarket cap are not
/// tick-checked; tick <= 0 disables the tick check.
#[test]
fn shape_violation_rules() {
    let (tick, lot) = (fpr(S / 2), fpr(S / 10));
    let off = 100 * S + S / 4;
    let trig = fpr(90 * S);
    assert_eq!(
        shape_violation(&order(off, S / 10 - 1, OrderType::Limit), tick, lot),
        Some(ShapeViolation::BelowLot { quantity: fpr(S / 10 - 1), lot })
    );
    assert_eq!(
        shape_violation(&order(off, S, OrderType::Limit), tick, lot),
        Some(ShapeViolation::OffTick { price: fpr(off), tick })
    );
    let stop_limit = OrderType::StopLimit { trigger: trig, limit: fpr(off) };
    assert_eq!(
        shape_violation(&order(0, S, stop_limit), tick, lot),
        Some(ShapeViolation::OffTick { price: fpr(off), tick })
    );
    let on_tick_stop_limit = OrderType::StopLimit { trigger: trig, limit: fpr(100 * S) };
    assert_eq!(shape_violation(&order(off, S, on_tick_stop_limit), tick, lot), None);
    assert_eq!(shape_violation(&order(off, S, OrderType::Market), tick, lot), None);
    assert_eq!(shape_violation(&order(off, S, OrderType::StopMarket { trigger: trig }), tick, lot), None);
    assert_eq!(shape_violation(&order(100 * S, S / 10, OrderType::Limit), tick, lot), None);
    assert_eq!(shape_violation(&order(off + 7, S, OrderType::Limit), FixedPoint::ZERO, lot), None);
    assert_eq!(shape_violation(&order(off + 7, S, stop_limit), fpr(-S), lot), None);
}

/// One text for the executor and the RPC; the modify path uses the same
/// tail after "modify rejected: ".
#[test]
fn shape_violation_messages() {
    let lot = ShapeViolation::BelowLot { quantity: fpr(S / 10 - 1), lot: fpr(S / 10) };
    let tick = ShapeViolation::OffTick { price: fpr(100 * S + S / 4), tick: fpr(S / 2) };
    assert_eq!(lot.to_string(), "quantity 0.09999999 below the lot size 0.10000000");
    assert_eq!(tick.to_string(), "price 100.25000000 is not a multiple of the tick 0.50000000");
    assert_eq!(
        lot.placement_message(),
        "order rejected: quantity 0.09999999 below the lot size 0.10000000"
    );
    assert_eq!(
        tick.placement_message(),
        "order rejected: price 100.25000000 is not a multiple of the tick 0.50000000"
    );
}
