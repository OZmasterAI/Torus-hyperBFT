//! Mode-2 (`TORUS_BOOK_ROWS=2`) parallel save-books drain
//! (`TORUS_SAVE_BOOKS_WORKERS`) — byte-identity witnesses.
//!
//! Contract: for ANY worker count the mode-2 `save_order_books` produces
//! byte-identical persisted state — root-CF level/meta/stop rows, node-local
//! order rows, per-block native state roots, the full-scan oracle — AND
//! identical in-memory book bytes / level-hash-cache statistics after every
//! save, AND the same `written` count. The parallel drain only changes which
//! thread computes each book's (pure) journal drain; the write pass is the
//! serial market-ascending sequence in every configuration.
//!
//! Cells:
//!   1. workers ∈ {1, 2, 3, 4, 8, 64} over a 12-market seeded adversarial
//!      universe (30 blocks, resident books, cache ON) — all pairwise equal
//!      to the serial (workers = 1) run; the parallel runs PROVABLY engaged
//!      (`last_save_workers() >= 2` on every multi-book block).
//!   2. Same with the level-hash cache OFF (`level_hash_cache_bytes = 0`).
//!   3. Alternating workers per block within ONE run (1,4,1,8,2,…) equals the
//!      all-serial run — the post-save book state after a parallel drain is
//!      indistinguishable from a serial one for the NEXT block.
//!   4. Work gate: below `save_books_min_ops` the serial path runs
//!      (`last_save_workers() == 1`); with the gate at 0 the parallel path
//!      runs; a single dirty book is always serial.

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

// ---- Helpers ----

const N_MARKETS: u64 = 12;

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

/// Seeded pseudo-random blocks over `N_MARKETS` markets: resting sprays
/// (deep-ish levels), crossing sweeps (front pops), cancels, both modify
/// kinds, cancel-alls, stops. Deliberately UNEVEN across markets (market
/// weight ∝ id) so LPT chunking is exercised with skewed journals.
fn adversarial_batches(blocks: usize, seed: u64, per_block: usize) -> Vec<Vec<(Address, NativeAction)>> {
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
            // Skew: pick market by a triangular-ish draw so high ids get more.
            let a = xorshift(&mut s) % N_MARKETS;
            let b = xorshift(&mut s) % N_MARKETS;
            let market = 1 + a.max(b);
            let roll = xorshift(&mut s) % 12;
            match roll {
                0..=6 => {
                    let is_buy = xorshift(&mut s) % 2 == 0;
                    // Few distinct prices ⇒ deep levels (many orders/level).
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
                7 | 8 => {
                    // crossing sweep (fills best levels, maybe rests)
                    let is_buy = xorshift(&mut s) % 2 == 0;
                    let price = if is_buy { 105 } else { 95 };
                    let qty = 2 + (xorshift(&mut s) % 8) as i64;
                    batch.push(place(t, gtc(market, is_buy, price, qty)));
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
                    if let Some(&id) =
                        placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        let qty_mod = xorshift(&mut s) % 2 == 0;
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
                _ => {
                    if xorshift(&mut s) % 4 == 0 {
                        batch.push((t, NativeAction::CancelAllOrders { market_id: None }));
                    } else {
                        batch.push(place(
                            t,
                            order(
                                market,
                                true,
                                fp(100),
                                fp(1),
                                OrderType::StopMarket { trigger: fp(150) },
                                TimeInForce::GTC,
                            ),
                        ));
                        placed.push(next_id);
                        next_id += 1;
                    }
                }
            }
        }
        out.push(batch);
    }
    out
}

