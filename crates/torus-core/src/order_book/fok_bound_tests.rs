//! s80 adversarial-worst-block (`TORUS_S80_FOK_BOUND`): the FOK pre-check
//! with the flag on (exact quantity early reject + copy-on-first-touch
//! scratch maps instead of whole-map clones when the maps are large for the
//! walk) must give byte-identical results to the flag off (the 29bfc705
//! pre-check), with the default scratch ratio and with the scratch forced
//! on every walk (ratio 0) — every placement's
//! result, the live account / reduce-only maps after it, and every resting
//! order row — over randomized multi-placement scenarios that exercise
//! every pre-check branch: quantity above the depth, self-skipped depth,
//! margin-skipped makers (phantom depth), reduce-only makers, checked takers
//! (with and without a mark charge), unchecked takers (mark, pre-batch
//! snapshots, D2 taker-only budgets) and FOKs that fill.

use super::*;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn cents(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 100))
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

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
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// Makers' account snapshots by trader (random per scenario).
struct Src(Vec<(Address, MakerAccount)>);
impl MakerAccountSource for Src {
    fn maker_account(&self, maker: &Address, _market_id: MarketId) -> MakerAccount {
        self.0.iter().find(|(a, _)| a == maker).map(|(_, m)| *m).unwrap_or(MakerAccount {
            free: fp(1_000_000),
            signed_pos: FixedPoint::ZERO,
            px: FixedPoint::ZERO,
        })
    }
}

/// A deterministic transcript of one scenario: every result and the live
/// state after it (maps sorted: their iteration order is per-process).
fn transcript(seed: u64, bound: bool, ratio: Option<usize>) -> (Vec<String>, FokBoundStats) {
    let _g = FokBoundOverride::set_ratio(bound, ratio);
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut b = OrderBook::new(1, cents(1), fp(1));
    let traders: Vec<Address> = (1..=10).map(addr).collect();
    let tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>> = if r.chance(50) {
        Some(std::sync::Arc::from(crate::margin::default_margin_tiers().as_slice()))
    } else {
        None
    };
    let mark = r.chance(70).then(|| fp(95 + r.below(11) as i64));
    let src = Src(
        traders
            .iter()
            .map(|&t| {
                let free = match r.below(4) {
                    0 => -fp(r.below(200) as i64),
                    1 => fp(r.below(30) as i64),
                    _ => fp(1_000_000),
                };
                let signed_pos = fp(r.below(41) as i64 - 20);
                let px = if r.chance(50) { FixedPoint::ZERO } else { fp(95 + r.below(11) as i64) };
                (t, MakerAccount { free, signed_pos, px })
            })
            .collect(),
    );
    let install = |b: &mut OrderBook, r: &mut Rng| {
        let mut ro = ReduceOnlyPositions::new();
        for &t in &traders {
            if r.chance(60) {
                ro.insert(t, fp(r.below(41) as i64 - 20));
            }
        }
        b.set_reduce_only_positions(ro);
        let mut am = AccountMargins::new(tiers.clone());
        am.set_mark(mark);
        am.set_pre_batch_snapshots(r.chance(50));
        for &t in &traders {
            match r.below(4) {
                0 => am.insert(t, fp(r.below(400) as i64 - 50), fp(95 + r.below(11) as i64)),
                1 => am.insert_taker_only(t, if r.chance(50) { FixedPoint::ZERO } else { fp(100) }),
                _ => {}
            }
        }
        b.set_account_margins(am);
    };
    let mut out = Vec::new();
    let snapshot = |b: &OrderBook, out: &mut Vec<String>| {
        let mut ro: Vec<(Address, FixedPoint)> =
            b.reduce_only_positions.positions.iter().map(|(a, p)| (*a, *p)).collect();
        ro.sort();
        let mut acc: Vec<String> =
            b.account_margins.traders.iter().map(|(a, e)| format!("{a}:{e:?}")).collect();
        acc.sort();
        out.push(format!(
            "ro {ro:?} acc {acc:?} charged {} bbo {:?} n {}",
            b.account_margins.charged_maker_fills,
            b.bbo_row(),
            b.order_count()
        ));
    };
    let mut ts = 0u64;
    // Resting book: both sides, every trader (so takers have own orders to
    // skip), some reduce-only (policed against the installed positions).
    install(&mut b, &mut r);
    for _ in 0..(20 + r.below(40)) {
        let is_buy = r.chance(50);
        let px = if is_buy { 90 + r.below(10) } else { 101 + r.below(10) };
        let mut p = PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: fp(px as i64),
            quantity: fp(1 + r.below(6) as i64),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: r.chance(15),
            client_order_id: None,
        };
        if r.chance(10) {
            p.price = p.price + cents(r.below(99) as i64 + 1);
        }
        let t = traders[r.below(traders.len() as u64) as usize];
        ts += 1;
        let res = b.place_order(p, t, ts);
        out.push(format!("rest {res:?}"));
    }
    snapshot(&b, &mut out);
    for step in 0..(30 + r.below(30)) {
        if step % 7 == 0 {
            install(&mut b, &mut r);
        }
        let t = traders[r.below(traders.len() as u64) as usize];
        let is_buy = r.chance(50);
        // Limits from inside the spread to through the whole other side.
        let px = if is_buy { 99 + r.below(14) } else { 88 + r.below(14) };
        let tif = match r.below(10) {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            _ => TimeInForce::FOK,
        };
        let span = if r.chance(30) { 80 } else { 12 };
        let p = PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: fp(px as i64),
            quantity: fp(1 + r.below(span) as i64),
            order_type: if r.chance(10) { OrderType::Market } else { OrderType::Limit },
            time_in_force: tif,
            reduce_only: r.chance(15),
            client_order_id: None,
        };
        let budget = match r.below(4) {
            0 => FixedPoint::ZERO,
            1 => fp(r.below(20) as i64),
            _ => fp(r.below(5_000) as i64),
        };
        let limit = TakerMarginLimit {
            budget,
            tiers: tiers.clone(),
            hold_price: (tif == TimeInForce::GTC && !is_buy).then_some(p.price),
        };
        let margin = r.chance(60).then_some(&limit);
        let makers: Option<&dyn MakerAccountSource> = if r.chance(80) { Some(&src) } else { None };
        ts += 1;
        let desc = format!("place {step} {t} {p:?} m {} k {}", margin.is_some(), makers.is_some());
        let res = b.place_order_with_accounts(p, t, ts, margin, makers);
        out.push(format!("{desc} -> {res:?}"));
        snapshot(&b, &mut out);
    }
    // Every order row (ids are allocated sequentially from 0).
    let max_id = b.next_id;
    for id in 0..max_id {
        if let Some(row) = b.encode_order_row(id) {
            out.push(format!("row {id} {row:?}"));
        }
    }
    (out, FokBoundStats::take())
}

