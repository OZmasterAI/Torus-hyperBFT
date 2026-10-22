//! s80 ro-basis-walk-memo (`TORUS_S80_RO_BASIS_MEMO=1`): with the flag on,
//! [`NativeExecutor::phase2_reservation_basis`] walks a sender's resting
//! orders in a market once per (sender, market) per call (memo) instead of
//! once per reduce-only non-stop order. Phase 2 touches no book, so the walk
//! is a pure function of (sender, market) within the call and the memo must
//! leave exactly the walk's output map.
//!
//! - [`memo_matches_walk_on_random_books`]: differential, random books
//!   (reduce-only and not, partial fills, saturating sizes), positions and
//!   order mixes: memo map == walk map, and the memo walks once per pair.
//! - [`memo_walks_once_per_pair`]: the walk counter, base vs memo.
//! - [`ro_basis_walk_timing`] (`#[ignore]`, release): the attack shape, one
//!   sender resting R in {100, 1,000, 5,000} non-reduce-only orders (one level
//!   and 50 levels) plus one call with 1,024 reduce-only orders of it; median
//!   of 11 runs per arm.

use super::*;
use torus_core::position::{MarginType, Position};

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// A whole-lot size near `i128::MAX / 3` (sums of two or more saturate).
fn huge() -> FixedPoint {
    FixedPoint::from_raw(i128::MAX / 3 / FixedPoint::SCALE * FixedPoint::SCALE)
}

fn new_ctx() -> (tempfile::TempDir, NativeExecContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=64u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .unwrap();
    }
    let ctx = NativeExecContext::new(
        db,
        9,
        10_000,
        0,
        1_000,
        10,
        Address::ZERO,
        Address::ZERO,
        Address::ZERO,
    );
    (dir, ctx)
}

