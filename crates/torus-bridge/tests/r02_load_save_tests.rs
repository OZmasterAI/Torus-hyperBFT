//! R02 branch 7 (audit main-chain-deep-review-2026-10-08, inventory section
//! (4) item 7): the order-book load and save paths. A LOCAL storage fault
//! must fail-stop the block (`take_fatal_error`, the committer's check),
//! never become ordinary state:
//! - load: the classic whole-book scan (was: no books, next id 1), the
//!   `__book_mode__` marker read (was: no marker, check skipped, marker
//!   rewritten), the `__next_global_order_id__` read (was: book-maxima scan,
//!   order-id reuse);
//! - the resident books' applied-marker guard: a failed marker read means
//!   "stale" (rebuild from persisted state), never "no marker" (reuse);
//! - save (every book mode): whole-book / order-row / node-local order-row /
//!   level-row / stop-row / meta writes, the stop-row scan of the diff, the
//!   next-id counter and mode-marker writes (were: logged, block went on).
//!
//! Absence and the deterministic decode policies are unchanged (an
//! undecodable classic blob is skipped, a wrong-length marker / counter row
//! reads as absent): the controls below pass before and after.

use std::sync::{Arc, Mutex};

use alloy_primitives::Address;

use torus_bridge::native_executor::{
    apply_deferred_book_save, BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::book_rows::{book_meta_key, ROW_TAG_STOP};
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_CONSENSUS_META, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Armed `(cf, key)`; key `None` = every key of the CF.
type Armed = Arc<Mutex<Vec<(&'static str, Option<Vec<u8>>)>>>;

/// A backend whose point reads (`reads`), scans (`scans`, matched on the
/// prefix; `None` = every scan of the CF) or writes / deletes (`writes`) of
/// the armed rows fail; `hits` counts the injected failures.
#[derive(Clone, Default)]
struct Failing {
    inner: Option<StateDb>,
    reads: Armed,
    scans: Armed,
    writes: Armed,
    hits: Arc<Mutex<usize>>,
}

const INJECTED: &str = "injected book storage failure";

impl Failing {
    fn new(db: &StateDb) -> Self {
        Self {
            inner: Some(db.clone()),
            ..Self::default()
        }
    }
    fn db(&self) -> &StateDb {
        self.inner.as_ref().unwrap()
    }
    fn arm_read(&self, cf: &'static str, key: &[u8]) {
        self.reads.lock().unwrap().push((cf, Some(key.to_vec())));
    }
    fn arm_scan(&self, cf: &'static str, prefix: Option<&[u8]>) {
        self.scans
            .lock()
            .unwrap()
            .push((cf, prefix.map(<[u8]>::to_vec)));
    }
    fn arm_write(&self, cf: &'static str, key: Option<&[u8]>) {
        self.writes
            .lock()
            .unwrap()
            .push((cf, key.map(<[u8]>::to_vec)));
    }
    fn hits(&self) -> usize {
        *self.hits.lock().unwrap()
    }
    fn hit(&self, list: &Armed, cf: &str, key: Option<&[u8]>) -> Result<(), StateError> {
        let armed = list
            .lock()
            .unwrap()
            .iter()
            .any(|(c, k)| *c == cf && (k.is_none() || k.as_deref() == key));
        if armed {
            *self.hits.lock().unwrap() += 1;
            return Err(StateError::Io(std::io::Error::other(INJECTED)));
        }
        Ok(())
    }
}

impl StateBackend for Failing {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.hit(&self.reads, cf, Some(key))?;
        self.db().get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.hit(&self.writes, cf, Some(key))?;
        self.db().put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.hit(&self.writes, cf, Some(key))?;
        self.db().delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        // A scan armed with a prefix fails for exactly that prefix; armed
        // without one, every scan of the CF fails.
        let armed = self
            .scans
            .lock()
            .unwrap()
            .iter()
            .any(|(c, k)| *c == cf && (k.is_none() || k.as_deref() == prefix));
        if armed {
            *self.hits.lock().unwrap() += 1;
            return Err(StateError::Io(std::io::Error::other(INJECTED)));
        }
        self.db().iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.db().atomic_write(ops)
    }
}

const NOW: u64 = 1_001;
const M: MarketId = 1;
const NEXT_ID_KEY: &[u8] = b"__next_global_order_id__";
const MODE_MARKER_KEY: &[u8] = b"__book_mode__";

const MODES: [BookMode; 4] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];
const ROW_MODES: [BookMode; 3] = [
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];
const LEVEL_MODES: [BookMode; 2] = [BookMode::LevelAuthority, BookMode::LevelAuthorityChunked];

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

