//! Item 6 Phase 4 step 5: the book's open-order log.
//!
//! Every change of a trader's [`OrderBook::open_order_count`] in a book
//! (resting orders plus pending stops) is logged per trader, net, until the
//! executor drains it ([`OrderBook::drain_open_log`]). So the sum of every
//! drained delta since the book was created (or loaded) equals the book's
//! count of each trader, whatever path changed it: placement (resting
//! remainder, stop stored), fill (full and partial), self-trade cancel,
//! reduce-only cut and sweep, cancel by id, cancel of a stop, modify (in
//! place and cancel + reinsert), cancel-all (one trader and many),
//! `take_pending_stops`, stops triggering, and the loads (classic decode and
//! row restore).

use super::*;
use std::collections::BTreeMap;

const TRADERS: u8 = 5;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

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

fn params(is_buy: bool, price: i64, qty: i64, ot: OrderType, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: ot,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn random_place(rng: &mut Rng) -> PlaceOrderParams {
    let is_buy = rng.below(2) == 0;
    let mut p = match rng.below(12) {
        0 => params(is_buy, if is_buy { 106 } else { 94 }, rng.pick(1, 5), OrderType::Limit, TimeInForce::IOC),
        1 => params(is_buy, if is_buy { 110 } else { 90 }, rng.pick(1, 3), OrderType::Market, TimeInForce::IOC),
        2 => params(is_buy, if is_buy { 104 } else { 96 }, rng.pick(1, 6), OrderType::Limit, TimeInForce::FOK),
        3 => params(is_buy, if is_buy { rng.pick(95, 99) } else { rng.pick(101, 105) }, rng.pick(1, 3), OrderType::Limit, TimeInForce::PostOnly),
        4 | 5 => {
            let trigger = if is_buy { rng.pick(101, 104) } else { rng.pick(96, 99) };
            let ot = if rng.below(2) == 0 {
                OrderType::StopMarket { trigger: fp(trigger) }
            } else {
                OrderType::StopLimit {
                    trigger: fp(trigger),
                    limit: fp(if is_buy { trigger + 2 } else { trigger - 2 }),
                }
            };
            params(is_buy, if is_buy { trigger + 5 } else { trigger - 5 }, rng.pick(1, 2), ot, TimeInForce::GTC)
        }
        // Resting or crossing limits: partial and full fills, self-trades.
        _ => params(
            is_buy,
            if is_buy { rng.pick(96, 102) } else { rng.pick(98, 104) },
            rng.pick(1, 4),
            OrderType::Limit,
            TimeInForce::GTC,
        ),
    };
    p.reduce_only = rng.below(8) == 0;
    p
}

/// Every trader's `open_order_count` (absent = 0).
fn counts(book: &OrderBook) -> BTreeMap<Address, i64> {
    (1..=TRADERS + 1)
        .map(addr)
        .map(|t| (t, book.open_order_count(&t) as i64))
        .filter(|&(_, n)| n > 0)
        .collect()
}

/// Drain `book`'s log into `acc` (zero sums dropped).
fn drain(book: &mut OrderBook, acc: &mut BTreeMap<Address, i64>) {
    book.drain_open_log(7, |t, d| {
        assert_ne!(d, 0, "a drained delta is never 0");
        *acc.entry(t).or_default() += i64::from(d);
    });
    acc.retain(|_, n| *n != 0);
    assert_eq!(book.open_log_len(), 0, "drained");
}

/// Every trader's count from [`OrderBook::for_each_open_count`].
fn listed(book: &OrderBook) -> BTreeMap<Address, i64> {
    let mut out: BTreeMap<Address, i64> = BTreeMap::new();
    book.for_each_open_count(|t, n| *out.entry(t).or_default() += n as i64);
    out.retain(|_, n| *n != 0);
    out
}

/// The book reloaded as the classic blob (Borsh), and as rows.
fn reloads(book: &mut OrderBook) -> [OrderBook; 2] {
    let blob = borsh::to_vec(&*book).unwrap();
    let classic = OrderBook::try_from_slice(&blob).unwrap();
    let mut rows = OrderBook::new(book.market_id, book.tick_size, book.lot_size);
    rows.set_next_order_id(book.next_order_id());
    rows.set_next_seq(book.next_seq());
    let mut loaded: Vec<(u64, Order)> = book
        .full_row_ops()
        .into_iter()
        .map(|(_, bytes)| OrderBook::decode_order_row(&bytes).unwrap())
        .collect();
    // Canonical load order: bids ascending (price, seq), then asks.
    loaded.sort_by_key(|(seq, o)| (o.side == Side::Sell, o.price, *seq));
    for (seq, order) in loaded {
        rows.insert_loaded_order(order, seq);
    }
    for (_, bytes) in book.stop_rows() {
        rows.restore_stop_row(&bytes).unwrap();
    }
    [classic, rows]
}

#[test]
fn open_log_sums_to_open_order_count_over_random_ops() {
    let mut seen = [0usize; 12];
    for seed in 1..=24u64 {
        let mut rng = Rng(0x0be2_0000 ^ seed);
        let mut book = OrderBook::new(1, fp(1), fp(1));
        let mut acc: BTreeMap<Address, i64> = BTreeMap::new();
        for step in 0..400u64 {
            let trader = addr(1 + rng.below(TRADERS as u64) as u8);
            let ids: Vec<OrderId> = {
                let mut v: Vec<OrderId> = (1..=TRADERS)
                    .flat_map(|n| book.orders_for_trader(&addr(n)).into_iter().map(|o| o.id))
                    .collect();
                v.sort_unstable();
                v
            };
            let stops: Vec<OrderId> = book.pending_stops().iter().map(|s| s.id).collect();
            let op = rng.below(100);
            let kind = match op {
                0..=49 => {
                    let mut ro = ReduceOnlyPositions::new();
                    for n in 1..=TRADERS {
                        ro.insert(addr(n), fp(rng.pick(-3, 3)));
                    }
                    let policed = rng.below(3) == 0;
                    if policed {
                        book.set_reduce_only_positions(ro);
                    }
                    let r = book.place_order(random_place(&mut rng), trader, step);
                    if policed {
                        book.clear_reduce_only_positions();
                    }
                    // The executor places fired stops (as their orders).
                    for s in r.triggered_stops {
                        book.place_order(s.params, s.trader, step);
                    }
                    0
                }
                50..=59 if !ids.is_empty() => {
                    let id = ids[rng.below(ids.len() as u64) as usize];
                    book.cancel_order(id).unwrap();
                    1
                }
                60..=64 => {
                    assert!(book.cancel_order(1_000_000).is_err());
                    2
                }
                65..=69 if !stops.is_empty() => {
                    let id = stops[rng.below(stops.len() as u64) as usize];
                    book.cancel_stop(id).unwrap();
                    3
                }
                70..=76 if !ids.is_empty() => {
                    let id = ids[rng.below(ids.len() as u64) as usize];
                    let price = (rng.below(2) == 0).then(|| fp(rng.pick(96, 104)));
                    let qty = (rng.below(2) == 0).then(|| fp(rng.pick(1, 4)));
                    let _ = book.modify_order(id, price, qty);
                    4
                }
                77..=81 => {
                    book.cancel_all(trader, None);
                    5
                }
                82..=85 => {
                    let senders: Vec<Address> =
                        (0..rng.pick(1, 4)).map(|_| addr(1 + rng.below(TRADERS as u64) as u8)).collect();
                    book.cancel_all_many(&senders);
                    6
                }
                86..=88 => {
                    book.take_pending_stops(&trader);
                    7
                }
                89..=91 => {
                    book.set_last_trade_price(Some(fp(rng.pick(94, 106))));
                    for s in book.check_stops() {
                        book.place_order(s.params, s.trader, step);
                    }
                    8
                }
                92..=94 => {
                    book.sweep_reduce_only_at(trader, fp(rng.pick(-2, 2)));
                    9
                }
                95..=97 => {
                    // A load: the reloaded book's log is its whole count.
                    for mut b in reloads(&mut book) {
                        let mut fresh = BTreeMap::new();
                        drain(&mut b, &mut fresh);
                        assert_eq!(fresh, counts(&b), "seed {seed} step {step}: reload");
                        assert_eq!(counts(&b), counts(&book), "seed {seed} step {step}");
                    }
                    10
                }
                _ => {
                    // No drain this step: the log keeps netting.
                    let r = book.place_order(random_place(&mut rng), trader, step);
                    for s in r.triggered_stops {
                        book.place_order(s.params, s.trader, step);
                    }
                    assert_eq!(listed(&book), counts(&book), "seed {seed} step {step}");
                    seen[11] += 1;
                    continue;
                }
            };
            seen[kind] += 1;
            drain(&mut book, &mut acc);
            assert_eq!(acc, counts(&book), "seed {seed} step {step} op {op}");
            assert_eq!(listed(&book), acc, "seed {seed} step {step}");
        }
        // A cleared log drops what it held: counting again starts from the books.
        book.place_order(params(true, 50, 1, OrderType::Limit, TimeInForce::GTC), addr(1), 9_999);
        assert!(book.open_log_len() > 0);
        book.clear_open_log(OrderBook::OPEN_LOG_UNKNOWN);
        assert_eq!(book.open_log_len(), 0);
    }
    // The stream exercises every op.
    assert!(seen.iter().all(|&n| n > 0), "ops seen {seen:?}");
}

/// A new book's log is empty; resting and stored stops log +1, an IOC that
/// never rests and a rejected stop log nothing, a partial maker fill logs
/// nothing, a full one -1; deltas net per trader before a drain.
#[test]
fn open_log_nets_per_trader_until_drained() {
    let mut b = OrderBook::new(1, fp(1), fp(1));
    assert_eq!(b.open_log_len(), 0);
    let (a, m) = (addr(1), addr(2));
    let maker = b.place_order(params(false, 100, 3, OrderType::Limit, TimeInForce::GTC), m, 1).order_id;
    b.place_order(params(true, 90, 1, OrderType::Limit, TimeInForce::GTC), a, 2);
    b.place_order(params(true, 80, 1, OrderType::Limit, TimeInForce::IOC), a, 3);
    let stop = OrderType::StopMarket { trigger: fp(110) };
    b.place_order(params(true, 120, 1, stop, TimeInForce::GTC), a, 4);
    let mut got = Vec::new();
    b.drain_open_log(7, |t, d| got.push((t, d)));
    got.sort();
    assert_eq!(got, vec![(a, 2), (m, 1)]);

    // Partial fill of the maker: no change; then a full fill: -1.
    b.place_order(params(true, 100, 1, OrderType::Limit, TimeInForce::IOC), addr(3), 5);
    assert_eq!(b.open_log_len(), 0, "a partial maker fill keeps the slot");
    b.place_order(params(true, 100, 2, OrderType::Limit, TimeInForce::IOC), addr(3), 6);
    assert!(b.get_order(maker).is_none());
    // A buy stop at or below the last trade price is rejected: no slot.
    let low = OrderType::StopMarket { trigger: fp(95) };
    let r = b.place_order(params(true, 120, 1, low, TimeInForce::GTC), a, 6);
    assert_eq!(r.status, OrderStatus::Rejected);
    // Place and cancel inside one drain window: nets to nothing.
    let id = b.place_order(params(true, 91, 1, OrderType::Limit, TimeInForce::GTC), a, 7).order_id;
    b.cancel_order(id).unwrap();
    let mut got = Vec::new();
    b.drain_open_log(7, |t, d| got.push((t, d)));
    assert_eq!(got, vec![(m, -1)]);
}

/// Codex r1: the log's owner. A new or decoded book is fresh (its log is
/// its whole count); a drain hands the count to the drain's owner, a clear
/// to the clear's.
#[test]
fn open_log_owner_follows_drains_clears_and_decodes() {
    let mut b = OrderBook::new(1, fp(1), fp(1));
    assert_eq!(b.open_log_owner(), OrderBook::OPEN_LOG_FRESH);
    b.place_order(params(true, 90, 1, OrderType::Limit, TimeInForce::GTC), addr(1), 1);
    let decoded: OrderBook = borsh::from_slice(&borsh::to_vec(&b).unwrap()).unwrap();
    assert_eq!(decoded.open_log_owner(), OrderBook::OPEN_LOG_FRESH);
    assert_eq!(decoded.open_log().collect::<Vec<_>>(), vec![(addr(1), 1)], "the whole count");
    b.drain_open_log(5, |_, _| {});
    assert_eq!(b.open_log_owner(), 5);
    b.place_order(params(true, 91, 1, OrderType::Limit, TimeInForce::GTC), addr(1), 2);
    assert_eq!(b.open_log_owner(), 5, "matching keeps the owner");
    b.clear_open_log(OrderBook::OPEN_LOG_UNKNOWN);
    assert_eq!(b.open_log_owner(), OrderBook::OPEN_LOG_UNKNOWN);
    b.clear_open_log(9);
    assert_eq!(b.open_log_owner(), 9);
}
