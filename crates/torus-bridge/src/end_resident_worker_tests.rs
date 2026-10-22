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
        .map(|cf| rows.dump(cf).unwrap())
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
    let spawned = || {
        torus_state::spawn_count::thread_totals()
            [torus_state::spawn_count::SpawnSite::EndResident as usize]
    };
    let spawned_before = spawned();
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
    // P2-3: one long-lived end-resident thread per holder, not one per
    // block (this thread's count: exact).
    assert_eq!(
        spawned(),
        spawned_before + 1,
        "16 worker blocks, one thread"
    );
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
        end_resident(h, rb, &mut overlay, torus_state::ResidentDelta::default(), true, None);
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
    assert_eq!(count("exec_end_resident_users"), 3.0);
    assert_eq!(count("exec_end_resident_wait"), 3.0, "begin of 2, begin of 3, the final read");
}

/// Item 6 cut 5, pipelined shape: the worker takes the block's delta from the
/// frozen set (`BlockDelta::Frozen`, app.rs's hand-off) and its slot equals
/// the inline slot from the delta taken before the freeze, and the DB, after
/// every block; a value given to `drop_later` is dropped on the worker
/// thread, not the calling one.
#[test]
fn frozen_delta_on_the_worker_equals_the_delta_before_freeze() {
    struct OnDrop(Arc<std::sync::Mutex<Vec<String>>>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            let name = std::thread::current().name().unwrap_or("").to_string();
            self.0.lock().unwrap().push(name);
        }
    }
    let dropped = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (_d1, db_inline) = open_db();
    let (_d2, db_worker) = open_db();
    seed(&db_inline);
    seed(&db_worker);
    let mut inline = ResidentBooks::default();
    let mut worker = ResidentBooks::default();
    for h in 1..=16u64 {
        for (db, holder, mode) in [(&db_inline, &mut inline, Mode::Inline), (&db_worker, &mut worker, Mode::Worker)] {
            let mut overlay = NativeStateOverlay::new(db.clone());
            let mut rb = begin_resident(Some(holder), &mut overlay, h, None);
            assert!(rb.attached(), "block {h}: R attached");
            let t = (h % 9) as u8 + 1;
            overlay.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'w', h as u8]).unwrap();
            overlay.put_cf_raw(CF_NATIVE_BALANCES, &[0x40 + h as u8; 20], b"new").unwrap();
            overlay.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1)).unwrap();
            overlay
                .put_cf_raw(torus_state::cf::CF_CONSENSUS_META, torus_state::cf::META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes())
                .unwrap();
            rb.drop_later(OnDrop(dropped.clone()));
            match mode {
                Mode::Inline => {
                    let delta = overlay.own_pending_delta();
                    let frozen = overlay.freeze(h);
                    end_resident(holder, rb, &mut overlay, delta, true, None);
                    frozen.flush_with_native_trie_stats(db, None, None, None).unwrap();
                }
                Mode::Worker => {
                    let frozen = overlay.freeze(h);
                    end_resident_on_worker(holder, rb, &mut overlay, BlockDelta::Frozen(frozen.clone()), true, None);
                    assert!(holder.rows_in_flight(), "block {h}: end_resident deferred to the worker");
                    frozen.flush_with_native_trie_stats(db, None, None, None).unwrap();
                }
            }
        }
        assert_eq!(dump_db(&db_inline), dump_db(&db_worker), "block {h}: state");
        let want = dump_rows(inline.rows().unwrap());
        assert_eq!(dump_rows(worker.rows().unwrap()), want, "block {h}: worker slot != inline slot");
        assert_eq!(want, dump_db(&db_worker), "block {h}: R != DB");
        assert_eq!(worker.trader_positions_match_rows(), Some(true), "block {h}: decoded positions");
    }
    assert_eq!((inline.rows_builds(), worker.rows_builds()), (1, 1), "R built once");
    let dropped = dropped.lock().unwrap();
    assert_eq!(dropped.len(), 32);
    assert_eq!(dropped.iter().filter(|n| *n == "torus-end-resident").count(), 16, "{dropped:?}");
}

