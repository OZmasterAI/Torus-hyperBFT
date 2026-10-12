//! s114 Codex P2: `book_reader::find_by_cloid` reads the order store and the
//! stop rows in separate scans. A block that commits between them may turn a
//! pending stop into the resting order its trigger placed (same trader,
//! same cloid: the cloid stays live throughout). The lookup must still find
//! it, whichever scan the commit lands before. Simulated with a backend that
//! serves the first `switch_at` reads from the pre-commit state and the rest
//! from the post-commit state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use torus_core::book_reader::{find_by_cloid, BookLayout, CloidOrder};
use torus_core::order_book::OrderBook;
use torus_state::backend::AtomicWriteOp;
use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::{StateBackend, StateDb, StateError};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

const C: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0001;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn trader() -> Address {
    Address::new([1; 20])
}

fn params(order_type: OrderType, price: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(price),
        quantity: fp(1),
        order_type,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: Some(C),
    }
}

/// A classic-layout DB holding market 1's book.
fn db_with(book: &OrderBook) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    db.put_cf_raw(
        CF_NATIVE_ORDER_BOOKS,
        &1u64.to_be_bytes(),
        &borsh::to_vec(book).unwrap(),
    )
    .unwrap();
    (dir, db)
}

/// Reads go to `before` until `switch_at` reads have been served, then to
/// `after`: a commit landing between two reads.
#[derive(Clone)]
struct Commit {
    before: StateDb,
    after: StateDb,
    reads: Arc<AtomicUsize>,
    switch_at: usize,
}

impl Commit {
    fn now(&self) -> &StateDb {
        if self.reads.fetch_add(1, Ordering::SeqCst) < self.switch_at {
            &self.before
        } else {
            &self.after
        }
    }
}

impl StateBackend for Commit {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.now().get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, _: &str, _: &[u8], _: &[u8]) -> Result<(), StateError> {
        unreachable!("read-only")
    }
    fn delete_cf_raw(&self, _: &str, _: &[u8]) -> Result<(), StateError> {
        unreachable!("read-only")
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.now().iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, _: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        unreachable!("read-only")
    }
}

#[test]
fn a_stop_that_triggers_between_the_scans_is_still_found() {
    // Before: a pending stop with C. After: the stop fired and its
    // stop-limit order rests with C.
    let mut pending = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
    let stop = OrderType::StopLimit {
        trigger: fp(105),
        limit: fp(90),
    };
    pending.place_order(params(stop, 90), trader(), 1);
    let mut rested = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
    rested.place_order(params(OrderType::Limit, 90), trader(), 2);
    let (_d1, before) = db_with(&pending);
    let (_d2, after) = db_with(&rested);

    let lookup = |switch_at| {
        let backend = Commit {
            before: before.clone(),
            after: after.clone(),
            reads: Arc::new(AtomicUsize::new(0)),
            switch_at,
        };
        let found = find_by_cloid(&backend, &trader(), C, BookLayout::Classic).unwrap();
        (found, backend.reads.load(Ordering::SeqCst))
    };
    let (all_before, reads) = lookup(usize::MAX);
    assert!(matches!(all_before, Some(CloidOrder::Stop(1, _))), "{all_before:?}");
    assert!(matches!(lookup(0).0, Some(CloidOrder::Resting(1, _))));
    for switch_at in 0..=reads {
        assert!(
            lookup(switch_at).0.is_some(),
            "a commit after {switch_at} of {reads} reads hid the live cloid"
        );
    }
}
