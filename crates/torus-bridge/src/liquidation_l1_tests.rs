//! Item 6 Phase 1 (C4, plan 2.5 / Step 4 P3): liquidation L1.
//!
//! Seeded block sequences through the node's lifecycle (R and the slot
//! attached, pipelined overlay over the previous block's frozen set, flushed
//! one block later), run twice: with L1 (sums cache) and with the reference
//! walk (`begin_resident(None, ..)`: no cache, `liq_view`'s `build` over the
//! trader's rows). Accounts near maintenance, marks moving / jumping / stale
//! / absent, big positions (stage-1 chunks and the cooldown, in which stage
//! 1 orders the entire position — HL parity, s88; two chunks of one account
//! in one block, rule B — s91, the `MULTI` accounts), cursor cuts
//! (small budgets every third block), backstop, ADL of a trader and of the
//! vault, Isolated positions, a listed market without a mark (positions at
//! entry; an account only there is skipped), a delisted market whose
//! aggregate stays fresh (L1 off for those blocks), marks off (every
//! aggregate gone), makers' books and crossing orders (traders dirtied in
//! the block). At every L1 valuation (shadow check inside `liq_view`) L1 ==
//! the walk; block by block the two runs produce identical results,
//! `CF_NATIVE_LIQUIDATION` / positions / balances rows and
//! `liquidations_triggered`.

use super::*;
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
    let mut b = [0x6Bu8; 20];
    b[12..].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

/// Three tiers (20x / 10x / 4x): market 2's config.
fn tiered(m: MarketId) -> MarketMarginConfig {
    MarketMarginConfig {
        market_id: m,
        max_leverage: 20,
        maintenance_factor_bps: 5000,
        tiers: vec![
            MarginTier { max_notional: fp(40_000), max_leverage: 20 },
            MarginTier { max_notional: fp(120_000), max_leverage: 10 },
            MarginTier { max_notional: FixedPoint::MAX, max_leverage: 4 },
        ],
    }
}

const TRADERS: u64 = 16;
/// Trader 14 trades only market 5 (listed, never marked): skipped.
const UNMARKED_ONLY: u64 = 14;
/// Trader 15 opens Isolated positions: skipped.
const ISOLATED: u64 = 15;
/// Markets 1..=4 marked; 5 listed, never marked.
const MARKETS: u64 = 5;
const MARKED: u64 = 4;
/// Market 4: delisted at 20 with its aggregate kept fresh until 25 (L1 off),
/// aggregate gone at 26, listed again at 30.
const DELIST: u64 = 20;
const AGG_GONE: u64 = 26;
const RELIST: u64 = 30;
/// Every aggregate gone (marks off) for these blocks.
const MARKS_OFF: std::ops::RangeInclusive<u64> = 40..=43;
const BLOCKS: u64 = 60;
const MID: i64 = 1_000;
/// Rule B (s91): `(block, trader)` — the trader opens long 150 in markets 1
/// and 3 (two positions above 100,000 notional) against [`MULTI_SHORT`] at
/// the block's prices, collateral 2.2% of the notional (MM 2.5%): stage 1,
/// outside any cooldown. [`MULTI_SHORT`] bids 200 at about 2% under the
/// price (whole units, the tick; inside the 2.5% cap; a stale maker ask it
/// crosses is taken, the rest rests) in both markets in that block, so both 20% chunks (30 each) fill
/// and the account is still under MM after the first.
const MULTI: [(u64, u64); 2] = [(13, 30), (49, 31)];
const MULTI_SHORT: u64 = 32;

fn maker(i: u64) -> Address {
    trader(100 + i)
}

/// What one block of a run produced.
#[derive(Default, PartialEq, Eq)]
struct BlockOut {
    results: String,
    /// E4: the context's margin configs and listed markets (market rows
    /// read through R in the L1 run).
    markets: String,
    rows: Vec<(Vec<u8>, Vec<u8>)>,
    triggered: u64,
    /// adl-budget A7: `liquidation_adl_work_total` after the block (drain
    /// work units: a ranking examines `liq_traders_after(None, MAX)`, the
    /// slot list on the L1 path, the walk on the reference path).
    adl_work: u64,
}

