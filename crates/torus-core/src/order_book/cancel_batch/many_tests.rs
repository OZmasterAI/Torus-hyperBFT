//! Differential tests: `cancel_all_many(senders)` must leave the book, every
//! persistence journal/cache and the per-sender results exactly as the
//! sequential `senders.iter().map(|s| cancel_all(*s, None))` does.

use super::tests::{addr, assert_same, fp, order};
use super::*;

/// Resting orders per trader in the fixtures (the old per-market cap; the
/// book has none now).
const PER_TRADER: usize = 200;

/// Tiny deterministic LCG so every scenario is reproducible with no deps.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn sequential(book: &mut OrderBook, senders: &[Address]) -> Vec<Vec<Order>> {
    senders.iter().map(|s| book.cancel_all(*s, None)).collect()
}

/// `assert_same` plus what the borsh bytes cannot show: level keys (an empty
/// queue left behind serializes as nothing) and the id allocator.
fn assert_books_equal(a: &mut OrderBook, b: &mut OrderBook) {
    assert_eq!(
        a.bids.keys().collect::<Vec<_>>(),
        b.bids.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        a.asks.keys().collect::<Vec<_>>(),
        b.asks.keys().collect::<Vec<_>>()
    );
    assert!(a
        .bids
        .values()
        .chain(a.asks.values())
        .all(|q| !q.is_empty()));
    assert_eq!(a.next_id, b.next_id);
    assert_eq!(a.pending_stop_count(), b.pending_stop_count());
    assert_same(a, b);
}

/// Batched vs sequential on two identical books, then assert full equality.
fn check(a: &mut OrderBook, b: &mut OrderBook, senders: &[Address]) {
    let got = a.cancel_all_many(senders);
    let want = sequential(b, senders);
    assert_eq!(got.len(), senders.len());
    assert_eq!(got, want, "per-sender results differ for {senders:?}");
    assert_books_equal(a, b);
}

fn stop(id: u64, trader: Address) -> StopOrder {
    StopOrder {
        id: id as OrderId,
        trader,
        market_id: 1,
        side: Side::Buy,
        trigger_price: fp(200),
        limit_price: None,
        price_cap: FixedPoint::ZERO,
        quantity: fp(1),
        time_in_force: TimeInForce::GTC,
        timestamp: 0,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Mode 0 = no caches, 2 = L3 level-hash cache, 3 = chunked digest.
fn new_book(mode: usize) -> OrderBook {
    let mut book = OrderBook::new(1, fp(1), fp(1));
    book.set_next_seq(1);
    book.set_next_order_id(5_000_000);
    if mode == 2 {
        book.ensure_level_hash_cache(1 << 20);
    }
    if mode == 3 {
        book.set_level_hash_chunked(true);
    }
    book
}

/// Prime persistence state and caches the way a saved book looks.
fn prime(book: &mut OrderBook) {
    book.full_row_ops();
    book.full_level_ops();
    let keys: Vec<_> = book.level_exists.iter().copied().collect();
    book.level_journal.extend(keys);
    book.take_level_ops();
    book.take_row_ops();
}

const LEVELS: [(Side, i64); 6] = [
    (Side::Buy, 100),
    (Side::Buy, 99),
    (Side::Sell, 110),
    (Side::Sell, 111),
    (Side::Buy, 95),
    (Side::Sell, 115),
];

/// A book shaped like the s63 profile: nearly all orders in two deep levels
/// per side, a few shallow ones, traders' orders interleaved through FIFO
/// queues, plus priority-changing modifies, mid-queue cancels, partial fills
/// and pending stops (incl. a stop-only trader `traders + 1`).
fn random_book(seed: u64, mode: usize, per_level: usize, traders: u64) -> OrderBook {
    let mut rng = Lcg(seed);
    let mut book = new_book(mode);
    let mut id = 1u64;
    for _ in 0..per_level * 4 {
        let trader = addr(1 + rng.below(traders) as u8);
        if book.trader_orders.get(&trader).map_or(0, TraderOrders::len) >= PER_TRADER {
            continue;
        }
        let level = if rng.below(40) == 0 {
            4 + rng.below(2)
        } else {
            rng.below(4)
        } as usize;
        let (side, price) = LEVELS[level];
        book.insert_order(order(id, trader, side, price));
        id += 1;
    }
    for _ in 0..24 {
        let trader = addr(1 + rng.below(traders) as u8);
        let Some(ids) = book.trader_orders.get(&trader).map(TraderOrders::to_vec) else {
            continue;
        };
        let target = ids[rng.below(ids.len() as u64) as usize];
        let side = book.order_index[&target].side;
        match rng.below(3) {
            0 => {
                book.modify_order(target, None, Some(fp(2))).unwrap();
            }
            1 => {
                let price = match side {
                    Side::Buy => 99 + rng.below(2) as i64,
                    Side::Sell => 110 + rng.below(2) as i64,
                };
                book.modify_order(target, Some(fp(price)), None).unwrap();
            }
            _ => {
                book.cancel_order(target).unwrap();
            }
        }
    }
    // Partial fills pop/shrink the fronts of the best levels.
    for (is_buy, price) in [(false, 99), (true, 111)] {
        book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy,
                price: fp(price),
                quantity: fp(7),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::IOC,
                reduce_only: false,
                client_order_id: None,
            },
            addr(250),
            99,
        );
    }
    for t in [1, 3, traders + 1] {
        book.pending_stops.push(stop(900_000 + t, addr(t as u8)));
    }
    book.pending_stops.push(stop(900_999, addr(251)));
    prime(&mut book);
    book
}

