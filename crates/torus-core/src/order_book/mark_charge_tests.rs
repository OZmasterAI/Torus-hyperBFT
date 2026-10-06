//! s94 option 1 (bad-debt route): a fill's loss against the mark is charged
//! at match time ([`crate::margin::mark_loss`], [`account_fill`],
//! [`TakerMarginLimit::affordable`]). Default 20x (IM 5%, MM 2.5%), mark
//! 100, quantities 10 unless stated: the charge starts beyond the fill's own
//! IM above its maintenance (buy > 102.63, sell < 97.62); fills near the
//! mark are checked exactly as without a mark.

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

fn params(is_buy: bool, price: FixedPoint, qty: FixedPoint, order_type: OrderType, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price,
        quantity: qty,
        order_type,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(is_buy: bool, price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    params(is_buy, price, qty, OrderType::Limit, TimeInForce::GTC)
}

fn filled(r: &PlaceResult) -> FixedPoint {
    r.fills.iter().fold(FixedPoint::ZERO, |a, f| a + f.quantity)
}

fn im(n: FixedPoint) -> FixedPoint {
    crate::margin::order_initial_margin(None, n)
}

/// Accounts by trader; anyone else: rich and flat.
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

fn flat(free: FixedPoint) -> MakerAccount {
    MakerAccount { free, signed_pos: FixedPoint::ZERO, px: fp(100) }
}

fn book() -> OrderBook {
    OrderBook::new(1, cents(1), fp(1))
}

/// Install `mark` (and nothing else) for the next placement.
fn with_mark(b: &mut OrderBook, mark: Option<FixedPoint>) {
    let mut am = AccountMargins::new(None);
    am.set_mark(mark);
    b.set_account_margins(am);
}

// ---------------------------------------------------------------------------
// makers
// ---------------------------------------------------------------------------

/// A maker bid 10 @103 whose account has `free` left after its reservation
/// (flat). Loss 30, tolerance IM(1,030) − MM(1,000) = 26.5: it needs 3.5 of
/// free margin. Free 0 → margin-cancelled whole, the taker moves on to the
/// bid at 102 (no charge: loss 20 <= 26); free 3.5 → filled, free ends 0.
/// No mark → filled (the pre-s94 check).
#[test]
fn a_maker_buying_above_the_mark_beyond_its_margin_is_cancelled() {
    for (mark, free, fills_at_103) in [
        (Some(fp(100)), FixedPoint::ZERO, false),
        (Some(fp(100)), cents(350), true),
        (Some(fp(100)), cents(349), false),
        (None, FixedPoint::ZERO, true),
    ] {
        let what = format!("mark {mark:?} free {free}");
        let mut b = book();
        let id = b.place_order(gtc(true, cents(10_300), fp(10)), addr(1), 1).order_id;
        b.place_order(gtc(true, cents(10_200), fp(10)), addr(3), 1);
        with_mark(&mut b, mark);
        let src = Src(vec![(addr(1), flat(free)), (addr(3), flat(FixedPoint::ZERO))]);
        let sell = params(false, fp(1), fp(10), OrderType::Market, TimeInForce::IOC);
        let r = b.place_order_with_accounts(sell, addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(10), "{what}");
        let maker = if fills_at_103 { addr(1) } else { addr(3) };
        assert!(r.fills.iter().all(|f| f.maker == maker), "{what}: {:?}", r.fills);
        if fills_at_103 {
            assert!(r.margin_cancels.is_empty(), "{what}");
        } else {
            assert_eq!(
                r.margin_cancels,
                vec![ReduceOnlyCut { order_id: id, trader: addr(1), price: cents(10_300), qty: fp(10) }],
                "{what}"
            );
        }
    }
}

/// The charge is committed to the maker's running free: after a charged
/// fill with free 3.5 the maker's entry holds 0, so its next charged fill in
/// the same book (another bid at 103) is cancelled.
#[test]
fn a_makers_charge_comes_off_its_running_free() {
    let mut b = book();
    b.place_order(gtc(true, cents(10_300), fp(10)), addr(1), 1);
    b.place_order(gtc(true, cents(10_300), fp(10)), addr(1), 2);
    with_mark(&mut b, Some(fp(100)));
    let src = Src(vec![(addr(1), flat(cents(350)))]);
    let sell = params(false, fp(1), fp(20), OrderType::Market, TimeInForce::IOC);
    let r = b.place_order_with_accounts(sell, addr(2), 3, None, Some(&src));
    assert_eq!(filled(&r), fp(10));
    assert_eq!(r.margin_cancels.len(), 1);
}

// ---------------------------------------------------------------------------
// checked takers (TakerMarginLimit)
// ---------------------------------------------------------------------------

/// IOC buy 10 @103 against an ask 10 @103, budget = its reservation
/// IM(1,030) = 51.5 + free. Per unit: IM 5.15 + loss 3 − tolerance 2.65 =
/// 5.5, so free 0 fills 9 (49.5 <= 51.5; the account stays >= MM) and is
/// cut at 103; free 3.5 fills 10 and the account's free ends 0. At 102 (no
/// charge) and without a mark it fills 10 as before.
#[test]
fn a_checked_taker_buying_through_the_mark_is_cut_to_what_its_margin_covers() {
    for (mark, ask, free, fills) in [
        (Some(fp(100)), 10_300, FixedPoint::ZERO, 9),
        (Some(fp(100)), 10_300, cents(350), 10),
        (Some(fp(100)), 10_200, FixedPoint::ZERO, 10),
        (None, 10_300, FixedPoint::ZERO, 10),
    ] {
        let what = format!("mark {mark:?} ask {ask} free {free}");
        let mut b = book();
        b.place_order(gtc(false, cents(ask), fp(10)), addr(1), 1);
        let mut ro = ReduceOnlyPositions::new();
        ro.insert(addr(2), FixedPoint::ZERO);
        b.set_reduce_only_positions(ro);
        let mut am = AccountMargins::new(None);
        am.set_mark(mark);
        am.insert(addr(2), free, fp(100));
        b.set_account_margins(am);
        let reserved = im(cents(ask) * fp(10));
        let lim = TakerMarginLimit { budget: reserved, tiers: None, hold_price: None };
        let ioc = params(true, cents(ask), fp(10), OrderType::Limit, TimeInForce::IOC);
        let r = b.place_order_with_accounts(ioc, addr(2), 2, Some(&lim), Some(&Src(vec![])));
        assert_eq!(filled(&r), fp(fills), "{what}");
        if fills < 10 {
            assert_eq!(r.status, OrderStatus::Cancelled, "{what}");
            assert_eq!(r.margin_cut_price, Some(cents(ask)), "{what}");
        }
        if free == cents(350) {
            assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, FixedPoint::ZERO, "{what}");
        }
    }
}

