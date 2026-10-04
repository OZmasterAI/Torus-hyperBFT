//! Item 6 Phase 1 (C3, plan 2.4 / Step 3 P1): the margin sums cache.
//!
//! Seeded block sequences through the node's lifecycle (R and the slot
//! attached, pipelined overlay over the previous block's frozen set, flushed
//! one block later): positions opened, increased, partly closed, flipped and
//! fully closed (some Isolated, some overflow-sized, some UPnL-heavy near
//! the C6b guard), balance-only writes
//! (negative `available` included), marks fresh / moved / stale by time /
//! absent / reappearing, flat and multi-tier configs (a config change
//! mid-run), a listing and a delisting mid-run, orders (Phase 2 / 3) and
//! withdrawals. At every consumer call (shadow check inside `pos_sums`) and
//! at three points of every block (explicit `view` / `pos_net` / `free` /
//! `transfer_required` / `Err` / maker account per market), the cache path
//! equals the reference path; the same sequence without R and cache gives
//! the same results and rows block by block.

use super::*;
use torus_core::position::Position;
use torus_state::cf::{
    CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS,
    META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::FrozenPending;

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// A listed market row with `initial_margin` % (one flat tier).
fn market_row(initial_margin: i64) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), fp(initial_margin).raw()))
        .unwrap()
}

fn agg_key(m: MarketId) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8).
fn agg_row(price: FixedPoint, ts: u64) -> Vec<u8> {
    [price.raw().to_be_bytes().as_slice(), &7u64.to_be_bytes(), &3u32.to_be_bytes(), &ts.to_be_bytes()].concat()
}

