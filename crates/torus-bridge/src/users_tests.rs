//! Item 6 Phase 4 step 1 (plan Task 1.1): the record set [`Users`] — one
//! record per trader of R's `CF_NATIVE_POSITIONS` and `CF_NATIVE_BALANCES`
//! rows (its positions, balance and `cvlm` row), the irregular traders' rows
//! and the rows under no trader kept raw — equals R decoded, trader by
//! trader, after every random block delta, re-encodes to exactly R's rows of
//! both CFs, and the records maintained warm (`apply`) equal a cold `build`.
//! A trader is regular only when every row of it in both CFs decodes AND
//! re-encodes to the same bytes: a lenient decode (`is_long` byte 2,
//! `margin_type` byte 7) or a trailing byte makes it opaque. Irregular rows
//! (keys other than `trader ‖ market`, values that do not decode or name
//! another trader / market, short keys) never break it: their trader reads
//! through the overlay, as today.
//!
//! The C7 / E2 / adl-budget C2 tests are moved over from
//! `trader_positions_tests.rs` unchanged (`TraderPositions` -> `Users`; `apply`
//! no longer takes R: the record set follows the delta on its own).

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use torus_core::position::{cum_volume_key, position_key, MarginType, NativeBalance, PositionManager, POSITION_V2_LEN};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
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
        cost_basis: FixedPoint::from_raw(i128::from(raw) * 11 + 5),
        realized_pnl: FixedPoint::from_raw(-(i128::from(raw))),
        isolated_margin: FixedPoint::ZERO,
        margin_type: if raw.is_multiple_of(5) { MarginType::Isolated } else { MarginType::Cross },
    }
}

fn bytes(p: &Position) -> Vec<u8> {
    borsh::to_vec(p).unwrap()
}

fn balance(raw: u64) -> NativeBalance {
    NativeBalance {
        available: FixedPoint::from_raw(i128::from(raw) * 13 + 1),
        order_margin: FixedPoint::from_raw(i128::from(raw % 7)),
    }
}

fn bal_bytes(b: &NativeBalance) -> Vec<u8> {
    borsh::to_vec(b).unwrap()
}

/// A `cvlm` row's value: a raw `FixedPoint`, 16 bytes big-endian.
fn volume_bytes(raw: u64) -> Vec<u8> {
    i128::from(raw).to_be_bytes().to_vec()
}

/// A v2 position row's two flag bytes, which `Position`'s Borsh decode reads
/// leniently (any non-zero byte = long / Isolated).
const IS_LONG_AT: usize = 1 + 20 + 8;
const MARGIN_TYPE_AT: usize = POSITION_V2_LEN - 1;

/// `p`'s row made irregular: `is_long` byte 2 (`how` 0), `margin_type` byte 7
/// (1) — both still decode — or a trailing byte (2, does not decode).
fn lenient(p: &Position, how: u64) -> Vec<u8> {
    let mut b = bytes(p);
    match how {
        0 => b[IS_LONG_AT] = 2,
        1 => b[MARGIN_TYPE_AT] = 7,
        _ => b.push(0),
    }
    b
}

/// The trader a balances key belongs to: a 20-byte balance row or a 24-byte
/// `cvlm ‖ trader` row; `None`: a row under no trader.
fn balance_owner(k: &[u8]) -> Option<Address> {
    match k.len() {
        20 => Some(Address::from_slice(k)),
        24 if k.starts_with(b"cvlm") => Some(Address::from_slice(&k[4..])),
        _ => None,
    }
}

/// The spec of a regular trader (a full re-encode, not `Users`' own check):
/// every positions row under `t` is a 28-byte `t ‖ m` whose value decodes to
/// a position of `t` and `m` and re-encodes to the same bytes; its balance
/// row (if any) decodes and re-encodes to the same bytes; its `cvlm` row (if
/// any) has 16 bytes.
fn reference_regular(rows: &ResidentRows, t: &Address) -> bool {
    let pos = rows.rows(CF_NATIVE_POSITIONS).unwrap();
    let bal = rows.rows(CF_NATIVE_BALANCES).unwrap();
    let positions = pos.range(t.to_vec()..).take_while(|(k, _)| k.starts_with(t.as_slice())).all(|(k, v)| {
        k.len() == 28
            && Position::try_from_slice(v).is_ok_and(|p| position_key(&p.trader, p.market_id) == k[..] && bytes(&p) == *v)
    });
    let balance = bal.get(t.as_slice()).is_none_or(|v| NativeBalance::try_from_slice(v).is_ok_and(|b| bal_bytes(&b) == *v));
    let volume = bal.get(&cum_volume_key(t)[..]).is_none_or(|v| v.len() == 16);
    positions && balance && volume
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
    /// Phase 4: regular users with a balance / a `cvlm` row; opaque traders
    /// all of whose rows decode (irregular only by the re-encode check);
    /// rows under no trader.
    balances: usize,
    volumes: usize,
    lenient: usize,
    other: usize,
}

/// One trader's report of `apply` (owned copy).
type Report = (Address, Option<(Vec<Change>, Vec<Position>)>);

/// Item 6 step 1: `apply`'s reports for one block — exactly one per trader
/// prefix the delta writes under in the positions CF (key order); a trader
/// reported with changes was regular before (`before`) and after (`after`),
/// its `now` is its record after, and its changes compose its record before
/// into it, each change's `before` being its position before; a trader
/// reported without was opaque before or after.
fn check_reports(reports: &[Report], delta: &ResidentDelta, before: &Users, after: &Users, stats: &mut Stats, tag: &str) {
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

/// `rec` == a cold build over `rows`; `rec` re-encodes to exactly R's rows
/// of both CFs; and for every trader in R (and every test trader): a
/// recorded trader is regular by the spec ([`reference_regular`]), its
/// positions == the DB's `positions_for_trader` and each market's
/// `get_position`, its balance and `cvlm` fields == R's rows byte for byte
/// (absent iff no row) and the DB's reads (`db` == R); an opaque trader has
/// an irregular row in one of the two CFs.
fn check(rec: &Users, rows: &ResidentRows, db: &StateDb, stats: &mut Stats, tag: &str) {
    assert!(rec.same_as(&Users::build(rows)), "{tag}: warm records != cold build");
    for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
        assert_eq!(&rec.encode_rows(cf), rows.rows(cf).unwrap(), "{tag}: {cf}: the record set != R's rows");
    }
    // Phase 4 step 2: the tracked row count / bytes (the users gauges).
    assert_eq!((rec.row_count(), rec.row_bytes()), row_totals(rows), "{tag}: tracked rows / bytes");
    let pm = PositionManager::new(db.clone());
    let r = rows.rows(CF_NATIVE_POSITIONS).unwrap();
    let b = rows.rows(CF_NATIVE_BALANCES).unwrap();
    // E2: the trader set = the traders of R's 28-byte keys (opaque or not).
    let with_key: BTreeSet<Address> = r.keys().filter(|k| k.len() == 28).map(|k| Address::from_slice(&k[..20])).collect();
    assert_eq!(rec.traders, with_key, "{tag}: trader set != traders of R's 28-byte keys");
    // adl-budget C2: per market, the traders of R's 28-byte keys `t ‖ m`.
    let mut by_market = Holders::default();
    for k in r.keys().filter(|k| k.len() == 28) {
        let m = MarketId::from_be_bytes(k[20..].try_into().unwrap());
        by_market.entry(m).or_default().insert(Address::from_slice(&k[..20]));
    }
    assert_eq!(rec.holders, by_market, "{tag}: holder lists != traders of R's keys per market");
    // c2-holder-hashset: `holders_with` == the BTreeSet reference (ascending
    // traders of R's keys `t ‖ m`) in every market, whatever the set's type.
    let mut want: BTreeMap<MarketId, BTreeSet<Address>> = BTreeMap::new();
    for k in r.keys().filter(|k| k.len() == 28) {
        want.entry(MarketId::from_be_bytes(k[20..].try_into().unwrap()))
            .or_default()
            .insert(Address::from_slice(&k[..20]));
    }
    for m in (0..=MARKETS + 1).chain([MarketId::MAX]) {
        let reference: Vec<Address> = want.get(&m).into_iter().flatten().copied().collect();
        assert_eq!(
            rec.holders_with(m, &[]),
            reference,
            "{tag}: holders_with({m}) != the BTreeSet reference"
        );
    }
    stats.other += r.keys().filter(|k| k.len() < 20).count() + b.keys().filter(|k| balance_owner(k).is_none()).count();
    let mut prefixes: BTreeSet<Address> =
        r.keys().filter(|k| k.len() >= 20).map(|k| Address::from_slice(&k[..20])).collect();
    prefixes.extend(b.keys().filter_map(|k| balance_owner(k)));
    prefixes.extend((0..=TRADERS).map(trader));
    for t in prefixes {
        let regular = reference_regular(rows, &t);
        let bal_row = b.get(t.as_slice());
        let vol_row = b.get(&cum_volume_key(&t)[..]);
        match rec.get(&t) {
            Some(ps) => {
                assert!(regular, "{tag}: {t} has a record but an irregular row");
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
                let u = rec.map.get(&t);
                assert_eq!(
                    u.is_some(),
                    !ps.is_empty() || bal_row.is_some() || vol_row.is_some(),
                    "{tag}: {t} has a record iff it has a row"
                );
                // Absent != zero: `None` iff R has no row.
                let bal = u.and_then(|u| u.balance.as_ref());
                assert_eq!(bal.map(bal_bytes).as_ref(), bal_row, "{tag}: {t} balance row");
                if let Some(bal) = bal {
                    stats.balances += 1;
                    assert_eq!(bal_bytes(&pm.get_native_balance(&t).unwrap()), bal_bytes(bal), "{tag}: {t} balance read");
                }
                let vol = u.and_then(|u| u.cum_volume);
                assert_eq!(vol.map(|v| v.raw().to_be_bytes().to_vec()).as_ref(), vol_row, "{tag}: {t} cvlm row");
                if let Some(v) = vol {
                    stats.volumes += 1;
                    assert_eq!(pm.get_cum_volume(&t).unwrap(), v, "{tag}: {t} cum_volume read");
                }
            }
            None => {
                stats.opaque += 1;
                assert!(!regular, "{tag}: {t} opaque without an irregular row");
                let decodes = r.range(t.to_vec()..).take_while(|(k, _)| k.starts_with(t.as_slice())).all(|(k, v)| {
                    k.len() == 28
                        && Position::try_from_slice(v).is_ok_and(|p| position_key(&p.trader, p.market_id) == k[..])
                }) && bal_row.is_none_or(|v| NativeBalance::try_from_slice(v).is_ok())
                    && vol_row.is_none_or(|v| v.len() == 16);
                stats.lenient += usize::from(decodes);
            }
        }
    }
}

use borsh::BorshDeserialize;

/// The test traders `0..TRADERS`.
fn test_traders() -> Vec<Address> {
    (0..TRADERS).map(trader).collect()
}

/// One random block's writes / tombstones of `CF_NATIVE_POSITIONS`.
fn random_block(o: &NativeStateOverlay, rng: &mut Lcg) {
    random_block_over(o, rng, &test_traders());
}

