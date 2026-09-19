//! Durable native-action data-availability (DA) body store.
//!
//! Stores native-action bodies keyed by their 32-byte action-hash in
//! [`CF_NATIVE_PENDING`], **decoupled** from the 60s nonce-staleness gate that
//! governs mempool admission. A body referenced by a proposed or committed
//! `CompactBlock` must always be reconstructable — even after the nonce window
//! expires or the process restarts — so consensus never wedges on a missing
//! body (livelock root cause, mem 28e1a821).
//!
//! Push-primary / fetch-rare (s297 Option D): bodies are mirrored here on every
//! ingest / produce / receive path, and the rare pull-fallback
//! (`/torus/native-da/1.0`) serves them by-hash on a miss. Bodies use the same
//! `bincode` encoding as the native-action network wire format.

use std::sync::{Condvar, Mutex, OnceLock};

use alloy_primitives::B256;
use torus_types::{compute_action_hash, compute_action_hash_with_scratch, SignedNativeAction};

use crate::cf::{CF_NATIVE_PENDING, CF_NATIVE_SHARDS};
use crate::db::StateDb;
use crate::erasure::{encode, ErasureParams};
use crate::error::StateError;
use crate::shard_store::{decode_stored_shard, encode_stored_shard, shard_key, StoredShard};

/// Process-wide body-arrival notifier (S391). `NativeDaStore` instances are
/// constructed independently over the one shared DB, so the notifier is a
/// process global: any `put` (proposer push ingest, gossip mirror, pull
/// absorb) wakes every waiter. The hot reconstruct path waits on this instead
/// of sleeping fixed 20ms ticks, so the common push-race resolves at actual
/// arrival time.
struct ArrivalNotifier {
    generation: Mutex<u64>,
    arrived: Condvar,
}

fn arrivals() -> &'static ArrivalNotifier {
    static ARRIVALS: OnceLock<ArrivalNotifier> = OnceLock::new();
    ARRIVALS.get_or_init(|| ArrivalNotifier {
        generation: Mutex::new(0),
        arrived: Condvar::new(),
    })
}

/// Durable, nonce-gate-decoupled store of native-action bodies keyed by action-hash.
///
/// CF-backed (survives restart). Cheap to clone — it shares the underlying
/// [`StateDb`] handle (an `Arc<DB>`), so every consensus / mempool / network
/// site can hold its own `NativeDaStore` over the one database.
#[derive(Clone)]
pub struct NativeDaStore {
    db: StateDb,
}

impl NativeDaStore {
    /// Wrap a shared state-db handle.
    pub fn new(db: StateDb) -> Self {
        Self { db }
    }

    /// Store a body, keyed by its [`compute_action_hash`]. Idempotent: re-putting
    /// the same body overwrites with identical bytes. Wakes every
    /// [`wait_for_arrival`](Self::wait_for_arrival) waiter.
    pub fn put(&self, action: &SignedNativeAction) -> Result<(), StateError> {
        let hash = compute_action_hash(action);
        let bytes =
            bincode::serialize(action).map_err(|e| StateError::InvalidData(e.to_string()))?;
        self.db
            .put_cf_raw(CF_NATIVE_PENDING, hash.as_slice(), &bytes)?;
        let notifier = arrivals();
        let mut generation = notifier
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation = generation.wrapping_add(1);
        notifier.arrived.notify_all();
        Ok(())
    }

