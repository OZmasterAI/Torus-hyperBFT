//! Item 6 Phase 4 step 1 (`docs/plans/item6-phase4-impl.md` Task 1.1): the
//! record set [`Users`] — one record per trader of R's `CF_NATIVE_POSITIONS`
//! and `CF_NATIVE_BALANCES` rows — kept in the resident rows slot next to R
//! and updated from each block's [`ResidentDelta`] at `end_resident` (built
//! cold with R). It replaces C7's `TraderPositions` (decoded positions, E2's
//! trader set, adl-budget C2's holder lists) and also follows the balance and
//! `cvlm` rows. Node-local, never consensus-visible, no format change. Step 1:
//! raw R still serves every read; the record set is checked against R.
//!
//! A trader's rows: every positions key under its 20-byte prefix, its 20-byte
//! balance row and its 24-byte `cvlm ‖ trader` row. The trader is regular
//! when every one of them decodes AND re-encodes to the same bytes: a
//! 28-byte key `t ‖ m` (big-endian, so key order = market order) whose value
//! is a position of trader `t` and market `m`; a balance value that is a
//! `NativeBalance`; a 16-byte `cvlm` value. Its record ([`UserState`]) is then
//! exactly what reading R through the overlay gives a trader with nothing
//! pending: `positions_for_trader` = its positions in key order,
//! `get_position(t, m)` = the row at `t ‖ m`, its balance and volume rows
//! (absent != zero). Any other trader (a longer or bare-prefix key, a value
//! that does not decode, names another trader / market, or decodes
//! leniently — `is_long` byte 2, `margin_type` byte 7 — so that it would not
//! re-encode to its bytes) is opaque: it has no record, its rows are kept raw
//! and it reads through the overlay, as today. Rows under no trader
//! (positions keys shorter than 20 bytes, balances keys neither 20 bytes nor
//! `cvlm` + 20 bytes) are kept raw too, so the record set holds every row of
//! the two CFs ([`Users::encode_rows`] == R's) and follows a delta without R.
//! Readers use a record only for a trader with no own pending position
//! writes in the block (`AccountReader`).
//!
//! Item 6 E2: the traders of R's 28-byte keys, sorted, kept with the records
//! (same lifecycle), so the liquidation step's candidate list
//! (`liq::traders_after`) is a slice of them instead of one seek over R per
//! trader ([`Users::traders_after`]). A balance-only user is not in it.
//!
//! adl-budget C2 (owner 18c s96 / s99): per market, the traders of R's keys
//! `t ‖ m` (same lifecycle), so an ADL ranking of `m` reads the holders of
//! `m` (merged with the block's dirty traders of `m`) instead of every
//! trader ([`Users::holders_with`], [`dirty_by_market`]).

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::map::{HashMap, HashSet};
use std::ops::Bound;

use alloy_primitives::Address;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_core::position::{cum_volume_key, NativeBalance, Position, POSITION_V2_LEN};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::error::StateError;
use torus_state::{ResidentDelta, ResidentRows, StateBackend};
use torus_types::{FixedPoint, MarketId};

use super::SumsResult;

/// A positions key's trader prefix / a regular key's length.
const TRADER: usize = 20;
const KEY: usize = 28;
/// A `cvlm ‖ trader` key of the balances CF.
const CVLM: &[u8] = b"cvlm";
const CVLM_KEY: usize = 24;
/// A v2 position row's two flag bytes (`Position`'s Borsh decode reads any
/// non-zero byte as long / Isolated).
const IS_LONG_AT: usize = 1 + 20 + 8;
const MARGIN_TYPE_AT: usize = POSITION_V2_LEN - 1;

/// Item 6 step 1: one changed row of a regular trader at the end of a block:
/// its position before the block and after (`None`: no row).
pub(crate) type Change = (Option<Position>, Option<Position>);

/// Item 6 step 1: [`Users::apply`]'s per-trader report.
pub(crate) type Seen<'a> = dyn FnMut(&Address, Option<(&[Change], &[Position])>) + 'a;