/// [`random_block`] over `traders` (same draws for [`test_traders`]).
fn random_block_over(o: &impl StateBackend, rng: &mut Lcg, traders: &[Address]) {
    let n = traders.len() as u64;
    // Irregular rows are written in about one block of six (they stay until
    // their own key is rewritten or deleted); removals happen in any block.
    let wild = rng.below(6) == 0;
    for _ in 0..1 + rng.below(12) {
        let t = traders[rng.below(n) as usize];
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
                let other = position(traders[((rng.below(n) + 1) % n) as usize], m + 1, 5, true);
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

/// Phase 4: one random block's balance, `cvlm` and irregular rows: balance
/// and `cvlm` writes / tombstones in every block; in about one block of six
/// also irregular balance rows (a trailing byte, a value that does not
/// decode, a 17-byte `cvlm`), position rows that decode leniently but do not
/// re-encode (`is_long` 2, `margin_type` 7) or carry a trailing byte, and
/// balances keys under no trader (a short key, a 24-byte key that is not
/// `cvlm`, a 23-byte `cvlm` key). Each irregular row stays until its own key
/// is rewritten or deleted.
fn random_balance_block(o: &NativeStateOverlay, rng: &mut Lcg) {
    random_balance_block_over(o, rng, &test_traders());
}

/// [`random_balance_block`] over `traders` (same draws for [`test_traders`]).
fn random_balance_block_over(o: &impl StateBackend, rng: &mut Lcg, traders: &[Address]) {
    let n = traders.len() as u64;
    let wild = rng.below(6) == 0;
    for _ in 0..1 + rng.below(10) {
        let t = traders[rng.below(n) as usize];
        let raw = rng.below(1_000);
        let mut choice = rng.below(100);
        if !wild && choice >= 85 {
            choice = 0;
        }
        let not_cvlm = [b"cvlx".as_slice(), t.as_slice()].concat();
        let short_cvlm = [b"cvlm".as_slice(), &t.as_slice()[..19]].concat();
        match choice {
            0..=39 => o.put_cf_raw(CF_NATIVE_BALANCES, t.as_slice(), &bal_bytes(&balance(raw))).unwrap(),
            40..=54 => o.delete_cf_raw(CF_NATIVE_BALANCES, t.as_slice()).unwrap(),
            55..=74 => o.put_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&t), &volume_bytes(raw)).unwrap(),
            75..=84 => o.delete_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&t)).unwrap(),
            85 => o.put_cf_raw(CF_NATIVE_BALANCES, t.as_slice(), &[bal_bytes(&balance(raw)), vec![0]].concat()).unwrap(),
            86 => o.put_cf_raw(CF_NATIVE_BALANCES, t.as_slice(), &[9, 9]).unwrap(),
            87 => o.put_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&t), &[0u8; 17]).unwrap(),
            88..=90 => {
                let m = 1 + rng.below(MARKETS);
                let p = position(t, m, raw, true);
                o.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t, m), &lenient(&p, choice - 88)).unwrap();
            }
            91 => o.put_cf_raw(CF_NATIVE_BALANCES, &t.as_slice()[..5], &[1]).unwrap(),
            92 => o.delete_cf_raw(CF_NATIVE_BALANCES, &t.as_slice()[..5]).unwrap(),
            93 => o.put_cf_raw(CF_NATIVE_BALANCES, &not_cvlm, &[2; 16]).unwrap(),
            94 => o.delete_cf_raw(CF_NATIVE_BALANCES, &not_cvlm).unwrap(),
            95 => o.put_cf_raw(CF_NATIVE_BALANCES, &short_cvlm, &[3; 16]).unwrap(),
            96 => o.delete_cf_raw(CF_NATIVE_BALANCES, &short_cvlm).unwrap(),
            _ => o.put_cf_raw(CF_NATIVE_BALANCES, t.as_slice(), &bal_bytes(&balance(raw))).unwrap(),
        }
    }
}

/// P1 (plan Task 1.1): 8 seeds x 150 blocks over positions, balances, `cvlm`
/// rows and irregular rows; the records maintained by `apply` after every
/// block == a cold build == R decoded trader by trader, and re-encode to R's
/// rows of both CFs.
#[test]
fn users_equal_decoded_rows_after_random_deltas() {
    let mut stats = Stats::default();
    for seed in 1..=8u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0x9E37_79B9);
        // A starting state with rows of every kind, then the cold build.
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            random_balance_block(&o, &mut rng);
            o.flush(&db).unwrap();
        }
        let mut rows = ResidentRows::build(&db).unwrap();
        let mut rec = Users::build(&rows);
        let mut plain = Users::build(&rows);
        check(&rec, &rows, &db, &mut stats, &format!("seed {seed} start"));
        let mut was_opaque: HashSet<Address> = (0..TRADERS).map(trader).filter(|t| rec.get(t).is_none()).collect();
        for h in 1..=150 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            random_balance_block(&o, &mut rng);
            let delta = o.own_pending_delta();
            o.flush(&db).unwrap();
            let before = Users::build(&rows);
            rows.apply(&delta);
            let mut reports: Vec<Report> = Vec::new();
            rec.apply(
                &delta,
                Some(&mut |t: &Address, r: Option<(&[Change], &[Position], &mut Sums)>| {
                    reports.push((*t, r.map(|(c, n, _)| (c.to_vec(), n.to_vec()))));
                }),
            );
            plain.apply(&delta, None);
            assert!(plain.same_as(&rec), "seed {seed} block {h}: records with / without reports");
            assert_eq!(rows, ResidentRows::build(&db).unwrap(), "seed {seed} block {h}: R == DB");
            check(&rec, &rows, &db, &mut stats, &format!("seed {seed} block {h}"));
            check_reports(&reports, &delta, &before, &rec, &mut stats, &format!("seed {seed} block {h}"));
            let now: HashSet<Address> = (0..TRADERS).map(trader).filter(|t| rec.get(t).is_none()).collect();
            stats.cleared += was_opaque.difference(&now).count();
            was_opaque = now;
        }
    }
    println!("USERS P1 {stats:?}");
    assert!(stats.regular > 3_000 && stats.found > 10_000, "non-vacuous: {stats:?}");
    assert!(stats.opaque > 500 && stats.cleared > 20, "irregular rows come and go: {stats:?}");
    assert!(stats.reported > 1_000 && stats.reported_none > 50 && stats.changes > 3_000, "reports: {stats:?}");
    assert!(stats.balances > 1_000 && stats.volumes > 1_000, "balance and cvlm rows: {stats:?}");
    assert!(stats.lenient > 50 && stats.other > 50, "lenient rows, rows under no trader: {stats:?}");
}

/// Plan Task 1.1: a regular user's fields re-encode to R's bytes (each
/// position row, the balance row, the `cvlm` row); a balance-only user is a
/// record but no liquidation candidate. A row that decodes but re-encodes
/// differently (`is_long` byte 2, `margin_type` byte 7), a trailing byte, or
/// a `cvlm` of the wrong length makes the trader opaque — its rows then kept
/// raw, byte for byte — and the regular row back makes it regular again,
/// warm (`apply`) and cold (`build`). Every single-byte change of a position
/// row and of a balance row: the trader is regular iff the row re-encodes to
/// itself (and names its key).
#[test]
fn users_encode_byte_identical_to_rows() {
    let (a, b) = (trader(0), trader(1));
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let pa = [position(a, 1, 3, true), position(a, 7, 5, false)];
    for p in &pa {
        db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&a, p.market_id), &bytes(p)).unwrap();
    }
    db.put_cf_raw(CF_NATIVE_BALANCES, a.as_slice(), &bal_bytes(&balance(4))).unwrap();
    db.put_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&a), &volume_bytes(77)).unwrap();
    db.put_cf_raw(CF_NATIVE_BALANCES, b.as_slice(), &bal_bytes(&balance(9))).unwrap();
    let mut rows = ResidentRows::build(&db).unwrap();
    let mut rec = Users::build(&rows);
    let both = [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES];
    {
        let (rp, rb) = (rows.rows(CF_NATIVE_POSITIONS).unwrap(), rows.rows(CF_NATIVE_BALANCES).unwrap());
        let u = rec.map.get(&a).expect("a: regular user");
        assert_eq!(u.positions.len(), 2);
        for p in &u.positions {
            assert_eq!(bytes(p), rp[&position_key(&a, p.market_id)[..]], "a: position {}", p.market_id);
        }
        assert_eq!(bal_bytes(u.balance.as_ref().unwrap()), rb[a.as_slice()], "a: balance");
        assert_eq!(u.cum_volume.unwrap().raw().to_be_bytes().to_vec(), rb[&cum_volume_key(&a)[..]], "a: cvlm");
        let ub = rec.map.get(&b).expect("b: balance-only user");
        assert!(ub.positions.is_empty() && ub.cum_volume.is_none() && ub.balance.is_some());
        assert_eq!(rec.get(&b).map(<[Position]>::len), Some(0), "b: no positions");
        assert!(!rec.traders.contains(&b), "b: no liquidation candidate");
        for cf in both {
            assert_eq!(&rec.encode_rows(cf), rows.rows(cf).unwrap(), "{cf}: byte-identical");
        }
    }
    // One block writes an irregular row of `a`, the next the regular one back.
    let block = |rows: &mut ResidentRows, rec: &mut Users, cf: &str, key: &[u8], value: &[u8]| {
        let o = NativeStateOverlay::new(db.clone());
        o.put_cf_raw(cf, key, value).unwrap();
        let delta = o.own_pending_delta();
        o.flush(&db).unwrap();
        rows.apply(&delta);
        rec.apply(&delta, None);
    };
    let pk = |m: MarketId| position_key(&a, m).to_vec();
    let irregular: Vec<(&str, &str, Vec<u8>, Vec<u8>)> = vec![
        ("is_long byte 2", CF_NATIVE_POSITIONS, pk(1), lenient(&pa[0], 0)),
        ("margin_type byte 7", CF_NATIVE_POSITIONS, pk(7), lenient(&pa[1], 1)),
        ("position trailing byte", CF_NATIVE_POSITIONS, pk(1), lenient(&pa[0], 2)),
        ("balance trailing byte", CF_NATIVE_BALANCES, a.to_vec(), [bal_bytes(&balance(4)), vec![0]].concat()),
        ("cvlm 17 bytes", CF_NATIVE_BALANCES, cum_volume_key(&a).to_vec(), vec![0; 17]),
        ("cvlm 15 bytes", CF_NATIVE_BALANCES, cum_volume_key(&a).to_vec(), vec![0; 15]),
    ];
    for (what, cf, key, bad) in irregular {
        let good = rows.rows(cf).unwrap()[&key[..]].clone();
        if what.contains("byte 2") || what.contains("byte 7") {
            let p = Position::try_from_slice(&bad).expect("decodes leniently");
            assert_eq!(bytes(&p), good, "{what}: decodes to the regular row's position");
        }
        for (value, regular) in [(bad, false), (good, true)] {
            block(&mut rows, &mut rec, cf, &key, &value);
            let tag = format!("{what}, regular {regular}");
            assert_eq!(rec.get(&a).is_some(), regular, "{tag}");
            assert_eq!(rec.opaque.contains_key(&a), !regular, "{tag}: raw rows kept iff opaque");
            assert!(rec.same_as(&Users::build(&rows)), "{tag}: warm == cold");
            for cf in both {
                assert_eq!(&rec.encode_rows(cf), rows.rows(cf).unwrap(), "{tag}: {cf} byte-identical");
            }
            assert!(rec.get(&b).is_some(), "{tag}: b untouched");
        }
    }
    // Every single-byte change of a canonical row, warm and cold.
    let (mut kept, mut turned) = (0usize, 0usize);
    let cases: [(&str, Vec<u8>, Vec<u8>); 2] =
        [(CF_NATIVE_POSITIONS, pk(1), bytes(&pa[0])), (CF_NATIVE_BALANCES, a.to_vec(), bal_bytes(&balance(4)))];
    for (cf, key, canonical) in cases {
        for i in 0..canonical.len() {
            for x in [0u8, 1, 2, 7, 0x80, 0xff] {
                let mut value = canonical.clone();
                value[i] = x;
                let want = if cf == CF_NATIVE_POSITIONS {
                    Position::try_from_slice(&value)
                        .is_ok_and(|p| bytes(&p) == value && position_key(&p.trader, p.market_id) == key[..])
                } else {
                    NativeBalance::try_from_slice(&value).is_ok_and(|b| bal_bytes(&b) == value)
                };
                block(&mut rows, &mut rec, cf, &key, &value);
                let tag = format!("{cf} byte {i} = {x:#x}");
                assert_eq!(rec.get(&a).is_some(), want, "{tag}: warm");
                assert_eq!(Users::build(&rows).get(&a).is_some(), want, "{tag}: cold");
                for cf in both {
                    assert_eq!(&rec.encode_rows(cf), rows.rows(cf).unwrap(), "{tag}: {cf} byte-identical");
                }
                if want { kept += 1 } else { turned += 1 }
                block(&mut rows, &mut rec, cf, &key, &canonical);
                assert!(rec.get(&a).is_some(), "{tag}: restored");
            }
        }
    }
    println!("USERS byte changes: regular {kept} opaque {turned}");
    assert!(kept > 300 && turned > 100, "non-vacuous: {kept} / {turned}");
}