/// A flip does not count the released IM twice. Long 10 at the mark 100
/// (free 0), IOC sell 11 @95 into a bid 11 @95 (budget: the opening unit's
/// reservation 4.75). The 10 closing units lose 50 = the IM they release
/// (fits, equity ends 0); the 11th would open at a loss: refused. Without
/// the charge all 11 fill.
#[test]
fn a_closing_fill_pays_its_loss_from_the_released_im_only() {
    for (mark, fills) in [(Some(fp(100)), 10), (None, 11)] {
        let mut b = book();
        b.place_order(gtc(true, fp(95), fp(11)), addr(1), 1);
        let mut ro = ReduceOnlyPositions::new();
        ro.insert(addr(2), fp(10));
        b.set_reduce_only_positions(ro);
        let mut am = AccountMargins::new(None);
        am.set_mark(mark);
        am.insert(addr(2), FixedPoint::ZERO, fp(100));
        b.set_account_margins(am);
        let lim = TakerMarginLimit { budget: im(fp(95)), tiers: None, hold_price: None };
        let ioc = params(false, fp(95), fp(11), OrderType::Limit, TimeInForce::IOC);
        let r = b.place_order_with_accounts(ioc, addr(2), 2, Some(&lim), Some(&Src(vec![])));
        assert_eq!(filled(&r), fp(fills), "mark {mark:?}");
    }
}

