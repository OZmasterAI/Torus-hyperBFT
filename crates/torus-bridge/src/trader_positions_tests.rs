//! Item 6 C7 (plan 5.2) P1: the decoded per-trader positions equal the
//! decoding of R's rows, trader by trader, after every random block delta,
//! and the records maintained warm (`apply`) equal a cold `build`.
//! Irregular rows (keys other than `trader ‖ market`, values that do not
//! decode or name another trader / market, short keys) never break it: their
//! trader reads through the overlay, as today.

use std::collections::BTreeSet;

use super::*;
use torus_core::position::{position_key, MarginType, PositionManager};
use torus_state::cf::CF_NATIVE_POSITIONS;
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId};

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

const TRADERS: u64 = 6;
const MARKETS: u64 = 8;

fn trader(i: u64) -> Address {
    let mut b = [0x3Cu8; 20];
    b[12..].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

fn position(t: Address, m: MarketId, raw: u64, long: bool) -> Position {
    Position {
        trader: t,
        market_id: m,
        is_long: long,
        size: FixedPoint::from_raw(i128::from(raw) + 1),
        entry_price: FixedPoint::from_raw(i128::from(raw) * 7 + 3),
        realized_pnl: FixedPoint::from_raw(-(i128::from(raw))),
        isolated_margin: FixedPoint::ZERO,
        margin_type: if raw.is_multiple_of(5) { MarginType::Isolated } else { MarginType::Cross },
    }
}

fn bytes(p: &Position) -> Vec<u8> {
    borsh::to_vec(p).unwrap()
}

#[derive(Default, Debug)]
struct Stats {
    regular: usize,
    opaque: usize,
    found: usize,
    cleared: usize,
    /// Item 6 step 1: traders reported with their changes / as re-decoded.
    reported: usize,
    reported_none: usize,
    changes: usize,
}

/// One trader's report of `apply` (owned copy).
type Report = (Address, Option<(Vec<Change>, Vec<Position>)>);

/// Item 6 step 1: `apply`'s reports for one block — exactly one per trader
/// prefix the delta writes under (key order); a trader reported with
/// changes was regular before (`before`) and after (`after`), its `now` is
/// its record after, and its changes compose its record before into it, each
/// change's `before` being its position before; a trader reported without
/// was opaque before or after.
fn check_reports(reports: &[Report], delta: &ResidentDelta, before: &TraderPositions, after: &TraderPositions, stats: &mut Stats, tag: &str) {
    let mut written: Vec<Address> =
        delta.entries(CF_NATIVE_POSITIONS).filter(|(k, _)| k.len() >= 20).map(|(k, _)| Address::from_slice(&k[..20])).collect();
    written.dedup();
    assert_eq!(reports.iter().map(|r| r.0).collect::<Vec<_>>(), written, "{tag}: one report per written trader");
    for (t, report) in reports {
        let Some((changes, now)) = report else {
            stats.reported_none += 1;
            assert!(before.get(t).is_none() || after.get(t).is_none(), "{tag}: {t} re-decoded while regular");
            continue;
        };
        stats.reported += 1;
        stats.changes += changes.len();
        let was = before.get(t).unwrap_or_else(|| panic!("{tag}: {t} reported but opaque before"));
        let is = after.get(t).unwrap_or_else(|| panic!("{tag}: {t} reported but opaque after"));
        assert_eq!(now.iter().map(bytes).collect::<Vec<_>>(), is.iter().map(bytes).collect::<Vec<_>>(), "{tag}: {t} now");
        let mut cur: std::collections::BTreeMap<MarketId, Vec<u8>> = was.iter().map(|p| (p.market_id, bytes(p))).collect();
        for (old, new) in changes {
            let m = old.as_ref().or(new.as_ref()).expect("a change has a side").market_id;
            assert_eq!(cur.get(&m), old.as_ref().map(bytes).as_ref(), "{tag}: {t} market {m} before");
            match new {
                Some(p) => {
                    assert_eq!(p.market_id, m, "{tag}: {t} one market per change");
                    cur.insert(m, bytes(p));
                }
                None => {
                    cur.remove(&m);
                }
            }
        }
        assert_eq!(cur.into_values().collect::<Vec<_>>(), is.iter().map(bytes).collect::<Vec<_>>(), "{tag}: {t} changes compose");
    }
}

/// `rec` == a cold build over `rows`, and for every trader prefix in R (and
/// every test trader): a recorded trader's positions == the DB's
/// `positions_for_trader` and each market's `get_position` (`db` == R); an
/// opaque trader's range holds an irregular row.
fn check(rec: &TraderPositions, rows: &ResidentRows, db: &StateDb, stats: &mut Stats, tag: &str) {
    assert!(rec.same_as(&TraderPositions::build(rows)), "{tag}: warm records != cold build");
    let pm = PositionManager::new(db.clone());
    let r = rows.rows(CF_NATIVE_POSITIONS).unwrap();
    // E2: the trader set = the traders of R's 28-byte keys (opaque or not).
    let with_key: BTreeSet<Address> = r.keys().filter(|k| k.len() == 28).map(|k| Address::from_slice(&k[..20])).collect();
    assert_eq!(rec.traders, with_key, "{tag}: trader set != traders of R's 28-byte keys");
    // adl-budget C2: per market, the traders of R's 28-byte keys `t ‖ m`.
    let mut by_market: HashMap<MarketId, BTreeSet<Address>> = HashMap::new();
    for k in r.keys().filter(|k| k.len() == 28) {
        let m = MarketId::from_be_bytes(k[20..].try_into().unwrap());
        by_market.entry(m).or_default().insert(Address::from_slice(&k[..20]));
    }
    assert_eq!(rec.holders, by_market, "{tag}: holder lists != traders of R's keys per market");
    let mut prefixes: BTreeSet<Address> =
        r.keys().filter(|k| k.len() >= 20).map(|k| Address::from_slice(&k[..20])).collect();
    prefixes.extend((0..=TRADERS).map(trader));
    for t in prefixes {
        match rec.get(&t) {
            Some(ps) => {
                stats.regular += 1;
                let want = pm.positions_for_trader(&t).expect("a recorded trader decodes");
                assert_eq!(
                    ps.iter().map(bytes).collect::<Vec<_>>(),
                    want.iter().map(bytes).collect::<Vec<_>>(),
                    "{tag}: positions of {t}"
                );
                for m in 0..=MARKETS + 1 {
                    let want = pm.get_position(&t, m).expect("a recorded trader's row decodes");
                    stats.found += usize::from(want.is_some());
                    assert_eq!(find(ps, m).map(bytes), want.as_ref().map(bytes), "{tag}: {t} market {m}");
                }
            }
            None => {
                stats.opaque += 1;
                let irregular = r
                    .range(t.to_vec()..)
                    .take_while(|(k, _)| k.starts_with(t.as_slice()))
                    .any(|(k, v)| {
                        k.len() != 28
                            || Position::try_from_slice(v).map_or(true, |p| position_key(&p.trader, p.market_id) != k[..])
                    });
                assert!(irregular, "{tag}: {t} opaque without an irregular row");
            }
        }
    }
}

use borsh::BorshDeserialize;

/// One random block's writes / tombstones of `CF_NATIVE_POSITIONS`.
fn random_block(o: &NativeStateOverlay, rng: &mut Lcg) {
    // Irregular rows are written in about one block of six (they stay until
    // their own key is rewritten or deleted); removals happen in any block.
    let wild = rng.below(6) == 0;
    for _ in 0..1 + rng.below(12) {
        let t = trader(rng.below(TRADERS));
        let m = 1 + rng.below(MARKETS);
        let key = position_key(&t, m);
        let p = position(t, m, rng.below(1_000), rng.below(2) == 0);
        let mut choice = rng.below(100);
        if !wild && matches!(choice, 85 | 86 | 91..=94 | 98) {
            choice = 0;
        }
        match choice {
            0..=69 => o.put_cf_raw(CF_NATIVE_POSITIONS, &key, &bytes(&p)).unwrap(),
            70..=84 => o.delete_cf_raw(CF_NATIVE_POSITIONS, &key).unwrap(),
            // a longer key under the trader holding a valid position
            85..=86 => o.put_cf_raw(CF_NATIVE_POSITIONS, &[&key[..], &[1]].concat(), &bytes(&p)).unwrap(),
            87..=90 => o.delete_cf_raw(CF_NATIVE_POSITIONS, &[&key[..], &[1]].concat()).unwrap(),
            // a valid position of another trader / market at this key
            91 => {
                let other = position(trader((rng.below(TRADERS) + 1) % TRADERS), m + 1, 5, true);
                o.put_cf_raw(CF_NATIVE_POSITIONS, &key, &bytes(&other)).unwrap();
            }
            92 => {
                let other = position(t, m + 1, 5, true);
                o.put_cf_raw(CF_NATIVE_POSITIONS, &key, &bytes(&other)).unwrap();
            }
            // a value that does not decode
            93 => o.put_cf_raw(CF_NATIVE_POSITIONS, &key, &[7, 7, 7]).unwrap(),
            // the bare 20-byte prefix, and a short key (under no trader)
            94 => o.put_cf_raw(CF_NATIVE_POSITIONS, t.as_slice(), &bytes(&p)).unwrap(),
            95..=97 => o.delete_cf_raw(CF_NATIVE_POSITIONS, t.as_slice()).unwrap(),
            98 => o.put_cf_raw(CF_NATIVE_POSITIONS, &key[..10], &[1, 2]).unwrap(),
            _ => o.delete_cf_raw(CF_NATIVE_POSITIONS, &key[..10]).unwrap(),
        }
    }
}

/// P1: 8 seeds x 150 blocks, the records maintained by `apply` after every
/// block == a cold build == the decoded DB rows trader by trader.
#[test]
fn records_equal_decoded_rows_after_random_deltas() {
    let mut stats = Stats::default();
    for seed in 1..=8u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0x9E37_79B9);
        // A starting state with rows of every kind, then the cold build.
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            o.flush(&db).unwrap();
        }
        let mut rows = ResidentRows::build(&db).unwrap();
        let mut rec = TraderPositions::build(&rows);
        let mut plain = TraderPositions::build(&rows);
        check(&rec, &rows, &db, &mut stats, &format!("seed {seed} start"));
        let mut was_opaque: HashSet<Address> = (0..TRADERS).map(trader).filter(|t| rec.get(t).is_none()).collect();
        for h in 1..=150 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            let delta = o.own_pending_delta();
            o.flush(&db).unwrap();
            let before = TraderPositions::build(&rows);
            rows.apply(&delta);
            let mut reports: Vec<Report> = Vec::new();
            rec.apply(
                &delta,
                &rows,
                Some(&mut |t: &Address, r: Option<(&[Change], &[Position])>| {
                    reports.push((*t, r.map(|(c, n)| (c.to_vec(), n.to_vec()))));
                }),
            );
            plain.apply(&delta, &rows, None);
            assert!(plain.same_as(&rec), "seed {seed} block {h}: records with / without reports");
            assert_eq!(rows, ResidentRows::build(&db).unwrap(), "seed {seed} block {h}: R == DB");
            check(&rec, &rows, &db, &mut stats, &format!("seed {seed} block {h}"));
            check_reports(&reports, &delta, &before, &rec, &mut stats, &format!("seed {seed} block {h}"));
            let now: HashSet<Address> = (0..TRADERS).map(trader).filter(|t| rec.get(t).is_none()).collect();
            stats.cleared += was_opaque.difference(&now).count();
            was_opaque = now;
        }
    }
    println!("TRADER_POSITIONS P1 {stats:?}");
    assert!(stats.regular > 3_000 && stats.found > 10_000, "non-vacuous: {stats:?}");
    assert!(stats.opaque > 500 && stats.cleared > 20, "irregular rows come and go: {stats:?}");
    assert!(stats.reported > 1_000 && stats.reported_none > 50 && stats.changes > 3_000, "reports: {stats:?}");
}