/// Plan Task 1.1 / row 6: the in-memory sizes, pinned (estimate:
/// `UserState` <= 112 B, `Position` 112 B).
#[test]
fn users_size_of_pins() {
    let (user, position) = (std::mem::size_of::<UserState>(), std::mem::size_of::<Position>());
    println!("USERS size_of UserState {user} Position {position}");
    assert_eq!(position, 112, "Position");
    assert_eq!(user, 112, "UserState");
}

/// A trader without rows reads as no positions; `find` misses a market it
/// does not hold.
#[test]
fn empty_trader_and_missing_market() {
    let rec = Users::build(&ResidentRows::default());
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
        let mut rec = Users::build(&rows);
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
            rec.apply(&delta, None);
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
        let mut rec = Users::build(&rows);
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
            rec.apply(&delta, None);
        }
    }
    println!("HOLDERS compared={compared} extra={extra} new_holders={new_holders}");
    assert!(compared > 5_000, "non-vacuous: {compared}");
    assert!(extra > 50 && new_holders > 50, "deleted and new keys in the block: {extra} / {new_holders}");
}

/// One scripted block's op: `t ‖ m` written with a position of `raw` / `long`,
/// deleted, or a longer key under `t` written / deleted (irregular).
#[derive(Clone, Copy)]
enum Op {
    Put(u64, MarketId, u64, bool),
    Del(u64, MarketId),
    Long(u64, MarketId, bool),
}

/// adl-budget C2 perf (`set_holder` only when a key appears or disappears):
/// after every scripted block the holder lists == the traders of R's keys
/// per market (`check`: and the warm records == a cold build) == C1 (the
/// walk's traders holding `m`). Blocks: opens over several traders and
/// markets, size changes, a write leaving a position unchanged (same bytes),
/// flips, closes, a tombstone of a key never held, a close + reopen of the
/// same key in one block (and an open + close), a trader going flat, a
/// market at `MarketId::MAX`, and a trader turning opaque and back (its
/// keys followed per key from the delta meanwhile).
#[test]
fn holder_lists_follow_scripted_blocks_bit_identically() {
    use torus_core::liquidation as liq;
    use Op::*;
    let blocks: Vec<Vec<Op>> = vec![
        vec![
            Put(0, 1, 10, true),
            Put(0, 2, 11, true),
            Put(1, 1, 12, false),
            Put(2, 3, 13, true),
            Put(3, 1, 14, true),
            Put(3, 4, 15, false),
        ],
        // size change, unchanged write, flip, close, open, tombstone of a key never held
        vec![
            Put(0, 1, 20, true),
            Put(1, 1, 12, false),
            Put(2, 3, 13, false),
            Del(3, 4),
            Put(4, 2, 16, true),
            Del(4, 3),
        ],
        // close + reopen in one block, open + close in one block, MAX market
        vec![
            Del(0, 2),
            Put(0, 2, 30, false),
            Del(3, 1),
            Put(3, 1, 31, true),
            Put(1, 4, 32, true),
            Del(1, 4),
            Put(2, MarketId::MAX, 33, true),
        ],
        // trader 0 flat, trader 2 opaque (a longer key) with a write and a close of its own
        vec![
            Del(0, 1),
            Del(0, 2),
            Put(1, 1, 40, true),
            Long(2, 3, true),
            Put(2, 1, 41, true),
            Del(2, MarketId::MAX),
        ],
        // trader 2 still opaque: an unchanged write, an open, a close
        vec![
            Put(2, 1, 41, true),
            Put(2, 2, 50, false),
            Del(2, 3),
            Put(0, 1, 51, true),
        ],
        // trader 2 regular again; reopen of a key closed while opaque
        vec![
            Long(2, 3, false),
            Put(2, 3, 60, true),
            Put(4, 2, 16, true),
            Del(1, 1),
            Put(1, 1, 61, false),
        ],
        // everyone flat but trader 4
        vec![
            Del(0, 1),
            Del(1, 1),
            Del(2, 1),
            Del(2, 2),
            Del(2, 3),
            Del(3, 1),
        ],
    ];
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut rows = ResidentRows::build(&db).unwrap();
    let mut rec = Users::build(&rows);
    let mut stats = Stats::default();
    let markets: Vec<MarketId> = (0..=5).chain([MarketId::MAX]).collect();
    for (h, ops) in blocks.iter().enumerate() {
        let o = NativeStateOverlay::new(db.clone());
        for &op in ops {
            match op {
                Put(i, m, raw, long) => {
                    let t = trader(i);
                    o.put_cf_raw(
                        CF_NATIVE_POSITIONS,
                        &position_key(&t, m),
                        &bytes(&position(t, m, raw, long)),
                    )
                    .unwrap();
                }
                Del(i, m) => o
                    .delete_cf_raw(CF_NATIVE_POSITIONS, &position_key(&trader(i), m))
                    .unwrap(),
                Long(i, m, true) => {
                    o.put_cf_raw(
                        CF_NATIVE_POSITIONS,
                        &[&position_key(&trader(i), m)[..], &[1]].concat(),
                        &[1],
                    )
                    .unwrap();
                }
                Long(i, m, false) => {
                    o.delete_cf_raw(
                        CF_NATIVE_POSITIONS,
                        &[&position_key(&trader(i), m)[..], &[1]].concat(),
                    )
                    .unwrap();
                }
            }
        }
        let delta = o.own_pending_delta();
        o.flush(&db).unwrap();
        rows.apply(&delta);
        rec.apply(&delta, None);
        let tag = format!("block {}", h + 1);
        check(&rec, &rows, &db, &mut stats, &tag);
        let walk = liq::traders_after(&db, None, usize::MAX).unwrap();
        for &m in &markets {
            let c1: Vec<Address> = walk
                .iter()
                .copied()
                .filter(|t| {
                    db.get_cf_raw(CF_NATIVE_POSITIONS, &position_key(t, m))
                        .unwrap()
                        .is_some()
                })
                .collect();
            assert_eq!(
                rec.holders_with(m, &[]),
                c1,
                "{tag} m {m}: holder list == C1"
            );
        }
    }
    assert!(
        stats.opaque > 0 && stats.regular > 0,
        "non-vacuous: {stats:?}"
    );
    assert_eq!(
        rec.holders.len(),
        1,
        "only trader 4 holds a market at the end"
    );
}

/// adl-budget C2 perf: `apply` touches the holder index only where a key
/// appears or disappears. Sentinels planted in the index (a holder dropped,
/// a non-holder added) survive a block that rewrites the dropped holder's
/// key (presence unchanged) and tombstones the added one's never-held key;
/// the same block's open and close still move their holders.
#[test]
fn holder_index_untouched_when_presence_does_not_change() {
    let (t0, t1) = (trader(0), trader(1));
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for (t, m) in [(t0, 1), (t0, 2), (t1, 1)] {
        db.put_cf_raw(
            CF_NATIVE_POSITIONS,
            &position_key(&t, m),
            &bytes(&position(t, m, 1, true)),
        )
        .unwrap();
    }
    let mut rows = ResidentRows::build(&db).unwrap();
    let mut rec = Users::build(&rows);
    rec.holders.get_mut(&1).unwrap().remove(&t0);
    rec.holders.entry(3).or_default().insert(t1);
    let o = NativeStateOverlay::new(db.clone());
    o.put_cf_raw(
        CF_NATIVE_POSITIONS,
        &position_key(&t0, 1),
        &bytes(&position(t0, 1, 2, false)),
    )
    .unwrap();
    o.delete_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t1, 3))
        .unwrap();
    o.put_cf_raw(
        CF_NATIVE_POSITIONS,
        &position_key(&t1, 2),
        &bytes(&position(t1, 2, 3, true)),
    )
    .unwrap();
    o.delete_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t0, 2))
        .unwrap();
    let delta = o.own_pending_delta();
    o.flush(&db).unwrap();
    rows.apply(&delta);
    rec.apply(&delta, None);
    let set = |m: MarketId| {
        rec.holders
            .get(&m)
            .map(|s| s.iter().copied().collect::<Vec<_>>())
    };
    assert_eq!(
        set(1),
        Some(vec![t1]),
        "rewrite of a held key: index untouched"
    );
    assert_eq!(
        set(3),
        Some(vec![t1]),
        "tombstone of a never-held key: index untouched"
    );
    assert_eq!(set(2), Some(vec![t1]), "open and close: holders moved");
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

/// c2-holder-hashset: `holders_with` is ascending (and merges `dirty` in
/// order) whatever order the holders entered the index: across blocks, by
/// the record path (`follow`) and the per-key path of an opaque trader, a
/// holder removed and re-added last.
#[test]
fn holders_with_ascending_whatever_the_insertion_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut rows = ResidentRows::build(&db).unwrap();
    let mut rec = Users::build(&rows);
    let mut block = |f: &dyn Fn(&NativeStateOverlay)| {
        let o = NativeStateOverlay::new(db.clone());
        f(&o);
        let delta = o.own_pending_delta();
        o.flush(&db).unwrap();
        rows.apply(&delta);
        rec.apply(&delta, None);
        assert!(rec.same_as(&Users::build(&rows)), "warm == cold");
        rec.holders_with(1, &[])
    };
    let open = |o: &NativeStateOverlay, i: u64| {
        let t = trader(i);
        o.put_cf_raw(
            CF_NATIVE_POSITIONS,
            &position_key(&t, 1),
            &bytes(&position(t, 1, i, true)),
        )
        .unwrap();
    };
    let ts = |is: &[u64]| is.iter().map(|&i| trader(i)).collect::<Vec<_>>();
    assert_eq!(block(&|o| open(o, 5)), ts(&[5]));
    assert_eq!(block(&|o| open(o, 1)), ts(&[1, 5]));
    assert_eq!(block(&|o| open(o, 9)), ts(&[1, 5, 9]));
    // Opaque (a longer key under it): its holder moves per key from the delta.
    let opaque = |o: &NativeStateOverlay| {
        let t = trader(3);
        o.put_cf_raw(
            CF_NATIVE_POSITIONS,
            &[&position_key(&t, 1)[..], &[1]].concat(),
            &[7],
        )
        .unwrap();
        open(o, 3);
    };
    assert_eq!(block(&opaque), ts(&[1, 3, 5, 9]));
    assert_eq!(block(&|o| open(o, 7)), ts(&[1, 3, 5, 7, 9]));
    assert_eq!(
        block(&|o| o
            .delete_cf_raw(CF_NATIVE_POSITIONS, &position_key(&trader(1), 1))
            .unwrap()),
        ts(&[3, 5, 7, 9])
    );
    assert_eq!(block(&|o| open(o, 1)), ts(&[1, 3, 5, 7, 9]));
    assert!(rec.get(&trader(3)).is_none(), "trader 3 opaque");
    assert_eq!(
        rec.holders_with(1, &ts(&[0, 4, 5, 10])),
        ts(&[0, 1, 3, 4, 5, 7, 9, 10]),
        "merged with dirty"
    );
    assert_eq!(
        rec.holders_with(2, &ts(&[2, 6])),
        ts(&[2, 6]),
        "no holder: dirty only"
    );
    assert_eq!(rec.holders_with(2, &[]), ts(&[]));
}