/// Counters of the L1 run (non-vacuous checks).
#[derive(Default, Debug)]
struct Stats {
    stage1: usize,
    backstop: usize,
    adl: usize,
    vault_adl: usize,
    skipped: usize,
    cuts: usize,
    cooldowns: usize,
    /// Stage-1 accounts met inside their 30 s cooldown (full-position orders).
    cooldown_stage1: usize,
    /// Rule B (s91): stage-1 accounts outside the cooldown whose step reduced
    /// two or more positions in the block and started a cooldown (a chunk
    /// plus at least one more order in the same block).
    multi_same_block: usize,
    l1: usize,
    l1_off: usize,
    /// E2: candidate lists from the slot's trader set (shadow-checked
    /// against the walk inside the step).
    traders_slice: usize,
    /// adl-budget Q1: ADL counterparty rankings (each from the slot's
    /// trader set, shadow-checked like the pass's list).
    adl_rankings: usize,
    shadow: usize,
    persistent: usize,
    memo: usize,
    dirty: usize,
    delisted_marked_blocks: usize,
    marks_off_blocks: usize,
}

/// The step's `Marks` (as `liquidation_pass` builds them).
fn step_marks<T: StateBackend>(ctx: &NativeExecContext<T>) -> (Vec<MarketId>, Marks) {
    let listed = ctx.governance.listed_market_ids().unwrap();
    let reader = AccountReader::of(ctx);
    let marks = listed.iter().filter_map(|&m| reader.mark(m).map(|p| (m, p))).collect();
    (listed, marks)
}

/// Exposure: matched pairs between traders (some big: chunks), each side's
/// balance set relative to its new notional (healthy to under MM).
fn write_exposure<T: StateBackend>(ctx: &NativeExecContext<T>, rng: &mut Lcg, prices: &[FixedPoint]) {
    for _ in 0..1 + rng.below(4) {
        let (a, b) = (rng.below(TRADERS), rng.below(TRADERS));
        if a == b {
            continue;
        }
        let m = if a == UNMARKED_ONLY || b == UNMARKED_ONLY { MARKETS } else { 1 + rng.below(MARKETS) };
        let cap = if rng.below(4) == 0 { 160 } else { 40 };
        let qty = fp(1 + rng.below(cap) as i64);
        let px = prices[m as usize];
        for (t, is_buy) in [(a, true), (b, false)] {
            let mt = if t == ISOLATED { MarginType::Isolated } else { MarginType::Cross };
            ctx.positions.apply_fill(&trader(t), m, is_buy, qty, px, mt).unwrap();
            let notional = qty.checked_mul(px).unwrap();
            let r = 15 + rng.below(70) as i128; // 1.5% .. 8.5% of the new notional
            let available = FixedPoint::from_raw(notional.raw() / 1_000 * r);
            ctx.positions.put_native_balance(&trader(t), &NativeBalance { available, order_margin: FixedPoint::ZERO }).unwrap();
        }
    }
}

/// Rule B: the [`MULTI`] account of block `h`, if any.
fn write_multi<T: StateBackend>(ctx: &NativeExecContext<T>, h: u64, prices: &[FixedPoint]) {
    let Some(&(_, i)) = MULTI.iter().find(|(b, _)| *b == h) else { return };
    let mut notional = FixedPoint::ZERO;
    for m in [1, 3] {
        let px = prices[m as usize];
        ctx.positions.apply_fill(&trader(i), m, true, fp(150), px, MarginType::Cross).unwrap();
        ctx.positions.apply_fill(&trader(MULTI_SHORT), m, false, fp(150), px, MarginType::Cross).unwrap();
        notional += fp(150).checked_mul(px).unwrap();
    }
    let available = FixedPoint::from_raw(notional.raw() / 1_000 * 22);
    ctx.positions.put_native_balance(&trader(i), &NativeBalance { available, order_margin: FixedPoint::ZERO }).unwrap();
    let rich = NativeBalance { available: fp(1_000_000_000), order_margin: FixedPoint::ZERO };
    ctx.positions.put_native_balance(&trader(MULTI_SHORT), &rich).unwrap();
}

/// Makers quote around each marked market's price (every third block);
/// a few traders send crossing IOC orders (fills dirty them in the block).
fn block_actions(rng: &mut Lcg, h: u64, prices: &[FixedPoint]) -> Vec<(Address, NativeAction)> {
    let order = |m: MarketId, is_buy: bool, price: FixedPoint, qty: i64, tif: TimeInForce| PlaceOrderParams {
        market_id: m,
        is_buy,
        price,
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    };
    let mut out = Vec::new();
    if MULTI.iter().any(|x| x.0 == h) {
        for m in [1, 3] {
            let p = prices[m as usize];
            // Whole units: the market row's tick is 1.
            let px = fp((p.raw() / 50 * 49 / FixedPoint::SCALE) as i64);
            let bid = order(m, true, px, 200, TimeInForce::GTC);
            out.push((trader(MULTI_SHORT), NativeAction::PlaceOrder(bid)));
        }
    }
    if h % 3 == 1 {
        for m in 1..=MARKED {
            let p = prices[m as usize];
            let off = FixedPoint::from_raw(p.raw() / 250);
            out.push((maker(0), NativeAction::PlaceOrder(order(m, true, p - off, 60, TimeInForce::GTC))));
            out.push((maker(1), NativeAction::PlaceOrder(order(m, false, p + off, 60, TimeInForce::GTC))));
        }
    }
    for _ in 0..rng.below(4) {
        let t = trader(rng.below(TRADERS));
        let m = 1 + rng.below(MARKED);
        let is_buy = rng.below(2) == 0;
        let p = prices[m as usize];
        let off = FixedPoint::from_raw(p.raw() / 100);
        let price = if is_buy { p + off } else { p - off };
        out.push((t, NativeAction::PlaceOrder(order(m, is_buy, price, 1 + rng.below(5) as i64, TimeInForce::IOC))));
    }
    out
}