/// A trader without rows reads as no positions; `find` misses a market it
/// does not hold.
#[test]
fn empty_trader_and_missing_market() {
    let rec = TraderPositions::build(&ResidentRows::default());
    assert_eq!(rec.get(&trader(0)).map(<[Position]>::len), Some(0));
    let ps = [position(trader(0), 2, 1, true), position(trader(0), 5, 2, false)];
    assert_eq!(find(&ps, 5).map(|p| p.market_id), Some(5));
    assert!(find(&ps, 3).is_none());
}

/// Item 6 E2: `traders_after` over R's trader set and the block's own
/// pending rows == the liquidation walk (`liq::traders_after`) through the
/// same overlay (R attached), for every cursor (none, each trader, addresses
/// between and around them) and limit, over random blocks with irregular
/// rows, traders that appear in the block (first position) and traders whose
/// every position the block deletes. Without R it answers `None` (the caller
/// walks).
#[test]
fn traders_after_equals_the_walk_over_r_and_pending() {
    use torus_core::liquidation as liq;
    let (mut compared, mut appeared, mut vanished) = (0usize, 0usize, 0usize);
    for seed in 1..=8u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0xE2E2_0517);
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            o.flush(&db).unwrap();
        }
        let mut rows = ResidentRows::build(&db).unwrap();
        let mut rec = TraderPositions::build(&rows);
        for h in 1..=80 {
            let mut o = NativeStateOverlay::new(db.clone());
            o.attach_resident(std::sync::Arc::new(rows.clone()));
            random_block(&o, &mut rng);
            // A trader's first position (a new trader, some blocks at the
            // last market key `t ‖ ff×8`), and a trader losing every row.
            if rng.below(3) == 0 {
                let t = trader(TRADERS + rng.below(3));
                let m = if rng.below(2) == 0 { MarketId::MAX } else { 1 + rng.below(MARKETS) };
                o.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t, m), &bytes(&position(t, m, 9, true))).unwrap();
            }
            if rng.below(3) == 0 {
                let t = trader(rng.below(TRADERS + 3));
                let keys: Vec<Vec<u8>> = o
                    .iterate_cf(CF_NATIVE_POSITIONS, Some(t.as_slice()))
                    .unwrap()
                    .into_iter()
                    .map(|(k, _)| k)
                    .filter(|k| k.len() == 28)
                    .collect();
                for k in keys {
                    o.delete_cf_raw(CF_NATIVE_POSITIONS, &k).unwrap();
                }
            }
            let mut cursors: Vec<Option<Address>> = vec![None, Some(Address::ZERO), Some(Address::new([0xff; 20]))];
            for i in 0..TRADERS + 3 {
                let t = trader(i);
                cursors.push(Some(t));
                let mut b = t.0 .0;
                b[19] ^= 0x80;
                cursors.push(Some(Address::new(b)));
                b[0] = 0x3B;
                cursors.push(Some(Address::new(b)));
            }
            let walk_all = liq::traders_after(&o, None, usize::MAX).unwrap();
            appeared += walk_all.iter().filter(|t| !rec.traders.contains(*t)).count();
            vanished += rec.traders.iter().filter(|t| !walk_all.contains(*t)).count();
            for after in &cursors {
                for limit in [0usize, 1, 2, 3, 5, usize::MAX] {
                    let want = liq::traders_after(&o, *after, limit).unwrap();
                    let got = rec.traders_after(&o, *after, limit).unwrap();
                    assert_eq!(got, Some(want), "seed {seed} block {h}: after {after:?} limit {limit}");
                    compared += 1;
                }
            }
            assert_eq!(rec.traders_after(&db, None, usize::MAX).unwrap(), None, "no R: the caller walks");
            let delta = o.own_pending_delta();
            o.detach_resident();
            o.flush(&db).unwrap();
            rows.apply(&delta);
            rec.apply(&delta, &rows, None);
        }
    }
    println!("TRADERS_AFTER compared={compared} appeared={appeared} vanished={vanished}");
    assert!(compared > 10_000, "non-vacuous: {compared}");
    assert!(appeared > 50 && vanished > 50, "traders appear / vanish in the block: {appeared} / {vanished}");
}