/// c2-holder-hashset µbench: [`Users::apply`] (with a `seen`
/// callback, like the node) over `UB_TP_BLOCKS` (20) blocks of
/// `UB_TP_WRITES` (50,000, ~2 per fill) position writes over `UB_TP_TRADERS`
/// (10,000) traders x `UB_TP_MARKETS` (300) markets, R starting with
/// `UB_TP_DENSITY_PCT` (10) % of the keys held. `UB_TP_TOGGLE_PCT` (30) % of
/// the writes open or close a key (the holder index moves), the rest rewrite
/// a held key. Deltas are made once (untimed); `UB_TP_REPS` (5) passes over
/// them from a fresh build. Prints ms per block and per 1k fills (writes / 2).
///
///   cargo test -p torus-bridge --release --lib ubench_trader_positions_apply -- --ignored --nocapture
#[test]
#[ignore = "µbench"]
fn ubench_trader_positions_apply() {
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (traders, markets) = (env("UB_TP_TRADERS", 10_000), env("UB_TP_MARKETS", 300));
    let (blocks, writes, reps) = (
        env("UB_TP_BLOCKS", 20),
        env("UB_TP_WRITES", 50_000),
        env("UB_TP_REPS", 5),
    );
    let (density, toggle) = (env("UB_TP_DENSITY_PCT", 10), env("UB_TP_TOGGLE_PCT", 30));
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut rng = Lcg(0xC2_4A54);
    let mut held: Vec<(u64, MarketId)> = Vec::new();
    let mut slot: HashMap<(u64, MarketId), usize> = HashMap::default();
    let o = NativeStateOverlay::new(db.clone());
    for i in 0..traders {
        for m in 1..=markets {
            if rng.below(100) < density {
                let t = trader(i);
                o.put_cf_raw(
                    CF_NATIVE_POSITIONS,
                    &position_key(&t, m),
                    &bytes(&position(t, m, rng.below(1_000), true)),
                )
                .unwrap();
                slot.insert((i, m), held.len());
                held.push((i, m));
            }
        }
    }
    o.flush(&db).unwrap();
    let rows0 = ResidentRows::build(&db).unwrap();
    let mut rows = rows0.clone();
    let mut deltas = Vec::new();
    for _ in 0..blocks {
        let o = NativeStateOverlay::new(db.clone());
        for _ in 0..writes {
            if rng.below(100) < toggle {
                // Half closes of a held key, half opens (a held pick
                // rewrites): R's size stays about where it started.
                let close = rng.below(2) == 0;
                let (i, m) = if close {
                    held[rng.below(held.len() as u64) as usize]
                } else {
                    (rng.below(traders), 1 + rng.below(markets))
                };
                let t = trader(i);
                if !close && slot.contains_key(&(i, m)) {
                    o.put_cf_raw(
                        CF_NATIVE_POSITIONS,
                        &position_key(&t, m),
                        &bytes(&position(t, m, rng.below(1_000), true)),
                    )
                    .unwrap();
                } else if let Some(at) = slot.remove(&(i, m)) {
                    o.delete_cf_raw(CF_NATIVE_POSITIONS, &position_key(&t, m))
                        .unwrap();
                    held.swap_remove(at);
                    if let Some(moved) = held.get(at) {
                        slot.insert(*moved, at);
                    }
                } else {
                    o.put_cf_raw(
                        CF_NATIVE_POSITIONS,
                        &position_key(&t, m),
                        &bytes(&position(t, m, rng.below(1_000), true)),
                    )
                    .unwrap();
                    slot.insert((i, m), held.len());
                    held.push((i, m));
                }
            } else {
                let (i, m) = held[rng.below(held.len() as u64) as usize];
                let t = trader(i);
                o.put_cf_raw(
                    CF_NATIVE_POSITIONS,
                    &position_key(&t, m),
                    &bytes(&position(t, m, rng.below(1_000), false)),
                )
                .unwrap();
            }
        }
        let delta = o.own_pending_delta();
        o.flush(&db).unwrap();
        rows.apply(&delta);
        deltas.push((delta, rows.clone()));
    }
    let mut per_rep = Vec::new();
    for rep in 0..reps {
        let mut rec = Users::build(&rows0);
        let mut ns = 0u128;
        let mut sink = 0usize;
        for (delta, _) in &deltas {
            let start = std::time::Instant::now();
            rec.apply(
                delta,
                Some(&mut |_: &Address, r: Option<(&[Change], &[Position], &mut Sums)>| {
                    sink += r.map_or(1, |(c, _, _)| c.len())
                }),
            );
            ns += start.elapsed().as_nanos();
        }
        if rep == 0 {
            assert!(
                rec.same_as(&Users::build(&deltas.last().unwrap().1)),
                "warm == cold"
            );
        }
        let ms_block = ns as f64 / 1e6 / blocks as f64;
        per_rep.push(ms_block);
        println!(
            "UB_TP rep {rep} ms_per_block {ms_block:.3} ms_per_1k_fills {:.4} (sink {sink})",
            ms_block / (writes as f64 / 2.0) * 1e3
        );
    }
    per_rep.sort_by(f64::total_cmp);
    let med = per_rep[per_rep.len() / 2];
    println!(
        "UB_TP traders {traders} markets {markets} held {} blocks {blocks} writes {writes} toggle {toggle}% median ms_per_block {med:.3} ms_per_1k_fills {:.4}",
        held.len(),
        med / (writes as f64 / 2.0) * 1e3
    );
}

// ======================================================================
// Item 6 Phase 4 step 2 (plan Task 2.2): `Users` as R's source for the
// positions and balances CFs (raw R's two maps deleted).
// ======================================================================

use std::ops::Bound as B;
use torus_state::resident_rows::ResidentSource;

/// Rows and key + value bytes of raw R's positions and balances.
fn row_totals(rows: &ResidentRows) -> (usize, usize) {
    [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES]
        .iter()
        .flat_map(|cf| rows.rows(cf).unwrap().iter())
        .fold((0, 0), |(n, b), (k, v)| (n + 1, b + k.len() + v.len()))
}

/// Traders whose address starts with `cvlm`: their balance rows sort among
/// the `cvlm ‖ trader` rows of the balances CF.
fn cvlm_trader(i: u64) -> Address {
    let mut b = [0x63u8; 20];
    b[..4].copy_from_slice(b"cvlm");
    b[12..].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

/// The step 2 trader set: the test traders, two `cvlm`-headed ones, and the
/// all-zero / all-ones addresses (the key space's ends).
fn step2_traders() -> Vec<Address> {
    let mut out = test_traders();
    out.extend([cvlm_trader(0), cvlm_trader(1), Address::ZERO, Address::new([0xff; 20])]);
    out
}

/// What raw R answers for a source range: its rows with key >= `start`, up
/// to the first whose key does not start with `prefix`.
fn raw_range(map: &BTreeMap<Vec<u8>, Vec<u8>>, start: &[u8], prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    map.range::<[u8], _>((B::Included(start), B::Unbounded))
        .take_while(|(k, _)| k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn source_range(u: &Users, slot: usize, start: &[u8], prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    ResidentSource::range(u, slot, start, prefix).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// Keys the source is probed with: every row of both CFs, per trader its
/// prefix, its `cvlm` key, every market key, a longer key; short and empty
/// keys; the key space's ends.
fn probe_keys(rows: &ResidentRows, traders: &[Address]) -> BTreeSet<Vec<u8>> {
    let mut keys: BTreeSet<Vec<u8>> = BTreeSet::new();
    for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
        keys.extend(rows.rows(cf).unwrap().keys().cloned());
    }
    for t in traders {
        keys.insert(t.to_vec());
        keys.insert(cum_volume_key(t).to_vec());
        keys.insert(t.as_slice()[..5].to_vec());
        for m in 0..=MARKETS + 1 {
            keys.insert(position_key(t, m).to_vec());
        }
        keys.insert([&position_key(t, 1)[..], &[1]].concat());
    }
    keys.extend([Vec::new(), vec![0x00], b"cvl".to_vec(), b"cvlm".to_vec(), vec![0xff; 30]]);
    keys
}

/// Prefixes: the empty one, every probe key cut at the trader / `cvlm` /
/// market boundaries (1, 4, 5, 19..=25, 27, 28, 29 bytes).
fn probe_prefixes(keys: &BTreeSet<Vec<u8>>) -> BTreeSet<Vec<u8>> {
    let mut out: BTreeSet<Vec<u8>> = BTreeSet::new();
    out.insert(Vec::new());
    for k in keys {
        for n in [1, 4, 5, 19, 20, 21, 23, 24, 25, 27, 28, 29] {
            if n <= k.len() {
                out.insert(k[..n].to_vec());
            }
        }
    }
    out
}

/// Starts for a prefix: the prefix, just above and below it inside its
/// range, its predecessor-ish (shorter), past its range, and the empty key.
fn probe_starts(p: &[u8], keys: &BTreeSet<Vec<u8>>, rng: &mut Lcg) -> Vec<Vec<u8>> {
    let mut past = p.to_vec();
    if let Some(last) = past.last_mut() {
        *last = last.wrapping_add(1);
    }
    let mut out = vec![p.to_vec(), [p, &[0x00]].concat(), [p, &[0x80]].concat(), p[..p.len().saturating_sub(1)].to_vec(), past, Vec::new()];
    let ks: Vec<&Vec<u8>> = keys.range::<[u8], _>((B::Included(p), B::Unbounded)).take(40).collect();
    if !ks.is_empty() {
        out.push(ks[rng.below(ks.len() as u64) as usize].clone());
    }
    out
}

#[derive(Default, Debug)]
struct SourceStats {
    gets: usize,
    hits: usize,
    ranges: usize,
    rows: usize,
    /// Balances ranges answered by the collect-and-sort slow path / without it.
    slow: usize,
    fast_balances: usize,
}

/// `rec` as R's source == raw `rows`: point reads and lengths of every
/// probe key, and a sample of ranges (start, prefix) over both CFs. The
/// balances ranges under a trader-length prefix (20+ bytes, not a `cvlm`
/// head shorter than a `cvlm` key) never take the slow path.
fn check_source(rec: &Users, rows: &ResidentRows, traders: &[Address], rng: &mut Lcg, stats: &mut SourceStats, tag: &str) {
    let keys = probe_keys(rows, traders);
    let prefixes: Vec<Vec<u8>> = probe_prefixes(&keys).into_iter().collect();
    for (slot, cf) in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES].into_iter().enumerate() {
        let raw = rows.rows(cf).unwrap();
        for k in &keys {
            let want = raw.get(k).cloned();
            stats.gets += 1;
            stats.hits += usize::from(want.is_some());
            assert_eq!(ResidentSource::get(rec, slot, k), want, "{tag} {cf}: get {k:?}");
            assert_eq!(ResidentSource::get_len(rec, slot, k), want.map(|v| v.len()), "{tag} {cf}: len {k:?}");
        }
        // Every trader-length prefix of a trader, and a random sample of the rest.
        let mut sample: Vec<&Vec<u8>> = prefixes.iter().filter(|p| p.len() == 20 || p.len() == 24 || p.is_empty()).collect();
        for _ in 0..40 {
            sample.push(&prefixes[rng.below(prefixes.len() as u64) as usize]);
        }
        for p in sample {
            for s in probe_starts(p, &keys, rng) {
                let before = balance_slow_scans();
                let got = source_range(rec, slot, &s, p);
                let slow = balance_slow_scans() - before;
                assert_eq!(got, raw_range(raw, &s, p), "{tag} {cf}: range from {s:?} under {p:?}");
                stats.ranges += 1;
                stats.rows += got.len();
                let fast = slot == 0 || (p.len() >= 20 && s.as_slice() >= p.as_slice() && !(p.len() < 24 && p.starts_with(b"cvlm")));
                if fast {
                    assert_eq!(slow, 0, "{tag} {cf}: range from {s:?} under {p:?} took the slow path");
                    stats.fast_balances += usize::from(slot == 1);
                } else {
                    stats.slow += slow;
                }
            }
        }
    }
}

/// Plan Task 2.2 / section 3 (Reads, Ordered iteration): `Users` as R's
/// source answers every point read, length and range (start, prefix) of
/// both CFs exactly as raw R's rows, after every random block over regular,
/// lenient and irregular rows, rows under no trader, tombstones, and traders
/// whose address starts with `cvlm` (their balance rows sort among the
/// `cvlm` rows) — opaque traders and rows under no trader served raw, the
/// positions CF in key order from the trader set, the balances CF from the
/// collect-and-sort slow path (counted) unless the prefix names one trader.
#[test]
fn users_source_reads_equal_raw_rows() {
    let traders = step2_traders();
    let mut stats = SourceStats::default();
    let mut opaque = 0;
    for seed in 1..=4u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0x51ED_0002);
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block_over(&o, &mut rng, &traders);
            random_balance_block_over(&o, &mut rng, &traders);
            o.flush(&db).unwrap();
        }
        let mut rows = ResidentRows::build(&db).unwrap();
        let mut rec = Users::build(&rows);
        check_source(&rec, &rows, &traders, &mut rng, &mut stats, &format!("seed {seed} start"));
        for h in 1..=40 {
            let o = NativeStateOverlay::new(db.clone());
            random_block_over(&o, &mut rng, &traders);
            random_balance_block_over(&o, &mut rng, &traders);
            let delta = o.own_pending_delta();
            o.flush(&db).unwrap();
            rows.apply(&delta);
            rec.apply(&delta, None);
            opaque += traders.iter().filter(|t| rec.get(t).is_none()).count();
            check_source(&rec, &rows, &traders, &mut rng, &mut stats, &format!("seed {seed} block {h}"));
        }
    }
    println!("USERS SOURCE {stats:?} opaque={opaque}");
    assert!(stats.hits > 5_000 && stats.ranges > 50_000 && stats.rows > 100_000, "non-vacuous: {stats:?}");
    assert!(stats.slow > 1_000 && stats.fast_balances > 5_000, "both balances paths: {stats:?}");
    assert!(opaque > 100, "opaque traders served raw: {opaque}");
}

