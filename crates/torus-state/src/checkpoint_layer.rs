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

use rocksdb::WriteBatch;

use crate::backend::{FrozenPending, PendingState};
use crate::{StateDb, StateError};

/// The since-checkpoint layer: last value or tombstone per `(CF, key)` over
/// the merged blocks, plus a running byte count (keys + values, a tombstone
/// counts its key) for the checkpoint byte cap.
pub struct CheckpointLayer {
    state: PendingState,
    bytes: usize,
    /// Height of the last merged block (`None` while empty).
    top: Option<u64>,
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
            bytes: 0,
            top: None,
        }
    }

    /// Fold `frozen` (the next block) on top: its writes replace earlier
    /// writes and tombstones, its tombstones replace earlier writes.
    pub fn merge(&mut self, _frozen: &FrozenPending) {}

    /// Bytes held (key + value per write, key per tombstone).
    pub fn bytes(&self) -> usize {
        self.bytes
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
            (0..self.below(max + 1)).map(|_| self.next() as u8).collect()
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
        for c in 0..CFS.len() {
            for i in 0..40 {
                if rng.below(2) == 0 {
                    let v = rng.bytes(12);
                    db.put_cf_raw(CFS[c], &key(i), &v).unwrap();
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
            assert_eq!(StateBackend::iterate_cf(&o, cf, None).unwrap(), want, "{what}: iterate {cf}");
            let pfx = [b'k', 2u8];
            let want_p: Vec<_> = want.iter().filter(|(k, _)| k.starts_with(&pfx)).cloned().collect();
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
            let want_f: Vec<_> = want.iter().filter(|(k, _)| *k >= from).take(5).cloned().collect();
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

        let o = NativeStateOverlay::with_layers(db.clone(), None, Some(Arc::new(l)), Some(Arc::new(c)));
        assert_eq!(o.get_cf_raw(cf, b"a1").unwrap(), None, "L tombstone hides DB");
        assert_eq!(o.get_cf_raw(cf, b"a2").unwrap(), None, "L tombstone hides C");
        assert_eq!(o.get_cf_raw(cf, b"a3").unwrap(), Some(b"l".to_vec()), "L over DB");
        assert_eq!(o.get_cf_raw(cf, b"a4").unwrap(), Some(b"c".to_vec()), "C over DB");
        assert_eq!(o.get_cf_len(cf, b"a1").unwrap(), None);
        let want = vec![(b"a3".to_vec(), b"l".to_vec()), (b"a4".to_vec(), b"c".to_vec())];
        assert_eq!(StateBackend::iterate_cf(&o, cf, None).unwrap(), want);
        assert_eq!(StateBackend::iterate_cf(&o, cf, Some(b"a")).unwrap(), want);
        assert_eq!(StateBackend::iterate_cf_from(&o, cf, b"a", 10).unwrap(), want);
        assert_eq!(StateBackend::iterate_cf_prefix_from(&o, cf, b"a", b"a", 10).unwrap(), want);
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
}
