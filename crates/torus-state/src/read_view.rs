//! Item 6 Phase 3 step 2: the published read view for validator ingress
//! (plan `docs/plans/item6-phase3-impl.md` "Step 2").
//!
//! With a checkpoint interval set, executed native blocks stay in memory
//! until the next checkpoint: the durable DB lags execution by up to one
//! interval. The ingress readers (mempool gossip session lookup, the
//! funded / duty-exemption / oracle-reporter checks, the EVM balance at
//! `add_evm_tx`) read a [`ReadView`] instead: the frozen sets of the blocks
//! since the last checkpoint S (newest first), then the checkpoint in flight
//! C, then the DB. The exec thread publishes a new view after each block (one
//! `Arc` swap); readers clone the current one.
//!
//! Scope: the layers answer only the [`INGRESS_CFS`]; every other CF reads
//! the DB (as before Phase 3 step 2). The view never holds the
//! since-checkpoint layer L itself (E merges into L in place and needs it
//! unshared): it holds the per-block sets L is built from. To keep that
//! memory to the ingress rows, the exec thread swaps each block older than
//! the newest two for its [`FrozenPending::slice`] over [`INGRESS_CFS`]
//! (Classic book blobs, ~77% of the state bytes written, are not pinned).
//!
//! Freshness contract: a view published after block n answers every
//! ingress key as of height >= n (the DB only moves forward, and a layer
//! stays in the view until it is durable). Reads are not a snapshot: a key
//! the view's layers do not hold may come from a newer checkpoint. That is
//! fine for the node-local ingress checks it serves (block validity never
//! reads it).
//!
//! A view with no layers ([`ReadView::db_only`]) delegates every call to the
//! [`StateDb`] as it is: interval 0 behaves exactly as before.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use alloy_primitives::Address;
use revm::state::AccountInfo;

use crate::backend::{
    intern_cf, merge_from, prefix_read_options, writes_under, AtomicWriteOp, CfId, FrozenPending,
    PendingState, StateBackend,
};
use crate::cf::{CF_ACCOUNTS, CF_SESSIONS};
use crate::checkpoint_layer::CheckpointLayer;
use crate::db::decode_account_info;
use crate::{StateDb, StateError};

/// The CFs the ingress readers read, which the view's layers answer:
/// sessions (gossip session lookup), native balances (min collateral, the
/// item B volume row), EVM accounts (spot balance, `add_evm_tx` nonce and
/// balance, `pending_nonce`), positions (funded path 3), the validator table
/// (duty exemption, oracle reporter) and the oracle CF (signer index; the
/// aggregate mark of the RPC submit price band). The RPC submit checks also
/// read the market listings (unknown market, tick / lot) and the fee-config
/// CF (governance price band width): a listing or a band change executed in
/// a block not yet checkpointed must not be refused at a validator's RPC.
/// Both are small per block: the markets CF gets the 16-byte order-id
/// counter row when orders flowed (and a row per governance listing); the
/// fee-config CF only governance rows.
pub const INGRESS_CFS: [&str; 8] = [
    crate::cf::CF_SESSIONS,
    crate::cf::CF_NATIVE_BALANCES,
    crate::cf::CF_ACCOUNTS,
    crate::cf::CF_NATIVE_POSITIONS,
    crate::cf::CF_STAKING_VALIDATORS,
    crate::cf::CF_NATIVE_ORACLE,
    crate::cf::CF_NATIVE_MARKETS,
    crate::cf::CF_FEE_CONFIG,
];

fn ingress_ids() -> &'static [CfId] {
    static IDS: OnceLock<Vec<CfId>> = OnceLock::new();
    IDS.get_or_init(|| INGRESS_CFS.iter().filter_map(|cf| intern_cf(cf)).collect())
}

struct Layers {
    /// Executed height the view reflects (`None`: the DB alone).
    height: Option<u64>,
    /// Frozen sets (or their ingress slices) of the blocks above S, newest
    /// first.
    blocks: Vec<Arc<FrozenPending>>,
    /// C: the checkpoint handed to the flush worker and not known durable.
    inflight: Option<Arc<CheckpointLayer>>,
}

/// Read-only layered state for the [`INGRESS_CFS`]: frozen blocks (newest
/// first) -> C -> DB; other CFs read the DB. Cheap to clone (one `Arc`).
/// Every write returns an error.
#[derive(Clone)]
pub struct ReadView {
    db: StateDb,
    layers: Arc<Layers>,
}

