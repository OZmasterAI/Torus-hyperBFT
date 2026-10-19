//! 8k L3.3 (`TORUS_BOOK_FAST_ENCODE`): `OrderBook::encode_classic` must be
//! byte-identical to `borsh::to_vec(book)` (the custom `BorshSerialize` impls
//! of `Order`, `StopOrder` and `OrderBook`) for every book, and
//! `classic_encoded_len` must be its exact length.

use super::*;
use proptest::prelude::*;

fn fp_raw(r: i128) -> FixedPoint {
    FixedPoint::from_raw(r)
}

/// One resting order's free fields (id, side, price level are assigned by the
/// builder so levels collide and both sides fill).
#[derive(Clone, Debug)]
struct OrderSpec {
    buy: bool,
    level: u8,
    remaining: i128,
    original: i128,
    order_type: u8,
    trigger: i128,
    limit: i128,
    tif: u8,
    timestamp: u64,
    reduce_only: bool,
    cloid: Option<u128>,
    trader: u8,
}

#[derive(Clone, Debug)]
struct StopSpec {
    buy: bool,
    trigger: i128,
    price_tag: u8,
    limit_or_cap: i128,
    quantity: i128,
    tif: u8,
    timestamp: u64,
    reduce_only: bool,
    cloid: Option<u128>,
    trader: u8,
}

fn order_spec() -> impl Strategy<Value = OrderSpec> {
    (
        (any::<bool>(), 0u8..12, any::<i128>(), any::<i128>(), 0u8..4),
        (any::<i128>(), any::<i128>(), 0u8..4, any::<u64>()),
        (
            any::<bool>(),
            proptest::option::of(any::<u128>()),
            any::<u8>(),
        ),
    )
        .prop_map(
            |(
                (buy, level, remaining, original, order_type),
                (trigger, limit, tif, timestamp),
                (reduce_only, cloid, trader),
            )| OrderSpec {
                buy,
                level,
                remaining,
                original,
                order_type,
                trigger,
                limit,
                tif,
                timestamp,
                reduce_only,
                cloid,
                trader,
            },
        )
}

fn stop_spec() -> impl Strategy<Value = StopSpec> {
    (
        (
            any::<bool>(),
            any::<i128>(),
            0u8..3,
            1i128..i128::MAX,
            any::<i128>(),
        ),
        (0u8..4, any::<u64>(), any::<bool>()),
        (proptest::option::of(any::<u128>()), any::<u8>()),
    )
        .prop_map(
            |(
                (buy, trigger, price_tag, limit_or_cap, quantity),
                (tif, timestamp, reduce_only),
                (cloid, trader),
            )| StopSpec {
                buy,
                trigger,
                price_tag,
                limit_or_cap,
                quantity,
                tif,
                timestamp,
                reduce_only,
                cloid,
                trader,
            },
        )
}

fn tif_of(t: u8) -> TimeInForce {
    match t {
        0 => TimeInForce::GTC,
        1 => TimeInForce::IOC,
        2 => TimeInForce::FOK,
        _ => TimeInForce::PostOnly,
    }
}

fn side_of(buy: bool) -> Side {
    if buy {
        Side::Buy
    } else {
        Side::Sell
    }
}

#[allow(clippy::too_many_arguments)]
fn build_book(
    market_id: MarketId,
    tick: i128,
    lot: i128,
    next_id: u128,
    ltp: Option<i128>,
    orders: &[OrderSpec],
    stops: &[StopSpec],
) -> OrderBook {
    let mut book = OrderBook::new(market_id, fp_raw(tick), fp_raw(lot));
    book.next_id = next_id;
    book.last_trade_price = ltp.map(fp_raw);
    for (i, s) in orders.iter().enumerate() {
        let order_type = match s.order_type {
            0 => OrderType::Limit,
            1 => OrderType::Market,
            2 => OrderType::StopMarket {
                trigger: fp_raw(s.trigger),
            },
            _ => OrderType::StopLimit {
                trigger: fp_raw(s.trigger),
                limit: fp_raw(s.limit),
            },
        };
        // Unique (trader, cloid): the cloid index debug-asserts it.
        let cloid = s.cloid.map(|c| c.wrapping_add(i as u128));
        book.insert_order(Order {
            id: 1_000 + i as u128,
            trader: Address::repeat_byte(s.trader),
            side: side_of(s.buy),
            price: fp_raw(100 + i128::from(s.level) * 7),
            remaining_qty: fp_raw(s.remaining),
            original_qty: fp_raw(s.original),
            order_type,
            time_in_force: tif_of(s.tif),
            timestamp: s.timestamp,
            reduce_only: s.reduce_only,
            client_order_id: cloid,
        });
    }
    for (i, s) in stops.iter().enumerate() {
        let (limit_price, price_cap) = match s.price_tag {
            0 => (None, FixedPoint::ZERO),
            1 => (Some(fp_raw(s.limit_or_cap)), FixedPoint::ZERO),
            _ => (None, fp_raw(s.limit_or_cap)),
        };
        // A stop's trader never equals an order's (`repeat_byte`), so the
        // cloid index has one key per cloid, as on a live book.
        let mut trader = [s.trader; 20];
        trader[18] = 0xEE;
        trader[19] = 0x11;
        let trader = Address::from(trader);
        let id = 900_000 + i as u128;
        OrderBook::index_cloid(&mut book.cloids, trader, s.cloid, id);
        book.pending_stops.push(StopOrder {
            id,
            trader,
            market_id,
            side: side_of(s.buy),
            trigger_price: fp_raw(s.trigger),
            limit_price,
            price_cap,
            quantity: fp_raw(s.quantity),
            time_in_force: tif_of(s.tif),
            timestamp: s.timestamp,
            reduce_only: s.reduce_only,
            client_order_id: s.cloid,
        });
    }
    book
}

