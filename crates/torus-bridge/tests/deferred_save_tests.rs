//! s63 port of item 6a (origin 4298728) — deferred mode-2/3 book save
//! (`save_order_books_deferred` + `apply_deferred_book_save`): byte-identity
//! witnesses.
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
//!   2b. Both modes PIPELINED (depth-1 lag, the exec pipeline's real shape):
//!      block N+1 executes on an overlay layered over frozen(N) BEFORE N's
//!      sidecar is applied/flushed — proves the next block never needs N's
//!      book bytes (resident books reused every block) and that the lagged
//!      worker-side apply still lands byte-identical state.
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Leg {
    /// `save_order_books` on the "exec thread", flush immediately.
    Serial,
    /// Deferred save, sidecar applied + flushed right after the block.
    Deferred,
    /// Deferred save under the exec pipeline's depth-1 lag: block N+1 runs on
    /// `with_parent(frozen(N))` while N (state + sidecar) is still unflushed.
    Pipelined,
}

/// One resident-books universe under `mode`. `Leg::Serial` = the serial
/// `save_order_books` on the "exec thread"; the deferred legs = the item-6a
/// split: `save_order_books_deferred` on the exec thread, then the payload
/// applied into a sidecar overlay against the (previous-heights-durable) DB —
/// the flush worker's view — and folded into the block's flush via
/// `flush_with_sidecar_native_trie_stats`.
fn run_universe(mode: BookMode, leg: Leg, seed: u64) -> RunResult {
    use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
    use torus_state::FrozenPending;
    use torus_bridge::native_executor::DeferredBookSave;

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

    // "W": apply the payload into a sidecar overlay that reads previous
    // heights from the DB (all durable when W runs job N — the pipeline
    // flushes strictly in order), then ONE atomic flush of state + sidecar.
    let flush_job = |height: u64,
                     frozen: std::sync::Arc<FrozenPending>,
                     save: Option<DeferredBookSave>,
                     written: &mut Vec<usize>,
                     roots: &mut Vec<B256>,
                     book_dumps: &mut Vec<Vec<(&'static str, Vec<u8>, Vec<u8>)>>| {
        let sidecar = save.map(|save| {
            let side_overlay = NativeStateOverlay::new(db.clone());
            written.push(apply_deferred_book_save(&side_overlay, None, save));
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
    };

    // Pipelined leg: the handed-off-but-unflushed job (height, frozen, books).
    let mut in_flight: Option<(u64, std::sync::Arc<FrozenPending>, Option<DeferredBookSave>)> =
        None;
    for (i, batch) in batches(20, seed, 40).iter().enumerate() {
        let height = (i + 1) as u64;
        let parent = in_flight.as_ref().map(|(_, f, _)| f.clone());
        let overlay = NativeStateOverlay::with_parent(db.clone(), parent);
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
        if height > 1 {
            assert!(
                ctx.resident_reused(),
                "{leg:?} h{height}: resident books must be reused (never reloaded through a \
                 parent layer that lacks the deferred book bytes)"
            );
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, batch);
        assert!(ctx.fatal_error.is_none(), "exec {height}: {:?}", ctx.fatal_error);

        let save = if leg == Leg::Serial {
            written.push(ctx.save_order_books());
            None
        } else {
            Some(
                ctx.save_order_books_deferred()
                    .expect("level-authority modes must defer"),
            )
        };
        let mut post: Vec<(MarketId, Vec<u8>)> = ctx
            .order_books
            .iter()
            .map(|(mid, b)| (*mid, borsh::to_vec(b).expect("book borsh")))
            .collect();
        post.sort_by_key(|(mid, _)| *mid);
        post_books.push(post);
        ctx.stash_resident(&mut books);
        drop(ctx);
        // As the app's fast path does: the marker rides the overlay so the next
        // block's resident guard reads N through the parent layer.
        StateBackend::put_cf_raw(
            &overlay,
            CF_CONSENSUS_META,
            META_NATIVE_APPLIED_HEIGHT,
            &height.to_be_bytes(),
        )
        .unwrap();
        let frozen = overlay.freeze(height);

        if leg == Leg::Pipelined {
            // Depth 1: W runs job N-1 only now — AFTER E executed N on top of it.
            if let Some((h, f, b)) = in_flight.take() {
                flush_job(h, f, b, &mut written, &mut roots, &mut book_dumps);
            }
            in_flight = Some((height, frozen, save));
        } else {
            flush_job(height, frozen, save, &mut written, &mut roots, &mut book_dumps);
        }
    }
    if let Some((h, f, b)) = in_flight.take() {
        flush_job(h, f, b, &mut written, &mut roots, &mut book_dumps);
    }
    RunResult {
        roots,
        written,
        book_dumps,
        post_books,
        _dir: dir,
    }
}

fn assert_runs_equal(mode: BookMode, leg: Leg, seed: u64) {
    let serial = run_universe(mode, Leg::Serial, seed);
    let deferred = run_universe(mode, leg, seed);
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
    assert_runs_equal(BookMode::LevelAuthority, Leg::Deferred, 0xA11CE);
}

#[test]
fn deferred_save_matches_serial_mode3() {
    assert_runs_equal(BookMode::LevelAuthorityChunked, Leg::Deferred, 0xB0B);
}

#[test]
fn pipelined_deferred_save_matches_serial_mode2() {
    assert_runs_equal(BookMode::LevelAuthority, Leg::Pipelined, 0xC0FFEE);
}

#[test]
fn pipelined_deferred_save_matches_serial_mode3() {
    assert_runs_equal(BookMode::LevelAuthorityChunked, Leg::Pipelined, 0xD00D);
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
