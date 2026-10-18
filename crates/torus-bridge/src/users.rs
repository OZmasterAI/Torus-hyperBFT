//! Item 6 Phase 4 step 1 (`docs/plans/item6-phase4-impl.md` Task 1.1): the
//! record set [`Users`] — one record per trader of R's `CF_NATIVE_POSITIONS`
//! and `CF_NATIVE_BALANCES` rows — kept in the resident rows slot next to R
//! and updated from each block's [`ResidentDelta`] at `end_resident` (built
//! cold with R). It replaces C7's `TraderPositions` (decoded positions, E2's
//! trader set, adl-budget C2's holder lists) and also follows the balance and
//! `cvlm` rows. Node-local, never consensus-visible, no format change.
//!
//! Phase 4 step 2 (Task 2.2): the record set is R's source for both CFs
//! ([`ResidentSource`]): R holds no raw map of them, and every overlay read
//! of them (point, length, prefix, seek, prefix check) is answered here —
//! records encoded per read, opaque traders' and the other rows as bytes.
//! The positions CF in key order walks the trader set (address order), each
//! user's positions in market order, merged with the raw rows; the balances
//! CF in key order (only the proof-only `liquidation_value_sum` walks it) is
//! a collect-and-sort slow path unless the prefix names one trader.
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
//! writes in the block (`AccountReader`). Phase 4 step 4: the typed loads
//! (`PositionCache`, `BalanceCache`, the `cum_volume` reads) use it per key,
//! for a key with no own pending row ([`TypedRows`]).
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

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use alloy_primitives::map::{HashMap, HashSet};
use std::ops::Bound;

use alloy_primitives::Address;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_core::error::CoreError;
use torus_core::position::{cum_volume_key, position_key, NativeBalance, Position, TypedRows, POSITION_V2_LEN};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::error::StateError;
use torus_state::resident_rows::{
    ResidentChange, ResidentRow, ResidentRowIter, ResidentSource, SourceRows, SOURCE_SLOTS,
};
#[cfg(test)]
use torus_state::ResidentRows;
#[cfg(test)]
use torus_state::FrozenPending;
use torus_state::{ResidentDelta, StateBackend};
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
/// A regular balance row's value: version byte + two raw `FixedPoint`s.
const BALANCE_LEN: usize = 33;
/// A regular `cvlm` row's value: a raw `FixedPoint`, 16 bytes BE.
const VOLUME_LEN: usize = 16;

/// Phase 4 step 2: [`ResidentSource`] slots (R's `RESIDENT_CFS` order).
pub(crate) const POSITIONS: usize = 0;
pub(crate) const BALANCES: usize = 1;

/// Item 6 step 1: one changed row of a regular trader at the end of a block:
/// its position before the block and after (`None`: no row).
pub(crate) type Change = (Option<Position>, Option<Position>);

/// Phase 4 step 3: a user's margin sums with the mark version they are
/// valued at (the tag), boxed so a user without sums pays 8 bytes.
pub(crate) type Sums = Option<Box<(u64, SumsResult)>>;

/// Item 6 step 1: [`Users::apply`]'s per-trader report. Phase 4 step 3:
/// with the trader's sums entry, which the callee moves on or clears.
pub(crate) type Seen<'a> = dyn FnMut(&Address, Option<(&[Change], &[Position], &mut Sums)>) + 'a;

/// One regular trader's record: its rows of both CFs, decoded.
#[derive(Debug, Default)]
pub(crate) struct UserState {
    /// `None`: no balance row (absent != zero: a different row set).
    pub(crate) balance: Option<NativeBalance>,
    /// `None`: no `cvlm` row.
    pub(crate) cum_volume: Option<FixedPoint>,
    /// Market order (= key order), without growth slack after a rebuild.
    pub(crate) positions: Vec<Position>,
    /// Phase 4 step 3 (plan section 3, "Sums"; was C3's `SumsCache`): the
    /// trader's margin sums over these positions, tagged with the mark
    /// version they are valued at. Read only while the tag equals the
    /// block's version ([`Users::sums`]; versions are never reused); a
    /// moving version clears nothing, a stale entry is ignored and later
    /// overwritten. Written only at `end_resident` (the block's memo, the
    /// carry of a written trader); a re-decoded record starts without.
    pub(crate) sums: Sums,
}

impl UserState {
    fn is_empty(&self) -> bool {
        self.balance.is_none() && self.cum_volume.is_none() && self.positions.is_empty()
    }

    /// Phase 4 step 2: its rows and their key + value bytes (every regular
    /// row has a fixed size).
    fn footprint(&self) -> (usize, usize) {
        let (n, b, v) = (self.positions.len(), usize::from(self.balance.is_some()), usize::from(self.cum_volume.is_some()));
        (n + b + v, n * (KEY + POSITION_V2_LEN) + b * (TRADER + BALANCE_LEN) + v * (CVLM_KEY + VOLUME_LEN))
    }

