//! Item 6 Phase 3 step 2 (s114): live counts of what Phase 3 retains, for
//! the `torus_exec_checkpoint_live_*` / `torus_exec_checkpoint_read_views_live`
//! gauges (archy's soaks: is RSS growth retention or allocator?).
//!
//! The exec thread attaches a [`RetentionStats`] to the objects its
//! checkpoint path keeps (frozen block sets, ingress slices, L, C, the
//! published read views); each holds a guard that adds itself on attach and
//! takes itself off when the object is freed, wherever that happens (the
//! exec thread, the merge thread, the dropper, a mempool / RPC reader).
//! No walk of the maps on the exec thread: a frozen set's bytes are
//! recorded when it is merged into L (off E, in the merge's own loop), an
//! ingress slice's when it is cut, a layer's from its running count.
//! Per-instance (one per exec context), not process-wide, so concurrent
//! tests do not see each other's objects. Node-local; never read by
//! execution.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// Live counts of the objects tracked against it.
#[derive(Debug, Default)]
pub struct RetentionStats {
    objects: AtomicI64,
    bytes: AtomicI64,
    views: AtomicI64,
    /// Heights of the live frozen sets / slices (multiset).
    heights: Mutex<BTreeMap<u64, u32>>,
}

impl RetentionStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Live tracked objects: frozen block sets, ingress slices, layers (L, C).
    pub fn live_objects(&self) -> i64 {
        self.objects.load(Relaxed)
    }

    /// Their bytes in the checkpoint layer's measure (key + value per write,
    /// key per tombstone). A frozen set counts 0 until merged into L.
    pub fn live_bytes(&self) -> i64 {
        self.bytes.load(Relaxed)
    }

    /// Live tracked [`crate::ReadView`] handles (every clone counts).
    pub fn read_views(&self) -> i64 {
        self.views.load(Relaxed)
    }

    /// Height of the oldest live tracked frozen set or slice.
    pub fn oldest_height(&self) -> Option<u64> {
        self.heights.lock().unwrap().keys().next().copied()
    }
}

/// One tracked object's share of a [`RetentionStats`] (taken off on drop).
pub(crate) struct Tracked {
    stats: Arc<RetentionStats>,
    bytes: AtomicUsize,
    height: Option<u64>,
}

impl Tracked {
    pub(crate) fn new(stats: &Arc<RetentionStats>, bytes: usize, height: Option<u64>) -> Self {
        stats.objects.fetch_add(1, Relaxed);
        stats.bytes.fetch_add(bytes as i64, Relaxed);
        if let Some(h) = height {
            *stats.heights.lock().unwrap().entry(h).or_insert(0) += 1;
        }
        Self {
            stats: stats.clone(),
            bytes: AtomicUsize::new(bytes),
            height,
        }
    }

    /// The object now holds `bytes`.
    pub(crate) fn set_bytes(&self, bytes: usize) {
        let old = self.bytes.swap(bytes, Relaxed);
        self.stats
            .bytes
            .fetch_add(bytes as i64 - old as i64, Relaxed);
    }

    pub(crate) fn stats(&self) -> &Arc<RetentionStats> {
        &self.stats
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.stats.objects.fetch_sub(1, Relaxed);
        self.stats
            .bytes
            .fetch_sub(self.bytes.load(Relaxed) as i64, Relaxed);
        if let Some(h) = self.height {
            let mut heights = self.stats.heights.lock().unwrap();
            if let Some(n) = heights.get_mut(&h) {
                *n -= 1;
                if *n == 0 {
                    heights.remove(&h);
                }
            }
        }
    }
}

/// One live read-view handle: cloning the view clones the pin.
pub(crate) struct ViewPin(Arc<RetentionStats>);

impl ViewPin {
    pub(crate) fn new(stats: &Arc<RetentionStats>) -> Self {
        stats.views.fetch_add(1, Relaxed);
        Self(stats.clone())
    }
}

