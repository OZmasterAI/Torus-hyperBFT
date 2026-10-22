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
            black_box(
                pm.apply_fill_cached_effect(c, &b, m, true, q, p, MarginType::Cross)
                    .unwrap(),
            );
            black_box(
                pm.apply_fill_cached_effect(c, &s, m, false, q, p, MarginType::Cross)
                    .unwrap(),
            );
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
                    funding: None,
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

// ============================================================================
// 8k L2.2 (`TORUS_SETTLE_NO_MERGE`): pass B's merge + flush_all vs
// flush_with_runs, both writing into a real NativeStateOverlay.
//
//   cargo test -p torus-core --release --test ubench_position_cache \
//     ubench_position_cache_flush_runs -- --ignored --nocapture
// ============================================================================

/// s94 shape: 16.8k fills per native block over 300 markets (56 per market),
/// two sides per fill drawn from 5,000 senders; q8re (s90.4): 22.8k (76).
const SENDERS: u64 = 5_000;

fn sender(i: u64) -> Address {
    // Spread like real addresses (the key's first bytes are the trader).
    let mut r = Rng(0xA076_1D64_78BD_642F ^ (i.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let mut a = [0u8; 20];
    for chunk in a.chunks_mut(8) {
        let v = r.next().to_be_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    Address::new(a)
}

/// Pass B's inputs for one block: 300 per-market plan caches. Each fill side
/// leaves its (trader, market) row dirty: ~85% live rows, ~15% tombstones
/// (full closes); each market also memoizes ~10% clean reads (misses that
/// are never written). Returns the caches and the dirty count.
fn block_runs<T: torus_state::StateBackend>(
    pm: &PositionManager<T>,
    fills_per_market: u64,
    seed: u64,
) -> (Vec<PositionCache>, usize) {
    let mut r = Rng(seed);
    let mut dirty = 0;
    let caches = (1..=MARKETS)
        .map(|m| {
            let mut c = PositionCache::new();
            for _ in 0..2 * fills_per_market {
                let t = sender(r.next() % SENDERS);
                let i = r.next();
                if i % 100 < 15 {
                    c.remove(&t, m);
                } else {
                    c.set(Position {
                        trader: t,
                        market_id: m,
                        is_long: i.is_multiple_of(2),
                        size: FixedPoint::from_raw((i % 5_000) as i128 * 100_000 + 1),
                        entry_price: FixedPoint::from_raw(2_000 * FixedPoint::SCALE),
                        cost_basis: FixedPoint::from_raw(2_000 * ((i % 5_000) as i128 + 1)),
                        realized_pnl: FixedPoint::from_raw((i % 777) as i128),
                        isolated_margin: FixedPoint::ZERO,
                        margin_type: MarginType::Cross,
                        funding: None,
                    });
                }
            }
            dirty += c.len();
            for _ in 0..(2 * fills_per_market) / 10 {
                c.load(pm, &sender(SENDERS + r.next() % SENDERS), m).unwrap();
            }
            c
        })
        .collect();
    (caches, dirty)
}

#[test]
#[ignore]
fn ubench_position_cache_flush_runs() {
    use torus_state::NativeStateOverlay;
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let rounds: usize = std::env::var("UBENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    for fills_per_market in [56u64, 76] {
        let fills = MARKETS * fills_per_market;
        // ms per block: [merge part of A, A total, B total]
        let (mut merge_ms, mut a_ms, mut b_ms) = (Vec::new(), Vec::new(), Vec::new());
        let mut dirty_n = 0;
        let mut entries_n = 0;
        let mut writes = [0usize; 2];
        for round in 0..rounds {
            // A B B A: alternate which arm goes first.
            let order: [bool; 2] = if round % 2 == 0 { [false, true] } else { [true, false] };
            for runs_arm in order {
                let seed = 0xC0FF_EE00 + round as u64;
                let overlay = NativeStateOverlay::new(db.clone());
                let pm = PositionManager::new(overlay.clone());
                let (caches, dirty) = block_runs(&pm, fills_per_market, seed);
                dirty_n = dirty;
                entries_n = caches.iter().map(PositionCache::len).sum();
                if !runs_arm {
                    // Today: reserve, 300 merges, flush_all, drop.
                    let t = Instant::now();
                    let mut batch = PositionCache::new();
                    batch.reserve(caches.iter().map(PositionCache::len).sum());
                    for c in caches {
                        batch.merge_disjoint(c);
                    }
                    let merged = t.elapsed();
                    batch.flush_all(&pm).unwrap();
                    drop(black_box(batch));
                    let total = t.elapsed();
                    merge_ms.push(merged.as_secs_f64() * 1e3);
                    a_ms.push(total.as_secs_f64() * 1e3);
                } else {
                    // 8k L2.2: 300 pushes onto the runs, flush_with_runs, drop.
                    let t = Instant::now();
                    let mut batch = PositionCache::new();
                    let mut runs: Vec<PositionCache> = Vec::with_capacity(caches.len());
                    for c in caches {
                        runs.push(c);
                    }
                    batch.flush_with_runs(&mut runs, &pm).unwrap();
                    drop(black_box(runs));
                    drop(black_box(batch));
                    b_ms.push(t.elapsed().as_secs_f64() * 1e3);
                }
                writes[usize::from(runs_arm)] = overlay.pending_write_count();
                drop(pm);
                drop(overlay);
            }
        }
        assert_eq!(writes[0], writes[1], "both arms wrote the same rows");
        let (a, b, m) = (median(a_ms.clone()), median(b_ms.clone()), median(merge_ms));
        let per_k = |ms: f64| ms / (fills as f64 / 1_000.0);
        println!(
            "UBENCH flush_runs {fills} fills/block ({fills_per_market}/market x {MARKETS}), {dirty_n} dirty rows, {entries_n} cache entries, {rounds} rounds/arm: \
             A merge+flush_all median {a:.3} ms (merge {m:.3} ms, min {:.3}), B flush_with_runs median {b:.3} ms (min {:.3}); \
             saving {:.3} ms/block = {:.4} ms per 1k fills ({:+.1}% of A)",
            a_ms.iter().cloned().fold(f64::MAX, f64::min),
            b_ms.iter().cloned().fold(f64::MAX, f64::min),
            a - b,
            per_k(a - b),
            (b - a) / a * 100.0,
        );
    }
}

// ============================================================================
// 8k L2.2 Codex P1 fix: the cost of the disjointness proof
// ============================================================================

/// `PositionCache::runs_disjoint`, the proof `flush_with_runs` runs before
/// its unmerged fast path, on pass B's 300 per-market caches at ~16.8k and
/// ~26k cache entries per block (the 8n deep-book sizes). It compares the
/// caches' key-market ranges (kept on insert, two compares per new key), so
/// it walks the 300 runs, never the keys: the cost must not grow with the
/// entry count.
///
///   cargo test -p torus-core --release --test ubench_position_cache ubench_position_cache_runs_disjoint -- --ignored --nocapture
#[test]
#[ignore]
fn ubench_position_cache_runs_disjoint() {
    let dir = tempfile::tempdir().unwrap();
    let pm = PositionManager::new(StateDb::open(dir.path()).unwrap());
    for target in [16_800usize, 26_000] {
        // The fewest fills per market whose caches reach `target` entries.
        let mut fills_per_market = 20;
        let (runs, entries) = loop {
            let (runs, _) = block_runs(&pm, fills_per_market, 0x0D15_7017);
            let n: usize = runs.iter().map(PositionCache::len).sum();
            if n >= target {
                break (runs, n);
            }
            fills_per_market += 1;
        };
        let batch = PositionCache::new();
        assert!(batch.runs_disjoint(&runs), "per-market caches are disjoint");
        let iters = 20_000;
        let mut ns = Vec::new();
        for _ in 0..ROUNDS * 2 {
            let t = Instant::now();
            for _ in 0..iters {
                black_box(black_box(&batch).runs_disjoint(black_box(&runs)));
            }
            ns.push(t.elapsed().as_nanos() as f64 / iters as f64);
        }
        println!(
            "UBENCH runs_disjoint {entries} cache entries ({fills_per_market} fills/market x {MARKETS} runs): median {:.1} ns/block, min {:.1}",
            median(ns.clone()),
            ns.iter().cloned().fold(f64::MAX, f64::min)
        );
    }
}
