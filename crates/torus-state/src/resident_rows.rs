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
//! Item 6 Phase 4 step 2 (`docs/plans/item6-phase4-impl.md` Task 2.1): R
//! holds the liquidation, oracle and market rows raw and reads positions and
//! balances (the first [`SOURCE_SLOTS`] CFs) from a [`ResidentSource`]: the
//! per-trader record set `Users` in torus-bridge (this crate cannot name its
//! types), which follows the delta of those two CFs itself. The source answers
//! both CFs completely (irregular rows included): no read of them goes past R,
//! so none reaches L's deferred part (p3 merge wait). Today's raw R for all
//! five CFs stays as the test-only reference path ([`ResidentRows::build`],
//! `cfg(test)` / feature `test-reference-paths`).
//!
//! Node-local cache, never consensus-visible: R is built from and equal to
//! what the DB (plus the parent layer) holds, every key included (`cvlm`
//! rows, keys of any length).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::backend::StateBackend;
use crate::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS};
use crate::error::StateError;

/// R's column families, in slot order: positions and balances (C1), and
/// item 6 E2's liquidation rows (cooldown / pending / previous-mark / cursor,
/// read per scanned trader by the liquidation step) and E3's oracle rows
/// (submissions, aggregates, signer index: the block-start oracle step reads
/// every submission and aggregate each block) and E4's market rows (listings
/// and their margin configs, scanned by the context, the oracle step and the
/// liquidation step each block; the order-id counter and book-mode marker,
/// written on the exec thread before the block's delta is taken). Written
/// only through the block's overlay, like the first two.
pub const RESIDENT_CFS: [&str; N] =
    [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_ORACLE, CF_NATIVE_MARKETS];
/// Number of R's column families.
const N: usize = 5;

/// Slot of `cf` in [`RESIDENT_CFS`], `None` for any other CF.
#[inline]
pub fn resident_slot(cf: &str) -> Option<usize> {
    RESIDENT_CFS.iter().position(|c| *c == cf)
}

/// Item 6 Phase 4 step 2: the first `SOURCE_SLOTS` CFs of [`RESIDENT_CFS`]
/// (positions, balances) are read from a [`ResidentSource`].
pub const SOURCE_SLOTS: usize = 2;

/// One row of a [`ResidentSource`]: key and value, borrowed when the source
/// holds the bytes, owned when it builds them (a record encoded per read).
pub type ResidentRow<'a> = (Cow<'a, [u8]>, Cow<'a, [u8]>);

/// Item 6 Phase 5 step 2: one slot's drained rows (a write, or `None`: a
/// tombstone), key-sorted ([`ResidentSource::drain`]).
pub type SourceRows = Vec<(Vec<u8>, Option<Vec<u8>>)>;

/// An ordered run of [`ResidentRow`]s.
pub type ResidentRowIter<'a> = Box<dyn Iterator<Item = ResidentRow<'a>> + 'a>;

/// Item 6 Phase 4 step 2 (plan row 12): a byte-level source for R's first
/// [`SOURCE_SLOTS`] CFs (slot 0 `CF_NATIVE_POSITIONS`, slot 1
/// `CF_NATIVE_BALANCES`), implemented by torus-bridge's `Users`. It must
/// hold every row of both CFs as the DB (plus the parent layer) does — the
/// overlay never reads them past R — and answer exactly as those rows held
/// in a sorted map would.
pub trait ResidentSource: Send + Sync {
    /// The row of `key` in `slot`, `None` if absent.
    fn get(&self, slot: usize, key: &[u8]) -> Option<Vec<u8>>;

    /// The length of [`Self::get`]'s row.
    fn get_len(&self, slot: usize, key: &[u8]) -> Option<usize> {
        self.get(slot, key).map(|v| v.len())
    }

    /// The rows of `slot` with key >= `start`, in key order, up to (not
    /// including) the first whose key does not start with `prefix` (a sorted
    /// map's `range(start..).take_while(|k| k.starts_with(prefix))`).
    fn range<'a>(&'a self, slot: usize, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a>;

