//! `TORUS_CKPT_DEFER_BOOKS=1` (p3 merge wait, books): the Classic book CF
//! `CF_NATIVE_ORDER_BOOKS` moves from L's main part into its deferred part,
//! beside positions and balances, so the end-of-block merge copies the book
//! blobs after it hands the main part back to the exec thread.
//!
//! The routing table is read from the env once per process, so these tests
//! are their own test binary (own process) and every test sets the env
//! first ([`defer_books_env`]), before it touches a layer. Public API only.
//!
//! The crate's unit tests assume the flag unset: a shell exporting
//! `TORUS_CKPT_DEFER_BOOKS=1` breaks `split_merge_equals_whole_merge_random`
//! and `deferred_part_blocks_only_deferred_reads` (books are their
//! non-deferred CF), as with the other env knobs.
//!
//! * (a) `split_merge_with_books_deferred_equals_whole_merge`: the pipeline's
//!   split merge equals a whole merge, read back through overlays.
//! * (b) `split_moves_book_rows_to_the_deferred_part`: after a split the main
//!   part holds no book row and the part holds them all.
//! * (c) `book_reads_wait_for_the_deferred_part`: book reads through L wait
//!   for the part (and are counted), markets / sessions reads do not; a lost
//!   part is an error for book reads only.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, sync_channel};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use rocksdb::WriteBatch;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDERS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_SESSIONS,
};
use torus_state::db::StateDb;
use torus_state::{
    CheckpointLayer, DeferredLayer, FrozenPending, LayerPart, NativeStateOverlay, StateBackend,
    StateError,
};

/// Turn the gate on for this process: the flag, Classic books
/// (`TORUS_BOOK_ROWS` unset), resident books on (`TORUS_RESIDENT_BOOKS`
/// unset). Once, before any test touches a layer or a DB: every test calls
/// this first and `Once` holds the others until it is done (edition 2021:
/// `set_var` is safe; no other thread of this process reads the env yet).
fn defer_books_env() {
    static ENV: Once = Once::new();
    ENV.call_once(|| {
        std::env::set_var("TORUS_CKPT_DEFER_BOOKS", "1");
        std::env::remove_var("TORUS_BOOK_ROWS");
        std::env::remove_var("TORUS_RESIDENT_BOOKS");
    });
}

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

/// The CFs of the random test: the three deferred ones first, then three
/// that stay in the main part.
const CFS: [&str; 6] = [
    CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS,
    CF_NATIVE_BALANCES,
    CF_NATIVE_MARKETS,
    CF_SESSIONS,
    CF_NATIVE_ORDERS,
];
const KEYS: u64 = 40;

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
        (0..self.below(max + 1))
            .map(|_| self.next() as u8)
            .collect()
    }
}

fn key(i: u64) -> Vec<u8> {
    // Two-byte prefix groups so prefix scans have something to cut.
    vec![b'k', (i / 8) as u8, (i % 8) as u8]
}

/// Live value per `(CF index in CFS, key)`.
type Model = BTreeMap<(usize, Vec<u8>), Vec<u8>>;

/// About half the keys of every CF in the DB, so an L tombstone (hides the
/// DB value) and an absent key (reads it) answer differently.
fn seed_db(db: &StateDb, rng: &mut Rng, model: &mut Model) {
    for (c, cf) in CFS.iter().enumerate() {
        for i in 0..KEYS {
            if rng.below(2) == 0 {
                let mut v = b"db".to_vec();
                v.extend(rng.bytes(8));
                db.put_cf_raw(cf, &key(i), &v).unwrap();
                model.insert((c, key(i)), v);
            }
        }
    }
}

/// One block: up to 24 writes / tombstones, a third of them on the book CF
/// (blob-sized values there).
fn random_block(db: &StateDb, h: u64, rng: &mut Rng, model: &mut Model) -> Arc<FrozenPending> {
    let o = NativeStateOverlay::new(db.clone());
    for _ in 0..rng.below(25) {
        let c = if rng.below(3) == 0 {
            0
        } else {
            rng.below(CFS.len() as u64) as usize
        };
        let k = key(rng.below(KEYS));
        if rng.below(3) == 0 {
            o.delete_cf_raw(CFS[c], &k).unwrap();
            model.remove(&(c, k));
        } else {
            let v = rng.bytes(if c == 0 { 96 } else { 12 });
            o.put_cf_raw(CFS[c], &k, &v).unwrap();
            model.insert((c, k), v);
        }
    }
    o.freeze(h)
}