/// A random run of senders: repeats, traders without orders (`traders+2..`),
/// the stop-only trader, and sometimes an immediate duplicate.
fn random_senders(rng: &mut Lcg, traders: u64) -> Vec<Address> {
    let len = 1 + rng.below(12) as usize;
    let mut senders = Vec::with_capacity(len);
    for _ in 0..len {
        if !senders.is_empty() && rng.below(6) == 0 {
            let dup = senders[rng.below(senders.len() as u64) as usize];
            senders.push(dup);
        } else {
            senders.push(addr(1 + rng.below(traders + 3) as u8));
        }
    }
    senders
}

#[test]
fn cancel_all_many_matches_sequential_randomized() {
    for seed in 0..24u64 {
        for mode in [0, 2, 3] {
            let traders = 4 + seed % 20;
            let mut a = random_book(seed, mode, 400, traders);
            let mut b = random_book(seed, mode, 400, traders);
            assert_books_equal(&mut a, &mut b);
            let mut rng = Lcg(seed ^ 0x5EED);
            for round in 0..3u64 {
                let senders = random_senders(&mut rng, traders);
                check(&mut a, &mut b, &senders);
                // Later appends/cancels exercise the preserved seq allocator,
                // invalidated hash prefixes and chunk marks.
                for book in [&mut a, &mut b] {
                    for k in 0..6u64 {
                        let (side, price) = LEVELS[(k % 4) as usize];
                        let trader = addr(1 + (seed + k + round) as u8 % traders as u8);
                        book.insert_order(order(3_000_000 + round * 10 + k, trader, side, price));
                    }
                }
                assert_books_equal(&mut a, &mut b);
            }
        }
    }
}

/// The s63 shape at depth: ~99% of orders in two deep levels per side, many
/// traders near 200 orders each, a run of ten cancel-alls.
#[test]
fn cancel_all_many_matches_sequential_deep_levels() {
    for mode in [0, 2, 3] {
        let mut a = random_book(7, mode, 8_000, 170);
        let mut b = random_book(7, mode, 8_000, 170);
        assert!(a.order_count() > 30_000);
        let senders: Vec<_> = [3u8, 17, 42, 99, 17, 150, 8, 60, 171, 121]
            .iter()
            .map(|&n| addr(n))
            .collect();
        check(&mut a, &mut b, &senders);
        let again: Vec<_> = (1..=40u8).map(addr).collect();
        check(&mut a, &mut b, &again);
    }
}

