//! Item 6 Phase 4 step 5 (plan decision 2 b): the stored per-user open-order
//! count. The executor keeps each user's open orders (resting plus pending
//! stops, all markets) as a counter fed by the books' open-order logs
//! ([`OrderBook::drain_open_log`]) instead of walking every book for every
//! sender per `execute_batch` call. The counter must always equal the books'
//! count, and every result and the state root must equal the run that counts
//! from the books (`test_open_count_scan`, the reference path), on every
//! executor path (batch, cancel-batch mode, match workers, two calls per
//! block, the single path), every book mode, with resident books, without,
//! and across a restart (books and counts rebuilt from the DB).

use super::*;
use crate::state_root::compute_native_state_root;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use torus_core::position::{NativeBalance, OPEN_ORDER_BASE_LIMIT};
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

const C: u128 = 0x0be2_0000_0000_0000_0000_0000_0000_0001;
const MARKETS: u64 = 4;
const TRADERS: u8 = 5;
const BLOCKS: usize = 12;
const ACTIONS: usize = 40;

const MODES: [BookMode; 4] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];

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

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn params(
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
    ot: OrderType,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: ot,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    params(market_id, is_buy, price, qty, OrderType::Limit, TimeInForce::GTC)
}

fn ctx_at(
    db: &StateDb,
    height: u64,
    mode: BookMode,
    holder: Option<&mut ResidentBooks>,
) -> NativeExecContext {
    let ctx = NativeExecContext::new_with_mode(
        db.clone(),
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        holder,
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    ctx
}

fn fund(ctx: &NativeExecContext, who: Address, amount: i64) {
    let bal = NativeBalance {
        available: fp(amount),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(&who, &bal).unwrap();
}

fn random_place(rng: &mut Rng) -> PlaceOrderParams {
    let m = 1 + rng.below(MARKETS);
    let is_buy = rng.below(2) == 0;
    let mut p = match rng.below(12) {
        // Takers that never rest (IOC, market, FOK).
        0 => params(m, is_buy, if is_buy { 106 } else { 94 }, rng.pick(1, 4), OrderType::Limit, TimeInForce::IOC),
        1 => params(m, is_buy, if is_buy { 110 } else { 90 }, rng.pick(1, 3), OrderType::Market, TimeInForce::IOC),
        2 => params(m, is_buy, if is_buy { 103 } else { 97 }, rng.pick(1, 5), OrderType::Limit, TimeInForce::FOK),
        3 => params(
            m,
            is_buy,
            if is_buy { rng.pick(96, 100) } else { rng.pick(100, 104) },
            rng.pick(1, 3),
            OrderType::Limit,
            TimeInForce::PostOnly,
        ),
        // Stops near the market: some fire on the block's trades.
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
            params(m, is_buy, if is_buy { trigger + 5 } else { trigger - 5 }, rng.pick(1, 2), ot, TimeInForce::GTC)
        }
        // Resting or crossing limits: partial and full fills, self-trades.
        _ => gtc(
            m,
            is_buy,
            if is_buy { rng.pick(96, 102) } else { rng.pick(98, 104) },
            rng.pick(1, 4),
        ),
    };
    p.reduce_only = rng.below(10) == 0;
    p.client_order_id = (rng.below(4) == 0).then(|| C + rng.below(8) as u128);
    p
}

/// Block actions; order ids for cancel / modify come from the books before
/// the block (sorted: no map order leaks in).
fn random_block(rng: &mut Rng, ctx: &NativeExecContext) -> Vec<(Address, NativeAction)> {
    let mut live: Vec<(MarketId, OrderId, Address)> = Vec::new();
    for (&m, b) in &ctx.order_books {
        for n in 1..=TRADERS {
            for o in b.orders_for_trader(&addr(n)) {
                live.push((m, o.id, o.trader));
            }
        }
        for s in b.pending_stops() {
            live.push((m, s.id, s.trader));
        }
    }
    live.sort();
    let mut block = Vec::with_capacity(ACTIONS);
    for _ in 0..ACTIONS {
        let trader = addr(1 + rng.below(TRADERS as u64) as u8);
        let r = rng.below(100);
        let action = if r < 55 {
            NativeAction::PlaceOrder(random_place(rng))
        } else if r < 62 {
            let n = 2 + rng.below(3) as usize;
            NativeAction::PlaceOrderBatch((0..n).map(|_| random_place(rng)).collect())
        } else if r < 74 && !live.is_empty() {
            let (_, id, owner) = live[rng.below(live.len() as u64) as usize];
            let who = if rng.below(5) == 0 { trader } else { owner };
            block.push((who, NativeAction::CancelOrder { order_id: id }));
            continue;
        } else if r < 78 {
            NativeAction::CancelByCloid {
                market_id: 1 + rng.below(MARKETS),
                cloid: C + rng.below(8) as u128,
            }
        } else if r < 85 && !live.is_empty() {
            let (_, id, owner) = live[rng.below(live.len() as u64) as usize];
            block.push((
                owner,
                NativeAction::ModifyOrder {
                    order_id: id,
                    new_price: (rng.below(2) == 0).then(|| fp(rng.pick(96, 104))),
                    new_qty: (rng.below(2) == 0).then(|| fp(rng.pick(1, 4))),
                },
            ));
            continue;
        } else if r < 91 {
            let market_id = (rng.below(2) == 0).then(|| 1 + rng.below(MARKETS));
            NativeAction::CancelAllOrders { market_id }
        } else {
            NativeAction::PlaceOrder(gtc(1 + rng.below(MARKETS), rng.below(2) == 0, rng.pick(97, 103), rng.pick(1, 3)))
        };
        block.push((trader, action));
    }
    block
}

/// After a call: the counter (books' logs absorbed) equals a count over the
/// books, for every user. Not on the reference path (it never reads the
/// counter, so it never feeds it).
fn check_counter(ctx: &mut NativeExecContext, what: &str) {
    if ctx.test_open_count_scan {
        return;
    }
    let (counter, books) = ctx.open_count_view();
    assert_eq!(counter, books, "{what}: counter != books");
}

/// Run `block` on the path the stream picks, checking the counter after
/// every call.
fn run_block(
    rng: &mut Rng,
    ctx: &mut NativeExecContext,
    block: &[(Address, NativeAction)],
    out: &mut String,
    what: &str,
) -> Vec<NativeActionResult> {
    let path = rng.below(6);
    let _ = writeln!(out, "path {path}");
    let call = |ctx: &mut NativeExecContext, part: &[(Address, NativeAction)], k: u64| {
        let r = match k {
            0 => NativeExecutor::execute_batch(ctx, part).results,
            1 => NativeExecutor::execute_batch_cancel_mode(ctx, part, true).results,
            2 => NativeExecutor::execute_batch_engine_mode(ctx, part, 4).results,
            3 => NativeExecutor::execute_batch_engine_mode(ctx, part, 1).results,
            _ => part
                .iter()
                .map(|(s, a)| {
                    let r = NativeExecutor::execute(ctx, s, a);
                    check_counter(ctx, &format!("{what}: single path"));
                    r
                })
                .collect(),
        };
        check_counter(ctx, &format!("{what}: path {k}"));
        r
    };
    match path {
        // A block's pre- and post-EVM calls (the second on match workers).
        5 => {
            let half = block.len() / 2;
            let mut r = call(ctx, &block[..half], 0);
            r.extend(call(ctx, &block[half..], 2));
            r
        }
        k => call(ctx, block, k),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Run {
    Resident,
    NonResident,
    /// Resident, the holder dropped before this block (cold rebuild).
    RestartAt(usize),
}

/// The transcript of `seed`'s stream: every result, every user's count
/// over the books after each block, the state root; plus the successes.
fn seeded_run(seed: u64, mode: BookMode, run: Run, scan: bool) -> (String, usize) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let ctx = ctx_at(&db, 1, mode, None);
    for n in 1..=TRADERS {
        // Trader 5 is thin: margin rejects and maker margin cancels.
        fund(&ctx, addr(n), if n == TRADERS { 2_000 } else { 100_000_000 });
    }
    drop(ctx);
    let mut rng = Rng(seed);
    let mut out = String::new();
    let mut oks = 0;
    for h in 0..BLOCKS {
        let what = format!("seed {seed:#x} {mode:?} {run:?} scan {scan} block {h}");
        if run == Run::RestartAt(h) {
            holder = ResidentBooks::default();
        }
        let resident = run != Run::NonResident;
        let mut ctx = ctx_at(&db, h as u64 + 1, mode, resident.then_some(&mut holder));
        ctx.test_open_count_scan = scan;
        check_counter(&mut ctx, &format!("{what}: after load"));
        let block = random_block(&mut rng, &ctx);
        let results = run_block(&mut rng, &mut ctx, &block, &mut out, &what);
        for r in &results {
            let _ = writeln!(out, "{} {:?} {:?}", r.success, r.reason, r.error);
            oks += usize::from(r.success);
        }
        let books: BTreeMap<Address, usize> = (1..=TRADERS)
            .map(addr)
            .map(|t| (t, ctx.order_books.values().map(|b| b.open_order_count(&t)).sum()))
            .collect();
        let _ = writeln!(out, "open {books:?}");
        ctx.save_order_books();
        ctx.stash_resident(&mut holder);
        if resident && !scan {
            let (carried, rebuilt) = holder.open_count_view().expect("resident books");
            assert_eq!(carried.expect("built by the block's counts"), rebuilt, "{what}: carried counter");
        }
    }
    let ctx = ctx_at(&db, 99, mode, None);
    let root = compute_native_state_root(&ctx.state).unwrap();
    let _ = writeln!(out, "root {root}");
    (out, oks)
}

/// The counter equals the books' count after every call and every block,
/// and the run equals the book-scan reference result by result and in its
/// state root: every book mode, resident, non-resident, and a restart.
#[test]
fn open_order_count_in_user_equals_book_scan() {
    for seed in [0x5eed_0be2u64, 0x0be2_0002] {
        for mode in MODES {
            let (want, oks) = seeded_run(seed, mode, Run::Resident, true);
            assert!(oks >= 200, "{mode:?}: the stream does work ({oks} successes)");
            for run in [Run::Resident, Run::NonResident, Run::RestartAt(BLOCKS / 2)] {
                let (got, _) = seeded_run(seed, mode, run, false);
                assert!(got == want, "seed {seed:#x} {mode:?} {run:?}: counter run != book scan");
            }
        }
    }
}

/// Place `n` resting bids for `who` spread over markets 1 and 2 (none cross).
fn rest_bids(ctx: &mut NativeExecContext, who: Address, n: usize) {
    let block: Vec<(Address, NativeAction)> = (0..n)
        .map(|k| {
            let m = 1 + (k % 2) as u64;
            (who, NativeAction::PlaceOrder(gtc(m, true, 10 + (k / 2 % 50) as i64, 1)))
        })
        .collect();
    let r = NativeExecutor::execute_batch(ctx, &block);
    assert!(r.results.iter().all(|r| r.success), "resting bids");
}

/// The cap at its edge: `a` holds limit - 1 open orders; within one block a
/// Phase 1 cancel frees a slot, the batch then takes the two free slots in
/// flat order and rejects the rest (reduce-only and stops at >= the base
/// limit), `b`'s sells fill two of `a`'s bids fully (freed for the NEXT
/// call), the post-EVM call sees them, and the single path sees the block
/// so far. Same results as the book scan, on the serial path and on match
/// workers.
#[test]
fn the_cap_rejects_the_same_orders_as_the_book_scan() {
    let limit = OPEN_ORDER_BASE_LIMIT as usize;
    let run = |scan: bool, threads: usize| -> Vec<(bool, FailureReason)> {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let (a, b) = (addr(1), addr(2));
        let mut ctx = ctx_at(&db, 1, BookMode::Classic, None);
        ctx.test_open_count_scan = scan;
        fund(&ctx, a, 100_000_000);
        fund(&ctx, b, 100_000_000);
        rest_bids(&mut ctx, a, limit - 1);
        let first = ctx.order_books[&1].orders_for_trader(&a)[0].id;
        let mut out = Vec::new();
        let reduce_only = PlaceOrderParams {
            reduce_only: true,
            ..gtc(2, false, 500, 1)
        };
        let stop = params(1, false, 1, 1, OrderType::StopMarket { trigger: fp(5) }, TimeInForce::GTC);
        let place = |p: PlaceOrderParams| (a, NativeAction::PlaceOrder(p));
        let call1 = vec![
            (a, NativeAction::CancelOrder { order_id: first }),
            place(gtc(1, true, 9, 1)),
            place(gtc(2, true, 9, 1)),
            place(reduce_only.clone()),
            place(stop.clone()),
            place(gtc(2, true, 8, 1)),
            // b sells into a's best bids (59 in markets 1 and 2).
            (b, NativeAction::PlaceOrder(params(1, false, 59, 1, OrderType::Limit, TimeInForce::IOC))),
            (b, NativeAction::PlaceOrder(params(2, false, 59, 1, OrderType::Limit, TimeInForce::IOC))),
        ];
        let call2 = vec![place(gtc(1, true, 7, 1)), place(gtc(1, true, 6, 1)), place(gtc(1, true, 5, 1))];
        for call in [&call1, &call2] {
            let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, call, threads);
            out.extend(r.results.iter().map(|r| (r.success, r.reason)));
            check_counter(&mut ctx, "cap");
        }
        // Single path: a is at the limit again; cancel one, place one.
        let last = ctx.order_books[&2].orders_for_trader(&a)[0].id;
        for action in [
            NativeAction::PlaceOrder(gtc(2, true, 4, 1)),
            NativeAction::CancelOrder { order_id: last },
            NativeAction::PlaceOrder(gtc(2, true, 4, 1)),
            NativeAction::PlaceOrder(reduce_only.clone()),
        ] {
            let r = NativeExecutor::execute(&mut ctx, &a, &action);
            out.push((r.success, r.reason));
            check_counter(&mut ctx, "cap single");
        }
        let open: usize = ctx.order_books.values().map(|b| b.open_order_count(&a)).sum();
        assert_eq!(open, limit, "a ends at the limit");
        out
    };
    let want = run(true, 1);
    let ok = |i: usize| want[i].0;
    let open_limit = |i: usize| !want[i].0 && want[i].1 == FailureReason::OpenLimit;
    // Call 1: cancel ok; two free slots (limit - 2 open): the GTCs take
    // them; reduce-only and stop are at >= the base limit, the last GTC over.
    assert!(ok(0) && ok(1) && ok(2), "{want:?}");
    assert!(open_limit(3) && open_limit(4) && open_limit(5), "{want:?}");
    assert!(ok(6) && ok(7), "b's sells fill: {want:?}");
    // Call 2: the two fills freed two slots; the third order is over.
    assert!(ok(8) && ok(9) && open_limit(10), "{want:?}");
    // Single path: at the limit; a cancel frees one; reduce-only rejected.
    assert!(open_limit(11) && ok(12) && ok(13) && open_limit(14), "{want:?}");
    for (scan, threads) in [(false, 1), (false, 4), (true, 4)] {
        assert_eq!(run(scan, threads), want, "scan {scan} threads {threads}");
    }
}

/// The liquidation step's closing orders and cancels: a liquidated trader's
/// resting order and pending stop go, its stage-1 close fills a maker's bid
/// in full; the counter follows (the next block's place sees it), resident
/// and after the stash.
#[test]
fn liquidation_closes_and_cancels_keep_the_counter_exact() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &1u64.to_be_bytes(), b"listed")
        .unwrap();
    let mut holder = ResidentBooks::default();
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(&db, 1, BookMode::Classic, Some(&mut holder));
    fund(&ctx, t, 300);
    fund(&ctx, s, 1_000_000);
    fund(&ctx, m, 1_000_000);
    let place = |ctx: &mut NativeExecContext, who: &Address, p: PlaceOrderParams| {
        let r = NativeExecutor::execute(ctx, who, &NativeAction::PlaceOrder(p));
        assert!(r.success, "{:?}", r.error);
    };
    place(&mut ctx, &t, gtc(1, true, 900, 1));
    let stop = params(1, true, 1_200, 1, OrderType::StopMarket { trigger: fp(1_100) }, TimeInForce::IOC);
    place(&mut ctx, &t, stop);
    ctx.positions
        .apply_fill(&t, 1, true, fp(10), fp(1_000), torus_core::position::MarginType::Cross)
        .unwrap();
    ctx.positions
        .apply_fill(&s, 1, false, fp(10), fp(1_000), torus_core::position::MarginType::Cross)
        .unwrap();
    place(&mut ctx, &m, gtc(1, true, 985, 10));
    place(&mut ctx, &m, gtc(1, true, 980, 1));
    check_counter(&mut ctx, "before");
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle.submit_price(v, 1, fp(990), 1, ctx.timestamp).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    ctx.oracle.aggregate_price(1, 1, ctx.timestamp, &stakes).unwrap();
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert!(ctx.order_books[&1].orders_for_trader(&t).is_empty(), "t's order cancelled");
    assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "t's stop cancelled");
    assert_eq!(ctx.order_books[&1].open_order_count(&m), 1, "m's 985 bid filled");
    check_counter(&mut ctx, "after liquidation");
    ctx.save_order_books();
    ctx.stash_resident(&mut holder);
    let (carried, rebuilt) = holder.open_count_view().expect("resident");
    assert_eq!(carried.expect("built"), rebuilt, "carried");
    assert_eq!(rebuilt.get(&m), Some(&1));
    assert_eq!(rebuilt.get(&t), None);
}

