//! Item 6 Phase 1 (C1): resident rows R — every row of `CF_NATIVE_POSITIONS`
//! and `CF_NATIVE_BALANCES` (and, item 6 E2-E4, of the further CFs in
//! [`RESIDENT_CFS`]) in memory, kept across blocks by the execution
//! pipeline (`ResidentBooks` in torus-bridge).
//!
//! R holds the post-state of the previous block. A block's overlay reads R's
//! CFs from R in place of the DB ([`crate::NativeStateOverlay::attach_resident`]):
//! own pending -> R; a key absent from R is absent. C6a (B0): the parent layer
//! (the previous block's frozen writes) is not consulted for R's CFs, R
//! already holds it. At the end
//! of the block R takes the block's own writes and tombstones of R's CFs
//! ([`ResidentDelta`], from [`crate::NativeStateOverlay::own_pending_delta`]).
//!
//! Node-local cache, never consensus-visible: R is built from and equal to
//! what the DB (plus the parent layer) holds, every key included (`cvlm`
//! rows, keys of any length).

use std::collections::BTreeMap;

use crate::backend::StateBackend;
use crate::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_POSITIONS};
use crate::error::StateError;

/// R's column families, in slot order: positions and balances (C1), and
/// item 6 E2's liquidation rows (cooldown / pending / previous-mark / cursor,
/// read per scanned trader by the liquidation step). Written only through the
/// block's overlay, like the first two.
pub const RESIDENT_CFS: [&str; N] = [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION];
/// Number of R's column families.
const N: usize = 3;

/// Slot of `cf` in [`RESIDENT_CFS`], `None` for any other CF.
#[inline]
pub fn resident_slot(cf: &str) -> Option<usize> {
    RESIDENT_CFS.iter().position(|c| *c == cf)
}

/// One sorted map per resident CF.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct ResidentRows {
    cfs: [BTreeMap<Vec<u8>, Vec<u8>>; N],
    /// Sum of key + value lengths over every map.
    bytes: usize,
}

/// One block's own writes (`Some`) and tombstones (`None`) of R's CFs,
/// key-sorted per CF.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct ResidentDelta {
    pub(crate) cfs: [Vec<(Vec<u8>, Option<Vec<u8>>)>; N],
}

impl ResidentDelta {
    /// Entries (writes + tombstones) over R's CFs.
    pub fn len(&self) -> usize {
        self.cfs.iter().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.cfs.iter().all(Vec::is_empty)
    }

    /// Keys of `cf` the block wrote or deleted, sorted (none for a CF outside
    /// R). Item 6 C3: the traders whose cached margin sums the block dirtied.
    pub fn keys<'a>(&'a self, cf: &str) -> impl Iterator<Item = &'a [u8]> + 'a {
        resident_slot(cf)
            .into_iter()
            .flat_map(move |s| self.cfs[s].iter().map(|(k, _)| k.as_slice()))
    }

    /// The block's writes (`Some`) and tombstones (`None`) of `cf`, key-sorted
    /// (none for a CF outside R). Item 6 C7: the decoded per-trader positions
    /// follow them.
    pub fn entries<'a>(&'a self, cf: &str) -> impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)> + 'a {
        resident_slot(cf)
            .into_iter()
            .flat_map(move |s| self.cfs[s].iter().map(|(k, v)| (k.as_slice(), v.as_deref())))
    }
}

/// Item 6 C6b: one key of an R CF that the block's own pending set writes
/// (`current: Some`) or deletes (`current: None`), with R's row (the state
/// at the start of the block, parent layer included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentChange {
    pub key: Vec<u8>,
    pub resident: Option<Vec<u8>>,
    pub current: Option<Vec<u8>>,
}

impl ResidentRows {
    /// Every row of R's CFs as `backend` sees them (`iterate_cf(cf, None)`).
    /// Build it through an overlay WITHOUT R attached: DB + parent layer = the
    /// previous block's post-state.
    pub fn build<B: StateBackend>(backend: &B) -> Result<Self, StateError> {
        let mut rows = Self::default();
        for (slot, cf) in RESIDENT_CFS.iter().enumerate() {
            let entries = backend.iterate_cf(cf, None)?;
            rows.bytes += entries.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
            rows.cfs[slot] = entries.into_iter().collect();
        }
        Ok(rows)
    }

    /// Apply a block's own writes and tombstones. Copies the values on
    /// purpose: moving the delta's buffers into R measured 2x slower upkeep
    /// and a ~2.5x slower liquidation walk over R in `ubench_econ` (s90).
    pub fn apply(&mut self, delta: &ResidentDelta) {
        for (map, entries) in self.cfs.iter_mut().zip(delta.cfs.iter()) {
            for (key, value) in entries {
                let old = match value {
                    Some(v) => {
                        self.bytes += key.len() + v.len();
                        map.insert(key.clone(), v.clone())
                    }
                    None => map.remove(key),
                };
                if let Some(old) = old {
                    self.bytes -= key.len() + old.len();
                }
            }
        }
    }

    /// The rows of `cf`, `None` if `cf` is not resident.
    pub fn rows(&self, cf: &str) -> Option<&BTreeMap<Vec<u8>, Vec<u8>>> {
        resident_slot(cf).map(|s| &self.cfs[s])
    }

    #[inline]
    pub(crate) fn slot(&self, slot: usize) -> &BTreeMap<Vec<u8>, Vec<u8>> {
        &self.cfs[slot]
    }

    /// Rows over R's CFs.
    pub fn len(&self) -> usize {
        self.cfs.iter().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.cfs.iter().all(BTreeMap::is_empty)
    }

    /// Key + value bytes over R's CFs (map overhead not included).
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
#[path = "resident_rows_tests.rs"]
mod tests;
