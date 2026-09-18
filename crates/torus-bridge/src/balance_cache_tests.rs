//! Balance write-back semantics and injected failures without process-global env.
use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use torus_state::backend::AtomicWriteOp;
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::StateError;

#[derive(Default)]
struct ObservedState {
    rows: BTreeMap<(String, Vec<u8>), Vec<u8>>,
    balance_reads: usize,
    balance_writes: Vec<Vec<u8>>,
    fail_reads: bool,
    fail_write: Option<Vec<u8>>,
}

#[derive(Clone, Default)]
struct ObservedBackend(Arc<Mutex<ObservedState>>);

impl StateBackend for ObservedBackend {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let mut state = self.0.lock().unwrap();
        if cf == CF_NATIVE_BALANCES {
            state.balance_reads += 1;
            if state.fail_reads {
                return Err(StateError::InvalidData("injected balance read failure".into()));
            }
        }
        Ok(state.rows.get(&(cf.into(), key.to_vec())).cloned())
    }

    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        let mut state = self.0.lock().unwrap();
        if cf == CF_NATIVE_BALANCES {
            state.balance_writes.push(key.to_vec());
            if state.fail_write.as_deref() == Some(key) {
                return Err(StateError::InvalidData("injected balance write failure".into()));
            }
        }
        state.rows.insert((cf.into(), key.to_vec()), value.to_vec());
        Ok(())
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.0.lock().unwrap().rows.remove(&(cf.into(), key.to_vec()));
        Ok(())
    }

    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>)
        -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>
    {
        Ok(self.0.lock().unwrap().rows.iter()
            .filter(|((name, key), _)| name == cf && prefix.map_or(true, |p| key.starts_with(p)))
            .map(|((_, key), value)| (key.clone(), value.clone()))
            .collect())
    }

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        // These fixtures exercise cache flush via ordinary put_cf_raw. Do not
        // silently bypass the failure/logging oracle through another write API.
        assert!(ops.is_empty(), "unexpected atomic write in balance-cache fixture");
        Ok(())
    }
}

fn addr(n: u8) -> Address { Address::from([n; 20]) }
fn fp(n: i64) -> FixedPoint { FixedPoint::from_raw(n as i128 * FixedPoint::SCALE) }

#[test]
fn cold_read_failure_retries_and_clean_warm_reads_do_not_materialize() {
    let backend = ObservedBackend::default();
    let positions = PositionManager::new(backend.clone());
    let mut cache = BalanceCache::new();
    backend.0.lock().unwrap().fail_reads = true;
    assert!(cache.load(&positions, &addr(1)).is_err());
    assert!(cache.map.is_empty(), "failed reads must not become cached defaults");
    backend.0.lock().unwrap().fail_reads = false;
    assert_eq!(cache.load(&positions, &addr(1)).unwrap().value.available, FixedPoint::ZERO);
    assert_eq!(cache.load(&positions, &addr(1)).unwrap().value.available, FixedPoint::ZERO);
    cache.flush_all(&positions).unwrap();
    let state = backend.0.lock().unwrap();
    assert_eq!(state.balance_reads, 2, "one failed read, one cold read, no warm read");
    assert!(state.balance_writes.is_empty());
    assert!(state.rows.is_empty(), "a clean default must not create a balance row");
}

#[test]
fn actual_margin_rejection_stays_clean_in_serial_and_sharded_prepare() {
    for threads in [0, 2] {
        let backend = ObservedBackend::default();
        let mut ctx = NativeExecContext::new_with_mode(
            backend.clone(), 1, 1000, 0, 100, 4,
            addr(99), addr(100), addr(101), BookMode::LevelAuthorityChunked, None,
        );
        assert!(ctx.fatal_error.is_none());
        let actions: Vec<_> = (1..=2).map(|n| (addr(n), NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: n as u64,
            is_buy: true,
            price: fp(100),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }))).collect();
        let result = NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, threads);
        assert!(ctx.fatal_error.is_none());
        assert_eq!(result.results.len(), 2);
        for action in result.results {
            assert!(!action.success);
            assert!(action.error.unwrap().starts_with("insufficient margin"));
        }
        let state = backend.0.lock().unwrap();
        assert_eq!(state.balance_reads, 2);
        assert!(state.balance_writes.is_empty(), "threads={threads}: rejected reserves wrote defaults");
        assert!(!state.rows.keys().any(|(cf, _)| cf == CF_NATIVE_BALANCES));
    }
}

