//! Item 6 P1 / P3 (exact per-fill fixes): the merged [`AccountMargins`]
//! entry and the hoisted taker invariants must give the same running free
//! margins and fill decisions as before.
//!
//! - `p1_account_margins_match_the_pre_p1_maps`: random operation sequences
//!   on the live [`AccountMargins`] and on a verbatim copy of the pre-P1
//!   three-map implementation (`old`), compared after every operation.
//! - `p1_p3_random_book_golden`: random books, positions, accounts, makers
//!   and placements (GTC / IOC / FOK / market, checked and unchecked
//!   takers); the digest of every result and of the final account / position
//!   state was recorded on the pre-P1 tree (b809d43).

use super::*;

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

/// xorshift64: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    /// Raw units in `[lo, hi)` whole units, plus a random fraction.
    fn fp_in(&mut self, lo: i64, hi: i64) -> FixedPoint {
        let span = (hi - lo) as u64 * FixedPoint::SCALE as u64;
        FixedPoint::from_raw(lo as i128 * FixedPoint::SCALE + self.below(span) as i128)
    }
}

fn tiers_of(k: u64) -> Option<std::sync::Arc<[crate::margin::MarginTier]>> {
    let t = |v: Vec<(i64, u32)>| {
        Some(std::sync::Arc::from(
            v.into_iter()
                .map(|(n, l)| crate::margin::MarginTier {
                    max_notional: if n < 0 { FixedPoint::MAX } else { fp(n) },
                    max_leverage: l,
                })
                .collect::<Vec<_>>(),
        ))
    };
    match k % 3 {
        0 => None,
        1 => t(vec![(1_000, 20), (-1, 5)]),
        _ => t(vec![(200, 50), (800, 10), (-1, 3)]),
    }
}

/// Verbatim pre-P1 `AccountMargins` (three maps), `load` and
/// `maker_fill_fits` — the reference.
mod old {
    use super::super::*;

    #[derive(Clone, Debug, Default)]
    pub struct OldAccountMargins {
        pub tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>,
        pub accounts: BTreeMap<Address, AccountMargin>,
        pub makers: BTreeMap<Address, AccountMargin>,
        pub taker_only: BTreeSet<Address>,
    }