/// Every read path the overlay offers, for every key of every CF, equals
/// the model.
fn assert_reads(o: &NativeStateOverlay, model: &Model, what: &str) {
    for (c, cf) in CFS.iter().enumerate() {
        for i in 0..KEYS {
            let k = key(i);
            let want = model.get(&(c, k.clone())).cloned();
            assert_eq!(
                o.get_cf_raw(cf, &k).unwrap(),
                want,
                "{what}: get {cf} {k:?}"
            );
            assert_eq!(
                o.get_cf_len(cf, &k).unwrap(),
                want.as_ref().map(Vec::len),
                "{what}: len {cf} {k:?}"
            );
        }
        let all: Vec<(Vec<u8>, Vec<u8>)> = model
            .range((c, Vec::new())..)
            .take_while(|((mc, _), _)| *mc == c)
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(
            StateBackend::iterate_cf(o, cf, None).unwrap(),
            all,
            "{what}: iterate {cf}"
        );
        let pfx = [b'k', 2u8];
        let in_pfx: Vec<_> = all
            .iter()
            .filter(|(k, _)| k.starts_with(&pfx))
            .cloned()
            .collect();
        assert_eq!(
            StateBackend::iterate_cf(o, cf, Some(&pfx)).unwrap(),
            in_pfx,
            "{what}: prefix {cf}"
        );
        assert_eq!(
            StateBackend::prefix_exists(o, cf, &pfx).unwrap(),
            !in_pfx.is_empty(),
            "{what}: prefix_exists {cf}"
        );
    }
}

/// (a) The end-of-block merge as the exec pipeline runs it (split, main
/// part, deferred part, absorb) leaves the layer a whole merge does, with
/// the book CF deferred, over 300 random blocks: same bytes, entry count
/// and top after every block. Every 50 blocks every key reads back as the
/// model through the whole layer, through the split layer before its part
/// is absorbed (main + a ready deferred part), and after the absorb.
#[test]
fn split_merge_with_books_deferred_equals_whole_merge() {
    defer_books_env();
    let (db, _dir) = temp_db();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut model = Model::new();
    seed_db(&db, &mut rng, &mut model);
    let mut whole = CheckpointLayer::new();
    let mut split = CheckpointLayer::new();
    for h in 1..=300u64 {
        let b = random_block(&db, h, &mut rng, &mut model);
        whole.merge(&b);
        let mut part = split.split_deferred();
        split.merge_part(&b, LayerPart::Main);
        part.merge_part(&b, LayerPart::Deferred);
        if h % 50 == 0 {
            let w = Arc::new(std::mem::take(&mut whole));
            let ow = NativeStateOverlay::with_layers(db.clone(), None, Some(w.clone()), None);
            assert_reads(&ow, &model, &format!("h{h} whole"));
            drop(ow);
            whole = Arc::try_unwrap(w).ok().expect("sole owner");

            let main = Arc::new(std::mem::take(&mut split));
            let d = Arc::new(DeferredLayer::ready(part));
            let os = NativeStateOverlay::with_split_layers(
                db.clone(),
                None,
                Some(main.clone()),
                Some(d.clone()),
                None,
            );
            assert_reads(&os, &model, &format!("h{h} split, before absorb"));
            drop(os);
            assert_eq!(d.take_read_waits().0, 0, "h{h}: a ready part never waits");
            split = Arc::try_unwrap(main).ok().expect("sole owner");
            part = Arc::try_unwrap(d)
                .ok()
                .expect("sole owner")
                .into_layer()
                .expect("ready part");
        }
        split.absorb(part);
        assert_eq!(split.bytes(), whole.bytes(), "h{h}: bytes");
        assert_eq!(split.entry_count(), whole.entry_count(), "h{h}: entries");
        assert_eq!(split.top_height(), whole.top_height(), "h{h}: top");
        if h % 50 == 0 {
            let s = Arc::new(std::mem::take(&mut split));
            let os = NativeStateOverlay::with_layers(db.clone(), None, Some(s.clone()), None);
            assert_reads(&os, &model, &format!("h{h} split, absorbed"));
            drop(os);
            split = Arc::try_unwrap(s).ok().expect("sole owner");
        }
    }
    assert!(whole.entry_count() > 0, "non-vacuous");
}

