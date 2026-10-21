//! 8k L3.3 microbench: the Classic whole-book encode, `borsh::to_vec(book)`
//! (`save_order_books`' Classic arm with `TORUS_BOOK_FAST_ENCODE=0`) vs
//! `OrderBook::encode_classic` (the default since s131: one allocation sized
//! from the cloid index, one copy per resting order) vs
//! `encode_classic_size_pass` (the ranker's
//! spec: an exact size pass over every order first), jemalloc as the global
//! allocator as in the node.
//!
//! Shapes: 300 books holding 1.6M resting orders in total (the P1M deep book,
//! results doc s96.3) and 0.21M (P0, the standard load); each plain (GTC
//! limits, no cloid: the bench load) and mixed (10% with a cloid, 1/7
//! reduce-only, a TIF mix, 5 pending stops per book). Every book's bytes are
//! checked equal first. A rep encodes all 300 books and keeps the blobs (as
//! the overlay does). Two allocator regimes: `drop` frees a rep's blobs right
//! after it; `hold` keeps them while the next rep encodes (the overlay holds
//! a block's blobs until its flush), so the encode writes into fresh memory.
//! Arms run A B S S B A per round; medians over all samples.
//!
//! Run: `cargo bench -p torus-core --bench book_encode` (env `REPS` = rounds,
//! default 11: 22 samples per arm and regime; `SHAPE` = plain | mixed | p1m |
//! p0 runs a subset; `SPREAD` = per-book size spread in percent, default 0).

use std::hint::black_box;
use std::time::Instant;

use torus_core::order_book::{Order, OrderBook, StopOrder};
use torus_types::{Address, FixedPoint, OrderType, Side, TimeInForce};

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const BOOKS: usize = 300;
const LEVELS_PER_SIDE: usize = 100;

/// Small deterministic PRNG (xorshift64*).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// `total` split as evenly as possible into `parts`; part `i`'s share.
fn share(total: usize, parts: usize, i: usize) -> usize {
    total / parts + usize::from(i < total % parts)
}

