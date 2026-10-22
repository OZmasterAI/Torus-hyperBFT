//! Item 6 Phase 4 step 4 (`docs/plans/item6-phase4-impl.md`): typed loads.
//!
//! `PositionManager::{get_position, get_native_balance, get_cum_volume}_typed`,
//! `PositionCache::load_typed` and `apply_fill_cached_effect_typed` read the
//! backend's own pending row first (bytes, decoded as before), else ask the
//! typed source; a source that declines (an opaque trader), a backend that
//! cannot tell (`OwnPendingRow::Unknown`) or no source read `get_cf_raw`.
//! The answers equal the untyped reads in every case, errors included.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use borsh::BorshDeserialize;
use torus_core::position::{
    cum_volume_key, position_key, MarginType, NativeBalance, Position, PositionCache, PositionManager, TypedRows,
};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{AtomicWriteOp, OwnPendingRow, StateBackend, StateError};
use torus_types::{Address, FixedPoint, MarketId};

type Key = (String, Vec<u8>);

/// R (the resident rows) under an own pending set; counts `get_cf_raw`.
#[derive(Default)]
struct FakeState {
    r: BTreeMap<Key, Vec<u8>>,
    pending: BTreeMap<Key, Option<Vec<u8>>>,
    /// R attached: `own_pending_row` answers; else `Unknown`.
    resident: bool,
    raw_reads: usize,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    /// A deep copy (clones of `Fake` share the state).
    fn fork(&self) -> Self {
        let s = self.0.lock().unwrap();
        Fake(Arc::new(Mutex::new(FakeState {
            r: s.r.clone(),
            pending: s.pending.clone(),
            resident: s.resident,
            raw_reads: 0,
        })))
    }
    fn raw_reads(&self) -> usize {
        self.0.lock().unwrap().raw_reads
    }
    fn pending(&self) -> BTreeMap<Key, Option<Vec<u8>>> {
        self.0.lock().unwrap().pending.clone()
    }
}

impl StateBackend for Fake {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let mut s = self.0.lock().unwrap();
        s.raw_reads += 1;
        let k = (cf.to_string(), key.to_vec());
        if let Some(row) = s.pending.get(&k) {
            return Ok(row.clone());
        }
        Ok(s.r.get(&k).cloned())
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.0.lock().unwrap().pending.insert((cf.to_string(), key.to_vec()), Some(value.to_vec()));
        Ok(())
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.0.lock().unwrap().pending.insert((cf.to_string(), key.to_vec()), None);
        Ok(())
    }
    fn iterate_cf(&self, _cf: &str, _prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        unimplemented!("not read by the typed loads")
    }
    fn atomic_write(&self, _ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        unimplemented!("not written by the typed loads")
    }
    fn own_pending_row(&self, cf: &str, key: &[u8]) -> OwnPendingRow {
        let s = self.0.lock().unwrap();
        if !s.resident {
            return OwnPendingRow::Unknown;
        }
        match s.pending.get(&(cf.to_string(), key.to_vec())) {
            Some(row) => OwnPendingRow::Pending(row.clone()),
            None => OwnPendingRow::Resident,
        }
    }
}

/// R decoded per trader (what `Users` holds); `opaque` traders decline.
#[derive(Default)]
struct FakeTyped {
    positions: HashMap<(Address, MarketId), Position>,
    balances: HashMap<Address, NativeBalance>,
    volumes: HashMap<Address, FixedPoint>,
    opaque: HashSet<Address>,
    answered: AtomicUsize,
    declined: AtomicUsize,
}