/// The flag-on pre-check is byte-identical to the flag-off one over 400
/// random scenarios — with the scratch forced on every walk (ratio 0) and
/// with the default ratio (the small maps here: mostly the clone) — and the
/// scenarios reach every pre-check outcome.
#[test]
fn fok_bound_is_byte_identical_to_the_clone_precheck() {
    let mut total_scratch = FokBoundStats::default();
    let mut total_hybrid = FokBoundStats::default();
    let mut total_off = FokBoundStats::default();
    let (mut fok_fills, mut fok_cancels) = (0usize, 0usize);
    for seed in 0..400u64 {
        let (off, s_off) = transcript(seed, false, None);
        for (ratio, total) in [(Some(0), &mut total_scratch), (None, &mut total_hybrid)] {
            let (on, s_on) = transcript(seed, true, ratio);
            assert_eq!(off.len(), on.len(), "seed {seed} ratio {ratio:?}: transcript length");
            for (i, (a, b)) in off.iter().zip(&on).enumerate() {
                assert_eq!(a, b, "seed {seed} ratio {ratio:?}: line {i} differs");
            }
            total.add(&s_on);
        }
        fok_fills += off.iter().filter(|l| l.contains("FOK") && l.contains("status: Filled")).count();
        fok_cancels += off.iter().filter(|l| l.contains("Some(FokCancel)")).count();
        total_off.add(&s_off);
    }
    println!(
        "s80 fok_bound differential: scratch {total_scratch:?}; hybrid {total_hybrid:?}; FOK fills {fok_fills}, FOK cancels {fok_cancels}"
    );
    // Flag off never takes the new paths; flag on takes both, and FOKs that
    // pass the quantity bound but fail the walk (phantom depth, margin) too.
    assert_eq!(total_off, FokBoundStats::default(), "flag off: no new path");
    for t in [&total_scratch, &total_hybrid] {
        assert!(t.early_rejects > 50, "quantity early rejects: {t:?}");
        assert!(t.walk_rejects > 20, "rejects after a walk (phantom depth / margin): {t:?}");
    }
    assert!(total_scratch.scratch_walks > 50, "copy-on-first-touch walks: {total_scratch:?}");
    assert_eq!(total_scratch.clone_walks, 0, "ratio 0: every walk on scratch maps: {total_scratch:?}");
    assert!(total_hybrid.clone_walks > 50, "default ratio, small maps: clone walks: {total_hybrid:?}");
    assert!(fok_fills > 50, "FOK fills: {fok_fills}");
    assert!(fok_cancels > 100, "FOK cancels: {fok_cancels}");
}

