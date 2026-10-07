//! Item 6 C7 (plan 5.2): R's position rows decoded per trader, kept in the
//! resident rows slot next to R and updated from each block's
//! [`ResidentDelta`] at `end_resident` (built cold with R). Node-local, never
//! consensus-visible, no format change.
//!
//! A trader's record is exactly what reading R through the overlay gives a
//! trader with nothing pending: `positions_for_trader` = every row under its
//! 20-byte prefix decoded, in key order; `get_position(t, m)` = the row at
//! `t ‖ m`. That holds only when every row under the prefix is regular: a
//! 28-byte key `t ‖ m` (big-endian, so key order = market order) whose value
//! decodes to a position of trader `t` and market `m`. A trader with any other
//! row (a longer or bare-prefix key, a value that does not decode or names
//! another trader / market) is opaque: it has no record and reads through the
//! overlay, as today. Keys shorter than 20 bytes are under no trader.
//! Readers use a record only for a trader with no own pending position
//! writes in the block (`AccountReader`).
//!
//! Item 6 E2: the traders of R's 28-byte keys, sorted, kept with the records
//! (same lifecycle), so the liquidation step's candidate list
//! (`liq::traders_after`) is a slice of them instead of one seek over R per
//! trader ([`TraderPositions::traders_after`]).
//!
//! adl-budget C2 (owner 18c s96 / s99): per market, the traders of R's keys
//! `t ‖ m` (same lifecycle), so an ADL ranking of `m` reads the holders of
//! `m` (merged with the block's dirty traders of `m`) instead of every
//! trader ([`TraderPositions::holders_with`], [`dirty_by_market`]).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Bound;

use alloy_primitives::Address;
use borsh::BorshDeserialize;
use torus_core::position::Position;
use torus_state::cf::CF_NATIVE_POSITIONS;
use torus_state::error::StateError;
use torus_state::{ResidentDelta, ResidentRows, StateBackend};
use torus_types::MarketId;

/// A positions key's trader prefix / a regular key's length.
const TRADER: usize = 20;
const KEY: usize = 28;

/// Item 6 step 1: one changed row of a regular trader at the end of a block:
/// its position before the block and after (`None`: no row).
pub(crate) type Change = (Option<Position>, Option<Position>);

/// Item 6 step 1: [`TraderPositions::apply`]'s per-trader report.
pub(crate) type Seen<'a> = dyn FnMut(&Address, Option<(&[Change], &[Position])>) + 'a;

#[derive(Debug, Default)]
pub(crate) struct TraderPositions {
    /// Regular traders with at least one row: their positions in key order.
    map: HashMap<Address, Vec<Position>>,
    /// Traders with an irregular row (normally none).
    opaque: HashSet<Address>,
    /// Item 6 E2: every trader with a 28-byte key in R (regular or opaque),
    /// ascending: the candidates of the liquidation walk.
    traders: BTreeSet<Address>,
    /// adl-budget C2: per market `m`, every trader with the 28-byte key
    /// `t ‖ m` in R (regular or opaque), ascending. A market without one has
    /// no entry. Followed per key from the delta (a write adds, a tombstone
    /// removes): O(log n) per changed key.
    holders: HashMap<MarketId, BTreeSet<Address>>,
}

/// The market of a 28-byte positions key `t ‖ m`.
fn market_of(key: &[u8]) -> MarketId {
    MarketId::from_be_bytes(key[TRADER..KEY].try_into().expect("28-byte key"))
}

/// The position of a regular row, `None` for an irregular one.
fn decode(key: &[u8], value: &[u8]) -> Option<Position> {
    if key.len() != KEY {
        return None;
    }
    let p = Position::try_from_slice(value).ok()?;
    (p.trader.as_slice() == &key[..TRADER] && p.market_id.to_be_bytes() == key[TRADER..]).then_some(p)
}