/// Every read of `cf` an overlay offers, over `keys` and `prefixes`.
fn overlay_read_dump(ov: &NativeStateOverlay, cf: &str, keys: &BTreeSet<Vec<u8>>, prefixes: &[Vec<u8>]) -> Vec<String> {
    let mut out = vec![format!("all {:?}", ov.iterate_cf(cf, None).unwrap()), format!("keys {:?}", ov.layer_keys(cf))];
    for k in keys {
        out.push(format!("get {k:?} {:?} {:?}", ov.get_cf_raw(cf, k).unwrap(), ov.get_cf_len(cf, k).unwrap()));
    }
    for p in prefixes {
        out.push(format!("pfx {p:?} {:?}", ov.iterate_cf(cf, Some(p)).unwrap()));
        out.push(format!("exists {p:?} {:?}", ov.prefix_exists(cf, p).unwrap()));
        out.push(format!("touches {p:?} {:?}", ov.layer_touches(cf, p)));
        let mut changes = ov.resident_changes(cf, p);
        if let Some(c) = changes.as_mut() {
            c.sort_by(|a, b| a.key.cmp(&b.key));
        }
        out.push(format!("changes {p:?} {changes:?}"));
        for limit in [1usize, 2, usize::MAX] {
            out.push(format!("from {p:?} {limit} {:?}", ov.iterate_cf_from(cf, p, limit).unwrap()));
            let start = [p.as_slice(), &[0x00]].concat();
            out.push(format!("pfrom {p:?} {limit} {:?}", ov.iterate_cf_prefix_from(cf, p, &start, limit).unwrap()));
        }
    }
    out
}

/// Plan Task 2.2: the block's overlay with R whose positions and balances
/// come from `Users` reads both CFs exactly as with raw R (today's R, the
/// reference path), with the block's own writes and tombstones on top (own
/// pending first, then R), after every random block; the record set then
/// follows the block's delta. R is the same `ResidentRows` either way but
/// for the source.
#[test]
fn overlay_through_users_equals_raw_r() {
    let traders = step2_traders();
    let mut compared = 0usize;
    for seed in 1..=3u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0x0E2A_0002);
        for _ in 0..3 {
            let o = NativeStateOverlay::new(db.clone());
            random_block_over(&o, &mut rng, &traders);
            random_balance_block_over(&o, &mut rng, &traders);
            o.flush(&db).unwrap();
        }
        let mut raw = ResidentRows::build(&db).unwrap();
        let mut users = std::sync::Arc::new(Users::build(&raw));
        for h in 1..=25 {
            let scratch = NativeStateOverlay::new(db.clone());
            random_block_over(&scratch, &mut rng, &traders);
            random_balance_block_over(&scratch, &mut rng, &traders);
            let delta = scratch.own_pending_delta();
            let mut with_source = raw.clone();
            with_source.set_source(users.clone());
            let overlay = |r: ResidentRows| {
                let mut ov = NativeStateOverlay::new(db.clone());
                ov.attach_resident(std::sync::Arc::new(r));
                for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
                    for (k, v) in delta.entries(cf) {
                        match v {
                            Some(v) => ov.put_cf_raw(cf, k, v).unwrap(),
                            None => ov.delete_cf_raw(cf, k).unwrap(),
                        }
                    }
                }
                ov
            };
            let (a, b) = (overlay(raw.clone()), overlay(with_source));
            let keys = probe_keys(&raw, &traders);
            let mut prefixes: Vec<Vec<u8>> = probe_prefixes(&keys).into_iter().filter(|p| p.len() <= 20 || p.len() == 24).collect();
            prefixes.retain(|_| rng.below(3) == 0);
            for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
                let (x, y) = (overlay_read_dump(&a, cf, &keys, &prefixes), overlay_read_dump(&b, cf, &keys, &prefixes));
                for (x, y) in x.iter().zip(y.iter()) {
                    assert_eq!(x, y, "seed {seed} block {h} {cf}: raw R vs Users");
                }
                compared += x.len();
            }
            drop((a, b));
            scratch.flush(&db).unwrap();
            raw.apply(&delta);
            std::sync::Arc::get_mut(&mut users).expect("the overlays dropped their R").apply(&delta, None);
            assert!(users.same_as(&Users::build(&raw)), "seed {seed} block {h}: warm == cold");
        }
    }
    assert!(compared > 50_000, "non-vacuous: {compared}");
}

/// Phase 4 step 2: the record set's row count and key + value bytes (the
/// users gauges, kept per change) equal its rows of both CFs after every
/// random block, cold and warm.
#[test]
fn users_row_count_and_bytes_track_the_rows() {
    let traders = step2_traders();
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut rng = Lcg(0xB17E5);
    let mut rows = ResidentRows::build(&db).unwrap();
    let mut rec = Users::build(&rows);
    assert_eq!((rec.row_count(), rec.row_bytes()), (0, 0), "empty");
    for h in 1..=120 {
        let o = NativeStateOverlay::new(db.clone());
        random_block_over(&o, &mut rng, &traders);
        random_balance_block_over(&o, &mut rng, &traders);
        let delta = o.own_pending_delta();
        o.flush(&db).unwrap();
        rows.apply(&delta);
        rec.apply(&delta, None);
        assert_eq!((rec.row_count(), rec.row_bytes()), row_totals(&rows), "block {h}: warm");
        let cold = Users::build(&rows);
        assert_eq!((cold.row_count(), cold.row_bytes()), row_totals(&rows), "block {h}: cold");
    }
    assert!(row_totals(&rows).0 > 30, "non-vacuous");
}

/// Phase 4 step 2: R's rebuild reads both CFs through the overlay without R
/// (DB + parent layer): `Users::build_from` == a cold build of raw R over the
/// same overlay, irregular rows and rows under no trader included.
#[test]
fn users_build_from_backend_equals_cold_build_of_r() {
    let traders = step2_traders();
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut rng = Lcg(0xC01D);
    for _ in 0..6 {
        let o = NativeStateOverlay::new(db.clone());
        random_block_over(&o, &mut rng, &traders);
        random_balance_block_over(&o, &mut rng, &traders);
        o.flush(&db).unwrap();
    }
    let parent = NativeStateOverlay::new(db.clone());
    random_block_over(&parent, &mut rng, &traders);
    random_balance_block_over(&parent, &mut rng, &traders);
    let ov = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    let built = Users::build_from(&ov).unwrap();
    let reference = Users::build(&ResidentRows::build(&ov).unwrap());
    assert!(built.same_as(&reference), "build_from == cold build of R");
    assert_eq!((built.row_count(), built.row_bytes()), (reference.row_count(), reference.row_bytes()));
    assert!(built.row_count() > 20, "non-vacuous");
}

/// Phase 4 step 2: the source slots `Users` answers are R's first two CFs.
#[test]
fn source_slots_are_positions_then_balances() {
    use torus_state::resident_rows::{resident_slot, SOURCE_SLOTS};
    assert_eq!(resident_slot(CF_NATIVE_POSITIONS), Some(POSITIONS));
    assert_eq!(resident_slot(CF_NATIVE_BALANCES), Some(BALANCES));
    assert_eq!(SOURCE_SLOTS, 2);
}

// ======================================================================
// Item 6 Phase 5 step 1 (`docs/plans/item6-phase5-impl.md` Task 1.1):
// `UsersMut` and the dirty sets, shadow only. The overlay still takes every
// write; a `Users` next to it takes the same writes through `UsersMut`
// (typed calls, or the write-through `put_row`), and the rows its dirty sets
// serialize at the end of the block must equal the overlay's pending rows of
// the two CFs, byte for byte, deletes included.
// ======================================================================

use std::sync::{Arc, Mutex};