/// (b) A split moves every book row (writes and tombstones) into the
/// deferred part, with positions and balances; markets and sessions stay
/// in the main part. Each part is written to its own copy of a DB that
/// holds book 3 and session s2, which the block deletes: a part holding
/// the tombstone deletes the row, the other leaves it.
#[test]
fn split_moves_book_rows_to_the_deferred_part() {
    defer_books_env();
    let book = |m: u64| m.to_be_bytes().to_vec();
    let (scratch, _d0) = temp_db();
    let o = NativeStateOverlay::new(scratch.clone());
    o.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(1), &[0xb1u8; 300])
        .unwrap();
    o.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(2), &[0xb2u8; 300])
        .unwrap();
    o.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(3)).unwrap();
    o.put_cf_raw(CF_NATIVE_POSITIONS, b"p1", b"pos").unwrap();
    o.put_cf_raw(CF_NATIVE_BALANCES, b"b1", b"bal").unwrap();
    o.put_cf_raw(CF_NATIVE_MARKETS, &book(1), b"mkt").unwrap();
    o.put_cf_raw(CF_SESSIONS, b"s1", b"ses").unwrap();
    o.delete_cf_raw(CF_SESSIONS, b"s2").unwrap();
    let mut main = CheckpointLayer::new();
    main.merge(&o.freeze(7));
    let whole_bytes = main.bytes();
    let part = main.split_deferred();
    assert_eq!(
        main.bytes() + part.bytes(),
        whole_bytes,
        "the split moves bytes"
    );
    assert_eq!(
        main.entry_count(),
        3,
        "main: markets write, session write + tombstone"
    );
    assert_eq!(
        part.entry_count(),
        5,
        "part: 2 book writes + 1 book tombstone, position, balance"
    );
    assert_eq!(main.top_height(), Some(7));
    assert_eq!(part.top_height(), Some(7));

    let apply = |l: &CheckpointLayer| {
        let (db, dir) = temp_db();
        db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(3), b"old")
            .unwrap();
        db.put_cf_raw(CF_SESSIONS, b"s2", b"old").unwrap();
        let mut batch = WriteBatch::default();
        l.append_to_batch(&db, &mut batch).unwrap();
        db.write(batch).unwrap();
        (db, dir)
    };
    let all = |db: &StateDb, cf: &str| StateBackend::iterate_cf(db, cf, None).unwrap();
    let (main_db, _d1) = apply(&main);
    let (part_db, _d2) = apply(&part);

    assert_eq!(
        all(&main_db, CF_NATIVE_ORDER_BOOKS),
        vec![(book(3), b"old".to_vec())],
        "main holds no book write and no book tombstone"
    );
    assert!(all(&main_db, CF_NATIVE_POSITIONS).is_empty());
    assert!(all(&main_db, CF_NATIVE_BALANCES).is_empty());
    assert_eq!(
        all(&main_db, CF_NATIVE_MARKETS),
        vec![(book(1), b"mkt".to_vec())]
    );
    assert_eq!(
        all(&main_db, CF_SESSIONS),
        vec![(b"s1".to_vec(), b"ses".to_vec())],
        "main: the session write, and its tombstone deleted s2"
    );

    assert_eq!(
        all(&part_db, CF_NATIVE_ORDER_BOOKS),
        vec![(book(1), vec![0xb1u8; 300]), (book(2), vec![0xb2u8; 300])],
        "part: both book writes, and its tombstone deleted book 3"
    );
    assert_eq!(
        all(&part_db, CF_NATIVE_POSITIONS),
        vec![(b"p1".to_vec(), b"pos".to_vec())]
    );
    assert_eq!(
        all(&part_db, CF_NATIVE_BALANCES),
        vec![(b"b1".to_vec(), b"bal".to_vec())]
    );
    assert!(all(&part_db, CF_NATIVE_MARKETS).is_empty());
    assert_eq!(
        all(&part_db, CF_SESSIONS),
        vec![(b"s2".to_vec(), b"old".to_vec())],
        "part holds no session row"
    );
}

