//! s80 adversarial worst-block microbench (candidate `adversarial-worst-block`,
//! merged ranked items 5 / 10 / 16). Public `OrderBook` API only. Three arms:
//!
//! - **A, FOK pre-check.** A book with `D` resting asks from 50 makers, the
//!   batch path's state installed (`AccountMargins` with `S` checked senders,
//!   a mark, pre-batch snapshots; `ReduceOnlyPositions` with the `S` senders,
//!   the 50 makers and `K` extra reduce-only holders). One "action" = 1,024
//!   FOK buys from one attacker, every one rejected (`FokCancel`):
//!   `over` = qty above the depth (the walk visits all `D` orders);
//!   `margin` = qty within the depth but the taker's margin does not fit the
//!   complete fill (the walk also visits ~`D` orders); `nocross` = limit
//!   below the best ask (the walk visits nothing: the clones are the cost);
//!   `thin` = one lot at the best ask, margin failing (the walk visits one
//!   order: the clones are the cost, and the quantity bound passes). `makers=Some` is the
//!   batch path (the pre-check clones both maps, then walks with the maker
//!   checks); `makers=None` is the lower bound (no clone, no maker checks).
//!   `clone` times the two map clones alone.
//! - **B, reduce-only sweep.** A trader with a long position keeps `R`
//!   reduce-only asks (one level or one per level); one action = 1,024
//!   reduce-only IOC sells from it, no fill (`nofill`) or a 1-lot fill each
//!   (`fill`). Every placement sweeps the trader's `R` kept orders.
//! - **C, stop miss.** 300 books x `N` parked stops; one miss = `pending_stop`
//!   of an absent id in every book (the by-id cancel probe), the current
//!   linear scan vs a binary search on the id-ascending slice.
//!
//! Prints ns/op and ms/action (median of `ADV_RUNS`, default 5). Env:
//! `ADV_ARMS` (default `ABC`), `ADV_D` (`2000,20000`), `ADV_S` (`80,209,2000`),
//! `ADV_K` (`0,10000`), `ADV_R` (`0,100,1000`), `ADV_N` (`1000,3000`),
//! `ADV_FOKS` (1,024 per action).
//!
//!   cargo test --release -p torus-core --test adversarial_match_ubench -- --ignored --nocapture

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use torus_core::margin::{default_margin_tiers, MarginTier};
use torus_core::order_book::{
    AccountMargins, MakerAccount, MakerAccountSource, OrderBook, ReduceOnlyPositions, TakerMarginLimit,
};
use torus_state::action_status::FailureReason;
use torus_types::{Address, FixedPoint, MarketId, OrderType, PlaceOrderParams, TimeInForce};

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const MARKET: MarketId = 1;
const SCALE: i128 = FixedPoint::SCALE;
/// tick 0.01, lot 0.001.
const TICK: i128 = SCALE / 100;
const LOT: i128 = SCALE / 1000;
const MID: i128 = 2_000 * SCALE;

fn fp(raw: i128) -> FixedPoint {
    FixedPoint::from_raw(raw)
}

fn addr(tag: u8, i: u64) -> Address {
    let mut a = [0u8; 20];
    a[0] = tag;
    a[4..12].copy_from_slice(&i.to_be_bytes());
    a[12..20].copy_from_slice(&(i.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_be_bytes());
    Address::new(a)
}

fn env_list(name: &str, default: &[usize]) -> Vec<usize> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v.split(',').map(|x| x.trim().parse().expect(name)).collect(),
        _ => default.to_vec(),
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn arm_on(c: char) -> bool {
    std::env::var("ADV_ARMS").map_or(true, |v| v.contains(c))
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

fn limit(is_buy: bool, price: i128, qty: i128, tif: TimeInForce, reduce_only: bool) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: MARKET,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only,
        client_order_id: None,
    }
}

/// Every maker is well funded and flat (the honest makers of the attack).
struct RichMakers;
impl MakerAccountSource for RichMakers {
    fn maker_account(&self, _maker: &Address, _market_id: MarketId) -> MakerAccount {
        MakerAccount { free: fp(1_000_000_000 * SCALE), signed_pos: FixedPoint::ZERO, px: FixedPoint::ZERO }
    }
}

fn tiers() -> Arc<[MarginTier]> {
    Arc::from(default_margin_tiers().as_slice())
}

/// Arm A: `d` one-lot asks over `d / 20` levels (20 per level) from 50
/// makers, round-robin, above `MID`.
fn fok_book(d: usize) -> (OrderBook, i128) {
    let mut b = OrderBook::new(MARKET, fp(TICK), fp(LOT));
    let per_level = 20;
    let mut top = MID;
    for i in 0..d {
        let px = MID + TICK * (1 + (i / per_level) as i128);
        top = top.max(px);
        let r = b.place_order(limit(false, px, LOT, TimeInForce::GTC, false), addr(1, (i % 50) as u64), i as u64);
        assert!(r.reject.is_none());
    }
    (b, top)
}