/// FOK buy 10 @103, budget 51.5 + free: the complete fill needs 55 (IM
/// 51.5 + loss 30 − tolerance 26.5). Free 0 → rejected whole (the ask
/// stays); free 3.5 → filled, free ends 0.
#[test]
fn a_fok_is_judged_on_its_complete_fills_loss() {
    for (free, fills) in [(FixedPoint::ZERO, 0), (cents(350), 10)] {
        let mut b = book();
        b.place_order(gtc(false, cents(10_300), fp(10)), addr(1), 1);
        let mut ro = ReduceOnlyPositions::new();
        ro.insert(addr(2), FixedPoint::ZERO);
        b.set_reduce_only_positions(ro);
        let mut am = AccountMargins::new(None);
        am.set_mark(Some(fp(100)));
        am.insert(addr(2), free, fp(100));
        b.set_account_margins(am);
        let lim = TakerMarginLimit { budget: cents(5_150), tiers: None, hold_price: None };
        let fok = params(true, cents(10_300), fp(10), OrderType::Limit, TimeInForce::FOK);
        let r = b.place_order_with_accounts(fok, addr(2), 2, Some(&lim), Some(&Src(vec![])));
        assert_eq!(filled(&r), fp(fills), "free {free}");
        if fills == 0 {
            assert_eq!(r.status, OrderStatus::Rejected);
            assert_eq!(b.best_ask(), Some(cents(10_300)));
        } else {
            assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, FixedPoint::ZERO);
        }
    }
}

// ---------------------------------------------------------------------------
// unchecked takers (no TakerMarginLimit: GTC buys, reduce-only)
// ---------------------------------------------------------------------------

/// GTC buy 10 @103 taking an ask 10 @103 (the executor does not match-check
/// GTC buys: their reservation at the limit covers the fill's IM). Its
/// snapshot free (after its reservation) must pay 0.35 a unit: free 0 → no
/// fill, the order is cancelled (it does not rest crossed); free 1.75 → 5
/// fill, the rest is cancelled; no mark → 10 fill.
#[test]
fn an_unchecked_gtc_buy_through_the_mark_is_cut_and_does_not_rest() {
    for (mark, free, fills) in [
        (Some(fp(100)), FixedPoint::ZERO, 0),
        (Some(fp(100)), cents(175), 5),
        (Some(fp(100)), cents(350), 10),
        (None, FixedPoint::ZERO, 10),
    ] {
        let what = format!("mark {mark:?} free {free}");
        let mut b = book();
        b.place_order(gtc(false, cents(10_300), fp(10)), addr(1), 1);
        with_mark(&mut b, mark);
        let src = Src(vec![(addr(2), flat(free))]);
        let r = b.place_order_with_accounts(gtc(true, cents(10_300), fp(10)), addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(fills), "{what}");
        assert_eq!(r.rested_qty, FixedPoint::ZERO, "{what}");
        if fills < 10 {
            assert_eq!(r.status, OrderStatus::Cancelled, "{what}");
            assert_eq!(r.margin_cut_price, Some(cents(10_300)), "{what}");
        }
    }
}

/// Reduce-only IOC buy 10 closing a short 10 (valued at 100) into an ask
/// @106: loss 60, released IM 50. Free 0 → no fill; free 10 → filled. @104
/// (loss 40 < 50) it fills even under water (free −100): the account does
/// not get worse. @102 (loss 20 <= tolerance 25): no charge, no check.
#[test]
fn a_reduce_only_close_pays_its_loss_from_the_released_im() {
    for (ask, free, fills) in [
        (106, FixedPoint::ZERO, 0),
        (106, fp(10), 10),
        (104, -fp(100), 10),
        (102, -fp(1_000), 10),
    ] {
        let what = format!("ask {ask} free {free}");
        let mut b = book();
        b.place_order(gtc(false, fp(ask), fp(10)), addr(1), 1);
        let mut ro = ReduceOnlyPositions::new();
        ro.insert(addr(2), -fp(10));
        b.set_reduce_only_positions(ro);
        with_mark(&mut b, Some(fp(100)));
        let src = Src(vec![(addr(2), MakerAccount { free, signed_pos: -fp(10), px: fp(100) })]);
        let mut p = params(true, fp(ask), fp(10), OrderType::Limit, TimeInForce::IOC);
        p.reduce_only = true;
        let r = b.place_order_with_accounts(p, addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(fills), "{what}");
    }
}

/// Reduce-only FOK (unchecked): the walk checks its charged fills like
/// matching — short 10, buy @106, free 0 → rejected whole; free 10 → filled.
#[test]
fn an_unchecked_fok_is_rejected_whole_when_its_charge_does_not_fit() {
    for (free, fills) in [(FixedPoint::ZERO, 0), (fp(10), 10)] {
        let mut b = book();
        b.place_order(gtc(false, fp(106), fp(10)), addr(1), 1);
        let mut ro = ReduceOnlyPositions::new();
        ro.insert(addr(2), -fp(10));
        b.set_reduce_only_positions(ro);
        with_mark(&mut b, Some(fp(100)));
        let src = Src(vec![(addr(2), MakerAccount { free, signed_pos: -fp(10), px: fp(100) })]);
        let mut p = params(true, fp(106), fp(10), OrderType::Limit, TimeInForce::FOK);
        p.reduce_only = true;
        let r = b.place_order_with_accounts(p, addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(fills), "free {free}");
        assert_eq!(b.best_ask().is_some(), fills == 0, "free {free}");
    }
}