/// The A/B's instrument: the time the exec thread spends getting the counts
/// (`open_count_ns`) and the log entries the counter absorbed
/// (`open_count_deltas`: one per (book, user) changed since the last
/// absorb). The reference scan absorbs nothing.
#[test]
fn open_count_time_and_absorbed_deltas_are_accumulated() {
    for scan in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut ctx = ctx_at(&db, 1, BookMode::Classic, None);
        ctx.test_open_count_scan = scan;
        let (a, b) = (addr(1), addr(2));
        fund(&ctx, a, 1_000_000);
        fund(&ctx, b, 1_000_000);
        let call1 = vec![
            (a, NativeAction::PlaceOrder(gtc(1, true, 90, 1))),
            (a, NativeAction::PlaceOrder(gtc(1, true, 91, 1))),
            (a, NativeAction::PlaceOrder(gtc(2, true, 90, 1))),
            (b, NativeAction::PlaceOrder(gtc(1, false, 110, 1))),
        ];
        NativeExecutor::execute_batch(&mut ctx, &call1);
        assert!(ctx.phase_accum.open_count_ns > 0, "scan {scan}: timed");
        assert_eq!(ctx.phase_accum.open_count_deltas, 0, "scan {scan}: empty books, empty logs");
        // (book 1, a), (book 2, a), (book 1, b).
        NativeExecutor::execute_batch(&mut ctx, &[(a, NativeAction::PlaceOrder(gtc(1, true, 92, 1)))]);
        let want = if scan { 0 } else { 3 };
        assert_eq!(ctx.phase_accum.open_count_deltas, want, "scan {scan}");
    }
}
