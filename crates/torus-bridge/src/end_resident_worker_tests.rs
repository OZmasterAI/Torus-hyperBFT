//! Item 6 step 2: `end_resident` on a worker thread
//! ([`end_resident_on_worker`]). The worker owns R, the block's delta and its
//! sums; the slot it makes enters the holder only at the next access of the
//! rows slot ([`ResidentBooks::settle_rows`]). Checked here: the slot equals
//! the inline `end_resident`'s after every block; `begin_resident` and every
//! other access (advance, invalidate, introspection, a second end) wait for
//! the worker; the books path (context constructor, stash) does not; a
//! worker panic leaves the slot empty and the next block rebuilds; failed /
//! shared ends spawn nothing; the timers are observed once per block.

use super::*;
use std::sync::mpsc;
use std::time::Duration;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::resident_rows::RESIDENT_CFS;
use torus_state::ResidentRows;

/// How long a blocked call is given to (wrongly) return before the gate opens.
const HOLD: Duration = Duration::from_millis(150);
/// Upper bound for anything that must finish (a hang fails instead of hanging).
const DEADLINE: Duration = Duration::from_secs(20);

fn open_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn pos_key(t: u8, m: u64) -> Vec<u8> {
    [&[t; 20][..], &m.to_be_bytes()].concat()
}

fn seed(db: &StateDb) {
    for t in 1..=9u8 {
        db.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'b', t]).unwrap();
        db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1), &[b'p', t]).unwrap();
    }
}

type Dump = Vec<Vec<(Vec<u8>, Vec<u8>)>>;

fn dump_rows(rows: &ResidentRows) -> Dump {
    RESIDENT_CFS
        .iter()
        .map(|cf| rows.rows(cf).unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect()
}

fn dump_db(db: &StateDb) -> Dump {
    RESIDENT_CFS.iter().map(|cf| StateBackend::iterate_cf(db, cf, None).unwrap()).collect()
}

/// A hook that reports the worker started, then waits for the release.
/// Returns (release, started, hook).
fn gate() -> (mpsc::Sender<()>, mpsc::Receiver<()>, WorkerHook) {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let hook = WorkerHook(Box::new(move || {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(DEADLINE).expect("test never released the worker");
    }));
    (release_tx, started_rx, hook)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Inline,
    Worker,
}

/// One serial block at `h`: begin, write (a balance of trader `h`, a new
/// balance row, delete trader `h`'s position), flush with the marker, end
/// inline or on the worker (with `hook`, if any). Returns whether R was rebuilt.
fn block(
    db: &StateDb,
    holder: &mut ResidentBooks,
    h: u64,
    mode: Mode,
    hook: Option<WorkerHook>,
    ok: bool,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
) -> bool {
    let mut overlay = NativeStateOverlay::new(db.clone());
    let mut rb = begin_resident(Some(holder), &mut overlay, h, None);
    assert!(rb.attached(), "block {h}: R attached");
    rb.worker_hook = hook;
    let t = (h % 9) as u8 + 1;
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'w', h as u8]).unwrap();
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[0x40 + h as u8; 20], b"new").unwrap();
    overlay.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1)).unwrap();
    let delta = overlay.own_pending_delta();
    overlay.flush_with_native_trie_and_marker(db, h).unwrap();
    let rebuilt = rb.rebuilt();
    match mode {
        Mode::Inline => end_resident(holder, rb, &mut overlay, delta, ok, metrics.as_deref()),
        Mode::Worker => end_resident_on_worker(holder, rb, &mut overlay, delta, ok, metrics),
    }
    rebuilt
}

/// Run `f` on the holder in another thread while the worker is held: it must
/// not return before the gate opens; then it must. Returns the holder and
/// `f`'s result.
fn blocked_until_release<R: Send + 'static>(
    holder: ResidentBooks,
    release: mpsc::Sender<()>,
    started: mpsc::Receiver<()>,
    what: &str,
    f: impl FnOnce(&mut ResidentBooks) -> R + Send + 'static,
) -> (ResidentBooks, R) {
    started.recv_timeout(DEADLINE).expect("worker never started");
    let (done_tx, done_rx) = mpsc::channel();
    let t = std::thread::spawn(move || {
        let mut holder = holder;
        let r = f(&mut holder);
        done_tx.send(()).unwrap();
        (holder, r)
    });
    assert!(
        done_rx.recv_timeout(HOLD).is_err(),
        "{what}: returned while the end_resident worker was still running"
    );
    release.send(()).unwrap();
    done_rx.recv_timeout(DEADLINE).unwrap_or_else(|_| panic!("{what}: never returned after the release"));
    let (holder, r) = t.join().unwrap();
    assert!(!holder.rows_in_flight(), "{what}: worker joined");
    (holder, r)
}