/// The markets / sessions reads of test (c) (main-part CFs).
type MainReads = (
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Vec<(Vec<u8>, Vec<u8>)>,
    Option<Vec<u8>>,
    Option<usize>,
    Vec<(Vec<u8>, Vec<u8>)>,
);

fn main_cf_reads(ov: &NativeStateOverlay) -> Result<MainReads, StateError> {
    Ok((
        ov.get_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes())?,
        ov.get_cf_raw(CF_NATIVE_MARKETS, &2u64.to_be_bytes())?,
        StateBackend::iterate_cf(ov, CF_NATIVE_MARKETS, None)?,
        ov.get_cf_raw(CF_SESSIONS, b"s1")?,
        ov.get_cf_len(CF_SESSIONS, b"s1")?,
        StateBackend::iterate_cf(ov, CF_SESSIONS, Some(b"s"))?,
    ))
}

/// (c) L split with its deferred part still being merged: a book point
/// read and a book prefix scan both block until the part is sent, then
/// answer as the whole layer would; each blocked read is counted once.
/// Markets and sessions reads (main part) answer at once and are never
/// counted. Once the part arrived, book reads no longer wait. A part whose
/// merge died is an error for book reads only.
#[test]
fn book_reads_wait_for_the_deferred_part() {
    defer_books_env();
    let book = |m: u64| m.to_be_bytes().to_vec();
    // Every book key is a big-endian market id: 7 zero bytes cover 0..=255.
    const BOOK_PFX: [u8; 7] = [0u8; 7];
    let (db, _dir) = temp_db();
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(1), b"db1")
        .unwrap();
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(3), b"db3")
        .unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, &book(1), b"db-mkt")
        .unwrap();
    let o = NativeStateOverlay::new(db.clone());
    o.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(1), b"L1")
        .unwrap();
    o.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(2), b"L2")
        .unwrap();
    o.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(3)).unwrap();
    o.put_cf_raw(CF_NATIVE_MARKETS, &book(2), b"L-mkt").unwrap();
    o.put_cf_raw(CF_SESSIONS, b"s1", b"L-ses").unwrap();
    let block = o.freeze(5);
    let mut whole = CheckpointLayer::new();
    whole.merge(&block);
    let mut main = CheckpointLayer::new();
    let mut part = main.split_deferred();
    main.merge_part(&block, LayerPart::Main);
    part.merge_part(&block, LayerPart::Deferred);

    // What the whole layer answers.
    let ow = NativeStateOverlay::with_layers(db.clone(), None, Some(Arc::new(whole)), None);
    let want_point = ow.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(1)).unwrap();
    let want_scan = StateBackend::iterate_cf(&ow, CF_NATIVE_ORDER_BOOKS, Some(&BOOK_PFX)).unwrap();
    let want_main = main_cf_reads(&ow).unwrap();
    assert_eq!(want_point, Some(b"L1".to_vec()), "L over the DB");
    assert_eq!(
        want_scan,
        vec![(book(1), b"L1".to_vec()), (book(2), b"L2".to_vec())],
        "L over the DB; L's tombstone hides book 3"
    );
    assert_eq!(want_main.0, Some(b"db-mkt".to_vec()));
    assert_eq!(want_main.1, Some(b"L-mkt".to_vec()));

    let (tx, rx) = sync_channel(1);
    let blocked = Arc::new(AtomicUsize::new(0));
    let seen = blocked.clone();
    let deferred = Arc::new(DeferredLayer::pending(rx).on_block(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    }));
    let ov = NativeStateOverlay::with_split_layers(
        db.clone(),
        None,
        Some(Arc::new(main)),
        Some(deferred.clone()),
        None,
    );

    // Main-part CFs answer while the part is still being merged.
    let (main_tx, main_rx) = channel();
    let reader = ov.clone();
    std::thread::spawn(move || {
        let _ = main_tx.send(main_cf_reads(&reader).map_err(|e| e.to_string()));
    });
    let got_main = main_rx.recv_timeout(Duration::from_secs(5));
    let blocked_before_books = blocked.load(Ordering::SeqCst);
    let settled_before_books = deferred.is_settled();

    // A book point read and a book prefix scan, each on its own thread.
    let (point_tx, point_rx) = channel();
    let reader = ov.clone();
    std::thread::spawn(move || {
        let r = reader.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes());
        let _ = point_tx.send(r.map_err(|e| e.to_string()));
    });
    let (scan_tx, scan_rx) = channel();
    let reader = ov.clone();
    std::thread::spawn(move || {
        let r = StateBackend::iterate_cf(&reader, CF_NATIVE_ORDER_BOOKS, Some(&BOOK_PFX));
        let _ = scan_tx.send(r.map_err(|e| e.to_string()));
    });
    // Both announce they block (bounded: a read that never blocks fails
    // the count below rather than hanging the test).
    let deadline = Instant::now() + Duration::from_secs(5);
    while blocked.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let announced = blocked.load(Ordering::SeqCst);
    let answered_early = (point_rx.try_recv().is_ok(), scan_rx.try_recv().is_ok());
    // Send before asserting, so no reader is left blocked on a failure.
    tx.send(part).unwrap();

    let got_main = got_main
        .expect("markets / sessions reads do not wait for the deferred part")
        .unwrap();
    assert_eq!(
        got_main, want_main,
        "main-part reads answer as the whole layer"
    );
    assert_eq!(blocked_before_books, 0, "no main-part read blocked");
    assert!(
        !settled_before_books,
        "main-part reads did not settle the part"
    );
    assert_eq!(
        announced, 2,
        "the book point read and the book scan both blocked"
    );
    assert_eq!(
        answered_early,
        (false, false),
        "no book read answered before the part arrived"
    );
    assert_eq!(
        point_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        want_point,
        "the point read answers as the whole layer once the part arrived"
    );
    assert_eq!(
        scan_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        want_scan,
        "the prefix scan answers as the whole layer once the part arrived"
    );
    let (waits, time) = deferred.take_read_waits();
    assert_eq!(waits, 2, "each blocked book read counted once");
    assert!(time > Duration::ZERO);

    // Settled: book reads answer at once, uncounted.
    assert_eq!(
        ov.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(2)).unwrap(),
        Some(b"L2".to_vec())
    );
    assert_eq!(
        ov.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(3)).unwrap(),
        None,
        "L's tombstone hides the DB"
    );
    assert_eq!(
        ov.get_cf_len(CF_NATIVE_ORDER_BOOKS, &book(1)).unwrap(),
        Some(2)
    );
    assert_eq!(main_cf_reads(&ov).unwrap(), want_main);
    assert_eq!(deferred.take_read_waits().0, 0, "settled: no more waits");
    assert_eq!(blocked.load(Ordering::SeqCst), 2, "no later read blocked");

    // A lost part (the merge died): book reads are errors, the main-part
    // CFs still answer (here from the DB: the main part is empty).
    let (tx, rx) = sync_channel::<CheckpointLayer>(1);
    drop(tx);
    let lost = NativeStateOverlay::with_split_layers(
        db.clone(),
        None,
        Some(Arc::new(CheckpointLayer::new())),
        Some(Arc::new(DeferredLayer::pending(rx))),
        None,
    );
    assert!(
        lost.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &book(1)).is_err(),
        "lost part: a book point read is an error"
    );
    assert!(
        StateBackend::iterate_cf(&lost, CF_NATIVE_ORDER_BOOKS, Some(&BOOK_PFX)).is_err(),
        "lost part: a book scan is an error"
    );
    assert_eq!(
        lost.get_cf_raw(CF_NATIVE_MARKETS, &book(1)).unwrap(),
        Some(b"db-mkt".to_vec())
    );
    assert_eq!(lost.get_cf_raw(CF_SESSIONS, b"s1").unwrap(), None);
    assert!(StateBackend::iterate_cf(&lost, CF_SESSIONS, None)
        .unwrap()
        .is_empty());
}