/// One seeded run. `l1`: the node path (R, slot, sums cache, shadow check
/// on); else the reference walk (`begin_resident(None, ..)`).
fn run(seed: u64, l1: bool, stats: &mut Stats) -> Vec<BlockOut> {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=MARKETS {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(5)).unwrap();
    }
    {
        let ctx = NativeExecContext::new(db.clone(), 1, 1_000, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        for i in 0..2 {
            ctx.positions
                .put_native_balance(&maker(i), &NativeBalance { available: fp(1_000_000_000), order_margin: FixedPoint::ZERO })
                .unwrap();
        }
    }
    let metrics = Arc::new(torus_telemetry::Metrics::new());
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
        let mut rb = begin_resident(l1.then_some(&mut holder), &mut overlay, h, None);
        assert_eq!(rb.attached(), l1);
        // Previous block's governance (configs show in the next context).
        if h == DELIST {
            overlay.delete_cf_raw(CF_NATIVE_MARKETS, &4u64.to_be_bytes()).unwrap();
        }
        if h == RELIST {
            overlay.put_cf_raw(CF_NATIVE_MARKETS, &4u64.to_be_bytes(), &market_row(5)).unwrap();
        }
        // Marks: kept (ages; stale after 60 s = ~9 blocks), refreshed at the
        // same price, moved, jumped; gone for 4 at AGG_GONE..RELIST and for
        // every market in MARKS_OFF.
        for m in 1..=MARKED {
            if MARKS_OFF.contains(&h) || (m == 4 && (AGG_GONE..RELIST).contains(&h)) {
                overlay.delete_cf_raw(CF_NATIVE_ORACLE, &agg_key(m)).unwrap();
                continue;
            }
            let fresh = h == *MARKS_OFF.end() + 1 || (m == 4 && (h == RELIST || (DELIST..AGG_GONE).contains(&h)));
            let p = prices[m as usize].raw();
            match rng.below(20) {
                0..=8 if !fresh => continue,
                0..=12 => {}
                13..=18 => prices[m as usize] = FixedPoint::from_raw(p + p / 1_000 * (rng.below(61) as i128 - 30)),
                _ => prices[m as usize] = FixedPoint::from_raw(p + p / 100 * (rng.below(25) as i128 - 12)),
            }
            overlay.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(prices[m as usize], now - 1)).unwrap();
        }
        let mut ctx =
            NativeExecContext::new(overlay.clone(), h, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        let ctx_markets = (
            ctx.margin_configs.iter().map(|(m, c)| (*m, c.clone())).collect::<BTreeMap<_, _>>(),
            ctx.governance.listed_market_ids().unwrap(),
        );
        ctx.margin_configs.insert(2, tiered(2));
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        ctx.metrics = Some(metrics.clone());
        ctx.attach_resident_block(&mut rb);
        if l1 {
            ctx.sums.as_mut().expect("slot sums attached").shadow = true;
        }
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        write_exposure(&ctx, &mut rng, &prices);
        write_multi(&ctx, h, &prices);
        let actions = block_actions(&mut rng, h, &prices);
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, if h % 2 == 0 { 4 } else { 0 });
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        // Rule B: stage-1 accounts outside the cooldown, their sizes before the step.
        let mut fresh_stage1: Vec<(Address, BTreeMap<MarketId, FixedPoint>)> = Vec::new();
        if l1 {
            // What the walk will meet (non-vacuous checks).
            let (listed, marks) = step_marks(&ctx);
            let table = ctx.block_marks.as_ref().unwrap();
            stats.delisted_marked_blocks += usize::from(!table.delisted_marked(&listed).is_empty());
            stats.marks_off_blocks += usize::from(marks.is_empty());
            for t in (0..TRADERS).chain(MULTI.map(|x| x.1)).map(trader).chain([LIQUIDATOR_VAULT]) {
                match NativeExecutor::liq_view_walk(&ctx, &marks, &t).unwrap().and_then(|v| liq::classify(&v)) {
                    Some(Health::Stage1) => {
                        stats.stage1 += 1;
                        let cooling = liq::in_cooldown(&ctx.state, &t, now).unwrap();
                        stats.cooldown_stage1 += usize::from(cooling);
                        if !cooling {
                            let ps = ctx.positions.positions_for_trader(&t).unwrap();
                            fresh_stage1.push((t, ps.iter().map(|p| (p.market_id, p.size)).collect()));
                        }
                    }
                    Some(Health::Backstop) => stats.backstop += 1,
                    Some(Health::Adl) if t == LIQUIDATOR_VAULT => stats.vault_adl += 1,
                    Some(Health::Adl) => stats.adl += 1,
                    Some(Health::Healthy) => {}
                    None => stats.skipped += 1,
                }
            }
        }
        let liq = if h % 3 == 0 {
            NativeExecutor::run_liquidations_with(&mut ctx, 3, 2, liq::ADL_WORK_PER_BLOCK)
        } else {
            NativeExecutor::run_liquidations(&mut ctx)
        };
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let liq_rows = ctx.state.iterate_cf(CF_NATIVE_LIQUIDATION, None).unwrap();
        if l1 {
            stats.cuts += usize::from(liq_rows.iter().any(|(k, _)| k.as_slice() == liq::CURSOR_KEY));
            stats.cooldowns += liq_rows.iter().filter(|(k, _)| k[0] == liq::COOLDOWN_TAG).count();
            for (t, before) in &fresh_stage1 {
                let after: BTreeMap<MarketId, FixedPoint> =
                    ctx.positions.positions_for_trader(t).unwrap().iter().map(|p| (p.market_id, p.size)).collect();
                let reduced = before.iter().filter(|(m, sz)| after.get(m).is_none_or(|a| a < sz)).count();
                let key = [[liq::COOLDOWN_TAG].as_slice(), t.as_slice()].concat();
                let chunked_now =
                    ctx.state.get_cf_raw(CF_NATIVE_LIQUIDATION, &key).unwrap() == Some(now.to_be_bytes().to_vec());
                stats.multi_same_block += usize::from(reduced >= 2 && chunked_now);
            }
            let sums = ctx.sums.as_ref().unwrap();
            let bad = sums.shadow_mismatches.lock().unwrap().clone();
            assert!(bad.is_empty(), "seed {seed} block {h}: L1 != walk / cache != reference: {bad:?}");
            let c = &sums.counters;
            let get = |a: &std::sync::atomic::AtomicUsize| a.load(std::sync::atomic::Ordering::Relaxed);
            stats.l1 += get(&c.l1);
            stats.l1_off += get(&c.l1_off);
            stats.traders_slice += get(&c.traders_slice);
            stats.adl_rankings += get(&c.adl_rankings);
            stats.shadow += get(&c.shadow);
            stats.persistent += get(&c.persistent);
            stats.memo += get(&c.memo);
            stats.dirty += get(&c.dirty);
        }
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        let results = format!(
            "{:?} {:?}",
            r.results.iter().map(|x| (&x.error, x.success)).collect::<Vec<_>>(),
            liq.iter().map(|x| (&x.error, x.success, x.gas_used)).collect::<Vec<_>>()
        );
        let markets = format!("{:?} {:?}", ctx_markets.0, ctx_markets.1);
        let mut rows = liq_rows;
        rows.extend(overlay.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap());
        rows.extend(overlay.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
        let adl_work = metrics.liquidation_adl_work_total.get();
        out.push(BlockOut { results, markets, rows, triggered: metrics.liquidations_triggered.get(), adl_work });
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = overlay.own_pending_delta();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rb, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    if l1 {
        assert_eq!(holder.rows_builds(), 1, "R built once");
        assert_eq!(holder.rows_shared_fallbacks(), 0);
    }
    out
}