fn trader(i: u64) -> Address {
    let mut b = [0x5Au8; 20];
    b[12..].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

/// Three tiers (20x / 10x / 4x) — the multi-tier config of market 2.
fn tiered(m: MarketId, top: u32) -> MarketMarginConfig {
    MarketMarginConfig {
        market_id: m,
        max_leverage: top,
        maintenance_factor_bps: 5000,
        tiers: vec![
            MarginTier { max_notional: fp(20_000), max_leverage: top },
            MarginTier { max_notional: fp(200_000), max_leverage: 10 },
            MarginTier { max_notional: FixedPoint::MAX, max_leverage: 4 },
        ],
    }
}

const TRADERS: u64 = 10;
/// Markets 1..=5 listed from the start, 6 listed at block 12, 5 delisted at block 25.
const MARKETS: u64 = 6;
const BLOCKS: u64 = 40;
const MID: i64 = 1_000;

/// What one block of a run produced (compared between the cache run and the
/// reference run).
#[derive(Default)]
struct BlockOut {
    results: String,
    rows: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Counters of the cache run (non-vacuous checks).
#[derive(Default, Debug)]
struct Stats {
    explicit: usize,
    shadow: usize,
    persistent_hits: usize,
    memo_hits: usize,
    computed: usize,
    dirty: usize,
    /// C6b: dirty valuations answered by the partial re-value / by a build.
    delta: usize,
    delta_fallback: usize,
    overflowed: usize,
    versions: BTreeSet<u64>,
}

/// Cache path == reference path for every trader: `view`, `pos_net`,
/// `free`, `transfer_required`, the error, and every maker account. The
/// reference reader has no cache (plan 2.4 path 1); a second one has no
/// mark table either (today's per-read oracle rule).
fn check_all<T: StateBackend>(ctx: &NativeExecContext<T>, stats: &mut Stats, tag: &str) {
    let cached = AccountReader::of(ctx);
    assert!(cached.sums.is_some() && cached.marks.is_some(), "{tag}: cache and table attached");
    let reference = AccountReader { sums: None, ..AccountReader::of(ctx) };
    let oracle_only = AccountReader { sums: None, marks: None, ..AccountReader::of(ctx) };
    for i in 0..TRADERS {
        let t = trader(i);
        let bal = ctx.positions.get_native_balance(&t).unwrap();
        let (a, b, c) = (cached.view(&t, &bal), reference.view(&t, &bal), oracle_only.view(&t, &bal));
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "{tag}: view of trader {i}");
        assert_eq!(format!("{b:?}"), format!("{c:?}"), "{tag}: view of trader {i} (no table)");
        if let (Ok(a), Ok(b)) = (&a, &b) {
            assert_eq!(a.free(), b.free(), "{tag}: free of trader {i}");
            assert_eq!(a.transfer_required(), b.transfer_required(), "{tag}: transfer_required of trader {i}");
        } else {
            stats.overflowed += 1;
        }
        assert_eq!(format!("{:?}", cached.pos_net(&t)), format!("{:?}", reference.pos_net(&t)), "{tag}: pos_net of trader {i}");
        assert_eq!(cached.maker_free(&t), reference.maker_free(&t), "{tag}: maker free of trader {i}");
        for m in 1..=MARKETS {
            assert_eq!(cached.maker_account(&t, m), reference.maker_account(&t, m), "{tag}: maker account {i} market {m}");
        }
        stats.explicit += 1;
    }
}

/// One seeded block's direct writes: positions and balances.
fn write_positions<T: StateBackend>(ctx: &NativeExecContext<T>, rng: &mut Lcg, h: u64) {
    for _ in 0..rng.below(6) {
        let t = trader(rng.below(TRADERS));
        let m = 1 + rng.below(MARKETS);
        let old = ctx.positions.get_position(&t, m).unwrap();
        let size = |rng: &mut Lcg| FixedPoint::from_raw(1 + rng.below(40 * FixedPoint::SCALE as u64) as i128);
        let entry = fp(MID - 50 + rng.below(100) as i64);
        let mut pos = old.clone().unwrap_or(Position {
            trader: t,
            market_id: m,
            is_long: rng.below(2) == 0,
            size: FixedPoint::ZERO,
            entry_price: entry,
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        });
        match rng.below(7) {
            // open / increase
            0 | 1 => pos.size += size(rng),
            // partial close
            2 if old.is_some() => pos.size = FixedPoint::from_raw((pos.size.raw() / 2).max(1)),
            // flip
            3 => {
                pos.is_long = !pos.is_long;
                pos.size = size(rng);
                pos.entry_price = entry;
            }
            // full close
            4 if old.is_some() => {
                ctx.positions.delete_position(&t, m).unwrap();
                continue;
            }
            // isolated
            5 => {
                pos.margin_type = if pos.margin_type == MarginType::Cross { MarginType::Isolated } else { MarginType::Cross };
                pos.size += size(rng);
            }
            // C6b: UPnL-heavy (entry far above the mark; ~0.4 x i128::MAX
            // per position): mixed-sign sums near the guard (rare)
            6 if h.is_multiple_of(4) => {
                pos.entry_price = fp(1_000_000);
                pos.size = FixedPoint::from_raw(i128::MAX / 999_000 * 4 / 10 / FixedPoint::SCALE * FixedPoint::SCALE);
            }
            // overflow-sized (rare)
            _ if h.is_multiple_of(9) => pos.size = FixedPoint::from_raw(i128::MAX / 3),
            _ => pos.size += size(rng),
        }
        ctx.positions.put_position(&pos).unwrap();
    }
    // Balance-only writes (sums unaffected), negative `available` included.
    for _ in 0..rng.below(3) {
        let t = trader(rng.below(TRADERS));
        let bal = NativeBalance { available: fp(rng.below(400_000) as i64 - 100_000), order_margin: fp(rng.below(500) as i64) };
        ctx.positions.put_native_balance(&t, &bal).unwrap();
    }
}

/// A few orders around the mid (some crossing) and one withdrawal attempt.
fn block_actions(rng: &mut Lcg) -> Vec<(Address, NativeAction)> {
    let mut out = Vec::new();
    for _ in 0..4 + rng.below(8) {
        let t = trader(rng.below(TRADERS));
        let m = 1 + rng.below(MARKETS);
        let is_buy = rng.below(2) == 0;
        let price = fp(MID - 3 + rng.below(7) as i64);
        let params = PlaceOrderParams {
            market_id: m,
            is_buy,
            price,
            quantity: fp(1 + rng.below(30) as i64),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: rng.below(10) == 0,
            client_order_id: None,
        };
        out.push((t, NativeAction::PlaceOrder(params)));
    }
    let t = trader(rng.below(TRADERS));
    out.push((t, NativeAction::TransferToSpot { amount: fp_to_u256(fp(1 + rng.below(50_000) as i64)) }));
    out
}

/// One seeded run. `cached`: the node path (R, slot, sums cache, shadow
/// check on); else the reference path (`begin_resident(None, ..)`).
fn run(seed: u64, cached: bool, stats: &mut Stats) -> Vec<BlockOut> {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=5u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(if m == 3 { 10 } else { 5 })).unwrap();
    }
    {
        let ctx = NativeExecContext::new(db.clone(), 1, 1_000, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        for i in 0..TRADERS {
            let available = fp(if i == 0 { -5_000 } else { 50_000 + 40_000 * i as i64 });
            ctx.positions.put_native_balance(&trader(i), &NativeBalance { available, order_margin: FixedPoint::ZERO }).unwrap();
        }
    }
    let mut rng = Lcg(seed);
    let mut holder = ResidentBooks::default();
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut books = HashMap::new();
    let mut next_id: u128 = 1;
    let mut prices: Vec<FixedPoint> = vec![fp(MID); MARKETS as usize + 1];
    let mut out = Vec::new();
    for h in 1..=BLOCKS {
        let now = 10_000 + 7 * h;
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rb = begin_resident(cached.then_some(&mut holder), &mut overlay, h, None);
        assert_eq!(rb.attached(), cached);
        // Previous block's governance: a listing at 12, a delisting at 25
        // (configs show in the next context).
        if h == 12 {
            overlay.put_cf_raw(CF_NATIVE_MARKETS, &6u64.to_be_bytes(), &market_row(5)).unwrap();
        }
        if h == 25 {
            overlay.delete_cf_raw(CF_NATIVE_MARKETS, &5u64.to_be_bytes()).unwrap();
        }
        // Marks: kept (ages, stale after 60 s = ~9 blocks), refreshed at the
        // same price, moved, removed (absent), and rewritten (reappearing).
        for m in 1..=MARKETS {
            match rng.below(10) {
                0..=3 => {}
                4..=5 => overlay.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(prices[m as usize], now - 1)).unwrap(),
                6..=7 => {
                    prices[m as usize] = fp(MID - 20 + rng.below(40) as i64);
                    overlay.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(prices[m as usize], now - 1)).unwrap();
                }
                8 => overlay.delete_cf_raw(CF_NATIVE_ORACLE, &agg_key(m)).unwrap(),
                _ => {}
            }
        }
        let mut ctx =
            NativeExecContext::new(overlay.clone(), h, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        // Multi-tier config for market 2, changing every 7 blocks.
        ctx.margin_configs.insert(2, tiered(2, if (h / 7) % 2 == 0 { 20 } else { 25 }));
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        ctx.attach_resident_block(&mut rb);
        if cached {
            ctx.sums.as_mut().expect("slot sums attached").shadow = true;
        }
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        if cached {
            stats.versions.insert(ctx.mark_version().unwrap());
            check_all(&ctx, stats, &format!("seed {seed} block {h} start"));
        }
        write_positions(&ctx, &mut rng, h);
        if cached {
            check_all(&ctx, stats, &format!("seed {seed} block {h} after writes"));
        }
        let actions = block_actions(&mut rng);
        let threads = if h % 2 == 0 { 4 } else { 0 };
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, threads);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let liq = NativeExecutor::run_liquidations(&mut ctx);
        if cached {
            check_all(&ctx, stats, &format!("seed {seed} block {h} end"));
            let sums = ctx.sums.as_ref().unwrap();
            let bad = sums.shadow_mismatches.lock().unwrap().clone();
            assert!(bad.is_empty(), "seed {seed} block {h}: cache != reference at a consumer call: {bad:?}");
            let c = &sums.counters;
            stats.shadow += c.shadow.load(std::sync::atomic::Ordering::Relaxed);
            stats.persistent_hits += c.persistent.load(std::sync::atomic::Ordering::Relaxed);
            stats.memo_hits += c.memo.load(std::sync::atomic::Ordering::Relaxed);
            stats.computed += c.computed.load(std::sync::atomic::Ordering::Relaxed);
            stats.dirty += c.dirty.load(std::sync::atomic::Ordering::Relaxed);
            stats.delta += c.delta.load(std::sync::atomic::Ordering::Relaxed);
            stats.delta_fallback += c.delta_fallback.load(std::sync::atomic::Ordering::Relaxed);
        }
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        let results = format!("{:?} {:?}", r.results.iter().map(|x| (&x.error, x.success)).collect::<Vec<_>>(), liq.iter().map(|x| (&x.error, x.success)).collect::<Vec<_>>());
        let mut rows = overlay.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap();
        rows.extend(overlay.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
        out.push(BlockOut { results, rows });
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = overlay.own_pending_delta();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rb, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    if cached {
        assert_eq!(holder.rows_builds(), 1, "R built once");
        assert_eq!(holder.rows_shared_fallbacks(), 0);
    }
    out
}