/// One regular trader's record: its rows of both CFs, decoded.
#[derive(Debug, Default)]
pub(crate) struct UserState {
    /// `None`: no balance row (absent != zero: a different row set).
    pub(crate) balance: Option<NativeBalance>,
    /// `None`: no `cvlm` row.
    pub(crate) cum_volume: Option<FixedPoint>,
    /// Market order (= key order), without growth slack after a rebuild.
    pub(crate) positions: Vec<Position>,
    /// Phase 4 step 3 (plan section 3): the trader's margin sums with their
    /// mark version, boxed so a light user pays 8 bytes. Always `None` in
    /// step 1 (`SumsCache` still holds the sums).
    #[allow(dead_code)]
    pub(crate) sums: Option<Box<(u64, SumsResult)>>,
}

impl UserState {
    fn is_empty(&self) -> bool {
        self.balance.is_none() && self.cum_volume.is_none() && self.positions.is_empty()
    }

    /// `t`'s rows of both CFs, as R holds them.
    fn raw(&self, t: &Address) -> RawRows {
        let mut out = RawRows::default();
        for p in &self.positions {
            out.positions.insert([t.as_slice(), &p.market_id.to_be_bytes()].concat(), encode(p, POSITION_V2_LEN));
        }
        if let Some(b) = &self.balance {
            out.balances.insert(t.to_vec(), encode(b, 33));
        }
        if let Some(v) = self.cum_volume {
            out.balances.insert(cum_volume_key(t).to_vec(), v.raw().to_be_bytes().to_vec());
        }
        out
    }
}

/// Raw rows of the two CFs: an opaque trader's, or the rows under no trader.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RawRows {
    positions: BTreeMap<Vec<u8>, Vec<u8>>,
    balances: BTreeMap<Vec<u8>, Vec<u8>>,
}

#[derive(Debug, Default)]
pub(crate) struct Users {
    /// Regular traders with at least one row (decision 3 a: a hash map).
    map: HashMap<Address, UserState>,
    /// Traders with an irregular row (normally none): their rows, raw.
    opaque: HashMap<Address, RawRows>,
    /// Rows under no trader (normally none), raw.
    other: RawRows,
    /// Item 6 E2: every trader with a 28-byte key in R (regular or opaque),
    /// ascending: the candidates of the liquidation walk.
    traders: BTreeSet<Address>,
    /// adl-budget C2: per market `m`, every trader with the 28-byte key
    /// `t ‖ m` in R (regular or opaque). A market without one has no entry.
    /// A regular trader's keys are followed on its record (only a key that
    /// appears or disappears touches the index); an opaque or re-decoded
    /// trader's per key from the delta (a write adds, a tombstone removes).
    /// foldhash (alloy's map and set), unordered: the only iteration is
    /// [`Self::holders_with`], which sorts (c2-holder-hashset).
    holders: Holders,
}

type Holders = HashMap<MarketId, HashSet<Address>>;

/// Which row of its trader a balances key is.
#[derive(Clone, Copy)]
enum BalanceRow {
    Balance,
    Volume,
}

/// The trader of a balances key and which of its rows it is; `None`: a row
/// under no trader.
fn balance_owner(key: &[u8]) -> Option<(Address, BalanceRow)> {
    match key.len() {
        TRADER => Some((Address::from_slice(key), BalanceRow::Balance)),
        CVLM_KEY if key.starts_with(CVLM) => Some((Address::from_slice(&key[CVLM.len()..]), BalanceRow::Volume)),
        _ => None,
    }
}

/// One trader's writes (`Some(Some)`) / tombstones (`Some(None)`) of its
/// balance and `cvlm` rows in a block (`None`: not written).
#[derive(Clone, Copy, Default)]
struct BalanceDelta<'a> {
    balance: Option<Option<&'a [u8]>>,
    volume: Option<Option<&'a [u8]>>,
}

/// The market of a 28-byte positions key `t ‖ m`.
fn market_of(key: &[u8]) -> MarketId {
    MarketId::from_be_bytes(key[TRADER..KEY].try_into().expect("28-byte key"))
}

fn encode<T: BorshSerialize>(x: &T, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    x.serialize(&mut out).expect("writing to a Vec cannot fail");
    out
}

/// The position of a regular row, `None` for an irregular one.
fn decode(key: &[u8], value: &[u8]) -> Option<Position> {
    if key.len() != KEY {
        return None;
    }
    let p = Position::try_from_slice(value).ok()?;
    // The decode reads exactly the row's bytes (version 2, every field fixed
    // width) and every field but the two flags raw, so the row re-encodes
    // to itself iff both flags hold 0 or 1.
    let canonical = value.len() == POSITION_V2_LEN
        && value[IS_LONG_AT] == u8::from(p.is_long)
        && value[MARGIN_TYPE_AT] == p.margin_type as u8;
    debug_assert_eq!(canonical, encode(&p, POSITION_V2_LEN) == value, "position re-encode check");
    (canonical && p.trader.as_slice() == &key[..TRADER] && p.market_id.to_be_bytes() == key[TRADER..]).then_some(p)
}