/// The flag-on walk takes the scratch maps when the installed maps hold
/// more than `FOK_SCRATCH_RATIO` entries per order the quantity bound
/// counted (a thin FOK over a large map: no O(map) clone), else the clone
/// (a deep walk over a small map: no per-visit seeding); both give the
/// flag-off result.
#[test]
fn fok_bound_picks_scratch_for_thin_walks_over_large_maps() {
    let run = |bound: bool, entries: u32, qty: i64| {
        let _g = FokBoundOverride::set(bound);
        let mut b = OrderBook::new(1, cents(1), fp(1));
        for i in 0..40u8 {
            b.place_order(gtc(false, fp(101 + i64::from(i % 4)), fp(1)), addr(20 + i), u64::from(i));
        }
        let mut am = AccountMargins::new(None);
        am.set_mark(Some(fp(100)));
        let mut ro = ReduceOnlyPositions::new();
        for i in 0..entries {
            let mut a = [7u8; 20];
            a[..4].copy_from_slice(&i.to_be_bytes());
            am.insert(Address::new(a), fp(10), fp(100));
            ro.insert(Address::new(a), fp(1));
        }
        am.insert(addr(9), fp(1_000_000), fp(100));
        b.set_account_margins(am);
        b.set_reduce_only_positions(ro);
        let src = Src(Vec::new());
        let p = PlaceOrderParams { time_in_force: TimeInForce::FOK, ..gtc(true, fp(104), fp(qty)) };
        let r = b.place_order_with_accounts(p, addr(9), 100, None, Some(&src));
        (format!("{r:?}"), FokBoundStats::take())
    };
    // Thin: 2 orders counted, 2 * 16 < 2 * 1,000 + 1 entries -> scratch.
    let (off, _) = run(false, 1_000, 2);
    let (on, s) = run(true, 1_000, 2);
    assert_eq!(off, on);
    assert!(off.contains("status: Filled"), "{off}");
    assert_eq!((s.scratch_walks, s.clone_walks), (1, 0), "{s:?}");
    // Deep: 30 orders counted, 30 * 16 > 2 * 10 + 1 entries -> clone.
    let (off, _) = run(false, 10, 30);
    let (on, s) = run(true, 10, 30);
    assert_eq!(off, on);
    assert!(off.contains("status: Filled"), "{off}");
    assert_eq!((s.scratch_walks, s.clone_walks), (0, 1), "{s:?}");
}

/// The quantity bound counts only other traders' resting quantity up to the
/// limit: a FOK that needs the taker's own order (or a level beyond its
/// limit) is rejected without a walk; one that fits exactly is walked and
/// fills.
#[test]
fn fok_bound_quantity_bound_is_exact_at_the_edge() {
    for bound in [false, true] {
        let _g = FokBoundOverride::set_ratio(bound, Some(0));
        let mut b = OrderBook::new(1, cents(1), fp(1));
        b.place_order(gtc(false, fp(101), fp(3)), addr(1), 1);
        b.place_order(gtc(false, fp(102), fp(4)), addr(2), 2);
        b.place_order(gtc(false, fp(102), fp(5)), addr(9), 3); // the taker's own
        b.place_order(gtc(false, fp(103), fp(50)), addr(3), 4); // beyond the limit
        let src = Src(Vec::new());
        let fok = |q: i64| PlaceOrderParams { time_in_force: TimeInForce::FOK, ..gtc(true, fp(102), fp(q)) };
        let r = b.place_order_with_accounts(fok(8), addr(9), 5, None, Some(&src));
        assert_eq!(r.reject, Some(FailureReason::FokCancel), "bound {bound}");
        let s = FokBoundStats::take();
        assert_eq!(s.early_rejects, u64::from(bound), "bound {bound}: {s:?}");
        let r = b.place_order_with_accounts(fok(7), addr(9), 6, None, Some(&src));
        assert_eq!(r.status, OrderStatus::Filled, "bound {bound}");
        assert_eq!(r.fills.len(), 2);
        let s = FokBoundStats::take();
        assert_eq!((s.early_rejects, s.scratch_walks), (0, u64::from(bound)), "bound {bound}: {s:?}");
    }
}

/// Copy-on-first-touch: the scratch starts empty and only the walked
/// makers (and the taker) are copied in, so a pre-check over a few makers
/// does not depend on the size of the installed maps.
#[test]
fn fok_bound_scratch_copies_only_touched_traders() {
    let _g = FokBoundOverride::set_ratio(true, Some(0));
    let mut b = OrderBook::new(1, cents(1), fp(1));
    b.place_order(gtc(false, fp(101), fp(3)), addr(1), 1);
    b.place_order(gtc(false, fp(102), fp(4)), addr(2), 2);
    let mut am = AccountMargins::new(None);
    am.set_mark(Some(fp(100)));
    let mut ro = ReduceOnlyPositions::new();
    for i in 0..5_000u32 {
        let mut a = [7u8; 20];
        a[..4].copy_from_slice(&i.to_be_bytes());
        am.insert(Address::new(a), fp(10), fp(100));
        ro.insert(Address::new(a), fp(1));
    }
    am.insert(addr(9), fp(1_000), fp(100));
    b.set_account_margins(am);
    b.set_reduce_only_positions(ro);
    let src = Src(Vec::new());
    let p = PlaceOrderParams { time_in_force: TimeInForce::FOK, ..gtc(true, fp(102), fp(7)) };
    let r = b.place_order_with_accounts(p, addr(9), 3, None, Some(&src));
    assert_eq!(r.status, OrderStatus::Filled);
    let s = FokBoundStats::take();
    assert_eq!(s.scratch_walks, 1);
    // The taker and the two makers: nothing else is copied.
    assert!(s.scratch_entries_max <= 3, "{s:?}");
}

fn gtc(is_buy: bool, price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}