/// The worker's slot == the inline slot == the DB after every block, with
/// the job really deferred (in flight right after the end), joined by the
/// next `begin_resident` on even blocks and by an introspection call on odd
/// ones; R built once in both.
#[test]
fn worker_slot_equals_inline_slot_after_every_block() {
    let (_d1, db_inline) = open_db();
    let (_d2, db_worker) = open_db();
    seed(&db_inline);
    seed(&db_worker);
    let mut inline = ResidentBooks::default();
    let mut worker = ResidentBooks::default();
    for h in 1..=16 {
        block(&db_inline, &mut inline, h, Mode::Inline, None, true, None);
        block(&db_worker, &mut worker, h, Mode::Worker, None, true, None);
        assert!(!inline.rows_in_flight());
        assert!(worker.rows_in_flight(), "block {h}: end_resident deferred to the worker");
        assert_eq!(dump_db(&db_inline), dump_db(&db_worker), "block {h}: state");
        if h % 2 == 1 {
            let want = dump_rows(inline.rows().unwrap());
            assert_eq!(dump_rows(worker.rows().unwrap()), want, "block {h}: worker slot != inline slot");
            assert_eq!(want, dump_db(&db_worker), "block {h}: R != DB");
            assert_eq!(worker.rows_height(), Some(h));
            assert_eq!(worker.trader_positions_match_rows(), Some(true), "block {h}: decoded positions");
        }
    }
    assert_eq!(dump_rows(worker.rows().unwrap()), dump_rows(inline.rows().unwrap()));
    assert_eq!((inline.rows_builds(), worker.rows_builds()), (1, 1), "R built once");
    assert_eq!(worker.rows_shared_fallbacks(), 0);
}

/// Ordering: `begin_resident` of the next block waits for the worker, then
/// REUSES its slot (block 2's writes visible through the overlay, no
/// rebuild). Taking the slot before the join would rebuild (or, with a
/// slot left in the holder, serve block 1's rows) — this test fails then.
#[test]
fn begin_resident_waits_for_the_worker_and_reuses_its_slot() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    block(&db, &mut holder, 1, Mode::Worker, None, true, None);
    let (release, started, hook) = gate();
    block(&db, &mut holder, 2, Mode::Worker, Some(hook), true, None);
    assert!(holder.rows_in_flight());
    let db2 = db.clone();
    let (holder, (rebuilt, bal, pos)) = blocked_until_release(holder, release, started, "begin_resident", move |holder| {
        let mut overlay = NativeStateOverlay::new(db2.clone());
        let rb = begin_resident(Some(holder), &mut overlay, 3, None);
        assert!(rb.attached());
        let bal = StateBackend::get_cf_raw(&overlay, CF_NATIVE_BALANCES, &[3u8; 20]).unwrap();
        let pos = StateBackend::get_cf_raw(&overlay, CF_NATIVE_POSITIONS, &pos_key(3, 1)).unwrap();
        (rb.rebuilt(), bal, pos)
    });
    assert!(!rebuilt, "the worker's slot (height 2) is reused, not rebuilt");
    assert_eq!(bal, Some(vec![b'w', 2]), "block 2's balance write is in R");
    assert_eq!(pos, None, "block 2's position delete is in R");
    assert_eq!(holder.rows_builds(), 1);
}