    /// `t`'s rows of both CFs, as R holds them.
    fn raw(&self, t: &Address) -> RawRows {
        let mut out = RawRows::default();
        for p in &self.positions {
            out.positions.insert([t.as_slice(), &p.market_id.to_be_bytes()].concat(), encode(p, POSITION_V2_LEN));
        }
        if let Some(b) = &self.balance {
            out.balances.insert(t.to_vec(), encode(b, BALANCE_LEN));
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

impl RawRows {
    /// Phase 4 step 2: its rows and their key + value bytes.
    fn footprint(&self) -> (usize, usize) {
        let bytes = |m: &BTreeMap<Vec<u8>, Vec<u8>>| m.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
        (self.positions.len() + self.balances.len(), bytes(&self.positions) + bytes(&self.balances))
    }
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
    /// Phase 4 step 2: rows of both CFs held (records, opaque, other) and
    /// their key + value bytes, kept per change (the users gauges).
    rows: usize,
    bytes: usize,
    /// Phase 4 step 4 (tests): typed loads answered from a record, per kind
    /// (position, balance, volume), and declined (an opaque trader).
    #[cfg(test)]
    typed: ([std::sync::atomic::AtomicUsize; 3], std::sync::atomic::AtomicUsize),
    /// Phase 5 step 1: the keys [`UsersMut`] changed since the last drain.
    /// Step 2: with each positions key's row at the block start.
    dirty: Dirty,
    /// Phase 5 step 2: the last drain's positions keys and their rows at
    /// the block start, kept for the sums carry at `end_resident`
    /// ([`Users::report_in_place`], off the execution thread).
    carry: Carry,
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
    debug_assert!(encode(&b, BALANCE_LEN) == value && value.len() == BALANCE_LEN, "balance re-encode check");
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
    /// Phase 4 step 2: every row of both CFs as `backend` sees them (R's
    /// rebuild: through the overlay WITHOUT R, DB + parent layer = the
    /// previous block's post-state).
    pub(crate) fn build_from<B: StateBackend>(backend: &B) -> Result<Self, StateError> {
        let pos: BTreeMap<Vec<u8>, Vec<u8>> = backend.iterate_cf(CF_NATIVE_POSITIONS, None)?.into_iter().collect();
        let bal: BTreeMap<Vec<u8>, Vec<u8>> = backend.iterate_cf(CF_NATIVE_BALANCES, None)?.into_iter().collect();
        Ok(Self::build_maps(&pos, &bal))
    }

    /// Every trader of raw R's positions and balances (tests: the reference).
    #[cfg(test)]
    pub(crate) fn build(rows: &ResidentRows) -> Self {
        let empty = BTreeMap::new();
        Self::build_maps(rows.rows(CF_NATIVE_POSITIONS).unwrap_or(&empty), rows.rows(CF_NATIVE_BALANCES).unwrap_or(&empty))
    }

    /// Every trader of the rows `pos` (positions CF) and `bal` (balances CF).
    fn build_maps(pos: &BTreeMap<Vec<u8>, Vec<u8>>, bal: &BTreeMap<Vec<u8>, Vec<u8>>) -> Self {
        let mut out = Self::default();
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
        (out.rows, out.bytes) = out.recount();
        out
    }

    /// Rows and key + value bytes held, counted afresh.
    fn recount(&self) -> (usize, usize) {
        let fps = self.map.values().map(UserState::footprint);
        let raws = self.opaque.values().chain([&self.other]).map(RawRows::footprint);
        fps.chain(raws).fold((0, 0), |(n, b), (dn, db)| (n + dn, b + db))
    }

    /// Phase 4 step 2: rows of both CFs held (the users gauge).
    pub(crate) fn row_count(&self) -> usize {
        self.rows
    }

    /// Phase 4 step 2: their key + value bytes (the users bytes gauge).
    pub(crate) fn row_bytes(&self) -> usize {
        self.bytes
    }

    /// The footprint of `t`'s rows held (record or opaque raw rows).
    fn footprint_of(&self, t: &Address) -> (usize, usize) {
        match self.opaque_of(t) {
            Some(raw) => raw.footprint(),
            None => self.map.get(t).map_or((0, 0), UserState::footprint),
        }
    }

    /// Account for a trader's rows going from `old` to `new`.
    fn track(&mut self, old: (usize, usize), new: (usize, usize)) {
        self.rows = self.rows + new.0 - old.0;
        self.bytes = self.bytes + new.1 - old.1;
    }

    /// A write / tombstone of a row under no trader.
    fn put_other(&mut self, positions: bool, key: &[u8], value: Option<&[u8]>) {
        let map = if positions { &mut self.other.positions } else { &mut self.other.balances };
        let old = match value {
            Some(v) => map.insert(key.to_vec(), v.to_vec()),
            None => map.remove(key),
        };
        if let Some(old) = old {
            self.rows -= 1;
            self.bytes -= key.len() + old.len();
        }
        if let Some(v) = value {
            self.rows += 1;
            self.bytes += key.len() + v.len();
        }
    }

    /// `t`'s raw rows if it is opaque.
    #[inline]
    fn opaque_of(&self, t: &Address) -> Option<&RawRows> {
        if self.opaque.is_empty() {
            None
        } else {
            self.opaque.get(t)
        }
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
    /// positions after) and its sums entry (Phase 4 step 3; the callee moves
    /// it on or clears it); `None` when its rows are re-decoded (opaque
    /// before, or an irregular write in either CF: a new record starts
    /// without sums).
    pub(crate) fn apply(&mut self, delta: &ResidentDelta, mut seen: Option<&mut Seen<'_>>) {
        // A trader's balance and `cvlm` rows are not adjacent in key order:
        // gathered per trader first.
        let mut balances: HashMap<Address, BalanceDelta<'_>> = HashMap::default();
        for (k, v) in delta.entries(CF_NATIVE_BALANCES) {
            match balance_owner(k) {
                Some((t, BalanceRow::Balance)) => balances.entry(t).or_default().balance = Some(v),
                Some((t, BalanceRow::Volume)) => balances.entry(t).or_default().volume = Some(v),
                None => self.put_other(false, k, v),
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
                self.put_other(true, first.0, first.1);
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
        let old = self.map.get(&t).map_or((0, 0), UserState::footprint);
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
            f(&t, Some((changes.as_slice(), u.positions.as_slice(), &mut u.sums)));
        }
        // E2: a regular trader's position rows are all 28-byte keys.
        if keyed {
            if u.positions.is_empty() {
                self.traders.remove(&t);
            } else {
                self.traders.insert(t);
            }
        }
        let new = u.footprint();
        if u.is_empty() {
            self.map.remove(&t);
        }
        self.track(old, new);
        true
    }

    /// One trader's entries of the delta on its rows (its raw rows if
    /// opaque, else its record re-encoded), then re-decoded. C2 / E2: its
    /// holders move per key from the delta; its trader-set entry follows its
    /// 28-byte keys.
    fn reload(&mut self, t: Address, group: &[(&[u8], Option<&[u8]>)], b: BalanceDelta<'_>) {
        let old = self.footprint_of(&t);
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
        let new = self.footprint_of(&t);
        self.track(old, new);
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
        if self.opaque_of(trader).is_some() {
            return None;
        }
        Some(self.map.get(trader).map_or(&[], |u| u.positions.as_slice()))
    }

    /// Phase 4 step 3: `trader`'s sums entry if it is tagged `version` (an
    /// entry at another version is never returned).
    #[inline]
    pub(crate) fn sums(&self, trader: &Address, version: u64) -> Option<SumsResult> {
        match self.map.get(trader)?.sums.as_deref() {
            Some(&(tag, r)) if tag == version => Some(r),
            _ => None,
        }
    }

    /// Phase 4 step 3: `trader`'s sums entry to write (`None`: no record —
    /// opaque, or no rows: nothing to keep sums on).
    pub(crate) fn sums_slot_mut(&mut self, trader: &Address) -> Option<&mut Sums> {
        self.map.get_mut(trader).map(|u| &mut u.sums)
    }

    /// Phase 4 step 3: drop every user's sums entry; returns the entries
    /// dropped (O(users): tests only, through
    /// `ResidentBooks::drop_sums_cache`; the node never clears all users).
    pub(crate) fn clear_sums(&mut self) -> usize {
        self.map.values_mut().filter_map(|u| u.sums.take()).count()
    }

    /// Phase 4 step 3 (tests): every sums entry as (trader, tag, sums).
    #[cfg(test)]
    pub(crate) fn sums_entries(&self) -> impl Iterator<Item = (Address, u64, SumsResult)> + '_ {
        self.map.iter().filter_map(|(t, u)| u.sums.as_deref().map(|&(v, r)| (*t, v, r)))
    }

    /// Item 6 E2: [`torus_core::liquidation::traders_after`]`(state, after,
    /// limit)` — the distinct traders of `state`'s 28-byte position keys,
    /// ascending, strictly after `after`, at most `limit` — where `state` is
    /// the block's overlay with these records' R attached (`None`: R not
    /// attached; the caller walks). A trader none of whose 28-byte keys the
    /// block wrote or deleted is a candidate iff it is in the set; any other
    /// is looked up through `state` (R plus the block's own rows).
    /// Phase 5 step 2: the node reads it through
    /// [`SharedUsers::traders_after`] (no guard across the reads of `state`).
    #[cfg(test)]
    pub(crate) fn traders_after<B: StateBackend>(
        &self,
        state: &B,
        after: Option<Address>,
        limit: usize,
    ) -> Result<Option<Vec<Address>>, StateError> {
        let Some(dirty) = dirty_traders_after(state, after) else {
            return Ok(None);
        };
        let lo = after.map_or(Bound::Unbounded, Bound::Excluded);
        merge_traders(state, self.traders.range((lo, Bound::Unbounded)).copied(), &dirty, limit).map(Some)
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

    /// Phase 4 step 2 (R holds no raw rows of the two CFs to compare
    /// with): [`Self::same_as`] a cold build of its own rows (records,
    /// opaque and other rows, indexes), with its kept row count and bytes
    /// equal to a fresh count (tests and `ResidentBooks` introspection; the
    /// rows themselves are compared with the DB through R's `dump`).
    pub(crate) fn self_consistent(&self) -> bool {
        let rows = |cf| self.encode_rows(cf);
        let cold = Self::build_maps(&rows(CF_NATIVE_POSITIONS), &rows(CF_NATIVE_BALANCES));
        self.same_as(&cold) && (self.rows, self.bytes) == self.recount() && (cold.rows, cold.bytes) == (self.rows, self.bytes)
    }
}

/// Item 6 Phase 5 step 1 (`docs/plans/item6-phase5-impl.md` Task 1.1): the
/// keys of both CFs changed since the last drain, by key shape. A key is in
/// at most one set (the shapes are disjoint); its row is read from the
/// record set at the drain, so a key written many times is emitted once.
/// Step 2 (Task 2.1): each positions key keeps its row at its first change
/// in the block (= at the block start): the overlay's own-change answers
/// (`resident_changes`: the sums' partial re-value) and the sums carry at
/// `end_resident` need the block-start rows the in-place record no longer
/// holds.
#[derive(Debug, Default)]
struct Dirty {
    /// 28-byte positions keys `t ‖ m`, each with its row at the block start.
    positions: HashMap<(Address, MarketId), Before>,
    /// Per trader: its markets in `positions`, in first-change order (a
    /// trader's own changes without a scan).
    by_trader: HashMap<Address, Vec<MarketId>>,
    /// 20-byte balance keys.
    balances: HashSet<Address>,
    /// 24-byte `cvlm ‖ t` keys.
    volumes: HashSet<Address>,
    /// Every other key, per slot ([`POSITIONS`], [`BALANCES`]), with (a
    /// positions key) its raw row at the block start; `None` for balances.
    raw: [BTreeMap<Vec<u8>, Option<Vec<u8>>>; 2],
}

/// Phase 5 step 2: a dirty positions key's row at the block start.
#[derive(Debug, Clone)]
enum Before {
    /// Its trader was regular at the first change: its position (`None`:
    /// no row).
    Typed(Option<Position>),
    /// Its trader was opaque: the raw row (`None`: no row).
    Raw(Option<Vec<u8>>),
}

impl Before {
    /// The row's bytes, as R held them.
    fn bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Typed(p) => p.as_ref().map(|p| encode(p, POSITION_V2_LEN)),
            Self::Raw(v) => v.clone(),
        }
    }
}

/// Phase 5 step 2: what the drains of a block leave for its sums carry
/// ([`Users::report_in_place`]): its changed positions keys with their rows
/// at the block start, and the traders of its other positions keys (their
/// records were re-decoded: no carry).
#[derive(Debug, Default)]
struct Carry {
    positions: HashMap<(Address, MarketId), Before>,
    by_trader: HashMap<Address, Vec<MarketId>>,
    raw_traders: HashSet<Address>,
}

// Phase 5 step 1 (tests): rows the drains serialized from a record / as raw
// bytes (an opaque trader's, a row under no trader).
#[cfg(test)]
thread_local! {
    static DRAIN_SOURCES: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

impl Dirty {
    fn len(&self) -> usize {
        self.positions.len()
            + self.balances.len()
            + self.volumes.len()
            + self.raw.iter().map(BTreeMap::len).sum::<usize>()
    }

    /// The dirty keys of `slot`, key-sorted (fixed-size keys sorted typed,
    /// then merged with the raw keys by bytes).
    fn sorted_keys(&self, slot: usize) -> Vec<Vec<u8>> {
        if slot == POSITIONS {
            let mut typed: Vec<(Address, MarketId)> = self.positions.keys().copied().collect();
            typed.sort_unstable();
            return merge_sorted(
                vec![
                    typed.iter().map(|(t, m)| position_key(t, *m).to_vec()).collect(),
                    self.raw[POSITIONS].keys().cloned().collect(),
                ],
            );
        }
        let mut bal: Vec<Address> = self.balances.iter().copied().collect();
        bal.sort_unstable();
        let mut vol: Vec<Address> = self.volumes.iter().copied().collect();
        vol.sort_unstable();
        merge_sorted(vec![
            bal.iter().map(|t| t.to_vec()).collect(),
            vol.iter().map(|t| cum_volume_key(t).to_vec()).collect(),
            self.raw[BALANCES].keys().cloned().collect(),
        ])
    }
}

/// Key-sorted runs of distinct keys merged into one key-sorted list (one
/// run, the usual case, is returned as it is).
fn merge_sorted(runs: Vec<Vec<Vec<u8>>>) -> Vec<Vec<u8>> {
    let mut runs: Vec<Vec<Vec<u8>>> = runs.into_iter().filter(|r| !r.is_empty()).collect();
    if runs.len() <= 1 {
        return runs.pop().unwrap_or_default();
    }
    let mut at = vec![0usize; runs.len()];
    let mut out = Vec::with_capacity(runs.iter().map(Vec::len).sum());
    loop {
        let mut best: Option<usize> = None;
        for i in 0..runs.len() {
            if at[i] < runs[i].len() && best.is_none_or(|b| runs[i][at[i]] < runs[b][at[b]]) {
                best = Some(i);
            }
        }
        let Some(i) = best else { break };
        out.push(std::mem::take(&mut runs[i][at[i]]));
        at[i] += 1;
    }
    out
}

/// One drained row: a write (`Some`) or a tombstone (`None`).
pub(crate) type DirtyRow = (Vec<u8>, Option<Vec<u8>>);

/// Phase 5 step 1: a drain's rows per CF, each CF key-sorted, each key once.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct DirtyRows {
    pub(crate) positions: Vec<DirtyRow>,
    pub(crate) balances: Vec<DirtyRow>,
}

/// Phase 5 step 1: the typed write API of the record set. Every call
/// changes the rows in place (records, or the raw rows of an opaque trader)
/// and marks the key dirty; [`Users::drain_dirty_into`] serializes each
/// dirty key's row once at freeze. Shadow only in step 1 (tests are its only
/// callers; step 2 moves the hot writers onto it).
pub(crate) struct UsersMut<'a> {
    users: &'a mut Users,
}

impl Users {
    /// Phase 5 step 1: the typed write handle.
    pub(crate) fn mutate(&mut self) -> UsersMut<'_> {
        UsersMut { users: self }
    }

    /// Phase 5 step 1: keys marked dirty since the last drain.
    pub(crate) fn dirty_keys(&self) -> usize {
        self.dirty.len()
    }

    /// Phase 5 step 2 (tests): what [`Self::drain_dirty`] would emit now,
    /// without draining.
    #[cfg(test)]
    pub(crate) fn dirty_preview(&self) -> DirtyRows {
        DirtyRows {
            positions: self.rows_at(POSITIONS, self.dirty.sorted_keys(POSITIONS)),
            balances: self.rows_at(BALANCES, self.dirty.sorted_keys(BALANCES)),
        }
    }

    /// Phase 5 step 1: every dirty key's row as the record set holds it now
    /// (a regular trader's encoded from its record, an opaque trader's and a
    /// row under no trader as bytes; absent: a tombstone), per CF in key
    /// order; the dirty sets are left empty. Step 2: the positions keys'
    /// rows at the block start move to the sums carry (`end_resident`).
    pub(crate) fn drain_dirty(&mut self) -> DirtyRows {
        let pos = self.dirty.sorted_keys(POSITIONS);
        let bal = self.dirty.sorted_keys(BALANCES);
        let Dirty { positions, by_trader, raw: [raw_positions, _], .. } = std::mem::take(&mut self.dirty);
        let carry = &mut self.carry;
        // Normally empty here (`end_resident` takes it every block); a
        // second drain before it keeps each key's first row.
        for (k, b) in positions {
            carry.positions.entry(k).or_insert(b);
        }
        for (t, ms) in by_trader {
            let have = carry.by_trader.entry(t).or_default();
            for m in ms {
                if !have.contains(&m) {
                    have.push(m);
                }
            }
        }
        carry
            .raw_traders
            .extend(raw_positions.keys().filter(|k| k.len() >= TRADER).map(|k| Address::from_slice(&k[..TRADER])));
        DirtyRows {
            positions: self.rows_at(POSITIONS, pos),
            balances: self.rows_at(BALANCES, bal),
        }
    }

    /// Phase 5 step 2: mark `t ‖ m` dirty, keeping its row as held now if
    /// this is its first change since the last drain. Call before the change.
    fn mark_position(&mut self, t: Address, m: MarketId) {
        if self.dirty.positions.contains_key(&(t, m)) {
            return;
        }
        let before = match self.opaque_of(&t) {
            Some(raw) => Before::Raw(raw.positions.get(position_key(&t, m).as_slice()).cloned()),
            None => Before::Typed(self.map.get(&t).and_then(|u| find(&u.positions, m).cloned())),
        };
        self.dirty.positions.insert((t, m), before);
        self.dirty.by_trader.entry(t).or_default().push(m);
    }

    /// Phase 5 step 2: mark `key` of `slot` dirty (any key shape), keeping a
    /// positions key's row as held now at its first change. Call before the
    /// change.
    fn mark(&mut self, slot: usize, key: &[u8]) {
        if slot == POSITIONS {
            if key.len() == KEY {
                self.mark_position(Address::from_slice(&key[..TRADER]), market_of(key));
            } else if !self.dirty.raw[POSITIONS].contains_key(key) {
                let before = self.position_row(key).map(|r| r.value().into_owned());
                self.dirty.raw[POSITIONS].insert(key.to_vec(), before);
            }
            return;
        }
        match balance_owner(key) {
            Some((t, BalanceRow::Balance)) => {
                self.dirty.balances.insert(t);
            }
            Some((t, BalanceRow::Volume)) => {
                self.dirty.volumes.insert(t);
            }
            None => {
                self.dirty.raw[BALANCES].entry(key.to_vec()).or_insert(None);
            }
        }
    }

    /// Phase 5 step 2: the keys of `slot` changed since the last drain,
    /// sorted (the overlay's `layer_keys`).
    pub(crate) fn dirty_key_list(&self, slot: usize) -> Vec<Vec<u8>> {
        self.dirty.sorted_keys(slot)
    }

    /// Phase 5 step 2: whether a key of `slot` under `prefix` changed since
    /// the last drain (the overlay's `layer_touches`).
    pub(crate) fn dirty_touches(&self, slot: usize, prefix: &[u8]) -> bool {
        let d = &self.dirty;
        let raw = d.raw[slot]
            .range::<[u8], _>((Bound::Included(prefix), Bound::Unbounded))
            .next()
            .is_some_and(|(k, _)| k.starts_with(prefix));
        if raw {
            return true;
        }
        if slot == POSITIONS {
            if prefix.len() >= TRADER {
                let t = Address::from_slice(&prefix[..TRADER]);
                return d
                    .by_trader
                    .get(&t)
                    .is_some_and(|ms| ms.iter().any(|m| position_key(&t, *m).starts_with(prefix)));
            }
            return d.positions.keys().any(|(t, m)| position_key(t, *m).starts_with(prefix));
        }
        d.balances.iter().any(|t| t.as_slice().starts_with(prefix))
            || d.volumes.iter().any(|t| cum_volume_key(t).starts_with(prefix))
    }

    /// Phase 5 step 2: the positions keys under `prefix` changed since the
    /// last drain, each with its row at the block start and now, key-sorted
    /// (the overlay's `resident_changes`). `None` for the balances CF: no
    /// block-start rows are kept for it (nothing asks).
    pub(crate) fn dirty_changes(&self, slot: usize, prefix: &[u8]) -> Option<Vec<ResidentChange>> {
        if slot != POSITIONS {
            return None;
        }
        let d = &self.dirty;
        let now = |key: &[u8]| self.position_row(key).map(|r| r.value().into_owned());
        let mut out: Vec<ResidentChange> = Vec::new();
        if prefix.len() >= TRADER {
            let t = Address::from_slice(&prefix[..TRADER]);
            for m in d.by_trader.get(&t).into_iter().flatten() {
                let key = position_key(&t, *m);
                if key.starts_with(prefix) {
                    let resident = d.positions[&(t, *m)].bytes();
                    out.push(ResidentChange { key: key.to_vec(), resident, current: now(&key) });
                }
            }
        } else {
            for ((t, m), b) in &d.positions {
                let key = position_key(t, *m);
                if key.starts_with(prefix) {
                    out.push(ResidentChange { key: key.to_vec(), resident: b.bytes(), current: now(&key) });
                }
            }
        }
        for (k, b) in d.raw[POSITIONS]
            .range::<[u8], _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix))
        {
            out.push(ResidentChange { key: k.clone(), resident: b.clone(), current: now(k) });
        }
        out.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        Some(out)
    }

    /// Phase 5 step 2 (`end_resident`, in place): the sums carry of the last
    /// drained block. `seen` is called once per trader whose positions rows
    /// the block changed, in address order: with its changes (row at the
    /// block start, row now), its positions now and its sums entry when it
    /// is regular now and was at each first change (as `apply` reports a
    /// trader regular before and after); else `None` (its sums entry, if it
    /// has a record, is dropped: the record was re-decoded or has rows the
    /// carry cannot value).
    pub(crate) fn report_in_place(&mut self, seen: &mut Seen<'_>) {
        let Carry { mut positions, by_trader, raw_traders } = std::mem::take(&mut self.carry);
        let mut traders: Vec<Address> = by_trader.keys().chain(raw_traders.iter()).copied().collect();
        traders.sort_unstable();
        traders.dedup();
        let mut changes: Vec<Change> = Vec::new();
        for t in traders {
            let ms = by_trader.get(&t).map_or(&[][..], Vec::as_slice);
            let typed = !raw_traders.contains(&t)
                && self.opaque_of(&t).is_none()
                && ms.iter().all(|m| matches!(positions.get(&(t, *m)), Some(Before::Typed(_))));
            match self.map.get_mut(&t) {
                Some(u) if typed => {
                    changes.clear();
                    for m in ms {
                        if let Some(Before::Typed(b)) = positions.remove(&(t, *m)) {
                            changes.push((b, find(&u.positions, *m).cloned()));
                        }
                    }
                    seen(&t, Some((changes.as_slice(), u.positions.as_slice(), &mut u.sums)));
                }
                Some(u) => {
                    u.sums = None;
                    seen(&t, None);
                }
                None => seen(&t, None),
            }
        }
    }

    /// The rows of `keys` in `slot` as held now (absent: a tombstone).
    fn rows_at(&self, slot: usize, keys: Vec<Vec<u8>>) -> Vec<DirtyRow> {
        keys.into_iter()
            .map(|k| {
                let v = self.row(slot, &k).map(|r| {
                    #[cfg(test)]
                    DRAIN_SOURCES.with(|c| {
                        let (rec, raw) = c.get();
                        c.set(if matches!(r, Row::Raw(_)) {
                            (rec, raw + 1)
                        } else {
                            (rec + 1, raw)
                        });
                    });
                    r.value().into_owned()
                });
                (k, v)
            })
            .collect()
    }

    /// Phase 5 step 1 (tests): rows this thread's drains serialized from a
    /// record and as raw bytes, so far.
    #[cfg(test)]
    pub(crate) fn drain_sources() -> (usize, usize) {
        DRAIN_SOURCES.with(std::cell::Cell::get)
    }

    /// Phase 5 step 1 (tests): [`Self::drain_dirty`] into `out` (the
    /// block's frozen set: its rows of the two CFs replaced key by key).
    /// Step 2: the node drains through [`SharedUsers`] at the freeze.
    #[cfg(test)]
    pub(crate) fn drain_dirty_into(&mut self, out: &mut FrozenPending) {
        let rows = self.drain_dirty();
        out.put_rows(CF_NATIVE_POSITIONS, rows.positions);
        out.put_rows(CF_NATIVE_BALANCES, rows.balances);
    }
}