    /// Store many bodies in ONE atomic `WriteBatch`, waking arrival waiters once.
    ///
    /// The leader's `produce_block` mirrors a full block's actions (up to the
    /// per-block cap) before the proposal is encoded; doing that with per-body
    /// [`put`](Self::put) calls issued N separate RocksDB writes + N notifier
    /// wakes on the consensus hot path (S395 floor shave #1). Empty input is a
    /// no-op. Same idempotent overwrite semantics as `put`.
    pub fn put_batch(&self, actions: &[SignedNativeAction]) -> Result<(), StateError> {
        if actions.is_empty() {
            return Ok(());
        }
        let mut batch = rocksdb::WriteBatch::default();
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let mut hash_scratch = Vec::with_capacity(64);
        let mut bytes = Vec::new();
        for action in actions {
            let hash = compute_action_hash_with_scratch(action, &mut hash_scratch);
            bytes.clear();
            bincode::serialize_into(&mut bytes, action)
                .map_err(|e| StateError::InvalidData(e.to_string()))?;
            // WriteBatch copies the value into its owned representation before
            // returning, so the next action can safely reuse this buffer.
            batch.put_cf(cf, hash.as_slice(), &bytes);
        }
        self.db.write(batch)?;
        let notifier = arrivals();
        let mut generation = notifier
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation = generation.wrapping_add(1);
        notifier.arrived.notify_all();
        Ok(())
    }