/// The balance of a regular balance row (every byte of a v1 row is read raw:
/// a row that decodes re-encodes to itself).
fn decode_balance(value: &[u8]) -> Option<NativeBalance> {
    let b = NativeBalance::try_from_slice(value).ok()?;
    debug_assert!(encode(&b, 33) == value, "balance re-encode check");
    Some(b)
}

/// The volume of a regular `cvlm` row: a raw `FixedPoint`, 16 bytes BE.
fn decode_volume(value: &[u8]) -> Option<FixedPoint> {
    Some(FixedPoint::from_raw(i128::from_be_bytes(value.try_into().ok()?)))
}

/// A write / tombstone of `key` on `map`.
fn put(map: &mut BTreeMap<Vec<u8>, Vec<u8>>, key: &[u8], value: Option<&[u8]>) {
    match value {
        Some(v) => {
            map.insert(key.to_vec(), v.to_vec());
        }
        None => {
            map.remove(key);
        }
    }
}

/// `t`'s record from its rows (`Ok(None)`: no rows), `Err`: an irregular
/// row (the rows come back).
fn classify(raw: RawRows) -> Result<Option<UserState>, RawRows> {
    let mut u = UserState::default();
    for (k, v) in &raw.positions {
        match decode(k, v) {
            Some(p) => u.positions.push(p),
            None => return Err(raw),
        }
    }
    for (k, v) in &raw.balances {
        let regular = match balance_owner(k) {
            Some((_, BalanceRow::Balance)) => decode_balance(v).map(|b| u.balance = Some(b)).is_some(),
            Some((_, BalanceRow::Volume)) => decode_volume(v).map(|x| u.cum_volume = Some(x)).is_some(),
            None => false,
        };
        if !regular {
            return Err(raw);
        }
    }
    if u.is_empty() {
        return Ok(None);
    }
    u.positions.shrink_to_fit();
    Ok(Some(u))
}

impl Users {
    /// Every trader of R's positions and balances (cold: with R, or when a
    /// block's state did not come back).
    pub(crate) fn build(rows: &ResidentRows) -> Self {
        let mut out = Self::default();
        let empty = BTreeMap::new();
        let pos = rows.rows(CF_NATIVE_POSITIONS).unwrap_or(&empty);
        let bal = rows.rows(CF_NATIVE_BALANCES).unwrap_or(&empty);
        // Traders with an irregular row: their rows are copied raw below.
        let mut bad: HashSet<Address> = HashSet::default();
        let mut cur: Option<(Address, Option<Vec<Position>>)> = None;
        let mut flush = |out: &mut Self, cur: Option<(Address, Option<Vec<Position>>)>| match cur {
            Some((t, Some(mut ps))) => {
                // ~112 bytes per position, without the build's growth slack.
                ps.shrink_to_fit();
                out.map.entry(t).or_default().positions = ps;
            }
            Some((t, None)) => {
                bad.insert(t);
            }
            None => {}
        };
        for (k, v) in pos {
            if k.len() < TRADER {
                out.other.positions.insert(k.clone(), v.clone());
                continue;
            }
            if k.len() == KEY {
                let t = Address::from_slice(&k[..TRADER]);
                out.traders.insert(t);
                out.holders.entry(market_of(k)).or_default().insert(t);
            }
            if cur.as_ref().is_none_or(|(t, _)| t.as_slice() != &k[..TRADER]) {
                flush(&mut out, cur.take());
                cur = Some((Address::from_slice(&k[..TRADER]), Some(Vec::new())));
            }
            if let Some((_, slot)) = cur.as_mut() {
                *slot = slot.take().and_then(|mut ps| {
                    ps.push(decode(k, v)?);
                    Some(ps)
                });
            }
        }
        flush(&mut out, cur);
        for (k, v) in bal {
            let Some((t, row)) = balance_owner(k) else {
                out.other.balances.insert(k.clone(), v.clone());
                continue;
            };
            if bad.contains(&t) {
                continue;
            }
            let regular = match row {
                BalanceRow::Balance => decode_balance(v).map(|b| out.map.entry(t).or_default().balance = Some(b)).is_some(),
                BalanceRow::Volume => decode_volume(v).map(|x| out.map.entry(t).or_default().cum_volume = Some(x)).is_some(),
            };
            if !regular {
                bad.insert(t);
            }
        }
        for t in bad {
            out.map.remove(&t);
            let mut raw = RawRows::default();
            for (k, v) in pos.range::<[u8], _>((Bound::Included(t.as_slice()), Bound::Unbounded)) {
                if !k.starts_with(t.as_slice()) {
                    break;
                }
                raw.positions.insert(k.clone(), v.clone());
            }
            for k in [t.to_vec(), cum_volume_key(&t).to_vec()] {
                if let Some(v) = bal.get(&k) {
                    raw.balances.insert(k, v.clone());
                }
            }
            out.opaque.insert(t, raw);
        }
        out
    }

