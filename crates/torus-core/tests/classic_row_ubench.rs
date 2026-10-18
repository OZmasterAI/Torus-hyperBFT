//! Item 6 Phase 5 step 3 cost µbench (release, on the node's allocator,
//! jemalloc): one native block's Classic encodes of every dirty book, the
//! reference (`borsh::to_vec`, today's `save_order_books` arm) vs
//! [`OrderBook::classic_row`] (the level dirty set), each followed
//! by the save's journal discards; plus `classic_row` with nothing dirty
//! (the assemble alone: header, the kept levels copied, stops).
//!
//! 300 books seeded with `resting` orders over `levels` price levels a side
//! (~600-840k resting orders after the blocks, the hsdb 600 s range). Each
//! block, per book, 390 operations: `top` = IOC takers filling at the top
//! and places within 5 levels of the mid; `random` = the same plus cancels
//! and qty-down modifies of random live orders (any level, any queue
//! position). Both book sets get the same operations (untimed); only the
//! saves are timed; the sets are checked equal at the end.
//!
//! `cargo test --release -p torus-core --test classic_row_ubench -- --ignored --nocapture`

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use torus_core::order_book::{ClassicRowStats, OrderBook};
use torus_types::{Address, FixedPoint, OrderId, OrderType, PlaceOrderParams, TimeInForce};

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

fn pick_live(book: &OrderBook, rng: &mut Rng) -> Option<OrderId> {
    let live: Vec<OrderId> = book
        .bid_queues()
        .chain(book.ask_queues())
        .flat_map(|(_, q)| q.iter().map(|o| o.id))
        .collect();
    (!live.is_empty()).then(|| live[rng.below(live.len() as u64) as usize])
}

#[test]
#[ignore = "perf µbench; run in release with --ignored --nocapture"]
fn ubench_classic_row_per_block() {
    const BOOKS: u64 = 300;
    const BLOCKS: usize = 12;
    for (resting, levels, random) in [
        (1_000i64, 50i64, false),
        (2_000, 100, false),
        (2_000, 50, true),
        (2_000, 100, true),
    ] {
        let mut sets: Vec<Vec<OrderBook>> = (0..2)
            .map(|_| (1..=BOOKS).map(|m| OrderBook::new(m, fp(1), fp(1))).collect())
            .collect();
        let mid = 10_000i64;
        for set in sets.iter_mut() {
            let mut rng = Rng(7);
            for book in set.iter_mut() {
                for i in 0..resting {
                    let is_buy = i % 2 == 0;
                    let off = 1 + rng.below(levels as u64) as i64;
                    let t = addr(1 + rng.below(200) as u8);
                    let p = params(is_buy, if is_buy { mid - off } else { mid + off }, 1 + rng.below(5) as i64);
                    book.place_order(p, t, 0);
                }
                book.discard_row_ops();
                book.discard_level_ops();
            }
        }
        for book in sets[1].iter_mut() {
            book.classic_row().unwrap();
            book.discard_row_ops();
            book.discard_level_ops();
        }
        let (mut ref_ms, mut cached_ms, mut assemble_ms) = (Vec::new(), Vec::new(), Vec::new());
        let mut stats = ClassicRowStats::default();
        let mut bytes = 0u64;
        for b in 0..BLOCKS as u64 {
            for set in sets.iter_mut() {
                let mut rng = Rng(1_000 + b);
                for book in set.iter_mut() {
                    for k in 0..390u64 {
                        let t = addr(1 + rng.below(200) as u8);
                        let op = k % 39;
                        if op < 16 || (!random && op >= 32) {
                            let is_buy = rng.below(2) == 0;
                            let p = if is_buy { mid + 1 } else { mid - 1 };
                            let p = PlaceOrderParams {
                                time_in_force: TimeInForce::IOC,
                                ..params(is_buy, p, 1 + rng.below(2) as i64)
                            };
                            book.place_order(p, t, b);
                        } else if op < 32 {
                            let is_buy = rng.below(2) == 0;
                            let off = 1 + rng.below(5) as i64;
                            let p = params(is_buy, if is_buy { mid - off } else { mid + off }, 1 + rng.below(3) as i64);
                            book.place_order(p, t, b);
                        } else if op < 38 {
                            if let Some(id) = pick_live(book, &mut rng) {
                                let _ = book.cancel_order(id);
                            }
                        } else if let Some(id) = pick_live(book, &mut rng) {
                            let _ = book.modify_order(id, None, Some(fp(1)));
                        }
                    }
                }
            }
            let t0 = std::time::Instant::now();
            for book in sets[0].iter_mut() {
                let row = borsh::to_vec(&*book).unwrap();
                std::hint::black_box(&row);
                book.discard_row_ops();
                book.discard_level_ops();
            }
            ref_ms.push(t0.elapsed().as_secs_f64() * 1e3);
            let t1 = std::time::Instant::now();
            for book in sets[1].iter_mut() {
                let (row, s) = book.classic_row().unwrap();
                std::hint::black_box(&row);
                book.discard_row_ops();
                book.discard_level_ops();
                stats.levels_encoded += s.levels_encoded;
                stats.levels_reused += s.levels_reused;
                bytes += s.bytes;
            }
            cached_ms.push(t1.elapsed().as_secs_f64() * 1e3);
            let t2 = std::time::Instant::now();
            for book in sets[1].iter_mut() {
                let (row, _) = book.classic_row().unwrap();
                std::hint::black_box(&row);
            }
            assemble_ms.push(t2.elapsed().as_secs_f64() * 1e3);
        }
        for (x, y) in sets[0].iter().zip(&sets[1]) {
            assert_eq!(borsh::to_vec(x).unwrap(), borsh::to_vec(y).unwrap());
        }
        let orders: usize = sets[0].iter().map(|b| b.order_count()).sum();
        let med = |v: &mut Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let n = BLOCKS as u64;
        println!(
            "ubench classic_row: {BOOKS} books, {} ops, seeded {resting} over {levels} levels a side ({orders} resting at the end, {:.1} MB of rows per block): reference {:.2} ms, classic_row {:.2} ms, assemble only {:.2} ms per block (medians of {BLOCKS}); per block levels encoded {} / copied {}",
            if random { "random" } else { "top" },
            bytes as f64 / n as f64 / 1e6,
            med(&mut ref_ms),
            med(&mut cached_ms),
            med(&mut assemble_ms),
            stats.levels_encoded / n,
            stats.levels_reused / n,
        );
    }
}