#[test]
fn cancel_all_many_empty_book_and_empty_run() {
    for mode in [0, 2, 3] {
        let mut a = new_book(mode);
        let mut b = new_book(mode);
        check(&mut a, &mut b, &[]);
        check(&mut a, &mut b, &[addr(1), addr(2), addr(1)]);
        let mut a = random_book(3, mode, 50, 5);
        let mut b = random_book(3, mode, 50, 5);
        assert!(a.cancel_all_many(&[]).is_empty());
        assert_books_equal(&mut a, &mut b);
    }
}

/// Duplicates get empty results (as the second sequential call does), the
/// stop-only trader loses its stop (as its sequential call does), and
/// orderless senders are no-ops.
#[test]
fn cancel_all_many_duplicate_absent_and_stop_only_senders() {
    for mode in [0, 2, 3] {
        let mut a = random_book(11, mode, 300, 6);
        let mut b = random_book(11, mode, 300, 6);
        let stop_only = addr(7);
        let senders = [
            addr(2),
            addr(9),
            addr(2),
            stop_only,
            addr(5),
            addr(2),
            addr(5),
        ];
        let got = a.cancel_all_many(&senders);
        assert!(!got[0].is_empty() && !got[4].is_empty());
        assert!(got[1].is_empty() && got[2].is_empty() && got[3].is_empty());
        assert!(got[5].is_empty() && got[6].is_empty());
        assert_eq!(got, sequential(&mut b, &senders));
        assert!(!a.pending_stops.iter().any(|s| s.trader == stop_only));
        assert_books_equal(&mut a, &mut b);
    }
}

/// Runs that empty whole levels, then re-create them.
#[test]
fn cancel_all_many_empties_levels_then_recreates() {
    for mode in [0, 2, 3] {
        let mut a = new_book(mode);
        let mut b = new_book(mode);
        for book in [&mut a, &mut b] {
            for i in 0..64u64 {
                book.insert_order(order(i + 1, addr(1 + (i % 3) as u8), Side::Buy, 98));
                book.insert_order(order(i + 101, addr(1 + (i % 2) as u8), Side::Sell, 112));
                book.insert_order(order(i + 201, addr(4), Side::Sell, 113));
            }
            prime(book);
        }
        check(&mut a, &mut b, &[addr(2), addr(1), addr(3)]);
        assert!(!a.bids.contains_key(&fp(98)) && !a.asks.contains_key(&fp(112)));
        for book in [&mut a, &mut b] {
            book.insert_order(order(500, addr(1), Side::Buy, 98));
            book.insert_order(order(501, addr(2), Side::Sell, 112));
        }
        assert_books_equal(&mut a, &mut b);
        check(&mut a, &mut b, &[addr(4), addr(2), addr(1)]);
    }
}

/// Malformed/stale indexes must take the exact sequential path.
#[test]
fn cancel_all_many_falls_back_on_stale_or_shared_indexes() {
    for case in 0..5 {
        for mode in [0, 2, 3] {
            // Mode 3 marks a queue-mutated order's chunk dirty by its seq, so
            // a deleted order_seq entry trips its debug_assert (no seq).
            if case == 1 && mode == 3 && cfg!(debug_assertions) {
                continue;
            }
            let mut a = random_book(21, mode, 300, 6);
            let mut b = random_book(21, mode, 300, 6);
            for book in [&mut a, &mut b] {
                let id = book.trader_orders[&addr(3)].to_vec()[2];
                match case {
                    0 => {
                        book.order_index.remove(&id);
                    }
                    1 => {
                        book.order_seq.remove(&id);
                    }
                    2 => {
                        book.order_index.get_mut(&id).unwrap().price = fp(999);
                    }
                    // The same id listed under two traders of the run.
                    3 => book.trader_orders.get_mut(&addr(4)).unwrap().push(id),
                    _ => book
                        .trader_orders
                        .get_mut(&addr(4))
                        .unwrap()
                        .edit(|v| v.insert(0, 777_777)),
                }
            }
            check(&mut a, &mut b, &[addr(4), addr(3), addr(1)]);
        }
    }
}