/// Every other access of the rows slot waits for the worker too, with the
/// inline semantics after it: an untouched block advances the slot, an
/// invalidation drops it, the introspection methods see it, a second end
/// replaces it; no worker is left behind.
#[test]
fn every_rows_access_waits_for_the_worker() {
    type Access = Box<dyn FnOnce(&mut ResidentBooks) -> Option<u64> + Send>;
    let cases: Vec<(&str, Access, Option<u64>)> = vec![
        ("advance_untouched", Box::new(|h: &mut ResidentBooks| {
            h.advance_untouched_with(true, 3);
            h.rows_height()
        }), Some(3)),
        ("advance_untouched (books switch off)", Box::new(|h: &mut ResidentBooks| {
            h.advance_untouched_with(false, 3);
            h.rows_height()
        }), Some(3)),
        ("advance_untouched (not the successor)", Box::new(|h: &mut ResidentBooks| {
            h.advance_untouched_with(true, 4);
            h.rows_height()
        }), None),
        ("invalidate", Box::new(|h: &mut ResidentBooks| {
            h.invalidate();
            h.rows_height()
        }), None),
        ("rows_height", Box::new(|h: &mut ResidentBooks| h.rows_height()), Some(2)),
        ("rows", Box::new(|h: &mut ResidentBooks| h.rows().map(|r| r.len() as u64).map(|_| 2)), Some(2)),
        ("trader_positions_match_rows", Box::new(|h: &mut ResidentBooks| {
            assert_eq!(h.trader_positions_match_rows(), Some(true));
            h.rows_height()
        }), Some(2)),
        ("settle_rows", Box::new(|h: &mut ResidentBooks| {
            h.settle_rows();
            h.rows_height()
        }), Some(2)),
    ];
    for (what, access, want) in cases {
        let (_d, db) = open_db();
        seed(&db);
        let mut holder = ResidentBooks::default();
        block(&db, &mut holder, 1, Mode::Worker, None, true, None);
        let (release, started, hook) = gate();
        block(&db, &mut holder, 2, Mode::Worker, Some(hook), true, None);
        let (mut holder, got) = blocked_until_release(holder, release, started, what, access);
        assert_eq!(got, want, "{what}");
        if want.is_some() {
            assert_eq!(dump_rows(holder.rows().unwrap()), dump_db(&db), "{what}: R == DB");
        }
    }
    // A second end (a harness ending twice without a begin) joins the first.
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    block(&db, &mut holder, 1, Mode::Worker, None, true, None);
    let (release, started, hook) = gate();
    block(&db, &mut holder, 2, Mode::Worker, Some(hook), true, None);
    let db2 = db.clone();
    let (mut holder, ()) = blocked_until_release(holder, release, started, "second end", move |h| {
        let mut overlay = NativeStateOverlay::new(db2.clone());
        let rb = begin_resident(None, &mut overlay, 3, None);
        end_resident(h, rb, &mut overlay, Default::default(), true, None);
    });
    assert_eq!(holder.rows_height(), Some(2), "the R-less end leaves the joined slot");
}

/// Lock: the books path (context constructor taking / rebuilding the books,
/// stash) never waits for the worker, so a mutex around the holder held by
/// it cannot deadlock with the worker (which never takes it).
#[test]
fn books_path_proceeds_while_the_worker_runs() {
    let (_d, db) = open_db();
    seed(&db);
    let holder = std::sync::Mutex::new(ResidentBooks::default());
    block(&db, &mut holder.lock().unwrap(), 1, Mode::Worker, None, true, None);
    let (release, started, hook) = gate();
    block(&db, &mut holder.lock().unwrap(), 2, Mode::Worker, Some(hook), true, None);
    started.recv_timeout(DEADLINE).expect("worker never started");
    let holder = Arc::new(holder);
    let (done_tx, done_rx) = mpsc::channel();
    let (h2, db2) = (holder.clone(), db.clone());
    let t = std::thread::spawn(move || {
        for h in [3u64, 4] {
            let mut guard = h2.lock().unwrap();
            let overlay = NativeStateOverlay::new(db2.clone());
            let mut ctx = NativeExecContext::new_with_mode(
                overlay,
                h,
                1_000,
                0,
                1_000,
                10,
                Address::ZERO,
                Address::ZERO,
                Address::ZERO,
                BookMode::Classic,
                Some(&mut guard),
            );
            ctx.save_order_books();
            ctx.stash_resident(&mut guard);
            assert!(guard.rows_in_flight(), "the books path joined the worker");
        }
        done_tx.send(()).unwrap();
    });
    done_rx.recv_timeout(DEADLINE).expect("books path blocked behind the end_resident worker");
    t.join().unwrap();
    release.send(()).unwrap();
    let mut guard = holder.lock().unwrap();
    assert_eq!(guard.rows_height(), Some(2));
    assert_eq!(dump_rows(guard.rows().unwrap()), dump_db(&db));
}