impl Clone for ViewPin {
    fn clone(&self) -> Self {
        Self::new(&self.0)
    }
}

impl Drop for ViewPin {
    fn drop(&mut self) {
        self.0.views.fetch_sub(1, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckpointLayer, NativeStateOverlay, ReadView, StateBackend, StateDb};

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (db, dir)
    }

    /// Frozen sets, slices and layers count while alive; bytes follow the
    /// layer's measure (a set's once merged, a slice's when cut, a layer's
    /// running count); everything returns to zero once freed.
    #[test]
    fn tracked_objects_count_live_and_bytes() {
        let (db, _dir) = temp_db();
        let stats = RetentionStats::new();
        let block = |h: u64| {
            let o = NativeStateOverlay::new(db.clone());
            o.put_cf_raw(crate::cf::CF_SESSIONS, &[h as u8; 4], b"session")
                .unwrap();
            o.put_cf_raw(crate::cf::CF_NATIVE_ORDERS, &[h as u8; 8], &[7u8; 100])
                .unwrap();
            o.freeze(h)
        };
        let (b1, b2) = (block(1), block(2));
        let untracked = block(3);
        b1.track(&stats);
        b2.track(&stats);
        b2.track(&stats);
        assert_eq!(stats.live_objects(), 2, "tracking twice counts once");
        assert_eq!(stats.live_bytes(), 0, "a set counts 0 until merged");
        assert_eq!(stats.oldest_height(), Some(1));

        let mut l = CheckpointLayer::new();
        l.track(&stats);
        assert_eq!(stats.live_objects(), 3);
        l.merge(&b1);
        l.merge(&untracked);
        assert_eq!(
            stats.live_bytes() as usize,
            l.bytes() + b1.state_bytes(),
            "L's running count + the merged set's bytes"
        );
        let slice = b2.slice(&crate::read_view::INGRESS_CFS);
        assert_eq!(
            stats.live_objects(),
            4,
            "a slice of a tracked set is tracked"
        );
        assert_eq!(
            stats.live_bytes() as usize,
            l.bytes() + b1.state_bytes() + slice.state_bytes()
        );
        assert!(slice.state_bytes() > 0 && slice.state_bytes() < b2.state_bytes());
        let copy = l.duplicate();
        assert_eq!(
            stats.live_objects(),
            5,
            "a copy of a tracked layer is tracked"
        );
        assert_eq!(
            stats.live_bytes() as usize,
            2 * l.bytes() + b1.state_bytes() + slice.state_bytes()
        );

        drop(b1);
        assert_eq!(stats.oldest_height(), Some(2));
        drop((l, copy));
        assert_eq!(stats.live_bytes() as usize, slice.state_bytes());
        drop(b2);
        assert_eq!(stats.oldest_height(), Some(2), "the slice is at height 2");
        drop(slice);
        assert_eq!(stats.live_objects(), 0);
        assert_eq!(stats.live_bytes(), 0);
        assert_eq!(stats.oldest_height(), None);
        drop(untracked);
        assert_eq!(stats.live_objects(), 0, "untracked sets never count");
    }

    /// Every handle on a tracked view counts (clones too) until dropped;
    /// an untracked view never counts.
    #[test]
    fn read_view_handles_pin_while_alive() {
        let (db, _dir) = temp_db();
        let stats = RetentionStats::new();
        let v = ReadView::new(db.clone(), 1, Vec::new(), None).with_stats(&stats);
        assert_eq!(stats.read_views(), 1);
        let held = v.clone();
        let held2 = held.clone();
        assert_eq!(stats.read_views(), 3);
        drop(held);
        assert_eq!(stats.read_views(), 2);
        drop((v, held2));
        assert_eq!(stats.read_views(), 0);
        let plain = ReadView::db_only(db.clone());
        let _c = plain.clone();
        assert_eq!(stats.read_views(), 0);
    }
}
