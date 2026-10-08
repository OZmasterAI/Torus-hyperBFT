//! Position v2 savings µbench: the two PositionCache costs of a 300-market
//! parallel-settle block (~25k fills, two position read-modify-writes per
//! fill, ~170 keys per market).
//!
//! - `apply`: `apply_fill_cached_effect` on warm per-market caches (every
//!   load a cache hit), ns per call (load + `fill_transition` + write-back).
//! - `merge`: pass B's 300 `merge_disjoint` calls into one batch cache,
//!   ns per merged entry, without and with a `reserve` of the total first.
//!
//!   cargo test -p torus-core --release --test ubench_position_cache -- --ignored --nocapture

use std::hint::black_box;
use std::time::Instant;

use torus_core::position::{MarginType, Position, PositionCache, PositionManager};
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, MarketId};

const MARKETS: u64 = 300;
const FILLS_PER_MARKET: usize = 84; // 25,200 fills per block
const TRADERS_PER_MARKET: u64 = 120;
const ROUNDS: usize = 15;

fn trader(m: u64, i: u64) -> Address {
    let mut a = [0u8; 20];
    a[..8].copy_from_slice(&(i * 7919 + m).to_be_bytes());
    a[12..].copy_from_slice(&i.to_be_bytes());
    Address::new(a)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// One fill: (market, buyer, seller, qty raw, price raw).
fn fills() -> Vec<(MarketId, Address, Address, i128, i128)> {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut v = Vec::new();
    for m in 1..=MARKETS {
        for _ in 0..FILLS_PER_MARKET {
            let b = r.next() % TRADERS_PER_MARKET;
            let s = (b + 1 + r.next() % (TRADERS_PER_MARKET - 1)) % TRADERS_PER_MARKET;
            let qty = (1 + r.next() % 5_000) as i128 * 100_000; // 0.001 .. 5
            let price = (1_900 * FixedPoint::SCALE) + (r.next() % 20_000_000_000) as i128;
            v.push((m, trader(m, b), trader(m, s), qty, price));
        }
    }
    v
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

#[test]
#[ignore]
fn ubench_position_cache_apply() {
    let dir = tempfile::tempdir().unwrap();
    let pm = PositionManager::new(StateDb::open(dir.path()).unwrap());
    let fills = fills();
    let mut caches: Vec<PositionCache> = (0..MARKETS).map(|_| PositionCache::new()).collect();
    let run = |caches: &mut Vec<PositionCache>| {
        for &(m, b, s, q, p) in &fills {
            let c = &mut caches[(m - 1) as usize];
            let (q, p) = (FixedPoint::from_raw(q), FixedPoint::from_raw(p));
            black_box(pm.apply_fill_cached_effect(c, &b, m, true, q, p, MarginType::Cross).unwrap());
            black_box(pm.apply_fill_cached_effect(c, &s, m, false, q, p, MarginType::Cross).unwrap());
        }
    };
    run(&mut caches); // warm: every key now cached
    let mut ns = Vec::new();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        run(&mut caches);
        ns.push(t.elapsed().as_nanos() as f64 / (2 * fills.len()) as f64);
    }
    println!(
        "UBENCH apply_fill_cached_effect (warm, {} calls/round): median {:.1} ns/call, min {:.1}",
        2 * fills.len(),
        median(ns.clone()),
        ns.iter().cloned().fold(f64::MAX, f64::min)
    );
}

/// Pass B shape: 300 per-market caches of ~TRADERS_PER_MARKET dirty keys.
fn market_caches() -> (Vec<PositionCache>, usize) {
    let mut total = 0;
    let caches = (1..=MARKETS)
        .map(|m| {
            let mut c = PositionCache::new();
            for i in 0..TRADERS_PER_MARKET {
                c.set(Position {
                    trader: trader(m, i),
                    market_id: m,
                    is_long: i % 2 == 0,
                    size: FixedPoint::from_raw(i as i128 + 1),
                    entry_price: FixedPoint::from_raw(2_000 * FixedPoint::SCALE),
                    cost_basis: FixedPoint::from_raw(2_000 * (i as i128 + 1)),
                    realized_pnl: FixedPoint::ZERO,
                    isolated_margin: FixedPoint::ZERO,
                    margin_type: MarginType::Cross,
                });
                total += 1;
            }
            c
        })
        .collect();
    (caches, total)
}

#[test]
#[ignore]
fn ubench_position_cache_merge() {
    for reserve in [false, true] {
        let (mut ns, mut build) = (Vec::new(), Vec::new());
        for _ in 0..ROUNDS * 2 {
            let t = Instant::now();
            let (caches, total) = market_caches();
            build.push(t.elapsed().as_nanos() as f64 / total as f64);
            let t = Instant::now();
            let mut batch = PositionCache::new();
            if reserve {
                batch.reserve(caches.iter().map(PositionCache::len).sum());
            }
            for c in caches {
                batch.merge_disjoint(c);
            }
            ns.push(t.elapsed().as_nanos() as f64 / total as f64);
            assert_eq!(batch.len(), total);
            black_box(batch);
        }
        println!(
            "UBENCH merge_disjoint x{MARKETS} (reserve={reserve}): median {:.1} ns/entry, min {:.1} (building the caches: {:.1} ns/set)",
            median(ns.clone()),
            ns.iter().cloned().fold(f64::MAX, f64::min),
            median(build)
        );
    }
}
