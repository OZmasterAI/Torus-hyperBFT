//! Item 6 Phase 3: the since-checkpoint layer **L** (plan
//! `docs/plans/item6-phase3-impl.md` section 3, task 1.4).
//!
//! With a checkpoint interval set, a native block's state rows are no longer
//! written to RocksDB per block. The exec thread keeps the last value (or
//! tombstone) per `(CF, key)` over heights `S+1..top` in one
//! [`CheckpointLayer`], built by merging each block's [`FrozenPending`] set,
//! and the flush worker writes the whole layer as ONE atomic batch at the
//! next checkpoint (marker = `top`).
//!
//! Readers see the layer through [`crate::NativeStateOverlay::with_layers`]:
//! own pending -> R -> parent -> L -> C (checkpoint in flight) -> DB.
//!
//! Only state rows: a block's out-of-batch hash extras (already durable) are
//! not merged.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use rocksdb::WriteBatch;

use crate::backend::{CfPending, FrozenPending, PendingState, NUM_CFS};
use crate::{StateDb, StateError};

/// p3 merge wait: the CFs of L merged AFTER the end-of-block merge hands L
/// back to the exec thread ([`LayerPart::Deferred`], read through a
/// [`DeferredLayer`]). They are R's two big CFs: positions are ~92% of a
/// block's state keys (results doc 41.1), and a native block reads both
/// only from R (`NativeStateOverlay::attach_resident`), never through L. A
/// read that does reach them through L (no R attached: R's rebuild, a
/// block without R) waits for their merge, so the choice only moves time,
/// never what a read returns.
///
/// The deferred part is these two CFs plus the book CF with
/// `TORUS_CKPT_DEFER_BOOKS=1` (Classic, resident books; see
/// `parse_defer_books`); this constant stays the pair every process defers.
pub const DEFERRED_CFS: [&str; 2] = [
    crate::cf::CF_NATIVE_POSITIONS,
    crate::cf::CF_NATIVE_BALANCES,
];

/// `TORUS_CKPT_DEFER_BOOKS=1`: also defer `CF_NATIVE_ORDER_BOOKS` (Classic
/// whole-book blobs: ~77% of a block's state bytes, ~199 x ~44 KB per
/// block at 300 markets, results doc 41.1), so the exec thread no longer
/// waits at block start for their copy into L. On only if `flag` trims to
/// `1`, the book mode is Classic (`book_rows` is not 1 / 2 / 3, as
/// `parse_book_rows_mode` in torus-bridge) and resident books are on
/// (`resident_books` is not 0, as `parse_resident_books_toggle`). There a
/// native block never reads the book CF through L: the Classic save only
/// writes whole blobs and the books stay in memory between blocks. Modes
/// 1 / 2 / 3 and `TORUS_RESIDENT_BOOKS=0` read it every block, so deferring
/// would only move the wait.
///
/// Performance only: every split, merge and read of L routes by the same
/// table ([`is_deferred_slot`]), and a read that reaches a deferred CF
/// through L waits for its part and gets the answer the whole layer gives.
/// A gate that disagrees with the executor's real mode can only add waits
/// (counted in `torus_exec_checkpoint_deferred_read_waits`), never change
/// what a read returns or what a checkpoint writes.
pub(crate) fn parse_defer_books(
    flag: Option<&str>,
    book_rows: Option<&str>,
    resident_books: Option<&str>,
) -> bool {
    let on = flag.map(str::trim) == Some("1");
    let classic = !matches!(book_rows.map(str::trim), Some("1" | "2" | "3"));
    let resident = !matches!(resident_books.map(str::trim), Some("0"));
    on && classic && resident
}

/// Which CF slots are deferred: [`DEFERRED_CFS`], plus the book CF if
/// `defer_books`.
fn deferred_slot_table(defer_books: bool) -> [bool; NUM_CFS] {
    std::array::from_fn(|i| {
        let name = crate::cf::ALL_CF_NAMES[i];
        DEFERRED_CFS.contains(&name) || (defer_books && name == crate::cf::CF_NATIVE_ORDER_BOOKS)
    })
}

/// Whether CF slot `i` (the overlay's CF index) is in L's deferred part:
/// one of [`DEFERRED_CFS`], or the book CF with `TORUS_CKPT_DEFER_BOOKS=1`
/// ([`parse_defer_books`]; the env is read once per process).
#[inline]
pub(crate) fn is_deferred_slot(i: usize) -> bool {
    static SLOTS: OnceLock<[bool; NUM_CFS]> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok();
        let flag = var("TORUS_CKPT_DEFER_BOOKS");
        let defer_books = parse_defer_books(
            flag.as_deref(),
            var("TORUS_BOOK_ROWS").as_deref(),
            var("TORUS_RESIDENT_BOOKS").as_deref(),
        );
        if flag.is_some() {
            tracing::info!(
                defer_books,
                "TORUS_CKPT_DEFER_BOOKS: book CF in the deferred part of L (on only for 1, Classic books, resident books)"
            );
        }
        deferred_slot_table(defer_books)
    })[i]
}

/// Which CFs of a layer an operation covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerPart {
    /// Every CF.
    All,
    /// Every CF except [`DEFERRED_CFS`] (and the book CF with
    /// `TORUS_CKPT_DEFER_BOOKS=1`).
    Main,
    /// Only [`DEFERRED_CFS`], plus the book CF with
    /// `TORUS_CKPT_DEFER_BOOKS=1`.
    Deferred,
}

impl LayerPart {
    #[inline]
    fn holds(self, slot: usize) -> bool {
        match self {
            Self::All => true,
            Self::Main => !is_deferred_slot(slot),
            Self::Deferred => is_deferred_slot(slot),
        }
    }
}

