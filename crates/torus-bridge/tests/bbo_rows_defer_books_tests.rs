//! Merge of main (`TORUS_CKPT_DEFER_BOOKS=1`, the p3 merge wait for Classic
//! book blobs) into item 7 step 2 (B16, the `cf_native_bbo` row per dirty
//! market). With the flag on, Classic resident books' blobs move from L's main
//! part into its deferred part; the bbo rows stay in the main part. Every
//! block's save still writes the bbo row of each dirty market, a read through
//! L answers it while the deferred part is still being merged (no wait), and
//! a checkpoint of L writes it (and an emptied book's tombstone deletes it).
//!
//! Own test binary: `is_deferred_slot` reads the env once per process, so the
//! flag is set before any test touches a layer ([`defer_books_env`]).

use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Once};

use alloy_primitives::Address;
use rocksdb::WriteBatch;
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_BBO, CF_NATIVE_ORDER_BOOKS};
use torus_state::{
    CheckpointLayer, DeferredLayer, LayerPart, NativeStateOverlay, StateBackend, StateDb,
};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const MARKET: MarketId = 1;

/// The flag on, Classic books (`TORUS_BOOK_ROWS` unset), resident books on
/// (`TORUS_RESIDENT_BOOKS` unset). Once, before any layer is touched
/// (edition 2021: `set_var` is safe; no other thread reads the env yet).
fn defer_books_env() {
    static ENV: Once = Once::new();
    ENV.call_once(|| {
        std::env::set_var("TORUS_CKPT_DEFER_BOOKS", "1");
        std::env::remove_var("TORUS_BOOK_ROWS");
        std::env::remove_var("TORUS_RESIDENT_BOOKS");
    });
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn gtc(is_buy: bool, price: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: MARKET,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// The expected row: bid raw ‖ ask raw (i128 BE), 0 for an empty side.
fn row(bid: i64, ask: i64) -> Vec<u8> {
    let raw = |p: i64| if p == 0 { 0i128 } else { fp(p).raw() };
    let mut r = raw(bid).to_be_bytes().to_vec();
    r.extend_from_slice(&raw(ask).to_be_bytes());
    r
}

fn key() -> Vec<u8> {
    MARKET.to_be_bytes().to_vec()
}

/// `layer` written to its own empty DB (as a checkpoint writes L).
fn apply(layer: &CheckpointLayer) -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=64u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut batch = WriteBatch::default();
    layer.append_to_batch(&db, &mut batch).unwrap();
    db.write(batch).unwrap();
    (db, dir)
}

/// A Classic resident-books chain whose blocks run over the DB and L, and
/// whose end-of-block merge is the pipeline's split merge (main part, then the
/// deferred part), as `split_merge_with_books_deferred_equals_whole_merge`.
struct Chain {
    _dir: tempfile::TempDir,
    db: StateDb,
    books: ResidentBooks,
    layer: CheckpointLayer,
    height: u64,
}

/// What one block's split merge left in L's two parts.
struct Split {
    /// The bbo row read through L with the deferred part still pending.
    bbo_through_l: Option<Vec<u8>>,
    /// The bbo CF and the book CF of the main part, written to a DB.
    main_bbo: Vec<(Vec<u8>, Vec<u8>)>,
    main_books: Vec<(Vec<u8>, Vec<u8>)>,
    /// The same of the deferred part.
    part_bbo: Vec<(Vec<u8>, Vec<u8>)>,
    part_books: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Chain {
    fn new() -> Self {
        defer_books_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        // Pre-testnet P0: orders need a listed market (placeholder rows,
        // tick / lot 1, as the old implicit book).
        for m in 1..=64u64 {
            db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
        }
        {
            let ctx0 = NativeExecContext::new_with_mode(
                db.clone(),
                0,
                1000,
                0,
                100,
                10,
                addr(99),
                addr(100),
                addr(101),
                BookMode::Classic,
                None,
            );
            for t in [addr(1), addr(2)] {
                let bal = NativeBalance {
                    available: fp(100_000_000),
                    order_margin: FixedPoint::ZERO,
                };
                ctx0.positions.put_native_balance(&t, &bal).unwrap();
            }
        }
        Self {
            _dir: dir,
            db,
            books: ResidentBooks::default(),
            layer: CheckpointLayer::new(),
            height: 0,
        }
    }

    /// Execute one block over (DB, L), save, then merge it into L the split
    /// way and report both parts.
    fn block(&mut self, actions: &[(Address, NativeAction)]) -> Split {
        self.height += 1;
        let height = self.height;
        let what = format!("h{height}");
        let l = Arc::new(std::mem::take(&mut self.layer));
        let overlay = NativeStateOverlay::with_layers(self.db.clone(), None, Some(l.clone()), None);
        let mut ctx = NativeExecContext::new_with_mode(
            overlay.clone(),
            height,
            1000 + height,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            BookMode::Classic,
            Some(&mut self.books),
        );
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        if height > 1 {
            assert!(ctx.resident_reused(), "{what}: resident books reused");
        }
        let r = NativeExecutor::execute_batch(&mut ctx, actions);
        for (i, res) in r.results.iter().enumerate() {
            assert!(res.success, "{what}: action {i}: {:?}", res.error);
        }
        ctx.save_order_books();
        assert!(
            ctx.fatal_error.is_none(),
            "{what}: save {:?}",
            ctx.fatal_error
        );
        ctx.stash_resident(&mut self.books);
        drop(ctx);
        let frozen = overlay.freeze(height);
        drop(overlay);

        // The pipeline's end-of-block merge: split L, main part first (handed
        // back to the exec thread), the deferred part merged after it.
        let mut main = Arc::try_unwrap(l).ok().expect("sole owner of L");
        let mut part = main.split_deferred();
        main.merge_part(&frozen, LayerPart::Main);
        part.merge_part(&frozen, LayerPart::Deferred);

        // A read through L while the deferred part is still being merged:
        // the bbo CF is in the main part, so it never blocks.
        let (tx, rx) = sync_channel::<CheckpointLayer>(1);
        let main = Arc::new(main);
        let pending = Arc::new(
            DeferredLayer::pending(rx)
                .on_block(|| panic!("a bbo read waited for the deferred part (book blobs)")),
        );
        let reader = NativeStateOverlay::with_split_layers(
            self.db.clone(),
            None,
            Some(main.clone()),
            Some(pending.clone()),
            None,
        );
        let bbo_through_l = StateBackend::get_cf_raw(&reader, CF_NATIVE_BBO, &key()).unwrap();
        drop(reader);
        assert_eq!(pending.take_read_waits().0, 0, "{what}: no wait");
        tx.send(part).unwrap();
        let part = Arc::try_unwrap(pending)
            .ok()
            .expect("sole owner of the part")
            .into_layer()
            .expect("part arrived");
        let main = Arc::try_unwrap(main).ok().expect("sole owner of L");

        let all = |db: &StateDb, cf: &str| StateBackend::iterate_cf(db, cf, None).unwrap();
        let (main_db, _d1) = apply(&main);
        let (part_db, _d2) = apply(&part);
        let split = Split {
            bbo_through_l,
            main_bbo: all(&main_db, CF_NATIVE_BBO),
            main_books: all(&main_db, CF_NATIVE_ORDER_BOOKS),
            part_bbo: all(&part_db, CF_NATIVE_BBO),
            part_books: all(&part_db, CF_NATIVE_ORDER_BOOKS),
        };

        let mut whole = main;
        whole.absorb(part);
        self.layer = whole;
        split
    }

    /// Checkpoint: write L to the DB and start a new L.
    fn checkpoint(&mut self) {
        let mut batch = WriteBatch::default();
        self.layer.append_to_batch(&self.db, &mut batch).unwrap();
        self.db.write(batch).unwrap();
        self.layer = CheckpointLayer::new();
    }

    fn db_bbo(&self) -> Option<Vec<u8>> {
        StateBackend::get_cf_raw(&self.db, CF_NATIVE_BBO, &key()).unwrap()
    }
}

/// bid 99 + ask 101 -> (99, 101) in the main part, the book blob in the
/// deferred part; a bid below the best writes no row; the ask cancelled ->
/// (99, 0); a checkpoint lands the row; both bids cancelled -> a tombstone in
/// the main part, and the next checkpoint deletes the DB row.
#[test]
fn bbo_rows_stay_in_the_main_part_with_book_blobs_deferred() {
    let mut c = Chain::new();

    // Global order ids: the bid is 1, the ask 2.
    let s = c.block(&[(addr(1), gtc(true, 99)), (addr(2), gtc(false, 101))]);
    assert_eq!(s.bbo_through_l, Some(row(99, 101)), "h1: read through L");
    assert_eq!(s.main_bbo, vec![(key(), row(99, 101))], "h1: main part");
    assert!(s.main_books.is_empty(), "h1: no book blob in the main part");
    assert!(s.part_bbo.is_empty(), "h1: no bbo row in the deferred part");
    assert_eq!(
        s.part_books
            .iter()
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>(),
        vec![key()],
        "h1: the flag is on (the book blob is deferred)"
    );

    // A bid below the best: the book blob moves, the top does not.
    let s = c.block(&[(addr(1), gtc(true, 98))]);
    assert_eq!(s.bbo_through_l, Some(row(99, 101)), "h2: read through L");
    assert!(s.part_bbo.is_empty() && s.main_books.is_empty(), "h2");
    assert_eq!(
        s.part_books.len(),
        1,
        "h2: the dirty book's blob is deferred"
    );

    let s = c.block(&[(addr(2), NativeAction::CancelOrder { order_id: 2 })]);
    assert_eq!(s.bbo_through_l, Some(row(99, 0)), "h3: read through L");
    assert_eq!(s.main_bbo, vec![(key(), row(99, 0))], "h3: main part");
    assert!(s.part_bbo.is_empty() && s.main_books.is_empty(), "h3");

    assert_eq!(c.db_bbo(), None, "nothing checkpointed yet");
    c.checkpoint();
    assert_eq!(
        c.db_bbo(),
        Some(row(99, 0)),
        "the checkpoint writes the row"
    );

    let s = c.block(&[
        (addr(1), NativeAction::CancelOrder { order_id: 1 }),
        (addr(1), NativeAction::CancelOrder { order_id: 3 }),
    ]);
    assert_eq!(s.bbo_through_l, None, "h4: the tombstone answers through L");
    assert!(s.main_bbo.is_empty() && s.part_bbo.is_empty(), "h4");
    assert_eq!(c.db_bbo(), Some(row(99, 0)), "h4: not checkpointed yet");
    c.checkpoint();
    assert_eq!(
        c.db_bbo(),
        None,
        "the checkpoint deletes the emptied book's row"
    );
}
