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
    pub(crate) fn apply(&mut self, delta: &ResidentDelta, rows: &ResidentRows) {
        let mut reload: Vec<Address> = Vec::new();
        for (k, v) in delta.entries(CF_NATIVE_POSITIONS) {
            if k.len() < TRADER {
                continue;
            }
            let t = Address::from_slice(&k[..TRADER]);
            if !self.opaque.is_empty() && self.opaque.contains(&t) {
                reload.push(t);
                continue;
            }
            match v {
                // A regular trader holds no irregular row: nothing to remove.
                None if k.len() != KEY => {}
                None => {
                    let m = MarketId::from_be_bytes(k[TRADER..].try_into().expect("28-byte key"));
                    if let Some(ps) = self.map.get_mut(&t) {
                        if let Ok(i) = ps.binary_search_by_key(&m, |p| p.market_id) {
                            ps.remove(i);
                            if ps.is_empty() {
                                self.map.remove(&t);
                            }
                        }
                    }
                }
                Some(v) => match decode(k, v) {
                    Some(p) => {
                        let ps = self.map.entry(t).or_default();
                        match ps.binary_search_by_key(&p.market_id, |q| q.market_id) {
                            Ok(i) => ps[i] = p,
                            Err(i) => ps.insert(i, p),
                        }
                    }
                    None => reload.push(t),
                },
            }
        }
        // The delta is key-sorted: a trader's entries are adjacent.
        reload.dedup();
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