/// Fold `src` (newer) into `dst`, keeping `bytes` (key + value per write,
/// key per tombstone) exact. Returns `src`'s bytes in that measure (s114:
/// counted in this loop, so no extra walk).
fn merge_cf(dst: &mut CfPending, bytes: &mut usize, src: &CfPending) -> usize {
    let mut seen = 0usize;
    for (k, v) in &src.writes {
        seen += k.len() + v.len();
        if dst.deletes.remove(k) {
            *bytes -= k.len();
        }
        match dst.writes.get_mut(k) {
            Some(old) => {
                *bytes = *bytes - old.len() + v.len();
                old.clone_from(v);
            }
            None => {
                *bytes += k.len() + v.len();
                dst.writes.insert(k.clone(), v.clone());
            }
        }
    }
    for k in &src.deletes {
        seen += k.len();
        if let Some(old) = dst.writes.remove(k) {
            *bytes -= k.len() + old.len();
        }
        if dst.deletes.insert(k.clone()) {
            *bytes += k.len();
        }
    }
    seen
}

/// The since-checkpoint layer: last value or tombstone per `(CF, key)` over
/// the merged blocks, plus a running byte count (keys + values, a tombstone
/// counts its key) for the checkpoint byte cap.
pub struct CheckpointLayer {
    state: PendingState,
    /// Bytes per CF slot (the byte cap reads the sum; a split moves its
    /// CFs' counts with them).
    bytes: [usize; NUM_CFS],
    /// Height of the last merged block (`None` while empty).
    top: Option<u64>,
    /// Item 6 Phase 3 (s114): this layer's share of the exec context's
    /// retention gauges, once attached ([`Self::track`]).
    tracked: Option<crate::retention::Tracked>,
}

impl Default for CheckpointLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckpointLayer {
    pub fn new() -> Self {
        Self {
            state: PendingState::new(),
            bytes: [0; NUM_CFS],
            top: None,
            tracked: None,
        }
    }

    /// Item 6 Phase 3 (s114): count this layer against `stats` until it is
    /// freed (a no-op if already tracked); its bytes follow [`Self::bytes`].
    /// A [`Self::duplicate`] of a tracked layer is tracked.
    pub fn track(&mut self, stats: &std::sync::Arc<crate::RetentionStats>) {
        if self.tracked.is_none() {
            self.tracked = Some(crate::retention::Tracked::new(stats, self.bytes(), None));
        }
    }

    /// Fold `frozen` (the next block) on top: its writes replace earlier
    /// writes and tombstones, its tombstones replace earlier writes.
    /// Records `frozen`'s bytes in its tracked share (s114: measured in this
    /// loop, so no extra walk).
    pub fn merge(&mut self, frozen: &FrozenPending) {
        self.merge_part(frozen, LayerPart::All);
    }

    /// [`Self::merge`] restricted to `part`'s CFs (p3 merge wait: the
    /// end-of-block merge folds [`LayerPart::Main`] into L, hands it back,
    /// then folds [`LayerPart::Deferred`] into the part split off it).
    pub fn merge_part(&mut self, frozen: &FrozenPending, part: LayerPart) {
        let src = frozen.pending_state();
        let mut seen = 0usize;
        for (i, ((dst, bytes), cfp)) in self
            .state
            .cfs
            .iter_mut()
            .zip(self.bytes.iter_mut())
            .zip(src.cfs.iter())
            .enumerate()
        {
            if part.holds(i) {
                seen += merge_cf(dst, bytes, cfp);
            }
        }
        self.top = Some(self.top.map_or(frozen.height(), |t| t.max(frozen.height())));
        frozen.note_bytes(part, seen);
        if let Some(t) = &self.tracked {
            t.set_bytes(self.bytes());
        }
    }

    /// p3 merge wait: move the [`DEFERRED_CFS`] (plus the book CF with
    /// `TORUS_CKPT_DEFER_BOOKS=1`) out into a layer of their own (same
    /// top), leaving this one without them. O(number of CFs): the maps
    /// move, nothing is copied. [`Self::absorb`] puts them back.
    pub fn split_deferred(&mut self) -> CheckpointLayer {
        let mut out = CheckpointLayer::new();
        for i in (0..NUM_CFS).filter(|i| is_deferred_slot(*i)) {
            std::mem::swap(&mut self.state.cfs[i], &mut out.state.cfs[i]);
            std::mem::swap(&mut self.bytes[i], &mut out.bytes[i]);
        }
        out.top = self.top;
        // s114 retention: the part is a live layer of its own until
        // [`Self::absorb`] takes it back; the bytes move with it.
        if let Some(t) = &self.tracked {
            t.set_bytes(self.bytes());
            out.track(t.stats());
        }
        out
    }

    /// Fold `newer` on top of this layer, consuming it. A CF only `newer`
    /// holds moves over without copying (the [`Self::split_deferred`] part
    /// coming back); a CF both hold merges entry by entry, `newer` winning.
    pub fn absorb(&mut self, mut newer: CheckpointLayer) {
        for i in 0..NUM_CFS {
            let src = &mut newer.state.cfs[i];
            if src.is_empty() {
                continue;
            }
            if self.state.cfs[i].is_empty() {
                std::mem::swap(&mut self.state.cfs[i], src);
                self.bytes[i] = newer.bytes[i];
            } else {
                merge_cf(&mut self.state.cfs[i], &mut self.bytes[i], src);
            }
        }
        self.top = match (self.top, newer.top) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if let Some(t) = &self.tracked {
            t.set_bytes(self.bytes());
        }
    }

    /// A deep copy (the fallback when another handle on a shared layer is
    /// still alive where the exec thread takes it back to merge into it).
    pub fn duplicate(&self) -> Self {
        let mut state = PendingState::new();
        for (dst, src) in state.cfs.iter_mut().zip(self.state.cfs.iter()) {
            dst.writes.clone_from(&src.writes);
            dst.deletes.clone_from(&src.deletes);
        }
        Self {
            state,
            bytes: self.bytes,
            top: self.top,
            tracked: self
                .tracked
                .as_ref()
                .map(|t| crate::retention::Tracked::new(t.stats(), self.bytes(), None)),
        }
    }

    /// Bytes held (key + value per write, key per tombstone).
    pub fn bytes(&self) -> usize {
        self.bytes.iter().sum()
    }

    /// Height of the last merged block.
    pub fn top_height(&self) -> Option<u64> {
        self.top
    }