use super::super::{begin_resident, end_resident, NativeExecContext, NativeExecutor, ResidentBooks};
use alloy_primitives::U256;
use torus_core::liquidation::{self as liquidation, AdlCandidate, LIQUIDATOR_VAULT};
use torus_core::lockbox::Lockbox;
use torus_state::cf::{ALL_CF_NAMES, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
use torus_state::resident_rows::ResidentChange;
use torus_state::running_hash::hashed_cf_id;
use torus_state::{AtomicWriteOp, OwnPendingRow};
use torus_types::{NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// What the shadow made per kind of `UsersMut` call.
#[derive(Debug, Default, Clone, Copy)]
struct MutStats {
    set_position: usize,
    remove_position: usize,
    set_balance: usize,
    add_cum_volume: usize,
    put_row: usize,
    /// Typed calls on an opaque trader (its rows stay raw).
    opaque_typed: usize,
}

/// One typed call a raw write stands for.
enum Call {
    SetPosition(Position),
    RemovePosition(Address, MarketId),
    SetBalance(Address, NativeBalance),
    AddCumVolume(Address, FixedPoint),
}

impl Call {
    fn owner(&self) -> Address {
        match self {
            Call::SetPosition(p) => p.trader,
            Call::RemovePosition(t, _) | Call::SetBalance(t, _) | Call::AddCumVolume(t, _) => *t,
        }
    }
}

/// The typed call for a write (`Some`) / tombstone (`None`) of `key` in
/// `slot`, if there is one: a regular position row or a positions key's
/// tombstone, a regular balance row, a 16-byte `cvlm` row of a regular
/// trader (as the volume added to its row). Any other write goes through
/// `put_row` (no production code deletes a balance or `cvlm` row).
fn typed_call(users: &Users, slot: usize, key: &[u8], value: Option<&[u8]>) -> Option<Call> {
    match (slot, value) {
        (POSITIONS, _) if key.len() != KEY => None,
        (POSITIONS, None) => Some(Call::RemovePosition(Address::from_slice(&key[..TRADER]), market_of(key))),
        (POSITIONS, Some(v)) => decode(key, v).map(Call::SetPosition),
        (_, None) => None,
        (_, Some(v)) => match super::balance_owner(key)? {
            (t, BalanceRow::Balance) => decode_balance(v).map(|b| Call::SetBalance(t, b)),
            (t, BalanceRow::Volume) => {
                let new = decode_volume(v)?;
                let cur = TypedRows::cum_volume(users, &t)?.unwrap_or(FixedPoint::ZERO);
                Some(Call::AddCumVolume(t, new - cur))
            }
        },
    }
}

/// The shadow's `Users` and its draws.
struct ShadowSide {
    users: Users,
    rng: Lcg,
    stats: MutStats,
}

impl ShadowSide {
    /// One write / tombstone of `key` in `cf` (other CFs: nothing), on the
    /// shadow's `Users`: a typed call one time in two when there is one, else
    /// the write-through.
    fn mirror(&mut self, cf: &str, key: &[u8], value: Option<&[u8]>) {
        let slot = match cf {
            CF_NATIVE_POSITIONS => POSITIONS,
            CF_NATIVE_BALANCES => BALANCES,
            _ => return,
        };
        let typed = self.rng.below(2) == 0;
        let Self { users, stats, .. } = self;
        let Some(call) = typed.then(|| typed_call(users, slot, key, value)).flatten() else {
            users.mutate().put_row(slot, key, value);
            stats.put_row += 1;
            return;
        };
        stats.opaque_typed += usize::from(users.get(&call.owner()).is_none());
        match call {
            Call::SetPosition(p) => {
                users.mutate().set_position(p);
                stats.set_position += 1;
            }
            Call::RemovePosition(t, m) => {
                users.mutate().remove_position(&t, m);
                stats.remove_position += 1;
            }
            Call::SetBalance(t, b) => {
                users.mutate().set_balance(&t, b);
                stats.set_balance += 1;
            }
            Call::AddCumVolume(t, x) => {
                users.mutate().add_cum_volume(&t, x).expect("a regular trader's cvlm row");
                stats.add_cum_volume += 1;
            }
        }
    }
}

/// A block overlay with a shadow: every call goes to the overlay, and every
/// write of the two CFs is also made on the shadow's `Users`.
#[derive(Clone)]
struct Shadow {
    inner: NativeStateOverlay,
    side: Arc<Mutex<ShadowSide>>,
}

impl Shadow {
    fn mirror(&self, cf: &str, key: &[u8], value: Option<&[u8]>) {
        self.side.lock().unwrap().mirror(cf, key, value);
    }
}

impl StateBackend for Shadow {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)?;
        self.mirror(cf, key, Some(value));
        Ok(())
    }
    fn put_cf_raw_owned(&self, cf: &str, key: &[u8], value: Vec<u8>) -> Result<(), StateError> {
        self.mirror(cf, key, Some(&value));
        self.inner.put_cf_raw_owned(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)?;
        self.mirror(cf, key, None);
        Ok(())
    }
    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn iterate_cf_from(&self, cf: &str, start: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf_from(cf, start, limit)
    }
    fn get_cf_len(&self, cf: &str, key: &[u8]) -> Result<Option<usize>, StateError> {
        self.inner.get_cf_len(cf, key)
    }
    fn iterate_cf_prefix_from(
        &self,
        cf: &str,
        prefix: &[u8],
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf_prefix_from(cf, prefix, start, limit)
    }
    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        self.inner.prefix_exists(cf, prefix)
    }
    fn layer_touches(&self, cf: &str, prefix: &[u8]) -> bool {
        self.inner.layer_touches(cf, prefix)
    }
    fn resident_changes(&self, cf: &str, prefix: &[u8]) -> Option<Vec<ResidentChange>> {
        self.inner.resident_changes(cf, prefix)
    }
    fn layer_keys(&self, cf: &str) -> Option<Vec<Vec<u8>>> {
        self.inner.layer_keys(cf)
    }
    fn own_pending_row(&self, cf: &str, key: &[u8]) -> OwnPendingRow {
        self.inner.own_pending_row(cf, key)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)?;
        for op in ops {
            match op {
                AtomicWriteOp::Put { cf, key, value } => self.mirror(cf, key, Some(value)),
                AtomicWriteOp::Delete { cf, key } => self.mirror(cf, key, None),
            }
        }
        Ok(())
    }
}

const P5_TRADERS: u64 = 8;
const P5_MARKETS: u64 = 3;
const P5_BLOCKS: u64 = 40;
const P5_CALLS: u64 = 3;
const P5_MID: i64 = 1_000;
/// The trader that holds an irregular row now and then (opaque) and trades.
const P5_ODD: u64 = P5_TRADERS - 1;
/// The irregular row's market (no book: no fill touches it).
const P5_ODD_MARKET: MarketId = 99;

fn p5_fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(i128::from(v) * FixedPoint::SCALE)
}

fn p5_trader(i: u64) -> Address {
    let mut b = [0x5Au8; 20];
    b[12..].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

/// What the direct writers did (`Ok` calls per writer; ADL closes made).
#[derive(Debug, Default)]
struct WriterStats {
    credit: usize,
    deposit: usize,
    withdraw: usize,
    transfer: usize,
    move_collateral: usize,
    backstop: usize,
    adl_closes: usize,
    cross_close: usize,
    apply_fill: usize,
    puts: usize,
    raw_blocks: usize,
    lenient: usize,
    batch_ok: usize,
    single_ok: usize,
}

/// The writers outside `execute_batch`, on the block's context: lockbox
/// credit / deposit / withdrawal, the liquidation legs (transfer, collateral
/// move, backstop, ADL to the escrow and its closes, the escrows' cross
/// close), `apply_fill` (opens, closes, flips), position / balance / `cvlm`
/// puts and position deletes; now and then a raw block of irregular rows
/// over the P1 traders (who never trade), and the odd trader's lenient row.
fn p5_direct_writes(ctx: &NativeExecContext<Shadow>, rng: &mut Lcg, h: u64, call: u64, ws: &mut WriterStats) {
    let pm = &ctx.positions;
    let st = &ctx.state;
    let px = |rng: &mut Lcg| p5_fp(P5_MID - 20 + rng.below(40) as i64);
    for _ in 0..1 + rng.below(4) {
        let t = p5_trader(rng.below(P5_TRADERS));
        let other = p5_trader(rng.below(P5_TRADERS));
        let m = 1 + rng.below(P5_MARKETS);
        let held = pm.get_position(&t, m).unwrap();
        match rng.below(13) {
            0 => ws.credit += usize::from(Lockbox::credit_native(st, &t, p5_fp(1 + rng.below(5_000) as i64)).is_ok()),
            1 => ws.deposit += usize::from(Lockbox::deposit_to_native(st, &t, p5_fp(1 + rng.below(5_000) as i64)).is_ok()),
            2 => {
                let amount = p5_fp(1 + rng.below(2_000) as i64);
                ws.withdraw += usize::from(Lockbox::withdraw_from_native_to(st, &t, &other, amount).is_ok());
            }
            3 => {
                if let Some(p) = held.filter(|_| t != other) {
                    let qty = p.size.min(p5_fp(1 + rng.below(3) as i64));
                    ws.transfer += usize::from(liquidation::transfer(pm, &t, &other, m, qty, px(rng)).is_ok());
                }
            }
            4 => {
                ws.move_collateral += usize::from(liquidation::move_collateral(pm, &t, &LIQUIDATOR_VAULT).is_ok());
                Lockbox::credit_native(st, &t, p5_fp(500_000)).unwrap();
            }
            5 if rng.below(3) == 0 => {
                let mark = px(rng);
                ws.backstop += usize::from(liquidation::backstop(pm, &t, &LIQUIDATOR_VAULT, |_| Some(mark)).is_ok());
                Lockbox::credit_native(st, &t, p5_fp(500_000)).unwrap();
            }
            6 => {
                // ADL: the position to its escrow, then the escrow's closes
                // against the holders of the other side.
                if let Some(p) = held {
                    let escrow = liquidation::adl_escrow(p.is_long);
                    let price = px(rng);
                    if liquidation::transfer(pm, &t, &escrow, m, p.size, price).is_err() {
                        continue;
                    }
                    let ranked: Vec<AdlCandidate> = (0..P5_TRADERS)
                        .map(p5_trader)
                        .filter_map(|c| pm.get_position(&c, m).unwrap())
                        .filter(|c| c.is_long != p.is_long)
                        .map(|c| AdlCandidate {
                            trader: c.trader,
                            is_long: c.is_long,
                            size: c.size,
                            entry_price: c.entry_price,
                            account_value: FixedPoint::ZERO,
                        })
                        .collect();
                    if let Ok((closes, _, _)) = liquidation::adl_close(pm, &escrow, m, price, p.size, &ranked) {
                        ws.adl_closes += closes.len();
                    }
                }
            }
            7 => {
                let (long, short) = (liquidation::adl_escrow(true), liquidation::adl_escrow(false));
                let size = |e: &Address| pm.get_position(e, m).unwrap().map_or(FixedPoint::ZERO, |p| p.size);
                let q = size(&long).min(size(&short)).min(p5_fp(1 + rng.below(3) as i64));
                let (pl, ps) = (px(rng), px(rng));
                ws.cross_close += usize::from(liquidation::cross_close(pm, m, q, pl, ps, &LIQUIDATOR_VAULT).is_ok());
            }
            8 => match held {
                Some(_) if rng.below(3) == 0 => pm.delete_position(&t, m).unwrap(),
                old => {
                    let size = p5_fp(1 + rng.below(9) as i64);
                    let entry = px(rng);
                    let pos = old.map(|p| Position { size: p.size + size, ..p }).unwrap_or(Position {
                        trader: t,
                        market_id: m,
                        is_long: rng.below(2) == 0,
                        size,
                        entry_price: entry,
                        cost_basis: entry * size,
                        realized_pnl: FixedPoint::ZERO,
                        isolated_margin: FixedPoint::ZERO,
                        margin_type: MarginType::Cross,
                    });
                    pm.put_position(&pos).unwrap();
                    ws.puts += 1;
                }
            },
            9 => {
                let bal = NativeBalance { available: p5_fp(100_000 + rng.below(400_000) as i64), order_margin: FixedPoint::ZERO };
                pm.put_native_balance(&t, &bal).unwrap();
                ws.puts += 1;
            }
            10 => {
                pm.put_cum_volume(&t, p5_fp(rng.below(10_000_000) as i64)).unwrap();
                ws.puts += 1;
            }
            _ => {
                // Opens, increases, reductions, closes and flips.
                let qty = p5_fp(1 + rng.below(12) as i64);
                let ok = pm.apply_fill(&t, m, rng.below(2) == 0, qty, px(rng), MarginType::Cross).is_ok();
                ws.apply_fill += usize::from(ok);
            }
        }
    }
    if rng.below(4) == 0 {
        random_block_over(st, rng, &test_traders());
        random_balance_block_over(st, rng, &test_traders());
        ws.raw_blocks += 1;
    }
    // The odd trader's position in a market without a book, `is_long`
    // byte 2: decodes, does not re-encode to itself (opaque while it lasts).
    if call == 0 {
        let odd = p5_trader(P5_ODD);
        let key = position_key(&odd, P5_ODD_MARKET);
        match h % 5 {
            1 => {
                let p = position(odd, P5_ODD_MARKET, h, true);
                st.put_cf_raw(CF_NATIVE_POSITIONS, &key, &lenient(&p, 0)).unwrap();
                ws.lenient += 1;
            }
            4 => st.delete_cf_raw(CF_NATIVE_POSITIONS, &key).unwrap(),
            _ => {}
        }
    }
}

/// One call's actions: crossing and resting limits around the mid, cancel-
/// alls, a modify of a resting order, transfers EVM <-> perp.
fn p5_actions(ctx: &NativeExecContext<Shadow>, rng: &mut Lcg) -> Vec<(Address, NativeAction)> {
    (0..6 + rng.below(10))
        .map(|_| {
            let t = p5_trader(rng.below(P5_TRADERS));
            let m = 1 + rng.below(P5_MARKETS);
            let action = match rng.below(12) {
                0 => NativeAction::CancelAllOrders { market_id: (rng.below(2) == 0).then_some(m) },
                1 => NativeAction::TransferToPerp { amount: U256::from(1 + rng.below(50)) * U256::from(FixedPoint::SCALE as u128) },
                2 => NativeAction::TransferToSpot { amount: U256::from(1 + rng.below(20)) * U256::from(FixedPoint::SCALE as u128) },
                3 => match ctx.order_books.get(&m).and_then(|b| b.orders_for_trader(&t).first().map(|o| o.id)) {
                    Some(order_id) => NativeAction::ModifyOrder {
                        order_id,
                        new_price: Some(p5_fp(P5_MID - 3 + rng.below(7) as i64)),
                        new_qty: None,
                    },
                    None => NativeAction::CancelAllOrders { market_id: None },
                },
                _ => NativeAction::PlaceOrder(PlaceOrderParams {
                    market_id: m,
                    is_buy: rng.below(2) == 0,
                    price: p5_fp(P5_MID - 2 + rng.below(5) as i64),
                    quantity: p5_fp(1 + rng.below(6) as i64),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                }),
            };
            (t, action)
        })
        .collect()
}

/// Plan Task 1.1: `dirty_rows_equal_overlay_rows_every_block`. Seeded blocks
/// through the node's lifecycle (R attached, the block's overlay over the
/// previous block's frozen set, flushed one block later) with every writer
/// of the two CFs on the block's context: `execute_batch` (engine forced,
/// sequential and parallel settle: the cache flushes, order-margin writes,
/// `cvlm`), single actions (`place_order_inner`'s fills), cancel-alls,
/// modifies, EVM <-> perp transfers, and [`p5_direct_writes`]. The context's
/// backend is a [`Shadow`]: every write of the two CFs is also made on a
/// `Users` through `UsersMut`. After every block the rows drained from its
/// dirty sets, put into the block's frozen set without the two CFs, give
/// exactly the overlay's frozen set (every CF, byte for byte, tombstones
/// included), each pending row from one dirty key; and the shadow's records
/// equal a cold build of the block's post-state.
#[test]
fn dirty_rows_equal_overlay_rows_every_block() {
    let mut ws = WriterStats::default();
    let mut ms = MutStats::default();
    let (mut rows_checked, mut tombstones, mut opaque_blocks) = (0usize, 0usize, 0usize);
    for seed in 1..=3u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0x2545_F491);
        for m in 1..=P5_MARKETS {
            let row = borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), p5_fp(1).raw(), p5_fp(1).raw(), p5_fp(5).raw())).unwrap();
            db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), &row).unwrap();
        }
        let pm = PositionManager::new(db.clone());
        for i in 0..P5_TRADERS {
            let t = p5_trader(i);
            pm.put_native_balance(&t, &NativeBalance { available: p5_fp(500_000), order_margin: FixedPoint::ZERO }).unwrap();
            let evm = revm::state::AccountInfo { balance: U256::from(10u128.pow(36)), ..Default::default() };
            db.put_account(&t, &evm).unwrap();
        }
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block(&o, &mut rng);
            random_balance_block(&o, &mut rng);
            o.flush(&db).unwrap();
        }
        let side = Arc::new(Mutex::new(ShadowSide {
            users: Users::build_from(&NativeStateOverlay::new(db.clone())).unwrap(),
            rng: Lcg(seed),
            stats: MutStats::default(),
        }));
        let mut holder = ResidentBooks::default();
        let mut parent: Option<Arc<FrozenPending>> = None;
        let mut books = Default::default();
        let mut next_id: u128 = 1;
        for h in 1..=P5_BLOCKS {
            let tag = format!("seed {seed} block {h}");
            let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
            let mut rb = begin_resident(Some(&mut holder), &mut overlay, h, None);
            assert!(rb.attached(), "{tag}: R attached");
            let shadow = Shadow { inner: overlay.clone(), side: side.clone() };
            let mut ctx = NativeExecContext::new(shadow, h, 10_000 + 7 * h, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
            ctx.order_books = std::mem::take(&mut books);
            ctx.next_global_order_id = next_id;
            ctx.attach_resident_block(&mut rb);
            for call in 0..P5_CALLS {
                p5_direct_writes(&ctx, &mut rng, h, call, &mut ws);
                let actions = p5_actions(&ctx, &mut rng);
                let r = match (h + call) % 3 {
                    0 => NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, 4),
                    1 => NativeExecutor::execute_batch_settle_mode(&mut ctx, &actions, false),
                    _ => NativeExecutor::execute_batch_settle_mode(&mut ctx, &actions, true),
                };
                ws.batch_ok += r.results.iter().filter(|x| x.success).count();
                for (t, a) in p5_actions(&ctx, &mut rng).into_iter().take(2) {
                    ws.single_ok += usize::from(NativeExecutor::execute(&mut ctx, &t, &a).success);
                }
                assert!(ctx.fatal_error.is_none(), "{tag} call {call}: {:?}", ctx.fatal_error);
            }
            books = std::mem::take(&mut ctx.order_books);
            next_id = ctx.next_global_order_id;
            ctx.detach_resident_block(&mut rb);
            drop(ctx);
            overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
            let delta = overlay.own_pending_delta();
            let frozen = overlay.freeze(h);

            // The typed path's frozen set: the overlay's cold CFs, and the
            // two CFs from the dirty sets.
            let cold: Vec<&str> =
                ALL_CF_NAMES.iter().copied().filter(|cf| *cf != CF_NATIVE_POSITIONS && *cf != CF_NATIVE_BALANCES).collect();
            let mut typed = frozen.slice(&cold);
            let mut s = side.lock().unwrap();
            let dirty = s.users.dirty_keys();
            s.users.drain_dirty_into(&mut typed);
            for cf in ALL_CF_NAMES {
                assert_eq!(typed.rows(cf), frozen.rows(cf), "{tag}: {cf}: dirty rows != the overlay's pending rows");
            }
            let pending = frozen.rows(CF_NATIVE_POSITIONS).len() + frozen.rows(CF_NATIVE_BALANCES).len();
            assert_eq!(dirty, pending, "{tag}: one dirty key per pending row of the two CFs");
            assert_eq!(s.users.dirty_keys(), 0, "{tag}: drained");
            rows_checked += pending;
            tombstones += [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES]
                .iter()
                .map(|cf| frozen.rows(cf).iter().filter(|(_, v)| v.is_none()).count())
                .sum::<usize>();
            opaque_blocks += usize::from(s.users.get(&p5_trader(P5_ODD)).is_none());

            end_resident(&mut holder, rb, &mut overlay, delta, true, None);
            if let Some(p) = parent.take() {
                p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
            }
            parent = Some(frozen);
            // In place == the block's post-state, built cold.
            let after = Users::build_from(&NativeStateOverlay::with_parent(db.clone(), parent.clone())).unwrap();
            assert!(s.users.same_as(&after), "{tag}: the shadow's records != a cold build of the post-state");
            assert!(s.users.self_consistent(), "{tag}: shadow self-consistent");
        }
        let s = side.lock().unwrap().stats;
        for (a, b) in [
            (&mut ms.set_position, s.set_position),
            (&mut ms.remove_position, s.remove_position),
            (&mut ms.set_balance, s.set_balance),
            (&mut ms.add_cum_volume, s.add_cum_volume),
            (&mut ms.put_row, s.put_row),
            (&mut ms.opaque_typed, s.opaque_typed),
        ] {
            *a += b;
        }
    }
    println!("P5 T1.1 writers {ws:?}; UsersMut {ms:?}; rows {rows_checked}, tombstones {tombstones}, odd opaque blocks {opaque_blocks}");
    assert!(rows_checked > 5_000 && tombstones > 300, "non-vacuous: rows {rows_checked}, tombstones {tombstones}");
    assert!(
        ms.set_position > 500 && ms.remove_position > 100 && ms.set_balance > 500 && ms.add_cum_volume > 100 && ms.put_row > 1_000,
        "every UsersMut call: {ms:?}"
    );
    assert!(ms.opaque_typed > 20 && opaque_blocks > 10, "typed calls on an opaque trader: {ms:?}, {opaque_blocks}");
    assert!(
        ws.credit > 10 && ws.deposit > 10 && ws.withdraw > 10 && ws.transfer > 10 && ws.move_collateral > 5 && ws.backstop > 2,
        "lockbox and liquidation writers: {ws:?}"
    );
    assert!(ws.adl_closes > 5 && ws.cross_close > 0 && ws.apply_fill > 50, "ADL, cross close, fills: {ws:?}");
    assert!(ws.batch_ok > 500 && ws.single_ok > 50 && ws.raw_blocks > 10 && ws.lenient > 5, "{ws:?}");
}

