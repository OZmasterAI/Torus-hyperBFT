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

use std::collections::{HashMap, HashSet};

use alloy_primitives::Address;
use borsh::BorshDeserialize;
use torus_core::position::Position;
use torus_state::cf::CF_NATIVE_POSITIONS;
use torus_state::{ResidentDelta, ResidentRows};
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
        let mut entries = delta.entries(CF_NATIVE_POSITIONS).filter(|(k, _)| k.len() >= TRADER).peekable();
        while let Some(first) = entries.next() {
            group.clear();
            group.push(first);
            while let Some(e) = entries.next_if(|(k, _)| k[..TRADER] == first.0[..TRADER]) {
                group.push(e);
            }
            let t = Address::from_slice(&first.0[..TRADER]);
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
        for t in reload {
            let ps = r
                .range::<[u8], _>((std::ops::Bound::Included(t.as_slice()), std::ops::Bound::Unbounded))
                .take_while(|(k, _)| k.starts_with(t.as_slice()))
                .map(|(k, v)| decode(k, v))
                .collect();
            self.set(t, ps);
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

    /// `trader`'s positions in key order (empty: no rows), `None` if opaque.
    #[inline]
    pub(crate) fn get(&self, trader: &Address) -> Option<&[Position]> {
        if !self.opaque.is_empty() && self.opaque.contains(trader) {
            return None;
        }
        Some(self.map.get(trader).map_or(&[], Vec::as_slice))
    }

    /// Same traders, same positions (tests and `ResidentBooks` introspection).
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        let enc = |ps: &Vec<Position>| ps.iter().map(|p| borsh::to_vec(p).ok()).collect::<Vec<_>>();
        self.opaque == other.opaque
            && self.map.len() == other.map.len()
            && self.map.iter().all(|(t, ps)| other.map.get(t).is_some_and(|o| enc(o) == enc(ps)))
    }
}

/// The position in `market_id` of a trader's record.
#[inline]
pub(crate) fn find(ps: &[Position], market_id: MarketId) -> Option<&Position> {
    ps.binary_search_by_key(&market_id, |p| p.market_id).ok().map(|i| &ps[i])
}

#[cfg(test)]
#[path = "trader_positions_tests.rs"]
mod tests;
