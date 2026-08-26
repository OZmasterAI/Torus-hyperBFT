//! Item 6a — deferred mode-2/3 book save (`save_order_books_deferred` +
//! `apply_deferred_book_save`): byte-identity witnesses.
//!
//! Contract: splitting the save at the drain/write boundary — pass 1 (journal
//! drain) on the exec thread, pass 2 (order rows, level rows, stop diff, meta)
//! applied later from the returned payload against a SIDECAR overlay that is
//! folded into the block's flush — produces byte-identical persisted state to
//! the serial `save_order_books`: per-block persisted native roots, both book
//! CFs, the root-CF write counts, and the in-memory post-books the next block
//! starts from. The stop-row and meta snapshots inside the payload are what
//! makes the worker-side apply independent of the (by then mutated) live book.
//!
//! Cells:
//!   1. Mode 2 (LevelAuthority, sponge cache on) over a seeded adversarial
//!      multi-market universe, serial vs deferred — everything equal.
//!   2. Mode 3 (LevelAuthorityChunked) — same.
//!   3. Classic / OrderRows: `save_order_books_deferred` returns `None` with
//!      zero side effects (the caller falls back to the serial save).

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    apply_deferred_book_save, BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};
use torus_state::native_trie::{
    build_native_trie_to_cf, persisted_native_root, NativeMemberCache, NativeTrieCache,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

const N_MARKETS: u64 = 5;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn order(
    market_id: MarketId,
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    order_type: OrderType,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Seeded pseudo-random blocks: resting sprays on deep-ish levels, crossing
/// sweeps, cancels, modifies, cancel-alls, stops — every save-path row kind.
fn batches(blocks: usize, seed: u64, per_block: usize) -> Vec<Vec<(Address, NativeAction)>> {
    let traders = [addr(1), addr(2), addr(3), addr(4), addr(5), addr(6)];
    let mut s = seed;
    let mut placed: Vec<u128> = Vec::new();
    let mut next_id: u128 = 1;
    let mut out = Vec::with_capacity(blocks);
    for _ in 0..blocks {
        let n = per_block / 2 + (xorshift(&mut s) as usize % per_block.max(1));
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            let t = traders[(xorshift(&mut s) % traders.len() as u64) as usize];
            let market = 1 + xorshift(&mut s) % N_MARKETS;
            match xorshift(&mut s) % 12 {
                0..=6 => {
                    let is_buy = xorshift(&mut s) % 2 == 0;
                    let price = if is_buy {
                        96 + (xorshift(&mut s) % 4) as i64
                    } else {
                        101 + (xorshift(&mut s) % 4) as i64
                    };
                    let qty = 1 + (xorshift(&mut s) % 5) as i64;
                    batch.push((
                        t,
                        NativeAction::PlaceOrder(order(
                            market,
                            is_buy,
                            fp(price),
                            fp(qty),
                            OrderType::Limit,
                        )),
                    ));
                    placed.push(next_id);
                    next_id += 1;
                }
                7 | 8 => {
                    let is_buy = xorshift(&mut s) % 2 == 0;
                    let price = if is_buy { 105 } else { 95 };
                    let qty = 2 + (xorshift(&mut s) % 8) as i64;
                    batch.push((
                        t,
                        NativeAction::PlaceOrder(order(
                            market,
                            is_buy,
                            fp(price),
                            fp(qty),
                            OrderType::Limit,
                        )),
                    ));
                    placed.push(next_id);
                    next_id += 1;
                }
                9 => {
                    if let Some(&id) =
                        placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        batch.push((t, NativeAction::CancelOrder { order_id: id }));
                    }
                }
                10 => {
                    if xorshift(&mut s) % 4 == 0 {
                        batch.push((t, NativeAction::CancelAllOrders { market_id: None }));
                    } else if let Some(&id) =
                        placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        batch.push((
                            t,
                            NativeAction::ModifyOrder {
                                order_id: id,
                                new_price: None,
                                new_qty: Some(fp(1)),
                            },
                        ));
                    }
                }
                _ => {
                    batch.push((
                        t,
                        NativeAction::PlaceOrder(order(
                            market,
                            true,
                            fp(100),
                            fp(1),
                            OrderType::StopMarket { trigger: fp(150) },
                        )),
                    ));
                    placed.push(next_id);
                    next_id += 1;
                }
            }
        }
        out.push(batch);
    }
    out
}

fn dump_cf(db: &StateDb, cf: &'static str) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, cf, None).expect("iterate cf")
}