impl UsersMut<'_> {
    /// `p`'s row (key `p.trader ‖ p.market_id`) = `p`.
    pub(crate) fn set_position(&mut self, p: Position) {
        let (t, m) = (p.trader, p.market_id);
        let users = &mut *self.users;
        users.mark_position(t, m);
        if users.opaque_of(&t).is_some() {
            let v = encode(&p, POSITION_V2_LEN);
            users.reload(
                t,
                &[(&position_key(&t, m), Some(&v))],
                BalanceDelta::default(),
            );
            return;
        }
        let old = users.map.get(&t).map_or((0, 0), UserState::footprint);
        let u = users.map.entry(t).or_default();
        match u.positions.binary_search_by_key(&m, |q| q.market_id) {
            Ok(i) => u.positions[i] = p,
            Err(i) => {
                u.positions.insert(i, p);
                Users::set_holder(&mut users.holders, t, m, true);
                users.traders.insert(t);
            }
        }
        let new = u.footprint();
        users.track(old, new);
    }

    /// No row at `t ‖ m` (a tombstone even if there was none).
    pub(crate) fn remove_position(&mut self, t: &Address, m: MarketId) {
        let users = &mut *self.users;
        users.mark_position(*t, m);
        if users.opaque_of(t).is_some() {
            users.reload(*t, &[(&position_key(t, m), None)], BalanceDelta::default());
            return;
        }
        let Some(u) = users.map.get_mut(t) else {
            return;
        };
        let Ok(i) = u.positions.binary_search_by_key(&m, |q| q.market_id) else {
            return;
        };
        let old = u.footprint();
        u.positions.remove(i);
        Users::set_holder(&mut users.holders, *t, m, false);
        if u.positions.is_empty() {
            users.traders.remove(t);
        }
        let new = u.footprint();
        if u.is_empty() {
            users.map.remove(t);
        }
        users.track(old, new);
    }

    /// `t`'s balance row = `b`.
    pub(crate) fn set_balance(&mut self, t: &Address, b: NativeBalance) {
        let users = &mut *self.users;
        users.dirty.balances.insert(*t);
        if users.opaque_of(t).is_some() {
            let v = encode(&b, BALANCE_LEN);
            users.reload(
                *t,
                &[],
                BalanceDelta {
                    balance: Some(Some(&v)),
                    volume: None,
                },
            );
            return;
        }
        let old = users.map.get(t).map_or((0, 0), UserState::footprint);
        let u = users.map.entry(*t).or_default();
        u.balance = Some(b);
        let new = u.footprint();
        users.track(old, new);
    }

    /// `t`'s `cvlm` row = its volume (zero without a row) + `amount`, as
    /// `add_cum_volume` reads and writes it (`Err`: an opaque trader's `cvlm`
    /// row is not 16 bytes, the read's error; nothing changes).
    pub(crate) fn add_cum_volume(
        &mut self,
        t: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        let users = &mut *self.users;
        if let Some(raw) = users.opaque_of(t) {
            let cur = match raw.balances.get(cum_volume_key(t).as_slice()) {
                None => FixedPoint::ZERO,
                Some(v) => decode_volume(v).ok_or_else(|| {
                    CoreError::Borsh(format!("cum_volume row has {} bytes, expected 16", v.len()))
                })?,
            };
            let v = (cur + amount).raw().to_be_bytes();
            users.dirty.volumes.insert(*t);
            users.reload(
                *t,
                &[],
                BalanceDelta {
                    balance: None,
                    volume: Some(Some(&v)),
                },
            );
            return Ok(());
        }
        users.dirty.volumes.insert(*t);
        let old = users.map.get(t).map_or((0, 0), UserState::footprint);
        let u = users.map.entry(*t).or_default();
        u.cum_volume = Some(u.cum_volume.unwrap_or(FixedPoint::ZERO) + amount);
        let new = u.footprint();
        users.track(old, new);
        Ok(())
    }

    /// Decision 3's write-through: any raw write (`Some`) or tombstone
    /// (`None`) of `key` in `slot` ([`POSITIONS`] / [`BALANCES`]), exactly
    /// as `apply` follows it from a delta (a regular row of a regular trader
    /// in place, any other change re-decodes the trader).
    pub(crate) fn put_row(&mut self, slot: usize, key: &[u8], value: Option<&[u8]>) {
        debug_assert!(slot == POSITIONS || slot == BALANCES, "source slot {slot}");
        let users = &mut *self.users;
        users.mark(slot, key);
        let (t, group, b) = if slot == POSITIONS {
            if key.len() < TRADER {
                users.put_other(true, key, value);
                return;
            }
            (
                Address::from_slice(&key[..TRADER]),
                Some((key, value)),
                BalanceDelta::default(),
            )
        } else {
            match balance_owner(key) {
                None => {
                    users.put_other(false, key, value);
                    return;
                }
                Some((t, BalanceRow::Balance)) => (
                    t,
                    None,
                    BalanceDelta {
                        balance: Some(value),
                        volume: None,
                    },
                ),
                Some((t, BalanceRow::Volume)) => (
                    t,
                    None,
                    BalanceDelta {
                        balance: None,
                        volume: Some(value),
                    },
                ),
            }
        };
        let group = group.as_slice();
        if !users.follow(t, group, b, &mut Vec::new(), &mut Vec::new(), None) {
            users.reload(t, group, b);
        }
    }
}