    impl OldAccountMargins {
        pub fn new(tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>) -> Self {
            Self { tiers, accounts: BTreeMap::new(), makers: BTreeMap::new(), taker_only: BTreeSet::new() }
        }
        pub fn insert(&mut self, trader: Address, free: FixedPoint, px: FixedPoint) {
            self.accounts.insert(trader, AccountMargin { free, px });
        }
        pub fn insert_taker_only(&mut self, trader: Address, px: FixedPoint) {
            self.accounts.insert(trader, AccountMargin { free: FixedPoint::ZERO, px });
            self.taker_only.insert(trader);
        }
        fn shared(&self, trader: &Address) -> bool {
            self.accounts.contains_key(trader) && !self.taker_only.contains(trader)
        }
        pub fn get(&self, trader: &Address) -> Option<AccountMargin> {
            self.accounts.get(trader).copied()
        }
        pub fn set_free(&mut self, trader: &Address, free: FixedPoint) {
            if let Some(a) = self.accounts.get_mut(trader) {
                a.free = free;
            }
        }
        fn load(
            &mut self,
            trader: Address,
            market_id: MarketId,
            src: &dyn MakerAccountSource,
            ro: &mut ReduceOnlyPositions,
        ) -> AccountMargin {
            if self.shared(&trader) {
                return self.accounts[&trader];
            }
            if let Some(a) = self.makers.get(&trader) {
                return *a;
            }
            let m = src.maker_account(&trader, market_id);
            if ro.get(&trader).is_none() {
                ro.insert(trader, m.signed_pos);
            }
            let a = AccountMargin { free: m.free, px: m.px };
            self.makers.insert(trader, a);
            a
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn maker_fill_fits(
        accounts: &mut OldAccountMargins,
        ro: &mut ReduceOnlyPositions,
        src: &dyn MakerAccountSource,
        market_id: MarketId,
        maker: Address,
        maker_is_buy: bool,
        price: FixedPoint,
        q: FixedPoint,
        rem: FixedPoint,
    ) -> bool {
        let a = accounts.load(maker, market_id, src, ro);
        let s = ro.get(&maker).unwrap_or(FixedPoint::ZERO);
        let size = if s < FixedPoint::ZERO { -s } else { s };
        let closing = q.min(reduce_only_allowance(s, maker_is_buy));
        let px = if a.px > FixedPoint::ZERO { a.px } else { price };
        let t = accounts.tiers.clone();
        let t = t.as_deref();
        let delta = (|| {
            let before = size.checked_mul(px).ok()?;
            let after = (size - closing)
                .checked_mul(px)
                .ok()?
                .checked_add(price.checked_mul(q - closing).ok()?)
                .ok()?;
            let share = crate::margin::order_initial_margin(t, price.checked_mul(rem).ok()?)
                - crate::margin::order_initial_margin(t, price.checked_mul(rem - q).ok()?);
            Some(crate::margin::im_delta(t, before, after) - share)
        })();
        let delta = delta.map(|d| if d == FixedPoint::from_raw(1) { FixedPoint::ZERO } else { d });
        match delta {
            Some(d) if closing == q || d <= a.free => {
                if accounts.shared(&maker) {
                    accounts.set_free(&maker, a.free - d);
                } else if let Some(e) = accounts.makers.get_mut(&maker) {
                    e.free = a.free - d;
                }
                true
            }
            _ => false,
        }
    }
}

/// Random maker accounts, fixed per trader for one source.
struct Src(Vec<MakerAccount>);

impl Src {
    fn random(rng: &mut Rng, n: u8) -> Self {
        Src((0..n)
            .map(|_| MakerAccount {
                free: if rng.chance(30) { -rng.fp_in(0, 60) } else { rng.fp_in(0, 400) },
                signed_pos: if rng.chance(40) { FixedPoint::ZERO } else { rng.fp_in(-30, 30) },
                px: if rng.chance(30) { FixedPoint::ZERO } else { rng.fp_in(90, 110) },
            })
            .collect())
    }
}

impl MakerAccountSource for Src {
    fn maker_account(&self, maker: &Address, _market_id: MarketId) -> MakerAccount {
        self.0[maker[0] as usize % self.0.len()]
    }
}

/// Whether `t`'s taker entry is a taker-only budget.
fn new_taker_only(am: &AccountMargins, t: &Address) -> bool {
    am.taker(t).1
}

#[test]
fn p1_account_margins_match_the_pre_p1_maps() {
    const N: u8 = 7;
    let (mut fits, mut cancels) = (0u32, 0u32);
    for seed in 1..=300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let tiers = tiers_of(rng.next());
        let src = Src::random(&mut rng, N);
        let mut new = AccountMargins::new(tiers.clone());
        let mut old = old::OldAccountMargins::new(tiers);
        let mut ro_new = ReduceOnlyPositions::new();
        for t in 0..N {
            if rng.chance(40) {
                let p = rng.fp_in(-20, 20);
                ro_new.insert(addr(t), p);
            }
        }
        let mut ro_old = ro_new.clone();
        for step in 0..120 {
            let t = addr(rng.below(N as u64) as u8);
            match rng.below(10) {
                0 => {
                    let (free, px) = (rng.fp_in(-100, 300), if rng.chance(30) { FixedPoint::ZERO } else { rng.fp_in(90, 110) });
                    new.insert(t, free, px);
                    old.insert(t, free, px);
                }
                1 => {
                    let px = rng.fp_in(90, 110);
                    new.insert_taker_only(t, px);
                    old.insert_taker_only(t, px);
                }
                2 => {
                    let free = rng.fp_in(-100, 300);
                    new.set_free(&t, free);
                    old.set_free(&t, free);
                }
                3 if rng.chance(20) => {
                    // The FOK pre-check works on clones.
                    new = new.clone();
                    old = old.clone();
                }
                _ => {
                    let is_buy = rng.chance(50);
                    let price = rng.fp_in(95, 105);
                    let q = rng.fp_in(0, 15) + FixedPoint::from_raw(1);
                    let rem = q + if rng.chance(50) { FixedPoint::ZERO } else { rng.fp_in(0, 20) };
                    let a = maker_fill_fits(&mut new, &mut ro_new, &src, 1, t, is_buy, price, q, rem);
                    let b = old::maker_fill_fits(&mut old, &mut ro_old, &src, 1, t, is_buy, price, q, rem);
                    assert_eq!(a, b, "seed {seed} step {step}: maker_fill_fits");
                    if a {
                        fits += 1;
                        // The book advances the position through the fill.
                        ro_new.apply_fill(&t, is_buy, q);
                        ro_old.apply_fill(&t, is_buy, q);
                    } else {
                        cancels += 1;
                    }
                }
            }
            for u in 0..N {
                let u = addr(u);
                assert_eq!(new.get(&u), old.get(&u), "seed {seed} step {step}: get");
                assert_eq!(new_taker_only(&new, &u), old.taker_only.contains(&u), "seed {seed} step {step}: taker_only");
                assert_eq!(ro_new.get(&u), ro_old.get(&u), "seed {seed} step {step}: ro");
            }
        }
    }
    assert!(fits > 2_000 && cancels > 2_000, "fits {fits} cancels {cancels}");
}

fn fnv(h: &mut u64, s: &str) {
    for b in s.bytes() {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
}

/// One random scenario's digest and outcome counts.
fn random_book_run(seed: u64, h: &mut u64, counts: &mut [u32; 6]) {
    const N: u8 = 8;
    let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
    let tick = FixedPoint::from_raw(FixedPoint::SCALE / 100);
    let lot = FixedPoint::from_raw(FixedPoint::SCALE / 10);
    let on_tick = |p: FixedPoint| FixedPoint::from_raw(p.raw() / tick.raw() * tick.raw());
    let on_lot = |q: FixedPoint| FixedPoint::from_raw((q.raw() / lot.raw()).max(1) * lot.raw());
    let mut b = OrderBook::new(1, tick, lot);
    let src = Src::random(&mut rng, N);
    let mut ts = 1u64;
    let place = |b: &mut OrderBook, rng: &mut Rng, mid: i64, ts: &mut u64| {
        let is_buy = rng.chance(50);
        let off = rng.fp_in(0, 4);
        let price = on_tick(if is_buy { fp(mid) - off } else { fp(mid) + off });
        let p = PlaceOrderParams {
            market_id: 1,
            is_buy,
            price,
            quantity: on_lot(rng.fp_in(0, 20)),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: rng.chance(10),
            client_order_id: None,
        };
        *ts += 1;
        b.place_order(p, addr(rng.below(N as u64) as u8), *ts);
    };
    for _ in 0..30 {
        place(&mut b, &mut rng, 100, &mut ts);
    }
    for batch in 0..6 {
        let tiers = tiers_of(rng.next());
        let mut ro = ReduceOnlyPositions::new();
        for t in b.reduce_only_traders() {
            ro.insert(t, rng.fp_in(-15, 15));
        }
        let mut am = AccountMargins::new(tiers.clone());
        for t in 0..N {
            if rng.chance(70) {
                ro.insert(addr(t), if rng.chance(30) { FixedPoint::ZERO } else { rng.fp_in(-25, 25) });
            }
            let px = if rng.chance(25) { FixedPoint::ZERO } else { rng.fp_in(95, 105) };
            match rng.below(3) {
                0 => am.insert(addr(t), if rng.chance(20) { -rng.fp_in(0, 50) } else { rng.fp_in(0, 300) }, px),
                1 => am.insert_taker_only(addr(t), px),
                _ => {}
            }
        }
        b.set_reduce_only_positions(ro);
        b.set_account_margins(am);
        for k in 0..12 {
            if rng.chance(30) {
                place(&mut b, &mut rng, 100, &mut ts);
                continue;
            }
            let sender = addr(rng.below(N as u64) as u8);
            let is_buy = rng.chance(50);
            let (order_type, tif) = match rng.below(5) {
                0 => (OrderType::Market, TimeInForce::IOC),
                1 => (OrderType::Limit, TimeInForce::IOC),
                2 => (OrderType::Limit, TimeInForce::FOK),
                _ => (OrderType::Limit, TimeInForce::GTC),
            };
            let off = rng.fp_in(0, 6);
            let price = on_tick(if is_buy { fp(100) + off } else { fp(100) - off });
            let qty = on_lot(rng.fp_in(0, 40));
            let p = PlaceOrderParams {
                market_id: 1,
                is_buy,
                price,
                quantity: qty,
                order_type,
                time_in_force: tif,
                reduce_only: rng.chance(8),
                client_order_id: None,
            };
            let hold = (tif == TimeInForce::GTC && !is_buy).then_some(price);
            let lim = rng.chance(75).then(|| TakerMarginLimit {
                budget: if rng.chance(15) { FixedPoint::ZERO } else { rng.fp_in(0, 150) },
                tiers: tiers.clone(),
                hold_price: hold,
            });
            let makers: Option<&dyn MakerAccountSource> = if rng.chance(85) { Some(&src) } else { None };
            ts += 1;
            let r = b.place_order_with_accounts(p, sender, ts, lim.as_ref(), makers);
            counts[0] += r.fills.len() as u32;
            counts[1] += r.margin_cancels.len() as u32;
            counts[2] += u32::from(r.status == OrderStatus::Rejected);
            counts[3] += u32::from(r.status == OrderStatus::Cancelled && lim.is_some());
            counts[4] += r.reduce_only_cuts.len() as u32;
            // s92: `margin_cut_price` is observability only and not in the
            // pre-P1 golden: it is left out of the hashed text.
            let r = format!("{:?}", PlaceResult { margin_cut_price: None, ..r }).replace(", margin_cut_price: None", "");
            fnv(h, &format!("{seed}/{batch}/{k} {r}"));
        }
        for t in 0..N {
            let t = addr(t);
            let a = b.account_margins();
            fnv(h, &format!("{:?} {} {:?}", a.get(&t), new_taker_only(a, &t), b.reduce_only_positions.get(&t)));
        }
        counts[5] += 1;
        b.clear_reduce_only_positions();
        b.clear_account_margins();
    }
    fnv(h, &format!("{:?} {:?} {}", b.best_bid(), b.best_ask(), b.order_count()));
}

#[test]
fn p1_p3_random_book_golden() {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut counts = [0u32; 6];
    for seed in 1..=400u64 {
        random_book_run(seed, &mut h, &mut counts);
    }
    println!("p1_p3 golden digest {h:#018x} counts [fills, margin_cancels, rejected, checked_cancelled, ro_cuts, batches] = {counts:?}");
    assert!(counts[0] > 5_000 && counts[1] > 500 && counts[2] > 200 && counts[3] > 500 && counts[4] > 50, "{counts:?}");
    assert_eq!(h, GOLDEN, "digest {h:#018x}");
}

/// Recorded on the pre-P1 tree (b809d43).
const GOLDEN: u64 = 0xde41_0947_173e_e9eb;