type Ctx = NativeExecContext<Failing>;

fn open() -> (tempfile::TempDir, StateDb, Failing) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let backend = Failing::new(&db);
    (dir, db, backend)
}

fn ctx_at(
    backend: &Failing,
    height: u64,
    mode: BookMode,
    resident: Option<&mut ResidentBooks>,
) -> Ctx {
    NativeExecContext::new_with_mode(
        backend.clone(),
        height,
        NOW,
        0,
        1_000,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        resident,
    )
}

fn gtc(trader: u8, is_buy: bool, price: i64, qty: i64) -> (Address, NativeAction) {
    (
        addr(trader),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: M,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

/// Trader 1's stop-limit buy (trigger 120, limit 121): a pending stop row.
fn stop(trader: u8) -> (Address, NativeAction) {
    (
        addr(trader),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: M,
            is_buy: true,
            price: fp(121),
            quantity: fp(1),
            order_type: OrderType::StopLimit {
                trigger: fp(120),
                limit: fp(121),
            },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

/// Block 1 in `mode`: traders 1..=3 funded, a bid (trader 1) and an ask
/// (trader 2) rest on market 1, plus `extra`. Nothing saved yet.
fn block1(
    backend: &Failing,
    mode: BookMode,
    resident: Option<&mut ResidentBooks>,
    extra: &[(Address, NativeAction)],
) -> Ctx {
    let mut ctx = ctx_at(backend, 1, mode, resident);
    assert_eq!(ctx.take_fatal_error(), None, "{mode:?}: clean load");
    for t in 1..=3 {
        ctx.positions
            .put_native_balance(
                &addr(t),
                &NativeBalance {
                    available: fp(1_000_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
    }
    let mut actions = vec![gtc(1, true, 99, 2), gtc(2, false, 101, 3)];
    actions.extend_from_slice(extra);
    for (s, a) in &actions {
        let r = NativeExecutor::execute(&mut ctx, s, a);
        assert!(r.success, "{mode:?}: {a:?}: {:?}", r.error);
    }
    assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
    ctx
}

/// Block 1 saved fault-free; returns the DB.
fn seeded(mode: BookMode) -> (tempfile::TempDir, StateDb, Failing) {
    let (d, db, backend) = open();
    let mut ctx = block1(&backend, mode, None, &[]);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None, "{mode:?}: clean save");
    (d, db, backend)
}

/// The block fail-stops on the injected fault, recorded by `step`.
#[track_caller]
fn assert_injected(reason: Option<String>, step: &str, what: &str) {
    let reason = reason.unwrap_or_else(|| panic!("{what}: must fail-stop"));
    assert!(
        reason.starts_with(step),
        "{what}: recorded by {step}: {reason}"
    );
    assert!(reason.contains(INJECTED), "{what}: {reason}");
}

// ---------------------------------------------------------------------------
// Load.
// ---------------------------------------------------------------------------

/// The classic loader's scan of `cf_native_order_books` fails. RED before
/// R02: the node started with no books (next id 1), the block went on.
#[test]
fn r02_classic_load_scan_fault_fail_stops() {
    let (_d, _db, backend) = seeded(BookMode::Classic);
    backend.arm_scan(CF_NATIVE_ORDER_BOOKS, None);
    let mut ctx = ctx_at(&backend, 2, BookMode::Classic, None);
    assert!(backend.hits() > 0, "fault injected");
    assert_injected(ctx.take_fatal_error(), "book load", "classic scan");
}

/// The `__book_mode__` marker read fails. RED before R02: read as "no
/// marker" (check skipped, marker rewritten at the next save).
#[test]
fn r02_book_mode_marker_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend) = seeded(mode);
        backend.arm_read(CF_NATIVE_MARKETS, MODE_MARKER_KEY);
        let mut ctx = ctx_at(&backend, 2, mode, None);
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_injected(
            ctx.take_fatal_error(),
            "book load: book-mode marker read",
            &format!("{mode:?}"),
        );
    }
}

/// The `__next_global_order_id__` read fails. RED before R02: read as "no
/// counter row" (book-maxima scan: order-id reuse once the books drain).
#[test]
fn r02_next_order_id_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend) = seeded(mode);
        backend.arm_read(CF_NATIVE_MARKETS, NEXT_ID_KEY);
        let mut ctx = ctx_at(&backend, 2, mode, None);
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_injected(
            ctx.take_fatal_error(),
            "book load: next_global_order_id read",
            &format!("{mode:?}"),
        );
    }
}