/// Epoch overflow keeps the sequential path's panic/wrap behavior.
#[test]
fn cancel_all_many_preserves_epoch_overflow_behavior() {
    let mut a = random_book(5, 2, 300, 6);
    let mut b = random_book(5, 2, 300, 6);
    for book in [&mut a, &mut b] {
        book.level_epoch.insert(
            (crate::book_rows::SIDE_TAG_ASK, fp(110).raw()),
            u64::MAX - 3,
        );
    }
    let senders = [addr(1), addr(2), addr(3), addr(4)];
    let got =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.cancel_all_many(&senders)));
    let want = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sequential(&mut b, &senders)
    }));
    assert_eq!(got.is_err(), want.is_err());
    assert_eq!(got.is_err(), cfg!(debug_assertions));
    if let (Ok(got), Ok(want)) = (got, want) {
        assert_eq!(got, want);
    }
    assert_books_equal(&mut a, &mut b);
}

/// Every owner assignment of small queues (A=1, B=2, survivor Z=3) on a bid
/// and an ask level, with no filler, a deep survivor tail (drives the
/// end-compaction branch) or a deep survivor middle, across run shapes.
#[test]
fn cancel_all_many_exhaustive_small_queues() {
    let runs: [&[u8]; 5] = [&[1, 2], &[2, 1], &[1, 2, 1, 2], &[9, 2, 1], &[3, 1, 2]];
    let mut compacted = 0usize;
    for len in 1..=6u32 {
        for code in 0..3usize.pow(len) {
            let owners: Vec<u8> = (0..len)
                .scan(code, |c, _| {
                    let d = (*c % 3) as u8 + 1;
                    *c /= 3;
                    Some(d)
                })
                .collect();
            for filler in 0..3 {
                for mode in [0, 2, 3] {
                    let build = || {
                        let mut book = new_book(mode);
                        let mut id = 1u64;
                        let mut layout: Vec<u8> = owners.clone();
                        let z = vec![3u8; 24];
                        match filler {
                            1 => layout.extend(z),
                            2 => {
                                let mid = layout.len() / 2;
                                layout.splice(mid..mid, z);
                            }
                            _ => {}
                        }
                        for &owner in &layout {
                            book.insert_order(order(id, addr(owner), Side::Buy, 100));
                            book.insert_order(order(id + 1, addr(owner), Side::Sell, 110));
                            id += 2;
                        }
                        // Reversed receipt order on one trader: output order
                        // must follow trader_orders, not FIFO.
                        if let Some(ids) = book.trader_orders.get_mut(&addr(2)) {
                            ids.edit(|v| v.reverse());
                        }
                        prime(&mut book);
                        book
                    };
                    for run in runs {
                        let senders: Vec<_> = run.iter().map(|&n| addr(n)).collect();
                        let mut a = build();
                        let mut b = build();
                        if let Some(plan) = a.plan_cancel_all_many(&senders) {
                            let mut start = 0;
                            for &(tag, price, end) in &plan.levels {
                                let len = if tag == crate::book_rows::side_tag(Side::Buy) {
                                    a.bids[&price].len()
                                } else {
                                    a.asks[&price].len()
                                };
                                let removal = choose_removal(len, &plan.targets[start..end]);
                                compacted += usize::from(matches!(removal, Removal::Compact(_)));
                                start = end;
                            }
                        }
                        check(&mut a, &mut b, &senders);
                    }
                }
            }
        }
    }
    assert!(
        compacted > 1_000,
        "compaction branch barely exercised: {compacted}"
    );
}