struct RunResult {
    /// Per-block persisted native root.
    roots: Vec<B256>,
    /// Per-block dump of BOTH book CFs.
    book_dumps: Vec<Vec<(&'static str, Vec<u8>, Vec<u8>)>>,
    /// Per-block `save_order_books()` return (root-CF write count).
    written: Vec<usize>,
    /// Per-block, per-market (borsh book bytes, level-hash-cache stats) AFTER
    /// the save — the in-memory post-state the next block starts from.
    post_books: Vec<Vec<(MarketId, Vec<u8>, Option<(u64, u64, u64, usize)>)>>,
    /// Per-block worker count the save actually used.
    workers_used: Vec<usize>,
    /// Final full dumps across all interesting CFs.
    dumps: Vec<(&'static str, Vec<u8>, Vec<u8>)>,
    oracle: B256,
    /// The universe DB (kept alive with its tempdir) for post-run reloads.
    _dir: tempfile::TempDir,
    db: StateDb,
}

/// One mode-2 universe (resident books, serial flush) with the save drain
/// worker cap chosen PER BLOCK by `workers_for(height)`. `cache_mb` = level
/// hash cache budget (0 = off). `min_ops` = parallel work gate.
fn run_universe(
    mode: BookMode,
    workers_for: &dyn Fn(u64) -> usize,
    min_ops: usize,
    cache_mb: usize,
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
    let batches = adversarial_batches(blocks, seed, per_block);
    let mut roots = Vec::new();
    let mut book_dumps = Vec::new();
    let mut written = Vec::new();
    let mut post_books = Vec::new();
    let mut workers_used = Vec::new();

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
            Some(&mut books),
        );
        ctx.level_hash_cache_bytes = cache_mb * 1024 * 1024;
        ctx.save_books_workers = workers_for(height);
        ctx.save_books_min_ops = min_ops;
        assert!(ctx.fatal_error.is_none(), "load {height}: {:?}", ctx.fatal_error);
        let _ = NativeExecutor::execute_batch(&mut ctx, batch);
        assert!(ctx.fatal_error.is_none(), "exec {height}: {:?}", ctx.fatal_error);
        written.push(ctx.save_order_books());
        workers_used.push(ctx.last_save_workers());
        let mut post: Vec<(MarketId, Vec<u8>, Option<(u64, u64, u64, usize)>)> = ctx
            .order_books
            .iter()
            .map(|(mid, b)| (*mid, borsh::to_vec(b).expect("book borsh"), b.level_hash_cache_stats()))
            .collect();
        post.sort_by_key(|(mid, _, _)| *mid);
        post_books.push(post);
        ctx.stash_resident(&mut books);
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
        workers_used,
        dumps,
        oracle: native_root_full(&db).unwrap(),
        _dir: dir,
        db,
    }
}

