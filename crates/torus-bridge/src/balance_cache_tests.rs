use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::{AtomicWriteOp, StateError};

#[derive(Default)]
struct Observed {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    writes: Vec<(Vec<u8>, Vec<u8>)>,
    reads: usize,
    fail_read: bool,
    fail_write: Option<usize>,
}

#[derive(Clone, Default)]
struct RecordingBackend(Arc<Mutex<Observed>>);

impl StateBackend for RecordingBackend {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        assert_eq!(cf, CF_NATIVE_BALANCES);
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        if state.fail_read {
            return Err(StateError::InvalidData("injected read".into()));
        }
        Ok(state.rows.get(key).cloned())
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        assert_eq!(cf, CF_NATIVE_BALANCES);
        let mut state = self.0.lock().unwrap();
        state.writes.push((key.to_vec(), value.to_vec()));
        if state.fail_write == Some(state.writes.len()) {
            return Err(StateError::InvalidData("injected write".into()));
        }
        state.rows.insert(key.to_vec(), value.to_vec());
        Ok(())
    }
    fn delete_cf_raw(&self, _: &str, _: &[u8]) -> Result<(), StateError> {
        unreachable!()
    }
    fn iterate_cf(
        &self,
        _: &str,
        _: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        unreachable!()
    }
    fn atomic_write(&self, _: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        unreachable!()
    }
}

// Independent pre-change implementation, including HashSet dirty tracking
// and allocating/sorting the entire retry set before every flush.
struct OldBalanceCache {
    map: HashMap<Address, NativeBalance>,
    dirty: std::collections::HashSet<Address>,
}

impl OldBalanceCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            dirty: std::collections::HashSet::new(),
        }
    }

    /// Return the sender's balance, reading through to `positions` on a miss.
    fn load<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        addr: &Address,
    ) -> Result<NativeBalance, CoreError> {
        if let Some(bal) = self.map.get(addr) {
            return Ok(bal.clone());
        }
        let bal = positions.get_native_balance(addr)?;
        self.map.insert(*addr, bal.clone());
        Ok(bal)
    }

    /// Update the cached balance and mark it dirty (write-back — no overlay PUT yet).
    fn set(&mut self, addr: &Address, bal: NativeBalance) {
        self.map.insert(*addr, bal);
        self.dirty.insert(*addr);
    }

    /// L3-ENG: absorb a Phase-2 worker's cache. Caller guarantees the key
    /// sets are DISJOINT (workers are sharded by sender), so merge order
    /// cannot affect any entry and the result equals the serial cache.
    fn merge_disjoint(&mut self, other: OldBalanceCache) {
        self.map.extend(other.map);
        self.dirty.extend(other.dirty);
    }

    /// Flush every pending dirty balance to the overlay (end of the `execute_batch` call).
    /// Keys are distinct per sender, so final overlay state is independent of flush order;
    /// sorted anyway to keep the write sequence deterministic.
    fn flush_all<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
    ) -> Result<(), CoreError> {
        let mut addrs: Vec<Address> = self.dirty.iter().copied().collect();
        addrs.sort();
        for addr in &addrs {
            if let Some(bal) = self.map.get(addr) {
                positions.put_native_balance(addr, bal)?;
            }
        }
        self.dirty.clear();
        Ok(())
    }
}

fn address(n: u8) -> Address {
    Address::from([n; 20])
}
fn balance(n: i128) -> NativeBalance {
    NativeBalance {
        available: FixedPoint::from_raw(n),
        order_margin: FixedPoint::from_raw(n / 3),
    }
}
fn bytes(balance: &NativeBalance) -> Vec<u8> {
    borsh::to_vec(balance).unwrap()
}
fn assert_same(a: &RecordingBackend, b: &RecordingBackend) {
    let a = a.0.lock().unwrap();
    let b = b.0.lock().unwrap();
    assert_eq!(a.rows, b.rows);
    assert_eq!(
        a.writes, b.writes,
        "exact sorted write attempts and serialized balances"
    );
    assert_eq!(a.reads, b.reads);
}

#[test]
fn balance_cache_repeated_updates_and_clean_reads_match_old_cache() {
    let new_db = RecordingBackend::default();
    let old_db = RecordingBackend::default();
    let new_positions = PositionManager::new(new_db.clone());
    let old_positions = PositionManager::new(old_db.clone());
    let mut new = BalanceCache::new();
    let mut old = OldBalanceCache::new();
    for n in 1..=100 {
        assert_eq!(
            bytes(&new.load(&new_positions, &address(n)).unwrap()),
            bytes(&old.load(&old_positions, &address(n)).unwrap())
        );
    }
    // Load returns an independent clone, without implicitly dirtying it.
    let mut loaded = new.load(&new_positions, &address(50)).unwrap();
    loaded.available = FixedPoint::from_raw(999);
    assert_ne!(
        bytes(&loaded),
        bytes(&new.load(&new_positions, &address(50)).unwrap())
    );
    old.load(&old_positions, &address(50)).unwrap();
    old.load(&old_positions, &address(50)).unwrap();
    for round in 0..40 {
        for n in [90, 3, 101] {
            new.set(&address(n), balance(round * 100 + n as i128));
            old.set(&address(n), balance(round * 100 + n as i128));
        }
    }
    assert_eq!(new.dirty.len(), 3);
    new.flush_all(&new_positions).unwrap();
    old.flush_all(&old_positions).unwrap();
    assert_same(&new_db, &old_db);
    assert_eq!(new_db.0.lock().unwrap().writes.len(), 3);
    assert!(new.dirty.is_empty());
    assert!(new.map.values().all(|entry| !entry.dirty));
    new.flush_all(&new_positions).unwrap();
    old.flush_all(&old_positions).unwrap();
    for n in [3, 3] {
        new.set(&address(n), balance(-7));
        old.set(&address(n), balance(-7));
    }
    new.flush_all(&new_positions).unwrap();
    old.flush_all(&old_positions).unwrap();
    assert_same(&new_db, &old_db);
    assert_eq!(new_db.0.lock().unwrap().writes.len(), 4);
}