/// P1: 6 seeds x 40 blocks — cache path == reference path at every consumer
/// call and at every explicit check, and the cache run's results and rows
/// equal the reference run's block by block.
#[test]
fn sums_cache_equals_reference_on_seeded_sequences() {
    let mut stats = Stats::default();
    for seed in 1..=6u64 {
        let cached = run(seed * 0x9E37_79B9, true, &mut stats);
        let reference = run(seed * 0x9E37_79B9, false, &mut Stats::default());
        for (h, (a, b)) in cached.iter().zip(reference.iter()).enumerate() {
            assert_eq!(a.results, b.results, "seed {seed} block {}: results", h + 1);
            assert!(a.rows == b.rows, "seed {seed} block {}: position / balance rows", h + 1);
        }
    }
    println!(
        "SUMS_CACHE P1 explicit={} shadow={} persistent={} memo={} computed={} dirty={} delta={} delta_fallback={} overflowed={} versions={}",
        stats.explicit, stats.shadow, stats.persistent_hits, stats.memo_hits, stats.computed, stats.dirty, stats.delta, stats.delta_fallback, stats.overflowed, stats.versions.len()
    );
    // C6b: dirty traders are re-valued from their memo sums (shadow-checked
    // against `build` above); the guard / no-base paths still build.
    assert!(stats.delta > 100 && stats.delta_fallback > 10, "both dirty paths used: {stats:?}");
    assert!(stats.explicit > 5_000 && stats.shadow > 5_000, "non-vacuous: {stats:?}");
    assert!(stats.persistent_hits > 100 && stats.memo_hits > 100, "every cache path used: {stats:?}");
    assert!(stats.computed > 100 && stats.dirty > 100, "every cache path used: {stats:?}");
    assert!(stats.overflowed > 0, "overflow-sized positions reached: {stats:?}");
    assert!(stats.versions.len() > 6, "marks / configs moved: {stats:?}");
}