/// adl-budget C2 (owner s96 / s99): R's holder list of a market merged with
/// the block's dirty traders of that market ([`dirty_by_market`]) is
/// ascending, has no duplicate, and holds every trader the overlay (R + the
/// block's own rows) has a key `t ‖ m` for — the traders the C1 ranking
/// finds holding `m` when it reads every trader of the walk. Its extra
/// entries are only dirty traders whose `t ‖ m` the block deleted (the
/// ranking reads no position for them and skips them, as C1 skips a trader
/// that does not hold `m`). Same random blocks as the E2 test (irregular
/// rows, a trader's first position, a trader losing every row, a market at
/// `MarketId::MAX`); without R there is no dirty map (the caller ranks over
/// the whole trader set).
#[test]
fn holder_lists_with_the_dirty_traders_cover_the_walk() {
    use torus_core::liquidation as liq;
    let (mut compared, mut extra, mut new_holders) = (0usize, 0usize, 0usize);
    for seed in 1..=8u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0xC2C2_0517);
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            o.flush(&db).unwrap();
        }
        let mut rows = ResidentRows::build(&db).unwrap();
        let mut rec = TraderPositions::build(&rows);
        assert!(dirty_by_market(&db).is_none(), "no R: no dirty map");
        for h in 1..=80 {
            let mut o = NativeStateOverlay::new(db.clone());
            o.attach_resident(std::sync::Arc::new(rows.clone()));
            random_block(&o, &mut rng);
            if rng.below(3) == 0 {
                let t = trader(TRADERS + rng.below(3));
                let m = if rng.below(2) == 0 { MarketId::MAX } else { 1 + rng.below(MARKETS) };
                o.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t, m), &bytes(&position(t, m, 9, true))).unwrap();
            }
            if rng.below(3) == 0 {
                let t = trader(rng.below(TRADERS + 3));
                let keys: Vec<Vec<u8>> = o
                    .iterate_cf(CF_NATIVE_POSITIONS, Some(t.as_slice()))
                    .unwrap()
                    .into_iter()
                    .map(|(k, _)| k)
                    .filter(|k| k.len() == 28)
                    .collect();
                for k in keys {
                    o.delete_cf_raw(CF_NATIVE_POSITIONS, &k).unwrap();
                }
            }
            let dirty = dirty_by_market(&o).expect("R attached: a dirty map");
            let walk = liq::traders_after(&o, None, usize::MAX).unwrap();
            for m in (0..=MARKETS + 1).chain([MarketId::MAX]) {
                let d = dirty.get(&m).map_or(&[][..], Vec::as_slice);
                assert!(d.windows(2).all(|w| w[0] < w[1]), "seed {seed} block {h} m {m}: dirty ascending, once");
                let got = rec.holders_with(m, d);
                assert!(got.windows(2).all(|w| w[0] < w[1]), "seed {seed} block {h} m {m}: ascending, no duplicate");
                let holds = |t: &Address| o.get_cf_raw(CF_NATIVE_POSITIONS, &position_key(t, m)).unwrap().is_some();
                let want: Vec<Address> = walk.iter().copied().filter(|t| holds(t)).collect();
                let kept: Vec<Address> = got.iter().copied().filter(|t| holds(t)).collect();
                assert_eq!(kept, want, "seed {seed} block {h} m {m}: holders == the walk's traders holding m");
                for t in got.iter().filter(|t| !holds(t)) {
                    assert!(d.contains(t), "seed {seed} block {h} m {m}: {t} listed without a key and not dirty");
                    extra += 1;
                }
                new_holders += want.iter().filter(|t| rec.holders.get(&m).is_none_or(|s| !s.contains(*t))).count();
                compared += 1;
            }
            let delta = o.own_pending_delta();
            o.detach_resident();
            o.flush(&db).unwrap();
            rows.apply(&delta);
            rec.apply(&delta, &rows, None);
        }
    }
    println!("HOLDERS compared={compared} extra={extra} new_holders={new_holders}");
    assert!(compared > 5_000, "non-vacuous: {compared}");
    assert!(extra > 50 && new_holders > 50, "deleted and new keys in the block: {extra} / {new_holders}");
}