fn put_pos(ctx: &NativeExecContext, trader: Address, market_id: MarketId, signed: i64) {
    if signed == 0 {
        return;
    }
    let size = fp(signed.abs());
    ctx.positions
        .put_position(&Position {
            trader,
            market_id,
            is_long: signed > 0,
            size,
            entry_price: fp(80),
            cost_basis: fp(80) * size,
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();
}

fn limit(market_id: MarketId, is_buy: bool, price: i64, quantity: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

type Basis = HashMap<usize, (FixedPoint, FixedPoint)>;

/// `(map, walks)` of one arm on this thread.
fn basis_arm(
    ctx: &NativeExecContext,
    orders: &[(usize, Address, &PlaceOrderParams)],
    memo: bool,
) -> (Basis, u64) {
    let w0 = ro_basis_walks_on_this_thread();
    let out = NativeExecutor::phase2_reservation_basis_with(ctx, orders, memo);
    (out, ro_basis_walks_on_this_thread() - w0)
}

/// Random books (resting reduce-only and not, partial fills, now and then a
/// saturating size), positions and Phase-2 order mixes: the memo leaves the
/// walk's map exactly, walks once per (sender, market) of a reduce-only
/// non-stop order, and the flag-off entry point is the walk.
#[test]
fn memo_matches_walk_on_random_books() {
    let mut rng = Lcg(0x80_B0A5_15);
    let (mut nonempty, mut saved) = (0usize, 0u64);
    for round in 0..300u64 {
        let (_dir, mut ctx) = new_ctx();
        let n_senders = 1 + rng.below(6);
        let n_markets = 1 + rng.below(4);
        let senders: Vec<Address> =
            (0..n_senders).map(|i| Address::new([10 + i as u8; 20])).collect();
        let maker = Address::new([200; 20]);
        let taker = Address::new([201; 20]);
        for m in 1..=n_markets {
            for s in &senders {
                if rng.one_in(2) {
                    put_pos(&ctx, *s, m, rng.below(61) as i64 - 30);
                }
            }
            if rng.one_in(4) {
                continue; // no book in this market
            }
            let mut b = OrderBook::new(m, FixedPoint::ONE, FixedPoint::ONE);
            let mut ts = 1u64;
            for _ in 0..rng.below(80) {
                let trader = if rng.one_in(4) { maker } else { senders[rng.below(n_senders) as usize] };
                let is_buy = rng.one_in(2);
                let price = if is_buy { 50 + rng.below(30) } else { 81 + rng.below(30) } as i64;
                let qty = if rng.one_in(40) { huge() } else { fp(1 + rng.below(20) as i64) };
                let mut p = limit(m, is_buy, price, qty);
                p.reduce_only = rng.one_in(3);
                b.place_order(p, trader, ts);
                ts += 1;
            }
            // Partial fills: a crossing IOC from a third trader leaves some
            // resting orders with remaining < quantity.
            if rng.one_in(3) {
                let mut p = limit(m, rng.one_in(2), 0, fp(1 + rng.below(40) as i64));
                p.price = if p.is_buy { fp(90 + rng.below(20) as i64) } else { fp(60 + rng.below(20) as i64) };
                p.time_in_force = TimeInForce::IOC;
                b.place_order(p, taker, ts);
            }
            ctx.insert_order_book(m, b);
        }
        let orders: Vec<(Address, PlaceOrderParams)> = (0..1 + rng.below(80))
            .map(|_| {
                let s = senders[rng.below(n_senders) as usize];
                let q = if rng.one_in(30) { huge() } else { fp(1 + rng.below(60) as i64) };
                let mut p = limit(1 + rng.below(n_markets + 1), rng.one_in(2), 50 + rng.below(60) as i64, q);
                match rng.below(10) {
                    0 => p.order_type = OrderType::Market,
                    1 => p.time_in_force = TimeInForce::IOC,
                    2 => p.time_in_force = TimeInForce::FOK,
                    3 => p.time_in_force = TimeInForce::PostOnly,
                    4 => p.order_type = OrderType::StopLimit { trigger: p.price, limit: p.price },
                    _ => {}
                }
                p.reduce_only = rng.below(5) < 2;
                (s, p)
            })
            .collect();
        let place_orders: Vec<(usize, Address, &PlaceOrderParams)> =
            orders.iter().enumerate().map(|(i, (s, p))| (i, *s, p)).collect();
        let pairs: HashSet<(Address, MarketId)> = orders
            .iter()
            .filter(|(_, p)| p.reduce_only && !NativeExecutor::is_stop(p))
            .map(|(s, p)| (*s, p.market_id))
            .collect();
        let ro_orders = orders
            .iter()
            .filter(|(_, p)| p.reduce_only && !NativeExecutor::is_stop(p))
            .count() as u64;

        let (walk, walks_base) = basis_arm(&ctx, &place_orders, false);
        let (memo, walks_memo) = basis_arm(&ctx, &place_orders, true);
        assert_eq!(memo, walk, "round {round}: memo map != walk map");
        assert_eq!(walks_base, ro_orders, "round {round}: base walks once per RO order");
        assert_eq!(walks_memo, pairs.len() as u64, "round {round}: memo walks once per pair");
        // The flag-off entry point (env unset in tests, pinned off here) is the walk.
        let off = with_ro_basis_memo(false, || NativeExecutor::phase2_reservation_basis(&ctx, &place_orders));
        assert_eq!(off, walk, "round {round}: flag-off entry point");
        let on = with_ro_basis_memo(true, || NativeExecutor::phase2_reservation_basis(&ctx, &place_orders));
        assert_eq!(on, walk, "round {round}: flag-on entry point");
        nonempty += usize::from(!walk.is_empty());
        saved += walks_base - walks_memo;
    }
    // The generator exercises the reduce-only clamp and repeated pairs.
    assert!(nonempty > 150, "only {nonempty} rounds had overrides");
    assert!(saved > 1_000, "memo saved only {saved} walks");
}

/// Base walks once per reduce-only non-stop order whose position read
/// succeeded; memo once per (sender, market). Stops and non-reduce-only
/// orders never walk, a missing book is a walk of nothing.
#[test]
fn memo_walks_once_per_pair() {
    let (_dir, mut ctx) = new_ctx();
    let s = Address::new([1; 20]);
    let t = Address::new([2; 20]);
    let mut b = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
    for k in 0..10 {
        b.place_order(limit(1, true, 40 + k, fp(1)), s, 1 + k as u64);
    }
    let mut ro_rest = limit(1, true, 30, fp(7));
    ro_rest.reduce_only = true;
    b.place_order(ro_rest, s, 20); // a resting reduce-only order is not counted
    ctx.insert_order_book(1, b);
    put_pos(&ctx, t, 1, -3);

    let mut orders = Vec::new();
    for _ in 0..50 {
        let mut p = limit(1, false, 100, fp(25));
        p.reduce_only = true;
        orders.push((s, p));
    }
    for _ in 0..20 {
        let mut p = limit(2, false, 100, fp(4)); // no book in market 2
        p.reduce_only = true;
        orders.push((s, p));
    }
    for _ in 0..30 {
        let mut p = limit(1, true, 60, fp(9));
        p.reduce_only = true;
        orders.push((t, p));
    }
    let mut stop = limit(1, false, 100, fp(5));
    stop.order_type = OrderType::StopLimit { trigger: fp(90), limit: fp(90) };
    stop.reduce_only = true;
    orders.push((s, stop));
    orders.push((s, limit(1, true, 41, fp(3)))); // grows the next orders' bound
    let mut last = limit(1, false, 100, fp(25));
    last.reduce_only = true;
    orders.push((s, last));
    let place_orders: Vec<(usize, Address, &PlaceOrderParams)> =
        orders.iter().enumerate().map(|(i, (s, p))| (i, *s, p)).collect();

    let (walk, walks_base) = basis_arm(&ctx, &place_orders, false);
    let (memo, walks_memo) = basis_arm(&ctx, &place_orders, true);
    assert_eq!(memo, walk);
    assert_eq!((walks_base, walks_memo), (101, 3));
    // S's sells: bound = resting non-RO 10 (the RO 7 is not counted) ...
    assert_eq!(walk[&0], (fp(100), fp(10)));
    // ... market 2 has no book: bound 0 ...
    assert_eq!(walk[&50], (fp(100), FixedPoint::ZERO));
    // ... T (short 3) buys 9: allowance 3, nothing resting.
    assert_eq!(walk[&70], (fp(60), fp(3)));
    // ... the stop keeps its full size, and the last sell gains the earlier
    // non-reduce-only buy of 3 in the batch.
    assert!(!walk.contains_key(&100));
    assert_eq!(walk[&102], (fp(100), fp(13)));
}

/// Median of `v` (sorted in place).
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The attack shape (r2 ro-basis-walk-memo): sender S rests R dust
/// non-reduce-only bids in one market (one level, or 50 levels), then one
/// Phase-2 call holds 1,024 reduce-only sells of S in that market (no
/// position needed: the RPC applies only the delisted rule to RO orders).
/// Median of 11 runs per arm (arms alternate run by run); the maps must be
/// identical. Run (release):
///   cargo test -p torus-bridge --release --lib ro_basis_walk_timing -- --ignored --nocapture
#[test]
#[ignore]
fn ro_basis_walk_timing() {
    let s = Address::new([7; 20]);
    let other = Address::new([8; 20]);
    println!("R\tlevels\tbase_ms\tmemo_ms\tbase_us_per_ro\tspeedup\twalks_base\twalks_memo");
    for r in [100usize, 1_000, 5_000] {
        for levels in [1usize, 50] {
            let (_dir, mut ctx) = new_ctx();
            let mut b = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
            for k in 0..r {
                let price = 10 + (k % levels) as i64;
                b.place_order(limit(1, true, price, fp(1)), s, 1 + k as u64);
                // Another trader's order in the same queues (deeper levels).
                if k % 4 == 0 {
                    b.place_order(limit(1, true, price, fp(1)), other, 1 + k as u64);
                }
            }
            assert_eq!(b.orders_for_trader(&s).len(), r);
            ctx.insert_order_book(1, b);
            let orders: Vec<(Address, PlaceOrderParams)> = (0..1_024)
                .map(|k| {
                    let mut p = limit(1, false, 100 + (k % 7) as i64, fp(2 * r as i64));
                    p.reduce_only = true;
                    (s, p)
                })
                .collect();
            let place_orders: Vec<(usize, Address, &PlaceOrderParams)> =
                orders.iter().enumerate().map(|(i, (s, p))| (i, *s, p)).collect();
            let (want, walks_base) = basis_arm(&ctx, &place_orders, false);
            let (got, walks_memo) = basis_arm(&ctx, &place_orders, true);
            assert_eq!(got, want, "R={r} levels={levels}");
            assert_eq!(want.len(), 1_024);
            assert!(want.values().all(|&(_, q)| q == fp(r as i64)));
            let (mut base, mut memo) = (Vec::new(), Vec::new());
            for _ in 0..11 {
                let t = std::time::Instant::now();
                let a = NativeExecutor::phase2_reservation_basis_with(&ctx, &place_orders, false);
                base.push(t.elapsed().as_secs_f64() * 1e3);
                let t = std::time::Instant::now();
                let c = NativeExecutor::phase2_reservation_basis_with(&ctx, &place_orders, true);
                memo.push(t.elapsed().as_secs_f64() * 1e3);
                assert_eq!(a, c);
            }
            let (bm, mm) = (median(&mut base), median(&mut memo));
            println!(
                "{r}\t{levels}\t{bm:.3}\t{mm:.3}\t{:.2}\t{:.0}x\t{walks_base}\t{walks_memo}",
                bm * 1e3 / 1_024.0,
                bm / mm
            );
            println!("  raw base_ms {base:.3?}");
            println!("  raw memo_ms {memo:.3?}");
        }
    }
}