/// The cache is dropped with the slot: a guard trip (skipped height) rebuilds
/// R with an empty cache, and `end_resident` drops the sums of every trader
/// the block wrote (positions), keeps the others, and keeps nothing at a
/// stale version.
#[test]
fn end_resident_drops_dirtied_traders_and_rebuild_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), &market_row(5)).unwrap();
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(1), &agg_row(fp(MID), 10_000)).unwrap();
    let (a, b) = (trader(0), trader(1));
    for t in [a, b] {
        PositionManager::new(db.clone())
            .put_position(&Position {
                trader: t,
                market_id: 1,
                is_long: true,
                size: fp(2),
                entry_price: fp(MID - 10),
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: MarginType::Cross,
            })
            .unwrap();
    }
    let mut holder = ResidentBooks::default();
    // Returns the traders with sums in the slot after the block and the
    // block's persistent hits.
    let block = |holder: &mut ResidentBooks, h: u64, write_a: bool| -> (Vec<Address>, usize) {
        let mut overlay = NativeStateOverlay::new(db.clone());
        let mut rb = begin_resident(Some(holder), &mut overlay, h, None);
        let mut ctx = NativeExecContext::new(overlay.clone(), h, 10_001, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        ctx.attach_resident_block(&mut rb);
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        let reader = AccountReader::of(&ctx);
        reader.pos_net(&a).unwrap();
        reader.pos_net(&b).unwrap();
        let hits = ctx.sums.as_ref().unwrap().counters.persistent.load(std::sync::atomic::Ordering::Relaxed);
        if write_a {
            ctx.positions.delete_position(&a, 1).unwrap();
        }
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        let delta = overlay.own_pending_delta();
        overlay.flush_with_native_trie_and_marker(&db, h).unwrap();
        end_resident(holder, rb, &mut overlay, delta, true, None);
        let mut kept: Vec<Address> = holder.rows.as_ref().unwrap().sums.map.keys().copied().collect();
        kept.sort();
        (kept, hits)
    };
    assert_eq!(block(&mut holder, 1, false), (vec![a, b], 0), "both valued and kept");
    assert_eq!(block(&mut holder, 2, true), (vec![b], 2), "a's positions written: dropped");
    assert_eq!(block(&mut holder, 3, false), (vec![a, b], 1), "a valued again, b from the slot");
    assert_eq!(block(&mut holder, 4, false), (vec![a, b], 2), "both from the slot");
    assert_eq!(block(&mut holder, 6, false), (vec![a, b], 0), "skipped height: R rebuilt, cache starts empty");
    assert_eq!(holder.rows_builds(), 2);
}