fn assert_identical(label: &str, base: &RunResult, other: &RunResult) {
    assert_eq!(base.roots.len(), other.roots.len(), "{label}: block count");
    for h in 0..base.roots.len() {
        assert_eq!(base.roots[h], other.roots[h], "{label}: block {} root diverged", h + 1);
        assert_eq!(
            base.book_dumps[h], other.book_dumps[h],
            "{label}: block {} book/level CF bytes diverged",
            h + 1
        );
        assert_eq!(
            base.written[h], other.written[h],
            "{label}: block {} save write count diverged",
            h + 1
        );
        assert_eq!(
            base.post_books[h], other.post_books[h],
            "{label}: block {} post-save in-memory books / cache stats diverged",
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

// ============================================================================
// 1 + 2. Worker-count matrix, cache on and off
// ============================================================================

#[test]
fn save_books_workers_matrix_byte_identical_cache_on() {
    let seed = 0x5A7E_B00C_5EEDu64;
    let serial = run_universe(BookMode::LevelAuthority, &|_| 1, 0, 64, seed, 30, 40);
    assert!(serial.workers_used.iter().all(|&w| w == 1), "serial run must not spawn");
    for workers in [2usize, 3, 4, 8, 64] {
        let par = run_universe(BookMode::LevelAuthority, &move |_| workers, 0, 64, seed, 30, 40);
        let label = format!("cache=on workers={workers}");
        assert_identical(&label, &serial, &par);
        // The parallel path must actually have engaged on the multi-book
        // blocks (gate = 0): every block with >= 2 dirty books.
        let engaged = par.workers_used.iter().filter(|&&w| w >= 2).count();
        assert!(
            engaged >= 25,
            "{label}: parallel drain engaged on only {engaged}/30 blocks: {:?}",
            par.workers_used
        );
        assert!(
            par.workers_used.iter().all(|&w| w <= workers && w <= N_MARKETS as usize),
            "{label}: worker count exceeded cap / dirty books: {:?}",
            par.workers_used
        );
    }
}

#[test]
fn save_books_workers_matrix_byte_identical_cache_off() {
    let seed = 0xBAD_CAFE_0FF5u64;
    let serial = run_universe(BookMode::LevelAuthority, &|_| 1, 0, 0, seed, 24, 40);
    for workers in [2usize, 4, 16] {
        let par = run_universe(BookMode::LevelAuthority, &move |_| workers, 0, 0, seed, 24, 40);
        assert_identical(&format!("cache=off workers={workers}"), &serial, &par);
        assert!(par.workers_used.iter().filter(|&&w| w >= 2).count() >= 20);
        // No cache anywhere: stats are None for every book in both runs.
        assert!(par
            .post_books
            .iter()
            .flatten()
            .all(|(_, _, stats)| stats.is_none()));
    }
}

// ============================================================================
// 3. Alternating worker counts within one run
// ============================================================================

#[test]
fn save_books_alternating_workers_equals_serial() {
    let seed = 0xA17E_12A7_1234u64;
    let serial = run_universe(BookMode::LevelAuthority, &|_| 1, 0, 64, seed, 30, 40);
    let pattern = [1usize, 4, 1, 8, 2, 3, 64, 1, 5];
    let alt = run_universe(BookMode::LevelAuthority, &move |h| pattern[(h as usize) % pattern.len()], 0, 64, seed, 30, 40);
    assert_identical("alternating", &serial, &alt);
    // Sanity: the pattern really alternated serial and parallel saves.
    assert!(alt.workers_used.iter().any(|&w| w == 1));
    assert!(alt.workers_used.iter().any(|&w| w >= 2));
}

// ============================================================================
// 4. Work gate + single-book blocks stay serial
// ============================================================================

#[test]
fn save_books_work_gate_and_single_book_are_serial() {
    let seed = 0x6A7E_0001u64;
    // A gate far above any block's journaled work ⇒ serial every block,
    // still byte-identical to the ungated parallel run.
    let gated = run_universe(BookMode::LevelAuthority, &|_| 8, 1_000_000, 64, seed, 12, 40);
    assert!(
        gated.workers_used.iter().all(|&w| w == 1),
        "gate must keep the serial path: {:?}",
        gated.workers_used
    );
    let open = run_universe(BookMode::LevelAuthority, &|_| 8, 0, 64, seed, 12, 40);
    assert!(open.workers_used.iter().any(|&w| w >= 2));
    assert_identical("gate vs open", &gated, &open);

    // Single dirty book per block (all orders on market 1) ⇒ never parallel
    // regardless of the cap, and identical to workers = 1.
    let (_dir, db) = open_test_db();
    {
        let ctx0 = NativeExecContext::new_with_mode(
            db.clone(), 0, 1000, 0, 100, 10, addr(99), addr(100), addr(101),
            BookMode::LevelAuthority, None,
        );
        fund_native(&ctx0, &addr(1), fp(1_000_000));
    }
    build_native_trie_to_cf(&db).unwrap();
    let overlay = NativeStateOverlay::new(db.clone());
    let mut ctx = NativeExecContext::new_with_mode(
        overlay.clone(), 1, 1001, 0, 100, 10, addr(99), addr(100), addr(101),
        BookMode::LevelAuthority, None,
    );
    ctx.save_books_workers = 8;
    ctx.save_books_min_ops = 0;
    let batch: Vec<(Address, NativeAction)> =
        (0..50).map(|i| place(addr(1), gtc(1, true, 90 + (i % 5), 1))).collect();
    let r = NativeExecutor::execute_batch(&mut ctx, &batch);
    assert!(r.results.iter().all(|x| x.success));
    let written = ctx.save_order_books();
    assert!(written > 0);
    assert_eq!(ctx.last_save_workers(), 1, "one dirty book must drain inline");
}

// ============================================================================
// 5. Mode 3 (chunked level hash) rides the same two-pass parallel drain
// ============================================================================

/// Mode 3 (`TORUS_BOOK_ROWS=3`, chunked depth-independent level digest) is
/// saved by the same two-pass path as mode 2: pass 1 drains each dirty
/// book's journals (re-hashing only its dirty 64-seq chunks) on worker
/// threads, pass 2 writes serially. Contract: byte-identical persisted state
/// and post-save in-memory books for ANY worker count; the parallel path
/// provably engages; the sponge cache is never used (stats None even with a
/// cache budget); the result differs from mode 2 (different preimage); and a
/// fresh reload under mode 3 passes boot verify 3 (from-scratch chunked
/// recompute of every level row == the persisted rows written by the
/// incremental parallel drains).
#[test]
fn save_books_workers_matrix_mode3_chunked_byte_identical() {
    let seed = 0xC4A1_4EDB_00C5u64;
    let serial = run_universe(BookMode::LevelAuthorityChunked, &|_| 1, 0, 64, seed, 30, 40);
    assert!(serial.workers_used.iter().all(|&w| w == 1), "serial run must not spawn");
    // Mode 3 never engages the sponge cache, even with a budget.
    assert!(serial
        .post_books
        .iter()
        .flatten()
        .all(|(_, _, stats)| stats.is_none()));
    for workers in [2usize, 4, 8, 64] {
        let par = run_universe(BookMode::LevelAuthorityChunked, &move |_| workers, 0, 64, seed, 30, 40);
        let label = format!("mode3 workers={workers}");
        assert_identical(&label, &serial, &par);
        let engaged = par.workers_used.iter().filter(|&&w| w >= 2).count();
        assert!(
            engaged >= 25,
            "{label}: parallel drain engaged on only {engaged}/30 blocks: {:?}",
            par.workers_used
        );
        assert!(par
            .post_books
            .iter()
            .flatten()
            .all(|(_, _, stats)| stats.is_none()));

        // Fresh reload from the parallel-drained DB under mode 3: boot
        // verify 3 recomputes every level row FROM SCRATCH (chunked oracle)
        // and must accept the incrementally maintained rows.
        let ctx = NativeExecContext::new_with_mode(
            par.db.clone(), 31, 1031, 0, 100, 10, addr(99), addr(100), addr(101),
            BookMode::LevelAuthorityChunked, None,
        );
        assert!(
            ctx.fatal_error.is_none(),
            "{label}: mode-3 reload boot verify failed: {:?}",
            ctx.fatal_error
        );
        assert!(!ctx.order_books.is_empty(), "{label}: reload found no books");
        // And a mode-2 reload of the same DB must fail-stop (marker/preimage
        // mismatch) — the fleets cannot silently mix.
        let ctx2 = NativeExecContext::new_with_mode(
            par.db.clone(), 31, 1031, 0, 100, 10, addr(99), addr(100), addr(101),
            BookMode::LevelAuthority, None,
        );
        assert!(ctx2.fatal_error.is_some(), "{label}: mode-2 reload of a mode-3 DB must fail-stop");
    }

    // Different preimage from mode 2 over the same batches: the level rows
    // (and hence the roots) differ, while the node-local order rows agree.
    let mode2 = run_universe(BookMode::LevelAuthority, &|_| 1, 0, 64, seed, 30, 40);
    assert_ne!(mode2.oracle, serial.oracle, "mode 3 must not equal mode 2's root");
    let rows = |r: &RunResult| -> Vec<(Vec<u8>, Vec<u8>)> {
        r.dumps
            .iter()
            .filter(|(cf, _, _)| *cf == CF_BOOK_ORDER_ROWS)
            .map(|(_, k, v)| (k.clone(), v.clone()))
            .collect()
    };
    assert_eq!(rows(&mode2), rows(&serial), "node-local order rows must be mode-independent");
}