/// One seek of [`Spy`]: its start, its prefix bound (`None`: unbounded) and
/// the keys it returned.
type Seek = (Vec<u8>, Option<Vec<u8>>, Vec<Vec<u8>>);

/// [`has_key`]'s reads through the overlay, recorded.
#[derive(Clone)]
struct Spy {
    inner: NativeStateOverlay,
    seeks: std::sync::Arc<std::sync::Mutex<Vec<Seek>>>,
}

impl StateBackend for Spy {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn iterate_cf_from(&self, cf: &str, start: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let rows = self.inner.iterate_cf_from(cf, start, limit)?;
        self.seeks.lock().unwrap().push((start.to_vec(), None, rows.iter().map(|(k, _)| k.clone()).collect()));
        Ok(rows)
    }
    fn iterate_cf_prefix_from(
        &self,
        cf: &str,
        prefix: &[u8],
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let rows = self.inner.iterate_cf_prefix_from(cf, prefix, start, limit)?;
        self.seeks.lock().unwrap().push((start.to_vec(), Some(prefix.to_vec()), rows.iter().map(|(k, _)| k.clone()).collect()));
        Ok(rows)
    }
    fn atomic_write(&self, ops: &[torus_state::AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

/// adl-budget s99 (owner decision 4; §11.3: ~96 ms of the S=750-like B₁):
/// [`has_key`] reads only under the trader's own prefix. Before, it was an
/// unbounded overlay seek from `t ‖ 00×8`: for a trader the block emptied
/// (B's ADL'd accounts: every key a tombstone over R) the merge stepped over
/// its tombstones AND every following emptied trader's, then returned the
/// next live key of another trader — quadratic in a block that empties many
/// adjacent accounts. Shape: 8 adjacent traders x 20 markets in R, all
/// deleted in the block, a live trader after them, one trader with a
/// longer-than-28-byte key and a 28-byte key, one with only a longer key.
/// Every seek must be bounded to the trader's prefix and never return
/// another trader's key; the answers equal the walk's (`liq::traders_after`
/// holds `t` iff `has_key`).
#[test]
fn has_key_seeks_only_under_the_traders_prefix() {
    use torus_core::liquidation as liq;
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let live = trader(20);
    let (mixed, long_only) = (trader(21), trader(22));
    for i in 0..8 {
        for m in 1..=20 {
            let t = trader(i);
            db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t, m), &bytes(&position(t, m, 3, true))).unwrap();
        }
    }
    db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&live, 1), &bytes(&position(live, 1, 3, false))).unwrap();
    db.put_cf_raw(CF_NATIVE_POSITIONS, &[&position_key(&mixed, 1)[..], &[1]].concat(), &[1]).unwrap();
    db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&mixed, 2), &bytes(&position(mixed, 2, 3, true))).unwrap();
    db.put_cf_raw(CF_NATIVE_POSITIONS, &[&position_key(&long_only, 1)[..], &[1]].concat(), &[1]).unwrap();
    let rows = ResidentRows::build(&db).unwrap();
    let mut o = NativeStateOverlay::new(db.clone());
    o.attach_resident(std::sync::Arc::new(rows));
    for i in 0..8 {
        for m in 1..=20 {
            o.delete_cf_raw(CF_NATIVE_POSITIONS, &position_key(&trader(i), m)).unwrap();
        }
    }
    let spy = Spy { inner: o.clone(), seeks: Default::default() };
    let walk = liq::traders_after(&o, None, usize::MAX).unwrap();
    assert_eq!(walk, vec![live, mixed], "the walk: the emptied traders and the long-key-only one are out");
    for t in (0..8).map(trader).chain([live, mixed, long_only]) {
        spy.seeks.lock().unwrap().clear();
        assert_eq!(has_key(&spy, &t).unwrap(), walk.contains(&t), "{t}");
        let seeks = spy.seeks.lock().unwrap().clone();
        assert!(!seeks.is_empty(), "{t}: read through the backend");
        for (start, bound, got) in &seeks {
            assert_eq!(bound.as_deref(), Some(t.as_slice()), "{t}: seek from {start:?} bounded to the trader's prefix");
            assert!(got.iter().all(|k| k.starts_with(t.as_slice())), "{t}: no other trader's key read: {got:?}");
        }
    }
    o.detach_resident();
}