/// P3: 6 seeds x 60 blocks — L1 == the walk at every L1 valuation, and the
/// L1 run's results, liquidation / position / balance rows and
/// `liquidations_triggered` equal the reference walk's block by block.
#[test]
fn liquidation_l1_equals_reference_walk_on_seeded_sequences() {
    let mut stats = Stats::default();
    let mut drain_blocks = 0;
    for seed in 1..=6u64 {
        let with_l1 = run(seed * 0x517C_C1B7, true, &mut stats);
        let reference = run(seed * 0x517C_C1B7, false, &mut Stats::default());
        for (h, (a, b)) in with_l1.iter().zip(reference.iter()).enumerate() {
            assert_eq!(a.results, b.results, "seed {seed} block {}: results", h + 1);
            assert_eq!(a.markets, b.markets, "seed {seed} block {}: margin configs / listed markets", h + 1);
            assert!(a.rows == b.rows, "seed {seed} block {}: liquidation / position / balance rows", h + 1);
            assert_eq!(a.triggered, b.triggered, "seed {seed} block {}: liquidations_triggered", h + 1);
            assert_eq!(a.adl_work, b.adl_work, "seed {seed} block {}: ADL drain work units", h + 1);
            let prev = if h == 0 { 0 } else { with_l1[h - 1].adl_work };
            drain_blocks += usize::from(a.adl_work > prev);
        }
    }
    println!("LIQ_L1 P3 {stats:?} drain_blocks={drain_blocks}");
    assert!(drain_blocks >= 6, "blocks with ADL drain work: {drain_blocks}");
    let s = &stats;
    assert!(s.stage1 > 20 && s.backstop > 20 && s.adl > 5 && s.vault_adl > 0, "every class met: {s:?}");
    assert!(s.skipped > 100 && s.cuts > 10 && s.cooldowns > 5, "skips, cursor cuts, chunks: {s:?}");
    assert!(s.cooldown_stage1 > 5, "stage 1 inside the cooldown (entire-position orders): {s:?}");
    assert!(
        s.multi_same_block >= 6 * MULTI.len(),
        "rule B: two chunks of one account in one block, every MULTI account: {s:?}"
    );
    assert!(s.l1 > 1_000 && s.shadow > 1_000, "L1 valuations checked: {s:?}");
    assert_eq!(s.traders_slice, 6 * BLOCKS as usize + s.adl_rankings, "E2: pass + ADL rankings from the slot: {s:?}");
    assert!(s.adl_rankings > 0, "ADL rankings met: {s:?}");
    assert!(s.l1_off > 50 && s.delisted_marked_blocks >= 6 * 5, "delisted market with a fresh mark: L1 off: {s:?}");
    assert!(s.marks_off_blocks >= 6 * 4, "marks off: {s:?}");
    assert!(s.persistent > 100 && s.memo > 100 && s.dirty > 100, "every cache path used by the walk: {s:?}");
}