/// True when a resting order carries a stop order type (the book never
/// rests one; the capacity hint does not count their extra bytes).
fn rests_stop_types(book: &OrderBook) -> bool {
    book.bids
        .values()
        .chain(book.asks.values())
        .flatten()
        .any(|o| !matches!(o.order_type, OrderType::Limit | OrderType::Market))
}

fn assert_same(book: &OrderBook) {
    let want = borsh::to_vec(book).unwrap();
    let got = book.encode_classic();
    assert_eq!(got.len(), want.len(), "length");
    assert_eq!(
        book.classic_encoded_len(),
        want.len(),
        "classic_encoded_len"
    );
    assert!(got == want, "bytes differ");
    let exact = book.encode_classic_size_pass();
    assert!(exact == want, "size-pass variant bytes differ");
    assert_eq!(
        exact.capacity(),
        exact.len(),
        "size pass: capacity is exact"
    );
    // One allocation, no slack, without a pass over the orders.
    if !rests_stop_types(book) {
        assert_eq!(book.classic_capacity_hint(), want.len(), "capacity hint");
        assert_eq!(got.capacity(), got.len(), "capacity is exact");
    }
    // And it decodes back to the same bytes.
    let back = OrderBook::try_from_slice(&got).unwrap();
    assert!(borsh::to_vec(&back).unwrap() == want, "round trip");
}

#[test]
fn empty_book_matches_borsh() {
    let book = OrderBook::new(7, fp_raw(1), fp_raw(1));
    assert_same(&book);
}

#[test]
fn every_variant_matches_borsh() {
    // One order per (order type, TIF, cloid) and one stop per price tag, on
    // both sides, with and without a last trade price.
    let mut orders = Vec::new();
    for ot in 0..4u8 {
        for tif in 0..4u8 {
            for cloid in [None, Some(u128::MAX - 3)] {
                orders.push(OrderSpec {
                    buy: (ot + tif) % 2 == 0,
                    level: ot * 2 + tif % 2,
                    remaining: -5 + i128::from(tif),
                    original: i128::MAX - i128::from(ot),
                    order_type: ot,
                    trigger: i128::MIN + 1,
                    limit: 42,
                    tif,
                    timestamp: u64::MAX - u64::from(tif),
                    reduce_only: cloid.is_some(),
                    cloid,
                    trader: ot * 4 + tif,
                });
            }
        }
    }
    let stops: Vec<StopSpec> = (0..3u8)
        .flat_map(|tag| {
            [None, Some(9u128)].map(|cloid| StopSpec {
                buy: tag % 2 == 0,
                trigger: 123,
                price_tag: tag,
                limit_or_cap: 456,
                quantity: 789,
                tif: tag,
                timestamp: 5,
                reduce_only: true,
                cloid,
                trader: 200 + tag,
            })
        })
        .collect();
    for ltp in [None, Some(-77i128), Some(i128::MAX)] {
        assert_same(&build_book(u64::MAX, 3, 5, u128::MAX, ltp, &orders, &stops));
        assert_same(&build_book(1, 3, 5, 1, ltp, &orders, &[]));
        assert_same(&build_book(1, 3, 5, 1, ltp, &[], &stops));
    }
}

#[test]
fn deep_plain_book_matches_borsh() {
    // The bench shape: plain GTC limits, many per level, both sides.
    let orders: Vec<OrderSpec> = (0..5_000u32)
        .map(|i| OrderSpec {
            buy: i % 3 != 0,
            level: (i % 11) as u8,
            remaining: i128::from(i) * 1_000_000,
            original: i128::from(i) * 2_000_000,
            order_type: 0,
            trigger: 0,
            limit: 0,
            tif: 0,
            timestamp: u64::from(i),
            reduce_only: false,
            cloid: None,
            trader: (i % 251) as u8,
        })
        .collect();
    assert_same(&build_book(3, 10, 1, 6_000, Some(1_000), &orders, &[]));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Live-book shape (no resting stop types): the hint is exact too.
    #[test]
    fn encode_classic_hint_exact_without_resting_stop_types(
        ltp in proptest::option::of(any::<i128>()),
        orders in proptest::collection::vec(order_spec(), 0..60),
        stops in proptest::collection::vec(stop_spec(), 0..6),
    ) {
        let orders: Vec<OrderSpec> = orders
            .into_iter()
            .map(|mut o| { o.order_type %= 2; o })
            .collect();
        let book = build_book(5, 1, 1, 77, ltp, &orders, &stops);
        let want = borsh::to_vec(&book).unwrap();
        let got = book.encode_classic();
        prop_assert!(got == want, "bytes differ");
        prop_assert_eq!(book.classic_capacity_hint(), want.len());
        prop_assert_eq!(got.capacity(), got.len());
    }

    #[test]
    fn encode_classic_matches_borsh(
        market_id in any::<u64>(),
        tick in any::<i128>(),
        lot in any::<i128>(),
        next_id in any::<u128>(),
        ltp in proptest::option::of(any::<i128>()),
        orders in proptest::collection::vec(order_spec(), 0..60),
        stops in proptest::collection::vec(stop_spec(), 0..6),
    ) {
        let book = build_book(market_id, tick, lot, next_id, ltp, &orders, &stops);
        let want = borsh::to_vec(&book).unwrap();
        let got = book.encode_classic();
        prop_assert_eq!(book.classic_encoded_len(), want.len());
        prop_assert!(got == want, "bytes differ");
        prop_assert!(book.encode_classic_size_pass() == want, "size-pass bytes differ");
        if !rests_stop_types(&book) {
            prop_assert_eq!(book.classic_capacity_hint(), want.len());
            prop_assert_eq!(got.capacity(), got.len());
        }
    }
}