// ---------------------------------------------------------------------------
// near the mark: exactly the pre-s94 outcome
// ---------------------------------------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One run of a random flow around 30,000 (prices mid ± 1..5 ticks of 1,
/// market caps ± 60, GTC / IOC / FOK / market / reduce-only, makers and
/// takers with thin to rich accounts, checked takers with and without an
/// account entry). Returns every placement's result and the final
/// account entries.
fn near_mark_run(mark: Option<FixedPoint>, seed: u64) -> (Vec<String>, Vec<Option<AccountMargin>>, usize) {
    let mut rng = Lcg(seed);
    let mut b = OrderBook::new(1, fp(1), FixedPoint::ONE / FixedPoint::from_raw(2 * FixedPoint::SCALE));
    let traders: Vec<Address> = (1..=12).map(addr).collect();
    let src = Src(
        traders
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let free = match i % 4 {
                    0 => fp(1),
                    1 => fp(800),
                    2 => -fp(50),
                    _ => fp(1_000_000),
                };
                (*t, MakerAccount { free, signed_pos: fp(i as i64 % 3 - 1), px: fp(30_000) })
            })
            .collect(),
    );
    let mut out = Vec::new();
    let mut fills = 0;
    for step in 0..400u64 {
        let t = traders[rng.below(traders.len() as u64) as usize];
        let is_buy = rng.below(2) == 0;
        let d = 1 + rng.below(5) as i64;
        let aggressive = rng.below(2) == 0;
        let px = if is_buy == aggressive { 30_000 + d } else { 30_000 - d };
        let qty = FixedPoint::ONE + FixedPoint::from_raw(rng.below(3) as i128 * FixedPoint::SCALE / 2);
        let kind = rng.below(10);
        let mut p = gtc(is_buy, fp(px), qty);
        match kind {
            0 => {
                p.order_type = OrderType::Market;
                p.time_in_force = TimeInForce::IOC;
                p.price = fp(if is_buy { 30_060 } else { 29_940 });
            }
            1 => p.time_in_force = TimeInForce::IOC,
            2 => p.time_in_force = TimeInForce::FOK,
            3 => p.reduce_only = true,
            _ => {}
        }
        let mut ro = ReduceOnlyPositions::new();
        for (i, tr) in traders.iter().enumerate() {
            ro.insert(*tr, fp(i as i64 % 3 - 1));
        }
        b.set_reduce_only_positions(ro);
        let mut am = AccountMargins::new(None);
        am.set_mark(mark);
        let checked = !p.reduce_only && (!is_buy || kind <= 2);
        if checked && rng.below(2) == 0 {
            am.insert(t, fp(rng.below(3_000) as i64) - fp(500), fp(30_000));
        } else if checked {
            am.insert_taker_only(t, fp(30_000));
        }
        b.set_account_margins(am);
        let reserved = im(p.price * qty);
        let lim = TakerMarginLimit {
            budget: reserved,
            tiers: None,
            hold_price: (p.order_type == OrderType::Limit && p.time_in_force == TimeInForce::GTC).then_some(p.price),
        };
        let r = b.place_order_with_accounts(p, t, step, checked.then_some(&lim), Some(&src));
        fills += r.fills.len();
        out.push(format!("{r:?}"));
    }
    let entries = traders.iter().map(|t| b.account_margins().get(t)).collect();
    (out, entries, fills)
}

/// Near the mark (fills within 5 ticks of 30,000, cap 60 = 0.2%) the charge
/// is always 0: every placement result and the account entries equal the
/// run without a mark, at a mark of 30,000 and at marks moved by ±5 ticks.
#[test]
fn fills_within_a_few_ticks_of_the_mark_are_unchanged() {
    for seed in [1u64, 7, 94, 2026] {
        let base = near_mark_run(None, seed);
        assert!(base.2 > 100, "seed {seed}: the flow fills ({})", base.2);
        for mark in [30_000, 29_995, 30_005] {
            assert_eq!(near_mark_run(Some(fp(mark)), seed), base, "seed {seed} mark {mark}");
        }
    }
}