/// The profile shape — several traders' orders scattered through a deep
/// level — must take the one-pass compaction, not repeated `remove`.
#[test]
fn cancel_all_many_plan_compacts_deep_multi_owner_levels() {
    let book = random_book(7, 0, 8_000, 170);
    let senders: Vec<_> = (1..=10u8).map(addr).collect();
    let plan = book.plan_cancel_all_many(&senders).expect("eligible");
    let mut start = 0;
    let mut compacted = 0;
    for &(tag, price, end) in &plan.levels {
        let queue = if tag == crate::book_rows::side_tag(Side::Buy) {
            &book.bids[&price]
        } else {
            &book.asks[&price]
        };
        let targets = &plan.targets[start..end];
        assert!(targets.windows(2).all(|w| w[0].position < w[1].position));
        if queue.len() > 1_000 && targets.len() > 100 {
            assert!(matches!(
                choose_removal(queue.len(), targets),
                Removal::Compact(_)
            ));
            compacted += 1;
        }
        start = end;
    }
    assert_eq!(compacted, 4, "all four deep levels compact once");
    // A single owner keeps the existing per-sender path.
    assert!(book
        .plan_cancel_all_many(&[addr(3), addr(3), addr(240)])
        .is_none());
}

/// Timing probe (run with `--ignored --nocapture`): the s63 market shape —
/// ~300k resting orders in two deep levels per side, 1 500 traders near 200
/// orders each — and a run of ten cancel-alls, sequential vs batched.
#[test]
#[ignore]
fn cancel_all_many_timing_probe_deep_levels() {
    fn trader(n: u64) -> Address {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        Address::from(bytes)
    }
    let build = |mode: usize| {
        let mut rng = Lcg(42);
        let mut book = new_book(mode);
        for id in 1..=300_000u64 {
            let t = trader(1 + rng.below(1_500));
            if book.trader_orders.get(&t).map_or(0, TraderOrders::len) >= PER_TRADER {
                continue;
            }
            let (side, price) = LEVELS[rng.below(4) as usize];
            book.insert_order(order(id, t, side, price));
        }
        prime(&mut book);
        book
    };
    for mode in [0, 3] {
        let (mut seq_ns, mut many_ns) = (Vec::new(), Vec::new());
        for round in 0..5u64 {
            let senders: Vec<_> = (0..10).map(|k| trader(1 + round * 97 + k * 131)).collect();
            let mut a = build(mode);
            let mut b = build(mode);
            let t = std::time::Instant::now();
            let got = a.cancel_all_many(&senders);
            many_ns.push(t.elapsed().as_nanos());
            let t = std::time::Instant::now();
            let want = sequential(&mut b, &senders);
            seq_ns.push(t.elapsed().as_nanos());
            assert_eq!(got, want);
            assert_books_equal(&mut a, &mut b);
        }
        seq_ns.sort_unstable();
        many_ns.sort_unstable();
        eprintln!(
            "mode {mode}: {} orders, 10 cancel-alls: sequential median {:.2} ms, batched median {:.2} ms (x{:.1})",
            build(mode).order_count(),
            seq_ns[2] as f64 / 1e6,
            many_ns[2] as f64 / 1e6,
            seq_ns[2] as f64 / many_ns[2] as f64
        );
    }
}