/// Plan Task 2.2 (P7): a live clone of the block's `Users` at the end (a
/// reader kept by mistake) leaves the slot empty, counted like a live clone
/// of R, inline and on the worker (no job deferred); the next block rebuilds
/// R and `Users` from the DB.
#[test]
fn live_clone_of_users_falls_back_to_a_rebuild() {
    for mode in [Mode::Inline, Mode::Worker] {
        let (_d, db) = open_db();
        seed(&db);
        let mut holder = ResidentBooks::default();
        block(&db, &mut holder, 1, mode, None, true, None);
        let mut overlay = NativeStateOverlay::new(db.clone());
        let rb = begin_resident(Some(&mut holder), &mut overlay, 2, None);
        assert!(!rb.rebuilt());
        let leaked = rb.users.clone().expect("the block holds the record set");
        let delta = overlay.own_pending_delta();
        overlay.flush_with_native_trie_and_marker(&db, 2).unwrap();
        match mode {
            Mode::Inline => end_resident(&mut holder, rb, &mut overlay, delta, true, None),
            Mode::Worker => end_resident_on_worker(&mut holder, rb, &mut overlay, delta, true, None),
        }
        assert!(!holder.rows_in_flight(), "{mode:?}: shared Users: nothing deferred");
        assert_eq!(holder.rows_shared_fallbacks(), 1, "{mode:?}");
        assert_eq!(holder.rows_height(), None, "{mode:?}: slot empty");
        drop(leaked);
        assert!(block(&db, &mut holder, 3, mode, None, true, None), "{mode:?}: rebuild");
        assert_eq!(dump_rows(holder.rows().unwrap()), dump_db(&db), "{mode:?}: rebuilt R == DB");
        assert_eq!(holder.trader_positions_match_rows(), Some(true), "{mode:?}");
        assert_eq!(holder.rows_builds(), 2, "{mode:?}");
    }
}

/// Phase 4 step 2: the slot's R holds the liquidation, oracle and market
/// rows raw and reads positions and balances from the slot's `Users` (no raw
/// map, the same record set), inline and on the worker; the gauges split:
/// rows / bytes of R's raw rows, and of the record set (together: R's five
/// CFs as before).
#[test]
fn slot_reads_positions_and_balances_from_users() {
    use torus_state::cf::CF_NATIVE_LIQUIDATION;
    for mode in [Mode::Inline, Mode::Worker] {
        let (_d, db) = open_db();
        seed(&db);
        db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[7u8; 3], b"liq").unwrap();
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let mut holder = ResidentBooks::default();
        for h in 1..=3 {
            block(&db, &mut holder, h, mode, None, true, Some(metrics.clone()));
        }
        let r = holder.rows().unwrap();
        assert!(r.has_source(), "{mode:?}");
        for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
            assert!(r.rows(cf).is_none(), "{mode:?} {cf}: no raw map");
        }
        assert_eq!(dump_rows(r), dump_db(&db), "{mode:?}: R == DB");
        let total = |cfs: &[&str]| -> (i64, i64) {
            cfs.iter()
                .flat_map(|cf| StateBackend::iterate_cf(&db, cf, None).unwrap())
                .fold((0, 0), |(n, b), (k, v)| (n + 1, b + (k.len() + v.len()) as i64))
        };
        let (raw_rows, raw_bytes) = total(&RESIDENT_CFS[2..]);
        let (user_rows, user_bytes) = total(&RESIDENT_CFS[..2]);
        assert_eq!((r.len() as i64, r.bytes() as i64), (raw_rows, raw_bytes), "{mode:?}: R's raw rows");
        assert_eq!(metrics.exec_resident_rows.get(), raw_rows, "{mode:?}");
        assert_eq!(metrics.exec_resident_rows_bytes.get(), raw_bytes, "{mode:?}");
        assert_eq!(metrics.exec_resident_users.get(), user_rows, "{mode:?}");
        assert_eq!(metrics.exec_resident_users_bytes.get(), user_bytes, "{mode:?}");
        let slot = holder.rows.as_ref().unwrap();
        assert_eq!((slot.users.row_count() as i64, slot.users.row_bytes() as i64), (user_rows, user_bytes));
    }
}