/// Item 6 Phase 5 step 2 (`docs/plans/item6-phase5-impl.md` Task 2.1): the
/// block's record set, shared by R (the overlay's reads and, in place, its
/// writes of the two CFs), the block's sums cache and the executor's typed
/// reads and writes. One `RwLock`: the writes (the settle's flush, `cvlm`,
/// the overlay's fold / write hook) run on the execution thread and never
/// overlap a parallel phase's reads. No guard is held across an overlay
/// call (lock order: the overlay's pending lock, then this one), and range
/// reads are copied out in chunks.
///
/// `in_place`: the node's mode, the record set takes the block's writes and
/// fills its change set at the freeze ([`ResidentSource::drain`]). `false`:
/// the Phase 4 reference path (tests): R reads it, every write stays on the
/// overlay, `end_resident` applies the block's delta.
#[derive(Debug)]
pub(crate) struct SharedUsers {
    users: RwLock<Users>,
    in_place: bool,
    /// The last drain's rows and nanoseconds, taken by `end_resident` (the
    /// drain metrics and the holder's count).
    drained: AtomicU64,
    drain_ns: AtomicU64,
}

impl SharedUsers {
    pub(crate) fn new(users: Users, in_place: bool) -> Self {
        Self { users: RwLock::new(users), in_place, drained: AtomicU64::new(0), drain_ns: AtomicU64::new(0) }
    }