    /// `t`'s rows (`raw`) as its record, opaque, or nothing.
    fn set(&mut self, t: Address, raw: RawRows) {
        match classify(raw) {
            Ok(Some(u)) => {
                self.opaque.remove(&t);
                self.map.insert(t, u);
            }
            Ok(None) => {
                self.opaque.remove(&t);
                self.map.remove(&t);
            }
            Err(raw) => {
                self.map.remove(&t);
                self.opaque.insert(t, raw);
            }
        }
    }

    /// End of a block: follow `delta` (the block's own writes and tombstones
    /// of both CFs). A regular write or a tombstone of a regular trader
    /// updates its record in place (only the written rows decode); any other
    /// change of a trader re-decodes its rows (its record re-encoded, or its
    /// raw rows, with the delta on them). Needs no R: the record set holds
    /// every row of the two CFs.
    ///
    /// Item 6 step 1: `seen` (if any) is called once per trader the delta
    /// writes under in the positions CF (key order), right after its rows are
    /// followed: `Some((changes, now))` for a trader regular before and after
    /// the block, with each changed position row's position before / after
    /// (composing exactly from its positions before the block to `now`, its
    /// positions after); `None` when its rows are re-decoded (opaque before,
    /// or an irregular write in either CF).
    pub(crate) fn apply(&mut self, delta: &ResidentDelta, mut seen: Option<&mut Seen<'_>>) {
        // A trader's balance and `cvlm` rows are not adjacent in key order:
        // gathered per trader first.
        let mut balances: HashMap<Address, BalanceDelta<'_>> = HashMap::default();
        for (k, v) in delta.entries(CF_NATIVE_BALANCES) {
            match balance_owner(k) {
                Some((t, BalanceRow::Balance)) => balances.entry(t).or_default().balance = Some(v),
                Some((t, BalanceRow::Volume)) => balances.entry(t).or_default().volume = Some(v),
                None => put(&mut self.other.balances, k, v),
            }
        }
        let mut group: Vec<(&[u8], Option<&[u8]>)> = Vec::new();
        let mut decoded: Vec<Option<Position>> = Vec::new();
        let mut changes: Vec<Change> = Vec::new();
        // The delta is key-sorted: a trader's entries are adjacent (a key
        // shorter than a trader prefix sorts outside every trader's range).
        let mut entries = delta.entries(CF_NATIVE_POSITIONS).peekable();
        while let Some(first) = entries.next() {
            if first.0.len() < TRADER {
                put(&mut self.other.positions, first.0, first.1);
                continue;
            }
            group.clear();
            group.push(first);
            while let Some(e) = entries.next_if(|(k, _)| k.len() >= TRADER && k[..TRADER] == first.0[..TRADER]) {
                group.push(e);
            }
            let t = Address::from_slice(&first.0[..TRADER]);
            let b = balances.remove(&t).unwrap_or_default();
            changes.clear();
            if !self.follow(t, &group, b, &mut decoded, &mut changes, seen.as_deref_mut()) {
                self.reload(t, &group, b);
                if let Some(f) = seen.as_deref_mut() {
                    f(&t, None);
                }
            }
        }
        // Traders whose balance / `cvlm` rows alone the block wrote.
        for (t, b) in balances {
            if !self.follow(t, &[], b, &mut decoded, &mut changes, None) {
                self.reload(t, &[], b);
            }
        }
    }