/// C6b (the cliff): a maker filled by an IOC order in the block's first
/// (pre-EVM) batch is dirty in the second (post-EVM) batch; each market it
/// fills there asks for its account. It is built once (its memo, over R)
/// and re-valued from it, never rebuilt per market. Every answer is
/// shadow-checked against `build`.
#[test]
fn maker_dirtied_by_an_earlier_batch_is_not_rebuilt_per_market() {
    const BOOKS: u64 = 6;
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let now = 10_000;
    let maker = trader(0);
    {
        let pm = PositionManager::new(db.clone());
        for m in 1..=BOOKS + 20 {
            if m <= BOOKS {
                db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(5)).unwrap();
            }
            db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(fp(MID + m as i64), now - 1)).unwrap();
            // The maker holds a position in every market (some never traded here).
            pm.put_position(&Position {
                trader: maker,
                market_id: m,
                is_long: m % 2 == 0,
                size: fp(3),
                entry_price: fp(MID - 7),
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: MarginType::Cross,
            })
            .unwrap();
        }
        for i in 0..=BOOKS {
            pm.put_native_balance(&trader(i), &NativeBalance { available: fp(10_000_000), order_margin: FixedPoint::ZERO }).unwrap();
        }
    }
    let mut holder = ResidentBooks::default();
    let mut overlay = NativeStateOverlay::new(db.clone());
    let mut rb = begin_resident(Some(&mut holder), &mut overlay, 1, None);
    let mut ctx = NativeExecContext::new(overlay.clone(), 1, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
    ctx.attach_resident_block(&mut rb);
    ctx.sums.as_mut().expect("slot sums attached").shadow = true;
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let order = |m: MarketId, is_buy: bool, tif: TimeInForce| {
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: m,
            is_buy,
            price: fp(MID),
            quantity: fp(if is_buy { 1 } else { 10 }),
            order_type: OrderType::Limit,
            time_in_force: tif,
            reduce_only: false,
            client_order_id: None,
        })
    };
    let builds = |ctx: &NativeExecContext<NativeStateOverlay>| ctx.sums.as_ref().unwrap().counters.builds_of(&maker);
    let ok = |r: &NativeBatchResult| assert!(r.results.iter().all(|x| x.success), "{:?}", r.results.iter().map(|x| &x.error).collect::<Vec<_>>());
    // Resting asks of the maker in every book.
    let asks: Vec<_> = (1..=BOOKS).map(|m| (maker, order(m, false, TimeInForce::GTC))).collect();
    ok(&NativeExecutor::execute_batch_engine_mode(&mut ctx, &asks, 0));
    let after_asks = builds(&ctx);
    assert_eq!(after_asks, 1, "valued once (Phase 2)");
    // Pre-EVM batch: an IOC buy fills the maker in market 1.
    ok(&NativeExecutor::execute_batch_engine_mode(&mut ctx, &[(trader(1), order(1, true, TimeInForce::IOC))], 0));
    assert!(ctx.positions.state().layer_touches(CF_NATIVE_POSITIONS, maker.as_slice()), "maker dirtied");
    let after_ioc = builds(&ctx);
    // Post-EVM batch: fills in every other book (serial and 4 workers).
    for threads in [0usize, 4] {
        let before = builds(&ctx);
        let buys: Vec<_> = (2..=BOOKS).map(|m| (trader(m), order(m, true, TimeInForce::GTC))).collect();
        ok(&NativeExecutor::execute_batch_engine_mode(&mut ctx, &buys, threads));
        let built = builds(&ctx) - before;
        assert!(built <= 1, "threads {threads}: maker built {built}x in one batch over {} markets", BOOKS - 1);
    }
    let sums = ctx.sums.as_ref().unwrap();
    assert!(sums.counters.dirty.load(std::sync::atomic::Ordering::Relaxed) >= 2 * (BOOKS as usize - 1), "dirty maker valued per market");
    let bad = sums.shadow_mismatches.lock().unwrap().clone();
    assert!(bad.is_empty(), "{bad:?}");
    assert!(after_ioc <= after_asks + 1);
    // The maker's account after all of it == a reference build.
    let reference = AccountReader { sums: None, ..AccountReader::of(&ctx) };
    let cached = AccountReader::of(&ctx);
    let bal = ctx.positions.get_native_balance(&maker).unwrap();
    assert_eq!(cached.view(&maker, &bal).unwrap(), reference.view(&maker, &bal).unwrap());
    let fills: usize = (2..=BOOKS).map(|m| ctx.positions.get_position(&trader(m), m).unwrap().map_or(0, |_| 1)).sum();
    assert_eq!(fills, BOOKS as usize - 1, "every post-EVM buyer filled");
}

