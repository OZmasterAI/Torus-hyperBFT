//! s94 option 2: the placement price band rule shared by the executor
//! (pre-book) and the RPC (intake), and the band reference (mark, else the
//! book fallback clamped to the last mark).

use super::*;

fn band() -> PriceBand {
    PriceBand { reference: fp(100), bps: 5_000 }
}

const S: i128 = FixedPoint::SCALE;

fn fpr(raw: i128) -> FixedPoint {
    FixedPoint::from_raw(raw)
}

fn fp(v: i64) -> FixedPoint {
    fpr(v as i128 * S)
}

fn order(order_type: OrderType, price: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price,
        quantity: fp(1),
        order_type,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn limit(price: FixedPoint) -> PlaceOrderParams {
    order(OrderType::Limit, price)
}


/// ±50% of 100 = [50, 150], both ends inside, one raw unit beyond outside;
/// the same for every time in force; odd bps print with two decimals.
#[test]
fn the_band_boundary_is_inclusive_at_exactly_the_band() {
    for (price, inside) in [
        (150 * S, true),
        (150 * S + 1, false),
        (50 * S, true),
        (50 * S - 1, false),
        (100 * S, true),
    ] {
        for tif in [TimeInForce::GTC, TimeInForce::IOC, TimeInForce::FOK, TimeInForce::PostOnly] {
            let p = PlaceOrderParams { time_in_force: tif, ..limit(fpr(price)) };
            let got = price_band_violation(&p, band());
            assert_eq!(got.is_none(), inside, "price {price} {tif:?}");
        }
    }
    let v = price_band_violation(&limit(fp(151)), PriceBand { reference: fp(100), bps: 1_250 }).unwrap();
    assert_eq!(
        v.placement_message(),
        "order rejected: price 151.00000000 is more than 12.50% from the reference price 100.00000000"
    );
    // A huge reference never overflows.
    let big = PriceBand { reference: FixedPoint::MAX, bps: 9_000 };
    assert!(big.contains(FixedPoint::MAX) && !big.contains(fp(1)));
}

/// What is banded: a Limit price; a StopLimit's limit (first) and trigger; a
/// StopMarket's trigger (its price is the market slippage cap). A Market
/// order is not banded at all (out of scope: P2 slippage cap).
#[test]
fn stop_triggers_and_the_stop_limit_limit_are_banded_market_caps_are_not() {
    let sl = |trigger: i64, limit: i64| order(OrderType::StopLimit { trigger: fp(trigger), limit: fp(limit) }, fp(limit));
    assert_eq!(price_band_violation(&sl(120, 151), band()).map(|v| v.price), Some(fp(151)));
    assert_eq!(price_band_violation(&sl(151, 120), band()).map(|v| v.price), Some(fp(151)));
    assert_eq!(price_band_violation(&sl(160, 151), band()).map(|v| v.price), Some(fp(151)), "limit first");
    assert_eq!(price_band_violation(&sl(120, 140), band()), None);
    let sm = |trigger: i64, cap: i64| order(OrderType::StopMarket { trigger: fp(trigger) }, fp(cap));
    assert_eq!(price_band_violation(&sm(49, 40), band()).map(|v| v.price), Some(fp(49)));
    assert_eq!(price_band_violation(&sm(60, 1_000), band()), None, "the cap is not banded");
    assert_eq!(price_band_violation(&order(OrderType::Market, fp(1_000_000)), band()), None);
}

/// The reference: a usable mark wins; without one the median of the book's
/// best bid / best ask / last trade (mid of two, the one of one, the last
/// mark of none) clamped to ±10% of the last mark; no last mark: none.
#[test]
fn the_reference_is_the_mark_else_the_book_clamped_to_the_last_mark() {
    let r = |mark: Option<i64>, last: Option<i64>, bid: Option<i64>, ask: Option<i64>, trade: Option<i64>| {
        band_reference(mark.map(fp), last.map(fp), bid.map(fp), ask.map(fp), trade.map(fp))
    };
    assert_eq!(r(Some(100), Some(90), Some(1), Some(2), Some(3)), Some(fp(100)), "a usable mark wins");
    assert_eq!(r(None, Some(100), Some(95), Some(107), Some(104)), Some(fp(104)), "median of 3");
    assert_eq!(r(None, Some(100), Some(95), Some(107), Some(96)), Some(fp(96)));
    assert_eq!(r(None, Some(100), Some(95), Some(107), None), Some(fp(101)), "mid of 2");
    assert_eq!(r(None, Some(100), None, None, Some(103)), Some(fp(103)), "the one of 1");
    assert_eq!(r(None, Some(100), None, None, None), Some(fp(100)), "empty book: the last mark");
    assert_eq!(r(None, Some(100), Some(150), Some(170), Some(160)), Some(fp(110)), "clamped up to +10%");
    assert_eq!(r(None, Some(100), Some(10), Some(20), Some(15)), Some(fp(90)), "clamped down to -10%");
    assert_eq!(r(None, None, Some(95), Some(107), Some(104)), None, "never marked: no band");
    assert_eq!(r(Some(0), None, None, None, None), None);
    // Odd raw values average without overflow (floor of the half-sum).
    let mid = band_reference(None, Some(fp(100)), Some(fpr(100 * S + 1)), Some(fpr(100 * S + 2)), None);
    assert_eq!(mid, Some(fpr(100 * S + 1)));
    let mut b = OrderBook::new(1, fp(1), fp(1));
    b.place_order(limit(fp(95)), Address::new([1; 20]), 1);
    b.place_order(PlaceOrderParams { is_buy: false, ..limit(fp(107)) }, Address::new([2; 20]), 2);
    assert_eq!(b.band_reference(None, Some(fp(100))), Some(fp(101)));
    assert_eq!(b.band_reference(Some(fp(99)), Some(fp(100))), Some(fp(99)));
}

/// The stored governance value: a decimal in 100..=9,000, else 5,000.
#[test]
fn the_band_parameter_parses_with_a_default() {
    use torus_types::price_band_bps as bps;
    assert_eq!(bps(None), 5_000);
    assert_eq!(bps(Some(b"1000")), 1_000);
    assert_eq!(bps(Some(b"100")), 100);
    assert_eq!(bps(Some(b"9000")), 9_000);
    for bad in [&b"99"[..], b"9001", b"0", b"-5", b"abc", b"", b"50.5"] {
        assert_eq!(bps(Some(bad)), 5_000, "{:?}", std::str::from_utf8(bad));
    }
}