impl std::fmt::Debug for ReadView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadView")
            .field("height", &self.layers.height)
            .field("blocks", &self.layers.blocks.len())
            .field(
                "inflight",
                &self.layers.inflight.as_ref().map(|c| c.top_height()),
            )
            .finish()
    }
}

fn read_only() -> StateError {
    StateError::InvalidData("read view is read-only".into())
}

impl ReadView {
    /// The DB alone: every call is the [`StateDb`] call.
    pub fn db_only(db: StateDb) -> Self {
        Self {
            db,
            layers: Arc::new(Layers {
                height: None,
                blocks: Vec::new(),
                inflight: None,
            }),
        }
    }

    /// The view after block `height`: `blocks` are the frozen sets (or
    /// ingress slices) of the blocks above the last checkpoint, newest first;
    /// `inflight` is the checkpoint handed to the flush worker (if not yet
    /// durable).
    pub fn new(
        db: StateDb,
        height: u64,
        blocks: Vec<Arc<FrozenPending>>,
        inflight: Option<Arc<CheckpointLayer>>,
    ) -> Self {
        debug_assert!(
            blocks.windows(2).all(|w| w[0].height() > w[1].height()),
            "read view blocks must be newest first"
        );
        Self {
            db,
            layers: Arc::new(Layers {
                height: Some(height),
                blocks,
                inflight,
            }),
        }
    }

    /// Executed height the view reflects (`None`: the DB alone).
    pub fn height(&self) -> Option<u64> {
        self.layers.height
    }

    /// Frozen block sets held, newest first.
    pub fn blocks(&self) -> &[Arc<FrozenPending>] {
        &self.layers.blocks
    }

    /// Frozen block sets held.
    pub fn block_count(&self) -> usize {
        self.layers.blocks.len()
    }

    /// Whether a checkpoint in flight is held.
    pub fn has_inflight(&self) -> bool {
        self.layers.inflight.is_some()
    }

    /// The DB under the layers.
    pub fn db(&self) -> &StateDb {
        &self.db
    }

    /// `Some(id)` when reads of `cf` go through the layers: an ingress CF and
    /// a view that holds any; `None` = the plain DB call.
    fn layered(&self, cf: &str) -> Option<CfId> {
        if self.layers.blocks.is_empty() && self.layers.inflight.is_none() {
            return None;
        }
        intern_cf(cf).filter(|id| ingress_ids().contains(id))
    }

    /// The layers, newest first.
    fn lower(&self) -> Vec<Option<&PendingState>> {
        self.layers
            .blocks
            .iter()
            .map(|b| Some(b.pending_state()))
            .chain(std::iter::once(
                self.layers.inflight.as_deref().map(|c| c.pending_state()),
            ))
            .collect()
    }
}

