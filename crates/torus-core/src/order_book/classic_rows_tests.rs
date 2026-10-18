//! Item 6 Phase 5 step 3: [`OrderBook::classic_row`] equals the reference
//! Classic row (`borsh::to_vec`, the `BorshSerialize for OrderBook` walk)
//! at every save, over every book primitive.

use super::*;

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 17
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

fn params(is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// The Classic save of one book, as `save_order_books` does it: the row,
/// then both journals dropped. The row must equal the reference encoding.
fn save(book: &mut OrderBook, what: &str, totals: &mut ClassicRowStats, saves: &mut u64) {
    let reference = borsh::to_vec(&*book).unwrap();
    let (row, stats) = book.classic_row().unwrap();
    assert!(row == reference, "{what}: classic_row != borsh ({} vs {} bytes)", row.len(), reference.len());
    assert_eq!(stats.bytes, row.len() as u64, "{what}");
    book.discard_row_ops();
    book.discard_level_ops();
    totals.full_build |= stats.full_build;
    totals.levels_reused += stats.levels_reused;
    totals.levels_encoded += stats.levels_encoded;
    totals.levels_removed += stats.levels_removed;
    totals.bytes += stats.bytes;
    *saves += 1;
    // A second save with no mutation in between re-encodes nothing.
    let (again, stats) = book.classic_row().unwrap();
    assert!(again == reference, "{what}: second classic_row differs");
    assert_eq!((stats.full_build, stats.levels_encoded, stats.levels_removed), (false, 0, 0), "{what}");
}

/// A resting order of `book`, at random.
fn pick_live(book: &OrderBook, rng: &mut Rng) -> Option<OrderId> {
    let live: Vec<OrderId> = book
        .bid_queues()
        .chain(book.ask_queues())
        .flat_map(|(_, q)| q.iter().map(|o| o.id))
        .collect();
    (!live.is_empty()).then(|| live[rng.below(live.len() as u64) as usize])
}

/// What the random driver did that changed the book (non-vacuity).
#[derive(Debug, Default)]
struct Did {
    rested: u64,
    fills: u64,
    self_trade: u64,
    reduce_only_cuts: u64,
    stops_stored: u64,
    stops_fired: u64,
    stops_cancelled: u64,
    stops_taken: u64,
    cancels: u64,
    cancel_all: u64,
    cancel_all_many: u64,
    modify_in_place: u64,
    modify_reinsert: u64,
    sweeps: u64,
    other_drains: u64,
    reloads: u64,
    skipped_saves: u64,
    full_builds: u64,
}

/// Random blocks of every book primitive; after each block (most of them)
/// a Classic save checked against the reference. Some blocks skip the save
/// (the book was not dirty: its journal carries over), some drain the
/// level journal another way (a mode-1/2/3 drain: the kept levels must be
/// dropped) and some reload the book from its row (a new instance).
fn random_run(seed: u64, blocks: u64) -> Did {
    let mut rng = Rng(seed);
    let mut book = OrderBook::new(1, fp(1), fp(1));
    let mut did = Did::default();
    let mut totals = ClassicRowStats::default();
    let mut saves = 0u64;
    let mut ids: Vec<OrderId> = Vec::new();
    let mut cloid: u128 = 1;
    let traders = 6u64;
    for b in 0..blocks {
        let mid = 100 + (b as i64 / 40) % 7 - 3;
        for _ in 0..1 + rng.below(30) {
            let t = addr(1 + rng.below(traders) as u8);
            match rng.below(17) {
                0..=3 => {
                    let is_buy = rng.chance(50);
                    let off = 1 + rng.below(10) as i64;
                    let mut p = params(is_buy, if is_buy { mid - off } else { mid + off }, 1 + rng.below(5) as i64);
                    if rng.chance(25) {
                        p.client_order_id = Some(cloid);
                        cloid += 1;
                    }
                    if rng.chance(10) {
                        p.time_in_force = TimeInForce::PostOnly;
                    }
                    let r = book.place_order(p, t, b);
                    ids.push(r.order_id);
                    did.rested += u64::from(r.rested_qty > FixedPoint::ZERO);
                    did.self_trade += r.self_trade_cancels.len() as u64;
                }
                4 | 5 => {
                    let is_buy = rng.chance(50);
                    let reach = rng.below(8) as i64;
                    let mut p = params(is_buy, if is_buy { mid + reach } else { mid - reach }, 1 + rng.below(12) as i64);
                    if rng.chance(30) {
                        p.time_in_force = TimeInForce::IOC;
                    }
                    let r = book.place_order(p, t, b);
                    ids.push(r.order_id);
                    did.fills += r.fills.len() as u64;
                    did.self_trade += r.self_trade_cancels.len() as u64;
                    if let Some(f) = r.fills.last() {
                        book.set_last_trade_price(Some(f.price));
                    }
                    for s in r.triggered_stops {
                        did.stops_fired += 1;
                        let r = book.place_order(s.params, s.trader, b);
                        ids.push(r.order_id);
                    }
                }
                6 => {
                    let is_buy = rng.chance(50);
                    let p = PlaceOrderParams {
                        order_type: OrderType::Market,
                        ..params(is_buy, if is_buy { mid + 20 } else { mid - 20 }, 1 + rng.below(6) as i64)
                    };
                    let r = book.place_order(p, t, b);
                    did.fills += r.fills.len() as u64;
                }
                7 => {
                    let is_buy = rng.chance(50);
                    let trigger = if is_buy { mid + 1 + rng.below(4) as i64 } else { mid - 1 - rng.below(4) as i64 };
                    let order_type = if rng.chance(50) {
                        OrderType::StopLimit { trigger: fp(trigger), limit: fp(trigger) }
                    } else {
                        OrderType::StopMarket { trigger: fp(trigger) }
                    };
                    let p = PlaceOrderParams { order_type, ..params(is_buy, trigger, 1 + rng.below(3) as i64) };
                    let r = book.place_order(p, t, b);
                    ids.push(r.order_id);
                    did.stops_stored += u64::from(r.status == OrderStatus::PendingTrigger);
                }
                8 => {
                    if let Some(id) = pick_live(&book, &mut rng).or(ids.last().copied()) {
                        did.cancels += u64::from(book.cancel_order(id).is_ok());
                    }
                }
                9 => {
                    let stops: Vec<OrderId> = book.pending_stops().iter().map(|s| s.id).collect();
                    if !stops.is_empty() {
                        let id = stops[rng.below(stops.len() as u64) as usize];
                        did.stops_cancelled += u64::from(book.cancel_stop(id).is_some());
                    }
                }
                10 => {
                    did.cancel_all += book.cancel_all(t, None).len() as u64;
                }
                11 => {
                    let senders: Vec<Address> =
                        (0..2 + rng.below(3)).map(|_| addr(1 + rng.below(traders) as u8)).collect();
                    did.cancel_all_many +=
                        book.cancel_all_many(&senders).iter().map(Vec::len).sum::<usize>() as u64;
                }
                12 => {
                    did.stops_taken += book.take_pending_stops(&t).len() as u64;
                }
                13 => {
                    if let Some(id) = pick_live(&book, &mut rng) {
                        if let Some(o) = book.get_order(id).cloned() {
                            let (price, qty) = match rng.below(3) {
                                0 => (None, Some(fp(1).max(o.remaining_qty - fp(1)))),
                                1 => (Some(o.price + fp(if o.side == Side::Buy { -1 } else { 1 })), None),
                                _ => (None, Some(o.remaining_qty + fp(2))),
                            };
                            let in_place = price.is_none() && qty.is_some_and(|q| q < o.remaining_qty);
                            if book.modify_order(id, price, qty).is_ok() {
                                if in_place {
                                    did.modify_in_place += 1;
                                } else {
                                    did.modify_reinsert += 1;
                                }
                            }
                        }
                    }
                }
                14 => {
                    let px = mid + rng.below(9) as i64 - 4;
                    book.set_last_trade_price(Some(fp(px)));
                    for s in book.check_stops() {
                        did.stops_fired += 1;
                        let r = book.place_order(s.params, s.trader, b);
                        ids.push(r.order_id);
                        did.fills += r.fills.len() as u64;
                    }
                }
                15 => {
                    // Reduce-only: a policed trader rests reduce-only orders,
                    // fills cut them, then a sweep at a smaller position.
                    let mut pos = ReduceOnlyPositions::new();
                    let size = rng.below(7) as i64 - 3;
                    pos.insert(t, fp(size));
                    book.set_reduce_only_positions(pos);
                    let is_buy = size < 0;
                    let off = 1 + rng.below(4) as i64;
                    let p = PlaceOrderParams {
                        reduce_only: true,
                        ..params(is_buy, if is_buy { mid - off } else { mid + off }, 1 + rng.below(3) as i64)
                    };
                    let r = book.place_order(p, t, b);
                    ids.push(r.order_id);
                    did.reduce_only_cuts += r.reduce_only_cuts.len() as u64;
                    // A taker against the policed trader's resting orders.
                    let taker = addr(1 + (t.0[0] % traders as u8));
                    let r = book.place_order(params(!is_buy, if is_buy { mid - 10 } else { mid + 10 }, 2), taker, b);
                    did.reduce_only_cuts += r.reduce_only_cuts.len() as u64;
                    did.fills += r.fills.len() as u64;
                    let cuts = book.sweep_reduce_only_at(t, FixedPoint::ZERO);
                    did.sweeps += cuts.len() as u64;
                    book.clear_reduce_only_positions();
                }
                _ => {
                    let next = book.next_order_id() + rng.below(3) as u128;
                    book.set_next_order_id(next);
                }
            }
        }
        let what = format!("seed {seed} block {b}");
        match rng.below(100) {
            0..=6 => did.skipped_saves += 1,
            7..=10 => {
                // Another drain of the level journal (a mode-1/2/3 save or a
                // full write): the kept levels can no longer be trusted.
                did.other_drains += 1;
                match rng.below(3) {
                    0 => {
                        book.take_level_ops();
                    }
                    1 => {
                        book.full_level_ops();
                    }
                    _ => book.discard_level_ops(),
                }
                book.discard_row_ops();
            }
            11..=13 => {
                // A reload: the book decoded from its row (a new instance).
                did.reloads += 1;
                let bytes = borsh::to_vec(&book).unwrap();
                book = borsh::from_slice(&bytes).unwrap();
                save(&mut book, &what, &mut totals, &mut saves);
            }
            _ => {
                let before = totals.full_build;
                totals.full_build = false;
                save(&mut book, &what, &mut totals, &mut saves);
                did.full_builds += u64::from(totals.full_build);
                totals.full_build |= before;
            }
        }
    }
    println!("classic_row seed {seed}: {saves} saves, {totals:?}, {did:?}");
    assert!(saves >= blocks * 3 / 4, "saves {saves}");
    assert!(totals.levels_reused > 0 && totals.levels_encoded > 0 && totals.levels_removed > 0, "{totals:?}");
    did
}

/// The plan's `book_dirty_rows_equal_save_order_books` at the book level:
/// every primitive (places resting / crossing / post-only / market, fills,
/// self-trade cancels, stops stored / fired / cancelled / taken, cancels,
/// cancel-all, `cancel_all_many`, modifies in place and re-inserted,
/// reduce-only cuts and sweeps, order-id bumps), the save skipped, other
/// journal drains and reloads: the Classic row equals the reference at
/// every save.
#[test]
fn classic_row_equals_borsh_every_save_random() {
    let mut all = Did::default();
    for seed in 1..=6u64 {
        let d = random_run(seed, 400);
        all.rested += d.rested;
        all.fills += d.fills;
        all.self_trade += d.self_trade;
        all.reduce_only_cuts += d.reduce_only_cuts;
        all.stops_stored += d.stops_stored;
        all.stops_fired += d.stops_fired;
        all.stops_cancelled += d.stops_cancelled;
        all.stops_taken += d.stops_taken;
        all.cancels += d.cancels;
        all.cancel_all += d.cancel_all;
        all.cancel_all_many += d.cancel_all_many;
        all.modify_in_place += d.modify_in_place;
        all.modify_reinsert += d.modify_reinsert;
        all.sweeps += d.sweeps;
        all.other_drains += d.other_drains;
        all.reloads += d.reloads;
        all.skipped_saves += d.skipped_saves;
        all.full_builds += d.full_builds;
    }
    println!("classic_row all seeds: {all:?}");
    // Every path reached, more than once.
    for (name, n) in [
        ("rested", all.rested),
        ("fills", all.fills),
        ("self_trade", all.self_trade),
        ("reduce_only_cuts", all.reduce_only_cuts),
        ("stops_stored", all.stops_stored),
        ("stops_fired", all.stops_fired),
        ("stops_cancelled", all.stops_cancelled),
        ("stops_taken", all.stops_taken),
        ("cancels", all.cancels),
        ("cancel_all", all.cancel_all),
        ("cancel_all_many", all.cancel_all_many),
        ("modify_in_place", all.modify_in_place),
        ("modify_reinsert", all.modify_reinsert),
        ("sweeps", all.sweeps),
        ("other_drains", all.other_drains),
        ("reloads", all.reloads),
        ("skipped_saves", all.skipped_saves),
        ("full_builds", all.full_builds),
    ] {
        assert!(n >= 3, "{name} reached {n} times: {all:?}");
    }
}

/// Only the journaled levels are encoded again: a book of 20 levels, one
/// order added to one level, then one level emptied.
#[test]
fn classic_row_encodes_only_journaled_levels() {
    let mut book = OrderBook::new(1, fp(1), fp(1));
    for i in 0..10 {
        book.place_order(params(true, 90 + i, 2), addr(1), 0);
        book.place_order(params(false, 111 + i, 2), addr(2), 0);
    }
    let mut totals = ClassicRowStats::default();
    let mut saves = 0;
    let (row, stats) = book.classic_row().unwrap();
    assert_eq!(row, borsh::to_vec(&book).unwrap());
    assert!(stats.full_build, "first row builds the cache: {stats:?}");
    assert_eq!(stats.levels_encoded, 20, "{stats:?}");
    book.discard_level_ops();
    book.place_order(params(true, 95, 1), addr(3), 1);
    let reference = borsh::to_vec(&book).unwrap();
    let (row, stats) = book.classic_row().unwrap();
    assert_eq!(row, reference);
    assert_eq!(
        (stats.full_build, stats.levels_encoded, stats.levels_reused, stats.levels_removed),
        (false, 1, 19, 0),
        "{stats:?}"
    );
    book.discard_level_ops();
    // A crossing sell takes the best bid level (99) whole.
    let r = book.place_order(params(false, 99, 2), addr(4), 2);
    assert_eq!(r.fills.len(), 1);
    let reference = borsh::to_vec(&book).unwrap();
    let (row, stats) = book.classic_row().unwrap();
    assert_eq!(row, reference);
    assert_eq!(
        (stats.full_build, stats.levels_encoded, stats.levels_reused, stats.levels_removed),
        (false, 0, 19, 1),
        "{stats:?}"
    );
    book.discard_level_ops();
    save(&mut book, "after", &mut totals, &mut saves);
}

/// A level-journal drain other than `classic_row`'s (a mode-1/2/3 save, a
/// full write, a bare discard) and a loaded order (no journal entry) drop
/// the kept levels: the next row is a full build, still exact.
#[test]
fn classic_row_rebuilds_after_other_drains_and_loaded_orders() {
    type Drain = fn(&mut OrderBook);
    let drains: [(&str, Drain); 4] = [
        ("take_level_ops", |b| {
            b.take_level_ops();
        }),
        ("full_level_ops", |b| {
            b.full_level_ops();
        }),
        ("discard_level_ops", |b| b.discard_level_ops()),
        ("insert_loaded_order", |b| {
            let o = Order {
                id: 9_999,
                trader: addr(9),
                side: Side::Buy,
                // A level the journal does not hold.
                price: fp(80),
                remaining_qty: fp(3),
                original_qty: fp(3),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                timestamp: 7,
                reduce_only: false,
                client_order_id: None,
            };
            let seq = b.next_seq();
            b.set_next_seq(seq + 1);
            b.insert_loaded_order(o, seq);
        }),
    ];
    for (name, drain) in drains {
        let mut book = OrderBook::new(1, fp(1), fp(1));
        for i in 0..5 {
            book.place_order(params(true, 95 + i, 2), addr(1), 0);
        }
        book.classic_row().unwrap();
        book.discard_level_ops();
        // A change, then the other drain takes its journal entry.
        book.place_order(params(true, 96, 1), addr(2), 1);
        drain(&mut book);
        let reference = borsh::to_vec(&book).unwrap();
        let (row, stats) = book.classic_row().unwrap();
        assert!(row == reference, "{name}: stale level bytes");
        assert!(stats.full_build, "{name}: {stats:?}");
    }
}

/// The batched cancel-all (`try_cancel_all_batch`: >= 32 of the trader's
/// orders, >= 1,024 in the book, concentrated on one level) and
/// `cancel_all_many`'s compaction keep the row exact.
#[test]
fn classic_row_after_batched_cancel_all() {
    let mut book = OrderBook::new(1, fp(1), fp(1));
    // One deep bid level (the batch needs the first target's level >= 1,024
    // deep), a few shallow ones, and asks.
    for i in 0..1_600u64 {
        let trader = if i % 3 == 0 { addr(1) } else { addr(2 + (i % 5) as u8) };
        let (is_buy, price) = match i % 7 {
            0..=4 => (true, 95),
            5 => (true, 90 + (i % 4) as i64),
            _ => (false, 110 + (i % 4) as i64),
        };
        book.place_order(params(is_buy, price, 1), trader, 0);
    }
    // Levels no cancel below touches (kept bytes copied).
    for p in 120..125 {
        book.place_order(params(false, p, 1), addr(9), 0);
    }
    let mut totals = ClassicRowStats::default();
    let mut saves = 0;
    save(&mut book, "setup", &mut totals, &mut saves);
    let ids = book.trader_orders[&addr(1)].to_vec();
    assert!(ids.len() >= 32 && book.order_index.len() >= 1_024);
    assert!(book.cancel_batch_has_concentrated_prefix(&ids), "the batch path is taken");
    let gone = book.cancel_all(addr(1), None);
    assert_eq!(gone.len(), ids.len());
    save(&mut book, "batched cancel_all", &mut totals, &mut saves);
    let many = book.cancel_all_many(&[addr(2), addr(3), addr(2)]);
    assert!(many[0].len() > 10 && many[1].len() > 10 && many[2].is_empty());
    save(&mut book, "cancel_all_many", &mut totals, &mut saves);
    assert!(totals.levels_reused > 0 && totals.levels_encoded > 0, "{totals:?}");
}