    /// Read access (a panic under the write lock poisons it: the block
    /// fail-stops and the record set is dropped with its slot).
    #[inline]
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Users> {
        self.users.read().expect("users lock poisoned (a writer panicked mid-block)")
    }

    /// Write access (the execution thread only).
    #[inline]
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, Users> {
        self.users.write().expect("users lock poisoned (a writer panicked mid-block)")
    }

    /// Exclusive access (between blocks: `end_resident`, introspection).
    pub(crate) fn get_mut(&mut self) -> &mut Users {
        self.users.get_mut().expect("users lock poisoned (a writer panicked mid-block)")
    }

    /// The record set takes the block's writes in place (the node).
    #[inline]
    pub(crate) fn in_place(&self) -> bool {
        self.in_place
    }

    /// The last drain's rows and seconds since the last call (0 after).
    pub(crate) fn take_drain_stats(&self) -> (u64, f64) {
        let rows = self.drained.swap(0, Ordering::Relaxed);
        let ns = self.drain_ns.swap(0, Ordering::Relaxed);
        (rows, ns as f64 * 1e-9)
    }

    /// [`Users::sums`].
    #[inline]
    pub(crate) fn sums(&self, trader: &Address, version: u64) -> Option<SumsResult> {
        self.read().sums(trader, version)
    }