fn build_books(total_orders: usize, mixed: bool) -> Vec<OrderBook> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut id: u128 = 1;
    let mut books = Vec::with_capacity(BOOKS);
    // `SPREAD` = percent: book m holds total/300 x (1 +- SPREAD%), uniform
    // (the total stays exact). 0 = every book the same size.
    let spread: f64 = std::env::var("SPREAD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
        / 100.0;
    let weights: Vec<f64> = (0..BOOKS)
        .map(|_| 1.0 + spread * ((rng.next() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0))
        .collect();
    let wsum: f64 = weights.iter().sum();
    let mut sizes: Vec<usize> = weights
        .iter()
        .map(|w| (total_orders as f64 * w / wsum) as usize)
        .collect();
    let short = total_orders - sizes.iter().sum::<usize>();
    for sz in sizes.iter_mut().take(short) {
        *sz += 1;
    }
    for (m, &spread_size) in sizes.iter().enumerate() {
        let per_book = if spread == 0.0 {
            share(total_orders, BOOKS, m)
        } else {
            spread_size
        };
        let mut book = OrderBook::new(
            m as u64,
            FixedPoint::from_raw(1_000),
            FixedPoint::from_raw(1),
        );
        book.set_last_trade_price(Some(FixedPoint::from_raw(5_000_000)));
        // Canonical load order: bids ascending (price, seq), then asks; seq
        // ascending within a level.
        let mut seq: u64 = 1;
        for (s, side) in [Side::Buy, Side::Sell].into_iter().enumerate() {
            let base: i128 = if side == Side::Buy {
                4_000_000
            } else {
                5_001_000
            };
            let per_side = share(per_book, 2, s);
            for lvl in 0..LEVELS_PER_SIDE {
                let price = FixedPoint::from_raw(base + lvl as i128 * 1_000);
                for _ in 0..share(per_side, LEVELS_PER_SIDE, lvl) {
                    let r = rng.next();
                    let cloid = (mixed && r % 10 == 1).then_some(u128::from(r) << 64 | id);
                    let qty = FixedPoint::from_raw(1 + (r >> 40) as i128);
                    let time_in_force = if mixed && r % 5 == 2 {
                        TimeInForce::PostOnly
                    } else {
                        TimeInForce::GTC
                    };
                    book.insert_loaded_order(
                        Order {
                            id,
                            trader: Address::from([(r >> 16) as u8; 20]),
                            side,
                            price,
                            remaining_qty: qty,
                            original_qty: qty,
                            order_type: OrderType::Limit,
                            time_in_force,
                            timestamp: 1_700_000_000_000 + id as u64,
                            reduce_only: mixed && r.is_multiple_of(7),
                            client_order_id: cloid,
                        },
                        seq,
                    );
                    id += 1;
                    seq += 1;
                }
            }
        }
        if mixed {
            for k in 0..5u128 {
                let stop = StopOrder {
                    id: 1_000_000_000 + m as u128 * 10 + k,
                    trader: Address::from([0xA0 + k as u8; 20]),
                    market_id: m as u64,
                    side: if k.is_multiple_of(2) { Side::Buy } else { Side::Sell },
                    trigger_price: FixedPoint::from_raw(4_500_000),
                    limit_price: (k.is_multiple_of(2)).then_some(FixedPoint::from_raw(4_600_000)),
                    price_cap: FixedPoint::from_raw(i128::from(k % 2 == 1) * 4_700_000),
                    quantity: FixedPoint::from_raw(10),
                    time_in_force: TimeInForce::GTC,
                    timestamp: 1,
                    reduce_only: k == 3,
                    client_order_id: (k == 1).then_some(77),
                };
                book.restore_stop_row(&borsh::to_vec(&stop).unwrap())
                    .unwrap();
            }
        }
        books.push(book);
    }
    books
}

#[derive(Clone, Copy)]
enum Arm {
    Borsh,
    Fast,
    SizePass,
}

fn encode_all(books: &[OrderBook], arm: Arm) -> (f64, Vec<Vec<u8>>) {
    let mut out = Vec::with_capacity(books.len());
    let t = Instant::now();
    for b in books {
        let v = match arm {
            Arm::Borsh => borsh::to_vec(b).unwrap(),
            Arm::Fast => b.encode_classic(),
            Arm::SizePass => b.encode_classic_size_pass(),
        };
        out.push(black_box(v));
    }
    (t.elapsed().as_secs_f64() * 1e3, out)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let reps: usize = std::env::var("REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(11);
    for (name, total) in [
        ("P1M 1.6M resting", 1_600_000usize),
        ("P0 0.21M resting", 210_000),
    ] {
        for mixed in [false, true] {
            // `SHAPE=plain|mixed|p1m|p0` runs a subset (one process per shape).
            if let Ok(only) = std::env::var("SHAPE") {
                let want = match only.as_str() {
                    "plain" => !mixed,
                    "mixed" => mixed,
                    "p1m" => total == 1_600_000,
                    "p0" => total == 210_000,
                    _ => true,
                };
                if !want {
                    continue;
                }
            }
            let books = build_books(total, mixed);
            let orders: usize = books.iter().map(|b| b.order_count()).sum();
            assert_eq!(orders, total);
            // Bytes equal, book by book, and no slack in the fast encode.
            let mut bytes = 0usize;
            for b in &books {
                let want = borsh::to_vec(b).unwrap();
                let fast = b.encode_classic();
                assert!(fast == want, "market {}: bytes differ", b.market_id);
                assert_eq!(fast.capacity(), fast.len(), "market {}: slack", b.market_id);
                assert!(b.encode_classic_size_pass() == want);
                bytes += want.len();
            }
            let variant = if mixed { "mixed" } else { "plain" };
            for hold in [false, true] {
                let mut t = [Vec::new(), Vec::new(), Vec::new()];
                let mut held: Option<Vec<Vec<u8>>> = None;
                // Warm-up round, then the timed ones.
                for round in 0..=reps {
                    for arm in [
                        Arm::Borsh,
                        Arm::Fast,
                        Arm::SizePass,
                        Arm::SizePass,
                        Arm::Fast,
                        Arm::Borsh,
                    ] {
                        let (ms, out) = encode_all(&books, arm);
                        if hold {
                            held = Some(out); // the previous rep's blobs drop here
                        } else {
                            drop(out);
                        }
                        if round > 0 {
                            t[arm as usize].push(ms);
                        }
                    }
                }
                drop(held);
                let [a, f, s] = &mut t;
                let (ma, mf, ms) = (median(a), median(f), median(s));
                println!(
                    "{name} {variant} [{regime}]: {orders} orders, {mb:.1} MB/rep, n={n} | borsh::to_vec {ma:.2} ms [{amin:.2}-{amax:.2}] | encode_classic {mf:.2} ms [{fmin:.2}-{fmax:.2}] cut {cf:.1}% ({sf:.2}x) | size-pass variant {ms:.2} ms cut {cs:.1}% ({ss:.2}x) | bytes equal",
                    regime = if hold { "hold" } else { "drop" },
                    mb = bytes as f64 / 1e6,
                    n = a.len(),
                    amin = a[0],
                    amax = a[a.len() - 1],
                    fmin = f[0],
                    fmax = f[f.len() - 1],
                    cf = (1.0 - mf / ma) * 100.0,
                    sf = ma / mf,
                    cs = (1.0 - ms / ma) * 100.0,
                    ss = ma / ms,
                );
            }
        }
    }
}