/// One mutation of [`dirty_rows_sorted_and_once`].
#[derive(Clone, Debug)]
enum P5Op {
    SetPosition(Position),
    RemovePosition(Address, MarketId),
    SetBalance(Address, NativeBalance),
    AddCumVolume(Address, FixedPoint),
    Raw(usize, Vec<u8>, Option<Vec<u8>>),
}

impl P5Op {
    /// The CF slot and key it writes.
    fn key(&self) -> (usize, Vec<u8>) {
        match self {
            P5Op::SetPosition(p) => (POSITIONS, position_key(&p.trader, p.market_id).to_vec()),
            P5Op::RemovePosition(t, m) => (POSITIONS, position_key(t, *m).to_vec()),
            P5Op::SetBalance(t, _) => (BALANCES, t.to_vec()),
            P5Op::AddCumVolume(t, _) => (BALANCES, cum_volume_key(t).to_vec()),
            P5Op::Raw(slot, k, _) => (*slot, k.clone()),
        }
    }

    /// On the record set (`false`: the call was refused, nothing changed).
    fn on_users(&self, u: &mut Users) -> bool {
        let mut w = u.mutate();
        match self {
            P5Op::SetPosition(p) => w.set_position(p.clone()),
            P5Op::RemovePosition(t, m) => w.remove_position(t, *m),
            P5Op::SetBalance(t, b) => w.set_balance(t, b.clone()),
            P5Op::AddCumVolume(t, x) => return w.add_cum_volume(t, *x).is_ok(),
            P5Op::Raw(slot, k, v) => w.put_row(*slot, k, v.as_deref()),
        }
        true
    }

    /// The same write through an overlay (`false`: refused, as above).
    fn on_overlay(&self, o: &NativeStateOverlay) -> bool {
        let pm = PositionManager::new(o.clone());
        match self {
            P5Op::SetPosition(p) => pm.put_position(p).unwrap(),
            P5Op::RemovePosition(t, m) => pm.delete_position(t, *m).unwrap(),
            P5Op::SetBalance(t, b) => pm.put_native_balance(t, b).unwrap(),
            P5Op::AddCumVolume(t, x) => match pm.get_cum_volume(t) {
                Ok(v) => pm.put_cum_volume(t, v + *x).unwrap(),
                Err(_) => return false,
            },
            P5Op::Raw(slot, k, v) => {
                let cf = if *slot == POSITIONS { CF_NATIVE_POSITIONS } else { CF_NATIVE_BALANCES };
                match v {
                    Some(v) => o.put_cf_raw(cf, k, v).unwrap(),
                    None => o.delete_cf_raw(cf, k).unwrap(),
                }
            }
        }
        true
    }
}

/// A random mutation of trader `t` (the P1 rows' traders): typed calls and
/// raw rows of every shape (irregular position / balance / `cvlm` rows,
/// keys under no trader).
fn p5_random_op(rng: &mut Lcg, t: Address) -> P5Op {
    let m = 1 + rng.below(MARKETS);
    let raw = rng.below(1_000);
    match rng.below(14) {
        0..=2 => P5Op::SetPosition(position(t, m, raw, rng.below(2) == 0)),
        3 | 4 => P5Op::RemovePosition(t, m),
        5 | 6 => P5Op::SetBalance(t, balance(raw)),
        7 | 8 => P5Op::AddCumVolume(t, FixedPoint::from_raw(i128::from(raw) - 300)),
        9 => P5Op::Raw(POSITIONS, position_key(&t, m).to_vec(), Some(lenient(&position(t, m, raw, true), rng.below(3)))),
        10 => P5Op::Raw(BALANCES, cum_volume_key(&t).to_vec(), Some(vec![0u8; 17])),
        11 => P5Op::Raw(BALANCES, t.to_vec(), (rng.below(2) == 0).then(|| bal_bytes(&balance(raw)))),
        12 => P5Op::Raw(POSITIONS, t.as_slice()[..7].to_vec(), (rng.below(2) == 0).then(|| vec![7, 7])),
        _ => P5Op::Raw(BALANCES, [b"cvlx".as_slice(), t.as_slice()].concat(), (rng.below(2) == 0).then(|| vec![1; 16])),
    }
}