/// A panic in the worker leaves the slot empty (no deadlock, nothing
/// poisoned); the next block rebuilds R, which equals the DB.
#[test]
fn worker_panic_leaves_the_slot_empty_and_the_next_block_rebuilds() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    block(&db, &mut holder, 1, Mode::Worker, None, true, None);
    let hook = WorkerHook(Box::new(|| panic!("planted end_resident worker panic (test)")));
    block(&db, &mut holder, 2, Mode::Worker, Some(hook), true, None);
    assert_eq!(holder.rows_height(), None, "panicked worker: slot empty");
    assert!(!holder.rows_in_flight());
    assert!(block(&db, &mut holder, 3, Mode::Worker, None, true, None), "block 3 rebuilds R");
    assert_eq!(holder.rows_builds(), 2);
    assert_eq!(dump_rows(holder.rows().unwrap()), dump_db(&db), "rebuilt R carried through 3 == DB");
    assert_eq!(holder.rows_height(), Some(3));
    // The same holder behind a mutex is not poisoned by the worker's panic.
    let m = std::sync::Mutex::new(holder);
    block(&db, &mut m.lock().unwrap(), 4, Mode::Worker, Some(WorkerHook(Box::new(|| panic!("again (test)")))), true, None);
    assert_eq!(m.lock().unwrap().rows_height(), None);
    assert!(!m.is_poisoned());
}

/// A failed block (`ok = false`) and a live clone of R spawn no worker: the
/// slot is empty at once (the clone counted), as inline.
#[test]
fn failed_or_shared_end_spawns_no_worker() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    block(&db, &mut holder, 1, Mode::Worker, None, true, None);
    block(&db, &mut holder, 2, Mode::Worker, None, false, None);
    assert!(!holder.rows_in_flight(), "failed block: nothing deferred");
    assert_eq!(holder.rows_height(), None);
    assert!(block(&db, &mut holder, 3, Mode::Worker, None, true, None), "rebuild after the failed block");

    let mut overlay = NativeStateOverlay::new(db.clone());
    let rb = begin_resident(Some(&mut holder), &mut overlay, 4, None);
    let leaked = overlay.clone();
    let delta = overlay.own_pending_delta();
    overlay.flush_with_native_trie_and_marker(&db, 4).unwrap();
    end_resident_on_worker(&mut holder, rb, &mut overlay, delta, true, None);
    assert!(!holder.rows_in_flight(), "shared R: nothing deferred");
    assert_eq!(holder.rows_shared_fallbacks(), 1);
    assert_eq!(holder.rows_height(), None);
    drop(leaked);
    assert!(block(&db, &mut holder, 5, Mode::Worker, None, true, None));
    assert_eq!(dump_rows(holder.rows().unwrap()), dump_db(&db));
}

/// Timers: the worker observes `exec_end_resident_seconds` (and its two
/// subs) once per block; the join observes `exec_end_resident_wait_seconds`
/// once per worker joined.
#[test]
fn worker_and_join_observe_their_timers_once_per_block() {
    let (_d, db) = open_db();
    seed(&db);
    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut holder = ResidentBooks::default();
    for h in 1..=3 {
        block(&db, &mut holder, h, Mode::Worker, None, true, Some(metrics.clone()));
    }
    assert_eq!(holder.rows_height(), Some(3)); // joins block 3's worker
    let text = metrics.encode();
    let count = |name: &str| {
        let key = format!("torus_{name}_seconds_count ");
        text.lines()
            .find_map(|l| l.strip_prefix(&key).and_then(|v| v.trim().parse::<f64>().ok()))
            .unwrap_or_else(|| panic!("{name} missing:\n{text}"))
    };
    assert_eq!(count("exec_end_resident"), 3.0);
    assert_eq!(count("exec_end_resident_rows"), 3.0);
    assert_eq!(count("exec_end_resident_positions"), 3.0);
    assert_eq!(count("exec_end_resident_wait"), 3.0, "begin of 2, begin of 3, the final read");
}
