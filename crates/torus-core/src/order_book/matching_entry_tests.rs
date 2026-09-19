// Frozen 080c4fa outer traversal; match_at_level remains the shared unchanged kernel.
use super::*;

impl OrderBook {
    fn execute_match_legacy(
        &mut self,
        taker: &mut Order,
        is_market: bool,
    ) -> (Vec<Fill>, Vec<Order>) {
        let mut fills = Vec::new();
        let mut self_trade_cancels = Vec::new();
        let cache_on = self.level_hash_cache.is_some();
        let chunked_on = self.level_hash_chunked;

        match taker.side {
            Side::Buy => {
                while taker.remaining_qty > FixedPoint::ZERO {
                    let best_ask = match self.asks.keys().next().copied() {
                        Some(p) => p,
                        None => break,
                    };
                    if !is_market && best_ask > taker.price {
                        break;
                    }
                    let queue = self.asks.get_mut(&best_ask).unwrap();
                    Self::match_at_level(
                        taker,
                        queue,
                        best_ask,
                        Side::Sell,
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut self.order_index,
                        &mut self.trader_orders,
                        &mut self.order_seq,
                        &mut self.row_journal,
                        &mut self.level_journal,
                        &mut self.level_epoch,
                        cache_on,
                        &mut self.dirty_chunks,
                        chunked_on,
                    );
                    if self.asks.get(&best_ask).is_none_or(|q| q.is_empty()) {
                        self.asks.remove(&best_ask);
                    }
                }
            }
            Side::Sell => {
                while taker.remaining_qty > FixedPoint::ZERO {
                    let best_bid = match self.bids.keys().next_back().copied() {
                        Some(p) => p,
                        None => break,
                    };
                    if !is_market && best_bid < taker.price {
                        break;
                    }
                    let queue = self.bids.get_mut(&best_bid).unwrap();
                    Self::match_at_level(
                        taker,
                        queue,
                        best_bid,
                        Side::Buy,
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut self.order_index,
                        &mut self.trader_orders,
                        &mut self.order_seq,
                        &mut self.row_journal,
                        &mut self.level_journal,
                        &mut self.level_epoch,
                        cache_on,
                        &mut self.dirty_chunks,
                        chunked_on,
                    );
                    if self.bids.get(&best_bid).is_none_or(|q| q.is_empty()) {
                        self.bids.remove(&best_bid);
                    }
                }
            }
        }

        (fills, self_trade_cancels)
    }
}

fn fp(n: i128) -> FixedPoint {
    FixedPoint::from_raw(n * FixedPoint::SCALE)
}

fn order(id: u128, trader: u8, side: Side, price: i128, qty: i128) -> Order {
    Order {
        id,
        trader: Address::repeat_byte(trader),
        side,
        price: fp(price),
        remaining_qty: fp(qty),
        original_qty: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        timestamp: 17,
        reduce_only: false,
        client_order_id: Some(id as u64),
    }
}

fn fixture(mode: u8) -> OrderBook {
    let mut book = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
    if mode == 1 {
        book.ensure_level_hash_cache(64 * LEVEL_CACHE_ENTRY_COST);
    } else if mode == 2 {
        book.set_level_hash_chunked(true);
    }
    let mut id = 1;
    for (side, prices) in [(Side::Buy, [90, 91, 92]), (Side::Sell, [100, 101, 102])] {
        for price in prices {
            for trader in [1, 2, 3] {
                book.insert_order(order(id, trader, side, price, 2));
                id += 1;
            }
        }
    }
    book.take_row_ops();
    book.take_level_ops();
    // Promote cache probes without changing order state; seed the chunk state
    // before matching so the test checks invalidation, not just cold rebuilds.
    for (side, prices) in [(Side::Buy, [90, 91, 92]), (Side::Sell, [100, 101, 102])] {
        for price in prices {
            book.level_journal
                .insert((crate::book_rows::side_tag(side), fp(price).raw()));
        }
    }
    book.take_level_ops();
    book
}