    /// Item 6 Phase 5 step 2 (`docs/plans/item6-phase5-impl.md` Task 2.1):
    /// the source takes the block's writes of its CFs in place (the node's
    /// record set). Then the overlay keeps none of them pending outside an
    /// open tx scope ([`Self::put_row`]), answers the block's own-change
    /// questions of the two CFs from the source's dirty sets
    /// ([`Self::dirty_keys`], [`Self::touches`], [`Self::changes`]) merged
    /// with its own pending set, and fills its change set from
    /// [`Self::drain`] at the freeze / the flush. `false` (default): a
    /// read-only source (the Phase 4 path): every write stays pending.
    fn in_place(&self) -> bool {
        false
    }

    /// An in-place source's write (`Some`) / tombstone (`None`) of `key` in
    /// `slot`, applied at once and marked dirty (the first write of a key in
    /// a block keeps its row before the block). Never called on a source
    /// that is not [`Self::in_place`].
    fn put_row(&self, slot: usize, key: &[u8], value: Option<&[u8]>) {
        let _ = (slot, key, value);
        unreachable!("put_row on a read-only resident source");
    }

    /// The keys of `slot` the block changed in place so far, sorted.
    fn dirty_keys(&self, _slot: usize) -> Vec<Vec<u8>> {
        Vec::new()
    }

    /// Whether the block changed a key of `slot` under `prefix` in place.
    fn touches(&self, _slot: usize, _prefix: &[u8]) -> bool {
        false
    }

    /// The keys of `slot` under `prefix` the block changed in place, each
    /// with its row at the block start (`resident`) and now (`current`),
    /// in key order; `None`: not answered for `slot` (the caller falls back).
    fn changes(&self, _slot: usize, _prefix: &[u8]) -> Option<Vec<ResidentChange>> {
        Some(Vec::new())
    }

    /// The block's changed rows of every slot, each key once in key order
    /// and serialized once (a write, or `None`: a tombstone); the dirty sets
    /// are emptied. Called once per block, at its freeze or flush.
    fn drain(&self) -> [SourceRows; SOURCE_SLOTS] {
        Default::default()
    }
}

/// Who answers R's first two CFs.
#[derive(Clone)]
enum SourceSlots {
    /// R's own raw maps (today's R; the test-only reference path).
    #[cfg_attr(not(any(test, feature = "test-reference-paths")), allow(dead_code))]
    Raw,
    /// The record set (the node).
    Source(Arc<dyn ResidentSource>),
    /// Nobody (the source was taken off for the end-of-block upkeep): the
    /// overlay reads the two CFs below R, as without R.
    Detached,
}

/// One sorted map per raw resident CF, and the source of the first two.
#[derive(Clone)]
pub struct ResidentRows {
    /// Slots 0..SOURCE_SLOTS are empty unless `users` is `Raw`.
    cfs: [BTreeMap<Vec<u8>, Vec<u8>>; N],
    users: SourceSlots,
    /// Sum of key + value lengths over every map.
    bytes: usize,
}

/// Item 6 Phase 4 step 2: how R answers one CF slot for the overlay.
#[derive(Clone, Copy)]
pub(crate) enum Resident<'a> {
    Raw(&'a BTreeMap<Vec<u8>, Vec<u8>>),
    Source(&'a dyn ResidentSource, usize),
}

impl<'a> Resident<'a> {
    #[inline]
    pub(crate) fn get(self, key: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::Raw(map) => map.get(key).cloned(),
            Self::Source(s, slot) => s.get(slot, key),
        }
    }

    #[inline]
    pub(crate) fn get_len(self, key: &[u8]) -> Option<usize> {
        match self {
            Self::Raw(map) => map.get(key).map(Vec::len),
            Self::Source(s, slot) => s.get_len(slot, key),
        }
    }

    /// The rows with key >= `start`, up to the first not under `prefix`.
    #[inline]
    pub(crate) fn range<'k>(self, start: &'k [u8], prefix: &'k [u8]) -> ResidentRange<'a, 'k> {
        match self {
            Self::Raw(map) => ResidentRange::Raw {
                range: map.range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded)),
                prefix,
            },
            Self::Source(s, slot) => ResidentRange::Source(s.range(slot, start, prefix)),
        }
    }
}

