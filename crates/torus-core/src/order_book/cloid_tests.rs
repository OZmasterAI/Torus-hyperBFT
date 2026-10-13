//! s114 cloid (owner decisions 1-3): u128 client order ids, one live
//! `(trader, cloid)` per book (the executor extends the rule across books),
//! the `(trader, cloid) -> order id` index kept on EVERY path that adds or
//! removes a resting order or a pending stop, and pending stops removable by
//! id. Each removal test checks the entry is gone (lookup `None`, index
//! empty) and the cloid can be placed again, and that the book's cloid log
//! carried the change ([`Mirror`]: what the executor's cross-market index
//! keeps from the log).

use super::*;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn cents(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 100))
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

/// Above u64::MAX.
const C: u128 = 0xfeed_0000_0000_0000_0000_0000_0000_0001;

fn book() -> OrderBook {
    OrderBook::new(1, cents(1), fp(1))
}

fn p(
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    ot: OrderType,
    tif: TimeInForce,
    cloid: Option<u128>,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price,
        quantity: qty,
        order_type: ot,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn gtc(is_buy: bool, price: i64, qty: i64, cloid: Option<u128>) -> PlaceOrderParams {
    p(
        is_buy,
        fp(price),
        fp(qty),
        OrderType::Limit,
        TimeInForce::GTC,
        cloid,
    )
}

fn stop_buy(trigger: i64, cloid: Option<u128>) -> PlaceOrderParams {
    p(
        true,
        fp(trigger + 10),
        fp(1),
        OrderType::StopMarket {
            trigger: fp(trigger),
        },
        TimeInForce::GTC,
        cloid,
    )
}

/// s114 follow-up: the executor's cross-market cloid index restricted to
/// one book, kept ONLY from the book's cloid log (`drain_cloid_log`), the
/// way `CloidMarkets::absorb` keeps it: a logged key takes the book's
/// current entry, or goes when the book has none.
struct Mirror(HashMap<(Address, u128), OrderId>);

impl Mirror {
    /// The index built from `b` as it is (its log emptied, as the
    /// executor's build does).
    fn of(b: &mut OrderBook) -> Self {
        b.clear_cloid_log();
        Mirror(entries(b))
    }

    /// Drain `b`'s log into the mirror; it must equal the book's index.
    fn assert_matches(&mut self, b: &mut OrderBook) {
        b.drain_cloid_log(|t, c, live| match live {
            Some(id) => {
                self.0.insert((t, c), id);
            }
            None => {
                self.0.remove(&(t, c));
            }
        });
        assert_eq!(b.cloid_log_len(), 0, "drained");
        assert_eq!(self.0, entries(b), "the cloid log missed a change");
    }
}

fn entries(b: &OrderBook) -> HashMap<(Address, u128), OrderId> {
    b.cloid_entries().map(|(t, c, id)| ((t, c), id)).collect()
}

/// The cloid is free again: no entry, an empty index, a log that carried
/// the removal to `m`, and a fresh order with it rests and is found (and
/// logged).
fn assert_freed(b: &mut OrderBook, m: &mut Mirror, trader: Address, cloid: u128) {
    m.assert_matches(b);
    assert_eq!(b.order_id_by_cloid(&trader, cloid), None);
    assert_eq!(b.cloid_count(), 0);
    b.verify_invariants();
    let r = b.place_order(gtc(true, 1, 1, Some(cloid)), trader, 99);
    assert_eq!(r.status, OrderStatus::Resting);
    assert_eq!(b.order_id_by_cloid(&trader, cloid), Some(r.order_id));
    m.assert_matches(b);
    assert_eq!(m.0.len(), 1);
    b.verify_invariants();
}

#[test]
fn a_live_cloid_rejects_a_second_order_of_the_same_trader() {
    let mut b = book();
    let first = b.place_order(gtc(true, 100, 1, Some(C)), addr(1), 1);
    assert_eq!(first.status, OrderStatus::Resting);
    assert_eq!(b.order_id_by_cloid(&addr(1), C), Some(first.order_id));

    let dup = b.place_order(gtc(false, 200, 1, Some(C)), addr(1), 2);
    assert_eq!(dup.status, OrderStatus::Rejected);
    assert_eq!(dup.reject, Some(FailureReason::DuplicateCloid));
    // A pending stop holds its cloid too.
    let dup_stop = b.place_order(stop_buy(300, Some(C)), addr(1), 3);
    assert_eq!(dup_stop.reject, Some(FailureReason::DuplicateCloid));

    // Another trader may use it; no cloid is never a duplicate.
    assert_eq!(
        b.place_order(gtc(true, 99, 1, Some(C)), addr(2), 4).status,
        OrderStatus::Resting
    );
    assert_eq!(
        b.place_order(gtc(true, 98, 1, None), addr(1), 5).status,
        OrderStatus::Resting
    );
    assert_eq!(
        b.place_order(gtc(true, 98, 1, None), addr(1), 6).status,
        OrderStatus::Resting
    );
    assert_eq!(b.cloid_count(), 2);
    b.verify_invariants();
}

#[test]
fn a_stop_cloid_is_found_and_held() {
    let mut b = book();
    let s = b.place_order(stop_buy(300, Some(C)), addr(1), 1);
    assert_eq!(s.status, OrderStatus::PendingTrigger);
    assert_eq!(b.order_id_by_cloid(&addr(1), C), Some(s.order_id));
    assert_eq!(
        b.place_order(gtc(true, 100, 1, Some(C)), addr(1), 2).reject,
        Some(FailureReason::DuplicateCloid)
    );
    b.verify_invariants();
}

/// s114 follow-up: every entry that goes in (a resting order, a stored stop)
/// is logged; an order without a cloid logs nothing.
#[test]
fn every_add_is_logged() {
    let mut b = book();
    let mut m = Mirror::of(&mut b);
    b.place_order(gtc(true, 100, 1, Some(C)), addr(1), 1);
    m.assert_matches(&mut b);
    b.place_order(stop_buy(300, Some(C + 1)), addr(1), 2);
    m.assert_matches(&mut b);
    b.place_order(gtc(true, 99, 1, None), addr(1), 3);
    assert_eq!(b.cloid_log_len(), 0, "no cloid: nothing logged");
    b.place_order(gtc(false, 150, 2, Some(C + 2)), addr(2), 4);
    b.place_order(gtc(true, 150, 1, Some(C + 3)), addr(3), 5); // fills, rests 0
    m.assert_matches(&mut b);
    assert_eq!(m.0.len(), 3);
}

#[test]
fn cancel_frees_the_cloid() {
    let mut b = book();
    let id = b
        .place_order(gtc(true, 100, 1, Some(C)), addr(1), 1)
        .order_id;
    let mut m = Mirror::of(&mut b);
    b.cancel_order(id).unwrap();
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn a_full_fill_frees_the_maker_cloid_and_a_partial_fill_keeps_it() {
    let mut b = book();
    let id = b
        .place_order(gtc(false, 100, 3, Some(C)), addr(1), 1)
        .order_id;
    let mut m = Mirror::of(&mut b);
    b.place_order(gtc(true, 100, 1, None), addr(2), 2);
    assert_eq!(
        b.order_id_by_cloid(&addr(1), C),
        Some(id),
        "partial fill keeps it"
    );
    m.assert_matches(&mut b);
    b.place_order(gtc(true, 100, 2, None), addr(2), 3);
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn a_taker_that_fills_whole_never_holds_its_cloid() {
    let mut b = book();
    b.place_order(gtc(false, 100, 1, None), addr(2), 1);
    let mut m = Mirror::of(&mut b);
    let r = b.place_order(gtc(true, 100, 1, Some(C)), addr(1), 2);
    assert_eq!(r.status, OrderStatus::Filled);
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn a_self_trade_cancel_frees_the_resting_cloid() {
    let mut b = book();
    b.place_order(gtc(false, 100, 1, Some(C)), addr(1), 1);
    let mut m = Mirror::of(&mut b);
    let r = b.place_order(gtc(true, 100, 1, None), addr(1), 2);
    assert_eq!(r.self_trade_cancels.len(), 1);
    // The taker (no cloid) rests; the maker's cloid is free.
    assert_eq!(b.order_id_by_cloid(&addr(1), C), None);
    m.assert_matches(&mut b);
    assert!(m.0.is_empty());
    assert_eq!(
        b.place_order(gtc(true, 50, 1, Some(C)), addr(1), 3).status,
        OrderStatus::Resting
    );
    b.verify_invariants();
}

#[test]
fn cancel_all_frees_resting_and_stop_cloids() {
    let mut b = book();
    b.place_order(gtc(true, 100, 1, Some(C)), addr(1), 1);
    b.place_order(stop_buy(300, Some(C + 1)), addr(1), 2);
    b.place_order(gtc(true, 90, 1, Some(C + 2)), addr(2), 3);
    let mut m = Mirror::of(&mut b);
    assert_eq!(b.cancel_all(addr(1), None).len(), 1);
    assert_eq!(b.order_id_by_cloid(&addr(1), C + 1), None);
    assert!(b.order_id_by_cloid(&addr(2), C + 2).is_some());
    m.assert_matches(&mut b);
    b.cancel_all(addr(2), None);
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn cancel_all_many_frees_every_senders_cloids() {
    let mut b = book();
    for (i, t) in [1u8, 2, 3].into_iter().enumerate() {
        b.place_order(gtc(true, 100 - i as i64, 1, Some(C)), addr(t), 1);
        b.place_order(stop_buy(300, Some(C + 1)), addr(t), 2);
    }
    let mut m = Mirror::of(&mut b);
    let out = b.cancel_all_many(&[addr(1), addr(2), addr(3)]);
    assert_eq!(out.iter().map(Vec::len).sum::<usize>(), 3);
    for t in [1u8, 2, 3] {
        assert_eq!(b.order_id_by_cloid(&addr(t), C), None);
        assert_eq!(b.order_id_by_cloid(&addr(t), C + 1), None);
    }
    assert_freed(&mut b, &mut m, addr(1), C);
}

/// The batch cancel-all path (deep concentrated level, >= 32 targets).
#[test]
fn the_batched_cancel_all_frees_cloids() {
    let mut b = book();
    for i in 0..1_100u128 {
        let t = if i % 2 == 0 { addr(1) } else { addr(2) };
        b.place_order(gtc(true, 100, 1, Some(C + i)), t, 1);
    }
    assert_eq!(b.cloid_count(), 1_100);
    let mut m = Mirror::of(&mut b);
    assert_eq!(b.cancel_all(addr(1), None).len(), 550);
    assert_eq!(b.cloid_count(), 550);
    m.assert_matches(&mut b);
    b.cancel_all(addr(2), None);
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn a_triggered_stop_releases_its_entry_and_carries_the_cloid() {
    let mut b = book();
    let s = b.place_order(stop_buy(105, Some(C)), addr(1), 1).order_id;
    b.place_order(gtc(false, 106, 1, None), addr(2), 2);
    let mut m = Mirror::of(&mut b);
    let r = b.place_order(gtc(true, 106, 1, None), addr(3), 3);
    assert_eq!(r.triggered_stops.len(), 1);
    assert_eq!(r.triggered_stops[0].id, s);
    assert_eq!(r.triggered_stops[0].params.client_order_id, Some(C));
    // Free until the executor places the triggered order under the cloid.
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn take_pending_stops_frees_stop_cloids() {
    let mut b = book();
    b.place_order(stop_buy(300, Some(C)), addr(1), 1);
    let mut m = Mirror::of(&mut b);
    assert_eq!(b.take_pending_stops(&addr(1)).len(), 1);
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn a_pending_stop_is_cancelled_by_id() {
    let mut b = book();
    let keep = b.place_order(stop_buy(400, None), addr(1), 1).order_id;
    let s = b.place_order(stop_buy(300, Some(C)), addr(1), 2).order_id;
    assert_eq!(b.pending_stop(s).map(|st| st.trader), Some(addr(1)));
    let mut m = Mirror::of(&mut b);
    let gone = b.cancel_stop(s).expect("stop cancelled");
    assert_eq!(
        (gone.id, gone.trader, gone.client_order_id),
        (s, addr(1), Some(C))
    );
    assert_eq!(gone.reservation_price(), fp(310));
    assert!(b.cancel_stop(s).is_none(), "second cancel finds nothing");
    assert!(b.pending_stop(s).is_none());
    assert!(b.pending_stop(keep).is_some(), "other stops stay");
    assert_eq!(b.pending_stop_count(), 1);
    assert_freed(&mut b, &mut m, addr(1), C);
}

#[test]
fn a_modify_keeps_the_cloid_on_the_same_order() {
    let mut b = book();
    let id = b
        .place_order(gtc(true, 100, 2, Some(C)), addr(1), 1)
        .order_id;
    let mut m = Mirror::of(&mut b);
    b.modify_order(id, Some(fp(101)), None).unwrap();
    assert_eq!(b.order_id_by_cloid(&addr(1), C), Some(id));
    m.assert_matches(&mut b);
    b.modify_order(id, None, Some(fp(1))).unwrap();
    assert_eq!(b.order_id_by_cloid(&addr(1), C), Some(id));
    m.assert_matches(&mut b);
    assert_eq!(b.get_order(id).unwrap().client_order_id, Some(C));
    assert_eq!(b.cloid_count(), 1);
    b.verify_invariants();
}

/// A reduce-only maker with nothing left to reduce is cut at match time.
#[test]
fn a_reduce_only_cut_frees_the_cloid() {
    let mut b = book();
    let mut ro = gtc(false, 100, 1, Some(C));
    ro.reduce_only = true;
    let mut pos = ReduceOnlyPositions::new();
    pos.insert(addr(1), fp(1));
    b.set_reduce_only_positions(pos);
    assert_eq!(b.place_order(ro, addr(1), 1).status, OrderStatus::Resting);
    let mut flat = ReduceOnlyPositions::new();
    flat.insert(addr(1), FixedPoint::ZERO);
    flat.insert(addr(2), FixedPoint::ZERO);
    b.set_reduce_only_positions(flat);
    let mut m = Mirror::of(&mut b);
    let r = b.place_order(gtc(true, 100, 1, None), addr(2), 2);
    assert_eq!(r.reduce_only_cuts.len(), 1);
    b.clear_reduce_only_positions();
    // The taker rests (no cloid); only the cut order held one.
    assert_eq!(b.order_id_by_cloid(&addr(1), C), None);
    assert_eq!(b.cloid_count(), 0);
    m.assert_matches(&mut b);
    assert!(m.0.is_empty());
    b.verify_invariants();
}

struct Broke;

impl MakerAccountSource for Broke {
    fn maker_account(&self, _maker: &Address, _market_id: MarketId) -> MakerAccount {
        MakerAccount {
            free: FixedPoint::ZERO,
            signed_pos: FixedPoint::ZERO,
            px: fp(100),
        }
    }
}

/// A maker that cannot afford its fill is margin-cancelled whole.
#[test]
fn a_margin_cancel_frees_the_cloid() {
    let mut b = book();
    b.place_order(
        p(
            true,
            cents(10_300),
            fp(10),
            OrderType::Limit,
            TimeInForce::GTC,
            Some(C),
        ),
        addr(1),
        1,
    );
    let mut am = AccountMargins::new(None);
    am.set_mark(Some(fp(100)));
    b.set_account_margins(am);
    let sell = p(
        false,
        fp(1),
        fp(10),
        OrderType::Market,
        TimeInForce::IOC,
        None,
    );
    let mut m = Mirror::of(&mut b);
    let r = b.place_order_with_accounts(sell, addr(2), 2, None, Some(&Broke));
    assert_eq!(r.margin_cancels.len(), 1);
    b.clear_account_margins();
    assert_freed(&mut b, &mut m, addr(1), C);
}

/// Restart: a reloaded book (classic blob or rows) rebuilds the index from
/// its orders and stops, and the 16-byte cloid round-trips.
#[test]
fn a_reloaded_book_rebuilds_the_index() {
    let mut b = book();
    let rest = b
        .place_order(gtc(true, 100, 1, Some(C)), addr(1), 1)
        .order_id;
    let stop = b
        .place_order(stop_buy(300, Some(C + 1)), addr(1), 2)
        .order_id;
    b.place_order(gtc(true, 99, 1, None), addr(2), 3);

    let blob = borsh::to_vec(&b).unwrap();
    let mut classic = OrderBook::try_from_slice(&blob).unwrap();
    // A load is not a change: nothing logged (the executor builds its
    // index from the loaded books).
    assert_eq!(classic.cloid_log_len(), 0, "classic load logs nothing");

    let mut rows = OrderBook::new(1, cents(1), fp(1));
    rows.set_next_seq(b.next_seq());
    rows.set_next_order_id(b.next_order_id());
    let mut orders: Vec<(u64, Order)> = b
        .full_row_ops()
        .into_iter()
        .map(|(_, bytes)| OrderBook::decode_order_row(&bytes).unwrap())
        .collect();
    orders.sort_by_key(|(seq, o)| (u8::from(o.side == Side::Sell), o.price, *seq));
    for (seq, o) in orders {
        rows.insert_loaded_order(o, seq);
    }
    for (_, bytes) in b.stop_rows() {
        rows.restore_stop_row(&bytes).unwrap();
    }

    for r in [&mut classic, &mut rows] {
        assert_eq!(r.order_id_by_cloid(&addr(1), C), Some(rest));
        assert_eq!(r.order_id_by_cloid(&addr(1), C + 1), Some(stop));
        assert_eq!(r.get_order(rest).unwrap().client_order_id, Some(C));
        assert_eq!(r.pending_stop(stop).unwrap().client_order_id, Some(C + 1));
        assert_eq!(r.cloid_count(), 2);
        assert_eq!(
            r.place_order(gtc(true, 1, 1, Some(C)), addr(1), 9).reject,
            Some(FailureReason::DuplicateCloid)
        );
        r.verify_invariants();
    }
}

#[test]
fn the_order_row_stores_the_cloid_in_16_bytes() {
    let mut b = book();
    let id = b
        .place_order(gtc(true, 100, 1, Some(C)), addr(1), 1)
        .order_id;
    let some = b.encode_order_row(id).unwrap();
    let mut tail = vec![1u8];
    tail.extend_from_slice(&C.to_be_bytes());
    assert!(some.ends_with(&tail));
    let none_id = b.place_order(gtc(true, 99, 1, None), addr(1), 1).order_id;
    assert_eq!(b.encode_order_row(none_id).unwrap().len() + 16, some.len());
}

// ---- perf/cloid-engine-cost: a pinned seeded stream ----
//
// The cloid engine-cost changes (pre-sized claimed set, one-pass log,
// skipped book re-check, compact key) must not change one result. This
// stream drives one book with cloid traffic (duplicates on purpose, fills,
// cancels by id / cloid, modifies, cancel-alls, stops, triggers) and folds
// every result, every drained log record, the index and the serialized
// book into one digest pinned on the pre-change code (c88d3073).

/// splitmix64: a fixed stream, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
}

/// The digest of `seed`'s stream on one book (see above). Also checks the
/// book's invariants and the log mirror along the way.
fn seeded_book_stream(seed: u64, ops: usize) -> u64 {
    use std::fmt::Write as _;
    let mut rng = Rng(seed);
    let mut b = book();
    let mut m = Mirror::of(&mut b);
    let mut out = String::new();
    let cloid = |rng: &mut Rng| (rng.below(4) != 0).then(|| C + rng.below(6) as u128);
    let live = |b: &OrderBook| {
        let mut v: Vec<(Address, u128, OrderId)> = b.cloid_entries().collect();
        v.sort();
        v
    };
    for k in 0..ops {
        let trader = addr(1 + rng.below(4) as u8);
        let ts = k as u64;
        let r = rng.below(100);
        let _ = write!(out, "{k}:{r}:");
        if r < 35 {
            let is_buy = rng.below(2) == 0;
            let px = if is_buy {
                rng.pick(96, 101)
            } else {
                rng.pick(99, 104)
            };
            let res = b.place_order(gtc(is_buy, px, rng.pick(1, 3), cloid(&mut rng)), trader, ts);
            let _ = write!(out, "{res:?}");
        } else if r < 45 {
            let is_buy = rng.below(2) == 0;
            let px = if is_buy { 106 } else { 94 };
            let params = p(
                is_buy,
                fp(px),
                fp(rng.pick(1, 4)),
                OrderType::Limit,
                TimeInForce::IOC,
                cloid(&mut rng),
            );
            let res = b.place_order(params, trader, ts);
            let _ = write!(out, "{res:?}");
        } else if r < 52 {
            let is_buy = rng.below(2) == 0;
            let trigger = if is_buy {
                rng.pick(101, 105)
            } else {
                rng.pick(95, 99)
            };
            let params = p(
                is_buy,
                fp(if is_buy { trigger + 5 } else { trigger - 5 }),
                fp(1),
                OrderType::StopMarket {
                    trigger: fp(trigger),
                },
                TimeInForce::GTC,
                cloid(&mut rng),
            );
            let res = b.place_order(params, trader, ts);
            let _ = write!(out, "{res:?}");
        } else if r < 62 {
            // Cancel by cloid (a miss is common).
            let c = C + rng.below(6) as u128;
            match b.order_id_by_cloid(&trader, c) {
                Some(id) if b.pending_stop(id).is_some() => {
                    let _ = write!(out, "stop {:?}", b.cancel_stop(id).map(|s| s.id));
                }
                Some(id) => {
                    let _ = write!(out, "cancel {:?}", b.cancel_order(id).map(|o| o.id));
                }
                None => out.push_str("miss"),
            }
        } else if r < 70 {
            let v = live(&b);
            if !v.is_empty() {
                let (_, _, id) = v[rng.below(v.len() as u64) as usize];
                match b.pending_stop(id) {
                    Some(_) => {
                        let _ = write!(out, "stop {:?}", b.cancel_stop(id).map(|s| s.id));
                    }
                    None => {
                        let _ = write!(out, "cancel {:?}", b.cancel_order(id).map(|o| o.id));
                    }
                }
            }
        } else if r < 75 {
            let v = live(&b);
            if !v.is_empty() {
                let (_, _, id) = v[rng.below(v.len() as u64) as usize];
                if let Some(o) = b.get_order(id) {
                    // Passive prices only (a raw book modify does not match).
                    let new_px = Some(fp(match o.side {
                        Side::Buy => rng.pick(90, 95),
                        Side::Sell => rng.pick(105, 110),
                    }));
                    let new_q = (rng.below(2) == 0).then(|| fp(1));
                    let res = b.modify_order(id, new_px, new_q);
                    let _ = write!(
                        out,
                        "modify {:?}",
                        res.map(|o| (o.id, o.price, o.remaining_qty))
                    );
                }
            }
        } else if r < 78 {
            let ids: Vec<OrderId> = b.cancel_all(trader, None).iter().map(|o| o.id).collect();
            let _ = write!(out, "cancel_all {ids:?}");
        } else if r < 81 {
            let fired: Vec<(OrderId, Option<Cloid>)> = b
                .check_stops()
                .iter()
                .map(|s| (s.id, s.params.client_order_id))
                .collect();
            let _ = write!(out, "triggered {fired:?}");
        } else if r < 83 {
            let _ = write!(out, "take_stops {:?}", b.take_pending_stops(&trader));
        } else if r < 92 {
            let is_buy = rng.below(2) == 0;
            let px = if is_buy {
                rng.pick(96, 101)
            } else {
                rng.pick(99, 104)
            };
            let res = b.place_order(gtc(is_buy, px, rng.pick(1, 3), None), trader, ts);
            let _ = write!(out, "{res:?}");
        } else {
            // Drain the log: the records the executor's index keeps.
            b.drain_cloid_log(|t, c, live| {
                let _ = write!(out, "[{t} {c:#x} {live:?}]");
                match live {
                    Some(id) => {
                        m.0.insert((t, c), id);
                    }
                    None => {
                        m.0.remove(&(t, c));
                    }
                }
            });
            assert_eq!(m.0, entries(&b), "op {k}: the log missed a change");
        }
        out.push('\n');
        if k % 97 == 0 {
            b.verify_invariants();
        }
    }
    m.assert_matches(&mut b);
    b.verify_invariants();
    let _ = write!(out, "live {:?}\n", live(&b));
    let blob = borsh::to_vec(&b).unwrap();
    // A reload gives the same index.
    let reloaded = OrderBook::try_from_slice(&blob).unwrap();
    assert_eq!(live(&reloaded), live(&b));
    let _ = write!(out, "blob {}", alloy_primitives::hex::encode(&blob));
    // The stream exercises what it claims to.
    assert!(
        out.matches("DuplicateCloid").count() >= 100,
        "seed {seed}: duplicates"
    );
    assert!(
        out.matches("cancel Ok").count() >= 100,
        "seed {seed}: cancels"
    );
    let fired = out.matches("TriggeredStop {").count() + out.matches("triggered [(").count();
    assert!(fired >= 5, "seed {seed}: triggers {fired}");
    // FNV-1a 64: a fixed fold, not a security hash.
    out.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, x| {
        (h ^ x as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Pinned on c88d3073 (before the cloid engine-cost changes): every
/// result, drained log record, the index and the book bytes of three
/// seeded streams.
#[test]
fn a_seeded_cloid_stream_gives_the_pinned_results() {
    let got: Vec<u64> = [1u64, 2, 0xc10d]
        .iter()
        .map(|&s| seeded_book_stream(s, 3_000))
        .collect();
    let want: [u64; 3] = [
        0x6801_a91a_0438_47d3,
        0x0caf_afa0_ae93_c5e2,
        0xe1d7_4ee9_ad42_4e95,
    ];
    assert_eq!(got, want, "got {got:#x?}");
}