/// adl-budget Q2 (A6 #3): one ranking per (block, market, side). The P2
/// fixture's shape over the node path (R and the slot attached): u1 and u2
/// long 1 @ 1,000 in markets 1..=4 (collateral 100: AV -300 at 900) against
/// a short; block 2 at 900 leaves 8 rows over 4 (market, long) keys, and
/// the drain (default W) ranks each key once: 4 rankings, not 8.
#[test]
fn p2_ranks_each_market_side_once_per_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=4u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(5)).unwrap();
    }
    {
        let ctx = NativeExecContext::new(db.clone(), 1, 1_000, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        let fund = |t: &Address, v: i64| {
            ctx.positions.put_native_balance(t, &NativeBalance { available: fp(v), order_margin: FixedPoint::ZERO }).unwrap();
        };
        let (u1, u2, s) = (trader(1), trader(2), trader(3));
        fund(&u1, 100);
        fund(&u2, 100);
        fund(&s, 10_000_000);
        for m in 1..=4 {
            for (t, is_long) in [(u1, true), (u2, true), (s, false), (s, false)] {
                ctx.positions.apply_fill(&t, m, is_long, fp(1), fp(MID), MarginType::Cross).unwrap();
            }
        }
    }
    let mut holder = ResidentBooks::default();
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut rankings = Vec::new();
    for (h, price) in [(1u64, MID), (2, 900)] {
        let now = 10_000 + h;
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rb = begin_resident(Some(&mut holder), &mut overlay, h, None);
        for m in 1..=4 {
            overlay.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(fp(price), now - 1)).unwrap();
        }
        let mut ctx =
            NativeExecContext::new(overlay.clone(), h, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        ctx.attach_resident_block(&mut rb);
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let c = &ctx.sums.as_ref().expect("slot sums attached").counters;
        rankings.push(c.adl_rankings.load(std::sync::atomic::Ordering::Relaxed));
        if h == 2 {
            assert!(liq::next_obligation(&ctx.state, &[liq::ADL_OBLIGATION_TAG]).unwrap().is_none(), "drained in B");
        }
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = overlay.own_pending_delta();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rb, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).unwrap();
        }
        parent = Some(frozen);
    }
    assert_eq!(rankings, vec![0, 4], "block 2: one ranking per (market, long)");
}