fn assert_book_state(actual: &OrderBook, expected: &OrderBook) {
    assert_eq!(actual.bids, expected.bids);
    assert_eq!(actual.asks, expected.asks);
    assert_eq!(actual.trader_orders, expected.trader_orders);
    assert_eq!(actual.order_index.len(), expected.order_index.len());
    for (id, loc) in &actual.order_index {
        let old = &expected.order_index[id];
        assert_eq!((loc.side, loc.price), (old.side, old.price));
    }
    assert_eq!(actual.order_seq, expected.order_seq);
    assert_eq!(actual.next_seq, expected.next_seq);
    assert_eq!(actual.next_id, expected.next_id);
    assert_eq!(actual.row_journal, expected.row_journal);
    assert_eq!(actual.row_exists, expected.row_exists);
    assert_eq!(actual.level_journal, expected.level_journal);
    assert_eq!(actual.level_exists, expected.level_exists);
    assert_eq!(actual.level_epoch, expected.level_epoch);
    assert_eq!(actual.dirty_chunks, expected.dirty_chunks);
    assert_eq!(actual.level_chunks, expected.level_chunks);
    assert_eq!(
        borsh::to_vec(actual).unwrap(),
        borsh::to_vec(expected).unwrap()
    );
}

fn assert_match(actual: &mut OrderBook, expected: &mut OrderBook, taker: Order, market: bool) {
    let mut new_taker = taker.clone();
    let mut old_taker = taker;
    let (new_fills, new_stp) = actual.execute_match(&mut new_taker, market);
    let (old_fills, old_stp) = expected.execute_match_legacy(&mut old_taker, market);
    assert_eq!(new_taker, old_taker);
    assert_eq!(new_stp, old_stp);
    let fields = |f: &Fill| {
        (
            f.maker_order_id,
            f.taker_order_id,
            f.price,
            f.quantity,
            f.maker,
            f.taker,
            f.maker_side,
            f.timestamp,
        )
    };
    assert_eq!(
        new_fills.iter().map(fields).collect::<Vec<_>>(),
        old_fills.iter().map(fields).collect::<Vec<_>>()
    );
    assert_book_state(actual, expected);
    assert_eq!(actual.take_row_ops(), expected.take_row_ops());
    assert_eq!(actual.take_level_ops(), expected.take_level_ops());
    assert_book_state(actual, expected);
}

#[test]
fn occupied_matching_matches_legacy_traversal_and_bookkeeping() {
    for mode in 0..3 {
        for side in [Side::Buy, Side::Sell] {
            let prices = if side == Side::Buy {
                [99, 100, 103]
            } else {
                [93, 92, 89]
            };
            for price in prices {
                for qty in [0, 1, 2, 3, 9, 30] {
                    for trader in [1, 9] {
                        for market in [false, true] {
                            let mut actual = fixture(mode);
                            let mut expected = fixture(mode);
                            assert_match(
                                &mut actual,
                                &mut expected,
                                order(1000, trader, side, price, qty),
                                market,
                            );
                            // A second sweep consumes partially filled or previously
                            // untouched levels after the first journals were drained.
                            assert_match(
                                &mut actual,
                                &mut expected,
                                order(1001, 9, side, price, 30),
                                true,
                            );
                            actual.verify_invariants();
                            expected.verify_invariants();
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn occupied_matching_preserves_partial_state_when_matching_panics() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    for mode in 0..3 {
        for side in [Side::Buy, Side::Sell] {
            let mut actual = fixture(mode);
            let mut expected = fixture(mode);
            for book in [&mut actual, &mut expected] {
                let queue = if side == Side::Buy {
                    book.asks.first_entry().unwrap().into_mut()
                } else {
                    book.bids.last_entry().unwrap().into_mut()
                };
                // The first maker is fully removed before the second maker's
                // deliberately malformed quantity causes checked subtraction to
                // panic. Keep that partially modified level in the map on unwind.
                queue[0].remaining_qty = FixedPoint::from_raw(1);
                queue[1].remaining_qty = FixedPoint::from_raw(i128::MIN);
            }
            let mut new_taker = order(1000, 9, side, 100, 1);
            new_taker.remaining_qty = FixedPoint::MAX;
            let mut old_taker = new_taker.clone();
            let new = catch_unwind(AssertUnwindSafe(|| {
                actual.execute_match(&mut new_taker, true)
            }))
            .unwrap_err();
            let old = catch_unwind(AssertUnwindSafe(|| {
                expected.execute_match_legacy(&mut old_taker, true)
            }))
            .unwrap_err();
            let text = |p: Box<dyn std::any::Any + Send>| {
                p.downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap()
            };
            assert_eq!(text(new), text(old));
            assert_eq!(new_taker, old_taker);
            assert_book_state(&actual, &expected);
            let queue = if side == Side::Buy {
                actual.asks.first_key_value().unwrap().1
            } else {
                actual.bids.last_key_value().unwrap().1
            };
            assert_eq!(queue.len(), 2, "earlier maker removal survives the panic");
        }
    }
}