    /// Writes + tombstones held.
    pub fn entry_count(&self) -> usize {
        self.state
            .cfs
            .iter()
            .map(|c| c.writes.len() + c.deletes.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.entry_count() == 0
    }

    /// The layer as a pending set (crate-internal: overlay reads, the
    /// checkpoint flush).
    pub(crate) fn pending_state(&self) -> &PendingState {
        &self.state
    }

    /// Append every write and tombstone to `batch` (CF order, key order).
    pub fn append_to_batch(&self, db: &StateDb, batch: &mut WriteBatch) -> Result<(), StateError> {
        self.state.append_to_batch(db.inner(), batch)
    }

    /// Item 6 Phase 3 (`Job::Checkpoint` on the flush worker): write the
    /// layer as ONE atomic batch through the block flush
    /// (`flush_pending_after_batch`): the layer's rows, then `side` (rows
    /// the worker kept: action status), trie maintenance or the stale mark
    /// over the layer's native entries, the applied marker = `height`, and
    /// the window's kept running-hash rows. Same error contract as a block
    /// flush (Err after a landed write = trie maintenance failed).
    pub fn flush_checkpoint(
        &self,
        side: Option<&CheckpointLayer>,
        rows: &crate::running_hash::ChainRows,
        target: &StateDb,
        height: u64,
        trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
        member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    ) -> Result<crate::NativeFlushStats, StateError> {
        crate::backend::flush_checkpoint_batch(
            &self.state,
            side.map(|s| &s.state),
            rows,
            target,
            height,
            trie_cache,
            member_cache,
        )
    }
}

/// p3 merge wait: the [`LayerPart::Deferred`] part of L while the merge
/// thread may still be folding the previous block into it. Readers that
/// need it ([`Self::read`]) block until it arrives; every other read of L
/// goes on without it. Once it arrived (or the sender died) the answer is
/// fixed, and every later call returns at once.
pub struct DeferredLayer {
    cell: OnceLock<Option<CheckpointLayer>>,
    /// The merge thread's channel until a caller settles the part. A
    /// caller blocked in `recv` holds it outside the mutex (`None` here
    /// while `cell` is unset); the others wait on `settled`.
    rx: Mutex<Option<Receiver<CheckpointLayer>>>,
    settled: Condvar,
    /// Reads that found the part still being merged and blocked (each
    /// blocked caller once, also one waiting for another's `recv`).
    read_waits: AtomicU64,
    /// Nanoseconds those reads blocked.
    read_wait_ns: AtomicU64,
    /// Called once per read before it blocks (a test seam; `None` in
    /// production).
    on_block: Option<Box<dyn Fn() + Send + Sync>>,
}

impl DeferredLayer {
    /// A part that is already merged.
    pub fn ready(layer: CheckpointLayer) -> Self {
        Self {
            cell: OnceLock::from(Some(layer)),
            rx: Mutex::new(None),
            settled: Condvar::new(),
            read_waits: AtomicU64::new(0),
            read_wait_ns: AtomicU64::new(0),
            on_block: None,
        }
    }

    /// A part the merge thread sends on `rx` once merged. A sender dropped
    /// without a value (the merge panicked) makes it `None` for good.
    pub fn pending(rx: Receiver<CheckpointLayer>) -> Self {
        Self {
            cell: OnceLock::new(),
            rx: Mutex::new(Some(rx)),
            settled: Condvar::new(),
            read_waits: AtomicU64::new(0),
            read_wait_ns: AtomicU64::new(0),
            on_block: None,
        }
    }

    /// The merged part for a read (blocks while it is being merged; such a
    /// read is counted in [`Self::read_waits`]). `None`: the merge failed.
    pub fn read(&self) -> Option<&CheckpointLayer> {
        self.resolve(true)
    }

    /// The merged part for the exec thread's end-of-block join (not counted
    /// as a read wait). `None`: the merge failed.
    pub fn join(&self) -> Option<&CheckpointLayer> {
        self.resolve(false)
    }

    /// Whether the part has arrived (or failed) without blocking.
    pub fn is_settled(&self) -> bool {
        self.cell.get().is_some()
    }

    /// Call `f` before each read blocks on this part (a test seam: a test
    /// holding the merge releases it here instead of deadlocking).
    pub fn on_block(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_block = Some(Box::new(f));
        self
    }

    /// `(reads that blocked, total time they blocked)` since the last call
    /// (the counters restart at 0).
    pub fn take_read_waits(&self) -> (u64, std::time::Duration) {
        (
            self.read_waits.swap(0, Ordering::Relaxed),
            std::time::Duration::from_nanos(self.read_wait_ns.swap(0, Ordering::Relaxed)),
        )
    }

    /// The part itself (joins first). `None`: the merge failed.
    pub fn into_layer(self) -> Option<CheckpointLayer> {
        self.join();
        self.cell.into_inner().flatten()
    }