    /// `trader`'s positions, copied (`None` if opaque; [`Users::get`]).
    pub(crate) fn positions_of(&self, trader: &Address) -> Option<Vec<Position>> {
        self.read().get(trader).map(<[Position]>::to_vec)
    }

    /// `trader`'s position in `market_id` (`None` if opaque; `Some(None)`:
    /// no row).
    #[inline]
    pub(crate) fn position_of(&self, trader: &Address, market_id: MarketId) -> Option<Option<Position>> {
        self.read().get(trader).map(|ps| find(ps, market_id).cloned())
    }

    /// [`Users::holders_with`].
    pub(crate) fn holders_with(&self, m: MarketId, dirty: &[Address]) -> Vec<Address> {
        self.read().holders_with(m, dirty)
    }

    /// [`Users::traders_after`] without a guard held across the reads of
    /// `state`: the record set's candidates from the cursor are copied out
    /// first (at most `limit` + the dirty traders: all the merge can read).
    pub(crate) fn traders_after<B: StateBackend>(
        &self,
        state: &B,
        after: Option<Address>,
        limit: usize,
    ) -> Result<Option<Vec<Address>>, StateError> {
        let Some(dirty) = dirty_traders_after(state, after) else {
            return Ok(None);
        };
        let lo = after.map_or(Bound::Unbounded, Bound::Excluded);
        let base: Vec<Address> = self
            .read()
            .traders
            .range((lo, Bound::Unbounded))
            .take(limit.saturating_add(dirty.len()))
            .copied()
            .collect();
        merge_traders(state, base.into_iter(), &dirty, limit).map(Some)
    }
}

/// [`SharedUsers`]' range: copied out of the record set in chunks under the
/// read lock (positions [`RANGE_CHUNK`] rows at a time; the balances slow
/// path at once), so no guard outlives a call.
struct ChunkedRange<'a> {
    src: &'a SharedUsers,
    slot: usize,
    prefix: Vec<u8>,
    /// Where the next chunk starts (`None`: done).
    next: Option<Vec<u8>>,
    buf: std::vec::IntoIter<(Vec<u8>, Vec<u8>)>,
}

/// Positions rows per [`ChunkedRange`] chunk (a trader's rows fit one).
const RANGE_CHUNK: usize = 512;

impl<'a> Iterator for ChunkedRange<'a> {
    type Item = ResidentRow<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((k, v)) = self.buf.next() {
                return Some((Cow::Owned(k), Cow::Owned(v)));
            }
            let start = self.next.take()?;
            let chunk = if self.slot == POSITIONS { RANGE_CHUNK } else { usize::MAX };
            let rows: Vec<(Vec<u8>, Vec<u8>)> = {
                let u = self.src.read();
                <Users as ResidentSource>::range(&u, self.slot, &start, &self.prefix)
                    .take(chunk)
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            };
            if rows.len() == chunk {
                if let Some((k, _)) = rows.last() {
                    let mut after = k.clone();
                    after.push(0);
                    self.next = Some(after);
                }
            }
            self.buf = rows.into_iter();
        }
    }
}

impl ResidentSource for SharedUsers {
    fn get(&self, slot: usize, key: &[u8]) -> Option<Vec<u8>> {
        <Users as ResidentSource>::get(&self.read(), slot, key)
    }

    fn get_len(&self, slot: usize, key: &[u8]) -> Option<usize> {
        <Users as ResidentSource>::get_len(&self.read(), slot, key)
    }

    fn range<'a>(&'a self, slot: usize, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a> {
        Box::new(ChunkedRange {
            src: self,
            slot,
            prefix: prefix.to_vec(),
            next: Some(start.to_vec()),
            buf: Vec::new().into_iter(),
        })
    }

    fn in_place(&self) -> bool {
        self.in_place
    }

    fn put_row(&self, slot: usize, key: &[u8], value: Option<&[u8]>) {
        debug_assert!(self.in_place, "put_row on the reference path");
        self.write().mutate().put_row(slot, key, value);
    }

    fn dirty_keys(&self, slot: usize) -> Vec<Vec<u8>> {
        self.read().dirty_key_list(slot)
    }

    fn touches(&self, slot: usize, prefix: &[u8]) -> bool {
        self.read().dirty_touches(slot, prefix)
    }

    fn changes(&self, slot: usize, prefix: &[u8]) -> Option<Vec<ResidentChange>> {
        self.read().dirty_changes(slot, prefix)
    }

    fn drain(&self) -> [SourceRows; SOURCE_SLOTS] {
        let timer = std::time::Instant::now();
        let rows = self.write().drain_dirty();
        let n = (rows.positions.len() + rows.balances.len()) as u64;
        self.drained.fetch_add(n, Ordering::Relaxed);
        self.drain_ns.fetch_add(timer.elapsed().as_nanos() as u64, Ordering::Relaxed);
        [rows.positions, rows.balances]
    }
}

/// Phase 4 step 4's typed loads, through the read lock.
impl TypedRows for SharedUsers {
    #[inline]
    fn position(&self, trader: &Address, market_id: MarketId) -> Option<Option<Position>> {
        self.read().position(trader, market_id)
    }

    #[inline]
    fn balance(&self, trader: &Address) -> Option<Option<NativeBalance>> {
        self.read().balance(trader)
    }

    #[inline]
    fn cum_volume(&self, trader: &Address) -> Option<Option<FixedPoint>> {
        self.read().cum_volume(trader)
    }
}

/// Phase 4 step 2: one row the record set holds, before it is encoded.
#[derive(Clone, Copy)]
enum Row<'a> {
    Raw(&'a [u8]),
    Position(&'a Position),
    Balance(&'a NativeBalance),
    Volume(FixedPoint),
}

impl<'a> Row<'a> {
    /// Its bytes, as R held them (a record re-encodes to its row: regular).
    #[inline]
    fn value(self) -> Cow<'a, [u8]> {
        match self {
            Self::Raw(v) => Cow::Borrowed(v),
            Self::Position(p) => Cow::Owned(encode(p, POSITION_V2_LEN)),
            Self::Balance(b) => Cow::Owned(encode(b, BALANCE_LEN)),
            Self::Volume(v) => Cow::Owned(v.raw().to_be_bytes().to_vec()),
        }
    }

    #[inline]
    fn len(self) -> usize {
        match self {
            Self::Raw(v) => v.len(),
            Self::Position(_) => POSITION_V2_LEN,
            Self::Balance(_) => BALANCE_LEN,
            Self::Volume(_) => VOLUME_LEN,
        }
    }
}

fn borrowed<'a>((k, v): (&'a Vec<u8>, &'a Vec<u8>)) -> ResidentRow<'a> {
    (Cow::Borrowed(k.as_slice()), Cow::Borrowed(v.as_slice()))
}

/// The smallest trader whose keys may be >= `start`: every key of a trader
/// below it is below `start` (`start`'s first 20 bytes, zero-padded).
fn first_trader_from(start: &[u8]) -> Address {
    let mut a = [0u8; TRADER];
    let n = start.len().min(TRADER);
    a[..n].copy_from_slice(&start[..n]);
    Address::new(a)
}

// Phase 4 step 2: the balances slow path's runs (the collect-and-sort of
// every balances row; tests count it).
#[cfg(test)]
thread_local! {
    static BALANCE_SLOW_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Phase 4 step 2 (tests): balances ranges this thread answered from the
/// collect-and-sort slow path.
#[cfg(test)]
pub(crate) fn balance_slow_scans() -> usize {
    BALANCE_SLOW_SCANS.with(std::cell::Cell::get)
}

/// A k-way merge of key-ordered row runs with disjoint keys.
struct MergeRows<'a> {
    runs: Vec<ResidentRowIter<'a>>,
    /// Each run's next row.
    heads: Vec<Option<ResidentRow<'a>>>,
}

impl<'a> MergeRows<'a> {
    fn new(mut runs: Vec<ResidentRowIter<'a>>) -> Self {
        let heads = runs.iter_mut().map(Iterator::next).collect();
        Self { runs, heads }
    }
}

impl<'a> Iterator for MergeRows<'a> {
    type Item = ResidentRow<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut best: Option<usize> = None;
        for (i, head) in self.heads.iter().enumerate() {
            let Some((k, _)) = head else { continue };
            let smaller = match best.and_then(|b| self.heads[b].as_ref()) {
                Some((bk, _)) => k.as_ref() < bk.as_ref(),
                None => true,
            };
            if smaller {
                best = Some(i);
            }
        }
        let i = best?;
        let next = self.runs[i].next();
        std::mem::replace(&mut self.heads[i], next)
    }
}