    /// One trader's entries of the delta (`group`: its positions entries, key
    /// order; `b`: its balance / `cvlm` entries) on its record; `false`
    /// (nothing changed): its rows must be re-decoded (opaque, or an
    /// irregular write). Every write is decoded before the record changes.
    /// C2: a key it adds to or removes from the record moves its holder.
    /// With `seen`, the changed positions are collected into `changes` and
    /// handed to it with the positions after them.
    fn follow(
        &mut self,
        t: Address,
        group: &[(&[u8], Option<&[u8]>)],
        b: BalanceDelta<'_>,
        decoded: &mut Vec<Option<Position>>,
        changes: &mut Vec<Change>,
        seen: Option<&mut Seen<'_>>,
    ) -> bool {
        if !self.opaque.is_empty() && self.opaque.contains_key(&t) {
            return false;
        }
        decoded.clear();
        for &(k, v) in group {
            match v {
                None => decoded.push(None),
                Some(v) => match decode(k, v) {
                    Some(p) => decoded.push(Some(p)),
                    None => return false,
                },
            }
        }
        let balance = match b.balance {
            Some(Some(v)) => match decode_balance(v) {
                Some(x) => Some(Some(x)),
                None => return false,
            },
            Some(None) => Some(None),
            None => None,
        };
        let volume = match b.volume {
            Some(Some(v)) => match decode_volume(v) {
                Some(x) => Some(Some(x)),
                None => return false,
            },
            Some(None) => Some(None),
            None => None,
        };
        let collect = seen.is_some();
        let u = self.map.entry(t).or_default();
        if let Some(x) = balance {
            u.balance = x;
        }
        if let Some(x) = volume {
            u.cum_volume = x;
        }
        let ps = &mut u.positions;
        let mut keyed = false;
        for (&(k, _), p) in group.iter().zip(decoded.drain(..)) {
            keyed |= k.len() == KEY;
            match p {
                // A regular trader holds no irregular row: nothing to remove.
                None if k.len() != KEY => {}
                None => {
                    let m = market_of(k);
                    if let Ok(i) = ps.binary_search_by_key(&m, |p| p.market_id) {
                        Self::set_holder(&mut self.holders, t, m, false);
                        let old = ps.remove(i);
                        if collect {
                            changes.push((Some(old), None));
                        }
                    }
                }
                Some(p) => {
                    let new = collect.then(|| p.clone());
                    let old = match ps.binary_search_by_key(&p.market_id, |q| q.market_id) {
                        Ok(i) => Some(std::mem::replace(&mut ps[i], p)),
                        Err(i) => {
                            Self::set_holder(&mut self.holders, t, p.market_id, true);
                            ps.insert(i, p);
                            None
                        }
                    };
                    if collect {
                        changes.push((old, new));
                    }
                }
            }
        }
        if let Some(f) = seen {
            f(&t, Some((changes.as_slice(), ps.as_slice())));
        }
        // E2: a regular trader's position rows are all 28-byte keys.
        if keyed {
            if ps.is_empty() {
                self.traders.remove(&t);
            } else {
                self.traders.insert(t);
            }
        }
        if u.is_empty() {
            self.map.remove(&t);
        }
        true
    }

    /// One trader's entries of the delta on its rows (its raw rows if
    /// opaque, else its record re-encoded), then re-decoded. C2 / E2: its
    /// holders move per key from the delta; its trader-set entry follows its
    /// 28-byte keys.
    fn reload(&mut self, t: Address, group: &[(&[u8], Option<&[u8]>)], b: BalanceDelta<'_>) {
        let mut raw = match self.opaque.remove(&t) {
            Some(raw) => raw,
            None => self.map.remove(&t).map(|u| u.raw(&t)).unwrap_or_default(),
        };
        let mut keyed = false;
        for &(k, v) in group {
            put(&mut raw.positions, k, v);
            if k.len() == KEY {
                keyed = true;
                Self::set_holder(&mut self.holders, t, market_of(k), v.is_some());
            }
        }
        if let Some(v) = b.balance {
            put(&mut raw.balances, t.as_slice(), v);
        }
        if let Some(v) = b.volume {
            put(&mut raw.balances, &cum_volume_key(&t), v);
        }
        if keyed {
            if raw.positions.keys().any(|k| k.len() == KEY) {
                self.traders.insert(t);
            } else {
                self.traders.remove(&t);
            }
        }
        self.set(t, raw);
    }

