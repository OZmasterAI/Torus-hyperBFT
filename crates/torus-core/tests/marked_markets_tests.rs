//! Perf s140 (section 130): `MarkedMarkets`, the RPC intake's grow-only set
//! of markets known marked in front of `oracle::ever_marked`. A marked market
//! is read from the state once; an unmarked one every time (only positive
//! answers are kept), so its first mark is seen at the next check; a read
//! fault is never cached.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use torus_core::oracle::MarkedMarkets;
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId};

/// Counts the aggregate-row point reads (`agg‖market`); fails them while
/// `fail` is set. Everything else delegates.
#[derive(Clone)]
struct CountingAgg {
    inner: StateDb,
    agg_reads: Arc<AtomicU64>,
    fail: Arc<AtomicBool>,
}

impl StateBackend for CountingAgg {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_ORACLE && key.starts_with(b"agg") {
            self.agg_reads.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(StateError::Io(std::io::Error::other("injected aggregate read failure")));
            }
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

fn setup() -> (tempfile::TempDir, CountingAgg) {
    let dir = tempfile::tempdir().unwrap();
    let inner = StateDb::open(dir.path()).unwrap();
    let s = CountingAgg { inner, agg_reads: Arc::default(), fail: Arc::default() };
    (dir, s)
}

impl CountingAgg {
    fn reads(&self) -> u64 {
        self.agg_reads.load(Ordering::SeqCst)
    }
}

/// The aggregate row of `m`: price(16) ‖ block(8) ‖ reporters(4) ‖ ts(8).
fn mark(s: &CountingAgg, m: MarketId, price: i128) {
    let key = [b"agg".as_slice(), &m.to_be_bytes()].concat();
    let row = [
        (price * FixedPoint::SCALE).to_be_bytes().as_slice(),
        &1u64.to_be_bytes(),
        &3u32.to_be_bytes(),
        &1_000u64.to_be_bytes(),
    ]
    .concat();
    s.inner.put_cf_raw(CF_NATIVE_ORACLE, &key, &row).unwrap();
}

#[test]
fn a_marked_market_is_read_from_the_state_once() {
    let (_d, s) = setup();
    mark(&s, 7, 100);
    let set = MarkedMarkets::new();
    for _ in 0..1_000 {
        assert!(set.ever_marked(&s, 7).unwrap());
    }
    assert_eq!(s.reads(), 1, "one aggregate read for 1,000 checks");
    assert_eq!(set.db_reads(), 1);
    assert!(set.contains(7));
}

#[test]
fn an_unmarked_market_is_read_every_time_and_joins_after_its_first_mark() {
    let (_d, s) = setup();
    let set = MarkedMarkets::new();
    for _ in 0..5 {
        assert!(!set.ever_marked(&s, 9).unwrap(), "no row: not marked");
    }
    assert_eq!(s.reads(), 5, "a negative answer is never kept");
    assert!(!set.contains(9));
    // A zero price is no mark, and is not kept either.
    mark(&s, 9, 0);
    assert!(!set.ever_marked(&s, 9).unwrap());
    assert_eq!(s.reads(), 6);
    // The first mark lands: the next check reads it and keeps it.
    mark(&s, 9, 100);
    assert!(set.ever_marked(&s, 9).unwrap(), "seen at the next check");
    assert!(set.ever_marked(&s, 9).unwrap());
    assert_eq!(s.reads(), 7, "then served from the set");
    assert!(set.contains(9));
}

#[test]
fn a_read_fault_is_never_cached() {
    let (_d, s) = setup();
    mark(&s, 3, 100);
    let set = MarkedMarkets::new();
    s.fail.store(true, Ordering::SeqCst);
    assert!(set.ever_marked(&s, 3).is_err());
    assert!(!set.contains(3));
    s.fail.store(false, Ordering::SeqCst);
    assert!(set.ever_marked(&s, 3).unwrap());
    assert!(set.ever_marked(&s, 3).unwrap());
    assert_eq!(s.reads(), 2, "the fault, then one good read");
}

#[test]
fn markets_are_kept_apart_including_ids_past_the_bitmap() {
    let (_d, s) = setup();
    let big = MarkedMarkets::DIRECT_IDS + 5;
    let ids = [0, 1, 63, 64, MarkedMarkets::DIRECT_IDS - 1, MarkedMarkets::DIRECT_IDS, big, u64::MAX];
    for &m in &ids {
        mark(&s, m, 100);
    }
    let set = MarkedMarkets::new();
    for _ in 0..3 {
        for &m in &ids {
            assert!(set.ever_marked(&s, m).unwrap(), "market {m}");
        }
    }
    assert_eq!(s.reads(), ids.len() as u64, "one read per market");
    for &m in &ids {
        assert!(set.contains(m), "market {m}");
    }
    // Neighbours of marked ids are not marked.
    for m in [2, 62, 65, MarkedMarkets::DIRECT_IDS + 1, big - 1, big + 1] {
        assert!(!set.contains(m), "market {m}");
        assert!(!set.ever_marked(&s, m).unwrap(), "market {m}");
    }
}

/// The intake verifies on many threads at once: every check answers right
/// and a marked market costs at most one read per racing thread.
#[test]
fn concurrent_checks_answer_right_and_read_at_most_once_per_thread() {
    const THREADS: u64 = 16;
    let (_d, s) = setup();
    let marked = [1u64, 2, 3, MarkedMarkets::DIRECT_IDS + 1];
    for &m in &marked {
        mark(&s, m, 100);
    }
    let set = Arc::new(MarkedMarkets::new());
    let start = Arc::new(std::sync::Barrier::new(THREADS as usize));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let (s, set, start) = (s.clone(), set.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                for _ in 0..500 {
                    for &m in &marked {
                        assert!(set.ever_marked(&s, m).unwrap(), "market {m}");
                    }
                    assert!(!set.ever_marked(&s, 4).unwrap(), "unmarked market 4");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let unmarked_reads = THREADS * 500;
    let marked_reads = s.reads() - unmarked_reads;
    assert!(
        marked_reads <= THREADS * marked.len() as u64,
        "marked markets read {marked_reads} times (at most one per thread and market)"
    );
    assert_eq!(set.db_reads(), s.reads());
}