impl Users {
    /// The positions row at `key`, as R held it.
    fn position_row(&self, key: &[u8]) -> Option<Row<'_>> {
        if key.len() < TRADER {
            return self.other.positions.get(key).map(|v| Row::Raw(v));
        }
        let t = Address::from_slice(&key[..TRADER]);
        if let Some(raw) = self.opaque_of(&t) {
            return raw.positions.get(key).map(|v| Row::Raw(v));
        }
        // A regular trader's position rows are all `t ‖ m`.
        if key.len() != KEY {
            return None;
        }
        find(&self.map.get(&t)?.positions, market_of(key)).map(Row::Position)
    }

    /// The balances row at `key`, as R held it.
    fn balance_row(&self, key: &[u8]) -> Option<Row<'_>> {
        let Some((t, row)) = balance_owner(key) else {
            return self.other.balances.get(key).map(|v| Row::Raw(v));
        };
        if let Some(raw) = self.opaque_of(&t) {
            return raw.balances.get(key).map(|v| Row::Raw(v));
        }
        let u = self.map.get(&t)?;
        match row {
            BalanceRow::Balance => u.balance.as_ref().map(Row::Balance),
            BalanceRow::Volume => u.cum_volume.map(Row::Volume),
        }
    }

    fn row(&self, slot: usize, key: &[u8]) -> Option<Row<'_>> {
        debug_assert!(slot == POSITIONS || slot == BALANCES, "source slot {slot}");
        match slot {
            POSITIONS => self.position_row(key),
            _ => self.balance_row(key),
        }
    }

    /// The positions CF from `start` while under `prefix`, in key order.
    fn position_range<'a>(&'a self, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a> {
        let under = prefix.to_vec();
        if prefix.len() >= TRADER && start >= prefix {
            // One trader's rows: every key under `prefix` is `t`'s, and they
            // all lie in `[prefix, start)` or from `start` on (start >= prefix).
            let t = Address::from_slice(&prefix[..TRADER]);
            if let Some(raw) = self.opaque_of(&t) {
                return Box::new(
                    raw.positions
                        .range::<[u8], _>((Bound::Included(start), Bound::Unbounded))
                        .take_while(move |(k, _)| k.starts_with(&under))
                        .map(borrowed),
                );
            }
            let ps = self.map.get(&t).map_or(&[][..], |u| u.positions.as_slice());
            let from = start.to_vec();
            return Box::new(
                ps.iter()
                    .map(move |p| (position_key(&t, p.market_id), p))
                    .skip_while(move |(k, _)| k.as_slice() < from.as_slice())
                    .take_while(move |(k, _)| k.starts_with(&under))
                    .map(|(k, p)| (Cow::Owned(k.to_vec()), Row::Position(p).value())),
            );
        }
        // The traders in address order (each one's positions in market order),
        // merged with the opaque traders' and the other rows.
        let from = start.to_vec();
        let regular = self
            .traders
            .range(first_trader_from(start)..)
            .filter(move |t| self.opaque_of(t).is_none())
            .flat_map(move |t| {
                let ps = self.map.get(t).map_or(&[][..], |u| u.positions.as_slice());
                ps.iter().map(move |p| (position_key(t, p.market_id), p))
            })
            .skip_while(move |(k, _)| k.as_slice() < from.as_slice())
            .map(|(k, p)| (Cow::Owned(k.to_vec()), Row::Position(p).value()));
        let range = || (Bound::Included(start), Bound::Unbounded);
        let mut opaque: Vec<ResidentRow<'a>> =
            self.opaque.values().flat_map(|r| r.positions.range::<[u8], _>(range()).map(borrowed)).collect();
        opaque.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let other = self.other.positions.range::<[u8], _>(range()).map(borrowed);
        let runs: Vec<ResidentRowIter<'a>> = vec![Box::new(regular), Box::new(opaque.into_iter()), Box::new(other)];
        Box::new(MergeRows::new(runs).take_while(move |(k, _)| k.starts_with(&under)))
    }

    /// The balances CF from `start` while under `prefix`, in key order: one
    /// trader's rows and the rows under no trader for a prefix naming one
    /// trader, else every row collected and sorted (the slow path; only the
    /// proof-only `liquidation_value_sum` walks the whole CF).
    fn balance_range<'a>(&'a self, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a> {
        let under = prefix.to_vec();
        let mut rows: Vec<ResidentRow<'a>> = Vec::new();
        // A prefix of 20+ bytes holds at most one balance row (the prefix
        // itself, 20 bytes) or one `cvlm` row (the prefix itself, a 24-byte
        // `cvlm` key) and rows under no trader — unless it is a `cvlm` head
        // shorter than a `cvlm` key (many traders' `cvlm` rows).
        let one_trader = prefix.len() >= TRADER && start >= prefix && !(prefix.len() < CVLM_KEY && prefix.starts_with(CVLM));
        if one_trader {
            let own = prefix.len() == TRADER || (prefix.len() == CVLM_KEY && prefix.starts_with(CVLM));
            if own && prefix >= start {
                if let Some(row) = self.balance_row(prefix) {
                    rows.push((Cow::Owned(prefix.to_vec()), row.value()));
                }
            }
            rows.extend(
                self.other
                    .balances
                    .range::<[u8], _>((Bound::Included(start), Bound::Unbounded))
                    .take_while(|(k, _)| k.starts_with(prefix))
                    .map(borrowed),
            );
        } else {
            #[cfg(test)]
            BALANCE_SLOW_SCANS.with(|c| c.set(c.get() + 1));
            for (t, u) in &self.map {
                if u.balance.is_some() && t.as_slice() >= start {
                    rows.push((Cow::Owned(t.to_vec()), self.balance_row(t.as_slice()).expect("a balance").value()));
                }
                if let Some(v) = u.cum_volume {
                    let k = cum_volume_key(t);
                    if k.as_slice() >= start {
                        rows.push((Cow::Owned(k.to_vec()), Row::Volume(v).value()));
                    }
                }
            }
            let range = || (Bound::Included(start), Bound::Unbounded);
            for r in self.opaque.values().chain([&self.other]) {
                rows.extend(r.balances.range::<[u8], _>(range()).map(borrowed));
            }
        }
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Box::new(rows.into_iter().take_while(move |(k, _)| k.starts_with(&under)))
    }
}

/// Phase 4 step 4: the typed loads (`PositionCache`, `BalanceCache`, the
/// `cum_volume` reads) of a key with no own pending row in the block: a
/// regular trader's record answers (no row: `Some(None)`), exactly the row R
/// encodes from it ([`ResidentSource::get`]); an opaque trader declines and
/// the caller reads its raw row through the overlay. The keys asked are a
/// trader's own (`t ‖ m`, `t`, `cvlm ‖ t`), never a row under no trader.
impl TypedRows for Users {
    #[inline]
    fn position(&self, trader: &Address, market_id: MarketId) -> Option<Option<Position>> {
        let Some(ps) = self.get(trader) else {
            self.count_typed(None);
            return None;
        };
        self.count_typed(Some(0));
        Some(find(ps, market_id).cloned())
    }