impl TraderPositions {
    /// Every trader of R's positions (cold: with R, or when a block's state
    /// did not come back).
    pub(crate) fn build(rows: &ResidentRows) -> Self {
        let mut out = Self::default();
        let mut cur: Option<(Address, Option<Vec<Position>>)> = None;
        for (k, v) in rows.rows(CF_NATIVE_POSITIONS).into_iter().flatten() {
            if k.len() < TRADER {
                continue;
            }
            if cur.as_ref().is_none_or(|(t, _)| t.as_slice() != &k[..TRADER]) {
                if let Some((t, ps)) = cur.take() {
                    out.set(t, ps);
                }
                cur = Some((Address::from_slice(&k[..TRADER]), Some(Vec::new())));
            }
            if let Some((_, slot)) = cur.as_mut() {
                *slot = slot.take().and_then(|mut ps| {
                    ps.push(decode(k, v)?);
                    Some(ps)
                });
            }
        }
        if let Some((t, ps)) = cur {
            out.set(t, ps);
        }
        for (k, _) in rows.rows(CF_NATIVE_POSITIONS).into_iter().flatten().filter(|(k, _)| k.len() == KEY) {
            let t = Address::from_slice(&k[..TRADER]);
            out.traders.insert(t);
            out.holders.entry(market_of(k)).or_default().insert(t);
        }
        out
    }

    /// `t`'s rows decoded (`None`: an irregular row).
    fn set(&mut self, t: Address, ps: Option<Vec<Position>>) {
        match ps {
            Some(mut ps) => {
                self.opaque.remove(&t);
                if ps.is_empty() {
                    self.map.remove(&t);
                } else {
                    // ~96 bytes per position, without the build's growth slack.
                    ps.shrink_to_fit();
                    self.map.insert(t, ps);
                }
            }
            None => {
                self.map.remove(&t);
                self.opaque.insert(t);
            }
        }
    }

    /// End of a block: follow `delta` (the block's own writes and tombstones),
    /// with `rows` = R after it. A regular write or a tombstone of a regular
    /// trader updates its record in place (only the written rows decode); any
    /// other change of a trader re-decodes its rows from `rows`.
    ///
    /// Item 6 step 1: `seen` (if any) is called once per trader the delta
    /// writes under (key order), right after its rows are followed:
    /// `Some((changes, now))` for a trader regular before and after the
    /// block, with each changed row's position before / after (composing
    /// exactly from its record before the block to `now`, its record after);
    /// `None` when its rows are re-decoded (opaque before, or an irregular
    /// write).
    pub(crate) fn apply(&mut self, delta: &ResidentDelta, rows: &ResidentRows, mut seen: Option<&mut Seen<'_>>) {
        let mut reload: Vec<Address> = Vec::new();
        let mut group: Vec<(&[u8], Option<&[u8]>)> = Vec::new();
        let mut changes: Vec<Change> = Vec::new();
        // The delta is key-sorted: a trader's entries are adjacent.
        // E2: traders whose 28-byte keys the delta writes or deletes (only
        // those can enter or leave the trader set).
        let mut keyed: Vec<Address> = Vec::new();
        let mut entries = delta.entries(CF_NATIVE_POSITIONS).filter(|(k, _)| k.len() >= TRADER).peekable();
        while let Some(first) = entries.next() {
            group.clear();
            group.push(first);
            while let Some(e) = entries.next_if(|(k, _)| k[..TRADER] == first.0[..TRADER]) {
                group.push(e);
            }
            let t = Address::from_slice(&first.0[..TRADER]);
            if group.iter().any(|(k, _)| k.len() == KEY) {
                keyed.push(t);
            }
            // C2: R after the block holds `t ‖ m` iff the delta wrote it.
            for &(k, v) in group.iter().filter(|(k, _)| k.len() == KEY) {
                self.set_holder(t, market_of(k), v.is_some());
            }
            changes.clear();
            if !self.follow(t, &group, &mut changes, seen.as_deref_mut()) {
                reload.push(t);
                if let Some(f) = seen.as_deref_mut() {
                    f(&t, None);
                }
            }
        }
        let Some(r) = rows.rows(CF_NATIVE_POSITIONS) else {
            return;
        };
        let under = |t: Address| {
            r.range::<[u8], _>((Bound::Included(t.as_slice()), Bound::Unbounded))
                .take_while(move |(k, _)| k.starts_with(t.as_slice()))
        };
        for t in reload {
            let ps = under(t).map(|(k, v)| decode(k, v)).collect();
            self.set(t, ps);
        }
        // E2: a regular trader's rows are all 28-byte keys (it has one iff it
        // has a record); an opaque trader's are looked up in R.
        for t in keyed {
            let keyed_now = if self.opaque.contains(&t) {
                under(t).any(|(k, _)| k.len() == KEY)
            } else {
                self.map.contains_key(&t)
            };
            if keyed_now {
                self.traders.insert(t);
            } else {
                self.traders.remove(&t);
            }
        }
    }