impl FakeTyped {
    fn of(r: &BTreeMap<Key, Vec<u8>>, opaque: HashSet<Address>) -> Self {
        let mut out = FakeTyped { opaque, ..FakeTyped::default() };
        for ((cf, k), v) in r {
            if cf == CF_NATIVE_POSITIONS {
                let p = Position::try_from_slice(v).unwrap();
                out.positions.insert((p.trader, p.market_id), p);
            } else if k.len() == 20 {
                out.balances.insert(Address::from_slice(k), NativeBalance::try_from_slice(v).unwrap());
            } else {
                let raw: [u8; 16] = v.as_slice().try_into().unwrap();
                out.volumes.insert(Address::from_slice(&k[4..]), FixedPoint::from_raw(i128::from_be_bytes(raw)));
            }
        }
        out
    }
    fn counts(&self) -> (usize, usize) {
        (self.answered.load(Ordering::Relaxed), self.declined.load(Ordering::Relaxed))
    }
    fn answer<V>(&self, t: &Address, v: impl FnOnce() -> Option<V>) -> Option<Option<V>> {
        if self.opaque.contains(t) {
            self.declined.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.answered.fetch_add(1, Ordering::Relaxed);
        Some(v())
    }
}

impl TypedRows for FakeTyped {
    fn position(&self, trader: &Address, market_id: MarketId) -> Option<Option<Position>> {
        self.answer(trader, || self.positions.get(&(*trader, market_id)).cloned())
    }
    fn balance(&self, trader: &Address) -> Option<Option<NativeBalance>> {
        self.answer(trader, || self.balances.get(trader).cloned())
    }
    fn cum_volume(&self, trader: &Address) -> Option<Option<FixedPoint>> {
        self.answer(trader, || self.volumes.get(trader).copied())
    }
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn pos(t: u8, m: MarketId, size: i64) -> Position {
    Position {
        trader: addr(t),
        market_id: m,
        is_long: size % 2 == 0,
        size: fp(size),
        entry_price: fp(100 + size),
        cost_basis: fp(100 + size) * fp(size),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
        funding: None,
    }
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

const TRADERS: u8 = 6;
const MARKETS: MarketId = 3;
/// Trader 6 is opaque.
const OPAQUE: u8 = TRADERS;

/// Random R and own pending rows over traders 1..=6 x markets 1..=3 (every
/// key: in R or not; pending none / a write / a tombstone).
fn random_state(rng: &mut Lcg) -> Fake {
    let fake = Fake::default();
    {
        let mut guard = fake.0.lock().unwrap();
        let s: &mut FakeState = &mut guard;
        let mut rows: Vec<(&str, Vec<u8>, Vec<u8>)> = Vec::new();
        for t in 1..=TRADERS {
            for m in 1..=MARKETS {
                rows.push((CF_NATIVE_POSITIONS, position_key(&addr(t), m).to_vec(), borsh::to_vec(&pos(t, m, 1 + t as i64 * m as i64)).unwrap()));
            }
            let bal = NativeBalance { available: fp(1_000 * t as i64), order_margin: fp(t as i64) };
            rows.push((CF_NATIVE_BALANCES, addr(t).to_vec(), borsh::to_vec(&bal).unwrap()));
            rows.push((CF_NATIVE_BALANCES, cum_volume_key(&addr(t)).to_vec(), fp(77 * t as i64).raw().to_be_bytes().to_vec()));
        }
        for (cf, k, v) in rows {
            if rng.below(3) != 0 {
                s.r.insert((cf.to_string(), k), v);
            }
        }
        let keys: Vec<Key> = (1..=TRADERS)
            .flat_map(|t| {
                (1..=MARKETS)
                    .map(move |m| (CF_NATIVE_POSITIONS.to_string(), position_key(&addr(t), m).to_vec()))
                    .chain([
                        (CF_NATIVE_BALANCES.to_string(), addr(t).to_vec()),
                        (CF_NATIVE_BALANCES.to_string(), cum_volume_key(&addr(t)).to_vec()),
                    ])
            })
            .collect();
        for (cf, k) in keys {
            let row = match rng.below(4) {
                0 | 1 => continue,
                2 => None,
                _ if cf == CF_NATIVE_POSITIONS => {
                    let t = k[0];
                    let m = MarketId::from_be_bytes(k[20..28].try_into().unwrap());
                    Some(borsh::to_vec(&pos(t, m, 50 + rng.below(9) as i64)).unwrap())
                }
                _ if k.len() == 20 => {
                    Some(borsh::to_vec(&NativeBalance { available: fp(rng.below(500) as i64), order_margin: FixedPoint::ZERO }).unwrap())
                }
                _ => Some(fp(rng.below(500) as i64).raw().to_be_bytes().to_vec()),
            };
            s.pending.insert((cf, k), row);
        }
        s.resident = true;
        drop(guard);
    }
    fake
}

/// Every typed read == the untyped read; the source is asked exactly for
/// keys without an own pending row (answered for a regular trader, declined
/// for the opaque one); `get_cf_raw` runs exactly when the source is not
/// used or declines. Without R (`Unknown`) or without a source: untyped.
#[test]
fn typed_loads_read_own_pending_then_the_record() {
    let mut rng = Lcg(0x7E5_7ED);
    let (mut clean, mut pending, mut declined) = (0usize, 0usize, 0usize);
    for round in 0..40 {
        let fake = random_state(&mut rng);
        if round % 8 == 7 {
            fake.0.lock().unwrap().resident = false;
        }
        let resident = fake.0.lock().unwrap().resident;
        let typed = FakeTyped::of(&fake.0.lock().unwrap().r, HashSet::from_iter([addr(OPAQUE)]));
        let pm = PositionManager::new(fake.clone());
        // One typed read: the result == `want`, and the counters move as the
        // key's class says.
        let mut check = |cf: &str, key: Vec<u8>, t: u8, read: &dyn Fn() -> String, want: String, tag: String| {
            let is_pending = fake.0.lock().unwrap().pending.contains_key(&(cf.to_string(), key));
            let (a0, d0, r0) = (typed.counts().0, typed.counts().1, fake.raw_reads());
            assert_eq!(read(), want, "round {round}: {tag}");
            let (a1, d1, r1) = (typed.counts().0, typed.counts().1, fake.raw_reads());
            let (asked, raw) = match (resident, is_pending, t == OPAQUE) {
                (false, _, _) => ((0, 0), 1),
                (true, true, _) => {
                    pending += 1;
                    ((0, 0), 0)
                }
                (true, false, true) => {
                    declined += 1;
                    ((0, 1), 1)
                }
                (true, false, false) => {
                    clean += 1;
                    ((1, 0), 0)
                }
            };
            assert_eq!(((a1 - a0, d1 - d0), r1 - r0), (asked, raw), "round {round}: {tag}: path");
        };
        for t in 1..=TRADERS {
            let a = addr(t);
            for m in 0..=MARKETS {
                let want = format!("{:?}", pm.get_position(&a, m));
                let key = position_key(&a, m).to_vec();
                check(CF_NATIVE_POSITIONS, key.clone(), t, &|| format!("{:?}", pm.get_position_typed(Some(&typed), &a, m)), want.clone(), format!("position {t} {m}"));
                check(
                    CF_NATIVE_POSITIONS,
                    key,
                    t,
                    &|| format!("{:?}", PositionCache::new().load_typed(&pm, Some(&typed), &a, m)),
                    want.clone(),
                    format!("cache load {t} {m}"),
                );
                // No source: the untyped read.
                let r0 = fake.raw_reads();
                assert_eq!(format!("{:?}", pm.get_position_typed(None, &a, m)), want, "round {round}: no source");
                assert_eq!(fake.raw_reads(), r0 + 1, "round {round}: no source reads the bytes");
            }
            let want = format!("{:?}", pm.get_native_balance(&a));
            check(CF_NATIVE_BALANCES, a.to_vec(), t, &|| format!("{:?}", pm.get_native_balance_typed(Some(&typed), &a)), want, format!("balance {t}"));
            let want = format!("{:?}", pm.get_cum_volume(&a));
            check(
                CF_NATIVE_BALANCES,
                cum_volume_key(&a).to_vec(),
                t,
                &|| format!("{:?}", pm.get_cum_volume_typed(Some(&typed), &a)),
                want,
                format!("cum_volume {t}"),
            );
        }
        // The fill path (`update_in_place`): typed and untyped give the same
        // effects and the same flushed rows.
        let (f1, f2) = (fake.fork(), fake.fork());
        let (pm1, pm2) = (PositionManager::new(f1.clone()), PositionManager::new(f2.clone()));
        let (mut c1, mut c2) = (PositionCache::new(), PositionCache::new());
        for _ in 0..12 {
            let t = addr(1 + rng.below(TRADERS as u64) as u8);
            let m = 1 + rng.below(MARKETS);
            let (buy, qty, px) = (rng.below(2) == 0, fp(1 + rng.below(80) as i64), fp(90 + rng.below(20) as i64));
            let e1 = pm1.apply_fill_cached_effect_typed(&mut c1, Some(&typed), &t, m, buy, qty, px, MarginType::Cross);
            let e2 = pm2.apply_fill_cached_effect(&mut c2, &t, m, buy, qty, px, MarginType::Cross);
            assert_eq!(format!("{e1:?}"), format!("{e2:?}"), "round {round}: fill effect");
        }
        c1.flush_all(&pm1).unwrap();
        c2.flush_all(&pm2).unwrap();
        assert_eq!(f1.pending(), f2.pending(), "round {round}: flushed rows");
    }
    assert!(clean > 300 && pending > 300 && declined > 30, "classes covered: clean {clean}, pending {pending}, declined {declined}");
}

/// An own pending row that does not decode fails the typed read with the
/// untyped read's error (position, balance, `cum_volume`).
#[test]
fn typed_loads_of_bad_pending_bytes_fail_as_untyped() {
    let fake = Fake::default();
    fake.0.lock().unwrap().resident = true;
    let typed = FakeTyped::default();
    let pm = PositionManager::new(fake.clone());
    let a = addr(1);
    fake.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&a, 1), &[2, 0, 1]).unwrap();
    fake.put_cf_raw(CF_NATIVE_BALANCES, a.as_slice(), &[9; 5]).unwrap();
    fake.put_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&a), &[1; 15]).unwrap();
    let want = format!("{:?}", pm.get_position(&a, 1));
    assert!(want.starts_with("Err"), "{want}");
    assert_eq!(format!("{:?}", pm.get_position_typed(Some(&typed), &a, 1)), want);
    assert_eq!(format!("{:?}", PositionCache::new().load_typed(&pm, Some(&typed), &a, 1)), want);
    let want = format!("{:?}", pm.get_native_balance(&a));
    assert!(want.starts_with("Err"), "{want}");
    assert_eq!(format!("{:?}", pm.get_native_balance_typed(Some(&typed), &a)), want);
    let want = format!("{:?}", pm.get_cum_volume(&a));
    assert!(want.starts_with("Err"), "{want}");
    assert_eq!(format!("{:?}", pm.get_cum_volume_typed(Some(&typed), &a)), want);
    assert_eq!(typed.counts(), (0, 0), "the source is never asked for a pending key");
}
