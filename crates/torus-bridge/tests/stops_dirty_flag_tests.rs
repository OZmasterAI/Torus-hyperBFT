//! r8 `stops-dirty-flag-range-scan` — byte-identity witnesses for skipping
//! `diff_stop_rows`' bounded `market ‖ 0x02` prefix seek on dirty markets
//! whose `pending_stops` set did not move since their last successful diff.
//!
//! Contract: for ANY block sequence the persisted state with the skip ON
//! (production) is byte-identical to the state produced when EVERY book is
//! force-marked dirty before EVERY save (`mark_stops_dirty()` — exactly the
//! pre-r8 behaviour, which scanned unconditionally): same root-CF stop /
//! level / meta rows, same node-local order rows, same per-block native state
//! roots, same full-scan oracle, same `save_order_books()` write count, same
//! in-memory books after the save.
//!
//! Cells (all over the same seeded adversarial universe that mixes resting
//! sprays, crossing sweeps, cancels, modifies, cancel-alls and STOP orders —
//! so stop placement, trigger cascades and cancel-all stop removal all fire):
//!   1. mode 1 (OrderRows), resident books;
//!   2. mode 2 (LevelAuthority), resident books;
//!   3. mode 3 (LevelAuthorityChunked, the production mode), resident books;
//!   4. mode 3 with resident books OFF — every block reloads each book from
//!      RocksDB, exercising `restore_stop_row` and the fresh-book "starts
//!      dirty" default;
//!   5. the skip PROVABLY engages: on the non-forced runs most dirty markets
//!      per block are skipped, and after every save every book is clean.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_HASHED, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRIE,
};
use torus_state::native_trie::{
    build_native_trie_to_cf, native_root_full, persisted_native_root, NativeMemberCache,
    NativeTrieCache,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers (same shape as save_books_parallel_tests.rs) ----

const N_MARKETS: u64 = 10;

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

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn order(
    market_id: MarketId,
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    order_type: OrderType,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, fp(price), fp(qty), OrderType::Limit, TimeInForce::GTC)
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn dump_cf(db: &StateDb, cf: &'static str) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, cf, None).expect("iterate cf")
}

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Seeded pseudo-random blocks: resting sprays, crossing sweeps that print
/// through the stop triggers (cascades), cancels, modifies, cancel-alls (the
/// stop-removal path) and stop placement on both sides.
/// `with_stops = false` drops the stop-placement arm (a stop-free universe —
/// the production shape at 300 markets, where the skip should be ~total).
fn stop_heavy_batches(
    blocks: usize,
    seed: u64,
    per_block: usize,
    with_stops: bool,
) -> Vec<Vec<(Address, NativeAction)>> {
    let traders = [addr(1), addr(2), addr(3), addr(4), addr(5), addr(6)];
    let mut s = seed;
    let mut next_id: u128 = 1;
    let mut placed: Vec<u128> = Vec::new();
    let mut out = Vec::with_capacity(blocks);
    for _ in 0..blocks {
        let n = per_block / 2 + (xorshift(&mut s) as usize % per_block.max(1));
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            let t = traders[(xorshift(&mut s) % traders.len() as u64) as usize];
            let market = 1 + xorshift(&mut s) % N_MARKETS;
            let roll = xorshift(&mut s) % 16;
            match roll {
                0..=6 => {
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let price = if is_buy {
                        96 + (xorshift(&mut s) % 4) as i64
                    } else {
                        101 + (xorshift(&mut s) % 4) as i64
                    };
                    let qty = 1 + (xorshift(&mut s) % 5) as i64;
                    batch.push(place(t, gtc(market, is_buy, price, qty)));
                    placed.push(next_id);
                    next_id += 1;
                }
                7..=9 => {
                    // Crossing sweep with a WIDE limit so the print can walk
                    // far enough to fire pending stops (trigger cascades).
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let price = if is_buy { 160 } else { 40 };
                    let qty = 2 + (xorshift(&mut s) % 8) as i64;
                    batch.push(place(t, gtc(market, is_buy, price, qty)));
                    placed.push(next_id);
                    next_id += 1;
                }
                10 => {
                    if let Some(&id) =
                        placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        batch.push((t, NativeAction::CancelOrder { order_id: id }));
                    }
                }
                11 => {
                    if let Some(&id) =
                        placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        let qty_mod = xorshift(&mut s).is_multiple_of(2);
                        batch.push((
                            t,
                            NativeAction::ModifyOrder {
                                order_id: id,
                                new_price: if qty_mod {
                                    None
                                } else {
                                    Some(fp(94 + (xorshift(&mut s) % 6) as i64))
                                },
                                new_qty: if qty_mod { Some(fp(1)) } else { None },
                            },
                        ));
                    }
                }
                12 => {
                    batch.push((t, NativeAction::CancelAllOrders { market_id: None }));
                }
                _ if !with_stops => {
                    // Stop-free universe: fall back to a plain resting order.
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    batch.push(place(t, gtc(market, is_buy, if is_buy { 97 } else { 103 }, 2)));
                    placed.push(next_id);
                    next_id += 1;
                }
                _ => {
                    // Stops on both sides, triggers straddling the print band.
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let trigger = if is_buy {
                        130 + (xorshift(&mut s) % 40) as i64
                    } else {
                        30 + (xorshift(&mut s) % 40) as i64
                    };
                    batch.push(place(
                        t,
                        order(
                            market,
                            is_buy,
                            fp(100),
                            fp(1),
                            OrderType::StopMarket { trigger: fp(trigger) },
                            TimeInForce::GTC,
                        ),
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

/// One CF dump entry: (column family, key, value).
type CfRow = (&'static str, Vec<u8>, Vec<u8>);

struct RunResult {
    roots: Vec<B256>,
    /// Per-block dump of BOTH book CFs.
    book_dumps: Vec<Vec<CfRow>>,
    /// Per-block `save_order_books()` return (root-CF write count).
    written: Vec<usize>,
    /// Per-block, per-market borsh book bytes AFTER the save.
    post_books: Vec<Vec<(MarketId, Vec<u8>)>>,
    /// Per-block (dirty markets, dirty markets whose stop scan was SKIPPED).
    skipped: Vec<(usize, usize)>,
    /// Per-block: were all books clean after the save?
    all_clean_after: Vec<bool>,
    dumps: Vec<CfRow>,
    oracle: B256,
    _dir: tempfile::TempDir,
}

/// One universe. `force_dirty` = pre-r8 behaviour: mark every book's stop set
/// dirty before every save so `diff_stop_rows` always runs its prefix seek.
/// `resident` = keep books in RAM across blocks (false ⇒ reload each block
/// from RocksDB, exercising `restore_stop_row`).
fn run_universe(
    mode: BookMode,
    force_dirty: bool,
    resident: bool,
    with_stops: bool,
    seed: u64,
    blocks: usize,
    per_block: usize,
) -> RunResult {
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
            fund_native(&ctx0, &t, fp(100_000_000));
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    let mut books = ResidentBooks::default();
    let batches = stop_heavy_batches(blocks, seed, per_block, with_stops);
    let mut roots = Vec::new();
    let mut book_dumps = Vec::new();
    let mut written = Vec::new();
    let mut post_books = Vec::new();
    let mut skipped = Vec::new();
    let mut all_clean_after = Vec::new();

    for (i, batch) in batches.iter().enumerate() {
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
            if resident { Some(&mut books) } else { None },
        );
        ctx.save_books_workers = 1;
        ctx.save_books_min_ops = 0;
        assert!(ctx.fatal_error.is_none(), "load {height}: {:?}", ctx.fatal_error);
        let _ = NativeExecutor::execute_batch(&mut ctx, batch);
        assert!(ctx.fatal_error.is_none(), "exec {height}: {:?}", ctx.fatal_error);

        if force_dirty {
            for b in ctx.order_books.values() {
                b.mark_stops_dirty();
            }
        }
        // Count how many of THIS block's dirty markets will skip the seek.
        let n_dirty = ctx.dirty_books.len();
        let n_skipped = ctx
            .dirty_books
            .iter()
            .filter(|m| ctx.order_books.get(m).is_some_and(|b| !b.stops_dirty()))
            .count();
        skipped.push((n_dirty, n_skipped));

        written.push(ctx.save_order_books());
        all_clean_after.push(ctx.order_books.values().all(|b| !b.stops_dirty()));

        let mut post: Vec<(MarketId, Vec<u8>)> = ctx
            .order_books
            .iter()
            .map(|(mid, b)| (*mid, borsh::to_vec(b).expect("book borsh")))
            .collect();
        post.sort_by_key(|(mid, _)| *mid);
        post_books.push(post);

        if resident {
            ctx.stash_resident(&mut books);
        }
        overlay
            .flush_with_native_trie_stats(
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

    let mut dumps = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_BOOK_ORDER_ROWS,
        CF_NATIVE_TRIE,
        CF_NATIVE_HASHED,
    ] {
        for (k, v) in dump_cf(&db, cf) {
            dumps.push((cf, k, v));
        }
    }
    RunResult {
        roots,
        book_dumps,
        written,
        post_books,
        skipped,
        all_clean_after,
        dumps,
        oracle: native_root_full(&db).unwrap(),
        _dir: dir,
    }
}

fn assert_identical(label: &str, base: &RunResult, other: &RunResult) {
    assert_eq!(base.roots.len(), other.roots.len(), "{label}: block count");
    for h in 0..base.roots.len() {
        assert_eq!(base.roots[h], other.roots[h], "{label}: block {} root diverged", h + 1);
        assert_eq!(
            base.book_dumps[h], other.book_dumps[h],
            "{label}: block {} book/level/stop CF bytes diverged",
            h + 1
        );
        assert_eq!(
            base.written[h], other.written[h],
            "{label}: block {} save write count diverged",
            h + 1
        );
        assert_eq!(
            base.post_books[h], other.post_books[h],
            "{label}: block {} post-save in-memory books diverged",
            h + 1
        );
    }
    assert_eq!(base.dumps, other.dumps, "{label}: final CF dumps diverged");
    assert_eq!(base.oracle, other.oracle, "{label}: full-scan oracle diverged");
    assert_eq!(
        *other.roots.last().unwrap(),
        other.oracle,
        "{label}: persisted root != oracle"
    );
}

/// Every stop row that the always-scan run persisted must also be there in
/// the skipping run — a stale stop row (missed delete) or a missing one
/// (missed put) is the whole risk of the flag, so pin it explicitly.
fn stop_rows_of(dump: &[CfRow]) -> Vec<(Vec<u8>, Vec<u8>)> {
    dump.iter()
        .filter(|(cf, k, _)| *cf == CF_NATIVE_ORDER_BOOKS && k.len() == 25 && k[8] == 0x02)
        .map(|(_, k, v)| (k.clone(), v.clone()))
        .collect()
}

fn assert_stop_rows_identical(label: &str, base: &RunResult, other: &RunResult) {
    for h in 0..base.book_dumps.len() {
        let a = stop_rows_of(&base.book_dumps[h]);
        let b = stop_rows_of(&other.book_dumps[h]);
        assert_eq!(a, b, "{label}: block {} STOP rows diverged", h + 1);
    }
    // The universe must actually have persisted stop rows somewhere, or the
    // witness is vacuous.
    let total: usize = base.book_dumps.iter().map(|d| stop_rows_of(d).len()).sum();
    assert!(total > 0, "{label}: no stop rows were ever persisted (vacuous test)");
}

// ============================================================================
// 1-3. Skip vs always-scan, per book mode, resident books
// ============================================================================

#[test]
fn stops_dirty_skip_is_byte_identical_mode1() {
    let seed = 0x570B_D147_0001u64;
    let scan = run_universe(BookMode::OrderRows, true, true, true, seed, 24, 40);
    let skip = run_universe(BookMode::OrderRows, false, true, true, seed, 24, 40);
    assert_stop_rows_identical("mode1", &scan, &skip);
    assert_identical("mode1", &scan, &skip);
}

#[test]
fn stops_dirty_skip_is_byte_identical_mode2() {
    let seed = 0x570B_D147_0002u64;
    let scan = run_universe(BookMode::LevelAuthority, true, true, true, seed, 24, 40);
    let skip = run_universe(BookMode::LevelAuthority, false, true, true, seed, 24, 40);
    assert_stop_rows_identical("mode2", &scan, &skip);
    assert_identical("mode2", &scan, &skip);
}

#[test]
fn stops_dirty_skip_is_byte_identical_mode3() {
    let seed = 0x570B_D147_0003u64;
    let scan = run_universe(BookMode::LevelAuthorityChunked, true, true, true, seed, 24, 40);
    let skip = run_universe(BookMode::LevelAuthorityChunked, false, true, true, seed, 24, 40);
    assert_stop_rows_identical("mode3", &scan, &skip);
    assert_identical("mode3", &scan, &skip);
}

// ============================================================================
// 4. Reload-every-block (no resident books): restore_stop_row + fresh-book
//    "starts dirty" must keep the skipping run byte-identical too.
// ============================================================================

#[test]
fn stops_dirty_skip_is_byte_identical_across_book_reloads() {
    let seed = 0x570B_D147_0004u64;
    let scan = run_universe(BookMode::LevelAuthorityChunked, true, false, true, seed, 20, 40);
    let skip = run_universe(BookMode::LevelAuthorityChunked, false, false, true, seed, 20, 40);
    assert_stop_rows_identical("reload", &scan, &skip);
    assert_identical("reload", &scan, &skip);
    // A book reloaded from RocksDB is conservative: it is dirty on arrival,
    // so nothing can be skipped on a reload block.
    assert!(
        skip.skipped.iter().all(|&(_, s)| s == 0),
        "reloaded books must never skip: {:?}",
        skip.skipped
    );
}

// ============================================================================
// 5. The skip provably engages (resident books) and leaves no book dirty.
// ============================================================================

#[test]
fn stops_dirty_skip_actually_engages() {
    let seed = 0x570B_D147_0005u64;
    let skip = run_universe(BookMode::LevelAuthorityChunked, false, true, true, seed, 24, 40);
    assert!(
        skip.all_clean_after.iter().all(|&c| c),
        "every book must be clean after a successful save: {:?}",
        skip.all_clean_after
    );
    let dirty: usize = skip.skipped.iter().map(|&(d, _)| d).sum();
    let saved: usize = skip.skipped.iter().map(|&(_, s)| s).sum();
    assert!(dirty > 0, "no dirty markets at all — vacuous");
    // This universe is deliberately stop-PATHOLOGICAL (a stop in ~1/8 actions
    // and wide sweeps that fire cascades every block), so only a minority of
    // markets can skip here. It only has to engage at all; the realistic
    // shape is the stop-free cell below.
    assert!(
        saved > 0,
        "the skip never engaged on a stop-heavy universe: {:?}",
        skip.skipped
    );
    // Block 1 can never skip (fresh books start dirty).
    assert_eq!(skip.skipped[0].1, 0, "fresh books must scan once");

    // Always-scan control: nothing is ever skipped there.
    let scan = run_universe(BookMode::LevelAuthorityChunked, true, true, true, seed, 24, 40);
    assert!(
        scan.skipped.iter().all(|&(_, s)| s == 0),
        "force-dirty control must never skip: {:?}",
        scan.skipped
    );
}

/// The production shape: order flow with NO stop orders at all. Every market
/// pays the `market ‖ 0x02` seek exactly ONCE (its first save) and skips it on
/// every later block — that is where the 300-market `stops_ns` goes.
#[test]
fn stop_free_flow_skips_every_scan_after_the_first_block() {
    let seed = 0x570B_D147_0006u64;
    let skip = run_universe(BookMode::LevelAuthorityChunked, false, true, false, seed, 20, 40);
    // Nothing ever placed a stop, so no stop row was ever written.
    assert!(
        skip.book_dumps.iter().all(|d| stop_rows_of(d).is_empty()),
        "the stop-free universe must persist no stop rows"
    );
    // Blocks 3.. : every dirty market skips (all 10 books were touched, and
    // therefore scanned once, in the first blocks).
    for (i, &(dirty, saved)) in skip.skipped.iter().enumerate().skip(2) {
        assert_eq!(
            saved,
            dirty,
            "block {}: {saved}/{dirty} skipped — a stop-free market must never rescan ({:?})",
            i + 1,
            skip.skipped
        );
    }
    // And the total scan count is bounded by the market count, not by blocks.
    let scans: usize = skip.skipped.iter().map(|&(d, s)| d - s).sum();
    assert!(
        scans <= N_MARKETS as usize,
        "{scans} scans for {N_MARKETS} markets over 20 blocks: {:?}",
        skip.skipped
    );

    // The always-scan control pays one seek per dirty market per block, and
    // is byte-identical.
    let scan = run_universe(BookMode::LevelAuthorityChunked, true, true, false, seed, 20, 40);
    assert_identical("stop-free", &scan, &skip);
    let control: usize = scan.skipped.iter().map(|&(d, s)| d - s).sum();
    assert!(
        control > scans * 10,
        "control {control} vs skipping {scans} — the saving is not visible"
    );
}