impl StateBackend for ReadView {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if let Some(id) = self.layered(cf) {
            for layer in self.lower().into_iter().flatten() {
                if let Some(hit) = layer.lookup(id, key) {
                    return Ok(hit.map(<[u8]>::to_vec));
                }
            }
        }
        StateBackend::get_cf_raw(&self.db, cf, key)
    }

    fn put_cf_raw(&self, _cf: &str, _key: &[u8], _value: &[u8]) -> Result<(), StateError> {
        Err(read_only())
    }

    fn delete_cf_raw(&self, _cf: &str, _key: &[u8]) -> Result<(), StateError> {
        Err(read_only())
    }

    fn atomic_write(&self, _ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        Err(read_only())
    }

    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let db_entries = StateBackend::iterate_cf(&self.db, cf, prefix)?;
        let Some(id) = self.layered(cf) else {
            return Ok(db_entries);
        };
        let p = prefix.unwrap_or(&[]);
        let lower = self.lower();
        if !lower.iter().flatten().any(|s| s.touches(id, p)) {
            return Ok(db_entries);
        }
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = db_entries.into_iter().collect();
        // Oldest layer first, each over what sits below.
        for layer in lower.iter().rev().flatten() {
            layer.overlay_into(id, prefix, &mut merged);
        }
        Ok(merged.into_iter().collect())
    }

    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let Some(id) = self.layered(cf) else {
            return StateBackend::iterate_cf_from(&self.db, cf, start, limit);
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let db = self.db.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let dbi = db
            .iterator_cf(
                cf_handle,
                rocksdb::IteratorMode::From(start, rocksdb::Direction::Forward),
            )
            .map(|item| item.map_err(StateError::from));
        merge_from(&PendingState::new(), &self.lower(), id, start, limit, dbi)
    }

    fn get_cf_len(&self, cf: &str, key: &[u8]) -> Result<Option<usize>, StateError> {
        if let Some(id) = self.layered(cf) {
            for layer in self.lower().into_iter().flatten() {
                if let Some(hit) = layer.lookup(id, key) {
                    return Ok(hit.map(<[u8]>::len));
                }
            }
        }
        StateBackend::get_cf_len(&self.db, cf, key)
    }

    fn iterate_cf_prefix_from(
        &self,
        cf: &str,
        prefix: &[u8],
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let Some(id) = self.layered(cf) else {
            return StateBackend::iterate_cf_prefix_from(&self.db, cf, prefix, start, limit);
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let db = self.db.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let dbi = db
            .iterator_cf_opt(
                cf_handle,
                prefix_read_options(prefix),
                rocksdb::IteratorMode::From(start, rocksdb::Direction::Forward),
            )
            .map(|item| item.map_err(StateError::from));
        merge_from(&PendingState::new(), &self.lower(), id, start, limit, dbi).map(|rows| {
            rows.into_iter()
                .take_while(|(k, _)| k.starts_with(prefix))
                .collect()
        })
    }

    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        let Some(id) = self.layered(cf) else {
            return StateBackend::prefix_exists(&self.db, cf, prefix);
        };
        let lower = self.lower();
        let live = |key: &[u8]| -> bool {
            match lower.iter().flatten().find_map(|s| s.lookup(id, key)) {
                Some(hit) => hit.is_some(),
                None => true,
            }
        };
        for layer in lower.iter().flatten() {
            if writes_under(&layer.cf(id).writes, prefix).any(|k| live(k)) {
                return Ok(true);
            }
        }
        let db = self.db.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        for item in crate::db::prefix_iter(db, &cf_handle, prefix) {
            let (key, _) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            if live(&key) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, StateError> {
        if self.layered(CF_ACCOUNTS).is_none() {
            return StateBackend::get_account(&self.db, address);
        }
        match self.get_cf_raw(CF_ACCOUNTS, address.as_slice())? {
            Some(data) => Ok(Some(decode_account_info(&data)?)),
            None => Ok(None),
        }
    }

    fn put_account(&self, _address: &Address, _info: &AccountInfo) -> Result<(), StateError> {
        Err(read_only())
    }

    fn get_session(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<torus_types::SessionData>, StateError> {
        if self.layered(CF_SESSIONS).is_none() {
            return StateBackend::get_session(&self.db, pubkey);
        }
        match self.get_cf_raw(CF_SESSIONS, pubkey)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| StateError::InvalidData(e.to_string())),
            None => Ok(None),
        }
    }

    fn put_session(
        &self,
        _pubkey: &[u8; 32],
        _data: &torus_types::SessionData,
    ) -> Result<(), StateError> {
        Err(read_only())
    }

    fn delete_session(&self, _pubkey: &[u8; 32]) -> Result<(), StateError> {
        Err(read_only())
    }

    fn count_sessions_for_owner(&self, owner: &Address) -> Result<usize, StateError> {
        if self.layered(CF_SESSIONS).is_none() {
            return StateBackend::count_sessions_for_owner(&self.db, owner);
        }
        let entries = self.iterate_cf(CF_SESSIONS, None)?;
        Ok(entries
            .iter()
            .filter(|(_, v)| {
                serde_json::from_slice::<torus_types::SessionData>(v)
                    .is_ok_and(|d| d.owner == *owner)
            })
            .count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeStateOverlay;

    const CFS: [&str; 4] = [
        crate::cf::CF_ACCOUNTS,
        crate::cf::CF_NATIVE_BALANCES,
        crate::cf::CF_SESSIONS,
        crate::cf::CF_NATIVE_POSITIONS,
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
            (0..1 + self.below(max))
                .map(|_| self.next() as u8)
                .collect()
        }
    }

    fn key(i: u64) -> Vec<u8> {
        vec![b'k', (i / 8) as u8, (i % 8) as u8]
    }

    type Model = BTreeMap<(usize, Vec<u8>), Vec<u8>>;

    fn block(db: &StateDb, height: u64, rng: &mut Rng, model: &mut Model) -> Arc<FrozenPending> {
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

    fn rows(model: &Model, c: usize, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        model
            .iter()
            .filter(|((mc, k), _)| *mc == c && k.starts_with(prefix))
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect()
    }

    fn assert_view_is_model(view: &ReadView, model: &Model, rng: &mut Rng, what: &str) {
        for (c, cf) in CFS.iter().enumerate() {
            assert_eq!(
                view.iterate_cf(cf, None).unwrap(),
                rows(model, c, &[]),
                "{what} {cf}"
            );
            for _ in 0..8 {
                let i = rng.below(48);
                let k = key(i);
                assert_eq!(
                    view.get_cf_raw(cf, &k).unwrap(),
                    model.get(&(c, k.clone())).cloned(),
                    "{what} {cf} get {k:?}"
                );
                assert_eq!(
                    view.get_cf_len(cf, &k).unwrap(),
                    model.get(&(c, k.clone())).map(Vec::len),
                    "{what} {cf} len {k:?}"
                );
                let prefix = &k[..2];
                let under = rows(model, c, prefix);
                assert_eq!(
                    view.iterate_cf(cf, Some(prefix)).unwrap(),
                    under,
                    "{what} {cf}"
                );
                assert_eq!(
                    view.prefix_exists(cf, prefix).unwrap(),
                    !under.is_empty(),
                    "{what} {cf} exists {prefix:?}"
                );
                let limit = 1 + rng.below(6) as usize;
                let from: Vec<_> = rows(model, c, &[])
                    .into_iter()
                    .filter(|(kk, _)| *kk >= k)
                    .take(limit)
                    .collect();
                assert_eq!(
                    view.iterate_cf_from(cf, &k, limit).unwrap(),
                    from,
                    "{what} {cf} from"
                );
                let pfrom: Vec<_> = under
                    .into_iter()
                    .filter(|(kk, _)| *kk >= k)
                    .take(limit)
                    .collect();
                assert_eq!(
                    view.iterate_cf_prefix_from(cf, prefix, &k, limit).unwrap(),
                    pfrom,
                    "{what} {cf} prefix from"
                );
            }
        }
    }

    /// Random DB rows, then 40 random blocks of writes and tombstones. Every
    /// split of the blocks into C (the oldest, merged) and frozen blocks
    /// above it reads exactly like applying the blocks one by one.
    #[test]
    fn read_view_layers_match_sequential_apply_random() {
        for seed in 1..=4u64 {
            let (db, _dir) = temp_db();
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed);
            let mut model = Model::new();
            for (c, cf) in CFS.iter().enumerate() {
                for i in 0..40 {
                    if rng.below(2) == 0 {
                        let v = rng.bytes(12);
                        db.put_cf_raw(cf, &key(i), &v).unwrap();
                        model.insert((c, key(i)), v);
                    }
                }
            }
            let mut blocks = Vec::new();
            for h in 1..=40 {
                blocks.push(block(&db, h, &mut rng, &mut model));
            }
            for split in [0usize, 1, 17, 39, 40] {
                let inflight = (split > 0).then(|| {
                    let mut c = CheckpointLayer::new();
                    for b in &blocks[..split] {
                        c.merge(b);
                    }
                    Arc::new(c)
                });
                let newest_first: Vec<_> = blocks[split..].iter().rev().cloned().collect();
                let view = ReadView::new(db.clone(), 40, newest_first, inflight);
                assert_view_is_model(
                    &view,
                    &model,
                    &mut rng,
                    &format!("seed {seed} split {split}"),
                );
            }
        }
    }

    /// No layers: the DB as it is; writes are refused either way.
    #[test]
    fn read_view_db_only_reads_db_and_refuses_writes() {
        let (db, _dir) = temp_db();
        db.put_cf_raw(CFS[1], b"a", b"1").unwrap();
        let view = ReadView::db_only(db.clone());
        assert_eq!(view.height(), None);
        assert_eq!(view.get_cf_raw(CFS[1], b"a").unwrap(), Some(b"1".to_vec()));
        db.put_cf_raw(CFS[1], b"b", b"2").unwrap();
        assert_eq!(
            view.iterate_cf(CFS[1], None).unwrap().len(),
            2,
            "live DB, not a snapshot"
        );
        assert!(view.put_cf_raw(CFS[1], b"c", b"3").is_err());
        assert!(view.delete_cf_raw(CFS[1], b"a").is_err());
        assert_eq!(db.get_cf_raw(CFS[1], b"c").unwrap(), None);
        let layered = ReadView::new(db.clone(), 1, Vec::new(), None);
        assert!(layered.put_cf_raw(CFS[1], b"c", b"3").is_err());
    }

    /// The newest layer wins: a tombstone in block n hides block n-1's
    /// write, C's write and the DB row; a key no layer holds reads the DB.
    #[test]
    fn read_view_newest_layer_wins() {
        let (db, _dir) = temp_db();
        let cf = crate::cf::CF_SESSIONS;
        db.put_cf_raw(cf, b"k", b"db").unwrap();
        db.put_cf_raw(cf, b"z", b"db").unwrap();
        let o = NativeStateOverlay::new(db.clone());
        o.put_cf_raw(cf, b"k", b"c").unwrap();
        let mut c = CheckpointLayer::new();
        c.merge(&o.freeze(1));
        let o = NativeStateOverlay::new(db.clone());
        o.put_cf_raw(cf, b"k", b"b2").unwrap();
        let b2 = o.freeze(2);
        let o = NativeStateOverlay::new(db.clone());
        o.delete_cf_raw(cf, b"k").unwrap();
        let b3 = o.freeze(3);
        let c = Arc::new(c);
        let at = |blocks: Vec<Arc<FrozenPending>>| {
            ReadView::new(db.clone(), 3, blocks, Some(c.clone()))
                .get_cf_raw(cf, b"k")
                .unwrap()
        };
        assert_eq!(at(vec![]), Some(b"c".to_vec()));
        assert_eq!(at(vec![b2.clone()]), Some(b"b2".to_vec()));
        assert_eq!(at(vec![b3.clone(), b2.clone()]), None);
        let v = ReadView::new(db.clone(), 3, vec![b3, b2], Some(c));
        assert_eq!(v.get_cf_raw(cf, b"z").unwrap(), Some(b"db".to_vec()));
        assert!(!v.prefix_exists(cf, b"k").unwrap());
        assert_eq!(
            v.iterate_cf(cf, None).unwrap(),
            vec![(b"z".to_vec(), b"db".to_vec())]
        );
        assert_eq!(
            (v.height(), v.block_count(), v.has_inflight()),
            (Some(3), 2, true)
        );
    }

    /// Only the ingress CFs read through the layers; any other CF reads the
    /// DB even when a layer holds the key.
    #[test]
    fn read_view_layers_answer_ingress_cfs_only() {
        let (db, _dir) = temp_db();
        let books = crate::cf::CF_NATIVE_ORDER_BOOKS;
        let bal = crate::cf::CF_NATIVE_BALANCES;
        db.put_cf_raw(books, b"m", b"db").unwrap();
        db.put_cf_raw(bal, b"t", b"db").unwrap();
        let o = NativeStateOverlay::new(db.clone());
        o.put_cf_raw(books, b"m", b"layer").unwrap();
        o.put_cf_raw(bal, b"t", b"layer").unwrap();
        let v = ReadView::new(db.clone(), 1, vec![o.freeze(1)], None);
        assert_eq!(v.get_cf_raw(books, b"m").unwrap(), Some(b"db".to_vec()));
        assert_eq!(v.iterate_cf(books, None).unwrap()[0].1, b"db".to_vec());
        assert_eq!(v.get_cf_raw(bal, b"t").unwrap(), Some(b"layer".to_vec()));
        assert!(!INGRESS_CFS.contains(&books));
    }

    /// `FrozenPending::slice` keeps exactly the listed CFs' writes and
    /// tombstones (same height); a view over slices reads like one over the
    /// full sets for those CFs.
    #[test]
    fn read_view_slice_keeps_listed_cfs() {
        let (db, _dir) = temp_db();
        let o = NativeStateOverlay::new(db.clone());
        o.put_cf_raw(crate::cf::CF_NATIVE_ORDER_BOOKS, b"m", b"blob")
            .unwrap();
        o.put_cf_raw(crate::cf::CF_SESSIONS, b"s", b"1").unwrap();
        o.delete_cf_raw(crate::cf::CF_NATIVE_POSITIONS, b"p")
            .unwrap();
        let full = o.freeze(5);
        let slice = Arc::new(full.slice(&INGRESS_CFS));
        assert_eq!(slice.height(), 5);
        assert!(!full.touches_only(&INGRESS_CFS));
        assert!(slice.touches_only(&INGRESS_CFS));
        assert_eq!(
            slice.state_bytes(),
            full.state_bytes() - b"m".len() - b"blob".len()
        );
        db.put_cf_raw(crate::cf::CF_NATIVE_POSITIONS, b"p", b"old")
            .unwrap();
        for b in [full, slice] {
            let v = ReadView::new(db.clone(), 5, vec![b], None);
            assert_eq!(
                v.get_cf_raw(crate::cf::CF_SESSIONS, b"s").unwrap(),
                Some(b"1".to_vec())
            );
            assert_eq!(
                v.get_cf_raw(crate::cf::CF_NATIVE_POSITIONS, b"p").unwrap(),
                None
            );
        }
    }
}