    /// C2: whether R holds `t ‖ m` now.
    fn set_holder(holders: &mut Holders, t: Address, m: MarketId, holds: bool) {
        if holds {
            holders.entry(m).or_default().insert(t);
        } else if let Some(set) = holders.get_mut(&m) {
            set.remove(&t);
            if set.is_empty() {
                holders.remove(&m);
            }
        }
    }

    /// adl-budget C2: every trader that may hold a position in `m` in the
    /// block's overlay over these records' R, ascending, each once: R's
    /// holders of `m` merged with `dirty` (ascending: the traders whose key
    /// `t ‖ m` the block wrote or deleted, from [`dirty_by_market`]). A
    /// superset of the holders — a dirty trader may have deleted its row —
    /// so the caller reads each one's position and skips those without,
    /// exactly as C1 skips a trader of the whole set that does not hold `m`.
    pub(crate) fn holders_with(&self, m: MarketId, dirty: &[Address]) -> Vec<Address> {
        let base = self.holders.get(&m);
        let mut out = Vec::with_capacity(base.map_or(0, HashSet::len) + dirty.len());
        out.extend(base.into_iter().flatten().chain(dirty));
        // The set is unordered (c2-holder-hashset): sorted here, ADL only.
        out.sort_unstable();
        out.dedup();
        out
    }

    /// `trader`'s positions in key order (empty: no rows), `None` if opaque.
    #[inline]
    pub(crate) fn get(&self, trader: &Address) -> Option<&[Position]> {
        if !self.opaque.is_empty() && self.opaque.contains_key(trader) {
            return None;
        }
        Some(self.map.get(trader).map_or(&[], |u| u.positions.as_slice()))
    }

    /// Item 6 E2: [`torus_core::liquidation::traders_after`]`(state, after,
    /// limit)` — the distinct traders of `state`'s 28-byte position keys,
    /// ascending, strictly after `after`, at most `limit` — where `state` is
    /// the block's overlay with these records' R attached (`None`: R not
    /// attached; the caller walks). A trader none of whose 28-byte keys the
    /// block wrote or deleted is a candidate iff it is in the set; any other
    /// is looked up through `state` (R plus the block's own rows).
    pub(crate) fn traders_after<B: StateBackend>(
        &self,
        state: &B,
        after: Option<Address>,
        limit: usize,
    ) -> Result<Option<Vec<Address>>, StateError> {
        let Some(pending) = state.layer_keys(CF_NATIVE_POSITIONS) else {
            return Ok(None);
        };
        let mut dirty: Vec<Address> =
            pending.iter().filter(|k| k.len() == KEY).map(|k| Address::from_slice(&k[..TRADER])).collect();
        dirty.dedup(); // key-sorted: a trader's keys are adjacent
        let lo = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut base = self.traders.range((lo, Bound::Unbounded)).peekable();
        let start = after.map_or(0, |a| dirty.partition_point(|t| *t <= a));
        let mut dirty = dirty[start..].iter().peekable();
        let mut out = Vec::new();
        while out.len() < limit {
            let t = match (base.peek(), dirty.peek()) {
                (None, None) => break,
                (Some(&&b), Some(&&d)) if b < d => {
                    base.next();
                    out.push(b);
                    continue;
                }
                (Some(&&b), None) => {
                    base.next();
                    out.push(b);
                    continue;
                }
                (_, Some(&&d)) => d,
            };
            dirty.next();
            if base.peek().is_some_and(|b| **b == t) {
                base.next();
            }
            if has_key(state, &t)? {
                out.push(t);
            }
        }
        Ok(Some(out))
    }