struct RunResult {
    roots: Vec<B256>,
    written: Vec<usize>,
    book_dumps: Vec<Vec<(&'static str, Vec<u8>, Vec<u8>)>>,
    post_books: Vec<Vec<(MarketId, Vec<u8>)>>,
    _dir: tempfile::TempDir,
}

/// One resident-books universe under `mode`. `deferred: false` = the serial
/// `save_order_books` on the "exec thread"; `true` = the item-6a split:
/// `save_order_books_deferred` on the exec thread, then the payload applied
/// into a sidecar overlay against the (previous-heights-durable) DB — the
/// flush worker's view — and folded into the block's flush via
/// `flush_with_sidecar_native_trie_stats`.
fn run_universe(mode: BookMode, deferred: bool, seed: u64) -> RunResult {
    let (dir, db) = open_test_db();
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
            mode,
            None,
        );
        for t in [addr(1), addr(2), addr(3), addr(4), addr(5), addr(6)] {
            let bal = NativeBalance {
                available: fp(100_000_000),
                order_margin: FixedPoint::ZERO,
            };
            ctx0.positions.put_native_balance(&t, &bal).unwrap();
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    let mut books = ResidentBooks::default();
    let mut roots = Vec::new();
    let mut written = Vec::new();
    let mut book_dumps = Vec::new();
    let mut post_books = Vec::new();
    for (i, batch) in batches(20, seed, 40).iter().enumerate() {
        let height = (i + 1) as u64;
        let overlay = NativeStateOverlay::new(db.clone());
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
            mode,
            Some(&mut books),
        );
        assert!(ctx.fatal_error.is_none(), "load {height}: {:?}", ctx.fatal_error);
        let _ = NativeExecutor::execute_batch(&mut ctx, batch);
        assert!(ctx.fatal_error.is_none(), "exec {height}: {:?}", ctx.fatal_error);

        let save = if deferred {
            let save = ctx
                .save_order_books_deferred()
                .expect("level-authority modes must defer");
            Some(save)
        } else {
            written.push(ctx.save_order_books());
            None
        };
        let mut post: Vec<(MarketId, Vec<u8>)> = ctx
            .order_books
            .iter()
            .map(|(mid, b)| (*mid, borsh::to_vec(b).expect("book borsh")))
            .collect();
        post.sort_by_key(|(mid, _)| *mid);
        post_books.push(post);
        ctx.stash_resident(&mut books);
        let frozen = overlay.freeze(height);

        // "W": the sidecar overlay reads previous heights from the DB (all
        // durable here — the pipeline flushes strictly in order), collects
        // pass-2 writes, and joins the block's one atomic flush.
        let sidecar = save.map(|save| {
            let side_overlay = NativeStateOverlay::new(db.clone());
            written.push(apply_deferred_book_save(&side_overlay, None, &save));
            side_overlay.freeze(height)
        });
        frozen
            .flush_with_sidecar_native_trie_stats(
                sidecar.as_deref(),
                &db,
                Some(height),
                None::<&mut NativeTrieCache>,
                None::<&mut NativeMemberCache>,
            )
            .expect("flush");

        roots.push(persisted_native_root(&db).unwrap());
        let mut per_block = Vec::new();
        for cf in [CF_NATIVE_ORDER_BOOKS, CF_BOOK_ORDER_ROWS] {
            for (k, v) in dump_cf(&db, cf) {
                per_block.push((cf, k, v));
            }
        }
        book_dumps.push(per_block);
    }
    RunResult {
        roots,
        written,
        book_dumps,
        post_books,
        _dir: dir,
    }
}

fn assert_runs_equal(mode: BookMode, seed: u64) {
    let serial = run_universe(mode, false, seed);
    let deferred = run_universe(mode, true, seed);
    assert_eq!(
        serial.roots, deferred.roots,
        "per-block persisted native roots diverged ({mode:?}, seed {seed})"
    );
    assert_eq!(
        serial.written, deferred.written,
        "root-CF write counts diverged ({mode:?}, seed {seed})"
    );
    for (h, (a, b)) in serial
        .book_dumps
        .iter()
        .zip(deferred.book_dumps.iter())
        .enumerate()
    {
        assert_eq!(a, b, "book CF bytes diverged at height {} ({mode:?})", h + 1);
    }
    assert_eq!(
        serial.post_books, deferred.post_books,
        "in-memory post-books diverged ({mode:?}, seed {seed})"
    );
    // Not vacuous: the universe persisted book rows.
    assert!(
        serial.book_dumps.last().unwrap().len() > 20,
        "vacuous universe ({mode:?}): only {} book rows",
        serial.book_dumps.last().unwrap().len()
    );
}

#[test]
fn deferred_save_matches_serial_mode2() {
    assert_runs_equal(BookMode::LevelAuthority, 0xA11CE);
}

#[test]
fn deferred_save_matches_serial_mode3() {
    assert_runs_equal(BookMode::LevelAuthorityChunked, 0xB0B);
}

/// Classic / OrderRows have no two-pass save: `save_order_books_deferred`
/// must return `None` and leave the overlay untouched (the caller then runs
/// the serial `save_order_books`, i.e. exact-today behavior).
#[test]
fn deferred_save_declines_classic_and_order_rows() {
    for mode in [BookMode::Classic, BookMode::OrderRows] {
        let (_dir, db) = open_test_db();
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
                mode,
                None,
            );
            let bal = NativeBalance {
                available: fp(1_000_000),
                order_margin: FixedPoint::ZERO,
            };
            ctx0.positions.put_native_balance(&addr(1), &bal).unwrap();
        }
        let overlay = NativeStateOverlay::new(db.clone());
        let mut ctx = NativeExecContext::new_with_mode(
            overlay.clone(),
            1,
            1001,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            mode,
            None,
        );
        let batch = vec![(
            addr(1),
            NativeAction::PlaceOrder(order(1, true, fp(100), fp(1), OrderType::Limit)),
        )];
        let _ = NativeExecutor::execute_batch(&mut ctx, &batch);
        let before = overlay.pending_write_count();
        assert!(
            ctx.save_order_books_deferred().is_none(),
            "{mode:?} must decline the deferred save"
        );
        assert_eq!(
            overlay.pending_write_count(),
            before,
            "{mode:?}: declining must have zero side effects"
        );
    }
}