    /// One trader's entries of the delta (`group`, key order) on its record;
    /// `false`: its rows must be re-decoded (opaque, or an irregular write).
    /// With `seen`, the changed rows are collected into `changes` and handed
    /// to it with the record after them.
    fn follow(
        &mut self,
        t: Address,
        group: &[(&[u8], Option<&[u8]>)],
        changes: &mut Vec<Change>,
        seen: Option<&mut Seen<'_>>,
    ) -> bool {
        if !self.opaque.is_empty() && self.opaque.contains(&t) {
            return false;
        }
        let collect = seen.is_some();
        let ps = self.map.entry(t).or_default();
        for &(k, v) in group {
            match v {
                // A regular trader holds no irregular row: nothing to remove.
                None if k.len() != KEY => {}
                None => {
                    let m = MarketId::from_be_bytes(k[TRADER..].try_into().expect("28-byte key"));
                    if let Ok(i) = ps.binary_search_by_key(&m, |p| p.market_id) {
                        let old = ps.remove(i);
                        if collect {
                            changes.push((Some(old), None));
                        }
                    }
                }
                Some(v) => {
                    let Some(p) = decode(k, v) else {
                        // Re-decoded from R; an emptied record is removed there.
                        return false;
                    };
                    let new = collect.then(|| p.clone());
                    let old = match ps.binary_search_by_key(&p.market_id, |q| q.market_id) {
                        Ok(i) => Some(std::mem::replace(&mut ps[i], p)),
                        Err(i) => {
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
        if ps.is_empty() {
            self.map.remove(&t);
        }
        true
    }

    /// C2: whether R holds `t ‖ m` now.
    fn set_holder(&mut self, t: Address, m: MarketId, holds: bool) {
        if holds {
            self.holders.entry(m).or_default().insert(t);
        } else if let Some(set) = self.holders.get_mut(&m) {
            set.remove(&t);
            if set.is_empty() {
                self.holders.remove(&m);
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
        let mut out = Vec::with_capacity(base.map_or(0, BTreeSet::len) + dirty.len());
        let mut a = base.into_iter().flatten().peekable();
        let mut b = dirty.iter().peekable();
        loop {
            let t = match (a.peek(), b.peek()) {
                (None, None) => break,
                (Some(&&x), Some(&&y)) if x == y => {
                    a.next();
                    b.next();
                    x
                }
                (Some(&&x), Some(&&y)) if x < y => {
                    a.next();
                    x
                }
                (Some(&&x), None) => {
                    a.next();
                    x
                }
                (_, Some(&&y)) => {
                    b.next();
                    y
                }
            };
            out.push(t);
        }
        out
    }

    /// `trader`'s positions in key order (empty: no rows), `None` if opaque.
    #[inline]
    pub(crate) fn get(&self, trader: &Address) -> Option<&[Position]> {
        if !self.opaque.is_empty() && self.opaque.contains(trader) {
            return None;
        }
        Some(self.map.get(trader).map_or(&[], Vec::as_slice))
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

    /// Same traders, same positions (tests and `ResidentBooks` introspection).
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        let enc = |ps: &Vec<Position>| ps.iter().map(|p| borsh::to_vec(p).ok()).collect::<Vec<_>>();
        self.traders == other.traders
            && self.holders == other.holders
            && self.opaque == other.opaque
            && self.map.len() == other.map.len()
            && self.map.iter().all(|(t, ps)| other.map.get(t).is_some_and(|o| enc(o) == enc(ps)))
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
    let mut out = ByMarket::new();
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
#[path = "trader_positions_tests.rs"]
mod tests;