    /// The record set's rows of `cf` (positions or balances; empty for any
    /// other CF) as R holds them: every record re-encoded, plus the raw rows
    /// (tests and `ResidentBooks` introspection; equals R's rows of `cf`).
    pub(crate) fn encode_rows(&self, cf: &str) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let side = |r: RawRows| if cf == CF_NATIVE_POSITIONS { r.positions } else { r.balances };
        let mut out = BTreeMap::new();
        if cf != CF_NATIVE_POSITIONS && cf != CF_NATIVE_BALANCES {
            return out;
        }
        for (t, u) in &self.map {
            out.extend(side(u.raw(t)));
        }
        for r in self.opaque.values().chain([&self.other]) {
            out.extend(side(r.clone()));
        }
        out
    }

    /// Same records, raw rows and indexes (tests and `ResidentBooks`
    /// introspection; the sums are not compared).
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.traders == other.traders
            && self.holders == other.holders
            && self.opaque == other.opaque
            && self.other == other.other
            && self.map.len() == other.map.len()
            && self.map.iter().all(|(t, u)| other.map.get(t).is_some_and(|o| o.raw(t) == u.raw(t)))
    }

    /// [`Self::same_as`] a cold build of `rows`, and re-encodes to exactly
    /// `rows`' positions and balances (tests and `ResidentBooks`
    /// introspection).
    pub(crate) fn match_rows(&self, rows: &ResidentRows) -> bool {
        let empty = BTreeMap::new();
        self.same_as(&Self::build(rows))
            && [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES]
                .into_iter()
                .all(|cf| self.encode_rows(cf) == *rows.rows(cf).unwrap_or(&empty))
    }
}

/// Item 6 E2: whether `state` holds a 28-byte position key of `t`. They all
/// lie in `[t ‖ 00×8, t ‖ ff×8]`; longer keys under `t` in between are
/// stepped over. (adl-budget A8 perf: also the ADL drain's flat check.)
///
/// adl-budget s99 (owner decision 4): each seek is bounded to `t`'s prefix
/// (`iterate_cf_prefix_from`), so it reads only `t`'s own keys and
/// tombstones. The unbounded seek it replaces gave the same answer (a key
/// past the prefix meant `false`) but, for a trader the block emptied, the
/// overlay merge stepped over its tombstones AND every following emptied
/// trader's before reaching a live key: quadratic in a block B that ADLs
/// many adjacent accounts (~96 ms of the S=750-like B₁, §11.3).
pub(super) fn has_key<B: StateBackend>(state: &B, t: &Address) -> Result<bool, StateError> {
    let mut start = [t.as_slice(), &[0u8; KEY - TRADER]].concat();
    loop {
        let Some((k, _)) = state.iterate_cf_prefix_from(CF_NATIVE_POSITIONS, t.as_slice(), &start, 1)?.pop() else {
            return Ok(false);
        };
        if k.len() == KEY {
            return Ok(true);
        }
        start = [k.as_slice(), &[0]].concat();
    }
}

/// adl-budget C2: the block's own pending 28-byte position keys of `state`
/// (writes and tombstones) as market -> traders, each list ascending and
/// without duplicates; `None`: R not attached (the caller ranks over the
/// whole trader set).
pub(crate) fn dirty_by_market<B: StateBackend>(state: &B) -> Option<ByMarket> {
    Some(by_market(&state.layer_keys(CF_NATIVE_POSITIONS)?))
}

/// adl-dirty-check: [`dirty_by_market`] and the traders under whose
/// positions prefix `state`'s own pending set writes or deletes
/// (`layer_touches(CF_NATIVE_POSITIONS, t)` iff `t` is in it), from one
/// `layer_keys`; `None`: R not attached.
pub(crate) fn dirty_by_market_and_traders<B: StateBackend>(
    state: &B,
) -> Option<(ByMarket, HashSet<Address>)> {
    let pending = state.layer_keys(CF_NATIVE_POSITIONS)?;
    let traders = pending
        .iter()
        .filter(|k| k.len() >= TRADER)
        .map(|k| Address::from_slice(&k[..TRADER]))
        .collect();
    Some((by_market(&pending), traders))
}

/// Market -> the traders of its keys (ascending, deduplicated).
type ByMarket = HashMap<MarketId, Vec<Address>>;

fn by_market(pending: &[Vec<u8>]) -> ByMarket {
    let mut out = ByMarket::default();
    // Key-sorted (`t ‖ m`): per market the traders come ascending.
    for k in pending.iter().filter(|k| k.len() == KEY) {
        out.entry(market_of(k)).or_default().push(Address::from_slice(&k[..TRADER]));
    }
    for list in out.values_mut() {
        list.dedup();
    }
    out
}

/// The position in `market_id` of a trader's record.
#[inline]
pub(crate) fn find(ps: &[Position], market_id: MarketId) -> Option<&Position> {
    ps.binary_search_by_key(&market_id, |p| p.market_id).ok().map(|i| &ps[i])
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