/// C6b magnitude guard: UPnL terms of mixed sign can cancel in a trader's
/// sums while `build`, adding its positions in key order, overflows on the
/// way. R: market 1 (+0.45 MAX UPnL) and market 3 (-0.45 MAX), Σ |UPnL|
/// 0.9 MAX; the block adds market 2 (+0.6 MAX). `build` overflows at market
/// 2; the partial re-value (0 + 0.6 MAX) would not — the guard (Σ |UPnL|
/// now 1.5 MAX) sends it to `build`: both say overflow.
#[test]
fn delta_guard_falls_back_when_build_order_overflows() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let now = 10_000;
    let t = trader(0);
    // |UPnL| = (1_000_000 - 1_000) x size ~ `tenths` / 10 x i128::MAX.
    let size = |tenths: i128| FixedPoint::from_raw(i128::MAX / 999_000 * tenths / 10 / FixedPoint::SCALE * FixedPoint::SCALE);
    let pos = |m: MarketId, is_long: bool, size: FixedPoint| Position {
        trader: t,
        market_id: m,
        is_long,
        size,
        entry_price: fp(1_000_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    };
    {
        let pm = PositionManager::new(db.clone());
        for m in 1..=3u64 {
            db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(5)).unwrap();
            db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(fp(MID), now - 1)).unwrap();
        }
        let half = FixedPoint::from_raw(size(9).raw() / 2);
        pm.put_position(&pos(1, false, half)).unwrap(); // short, mark far below entry: UPnL > 0
        pm.put_position(&pos(3, true, half)).unwrap(); // long: UPnL < 0
    }
    let mut holder = ResidentBooks::default();
    let mut overlay = NativeStateOverlay::new(db.clone());
    let mut rb = begin_resident(Some(&mut holder), &mut overlay, 1, None);
    let mut ctx = NativeExecContext::new(overlay.clone(), 1, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
    ctx.attach_resident_block(&mut rb);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let reader = AccountReader::of(&ctx);
    let base = reader.pos_sums(&t).expect("R's rows: no overflow");
    assert!(base.abs[0] < ABS_GUARD && base.abs[0] > ABS_GUARD / 10 * 8, "Σ |UPnL| ~0.9 MAX: {:?}", base.abs);
    ctx.positions.put_position(&pos(2, false, size(6))).unwrap();
    let reader = AccountReader::of(&ctx);
    let reference = AccountReader { sums: None, ..AccountReader::of(&ctx) };
    let got = format!("{:?}", reader.pos_sums(&t));
    assert_eq!(got, format!("{:?}", reference.pos_sums(&t)));
    assert!(got.contains("Overflow"), "build order overflows: {got}");
    let c = &ctx.sums.as_ref().unwrap().counters;
    assert_eq!(c.delta.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(c.delta_fallback.load(std::sync::atomic::Ordering::Relaxed), 1);
}