/// [`Resident::range`]'s rows.
pub(crate) enum ResidentRange<'a, 'k> {
    Raw {
        range: std::collections::btree_map::Range<'a, Vec<u8>, Vec<u8>>,
        prefix: &'k [u8],
    },
    Source(ResidentRowIter<'a>),
    Done,
}

impl<'a> Iterator for ResidentRange<'a, '_> {
    type Item = ResidentRow<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Raw { range, prefix } => match range.next() {
                Some((k, v)) if k.starts_with(prefix) => Some((Cow::Borrowed(k.as_slice()), Cow::Borrowed(v.as_slice()))),
                _ => {
                    *self = Self::Done;
                    None
                }
            },
            Self::Source(it) => it.next(),
            Self::Done => None,
        }
    }
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

    /// Item 6 Phase 5 step 2: this delta without the source slots
    /// (positions, balances): R's raw CFs only.
    pub fn raw_only(mut self) -> Self {
        for slot in &mut self.cfs[..SOURCE_SLOTS] {
            slot.clear();
        }
        self
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

impl std::fmt::Debug for ResidentRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let users = match &self.users {
            SourceSlots::Raw => "raw",
            SourceSlots::Source(_) => "source",
            SourceSlots::Detached => "detached",
        };
        f.debug_struct("ResidentRows")
            .field("raw_rows", &self.len())
            .field("bytes", &self.bytes)
            .field("positions_and_balances", &users)
            .finish()
    }
}

/// Same rows in every CF (raw or from a source; tests and introspection).
impl PartialEq for ResidentRows {
    fn eq(&self, other: &Self) -> bool {
        RESIDENT_CFS.iter().all(|cf| self.dump(cf) == other.dump(cf))
    }
}

impl Eq for ResidentRows {}

/// Today's raw R, empty (the test-only reference path).
#[cfg(any(test, feature = "test-reference-paths"))]
impl Default for ResidentRows {
    fn default() -> Self {
        Self::empty(SourceSlots::Raw)
    }
}

impl ResidentRows {
    fn empty(users: SourceSlots) -> Self {
        Self { cfs: Default::default(), users, bytes: 0 }
    }

    /// The raw slots' rows as `backend` sees them (`iterate_cf(cf, None)`).
    fn fill<B: StateBackend>(&mut self, backend: &B, slots: std::ops::Range<usize>) -> Result<(), StateError> {
        for slot in slots {
            let entries = backend.iterate_cf(RESIDENT_CFS[slot], None)?;
            self.bytes += entries.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
            self.cfs[slot] = entries.into_iter().collect();
        }
        Ok(())
    }

    /// Today's raw R (the test-only reference path): every row of R's five
    /// CFs as `backend` sees them (`iterate_cf(cf, None)`). Build it through
    /// an overlay WITHOUT R attached: DB + parent layer = the previous
    /// block's post-state.
    #[cfg(any(test, feature = "test-reference-paths"))]
    pub fn build<B: StateBackend>(backend: &B) -> Result<Self, StateError> {
        let mut rows = Self::default();
        rows.fill(backend, 0..N)?;
        Ok(rows)
    }

    /// Item 6 Phase 4 step 2: R as the node builds it: the liquidation,
    /// oracle and market rows raw as `backend` sees them, positions and
    /// balances from `source` (built from the same `backend`). Build it
    /// through an overlay WITHOUT R attached.
    pub fn build_with_source<B: StateBackend>(backend: &B, source: Arc<dyn ResidentSource>) -> Result<Self, StateError> {
        let mut rows = Self::empty(SourceSlots::Source(source));
        rows.fill(backend, SOURCE_SLOTS..N)?;
        Ok(rows)
    }