/// s80 cancel-all-generation-bump premise probe (run with `--ignored
/// --nocapture`, release). The bench shape: 300 books, 600 traders, GTC
/// limits within 5 ticks of the mid, one side per (trader, market), no
/// cloids / reduce-only; `PROBE_RESTING` resting orders (default 104k and
/// 1.1M); per block 24 distinct cancel-all senders, then the cancelled count
/// is re-placed by random traders (untimed, identical in both arms).
///
/// Arm A (today): per book, ascending, `plan` + `apply` of
/// `cancel_all_many` over the block's senders holding orders there, plus the
/// per-order margin term. Arm B (generation bump, lean): per (sender, book)
/// an O(1) bump (the book's `trader_orders` entry moved to a stale list, the
/// trader's generation +1, reduce-only forget, the maintained-sum read); per
/// block a top purge (stale fronts of the best bid/ask popped with their
/// index removal, so BBO is live; stale = the order's placement generation
/// differs from its trader's); at window close (every `PROBE_WINDOW` = 100
/// blocks) the deferred index removal of every stale order and one `retain`
/// per touched level. Also timed: the maintained-sum upkeep a rest and a fill
/// would pay (`upkeep`: two map updates per event). After the window both
/// arms' books must be identical (borsh bytes, levels, ids).
#[test]
#[ignore]
fn cancel_all_gen_bump_probe_bench_shape() {
    use std::time::Instant;
    fn trader(n: u64) -> Address {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(n + 1).to_be_bytes());
        bytes[19] = 0xAB;
        Address::from(bytes)
    }
    fn bench_order(id: u64, t: u64, m: u64, d: u64) -> Order {
        let is_buy = (t + m).is_multiple_of(2);
        let price = if is_buy { 1_000 - d as i64 } else { 1_000 + d as i64 };
        Order {
            id: id as OrderId,
            trader: trader(t),
            side: if is_buy { Side::Buy } else { Side::Sell },
            price: fp(price),
            remaining_qty: fp(5),
            original_qty: fp(5),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            timestamp: id,
            reduce_only: false,
            client_order_id: None,
        }
    }
    /// Stale iff the order's placement generation is not its trader's.
    fn is_stale(gens: &HashMap<Address, u32>, order_gen: &HashMap<OrderId, u32>, o: &Order) -> bool {
        gens.get(&o.trader).copied().unwrap_or(0) != order_gen[&o.id]
    }
    const BOOKS: u64 = 300;
    const TRADERS: u64 = 600;
    const CA: usize = 24;
    let env = |k: &str| std::env::var(k).ok();
    let window: u64 = env("PROBE_WINDOW").and_then(|v| v.parse().ok()).unwrap_or(100);
    let upkeep_events: u64 = env("PROBE_UPKEEP").and_then(|v| v.parse().ok()).unwrap_or(21_000);
    let mode: usize = env("PROBE_MODE").and_then(|v| v.parse().ok()).unwrap_or(0);
    let restings: Vec<u64> = env("PROBE_RESTING")
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![104_000, 1_100_000]);
    for resting in restings {
        let mut rng = Lcg(0x5EED_80CA ^ resting);
        let mut next_id = 1u64;
        let mut a: Vec<OrderBook> = (0..BOOKS).map(|_| new_book(mode)).collect();
        let mut b: Vec<OrderBook> = (0..BOOKS).map(|_| new_book(mode)).collect();
        let mut gens: Vec<HashMap<Address, u32>> = vec![HashMap::default(); BOOKS as usize];
        let mut order_gen: Vec<HashMap<OrderId, u32>> = vec![HashMap::default(); BOOKS as usize];
        let mut sums: Vec<HashMap<Address, FixedPoint>> = vec![HashMap::default(); BOOKS as usize];
        let mut stale: Vec<Vec<TraderOrders>> = (0..BOOKS).map(|_| Vec::new()).collect();
        let mut place = |a: &mut Vec<OrderBook>,
                         b: &mut Vec<OrderBook>,
                         gens: &Vec<HashMap<Address, u32>>,
                         order_gen: &mut Vec<HashMap<OrderId, u32>>,
                         rng: &mut Lcg| {
            let (t, m, d) = (rng.below(TRADERS), rng.below(BOOKS), 1 + rng.below(5));
            let o = bench_order(next_id, t, m, d);
            next_id += 1;
            let m = m as usize;
            order_gen[m].insert(o.id, gens[m].get(&o.trader).copied().unwrap_or(0));
            a[m].insert_order(o.clone());
            b[m].insert_order(o);
        };
        for _ in 0..resting {
            place(&mut a, &mut b, &gens, &mut order_gen, &mut rng);
        }
        for book in a.iter_mut().chain(b.iter_mut()) {
            prime(book);
        }
        let (mut a_plan, mut a_apply, mut a_margin) = (0u128, 0u128, 0u128);
        let (mut b_bump, mut b_purge, mut c_reclaim, mut upkeep) = (0u128, 0u128, 0u128, 0u128);
        let (mut hits, mut cancelled_n, mut purged_n, mut stale_max) = (0u64, 0u64, 0u64, 0usize);
        let mut sink = FixedPoint::ZERO;
        for _blk in 0..window {
            let mut senders: Vec<Address> = Vec::with_capacity(CA);
            while senders.len() < CA {
                let s = trader(rng.below(TRADERS));
                if !senders.contains(&s) {
                    senders.push(s);
                }
            }
            let mut blk_cancelled = 0u64;
            // ---- Arm A: today's per-book plan + apply + margin terms.
            for book in a.iter_mut() {
                let members: Vec<Address> =
                    senders.iter().copied().filter(|s| book.trader_orders.contains_key(s)).collect();
                if members.is_empty() {
                    continue;
                }
                hits += members.len() as u64;
                let t0 = Instant::now();
                let plan = book.plan_cancel_all_many(&members);
                let t1 = Instant::now();
                let out = match plan {
                    Some(plan) => book.apply_cancel_all_many(&members, plan),
                    None => members.iter().map(|s| book.cancel_all(*s, None)).collect(),
                };
                let t2 = Instant::now();
                for orders in &out {
                    for o in orders {
                        sink += o.price * o.remaining_qty / fp(20);
                    }
                    blk_cancelled += orders.len() as u64;
                }
                let t3 = Instant::now();
                a_plan += (t1 - t0).as_nanos();
                a_apply += (t2 - t1).as_nanos();
                a_margin += (t3 - t2).as_nanos();
                drop(out);
            }
            cancelled_n += blk_cancelled;
            // ---- Arm B: O(1) bump per (sender, book) with an entry ...
            let t0 = Instant::now();
            for (m, book) in b.iter_mut().enumerate() {
                for s in &senders {
                    let Some(ids) = book.trader_orders.remove(s) else { continue };
                    *gens[m].entry(*s).or_insert(0) += 1;
                    book.forget_reduce_only_trader(s);
                    if let Some(sum) = sums[m].remove(s) {
                        sink += sum;
                    }
                    stale[m].push(ids);
                }
            }
            let t1 = Instant::now();
            // ... then the top purge: stale fronts of the best levels popped,
            // with their index removal, until BBO is live.
            for (m, book) in b.iter_mut().enumerate() {
                let cache_on = book.level_hash_cache.is_some();
                let chunked_on = book.level_hash_chunked;
                for side in [Side::Buy, Side::Sell] {
                    loop {
                        let levels = match side {
                            Side::Buy => &mut book.bids,
                            Side::Sell => &mut book.asks,
                        };
                        let mut entry = match side {
                            Side::Buy => levels.last_entry(),
                            Side::Sell => levels.first_entry(),
                        };
                        let Some(queue) = entry.as_mut() else { break };
                        let price = *queue.key();
                        let q = queue.get_mut();
                        while q.front().is_some_and(|o| is_stale(&gens[m], &order_gen[m], o)) {
                            let o = q.pop_front().expect("front");
                            book.order_index.remove(&o.id);
                            let seq = book.order_seq.remove(&o.id);
                            let tag = crate::book_rows::side_tag(side);
                            book.row_journal.insert(o.id);
                            book.level_journal.insert((tag, price.raw()));
                            OrderBook::bump_level_epoch(cache_on, &mut book.level_epoch, tag, price.raw());
                            OrderBook::mark_chunk_dirty(chunked_on, &mut book.dirty_chunks, tag, price.raw(), seq);
                            purged_n += 1;
                        }
                        if q.is_empty() {
                            entry.expect("level").remove();
                            continue;
                        }
                        break;
                    }
                }
            }
            let t2 = Instant::now();
            b_bump += (t1 - t0).as_nanos();
            b_purge += (t2 - t1).as_nanos();
            stale_max = stale_max.max(stale.iter().flatten().map(TraderOrders::len).sum());
            // ---- Untimed: re-place the cancelled count in both arms; timed
            // separately: the maintained-sum upkeep per rest (+) and per fill
            // (-, emulated as a second update of the same entry).
            // `PROBE_UPKEEP` events per block (default 21k: the bench's
            // ~11k fills + ~10k resting placements), one sum update each.
            let t3 = Instant::now();
            let mut rng2 = Lcg(rng.0);
            for _ in 0..upkeep_events {
                let (t, m) = (rng2.below(TRADERS), rng2.below(BOOKS));
                let e = sums[m as usize].entry(trader(t)).or_insert(FixedPoint::ZERO);
                *e += fp(5);
            }
            upkeep += t3.elapsed().as_nanos();
            for _ in 0..blk_cancelled {
                place(&mut a, &mut b, &gens, &mut order_gen, &mut rng);
            }
        }
        // ---- Arm B window close: deferred index removal + one retain per
        // touched level.
        let t0 = Instant::now();
        for (m, book) in b.iter_mut().enumerate() {
            if stale[m].is_empty() {
                continue;
            }
            let cache_on = book.level_hash_cache.is_some();
            let chunked_on = book.level_hash_chunked;
            let mut touched: Vec<(u8, FixedPoint)> = Vec::new();
            for list in std::mem::take(&mut stale[m]) {
                for &id in list.iter() {
                    let Some(loc) = book.order_index.remove(&id) else { continue };
                    let seq = book.order_seq.remove(&id);
                    let tag = crate::book_rows::side_tag(loc.side);
                    book.row_journal.insert(id);
                    book.level_journal.insert((tag, loc.price.raw()));
                    OrderBook::bump_level_epoch(cache_on, &mut book.level_epoch, tag, loc.price.raw());
                    OrderBook::mark_chunk_dirty(chunked_on, &mut book.dirty_chunks, tag, loc.price.raw(), seq);
                    touched.push((tag, loc.price));
                }
            }
            touched.sort_unstable();
            touched.dedup();
            let (g, og) = (&gens[m], &order_gen[m]);
            for (tag, price) in touched {
                let levels = if tag == crate::book_rows::side_tag(Side::Buy) {
                    &mut book.bids
                } else {
                    &mut book.asks
                };
                let q = levels.get_mut(&price).expect("touched level");
                q.retain(|o| !is_stale(g, og, o));
                if q.is_empty() {
                    levels.remove(&price);
                }
            }
        }
        c_reclaim += t0.elapsed().as_nanos();
        for (x, y) in a.iter_mut().zip(b.iter_mut()) {
            assert_books_equal(x, y);
        }
        let w = window as f64;
        let ms = |ns: u128| ns as f64 / 1e6 / w;
        let a_tot = ms(a_plan + a_apply + a_margin);
        let b_tot = ms(b_bump + b_purge) + ms(c_reclaim) + ms(upkeep);
        eprintln!(
            "PROBE mode={mode} resting={resting} window={window} hits/blk={:.0} cancelled/blk={:.0} purged/blk={:.0} stale_max={stale_max} \
             | A/blk plan={:.3} apply={:.3} margin={:.3} total={:.3} ms ({:.0} ns/cancelled) | B/blk bump={:.3} purge={:.3} upkeep={:.3} \
             reclaim/window={:.2} ms (={:.3}/blk) total={:.3} ms | net CPU saving/blk={:.3} ms ({:.0}% of A) sink={}",
            hits as f64 / w,
            cancelled_n as f64 / w,
            purged_n as f64 / w,
            ms(a_plan),
            ms(a_apply),
            ms(a_margin),
            a_tot,
            a_tot * 1e6 * w / cancelled_n.max(1) as f64,
            ms(b_bump),
            ms(b_purge),
            ms(upkeep),
            c_reclaim as f64 / 1e6,
            ms(c_reclaim),
            b_tot,
            a_tot - b_tot,
            100.0 * (a_tot - b_tot) / a_tot,
            sink.raw() % 7,
        );
    }
}