// ======================================================================
// s80 end-resident-gate (`TORUS_S80_ER_GATE`): the worker follows the
// frozen set borrowed and drops the block's leftovers after its slot is
// ready.
// ======================================================================

/// One pipelined-shape block at `h` (the delta from the frozen set) ended
/// inline (`None`) or on the worker with the gate off / on; a value given
/// to `drop_later` records the thread it is dropped on.
fn frozen_block(
    db: &StateDb,
    holder: &mut ResidentBooks,
    h: u64,
    gate: Option<bool>,
    dropped: &Arc<std::sync::Mutex<Vec<String>>>,
) {
    struct OnDrop(Arc<std::sync::Mutex<Vec<String>>>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            let name = std::thread::current().name().unwrap_or("").to_string();
            self.0.lock().unwrap().push(name);
        }
    }
    let mut overlay = NativeStateOverlay::new(db.clone());
    let mut rb = begin_resident(Some(holder), &mut overlay, h, None);
    assert!(rb.attached(), "block {h}: R attached");
    let t = (h % 9) as u8 + 1;
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'w', h as u8]).unwrap();
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[0x40 + h as u8; 20], b"new").unwrap();
    overlay.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 2 + h % 3), &[b'q', h as u8]).unwrap();
    overlay.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1)).unwrap();
    overlay.delete_cf_raw(CF_NATIVE_BALANCES, &[0x40 + h as u8 - 1; 20]).unwrap();
    overlay
        .put_cf_raw(torus_state::cf::CF_CONSENSUS_META, torus_state::cf::META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes())
        .unwrap();
    rb.drop_later(OnDrop(dropped.clone()));
    match gate {
        None => {
            let delta = overlay.own_pending_delta();
            let frozen = overlay.freeze(h);
            end_resident(holder, rb, &mut overlay, delta, true, None);
            frozen.flush_with_native_trie_stats(db, None, None, None).unwrap();
        }
        Some(gate) => {
            let frozen = overlay.freeze(h);
            end_resident_on_worker_with(holder, rb, &mut overlay, BlockDelta::Frozen(frozen.clone()), true, None, gate);
            assert!(holder.rows_in_flight(), "block {h}: end_resident deferred to the worker");
            frozen.flush_with_native_trie_stats(db, None, None, None).unwrap();
        }
    }
}

/// s80 end-resident-gate: the worker with the gate on (the frozen set
/// borrowed) makes the slot the inline end and the gate-off worker (the
/// copied delta) make, and R equals the DB, after every block; the block's
/// leftovers are dropped on the worker thread in both worker modes.
#[test]
fn er_gate_worker_slot_equals_the_copied_delta_slot() {
    let dropped = Arc::new(std::sync::Mutex::new(Vec::new()));
    let dbs: Vec<_> = (0..3).map(|_| open_db()).collect();
    for (_, db) in &dbs {
        seed(db);
    }
    let mut holders: Vec<ResidentBooks> = (0..3).map(|_| ResidentBooks::default()).collect();
    let gates = [None, Some(false), Some(true)];
    for h in 1..=16u64 {
        for ((holder, (_, db)), gate) in holders.iter_mut().zip(&dbs).zip(gates) {
            frozen_block(db, holder, h, gate, &dropped);
        }
        let want = dump_rows(holders[0].rows().unwrap());
        assert_eq!(want, dump_db(&dbs[0].1), "block {h}: inline R != DB");
        for (i, holder) in holders.iter_mut().enumerate().skip(1) {
            assert_eq!(dump_rows(holder.rows().unwrap()), want, "block {h}: worker {:?} slot != inline slot", gates[i]);
            assert_eq!(dump_db(&dbs[i].1), dump_db(&dbs[0].1), "block {h}: state");
            assert_eq!(holder.trader_positions_match_rows(), Some(true), "block {h}: {:?} decoded positions", gates[i]);
        }
    }
    for holder in &holders {
        assert_eq!(holder.rows_builds(), 1, "R built once");
        assert_eq!(holder.rows_shared_fallbacks(), 0);
    }
    // Gate on: the last block's leftovers may still be dropping after its
    // slot was joined.
    let deadline = std::time::Instant::now() + DEADLINE;
    while dropped.lock().unwrap().len() < 48 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let dropped = dropped.lock().unwrap();
    assert_eq!(dropped.len(), 48);
    assert_eq!(dropped.iter().filter(|n| *n == "torus-end-resident").count(), 32, "{dropped:?}");
}