/// Plan Task 1.1: `dirty_rows_sorted_and_once`. From P1-like rows (every
/// kind of row, opaque traders), a set of mutations — typed calls and raw
/// rows of every shape, many keys written several times — is applied in
/// several interleavings (each key's own mutations keep their order, the
/// keys' are interleaved: key order, reverse key order, random). Every
/// interleaving drains the same rows; each changed key appears once, in
/// strictly ascending `(cf_id, key)` order (positions before balances, keys
/// byte-wise: a `cvlm ‖ t` row among the 20-byte balance rows); one row per
/// dirty key; the record set is left self-consistent and equal to a cold
/// build of its rows; a second drain is empty; and the rows equal an
/// overlay's pending rows after the same writes.
#[test]
fn dirty_rows_sorted_and_once() {
    let (pos_id, bal_id) = (hashed_cf_id(CF_NATIVE_POSITIONS).unwrap(), hashed_cf_id(CF_NATIVE_BALANCES).unwrap());
    assert!(pos_id < bal_id, "positions sort before balances");
    let (mut drained, mut refused) = (0usize, 0usize);
    for seed in 1..=6u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut rng = Lcg(seed * 0x9E37_79B9);
        for _ in 0..4 {
            let o = NativeStateOverlay::new(db.clone());
            random_block_over(&o, &mut rng, &step2_traders());
            random_balance_block_over(&o, &mut rng, &step2_traders());
            o.flush(&db).unwrap();
        }
        let rows = ResidentRows::build(&db).unwrap();
        // Per key, its mutations in order.
        let mut per_key: BTreeMap<(usize, Vec<u8>), Vec<P5Op>> = BTreeMap::new();
        let traders = step2_traders();
        for _ in 0..120 {
            let t = traders[rng.below(traders.len() as u64) as usize];
            let op = p5_random_op(&mut rng, t);
            per_key.entry(op.key()).or_default().push(op);
        }
        let keys: Vec<(usize, Vec<u8>)> = per_key.keys().cloned().collect();
        let mut runs: Vec<DirtyRows> = Vec::new();
        for order in 0..5 {
            let mut lists: Vec<std::collections::VecDeque<P5Op>> = per_key.values().cloned().map(Into::into).collect();
            if order == 1 {
                lists.reverse();
            }
            let mut users = Users::build(&rows);
            let mut seen = 0;
            while lists.iter().any(|l| !l.is_empty()) {
                let live: Vec<usize> = (0..lists.len()).filter(|&i| !lists[i].is_empty()).collect();
                let i = if order < 2 { live[0] } else { live[rng.below(live.len() as u64) as usize] };
                let op = lists[i].pop_front().unwrap();
                seen += 1;
                if !op.on_users(&mut users) {
                    refused += 1;
                }
            }
            assert_eq!(seen, 120);
            let n = users.dirty_keys();
            let d = users.drain_dirty();
            let seq: Vec<(u8, &[u8])> = d
                .positions
                .iter()
                .map(|(k, _)| (pos_id, k.as_slice()))
                .chain(d.balances.iter().map(|(k, _)| (bal_id, k.as_slice())))
                .collect();
            assert!(seq.windows(2).all(|w| w[0] < w[1]), "seed {seed} order {order}: strictly ascending (cf_id, key)");
            assert!(seq.len() <= keys.len() && seq.len() == n, "seed {seed} order {order}: one row per dirty key");
            assert_eq!(users.dirty_keys(), 0, "seed {seed} order {order}: drained");
            assert_eq!(users.drain_dirty(), DirtyRows::default(), "seed {seed} order {order}: a second drain is empty");
            assert!(users.self_consistent(), "seed {seed} order {order}: self-consistent");
            drained += seq.len();
            runs.push(d);
        }
        for (i, r) in runs.iter().enumerate().skip(1) {
            assert_eq!(r, &runs[0], "seed {seed}: interleaving {i} drains other rows than key order");
        }
        // The same writes through an overlay, in key order.
        let o = NativeStateOverlay::new(db.clone());
        for op in per_key.values().flatten() {
            op.on_overlay(&o);
        }
        let frozen = o.freeze(1);
        let typed = &runs[0];
        assert_eq!(frozen.rows(CF_NATIVE_POSITIONS), typed.positions, "seed {seed}: positions vs the overlay");
        assert_eq!(frozen.rows(CF_NATIVE_BALANCES), typed.balances, "seed {seed}: balances vs the overlay");
    }
    println!("P5 T1.1 sorted-and-once: rows drained {drained}, refused calls {refused}");
    assert!(drained > 2_000 && refused > 0, "non-vacuous: {drained}, {refused}");
}

/// Plan Task 1.1: `opaque_trader_stays_raw`. A trader with an irregular row
/// keeps its rows raw: typed writes to it (position set and removed,
/// balance, `cvlm` added) change its raw rows, and the drain serializes none
/// of its rows from a record (its lenient row comes out byte for byte, `is_long`
/// byte 2 kept). A regular trader written typed and then given an irregular
/// row in the same block turns opaque: its earlier typed rows drain raw too.
/// Once its irregular row is gone the trader is regular again and drains
/// from its record. `add_cum_volume` on a 17-byte `cvlm` row is refused and
/// changes nothing.
#[test]
fn opaque_trader_stays_raw() {
    let (a, b, c) = (trader(0), trader(1), trader(2));
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let put = |cf, k: &[u8], v: &[u8]| db.put_cf_raw(cf, k, v).unwrap();
    put(CF_NATIVE_POSITIONS, &position_key(&a, 1), &bytes(&position(a, 1, 3, true)));
    put(CF_NATIVE_BALANCES, a.as_slice(), &bal_bytes(&balance(4)));
    put(CF_NATIVE_POSITIONS, &position_key(&b, 1), &bytes(&position(b, 1, 6, false)));
    let odd = lenient(&position(b, 7, 8, true), 0);
    put(CF_NATIVE_POSITIONS, &position_key(&b, 7), &odd);
    put(CF_NATIVE_BALANCES, b.as_slice(), &bal_bytes(&balance(9)));
    put(CF_NATIVE_BALANCES, &cum_volume_key(&b), &volume_bytes(40));
    put(CF_NATIVE_POSITIONS, &position_key(&c, 2), &bytes(&position(c, 2, 1, true)));
    put(CF_NATIVE_BALANCES, &cum_volume_key(&c), &[0u8; 17]);
    let mut users = Users::build(&ResidentRows::build(&db).unwrap());
    assert!(users.get(&a).is_some() && users.get(&b).is_none() && users.get(&c).is_none(), "a regular, b and c opaque");

    // Typed writes to opaque b stay raw.
    let pb = position(b, 2, 11, false);
    let mut w = users.mutate();
    w.set_position(pb.clone());
    w.remove_position(&b, 1);
    w.remove_position(&b, 5); // no row: a tombstone all the same
    w.set_balance(&b, balance(12));
    w.add_cum_volume(&b, FixedPoint::from_raw(2)).unwrap();
    let odd2 = lenient(&position(b, 7, 13, true), 0);
    w.put_row(POSITIONS, &position_key(&b, 7), Some(&odd2));
    assert!(users.get(&b).is_none(), "b stays opaque");
    let before = Users::drain_sources();
    let d = users.drain_dirty();
    let after = Users::drain_sources();
    assert_eq!(after.0 - before.0, 0, "no row of opaque b serialized from a record");
    assert_eq!(after.1 - before.1, 4, "b's four rows drained raw");
    assert_eq!(
        d.positions,
        vec![
            (position_key(&b, 1).to_vec(), None),
            (position_key(&b, 2).to_vec(), Some(bytes(&pb))),
            (position_key(&b, 5).to_vec(), None),
            (position_key(&b, 7).to_vec(), Some(odd2.clone())),
        ]
    );
    assert_eq!(odd2[1 + 20 + 8], 2, "the lenient byte is kept");
    assert_eq!(
        d.balances,
        vec![(b.to_vec(), Some(bal_bytes(&balance(12)))), (cum_volume_key(&b).to_vec(), Some(volume_bytes(42)))]
    );

    // Regular a: a typed write, then an irregular row in the same block.
    let pa = position(a, 3, 5, true);
    let odd_a = lenient(&position(a, 4, 6, false), 1);
    users.mutate().set_position(pa.clone());
    assert!(users.get(&a).is_some());
    users.mutate().put_row(POSITIONS, &position_key(&a, 4), Some(&odd_a));
    assert!(users.get(&a).is_none(), "a turned opaque");
    let before = Users::drain_sources();
    let d = users.drain_dirty();
    let after = Users::drain_sources();
    assert_eq!((after.0 - before.0, after.1 - before.1), (0, 2), "a's rows drained raw");
    assert_eq!(
        d.positions,
        vec![(position_key(&a, 3).to_vec(), Some(bytes(&pa))), (position_key(&a, 4).to_vec(), Some(odd_a))]
    );
    assert!(d.balances.is_empty());

    // b's irregular row deleted: b is regular again, drained from its record.
    users.mutate().put_row(POSITIONS, &position_key(&b, 7), None);
    assert!(users.get(&b).is_some(), "b regular again");
    users.mutate().set_balance(&b, balance(14));
    let before = Users::drain_sources();
    let d = users.drain_dirty();
    let after = Users::drain_sources();
    assert_eq!((after.0 - before.0, after.1 - before.1), (1, 0), "b's balance drained from its record");
    assert_eq!(d.positions, vec![(position_key(&b, 7).to_vec(), None)]);
    assert_eq!(d.balances, vec![(b.to_vec(), Some(bal_bytes(&balance(14))))]);

    // A 17-byte `cvlm` row: the add is refused, nothing changes.
    let rows_before = users.encode_rows(CF_NATIVE_BALANCES);
    assert!(users.mutate().add_cum_volume(&c, FixedPoint::from_raw(1)).is_err());
    assert_eq!(users.dirty_keys(), 0, "a refused add marks nothing");
    assert_eq!(users.encode_rows(CF_NATIVE_BALANCES), rows_before);
    assert!(users.self_consistent());
}

/// Plan section 3 "Order" (step 1 measures): a block's dirty keys in a
/// `BTreeSet` vs a hash set sorted at the drain. ~120k position keys over
/// 60k distinct `(trader, market)` and 40k balance keys over 20k traders
/// (about a 300-market native block at ~49k fills), inserted in fill order,
/// then emitted sorted. Ignored: a timing, not a check.
#[test]
#[ignore = "ubench: run with --ignored --nocapture"]
fn ubench_dirty_set_btree_vs_hash_sort() {
    let mut rng = Lcg(7);
    let traders: Vec<Address> = (0..20_000u64).map(|i| {
        let mut b = [0u8; 20];
        b[..8].copy_from_slice(&i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes());
        Address::new(b)
    }).collect();
    let pos: Vec<(Address, MarketId)> =
        (0..120_000).map(|_| (traders[rng.below(20_000) as usize], rng.below(300))).collect();
    let bal: Vec<Address> = (0..40_000).map(|_| traders[rng.below(20_000) as usize]).collect();
    let best = |f: &dyn Fn() -> usize| (0..7).map(|_| {
        let t = std::time::Instant::now();
        std::hint::black_box(f());
        t.elapsed()
    }).min().unwrap();
    let btree = best(&|| {
        let mut p = BTreeSet::new();
        let mut b = BTreeSet::new();
        pos.iter().for_each(|k| { p.insert(*k); });
        bal.iter().for_each(|k| { b.insert(*k); });
        p.iter().count() + b.iter().count()
    });
    let hash = best(&|| {
        let mut p: HashSet<(Address, MarketId)> = HashSet::default();
        let mut b: HashSet<Address> = HashSet::default();
        pos.iter().for_each(|k| { p.insert(*k); });
        bal.iter().for_each(|k| { b.insert(*k); });
        let mut p: Vec<_> = p.into_iter().collect();
        let mut b: Vec<_> = b.into_iter().collect();
        p.sort_unstable();
        b.sort_unstable();
        p.len() + b.len()
    });
    println!("DIRTY SET ubench: {} position + {} balance inserts: BTreeSet {:?}, hash set + sort {:?}", pos.len(), bal.len(), btree, hash);
}