/// Control: a fault-free reload sees the saved books and counter; nothing
/// fail-stops.
#[test]
fn r02_load_without_fault_unchanged() {
    for mode in MODES {
        let (_d, _db, backend) = seeded(mode);
        let mut ctx = ctx_at(&backend, 2, mode, None);
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(backend.hits(), 0);
        assert_eq!(ctx.resting_order_count(), 2, "{mode:?}");
        assert_eq!(ctx.next_global_order_id, 3, "{mode:?}");
    }
}

/// Decode policies kept (item 8, owner's call): an undecodable classic
/// whole-book blob is skipped (no book), a wrong-length mode marker or
/// counter row reads as absent. None of them fail-stops.
#[test]
fn r02_load_decode_policies_unchanged() {
    // Undecodable classic blob: the market has no book.
    let (_d, db, backend) = seeded(BookMode::Classic);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &M.to_be_bytes(), b"garbage")
        .unwrap();
    let mut ctx = ctx_at(&backend, 2, BookMode::Classic, None);
    assert_eq!(ctx.take_fatal_error(), None, "undecodable blob: skipped");
    assert_eq!(ctx.resting_order_count(), 0);
    assert_eq!(ctx.next_global_order_id, 3, "the counter row still holds");

    // Wrong-length marker / counter rows read as absent.
    for mode in MODES {
        let (_d, db, backend) = seeded(mode);
        db.put_cf_raw(CF_NATIVE_MARKETS, MODE_MARKER_KEY, b"xx")
            .unwrap();
        db.put_cf_raw(CF_NATIVE_MARKETS, NEXT_ID_KEY, b"short")
            .unwrap();
        let mut ctx = ctx_at(&backend, 2, mode, None);
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(ctx.next_global_order_id, 3, "{mode:?}: book-maxima scan");
    }
}

// ---------------------------------------------------------------------------
// Resident books: the applied-marker guard.
// ---------------------------------------------------------------------------

/// Block 1 in resident mode, saved and stashed; returns the holder.
fn resident_block1(backend: &Failing, mode: BookMode) -> ResidentBooks {
    let mut holder = ResidentBooks::default();
    let mut ctx = block1(backend, mode, Some(&mut holder), &[]);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    ctx.stash_resident(&mut holder);
    holder
}

/// The applied-marker read fails at block 2. RED before R02: read as "no
/// marker", which passes the staleness guard (the holder was reused).
#[test]
fn r02_resident_applied_marker_read_fault_rebuilds() {
    for mode in MODES {
        let (_d, _db, backend) = open();
        let mut holder = resident_block1(&backend, mode);
        backend.arm_read(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT);
        let mut ctx = ctx_at(&backend, 2, mode, Some(&mut holder));
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert!(
            !ctx.resident_reused(),
            "{mode:?}: stale -> rebuilt from persisted state"
        );
        assert_eq!(
            ctx.take_fatal_error(),
            None,
            "{mode:?}: the rebuild read the DB fine"
        );
        assert_eq!(ctx.resting_order_count(), 2, "{mode:?}");
    }
}

/// Control: no marker row (test DBs) and no fault: the holder is reused,
/// as today; a marker that disagrees rebuilds, as today.
#[test]
fn r02_resident_applied_marker_without_fault_unchanged() {
    for mode in MODES {
        let (_d, db, backend) = open();
        let mut holder = resident_block1(&backend, mode);
        let ctx = ctx_at(&backend, 2, mode, Some(&mut holder));
        assert!(ctx.resident_reused(), "{mode:?}: reused");
        drop(ctx);

        let (_d2, db2, backend2) = open();
        let mut holder = resident_block1(&backend2, mode);
        db2.put_cf_raw(
            CF_CONSENSUS_META,
            META_NATIVE_APPLIED_HEIGHT,
            &7u64.to_be_bytes(),
        )
        .unwrap();
        let ctx = ctx_at(&backend2, 2, mode, Some(&mut holder));
        assert!(!ctx.resident_reused(), "{mode:?}: marker mismatch rebuilds");
        let _ = db;
    }
}