/// s80 end-resident-gate: with the gate on, the next access of the rows
/// slot returns while the block's leftovers (a `drop_later` value, the
/// frozen set's handle) are still being dropped on the worker; with it off
/// (today's order: the leftovers first) it waits for that drop. Either way
/// the slot is the block's and the next block reuses it.
#[test]
fn er_gate_slot_is_ready_before_the_leftovers_drop() {
    struct Blocker(mpsc::Sender<()>, mpsc::Receiver<()>);
    impl Drop for Blocker {
        fn drop(&mut self) {
            let _ = self.0.send(());
            let _ = self.1.recv_timeout(DEADLINE);
        }
    }
    for gate in [false, true] {
        let (_d, db) = open_db();
        seed(&db);
        let mut holder = ResidentBooks::default();
        let none = Arc::new(std::sync::Mutex::new(Vec::new()));
        frozen_block(&db, &mut holder, 1, Some(gate), &none);
        let mut overlay = NativeStateOverlay::new(db.clone());
        let mut rb = begin_resident(Some(&mut holder), &mut overlay, 2, None);
        assert!(!rb.rebuilt(), "gate={gate}: block 2 reuses R");
        overlay.put_cf_raw(CF_NATIVE_BALANCES, &[3u8; 20], b"two").unwrap();
        overlay
            .put_cf_raw(torus_state::cf::CF_CONSENSUS_META, torus_state::cf::META_NATIVE_APPLIED_HEIGHT, &2u64.to_be_bytes())
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        rb.drop_later(Blocker(started_tx, release_rx));
        let frozen = overlay.freeze(2);
        end_resident_on_worker_with(&mut holder, rb, &mut overlay, BlockDelta::Frozen(frozen.clone()), true, None, gate);
        frozen.flush_with_native_trie_stats(&db, None, None, None).unwrap();
        drop(frozen);
        started_rx.recv_timeout(DEADLINE).expect("the leftovers' drop never started");
        let (done_tx, done_rx) = mpsc::channel();
        let t = std::thread::spawn(move || {
            let h = holder.rows_height();
            done_tx.send(()).unwrap();
            (holder, h)
        });
        if gate {
            done_rx
                .recv_timeout(DEADLINE / 2)
                .expect("gate on: the slot access waited for the leftovers' drop");
            release_tx.send(()).unwrap();
        } else {
            assert!(done_rx.recv_timeout(HOLD).is_err(), "gate off: the slot came before the leftovers' drop");
            release_tx.send(()).unwrap();
            done_rx.recv_timeout(DEADLINE).expect("gate off: never returned after the release");
        }
        let (mut holder, h) = t.join().unwrap();
        assert_eq!(h, Some(2), "gate={gate}: the slot is block 2's");
        assert_eq!(dump_rows(holder.rows().unwrap()), dump_db(&db), "gate={gate}: R == DB");
        assert_eq!(holder.trader_positions_match_rows(), Some(true), "gate={gate}");
        frozen_block(&db, &mut holder, 3, Some(gate), &none);
        assert_eq!(dump_rows(holder.rows().unwrap()), dump_db(&db), "gate={gate}: R == DB after 3");
        assert_eq!(holder.rows_builds(), 1, "gate={gate}: R built once");
    }
}

/// s80 end-resident-gate: `TORUS_S80_ER_GATE` parses `1` / `true` / `on` as
/// on, anything else (unset included) as off.
#[test]
fn er_gate_flag_parse() {
    for v in ["1", " 1 ", "true", "on"] {
        assert!(parse_er_gate(Some(v)), "{v}");
    }
    for v in [None, Some(""), Some("0"), Some("false"), Some("off"), Some("2"), Some("yes")] {
        assert!(!parse_er_gate(v), "{v:?}");
    }
}
