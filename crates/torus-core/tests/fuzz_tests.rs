//! Proptest fuzz testing for OrderBook invariants (task 2.1b.3).

use proptest::prelude::*;
use torus_core::order_book::{OrderBook, OrderStatus};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn arb_order_params() -> impl Strategy<Value = (PlaceOrderParams, Address)> {
    (
        prop::bool::ANY,
        1..=500i64,
        1..=100i64,
        0..=3u8,
        0..=2u8,
        1..=20u8,
    )
        .prop_map(|(is_buy, price, qty, ot_sel, tif_sel, trader_id)| {
            let order_type = if ot_sel < 3 {
                OrderType::Limit
            } else {
                OrderType::Market
            };
            let time_in_force = match tif_sel {
                0 => TimeInForce::GTC,
                1 => TimeInForce::IOC,
                _ => TimeInForce::FOK,
            };
            let params = PlaceOrderParams {
                market_id: 1,
                is_buy,
                price: fp(price),
                quantity: fp(qty),
                order_type,
                time_in_force,
                reduce_only: false,
                client_order_id: None,
            };
            (params, Address::from([trader_id; 20]))
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// After every operation, all book invariants must hold.
    #[test]
    fn fuzz_book_invariants(
        orders in prop::collection::vec(arb_order_params(), 1..200)
    ) {
        let mut ob = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));

        for (i, (params, trader)) in orders.iter().enumerate() {
            let result = ob.place_order(params.clone(), *trader, i as u64);
            ob.verify_invariants();

            // All fills have positive quantity
            for fill in &result.fills {
                prop_assert!(
                    fill.quantity > FixedPoint::ZERO,
                    "Fill with non-positive quantity at op {}",
                    i
                );
            }
        }
    }

    /// cancel_all removes exactly the target trader's orders.
    #[test]
    fn fuzz_cancel_all_consistent(
        orders in prop::collection::vec(arb_order_params(), 10..100)
    ) {
        let mut ob = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
        let target = Address::from([1u8; 20]);

        for (i, (params, _)) in orders.iter().enumerate() {
            let trader = if i % 3 == 0 {
                target
            } else {
                Address::from([(i % 20 + 2) as u8; 20])
            };
            ob.place_order(params.clone(), trader, i as u64);
        }

        ob.verify_invariants();
        let before = ob.order_count();
        let cancelled = ob.cancel_all(target, None);
        let after = ob.order_count();
        ob.verify_invariants();

        prop_assert_eq!(before - cancelled.len(), after);

        let again = ob.cancel_all(target, None);
        prop_assert!(again.is_empty());
    }

    /// Mixed place/cancel/modify sequence preserves all invariants.
    #[test]
    fn fuzz_place_cancel_modify_sequence(
        orders in prop::collection::vec(arb_order_params(), 1..100)
    ) {
        let mut ob = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
        let mut resting_ids: Vec<u128> = Vec::new();

        for (i, (params, trader)) in orders.iter().enumerate() {
            let action = i % 10;
            if action < 7 {
                let result = ob.place_order(params.clone(), *trader, i as u64);
                if result.status == OrderStatus::Resting
                    || result.status == OrderStatus::PartiallyFilled
                {
                    resting_ids.push(result.order_id);
                }
            } else if action < 9 {
                if let Some(id) = resting_ids.pop() {
                    let _ = ob.cancel_order(id);
                }
            } else if let Some(&id) = resting_ids.last() {
                let _ = ob.modify_order(id, None, Some(fp(1)));
            }
            ob.verify_invariants();
        }
    }
}