/// The batch path's per-market state: `s` checked senders (the attacker is
/// sender 0), the mark, pre-batch snapshots; the reduce-only map holds the
/// senders, the 50 makers (batch senders too) and `k` reduce-only holders.
fn install_maps(b: &mut OrderBook, s: usize, k: usize, attacker_free: i128) -> (AccountMargins, ReduceOnlyPositions) {
    let mut am = AccountMargins::new(Some(tiers()));
    am.set_mark(Some(fp(MID)));
    am.set_pre_batch_snapshots(true);
    let mut ro = ReduceOnlyPositions::new();
    for i in 0..s {
        let free = if i == 0 { attacker_free } else { 1_000_000 * SCALE };
        am.insert(addr(2, i as u64), fp(free), fp(MID));
        ro.insert(addr(2, i as u64), FixedPoint::ZERO);
    }
    for m in 0..50u64 {
        ro.insert(addr(1, m), FixedPoint::ZERO);
    }
    for j in 0..k {
        ro.insert(addr(3, j as u64), fp(LOT * 10));
    }
    b.set_account_margins(am.clone());
    b.set_reduce_only_positions(ro.clone());
    (am, ro)
}

#[test]
#[ignore]
fn adversarial_fok_precheck() {
    if !arm_on('A') {
        return;
    }
    let runs = env_usize("ADV_RUNS", 5);
    let foks = env_usize("ADV_FOKS", 1024);
    let flag = std::env::var("TORUS_S80_FOK_BOUND").unwrap_or_default();
    println!("ARM A FOK pre-check: runs={runs} foks/action={foks} TORUS_S80_FOK_BOUND={flag:?}");
    println!("ARM A\tD\tS\tK\tcase\tmakers\tns/fok\tms/action");
    let src = RichMakers;
    for &d in &env_list("ADV_D", &[2000, 20000]) {
        let top = fok_book(d).1;
        for &s in &env_list("ADV_S", &[80, 209, 2000]) {
            for &k in &env_list("ADV_K", &[0, 10000]) {
                // Clone-only cost (the two maps of the pre-check).
                {
                    let mut b = fok_book(d).0;
                    let (am, ro) = install_maps(&mut b, s, k, 1_000_000 * SCALE);
                    let mut xs = Vec::new();
                    for _ in 0..runs {
                        let t = Instant::now();
                        for _ in 0..foks {
                            black_box(am.clone());
                            black_box(ro.clone());
                        }
                        xs.push(t.elapsed().as_nanos() as f64);
                    }
                    let m = median(xs);
                    println!("ARM A\t{d}\t{s}\t{k}\tclone\t-\t{:.0}\t{:.3}", m / foks as f64, m / 1e6);
                }
                for case in ["nocross", "thin", "over", "margin"] {
                    for with_makers in [true, false] {
                        let mut b = fok_book(d).0;
                        // `margin`: the attacker has no free margin and a
                        // 1-raw budget, so the complete fill's need fails.
                        // `nocross`: limit below the best ask (the walk
                        // visits nothing; the clones are the cost).
                        let (free, budget, qty, px) = match case {
                            "nocross" => (1_000_000 * SCALE, 1_000_000 * SCALE, LOT * 10, MID),
                            "thin" => (0, 1, LOT, MID + TICK),
                            "over" => (1_000_000 * SCALE, 1_000_000 * SCALE, LOT * (d as i128 + 1), top),
                            _ => (0, 1, LOT * (d as i128 - 1), top),
                        };
                        let (am, ro) = install_maps(&mut b, s, k, free);
                        let lim = TakerMarginLimit { budget: fp(budget), tiers: Some(tiers()), hold_price: None };
                        let attacker = addr(2, 0);
                        let mk: Option<&dyn MakerAccountSource> = if with_makers { Some(&src) } else { None };
                        let before = b.order_count();
                        let mut xs = Vec::new();
                        for _ in 0..runs {
                            // Each action starts from the installed batch state.
                            b.set_account_margins(am.clone());
                            b.set_reduce_only_positions(ro.clone());
                            let t = Instant::now();
                            for i in 0..foks {
                                let r = b.place_order_with_accounts(
                                    limit(true, px, qty, TimeInForce::FOK, false),
                                    attacker,
                                    i as u64,
                                    Some(&lim),
                                    mk,
                                );
                                debug_assert_eq!(r.reject, Some(FailureReason::FokCancel));
                                black_box(r);
                            }
                            xs.push(t.elapsed().as_nanos() as f64);
                        }
                        // Every FOK was rejected and the book is unchanged.
                        let r = b.place_order_with_accounts(
                            limit(true, px, qty, TimeInForce::FOK, false),
                            attacker,
                            0,
                            Some(&lim),
                            mk,
                        );
                        assert_eq!(r.reject, Some(FailureReason::FokCancel), "case {case} d {d}");
                        assert_eq!(b.order_count(), before);
                        let m = median(xs);
                        let tag = if with_makers { "Some" } else { "None" };
                        println!("ARM A\t{d}\t{s}\t{k}\t{case}\t{tag}\t{:.0}\t{:.3}", m / foks as f64, m / 1e6);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore]
fn adversarial_ro_sweep() {
    if !arm_on('B') {
        return;
    }
    let runs = env_usize("ADV_RUNS", 5);
    let n = env_usize("ADV_FOKS", 1024);
    println!("ARM B RO sweep: runs={runs} orders/action={n}");
    println!("ARM B\tR\tlayout\tcase\tns/order\tms/action");
    let a = addr(4, 0);
    let maker = addr(4, 1);
    for &r in &env_list("ADV_R", &[0, 100, 1000]) {
        for layout in ["one", "spread"] {
            if r == 0 && layout == "spread" {
                continue;
            }
            for case in ["nofill", "fill"] {
                let mut xs = Vec::new();
                for _ in 0..runs {
                    let mut b = OrderBook::new(MARKET, fp(TICK), fp(LOT));
                    // Long R + 2n lots: no sweep ever cuts.
                    let pos = fp(LOT * (r as i128 + 2 * n as i128 + 16));
                    let mut ro = ReduceOnlyPositions::new();
                    ro.insert(a, pos);
                    b.set_reduce_only_positions(ro);
                    for i in 0..r {
                        let px = match layout {
                            "one" => MID + 100 * SCALE,
                            _ => MID + 100 * SCALE + TICK * i as i128,
                        };
                        let p = b.place_order(limit(false, px, LOT, TimeInForce::GTC, true), a, i as u64);
                        assert!(p.reject.is_none());
                    }
                    // Bids: n one-lot bids at MID - 10 from another trader.
                    for i in 0..n {
                        let p = b.place_order(limit(true, MID - 10 * SCALE, LOT, TimeInForce::GTC, false), maker, i as u64);
                        assert!(p.reject.is_none());
                    }
                    let px = match case {
                        "nofill" => MID - 5 * SCALE,
                        _ => MID - 10 * SCALE,
                    };
                    let t = Instant::now();
                    for i in 0..n {
                        let p = b.place_order(limit(false, px, LOT, TimeInForce::IOC, true), a, (r + i) as u64);
                        black_box(&p);
                        debug_assert!(p.reduce_only_cuts.is_empty());
                    }
                    xs.push(t.elapsed().as_nanos() as f64);
                    assert_eq!(b.order_count(), r + if case == "fill" { 0 } else { n });
                }
                let m = median(xs);
                println!("ARM B\t{r}\t{layout}\t{case}\t{:.0}\t{:.3}", m / n as f64, m / 1e6);
            }
        }
    }
}

#[test]
#[ignore]
fn adversarial_stop_miss() {
    if !arm_on('C') {
        return;
    }
    let runs = env_usize("ADV_RUNS", 5);
    let misses = env_usize("ADV_MISSES", 200);
    println!("ARM C stop miss: 300 books, runs={runs} misses/run={misses}");
    println!("ARM C\tstops/book\ttotal\tsearch\tus/miss");
    for &n in &env_list("ADV_N", &[1000, 3000]) {
        let books: Vec<OrderBook> = (0..300u64)
            .map(|m| {
                let mut b = OrderBook::new(m + 1, fp(TICK), fp(LOT));
                for i in 0..n {
                    let p = PlaceOrderParams {
                        market_id: m + 1,
                        is_buy: true,
                        price: fp(MID + 200 * SCALE),
                        quantity: fp(LOT),
                        order_type: OrderType::StopMarket { trigger: fp(MID + 100 * SCALE + TICK * (i as i128 % 500)) },
                        time_in_force: TimeInForce::GTC,
                        reduce_only: false,
                        client_order_id: None,
                    };
                    let r = b.place_order(p, addr(5, i as u64 % 300), i as u64);
                    assert!(r.reject.is_none(), "{:?}", r.reject);
                }
                assert_eq!(b.pending_stop_count(), n);
                assert!(b.pending_stops().windows(2).all(|w| w[0].id < w[1].id), "id-ascending");
                b
            })
            .collect();
        for search in ["linear", "binary"] {
            let mut xs = Vec::new();
            for run in 0..runs {
                let t = Instant::now();
                for j in 0..misses {
                    // An id no book holds (above every allocated id).
                    let id = u128::MAX - (run * misses + j) as u128;
                    for b in &books {
                        let hit = match search {
                            "linear" => b.pending_stop(id).is_some(),
                            _ => b.pending_stops().binary_search_by_key(&id, |s| s.id).is_ok(),
                        };
                        assert!(!black_box(hit));
                    }
                }
                xs.push(t.elapsed().as_nanos() as f64);
            }
            let m = median(xs);
            println!("ARM C\t{n}\t{}\t{search}\t{:.2}", 300 * n, m / misses as f64 / 1e3);
        }
    }
}