    /// Item 6 Phase 4 step 2: read positions and balances from `source`
    /// from now on (raw maps of them, if any, are dropped).
    pub fn set_source(&mut self, source: Arc<dyn ResidentSource>) {
        for map in &mut self.cfs[..SOURCE_SLOTS] {
            self.bytes -= map.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
            *map = BTreeMap::new();
        }
        self.users = SourceSlots::Source(source);
    }

    /// Item 6 Phase 4 step 2: take the source off (the end of the block
    /// takes the record set back with `Arc::get_mut`). Until
    /// [`Self::set_source`], R does not hold positions and balances: an
    /// overlay reads them below R.
    pub fn take_source(&mut self) -> Option<Arc<dyn ResidentSource>> {
        match std::mem::replace(&mut self.users, SourceSlots::Detached) {
            SourceSlots::Source(s) => Some(s),
            other => {
                self.users = other;
                None
            }
        }
    }

    /// Whether positions and balances come from a source.
    pub fn has_source(&self) -> bool {
        matches!(self.users, SourceSlots::Source(_))
    }

    /// Item 6 Phase 5 step 2: the source of positions and balances if it
    /// takes the block's writes in place ([`ResidentSource::in_place`]).
    #[inline]
    pub(crate) fn in_place_source(&self) -> Option<&dyn ResidentSource> {
        match &self.users {
            SourceSlots::Source(s) if s.in_place() => Some(&**s),
            _ => None,
        }
    }

    /// How R answers CF `slot` of [`RESIDENT_CFS`] (`None`: it does not hold
    /// that CF now).
    #[inline]
    pub(crate) fn view(&self, slot: usize) -> Option<Resident<'_>> {
        if slot >= SOURCE_SLOTS {
            return Some(Resident::Raw(&self.cfs[slot]));
        }
        match &self.users {
            SourceSlots::Raw => Some(Resident::Raw(&self.cfs[slot])),
            SourceSlots::Source(s) => Some(Resident::Source(&**s, slot)),
            SourceSlots::Detached => None,
        }
    }

    /// Every row R holds of `cf` in key order, raw or from the source
    /// (`None`: `cf` is not resident, or its source is taken off). Tests and
    /// introspection.
    pub fn dump(&self, cf: &str) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
        let view = self.view(resident_slot(cf)?)?;
        Some(view.range(&[], &[]).map(|(k, v)| (k.into_owned(), v.into_owned())).collect())
    }

    /// Apply a block's own writes and tombstones to the raw CFs (positions
    /// and balances only when held raw: a source's owner applies them to
    /// it). Copies the values on purpose: moving the delta's buffers into R
    /// measured 2x slower upkeep and a ~2.5x slower liquidation walk over R
    /// in `ubench_econ` (s90).
    pub fn apply(&mut self, delta: &ResidentDelta) {
        let from = if matches!(self.users, SourceSlots::Raw) { 0 } else { SOURCE_SLOTS };
        for (map, entries) in self.cfs.iter_mut().zip(delta.cfs.iter()).skip(from) {
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

    /// The raw rows of `cf`, `None` if `cf` is not resident or not held raw
    /// (positions and balances from a source: [`Self::dump`]).
    pub fn rows(&self, cf: &str) -> Option<&BTreeMap<Vec<u8>, Vec<u8>>> {
        match self.view(resident_slot(cf)?)? {
            Resident::Raw(map) => Some(map),
            Resident::Source(..) => None,
        }
    }

    /// Raw rows over R's CFs (with a source: the liquidation, oracle and
    /// market rows).
    pub fn len(&self) -> usize {
        self.cfs.iter().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.cfs.iter().all(BTreeMap::is_empty)
    }

    /// Key + value bytes of R's raw rows (map overhead not included).
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
#[path = "resident_rows_tests.rs"]
mod tests;