// ---------------------------------------------------------------------------
// Save.
// ---------------------------------------------------------------------------

/// Block 1 with `arm` applied right before the save; returns the fail-stop.
fn save_with(
    mode: BookMode,
    extra: &[(Address, NativeAction)],
    arm: impl Fn(&Failing),
) -> (Option<String>, usize) {
    let (_d, _db, backend) = open();
    let mut ctx = block1(&backend, mode, None, extra);
    arm(&backend);
    ctx.save_order_books();
    (ctx.take_fatal_error(), backend.hits())
}

/// Classic: the whole-book put fails. RED before R02: logged only.
#[test]
fn r02_classic_book_write_fault_fail_stops() {
    let (fatal, hits) = save_with(BookMode::Classic, &[], |b| {
        b.arm_write(CF_NATIVE_ORDER_BOOKS, Some(&M.to_be_bytes()))
    });
    assert!(hits > 0, "fault injected");
    assert_injected(fatal, "book save", "classic");
}

/// Every row write of `cf_native_order_books` fails: the order rows (mode
/// 1) / the level rows (modes 2/3). RED before R02: logged only.
#[test]
fn r02_book_row_write_fault_fail_stops() {
    for mode in ROW_MODES {
        let (fatal, hits) = save_with(mode, &[], |b| b.arm_write(CF_NATIVE_ORDER_BOOKS, None));
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

/// Modes 2/3: the node-local order-row write fails. RED before R02.
#[test]
fn r02_node_local_order_row_write_fault_fail_stops() {
    for mode in LEVEL_MODES {
        let (fatal, hits) = save_with(mode, &[], |b| b.arm_write(CF_BOOK_ORDER_ROWS, None));
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

/// Modes 1-3: the meta row put fails. RED before R02.
#[test]
fn r02_meta_row_write_fault_fail_stops() {
    for mode in ROW_MODES {
        let (fatal, hits) = save_with(mode, &[], |b| {
            b.arm_write(CF_NATIVE_ORDER_BOOKS, Some(&book_meta_key(M)))
        });
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

fn stop_prefix() -> Vec<u8> {
    let mut p = M.to_be_bytes().to_vec();
    p.push(ROW_TAG_STOP);
    p
}

/// Modes 1-3: the stop-row diff's scan fails. RED before R02: read as "no
/// persisted stops" (stale rows never deleted, every stop re-put).
#[test]
fn r02_stop_rows_scan_fault_fail_stops() {
    for mode in ROW_MODES {
        let (fatal, hits) = save_with(mode, &[], |b| {
            b.arm_scan(CF_NATIVE_ORDER_BOOKS, Some(&stop_prefix()))
        });
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

/// Modes 1-3: a pending stop's row put fails. RED before R02.
#[test]
fn r02_stop_row_write_fault_fail_stops() {
    for mode in ROW_MODES {
        let (fatal, hits) = save_with(mode, &[stop(1)], |b| {
            let mut key = stop_prefix();
            key.extend_from_slice(&3u128.to_be_bytes());
            b.arm_write(CF_NATIVE_ORDER_BOOKS, Some(&key));
        });
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

/// Every mode: the order-id counter put fails. RED before R02: logged only
/// (a restart then reads an older counter: order-id reuse).
#[test]
fn r02_next_order_id_write_fault_fail_stops() {
    for mode in MODES {
        let (fatal, hits) = save_with(mode, &[], |b| {
            b.arm_write(CF_NATIVE_MARKETS, Some(NEXT_ID_KEY))
        });
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

/// Every mode: the first `__book_mode__` marker put fails. RED before R02.
#[test]
fn r02_book_mode_marker_write_fault_fail_stops() {
    for mode in MODES {
        let (fatal, hits) = save_with(mode, &[], |b| {
            b.arm_write(CF_NATIVE_MARKETS, Some(MODE_MARKER_KEY))
        });
        assert!(hits > 0, "{mode:?}: fault injected");
        assert_injected(fatal, "book save", &format!("{mode:?}"));
    }
}

/// Deferred save (modes 2/3, pipelined): the counter / marker puts it makes
/// on the exec thread fail. RED before R02: logged only.
#[test]
fn r02_deferred_save_counter_and_marker_write_faults_fail_stop() {
    for mode in LEVEL_MODES {
        for key in [NEXT_ID_KEY, MODE_MARKER_KEY] {
            let (_d, _db, backend) = open();
            let mut ctx = block1(&backend, mode, None, &[]);
            backend.arm_write(CF_NATIVE_MARKETS, Some(key));
            assert!(
                ctx.save_order_books_deferred().is_some(),
                "{mode:?}: deferred"
            );
            assert!(backend.hits() > 0, "{mode:?}: fault injected");
            assert_injected(
                ctx.take_fatal_error(),
                "book save",
                &format!("{mode:?} {key:?}"),
            );
        }
    }
}

/// Deferred save (modes 2/3, pipelined): the flush worker's pass 2 (order
/// rows, level rows, stop diff, meta) fails. Before R02 it returned a write
/// count and logged; now the first failure is the `Err` (exec_pipeline maps
/// it to a failed job: W fail-stops, nothing of the block lands). GREEN-only
/// test: the `Result` return is the fix.
#[test]
fn r02_deferred_book_apply_write_fault_is_err() {
    let arms: [(&str, fn(&Failing)); 4] = [
        ("node-local order row", |b| {
            b.arm_write(CF_BOOK_ORDER_ROWS, None)
        }),
        ("level row", |b| b.arm_write(CF_NATIVE_ORDER_BOOKS, None)),
        ("stop rows scan", |b| {
            b.arm_scan(CF_NATIVE_ORDER_BOOKS, Some(&stop_prefix()))
        }),
        ("meta row", |b| {
            b.arm_write(CF_NATIVE_ORDER_BOOKS, Some(&book_meta_key(M)))
        }),
    ];
    for mode in LEVEL_MODES {
        for (what, arm) in arms {
            let (_d, _db, backend) = open();
            let mut ctx = block1(&backend, mode, None, &[]);
            let save = ctx.save_order_books_deferred().expect("deferred");
            assert_eq!(
                ctx.take_fatal_error(),
                None,
                "{mode:?} {what}: exec-thread half clean"
            );
            arm(&backend);
            let e = apply_deferred_book_save(&backend, None, save).expect_err(what);
            assert!(backend.hits() > 0, "{mode:?} {what}: fault injected");
            assert!(e.starts_with("book save"), "{mode:?} {what}: {e}");
            assert!(e.contains(INJECTED), "{mode:?} {what}: {e}");
        }
    }
}

/// Control: the deferred apply without a fault writes what the serial save
/// writes (same count, same rows).
#[test]
fn r02_deferred_book_apply_without_fault_unchanged() {
    for mode in LEVEL_MODES {
        let (_d, db_a, backend_a) = open();
        let mut a = block1(&backend_a, mode, None, &[stop(1)]);
        let serial = a.save_order_books();
        let (_d2, db_b, backend_b) = open();
        let mut b = block1(&backend_b, mode, None, &[stop(1)]);
        let save = b.save_order_books_deferred().expect("deferred");
        let deferred = apply_deferred_book_save(&backend_b, None, save).expect("no fault");
        assert_eq!(serial, deferred, "{mode:?}: write count");
        for cf in [CF_NATIVE_ORDER_BOOKS, CF_BOOK_ORDER_ROWS, CF_NATIVE_MARKETS] {
            assert_eq!(
                db_a.iterate_cf(cf, None).unwrap(),
                db_b.iterate_cf(cf, None).unwrap(),
                "{mode:?} {cf}"
            );
        }
    }
}

/// Control: a fault-free save writes the same rows as before and a reload
/// sees them; the stop row lands in modes 1-3; nothing fail-stops.
#[test]
fn r02_save_without_fault_unchanged() {
    for mode in MODES {
        let (_d, db, backend) = open();
        let mut ctx = block1(&backend, mode, None, &[stop(1)]);
        let written = ctx.save_order_books();
        assert!(written > 0, "{mode:?}");
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(backend.hits(), 0);
        if mode != BookMode::Classic {
            assert_eq!(
                db.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&stop_prefix()))
                    .unwrap()
                    .len(),
                1,
                "{mode:?}: the stop row"
            );
        }
        let mut again = ctx_at(&backend, 2, mode, None);
        assert_eq!(again.take_fatal_error(), None, "{mode:?}");
        assert_eq!(again.resting_order_count(), 2, "{mode:?}");
        assert_eq!(again.next_global_order_id, 4, "{mode:?}");
    }
}