#[test]
fn actual_zero_pnl_close_materializes_balance_once() {
    let backend = ObservedBackend::default();
    let positions = PositionManager::new(backend.clone());
    let mut balances = BalanceCache::new();
    let mut positions_cache = PositionCache::new();
    // Open then close at the same price: the close returns Some(ZERO), which
    // must still materialize the balance even though the numeric value stays 0.
    for is_buy in [true, false] {
        NativeExecutor::apply_fill_via_caches(
            &positions, &mut positions_cache, &mut balances,
            &addr(1), 1, is_buy, fp(1), fp(100),
        ).unwrap();
    }
    balances.flush_all(&positions).unwrap();
    balances.flush_all(&positions).unwrap();
    let state = backend.0.lock().unwrap();
    assert_eq!(state.balance_reads, 1);
    assert_eq!(state.balance_writes, vec![addr(1).as_slice().to_vec()]);
    assert_eq!(
        state.rows.get(&(CF_NATIVE_BALANCES.into(), addr(1).as_slice().to_vec())).unwrap(),
        &borsh::to_vec(&NativeBalance::default()).unwrap(),
    );
}

#[test]
fn merged_dirty_entries_flush_sorted_and_partial_failure_retries_full_set() {
    let backend = ObservedBackend::default();
    let positions = PositionManager::new(backend.clone());
    let mut cache = BalanceCache::new();
    for n in [3, 1] {
        cache.load(&positions, &addr(n)).unwrap().update(|bal| bal.available = fp(n as i64));
    }
    let mut worker_cache = BalanceCache::new();
    worker_cache.load(&positions, &addr(2)).unwrap(); // clean default stays absent
    worker_cache.load(&positions, &addr(4)).unwrap().update(|bal| bal.available += FixedPoint::ZERO);
    cache.merge_disjoint(worker_cache);
    backend.0.lock().unwrap().fail_write = Some(addr(3).as_slice().to_vec());
    assert!(cache.flush_all(&positions).is_err());
    {
        let mut state = backend.0.lock().unwrap();
        assert_eq!(state.balance_writes, vec![addr(1).as_slice().to_vec(), addr(3).as_slice().to_vec()]);
        state.balance_writes.clear();
        state.fail_write = None;
    }
    cache.flush_all(&positions).unwrap();
    cache.flush_all(&positions).unwrap();
    {
        let state = backend.0.lock().unwrap();
        assert_eq!(state.balance_writes, [1, 3, 4].map(|n| addr(n).as_slice().to_vec()));
        assert!(!state.rows.contains_key(&(CF_NATIVE_BALANCES.into(), addr(2).as_slice().to_vec())));
        for n in [1, 3, 4] {
            let expected = NativeBalance {
                available: if n == 4 { FixedPoint::ZERO } else { fp(n as i64) },
                order_margin: FixedPoint::ZERO,
            };
            assert_eq!(
                state.rows.get(&(CF_NATIVE_BALANCES.into(), addr(n).as_slice().to_vec())).unwrap(),
                &borsh::to_vec(&expected).unwrap(),
            );
        }
    }
    cache.load(&positions, &addr(4)).unwrap().update(|bal| bal.available += fp(1));
    cache.flush_all(&positions).unwrap();
    let state = backend.0.lock().unwrap();
    assert_eq!(state.balance_reads, 4, "merges and warm writes must reuse loaded values");
    assert_eq!(state.balance_writes.last().unwrap(), &addr(4).as_slice().to_vec());
    assert_eq!(state.balance_writes.len(), 4, "only the newly dirtied entry is written again");
}

#[test]
fn arithmetic_panic_keeps_previous_value_and_dirty_state() {
    for dirty in [false, true] {
        let initial = NativeBalance {
            available: FixedPoint::from_raw(i128::MAX),
            order_margin: fp(1),
        };
        let mut entry = CachedBalance { value: initial.clone(), dirty };
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            entry.update(|bal| {
                bal.order_margin -= fp(1);
                bal.available += fp(1); // overflow after the first field changed
            });
        }));
        assert!(panic.is_err());
        assert_eq!(entry.value.available, initial.available);
        assert_eq!(entry.value.order_margin, initial.order_margin);
        assert_eq!(entry.dirty, dirty);
    }
}