#[test]
fn balance_cache_disjoint_merge_matches_old_cache_in_both_orders() {
    for reverse in [false, true] {
        let new_db = RecordingBackend::default();
        let old_db = RecordingBackend::default();
        let new_positions = PositionManager::new(new_db.clone());
        let old_positions = PositionManager::new(old_db.clone());
        let mut new = BalanceCache::new();
        let mut old = OldBalanceCache::new();
        let shards = if reverse { [2, 1] } else { [1, 2] };
        for shard in shards {
            let mut new_worker = BalanceCache::new();
            let mut old_worker = OldBalanceCache::new();
            new_worker
                .load(&new_positions, &address(shard + 100))
                .unwrap();
            old_worker
                .load(&old_positions, &address(shard + 100))
                .unwrap();
            for value in [1, 2, 3] {
                new_worker.set(&address(shard), balance(value));
                old_worker.set(&address(shard), balance(value));
            }
            new.merge_disjoint(new_worker);
            old.merge_disjoint(old_worker);
        }
        assert_eq!(new.dirty.len(), 2);
        assert_eq!(new.map.len(), 4);
        new.flush_all(&new_positions).unwrap();
        old.flush_all(&old_positions).unwrap();
        assert_same(&new_db, &old_db);
        assert_eq!(new_db.0.lock().unwrap().writes.len(), 2);
    }
}

#[test]
fn balance_cache_partial_flush_keeps_all_dirty_for_repeated_retries() {
    for fail_at in 1..=3 {
        let new_db = RecordingBackend::default();
        let old_db = RecordingBackend::default();
        let new_positions = PositionManager::new(new_db.clone());
        let old_positions = PositionManager::new(old_db.clone());
        let mut new = BalanceCache::new();
        let mut old = OldBalanceCache::new();
        for n in [3, 1, 2] {
            new.set(&address(n), balance(n as i128));
            old.set(&address(n), balance(n as i128));
        }
        for (attempt, failure) in [Some(fail_at), Some(1), None].into_iter().enumerate() {
            for db in [&new_db, &old_db] {
                let mut state = db.0.lock().unwrap();
                state.writes.clear();
                state.fail_write = failure;
            }
            let got = new.flush_all(&new_positions).map_err(|e| e.to_string());
            let expected = old.flush_all(&old_positions).map_err(|e| e.to_string());
            assert_eq!(got, expected);
            assert_same(&new_db, &old_db);
            if failure.is_some() {
                assert!(got.is_err());
                assert!(new.map.values().all(|entry| entry.dirty));
                assert_eq!(new.dirty.len(), if attempt == 0 { 3 } else { 4 });
                // Updating a previously written dirty address must not append it
                // twice; adding a new sender must join the complete retry set.
                for n in [1, 4] {
                    new.set(&address(n), balance(99));
                    old.set(&address(n), balance(99));
                }
            } else {
                assert!(got.is_ok());
                assert!(new.dirty.is_empty());
                assert!(new.map.values().all(|entry| !entry.dirty));
                assert_eq!(new_db.0.lock().unwrap().writes.len(), 4);
            }
        }
    }
}

#[test]
fn balance_cache_failed_load_is_not_cached_or_dirtied() {
    let new_db = RecordingBackend::default();
    let old_db = RecordingBackend::default();
    let new_positions = PositionManager::new(new_db.clone());
    let old_positions = PositionManager::new(old_db.clone());
    let mut new = BalanceCache::new();
    let mut old = OldBalanceCache::new();
    for fail in [true, false] {
        new_db.0.lock().unwrap().fail_read = fail;
        old_db.0.lock().unwrap().fail_read = fail;
        let got = new
            .load(&new_positions, &address(1))
            .map(|v| bytes(&v))
            .map_err(|e| e.to_string());
        let expected = old
            .load(&old_positions, &address(1))
            .map(|v| bytes(&v))
            .map_err(|e| e.to_string());
        assert_eq!(got, expected);
        assert_eq!(new.map.len(), usize::from(!fail));
        assert!(new.dirty.is_empty());
    }
    assert_same(&new_db, &old_db);
}