    fn lock_rx(&self) -> MutexGuard<'_, Option<Receiver<CheckpointLayer>>> {
        self.rx.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Settle the part, blocking while it is being merged. Every caller
    /// that blocks (the one receiving it and each one waiting for that
    /// receiver) counts once if `count`. The decision is made under `rx`'s
    /// lock: the receiver is only taken after `try_recv` found nothing, so
    /// a caller finding it taken knows the part has not been published.
    fn resolve(&self, count: bool) -> Option<&CheckpointLayer> {
        if let Some(done) = self.cell.get() {
            return done.as_ref();
        }
        let timer = std::time::Instant::now();
        let mut blocked = false;
        let mut g = self.lock_rx();
        while self.cell.get().is_none() {
            let arrived = match g.as_ref().map(Receiver::try_recv) {
                Some(Ok(layer)) => Some(Some(layer)),
                Some(Err(TryRecvError::Disconnected)) => Some(None),
                Some(Err(TryRecvError::Empty)) | None => None,
            };
            if let Some(got) = arrived {
                *g = None;
                let _ = self.cell.set(got);
                self.settled.notify_all();
                break;
            }
            // Still being merged: this caller blocks.
            if !blocked {
                blocked = true;
                if let (true, Some(f)) = (count, &self.on_block) {
                    // Without the lock (and with the receiver left in
                    // place), then look again.
                    drop(g);
                    f();
                    g = self.lock_rx();
                    continue;
                }
            }
            match g.take() {
                // Receive it here, without the lock; other callers wait
                // on `settled`.
                Some(rx) => {
                    drop(g);
                    let got = rx.recv().ok();
                    g = self.lock_rx();
                    let _ = self.cell.set(got);
                    self.settled.notify_all();
                }
                // Another caller is receiving it.
                None => g = self.settled.wait(g).unwrap_or_else(PoisonError::into_inner),
            }
        }
        drop(g);
        if blocked && count {
            self.read_waits.fetch_add(1, Ordering::Relaxed);
            self.read_wait_ns
                .fetch_add(timer.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        self.cell.get().and_then(Option::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::StateBackend;
    use crate::NativeStateOverlay;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const CFS: [&str; 8] = [
        crate::cf::CF_ACCOUNTS,
        crate::cf::CF_NATIVE_ORDERS,
        crate::cf::CF_NATIVE_ORDER_BOOKS,
        crate::cf::CF_NATIVE_MARKETS,
        crate::cf::CF_STAKING_VALIDATORS,
        crate::cf::CF_NATIVE_ORACLE,
        crate::cf::CF_SESSIONS,
        crate::cf::CF_CONSENSUS_META,
    ];

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (db, dir)
    }

    /// xorshift64*: deterministic, no dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn bytes(&mut self, max: u64) -> Vec<u8> {
            (0..self.below(max + 1))
                .map(|_| self.next() as u8)
                .collect()
        }
    }

    fn key(i: u64) -> Vec<u8> {
        // Two-byte prefix groups so prefix scans have something to cut.
        vec![b'k', (i / 8) as u8, (i % 8) as u8]
    }

    type Model = BTreeMap<(usize, Vec<u8>), Vec<u8>>;

    fn frozen_block(
        db: &StateDb,
        height: u64,
        rng: &mut Rng,
        model: &mut Model,
    ) -> Arc<FrozenPending> {
        let o = NativeStateOverlay::new(db.clone());
        for _ in 0..rng.below(24) {
            let c = rng.below(CFS.len() as u64) as usize;
            let k = key(rng.below(40));
            if rng.below(3) == 0 {
                o.delete_cf_raw(CFS[c], &k).unwrap();
                model.remove(&(c, k));
            } else {
                let v = rng.bytes(12);
                o.put_cf_raw(CFS[c], &k, &v).unwrap();
                model.insert((c, k), v);
            }
        }
        o.freeze(height)
    }

    fn seed_db(db: &StateDb, rng: &mut Rng, model: &mut Model) {
        for (c, cf) in CFS.iter().enumerate() {
            for i in 0..40 {
                if rng.below(2) == 0 {
                    let v = rng.bytes(12);
                    db.put_cf_raw(cf, &key(i), &v).unwrap();
                    model.insert((c, key(i)), v);
                }
            }
        }
    }

    fn assert_reads_match(db: &StateDb, layer: &Arc<CheckpointLayer>, model: &Model, what: &str) {
        let o = NativeStateOverlay::with_layers(db.clone(), None, Some(layer.clone()), None);
        for (c, cf) in CFS.iter().enumerate() {
            for i in 0..40 {
                let k = key(i);
                assert_eq!(
                    o.get_cf_raw(cf, &k).unwrap(),
                    model.get(&(c, k.clone())).cloned(),
                    "{what}: get {cf} {k:?}"
                );
            }
            let want: Vec<(Vec<u8>, Vec<u8>)> = model
                .range((c, Vec::new())..)
                .take_while(|((mc, _), _)| *mc == c)
                .map(|((_, k), v)| (k.clone(), v.clone()))
                .collect();
            assert_eq!(
                StateBackend::iterate_cf(&o, cf, None).unwrap(),
                want,
                "{what}: iterate {cf}"
            );
            let pfx = [b'k', 2u8];
            let want_p: Vec<_> = want
                .iter()
                .filter(|(k, _)| k.starts_with(&pfx))
                .cloned()
                .collect();
            assert_eq!(
                StateBackend::iterate_cf(&o, cf, Some(&pfx)).unwrap(),
                want_p,
                "{what}: prefix {cf}"
            );
            assert_eq!(
                StateBackend::prefix_exists(&o, cf, &pfx).unwrap(),
                !want_p.is_empty(),
                "{what}: prefix_exists {cf}"
            );
            let from = key(13);
            let want_f: Vec<_> = want
                .iter()
                .filter(|(k, _)| *k >= from)
                .take(5)
                .cloned()
                .collect();
            assert_eq!(
                StateBackend::iterate_cf_from(&o, cf, &from, 5).unwrap(),
                want_f,
                "{what}: iterate_from {cf}"
            );
        }
    }

    /// 500 random blocks of writes and tombstones over 8 CFs, merged into L
    /// one by one: reading through L (point, prefix, from-scans) equals the
    /// block-by-block `BTreeMap` model, and `append_to_batch` written to the
    /// DB leaves exactly the model.
    #[test]
    fn layer_merge_matches_sequential_apply_random() {
        let (db, _dir) = temp_db();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut model = Model::new();
        seed_db(&db, &mut rng, &mut model);
        let mut layer = CheckpointLayer::new();
        for h in 1..=500u64 {
            let frozen = frozen_block(&db, h, &mut rng, &mut model);
            layer.merge(&frozen);
            assert_eq!(layer.top_height(), Some(h));
            if h % 50 == 0 {
                let l = Arc::new(std::mem::take(&mut layer));
                assert_reads_match(&db, &l, &model, &format!("h{h}"));
                layer = Arc::try_unwrap(l).ok().expect("sole owner");
            }
        }
        let mut batch = WriteBatch::default();
        layer.append_to_batch(&db, &mut batch).unwrap();
        db.write(batch).unwrap();
        for (c, cf) in CFS.iter().enumerate() {
            let got = StateBackend::iterate_cf(&db, cf, None).unwrap();
            let want: Vec<(Vec<u8>, Vec<u8>)> = model
                .range((c, Vec::new())..)
                .take_while(|((mc, _), _)| *mc == c)
                .map(|((_, k), v)| (k.clone(), v.clone()))
                .collect();
            assert_eq!(got, want, "DB after the layer batch: {cf}");
        }
    }

    /// A tombstone in L hides the DB value on every read path, and the read
    /// order is own -> parent -> L -> C -> DB.
    #[test]
    fn layer_tombstone_hides_db_value() {
        let (db, _dir) = temp_db();
        let cf = crate::cf::CF_NATIVE_ORDERS;
        db.put_cf_raw(cf, b"a1", b"db").unwrap();
        db.put_cf_raw(cf, b"a2", b"db").unwrap();
        db.put_cf_raw(cf, b"a3", b"db").unwrap();

        // C (in flight): writes a2 = "c", a4 = "c".
        let oc = NativeStateOverlay::new(db.clone());
        oc.put_cf_raw(cf, b"a2", b"c").unwrap();
        oc.put_cf_raw(cf, b"a4", b"c").unwrap();
        let mut c = CheckpointLayer::new();
        c.merge(&oc.freeze(1));
        // L: deletes a1 and a2 (over C and the DB), writes a3 = "l".
        let ol = NativeStateOverlay::new(db.clone());
        ol.delete_cf_raw(cf, b"a1").unwrap();
        ol.delete_cf_raw(cf, b"a2").unwrap();
        ol.put_cf_raw(cf, b"a3", b"l").unwrap();
        let mut l = CheckpointLayer::new();
        l.merge(&ol.freeze(2));
        assert_eq!(l.entry_count(), 3);

        let o =
            NativeStateOverlay::with_layers(db.clone(), None, Some(Arc::new(l)), Some(Arc::new(c)));
        assert_eq!(
            o.get_cf_raw(cf, b"a1").unwrap(),
            None,
            "L tombstone hides DB"
        );
        assert_eq!(
            o.get_cf_raw(cf, b"a2").unwrap(),
            None,
            "L tombstone hides C"
        );
        assert_eq!(
            o.get_cf_raw(cf, b"a3").unwrap(),
            Some(b"l".to_vec()),
            "L over DB"
        );
        assert_eq!(
            o.get_cf_raw(cf, b"a4").unwrap(),
            Some(b"c".to_vec()),
            "C over DB"
        );
        assert_eq!(o.get_cf_len(cf, b"a1").unwrap(), None);
        let want = vec![
            (b"a3".to_vec(), b"l".to_vec()),
            (b"a4".to_vec(), b"c".to_vec()),
        ];
        assert_eq!(StateBackend::iterate_cf(&o, cf, None).unwrap(), want);
        assert_eq!(StateBackend::iterate_cf(&o, cf, Some(b"a")).unwrap(), want);
        assert_eq!(
            StateBackend::iterate_cf_from(&o, cf, b"a", 10).unwrap(),
            want
        );
        assert_eq!(
            StateBackend::iterate_cf_prefix_from(&o, cf, b"a", b"a", 10).unwrap(),
            want
        );
        assert!(!StateBackend::prefix_exists(&o, cf, b"a1").unwrap());
        assert!(!StateBackend::prefix_exists(&o, cf, b"a2").unwrap());
        assert!(StateBackend::prefix_exists(&o, cf, b"a4").unwrap());

        // A parent write sits over L's tombstone; own pending over the parent.
        let op = NativeStateOverlay::new(db.clone());
        op.put_cf_raw(cf, b"a1", b"p").unwrap();
        op.put_cf_raw(cf, b"a3", b"p").unwrap();
        let parent = op.freeze(3);
        let o2 = NativeStateOverlay::with_layers(
            db.clone(),
            Some(parent),
            o.layer_for_tests(),
            o.inflight_for_tests(),
        );
        o2.put_cf_raw(cf, b"a3", b"own").unwrap();
        assert_eq!(o2.get_cf_raw(cf, b"a1").unwrap(), Some(b"p".to_vec()));
        assert_eq!(o2.get_cf_raw(cf, b"a3").unwrap(), Some(b"own".to_vec()));
        assert_eq!(
            StateBackend::iterate_cf(&o2, cf, None).unwrap(),
            vec![
                (b"a1".to_vec(), b"p".to_vec()),
                (b"a3".to_vec(), b"own".to_vec()),
                (b"a4".to_vec(), b"c".to_vec()),
            ]
        );
    }

    /// `bytes()` = key + value per write, key per tombstone; overwrites and
    /// write <-> tombstone flips adjust it exactly.
    #[test]
    fn layer_bytes_tracks_inserts_and_overwrites() {
        let (db, _dir) = temp_db();
        let cf = crate::cf::CF_NATIVE_ORDERS;
        let block = |f: &dyn Fn(&NativeStateOverlay), h: u64| {
            let o = NativeStateOverlay::new(db.clone());
            f(&o);
            o.freeze(h)
        };
        let mut l = CheckpointLayer::new();
        assert_eq!(l.bytes(), 0);
        l.merge(&block(&|o| o.put_cf_raw(cf, b"abc", b"12345").unwrap(), 1));
        assert_eq!(l.bytes(), 3 + 5);
        l.merge(&block(&|o| o.put_cf_raw(cf, b"abc", b"12").unwrap(), 2));
        assert_eq!(l.bytes(), 3 + 2, "overwrite replaces the value bytes");
        l.merge(&block(&|o| o.delete_cf_raw(cf, b"abc").unwrap(), 3));
        assert_eq!(l.bytes(), 3, "tombstone keeps the key bytes");
        l.merge(&block(&|o| o.delete_cf_raw(cf, b"abc").unwrap(), 4));
        assert_eq!(l.bytes(), 3, "second tombstone adds nothing");
        l.merge(&block(&|o| o.put_cf_raw(cf, b"abc", b"wxyz").unwrap(), 5));
        assert_eq!(l.bytes(), 3 + 4);
        l.merge(&block(
            &|o| {
                o.put_cf_raw(crate::cf::CF_SESSIONS, b"k", b"v").unwrap();
                o.delete_cf_raw(cf, b"zz").unwrap();
            },
            6,
        ));
        assert_eq!(l.bytes(), 3 + 4 + 2 + 2);
        assert_eq!(l.entry_count(), 3);
        assert_eq!(l.top_height(), Some(6));
    }

    /// `FrozenPending::state_bytes()` uses the layer's measure (an empty
    /// layer merged with the block holds exactly that), and merging a block
    /// never grows a layer by more than it: `L.bytes + block.state_bytes`
    /// bounds the merged layer (the checkpoint byte cap's estimate).
    #[test]
    fn frozen_state_bytes_bound_the_merge() {
        let (db, _dir) = temp_db();
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        let mut model = Model::new();
        let mut layer = CheckpointLayer::new();
        for h in 1..=300u64 {
            let frozen = frozen_block(&db, h, &mut rng, &mut model);
            let mut alone = CheckpointLayer::new();
            alone.merge(&frozen);
            assert_eq!(alone.bytes(), frozen.state_bytes(), "h{h}: same measure");
            let bound = layer.bytes() + frozen.state_bytes();
            layer.merge(&frozen);
            assert!(layer.bytes() <= bound, "h{h}: {} > {bound}", layer.bytes());
        }
    }

    // ---- TORUS_CKPT_DEFER_BOOKS: the routing table ----

    fn slot_of(cf: &str) -> usize {
        crate::cf::ALL_CF_NAMES
            .iter()
            .position(|n| *n == cf)
            .unwrap()
    }

    /// The gate is on only for a trimmed `1`, in Classic (`TORUS_BOOK_ROWS`
    /// not 1 / 2 / 3, as `parse_book_rows_mode`) with resident books
    /// (`TORUS_RESIDENT_BOOKS` not 0, as `parse_resident_books_toggle`).
    /// All unset is off (today's table).
    #[test]
    fn parse_defer_books_table() {
        type Case = (
            Option<&'static str>,
            Option<&'static str>,
            Option<&'static str>,
            bool,
        );
        let cases: &[Case] = &[
            (None, None, None, false),
            (Some("1"), None, None, true),
            (Some(" 1\n"), None, None, true),
            (Some("0"), None, None, false),
            (Some(""), None, None, false),
            (Some("true"), None, None, false),
            (Some("11"), None, None, false),
            // Book mode: only 1 / 2 / 3 leave Classic.
            (Some("1"), Some("0"), None, true),
            (Some("1"), Some(""), None, true),
            (Some("1"), Some("4"), None, true),
            (Some("1"), Some("1"), None, false),
            (Some("1"), Some(" 2 "), None, false),
            (Some("1"), Some("3"), None, false),
            // Resident books: only 0 turns them off.
            (Some("1"), None, Some("1"), true),
            (Some("1"), None, Some(""), true),
            (Some("1"), None, Some("no"), true),
            (Some("1"), None, Some("0"), false),
            (Some("1"), None, Some(" 0 "), false),
            (Some("1"), Some("2"), Some("0"), false),
            (None, Some("0"), Some("1"), false),
        ];
        for &(flag, rows, resident, want) in cases {
            assert_eq!(
                parse_defer_books(flag, rows, resident),
                want,
                "flag {flag:?}, TORUS_BOOK_ROWS {rows:?}, TORUS_RESIDENT_BOOKS {resident:?}"
            );
        }
    }

    /// Flag off: the deferred slots are exactly [`DEFERRED_CFS`] (positions
    /// and balances), today's table.
    #[test]
    fn deferred_slot_table_off_is_todays() {
        let t = deferred_slot_table(false);
        let mut want: Vec<usize> = DEFERRED_CFS.iter().map(|cf| slot_of(cf)).collect();
        want.sort_unstable();
        let got: Vec<usize> = (0..NUM_CFS).filter(|i| t[*i]).collect();
        assert_eq!(got, want);
        assert_eq!(got.len(), 2);
        assert!(!t[slot_of(crate::cf::CF_NATIVE_ORDER_BOOKS)]);
    }

    /// Flag on: today's table plus exactly the book slot.
    #[test]
    fn deferred_slot_table_on_adds_only_the_book_slot() {
        let off = deferred_slot_table(false);
        let on = deferred_slot_table(true);
        let books = slot_of(crate::cf::CF_NATIVE_ORDER_BOOKS);
        for i in 0..NUM_CFS {
            assert_eq!(
                on[i],
                off[i] || i == books,
                "slot {i} ({})",
                crate::cf::ALL_CF_NAMES[i]
            );
        }
        assert!(on[books]);
        assert_eq!(on.iter().filter(|d| **d).count(), 3);
    }

    // ---- p3 merge wait: L split into a main and a deferred part ----
    //
    // These tests assume TORUS_CKPT_DEFER_BOOKS unset (the routing table is
    // read from the env once per process): a shell exporting
    // TORUS_CKPT_DEFER_BOOKS=1 breaks `split_merge_equals_whole_merge_random`
    // and `deferred_part_blocks_only_deferred_reads` (books are their
    // non-deferred CF), as with the other env knobs; so does torus-consensus
    // `layer_merge_hands_l_back_before_the_slice` (a book row as the main
    // part). The flag-on behaviour is tested in its own process:
    // `tests/ckpt_defer_books.rs`.

    /// The CFs of the split tests: two deferred (R's) and four others.
    const SPLIT_CFS: [&str; 6] = [
        crate::cf::CF_NATIVE_POSITIONS,
        crate::cf::CF_NATIVE_BALANCES,
        crate::cf::CF_NATIVE_ORDER_BOOKS,
        crate::cf::CF_NATIVE_NONCES,
        crate::cf::CF_SESSIONS,
        crate::cf::CF_CONSENSUS_META,
    ];

    fn split_block(db: &StateDb, h: u64, rng: &mut Rng) -> Arc<FrozenPending> {
        let o = NativeStateOverlay::new(db.clone());
        for _ in 0..rng.below(24) {
            let cf = SPLIT_CFS[rng.below(SPLIT_CFS.len() as u64) as usize];
            let k = key(rng.below(40));
            if rng.below(3) == 0 {
                o.delete_cf_raw(cf, &k).unwrap();
            } else {
                o.put_cf_raw(cf, &k, &rng.bytes(12)).unwrap();
            }
        }
        o.freeze(h)
    }

    /// Every (CF, key, value-or-tombstone) of a layer, in order.
    type LayerRow = (usize, Vec<u8>, Option<Vec<u8>>);

    fn layer_rows(l: &CheckpointLayer) -> Vec<LayerRow> {
        let mut out = Vec::new();
        for (i, c) in l.pending_state().cfs.iter().enumerate() {
            out.extend(
                c.writes
                    .iter()
                    .map(|(k, v)| (i, k.clone(), Some(v.clone()))),
            );
            out.extend(c.deletes.iter().map(|k| (i, k.clone(), None)));
        }
        out
    }

    /// The end-of-block merge as the exec pipeline runs it (split, main
    /// part, deferred part, absorb back) leaves exactly the layer a whole
    /// merge does: same rows, bytes and top, over 400 random blocks. The
    /// split part holds only `DEFERRED_CFS`, the main part none of them.
    #[test]
    fn split_merge_equals_whole_merge_random() {
        let (db, _dir) = temp_db();
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mut whole = CheckpointLayer::new();
        let mut split = CheckpointLayer::new();
        let deferred_slots: Vec<usize> = DEFERRED_CFS
            .iter()
            .map(|cf| {
                crate::cf::ALL_CF_NAMES
                    .iter()
                    .position(|n| n == cf)
                    .unwrap()
            })
            .collect();
        for h in 1..=400u64 {
            let b = split_block(&db, h, &mut rng);
            whole.merge(&b);
            let mut part = split.split_deferred();
            for (i, c) in split.pending_state().cfs.iter().enumerate() {
                assert!(
                    !deferred_slots.contains(&i) || c.is_empty(),
                    "h{h}: main part holds a deferred CF"
                );
            }
            for (i, c) in part.pending_state().cfs.iter().enumerate() {
                assert!(
                    deferred_slots.contains(&i) || c.is_empty(),
                    "h{h}: deferred part holds another CF"
                );
            }
            split.merge_part(&b, LayerPart::Main);
            part.merge_part(&b, LayerPart::Deferred);
            split.absorb(part);
            assert_eq!(layer_rows(&split), layer_rows(&whole), "h{h}: rows");
            assert_eq!(split.bytes(), whole.bytes(), "h{h}: bytes");
            assert_eq!(split.top_height(), whole.top_height(), "h{h}: top");
        }
        assert!(!whole.is_empty());
    }

    /// `absorb` of a layer sharing CFs merges it on top (newer wins), the
    /// same as merging its blocks one by one.
    #[test]
    fn absorb_overlapping_layer_is_a_merge() {
        let (db, _dir) = temp_db();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let blocks: Vec<_> = (1..=60u64).map(|h| split_block(&db, h, &mut rng)).collect();
        let mut whole = CheckpointLayer::new();
        blocks.iter().for_each(|b| whole.merge(b));
        let (mut old, mut new) = (CheckpointLayer::new(), CheckpointLayer::new());
        blocks[..30].iter().for_each(|b| old.merge(b));
        blocks[30..].iter().for_each(|b| new.merge(b));
        old.absorb(new);
        assert_eq!(layer_rows(&old), layer_rows(&whole));
        assert_eq!(old.bytes(), whole.bytes());
        assert_eq!(old.top_height(), Some(60));
    }

    /// Split L: with the deferred part still being merged, reads of every
    /// other CF (point, length, scans, prefix checks) answer at once; a read
    /// of a deferred CF waits for the part, then answers as the whole layer
    /// would, and is counted. A part whose merge died is an error for its
    /// CFs only.
    #[test]
    fn deferred_part_blocks_only_deferred_reads() {
        use std::time::Duration;
        let (db, _dir) = temp_db();
        let pos = crate::cf::CF_NATIVE_POSITIONS;
        let books = crate::cf::CF_NATIVE_ORDER_BOOKS;
        db.put_cf_raw(pos, b"p1", b"db").unwrap();
        let o = NativeStateOverlay::new(db.clone());
        o.put_cf_raw(pos, b"p1", b"L").unwrap();
        o.put_cf_raw(books, b"b1", b"L").unwrap();
        let mut main = CheckpointLayer::new();
        let mut part = main.split_deferred();
        let block = o.freeze(5);
        main.merge_part(&block, LayerPart::Main);
        part.merge_part(&block, LayerPart::Deferred);

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let deferred = Arc::new(DeferredLayer::pending(rx));
        let ov = NativeStateOverlay::with_split_layers(
            db.clone(),
            None,
            Some(Arc::new(main)),
            Some(deferred.clone()),
            None,
        );
        let (done_tx, done) = std::sync::mpsc::channel();
        let reader = ov.clone();
        std::thread::spawn(move || {
            let r = (
                reader.get_cf_raw(books, b"b1").unwrap(),
                reader.get_cf_len(books, b"b1").unwrap(),
                StateBackend::iterate_cf(&reader, books, None).unwrap(),
                StateBackend::iterate_cf_from(&reader, books, b"", 5).unwrap(),
                StateBackend::prefix_exists(&reader, books, b"b").unwrap(),
            );
            let _ = done_tx.send(r);
        });
        let (got, len, all, from, exists) = done
            .recv_timeout(Duration::from_secs(5))
            .expect("non-deferred reads do not wait for the deferred part");
        assert_eq!(got, Some(b"L".to_vec()));
        assert_eq!(len, Some(1));
        assert_eq!(all, vec![(b"b1".to_vec(), b"L".to_vec())]);
        assert_eq!(from, all);
        assert!(exists);
        assert!(!deferred.is_settled());

        let (pos_tx, pos_rx) = std::sync::mpsc::channel();
        let reader = ov.clone();
        std::thread::spawn(move || {
            let _ = pos_tx.send(reader.get_cf_raw(pos, b"p1").unwrap());
        });
        assert!(
            pos_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a deferred read waits for the part"
        );
        tx.send(part).unwrap();
        assert_eq!(
            pos_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Some(b"L".to_vec()),
            "L over the DB once merged"
        );
        assert_eq!(deferred.take_read_waits().0, 1, "one read blocked");
        assert_eq!(deferred.take_read_waits().0, 0, "taken");
        assert_eq!(
            StateBackend::iterate_cf(&ov, pos, None).unwrap(),
            vec![(b"p1".to_vec(), b"L".to_vec())]
        );

        let (tx, rx) = std::sync::mpsc::sync_channel::<CheckpointLayer>(1);
        drop(tx);
        let lost = NativeStateOverlay::with_split_layers(
            db.clone(),
            None,
            Some(Arc::new(CheckpointLayer::new())),
            Some(Arc::new(DeferredLayer::pending(rx))),
            None,
        );
        assert!(lost.get_cf_raw(pos, b"p1").is_err(), "lost part: error");
        assert!(StateBackend::iterate_cf(&lost, pos, None).is_err());
        assert_eq!(lost.get_cf_raw(books, b"b1").unwrap(), None);
    }

    /// Codex P3 (merge wait r1): a point read of a deferred CF whose key the
    /// parent holds (a write or a tombstone) answers from the parent at once,
    /// without waiting for L's deferred part, and is not counted as a wait.
    /// A key the parent lacks still waits and then reads L. Answers are the
    /// ones the whole layer gives.
    #[test]
    fn parent_hit_does_not_wait_for_the_deferred_part() {
        use std::time::Duration;
        let (db, _dir) = temp_db();
        let pos = crate::cf::CF_NATIVE_POSITIONS;
        let bal = crate::cf::CF_NATIVE_BALANCES;
        db.put_cf_raw(pos, b"gone", b"db").unwrap();
        db.put_cf_raw(pos, b"lower", b"db").unwrap();
        // L (deferred part): every key written at height 5.
        let o = NativeStateOverlay::new(db.clone());
        for (cf, k) in [
            (pos, &b"p1"[..]),
            (pos, b"gone"),
            (pos, b"lower"),
            (bal, b"b1"),
        ] {
            o.put_cf_raw(cf, k, b"L").unwrap();
        }
        let mut main = CheckpointLayer::new();
        let mut part = main.split_deferred();
        let block = o.freeze(5);
        main.merge_part(&block, LayerPart::Main);
        part.merge_part(&block, LayerPart::Deferred);
        // Parent (height 6): writes p1 and b1, tombstones `gone`.
        let p = NativeStateOverlay::new(db.clone());
        p.put_cf_raw(pos, b"p1", b"parent").unwrap();
        p.put_cf_raw(bal, b"b1", b"parent").unwrap();
        p.delete_cf_raw(pos, b"gone").unwrap();
        let parent = p.freeze(6);

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let deferred = Arc::new(DeferredLayer::pending(rx));
        let ov = NativeStateOverlay::with_split_layers(
            db.clone(),
            Some(parent),
            Some(Arc::new(main)),
            Some(deferred.clone()),
            None,
        );
        let (done_tx, done) = std::sync::mpsc::channel();
        let reader = ov.clone();
        std::thread::spawn(move || {
            let r = (
                reader.get_cf_raw(pos, b"p1").unwrap(),
                reader.get_cf_raw(bal, b"b1").unwrap(),
                reader.get_cf_raw(pos, b"gone").unwrap(),
                reader.get_cf_len(pos, b"p1").unwrap(),
                reader.get_cf_len(pos, b"gone").unwrap(),
            );
            let _ = done_tx.send(r);
        });
        let got = done.recv_timeout(Duration::from_secs(2));
        // Unblock a reader stuck on the part before asserting (no leak).
        let _ = tx.send(part);
        let (p1, b1, gone, p1_len, gone_len) =
            got.expect("a parent hit does not wait for the deferred part");
        assert_eq!(p1, Some(b"parent".to_vec()));
        assert_eq!(b1, Some(b"parent".to_vec()));
        assert_eq!(gone, None, "the parent's tombstone wins over L and the DB");
        assert_eq!(p1_len, Some(6));
        assert_eq!(gone_len, None);
        assert_eq!(deferred.take_read_waits().0, 0, "parent hits are not waits");
        assert_eq!(
            ov.get_cf_raw(pos, b"lower").unwrap(),
            Some(b"L".to_vec()),
            "a parent miss reads L"
        );
    }

    /// Codex P3 (merge wait r1): every read that blocks on the deferred
    /// part is counted, including one waiting while another thread receives
    /// it (two blocked readers record 2). Reads after it arrived, and the
    /// exec join, are not counted.
    #[test]
    fn concurrent_blocked_reads_are_each_counted() {
        use std::sync::atomic::AtomicUsize;
        use std::time::{Duration, Instant};
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let blocked = Arc::new(AtomicUsize::new(0));
        let seen = blocked.clone();
        let d = Arc::new(DeferredLayer::pending(rx).on_block(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let d = d.clone();
                std::thread::spawn(move || d.read().map(|l| l.top_height()))
            })
            .collect();
        // Both readers announce they block (bounded: a reader that never
        // announces it fails the count below rather than hanging).
        let deadline = Instant::now() + Duration::from_secs(2);
        while blocked.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut part = CheckpointLayer::new();
        part.top = Some(9);
        tx.send(part).unwrap();
        for r in readers {
            assert_eq!(r.join().unwrap(), Some(Some(9)));
        }
        assert_eq!(
            blocked.load(Ordering::SeqCst),
            2,
            "each blocked read announced"
        );
        let (waits, time) = d.take_read_waits();
        assert_eq!(waits, 2, "two blocked reads, two waits");
        assert!(time > Duration::ZERO);
        assert!(d.read().is_some() && d.join().is_some());
        assert_eq!(d.take_read_waits().0, 0, "settled: no more waits");

        // Arrived before the first read: no wait.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(CheckpointLayer::new()).unwrap();
        let d = DeferredLayer::pending(rx);
        assert!(d.read().is_some());
        assert_eq!(d.take_read_waits().0, 0, "already merged: no wait");
    }
}