    /// Snapshot the arrival generation. Take it BEFORE checking the store for
    /// missing bodies and pass it to [`wait_for_arrival`](Self::wait_for_arrival)
    /// so an insert racing the check can never be missed.
    pub fn arrival_generation() -> u64 {
        *arrivals()
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Block until any body arrives (a `put` after `seen_generation` was
    /// snapshotted) or `timeout` elapses. Returns the latest generation, which
    /// the caller feeds back in on the next iteration of its wait loop.
    pub fn wait_for_arrival(seen_generation: u64, timeout: std::time::Duration) -> u64 {
        let notifier = arrivals();
        let generation = notifier
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *generation > seen_generation {
            return *generation;
        }
        let (generation, _timed_out) = notifier
            .arrived
            .wait_timeout_while(generation, timeout, |g| *g <= seen_generation)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation
    }

    /// Fetch a body by action-hash. Returns `None` if absent.
    pub fn get(&self, hash: &B256) -> Result<Option<SignedNativeAction>, StateError> {
        match self.db.get_cf_raw(CF_NATIVE_PENDING, hash.as_slice())? {
            Some(bytes) => {
                let action = bincode::deserialize(&bytes)
                    .map_err(|e| StateError::InvalidData(e.to_string()))?;
                Ok(Some(action))
            }
            None => Ok(None),
        }
    }

    /// Batched [`get`](Self::get): one RocksDB `MultiGet` for all `hashes`,
    /// results in input order, `None` per absent hash. A present-but-undecodable
    /// body is `Err(InvalidData)` exactly as in `get`. The hot compact-block
    /// reconstruct path uses this instead of a per-hash `get` loop.
    pub fn get_batch(&self, hashes: &[B256]) -> Result<Vec<Option<SignedNativeAction>>, StateError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        // Decode directly from pinned values: the generic raw MultiGet copies
        // each full body into a temporary Vec that this path would discard.
        // Input is deliberately not sorted; duplicates and caller order matter.
        // Collect storage errors before decoding, as the old raw helper did.
        let pinned = self
            .db
            .inner()
            .batched_multi_get_cf(cf, hashes.iter().map(|hash| hash.as_slice()), false)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        pinned
            .into_iter()
            .map(|slot| match slot {
                Some(bytes) => bincode::deserialize(&bytes)
                    .map(Some)
                    .map_err(|e| StateError::InvalidData(e.to_string())),
                None => Ok(None),
            })
            .collect()
    }

    /// Fetch the raw stored bytes (`bincode(SignedNativeAction)`) by action-hash,
    /// without deserializing. The `/torus/native-da/1.0` serve path ships these
    /// bytes verbatim to a requesting peer. Takes the 32-byte hash directly so the
    /// network layer needn't depend on `alloy-primitives`. Returns `None` if absent.
    pub fn get_raw(&self, hash: &[u8; 32]) -> Result<Option<Vec<u8>>, StateError> {
        self.db.get_cf_raw(CF_NATIVE_PENDING, hash.as_slice())
    }

    /// Raw batched reads for DA serving. Preserve request order and duplicates;
    /// values are returned verbatim, including undecodable stored bytes.
    /// A storage error fails the batch, allowing callers to retry per key.
    pub fn get_raw_batch(&self, hashes: &[[u8; 32]]) -> Result<Vec<Option<Vec<u8>>>, StateError> {
        let keys: Vec<&[u8]> = hashes.iter().map(|hash| hash.as_slice()).collect();
        self.db.multi_get_cf_raw(CF_NATIVE_PENDING, &keys)
    }

    /// Remove bodies by hash (e.g. after commit + an eviction window). Best-effort:
    /// absent keys are silently skipped.
    pub fn remove(&self, hashes: &[B256]) -> Result<(), StateError> {
        for h in hashes {
            self.db.delete_cf_raw(CF_NATIVE_PENDING, h.as_slice())?;
        }
        Ok(())
    }

    /// Presence check by action-hash WITHOUT copying the body. The pre-warm pull
    /// filter calls this once per manifest hash, so it must not pay `get_raw`'s
    /// multi-KB value copy just to test existence.
    pub fn contains(&self, hash: &[u8; 32]) -> Result<bool, StateError> {
        self.db.exists_cf_raw(CF_NATIVE_PENDING, hash.as_slice())
    }

    /// Erasure-encode each body under `params` and persist all `n` shards into
    /// [`CF_NATIVE_SHARDS`] in ONE atomic `WriteBatch` (Sprint 5 T5, recovery-path).
    ///
    /// ADDITIVE: the whole-body mirror ([`put_batch`](Self::put_batch)) remains the
    /// durability guarantee; this only ADDS shard custody so a lagging peer can
    /// reconstruct from `k` shards gathered across `k` DIFFERENT sources instead of
    /// pulling the whole body from one (kills the s338 single-source hotspot). The
    /// bytes sharded are the SAME `bincode(SignedNativeAction)` keyed by
    /// [`compute_action_hash`], so the body-hash backstop at reconstruct time
    /// re-hashes to exactly this. Empty input is a no-op.
    pub fn put_shards_batch(
        &self,
        actions: &[SignedNativeAction],
        params: ErasureParams,
    ) -> Result<(), StateError> {
        if actions.is_empty() {
            return Ok(());
        }
        params
            .validate()
            .map_err(|e| StateError::InvalidData(e.to_string()))?;
        let mut batch = rocksdb::WriteBatch::default();
        let cf = self.db.cf_handle(CF_NATIVE_SHARDS)?;
        let mut hash_scratch = Vec::with_capacity(64);
        let mut body = Vec::new();
        for action in actions {
            let hash = compute_action_hash_with_scratch(action, &mut hash_scratch);
            body.clear();
            bincode::serialize_into(&mut body, action)
                .map_err(|e| StateError::InvalidData(e.to_string()))?;
            // EncodedBody owns its shards; no body-buffer borrow survives this
            // iteration. All shards still enter the same atomic WriteBatch.
            let enc = encode(&body, params).map_err(|e| StateError::InvalidData(e.to_string()))?;
            for i in 0..enc.params.n {
                let stored = StoredShard::from_encoded_body(&enc, i);
                let bytes = encode_stored_shard(&stored)?;
                batch.put_cf(cf, shard_key(&hash.0, i as u16), &bytes);
            }
        }
        self.db.write(batch)?;
        Ok(())
    }

    /// Fetch a single custodied shard for `(body_hash, index)`. `None` when this
    /// node does not custody it (the serve path answers `present=false`; a fetcher
    /// then tries another peer/index).
    pub fn get_shard(
        &self,
        body_hash: &[u8; 32],
        index: u16,
    ) -> Result<Option<StoredShard>, StateError> {
        match self
            .db
            .get_cf_raw(CF_NATIVE_SHARDS, &shard_key(body_hash, index))?
        {
            Some(bytes) => Ok(Some(decode_stored_shard(&bytes)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, NativeDaStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open temp StateDb");
        (dir, NativeDaStore::new(db))
    }

    /// `contains` must reflect raw presence in CF_NATIVE_PENDING without needing
    /// a decodable body (the serve path also ships raw bytes verbatim).
    #[test]
    fn contains_reflects_put_and_absent() {
        let (_dir, store) = temp_store();
        let present = [7u8; 32];
        let absent = [9u8; 32];
        store
            .db
            .put_cf_raw(CF_NATIVE_PENDING, &present, b"body-bytes")
            .expect("raw put");
        assert!(store.contains(&present).expect("contains present"));
        assert!(!store.contains(&absent).expect("contains absent"));
    }

    /// `get_batch` must return one slot per input hash, in input order, with
    /// `None` for absent keys — the single-read replacement for the per-hash
    /// `get` loop on the hot reconstruct path.
    #[test]
    fn get_batch_preserves_order_and_reports_missing() {
        let (_dir, store) = temp_store();
        let a1 = dummy_action(1);
        let a2 = dummy_action(2);
        let a3 = dummy_action(3);
        let h1 = compute_action_hash(&a1);
        let h3 = compute_action_hash(&a3);
        let missing = B256::repeat_byte(0xAB);
        store.put_batch(&[a1.clone(), a2, a3.clone()]).expect("put_batch");

        let got = store.get_batch(&[h1, missing, h3]).expect("get_batch");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].as_ref().map(compute_action_hash), Some(h1));
        assert!(got[1].is_none(), "absent hash must read as None");
        assert_eq!(got[2].as_ref().map(compute_action_hash), Some(h3));

        assert!(store.get_batch(&[]).expect("empty get_batch").is_empty());
    }

    /// A present-but-undecodable body is an `Err`, exactly like `get`.
    #[test]
    fn get_batch_rejects_invalid_data() {
        let (_dir, store) = temp_store();
        let bad = [5u8; 32];
        store
            .db
            .put_cf_raw(CF_NATIVE_PENDING, &bad, b"not-bincode")
            .expect("raw put");
        assert!(matches!(
            store.get_batch(&[B256::from(bad)]),
            Err(StateError::InvalidData(_))
        ));
    }

    #[test]
    fn pinned_batch_matches_single_reads_for_unsorted_duplicates_and_sst_values() {
        use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};
        let (dir, store) = temp_store();
        let mut large = dummy_action(2);
        large.action = NativeAction::PlaceOrderBatch(vec![PlaceOrderParams {
            market_id: 7,
            is_buy: true,
            price: FixedPoint::from_raw(123_000_000),
            quantity: FixedPoint::from_raw(100_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }; 400]);
        let actions = [dummy_action(1), large, dummy_action(3)];
        store.put_batch(&actions).unwrap();
        let mut hashes: Vec<_> = actions.iter().map(compute_action_hash).collect();
        hashes.sort_unstable();
        hashes.reverse();
        hashes.insert(1, B256::repeat_byte(0xAB));
        hashes.extend_from_within(0..3);
        let check = |store: &NativeDaStore| {
            let expected: Vec<_> = hashes.iter().map(|hash| store.get(hash).unwrap()).collect();
            let got = store.get_batch(&hashes).unwrap();
            assert_eq!(bincode::serialize(&got).unwrap(), bincode::serialize(&expected).unwrap());
            assert_eq!(got.len(), hashes.len());
            assert!(store.get_batch(&[]).unwrap().is_empty());
        };
        check(&store); // memtable values
        let cf = store.db.cf_handle(CF_NATIVE_PENDING).unwrap();
        store.db.inner().flush_cf(cf).unwrap();
        store.db.inner().compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        check(&store); // SST values and block-cache pins
        drop(store);
        let reopened = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        check(&reopened);
        // Overwrite and delete after prior reads; pins must not escape decoding.
        reopened.db.put_cf_raw(CF_NATIVE_PENDING, hashes[0].as_slice(), b"bad").unwrap();
        assert!(matches!(reopened.get_batch(&hashes), Err(StateError::InvalidData(_))));
        reopened.remove(&[hashes[0]]).unwrap();
        check(&reopened);
    }

    fn dummy_action(nonce: u64) -> SignedNativeAction {
        use torus_types::{ActionSignature, NativeAction, Signature};
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    // Frozen 6a51413 shard batch oracle: keep allocating bincode::serialize
    // independent of the reusable production serializer.
    fn legacy_put_shards_batch(
        store: &NativeDaStore,
        actions: &[SignedNativeAction],
        params: ErasureParams,
    ) -> Result<(), StateError> {
        if actions.is_empty() {
            return Ok(());
        }
        params
            .validate()
            .map_err(|e| StateError::InvalidData(e.to_string()))?;
        let mut batch = rocksdb::WriteBatch::default();
        let cf = store.db.cf_handle(CF_NATIVE_SHARDS)?;
        for action in actions {
            let hash = compute_action_hash(action);
            let body =
                bincode::serialize(action).map_err(|e| StateError::InvalidData(e.to_string()))?;
            let enc = encode(&body, params).map_err(|e| StateError::InvalidData(e.to_string()))?;
            for i in 0..enc.params.n {
                let stored = StoredShard::from_encoded_body(&enc, i);
                let bytes = encode_stored_shard(&stored)?;
                batch.put_cf(cf, shard_key(&hash.0, i as u16), &bytes);
            }
        }
        store.db.write(batch)?;
        Ok(())
    }

    #[test]
    fn batch_hash_scratch_preserves_body_and_shard_bytes_across_reuse_and_reopen() {
        use crate::StateBackend;
        use torus_types::{ActionSignature, Ed25519Sig, FixedPoint, NativeAction,
            OrderType, PlaceOrderParams, TimeInForce};
        let (dir, actual) = temp_store();
        let (_reference_dir, reference) = temp_store();
        let large = SignedNativeAction {
            action: NativeAction::PlaceOrderBatch(vec![PlaceOrderParams {
                market_id: 7, is_buy: true, price: FixedPoint::from_raw(123_000_000),
                quantity: FixedPoint::from_raw(100_000_011),
                order_type: OrderType::StopLimit {
                    trigger: FixedPoint::from_raw(200_000_000),
                    limit: FixedPoint::from_raw(123_000_000),
                },
                time_in_force: TimeInForce::GTC, reduce_only: false, client_order_id: Some(42),
            }; 400]),
            nonce: u64::MAX,
            signature: ActionSignature::Session {
                session_pubkey: [7; 32], sig: Ed25519Sig([9; 64]),
            },
        };
        let mut small_session = dummy_action(3);
        small_session.signature = large.signature.clone();
        // Repeated first body also pins the existing idempotent overwrite.
        let actions = vec![dummy_action(1), large, small_session, dummy_action(1)];
        let params = ErasureParams::new(2, 3);
        actual.put_batch(&actions).unwrap();
        actual.put_shards_batch(&actions, params).unwrap();
        for action in &actions {
            reference.put(action).unwrap();
        }
        legacy_put_shards_batch(&reference, &actions, params).unwrap();
        for cf in [CF_NATIVE_PENDING, CF_NATIVE_SHARDS] {
            assert_eq!(actual.db.iterate_cf(cf, None).unwrap(), reference.db.iterate_cf(cf, None).unwrap());
        }
        drop(actual);
        let reopened = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        for action in &actions {
            let hash = compute_action_hash(action);
            assert_eq!(reopened.get_raw(&hash.0).unwrap().unwrap(), bincode::serialize(action).unwrap());
            for i in 0..params.n as u16 {
                let shard = reopened.get_shard(&hash.0, i).unwrap().unwrap();
                assert!(shard.verify());
                assert_eq!(encode_stored_shard(&shard).unwrap(),
                    encode_stored_shard(&reference.get_shard(&hash.0, i).unwrap().unwrap()).unwrap());
            }
        }
    }

    /// S391: `put` must wake a blocked `wait_for_arrival` immediately — the hot
    /// reconstruct path waits on this instead of sleeping fixed 20ms ticks, so
    /// a racing push resolves at arrival time rather than the next tick.
    #[test]
    fn wait_for_arrival_wakes_on_put() {
        let (_dir, store) = temp_store();
        let seen = NativeDaStore::arrival_generation();
        let writer = store.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            writer.put(&dummy_action(1)).expect("put");
        });
        let start = std::time::Instant::now();
        let new_gen =
            NativeDaStore::wait_for_arrival(seen, std::time::Duration::from_millis(2_000));
        t.join().unwrap();
        assert!(new_gen > seen, "a put must advance the arrival generation");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1_000),
            "waiter must wake on the arrival, not ride out the timeout (elapsed {:?})",
            start.elapsed()
        );
    }

    /// With no arrival the wait returns at the timeout. Tolerant to unrelated
    /// concurrent puts from parallel tests (asserts return, not stasis).
    #[test]
    fn wait_for_arrival_returns_on_timeout() {
        let seen = NativeDaStore::arrival_generation();
        let start = std::time::Instant::now();
        let new_gen = NativeDaStore::wait_for_arrival(seen, std::time::Duration::from_millis(30));
        assert!(new_gen >= seen, "generation never regresses");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1_500),
            "wait must return promptly after its timeout"
        );
    }

    /// T5: mirroring encodes each body into exactly `n` shards keyed `0..n`, and
    /// every stored shard verifies against its own committed root.
    #[test]
    fn mirror_encodes_and_persists_n_shards() {
        let (_dir, store) = temp_store();
        let action = dummy_action(1);
        let params = ErasureParams::new(2, 3);
        store
            .put_shards_batch(std::slice::from_ref(&action), params)
            .expect("put shards");
        let hash = compute_action_hash(&action);
        for i in 0..params.n as u16 {
            let s = store.get_shard(&hash.0, i).expect("get").expect("shard present");
            assert_eq!(s.shard_index, i);
            assert!(s.verify(), "stored shard {i} must verify against its root");
        }
        // No (n+1)th shard.
        assert!(store
            .get_shard(&hash.0, params.n as u16)
            .expect("get")
            .is_none());
    }

    /// T5: the shards read back out reconstruct the ORIGINAL body bytes, which
    /// re-hash to the action-hash used as the key (body-hash backstop identity).
    #[test]
    fn stored_shards_reconstruct_original_body() {
        use crate::erasure::reconstruct;
        let (_dir, store) = temp_store();
        let action = dummy_action(42);
        let params = ErasureParams::new(2, 3);
        store
            .put_shards_batch(std::slice::from_ref(&action), params)
            .expect("put shards");
        let hash = compute_action_hash(&action);
        let mut slots: Vec<Option<Vec<u8>>> = Vec::new();
        let mut body_len = 0usize;
        for i in 0..params.n as u16 {
            let s = store.get_shard(&hash.0, i).expect("get").expect("present");
            body_len = s.body_len as usize;
            slots.push(Some(s.shard_bytes));
        }
        let body = reconstruct(slots, params, body_len).expect("reconstruct");
        let original = bincode::serialize(&action).expect("serialize");
        assert_eq!(body, original, "reconstructed body must equal bincode(action)");
        let decoded: SignedNativeAction = bincode::deserialize(&body).expect("decode");
        assert_eq!(compute_action_hash(&decoded), hash, "backstop: re-hash matches key");
    }

    /// T5: shard encode is ADDITIVE — the whole-body store still serves the body
    /// after both mirrors run (no disturbance to the durability path).
    #[test]
    fn mirror_still_populates_whole_body_store() {
        let (_dir, store) = temp_store();
        let action = dummy_action(7);
        store
            .put_batch(std::slice::from_ref(&action))
            .expect("put_batch body");
        store
            .put_shards_batch(std::slice::from_ref(&action), ErasureParams::new(2, 3))
            .expect("put shards");
        let hash = compute_action_hash(&action);
        assert!(
            store.get(&hash).expect("get").is_some(),
            "whole body still present after shard encode"
        );
    }
}