    #[inline]
    fn balance(&self, trader: &Address) -> Option<Option<NativeBalance>> {
        if self.opaque_of(trader).is_some() {
            self.count_typed(None);
            return None;
        }
        self.count_typed(Some(1));
        Some(self.map.get(trader).and_then(|u| u.balance.clone()))
    }

    #[inline]
    fn cum_volume(&self, trader: &Address) -> Option<Option<FixedPoint>> {
        if self.opaque_of(trader).is_some() {
            self.count_typed(None);
            return None;
        }
        self.count_typed(Some(2));
        Some(self.map.get(trader).and_then(|u| u.cum_volume))
    }
}

impl Users {
    /// Phase 4 step 4 (tests): count one typed load (`Some(kind)`: answered
    /// from a record; `None`: declined).
    #[inline]
    fn count_typed(&self, _kind: Option<usize>) {
        #[cfg(test)]
        {
            let c = match _kind {
                Some(k) => &self.typed.0[k],
                None => &self.typed.1,
            };
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Phase 4 step 4 (tests): typed loads answered per kind (position,
    /// balance, volume) and declined, so far.
    #[cfg(test)]
    pub(crate) fn typed_counts(&self) -> ([usize; 3], usize) {
        let load = |c: &std::sync::atomic::AtomicUsize| c.load(std::sync::atomic::Ordering::Relaxed);
        (self.typed.0.each_ref().map(load), load(&self.typed.1))
    }
}

/// Phase 4 step 2: R's positions (slot 0) and balances (slot 1), every row:
/// a regular trader's from its record (encoded per read), an opaque trader's
/// and the rows under no trader as bytes.
impl ResidentSource for Users {
    fn get(&self, slot: usize, key: &[u8]) -> Option<Vec<u8>> {
        self.row(slot, key).map(|r| r.value().into_owned())
    }

    fn get_len(&self, slot: usize, key: &[u8]) -> Option<usize> {
        self.row(slot, key).map(Row::len)
    }

    fn range<'a>(&'a self, slot: usize, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a> {
        debug_assert!(slot == POSITIONS || slot == BALANCES, "source slot {slot}");
        match slot {
            POSITIONS => self.position_range(start, prefix),
            _ => self.balance_range(start, prefix),
        }
    }
}

/// Item 6 E2: the traders of `state`'s own pending 28-byte position keys
/// strictly after `after`, ascending, each once (`None`: R not attached).
fn dirty_traders_after<B: StateBackend>(state: &B, after: Option<Address>) -> Option<Vec<Address>> {
    let pending = state.layer_keys(CF_NATIVE_POSITIONS)?;
    let mut dirty: Vec<Address> =
        pending.iter().filter(|k| k.len() == KEY).map(|k| Address::from_slice(&k[..TRADER])).collect();
    dirty.dedup(); // key-sorted: a trader's keys are adjacent
    let start = after.map_or(0, |a| dirty.partition_point(|t| *t <= a));
    dirty.drain(..start);
    Some(dirty)
}

/// Item 6 E2: [`Users::traders_after`]'s merge of the record set's traders
/// from the cursor on (`base`, ascending) with the block's dirty traders
/// (`dirty`, ascending): a base trader that is not dirty is a candidate; a
/// dirty one is looked up through `state`. At most `limit`; `base` is read
/// at most `limit + dirty.len()` far.
fn merge_traders<B: StateBackend>(
    state: &B,
    base: impl Iterator<Item = Address>,
    dirty: &[Address],
    limit: usize,
) -> Result<Vec<Address>, StateError> {
    let mut base = base.peekable();
    let mut dirty = dirty.iter().peekable();
    let mut out = Vec::new();
    while out.len() < limit {
        let t = match (base.peek(), dirty.peek()) {
            (None, None) => break,
            (Some(&b), Some(&&d)) if b < d => {
                base.next();
                out.push(b);
                continue;
            }
            (Some(&b), None) => {
                base.next();
                out.push(b);
                continue;
            }
            (_, Some(&&d)) => d,
        };
        dirty.next();
        if base.peek().is_some_and(|b| *b == t) {
            base.next();
        }
        if has_key(state, &t)? {
            out.push(t);
        }
    }
    Ok(out)
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

/// Phase 4 step 6 (`docs/plans/item6-phase4-impl.md` "Step 6"): the seam of
/// the users RSS ubench (`tests/ubench_users_rss.rs`, an integration test,
/// which cannot name the crate-private record set). A cold build of the
/// record set from raw rows of both CFs (the build R's rebuild runs) and
/// what the ubench checks on it. Not an API: nothing in the node calls it.
#[doc(hidden)]
pub mod bench {
    use std::collections::BTreeMap;

    use super::{Users, CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
    use crate::native_executor::{put_sums, PosSums};
    use torus_types::FixedPoint;

    /// One record set, built cold.
    pub struct UsersProbe(Users);

    impl UsersProbe {
        /// The record set of the rows `pos` (positions CF) and `bal`
        /// (balances CF), as R's rebuild builds it.
        pub fn build(pos: &BTreeMap<Vec<u8>, Vec<u8>>, bal: &BTreeMap<Vec<u8>, Vec<u8>>) -> Self {
            Self(Users::build_maps(pos, bal))
        }

        /// Regular traders (with a record).
        pub fn regular(&self) -> usize {
            self.0.map.len()
        }

        /// Traders kept raw (an irregular row) plus rows under no trader.
        pub fn raw_rows(&self) -> usize {
            self.0.opaque.values().chain([&self.0.other]).map(|r| r.footprint().0).sum()
        }

        /// Positions held on records.
        pub fn positions(&self) -> usize {
            self.0.map.values().map(|u| u.positions.len()).sum()
        }

        /// Records with a balance row, and with a `cvlm` row.
        pub fn balances_and_volumes(&self) -> (usize, usize) {
            let n = |f: fn(&super::UserState) -> bool| self.0.map.values().filter(|u| f(u)).count();
            (n(|u| u.balance.is_some()), n(|u| u.cum_volume.is_some()))
        }

        /// E2's trader set, and adl C2's (market, holder) entries.
        pub fn traders_and_holders(&self) -> (usize, usize) {
            (self.0.traders.len(), self.0.holders.values().map(|h| h.len()).sum())
        }

        /// The record map's capacity (its one table allocation).
        pub fn map_capacity(&self) -> usize {
            self.0.map.capacity()
        }

        /// Rows held and their key + value bytes (the users gauges).
        pub fn rows_and_bytes(&self) -> (usize, usize) {
            (self.0.row_count(), self.0.row_bytes())
        }

        /// The rows of `positions` (else balances) as R holds them.
        pub fn encode_rows(&self, positions: bool) -> BTreeMap<Vec<u8>, Vec<u8>> {
            self.0.encode_rows(if positions { CF_NATIVE_POSITIONS } else { CF_NATIVE_BALANCES })
        }

        /// Give every record a sums entry at mark version `version` (the
        /// steady state: `end_resident` writes one per valued trader and a
        /// moving version clears none). Returns the records given one.
        pub fn fill_sums(&mut self, version: u64) -> usize {
            let z = FixedPoint::ZERO;
            let sums = PosSums { upnl: z, position_im: z, notional: z, maintenance: z, isolated: 0, marked: 0, abs: [0; 4] };
            let mut n = 0;
            for u in self.0.map.values_mut() {
                put_sums(&mut u.sums, version, Ok(sums));
                n += 1;
            }
            n
        }

        /// Records whose sums entry is tagged `version`.
        pub fn sums_at(&self, version: u64) -> usize {
            self.0.map.keys().filter(|t| self.0.sums(t, version).is_some()).count()
        }
    }
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
