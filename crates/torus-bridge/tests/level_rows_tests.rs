//! 3c (level-rows-as-authority, `TORUS_BOOK_ROWS=2`) — executor-level gates
//! (design §5.2 items 2–8).
//!
//! Contracts:
//!   1. Mode-2 save/load roundtrip: multi-block adversarial script → reopen →
//!      rebuilt books byte-equal in-RAM books; idle save writes nothing.
//!   2. Semantic differential across modes 0/1/2: identical fills / balances /
//!      positions / trade history and identical in-memory books after every
//!      block; state roots pairwise DIFFERENT (consensus-visible by design).
//!   3. Fail-stop matrix: each mode's loader is fatal on every other mode's
//!      on-disk artifacts; `__book_mode__` marker catches wrong-flag restarts
//!      on EMPTY books; split-brain store is fatal.
//!   4. Corrupt-one-byte: flipping one byte in the node-local order store (or
//!      a root level row) makes the mode-2 boot verify fail-stop.
//!   5. N-mode combo matrix within mode 2: resident × trie-cache ×
//!      member-cache × parallel-settle over a 40-block adversarial sequence
//!      with a mid-run restart ⇒ per-block roots, trie CF, mirror CF, BOTH
//!      book CFs and the oracle byte-identical across all combos.
//!   6. Dirty-entry bound witness: a many-order block dirties O(levels +
//!      meta + balances) root entries, NOT O(orders).
//!   7. Crash between save and stash: rebuild + boot verify green.
//!   8. `save_book_full` == accumulated deltas (byte-compare both book CFs).

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_HASHED, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_TRIE, CF_NATIVE_USER_TRADES,
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

fn make_ctx(state_db: StateDb, height: u64, mode: BookMode) -> NativeExecContext {
    NativeExecContext::new_with_mode(
        state_db,
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        None,
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

#[allow(clippy::too_many_arguments)]
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

/// Classic whole-book Borsh bytes — the canonical in-memory book comparator.
fn book_bytes(ctx: &NativeExecContext) -> Vec<(MarketId, Vec<u8>)> {
    let mut out: Vec<(MarketId, Vec<u8>)> = ctx
        .order_books
        .iter()
        .map(|(mid, book)| (*mid, borsh::to_vec(book).expect("book borsh")))
        .collect();
    out.sort_by_key(|(mid, _)| *mid);
    out
}

fn non_book_dump(db: &StateDb) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
    ] {
        for (k, v) in dump_cf(db, cf) {
            out.push((cf, k, v));
        }
    }
    out
}

/// The 3-block adversarial script from book_rows_tests (places incl. dust,
/// partial fills, both modify kinds, STP, stop order, cancels across 3
/// markets), parameterized by persistence mode.
fn run_script(db: StateDb, mode: BookMode) -> ScriptResult {
    let m1 = addr(1);
    let m2 = addr(2);
    let t1 = addr(3);
    let s1 = addr(4);
    let dusty = FixedPoint::from_raw(2 * FixedPoint::SCALE + 7);

    let mut fingerprints = Vec::new();
    let mut books_after = Vec::new();
    let mut writes = Vec::new();

    let mut ctx = make_ctx(db.clone(), 1, mode);
    assert!(ctx.fatal_error.is_none(), "load1: {:?}", ctx.fatal_error);
    for t in [m1, m2, t1, s1] {
        fund_native(&ctx, &t, fp(1_000_000));
    }
    let id0 = ctx.next_global_order_id;
    let b1 = vec![
        place(m1, gtc(1, true, 100, 5)),
        place(m2, gtc(1, true, 100, 3)),
        place(m1, gtc(1, false, 105, 4)),
        place(m2, order(2, true, fp(50), dusty, OrderType::Limit, TimeInForce::GTC)),
        place(m1, gtc(2, false, 55, 6)),
        place(
            s1,
            order(
                3,
                true,
                fp(10),
                fp(2),
                OrderType::StopMarket { trigger: fp(12) },
                TimeInForce::GTC,
            ),
        ),
        place(m2, gtc(3, false, 11, 1)),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &b1);
    assert!(r.results.iter().all(|x| x.success), "block1 failures");
    writes.push(ctx.save_order_books());
    fingerprints.push(non_book_dump(&db));
    books_after.push(book_bytes(&ctx));

    let mut ctx = make_ctx(db.clone(), 2, mode);
    assert!(ctx.fatal_error.is_none(), "load2: {:?}", ctx.fatal_error);
    let b2 = vec![
        place(t1, gtc(1, false, 100, 2)),
        (
            m2,
            NativeAction::ModifyOrder {
                order_id: id0 + 3,
                new_price: None,
                new_qty: Some(fp(1)),
            },
        ),
        (
            m1,
            NativeAction::ModifyOrder {
                order_id: id0,
                new_price: Some(fp(99)),
                new_qty: None,
            },
        ),
        place(s1, gtc(2, true, 54, 1)),
        place(s1, gtc(2, false, 54, 1)),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &b2);
    assert!(r.results.iter().all(|x| x.success), "block2 failures");
    writes.push(ctx.save_order_books());
    fingerprints.push(non_book_dump(&db));
    books_after.push(book_bytes(&ctx));

    let mut ctx = make_ctx(db.clone(), 3, mode);
    assert!(ctx.fatal_error.is_none(), "load3: {:?}", ctx.fatal_error);
    let b3 = vec![
        (m2, NativeAction::CancelOrder { order_id: id0 + 3 }),
        place(t1, gtc(1, true, 105, 4)),
        place(t1, gtc(3, true, 11, 1)),
        (m1, NativeAction::CancelAllOrders { market_id: None }),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &b3);
    assert!(r.results.iter().all(|x| x.success), "block3 failures");
    writes.push(ctx.save_order_books());
    fingerprints.push(non_book_dump(&db));
    books_after.push(book_bytes(&ctx));

    // Reload (block 4, no ops): the persisted form round-trips.
    let mut ctx = make_ctx(db.clone(), 4, mode);
    assert!(ctx.fatal_error.is_none(), "reload: {:?}", ctx.fatal_error);
    let reloaded_books = book_bytes(&ctx);
    let idle_writes = ctx.save_order_books();

    ScriptResult {
        fingerprints,
        books_after,
        writes,
        reloaded_books,
        idle_writes,
        book_cf: dump_cf(&db, CF_NATIVE_ORDER_BOOKS),
        store_cf: dump_cf(&db, CF_BOOK_ORDER_ROWS),
        root: native_root_full(&db).expect("root"),
    }
}

struct ScriptResult {
    fingerprints: Vec<Vec<(&'static str, Vec<u8>, Vec<u8>)>>,
    books_after: Vec<Vec<(MarketId, Vec<u8>)>>,
    #[allow(dead_code)]
    writes: Vec<usize>,
    reloaded_books: Vec<(MarketId, Vec<u8>)>,
    idle_writes: usize,
    book_cf: Vec<(Vec<u8>, Vec<u8>)>,
    store_cf: Vec<(Vec<u8>, Vec<u8>)>,
    root: B256,
}

// ============================================================================
// 2. Semantic differential modes 0/1/2
// ============================================================================

#[test]
fn semantic_differential_modes_0_1_2() {
    let (_d0, db0) = open_test_db();
    let (_d1, db1) = open_test_db();
    let (_d2, db2) = open_test_db();

    let classic = run_script(db0, BookMode::Classic);
    let rows = run_script(db1, BookMode::OrderRows);
    let levels = run_script(db2, BookMode::LevelAuthority);

    for (i, ((c, r), l)) in classic
        .fingerprints
        .iter()
        .zip(rows.fingerprints.iter())
        .zip(levels.fingerprints.iter())
        .enumerate()
    {
        assert_eq!(c, r, "mode 0 vs 1: non-book state diverged after block {}", i + 1);
        assert_eq!(c, l, "mode 0 vs 2: non-book state diverged after block {}", i + 1);
    }
    for (i, ((c, r), l)) in classic
        .books_after
        .iter()
        .zip(rows.books_after.iter())
        .zip(levels.books_after.iter())
        .enumerate()
    {
        assert_eq!(c, r, "mode 0 vs 1: in-memory books diverged after block {}", i + 1);
        assert_eq!(c, l, "mode 0 vs 2: in-memory books diverged after block {}", i + 1);
    }

    // Roundtrip: every mode reloads the same books (FIFO priority incl. the
    // re-stamped modify survives all three persistence forms).
    assert_eq!(classic.reloaded_books, rows.reloaded_books);
    assert_eq!(classic.reloaded_books, levels.reloaded_books);
    assert_eq!(classic.books_after.last().unwrap(), &classic.reloaded_books);
    assert_eq!(levels.books_after.last().unwrap(), &levels.reloaded_books);

    // Idle saves write nothing in any mode.
    assert_eq!(classic.idle_writes, 0);
    assert_eq!(rows.idle_writes, 0);
    assert_eq!(levels.idle_writes, 0);

    // Mode 2 stores NO order rows in the root CF, and its full order data in
    // the node-local store; modes 0/1 leave the node-local store EMPTY.
    assert!(
        levels
            .book_cf
            .iter()
            .all(|(k, _)| !(k.len() == 25 && k[8] == 0x01)),
        "mode 2 must not put order rows in the root CF"
    );
    assert!(!levels.store_cf.is_empty(), "mode 2 uses the node-local store");
    assert!(classic.store_cf.is_empty(), "mode 0 must not touch the store");
    assert!(rows.store_cf.is_empty(), "mode 1 must not touch the store");

    // Consensus visibility: roots pairwise different.
    assert_ne!(classic.root, rows.root, "mode 0 vs 1 roots must differ");
    assert_ne!(classic.root, levels.root, "mode 0 vs 2 roots must differ");
    assert_ne!(rows.root, levels.root, "mode 1 vs 2 roots must differ");
}

// ============================================================================
// 3. Fail-stop matrix (9 cells + marker + split-brain)
// ============================================================================

/// Write one block of real book content (a resting order + a pending stop)
/// under `mode`, returning the db.
fn seed_db(mode: BookMode) -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1, mode);
    fund_native(&ctx, &addr(1), fp(1_000_000));
    let r = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            place(addr(1), gtc(1, true, 100, 1)),
            place(
                addr(1),
                order(
                    1,
                    true,
                    fp(10),
                    fp(1),
                    OrderType::StopMarket { trigger: fp(200) },
                    TimeInForce::GTC,
                ),
            ),
        ],
    );
    assert!(r.results.iter().all(|x| x.success));
    assert!(ctx.fatal_error.is_none());
    ctx.save_order_books();
    (dir, db)
}

#[test]
fn fail_stop_matrix_all_mode_pairs() {
    let modes = [BookMode::Classic, BookMode::OrderRows, BookMode::LevelAuthority];
    for write_mode in modes {
        for load_mode in modes {
            let (_dir, db) = seed_db(write_mode);
            let ctx = make_ctx(db, 2, load_mode);
            if write_mode == load_mode {
                assert!(
                    ctx.fatal_error.is_none(),
                    "{write_mode:?} -> {load_mode:?} must load clean: {:?}",
                    ctx.fatal_error
                );
                assert!(!ctx.order_books.is_empty(), "books must be present");
            } else {
                let err = ctx.fatal_error.as_deref().unwrap_or("");
                assert!(
                    err.contains("TORUS_BOOK_ROWS") || err.contains("book-mode marker"),
                    "{write_mode:?} -> {load_mode:?} must fail-stop, got: {err:?}"
                );
            }
        }
    }
}

/// Marker-only detection: books still EMPTY (nothing to content-sniff), but a
/// prior save stamped the mode marker — a wrong-flag restart is still fatal.
#[test]
fn book_mode_marker_catches_wrong_flag_on_empty_books() {
    let (_dir, db) = open_test_db();
    {
        let mut ctx = make_ctx(db.clone(), 1, BookMode::LevelAuthority);
        assert!(ctx.fatal_error.is_none());
        ctx.save_order_books(); // no books — writes only the marker
    }
    // Same mode: clean.
    let ctx = make_ctx(db.clone(), 2, BookMode::LevelAuthority);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    // Wrong mode: fatal via the marker alone.
    for wrong in [BookMode::Classic, BookMode::OrderRows] {
        let ctx = make_ctx(db.clone(), 2, wrong);
        let err = ctx.fatal_error.as_deref().unwrap_or("");
        assert!(
            err.contains("book-mode marker"),
            "empty-book wrong-flag restart must fail via marker, got: {err:?}"
        );
    }
}

#[test]
fn split_brain_store_without_root_rows_is_fatal() {
    let (_dir, db) = open_test_db();
    // A well-formed node-local order row with NO root-CF content at all.
    let mut book = torus_core::order_book::OrderBook::new(1, fp(1), fp(1));
    let r = book.place_order(gtc(1, true, 100, 1), addr(1), 1);
    let row = book.encode_order_row(r.order_id).unwrap();
    let key = torus_core::book_rows::book_order_key(1, r.order_id);
    StateDb::put_cf_raw(&db, CF_BOOK_ORDER_ROWS, &key, &row).unwrap();

    let ctx = make_ctx(db, 1, BookMode::LevelAuthority);
    let err = ctx.fatal_error.as_deref().unwrap_or("");
    assert!(
        err.contains("split-brain"),
        "orphan store content must be fatal, got: {err:?}"
    );
}

// ============================================================================
// 4. Corrupt-one-byte boot-verify fail-stop
// ============================================================================

#[test]
fn corrupt_one_byte_in_order_store_fails_boot_verify() {
    let (_dir, db) = seed_db(BookMode::LevelAuthority);

    // Flip one byte inside the stored order row's remaining_qty.
    let rows = dump_cf(&db, CF_BOOK_ORDER_ROWS);
    assert!(!rows.is_empty());
    let (key, mut val) = rows[0].clone();
    // value = seq(8) ‖ id(16) ‖ trader(20) ‖ side(1) ‖ price(16) ‖ remaining(16)…
    let off = 8 + 16 + 20 + 1 + 16 + 15; // low byte of remaining_qty
    val[off] ^= 0x01;
    StateDb::put_cf_raw(&db, CF_BOOK_ORDER_ROWS, &key, &val).unwrap();

    let ctx = make_ctx(db, 2, BookMode::LevelAuthority);
    let err = ctx.fatal_error.as_deref().unwrap_or("");
    assert!(
        err.contains("corrupt") || err.contains("stale") || err.contains("!="),
        "one flipped byte must fail boot verify, got: {err:?}"
    );
}

#[test]
fn corrupt_level_hash_in_root_cf_fails_boot_verify() {
    let (_dir, db) = seed_db(BookMode::LevelAuthority);

    let rows = dump_cf(&db, CF_NATIVE_ORDER_BOOKS);
    let (key, mut val) = rows
        .iter()
        .find(|(k, _)| k.len() == 26 && k[8] == 0x03)
        .expect("a level row exists")
        .clone();
    val[51] ^= 0x80; // inside level_hash
    StateDb::put_cf_raw(&db, CF_NATIVE_ORDER_BOOKS, &key, &val).unwrap();

    let ctx = make_ctx(db, 2, BookMode::LevelAuthority);
    let err = ctx.fatal_error.as_deref().unwrap_or("");
    assert!(
        err.contains("level rows") || err.contains("corrupt") || err.contains("stale"),
        "corrupt level hash must fail boot verify, got: {err:?}"
    );
}

// ============================================================================
// 5. N-mode combo matrix within mode 2 (40-block adversarial + restart)
// ============================================================================

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// 40 deterministic pseudo-random blocks over 3 markets: sprays, crossing
/// sweeps, cancels, both modify kinds, cancel-alls, stop orders. Every
/// PlaceOrder consumes exactly one global order id, so ids are predictable
/// and cancels/modifies can target real (or recently-consumed — failures are
/// deterministic too) orders.
fn adversarial_batches(blocks: usize) -> Vec<Vec<(Address, NativeAction)>> {
    let traders = [addr(1), addr(2), addr(3), addr(4)];
    let mut s = 0xC0FFEE_D00Du64;
    let mut next_id: u128 = 1;
    let mut placed: Vec<u128> = Vec::new();
    let mut out = Vec::with_capacity(blocks);
    for _ in 0..blocks {
        let n = 3 + (xorshift(&mut s) % 6) as usize;
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            let t = traders[(xorshift(&mut s) % 4) as usize];
            let market = 1 + (xorshift(&mut s) % 3) as u64;
            match xorshift(&mut s) % 10 {
                0..=4 => {
                    // resting-ish limit spray
                    let is_buy = xorshift(&mut s) % 2 == 0;
                    let price = if is_buy {
                        90 + (xorshift(&mut s) % 10) as i64
                    } else {
                        101 + (xorshift(&mut s) % 10) as i64
                    };
                    let qty = 1 + (xorshift(&mut s) % 5) as i64;
                    batch.push(place(t, gtc(market, is_buy, price, qty)));
                    placed.push(next_id);
                    next_id += 1;
                }
                5 => {
                    // crossing sweep (fills best levels, maybe rests)
                    let is_buy = xorshift(&mut s) % 2 == 0;
                    let price = if is_buy { 105 } else { 95 };
                    let qty = 2 + (xorshift(&mut s) % 6) as i64;
                    batch.push(place(t, gtc(market, is_buy, price, qty)));
                    placed.push(next_id);
                    next_id += 1;
                }
                6 => {
                    if let Some(&id) =
                        placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        batch.push((t, NativeAction::CancelOrder { order_id: id }));
                    }
                }
                7 => {
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
                                    Some(fp(92 + (xorshift(&mut s) % 8) as i64))
                                },
                                new_qty: if qty_mod { Some(fp(1)) } else { None },
                            },
                        ));
                    }
                }
                8 => {
                    batch.push((t, NativeAction::CancelAllOrders { market_id: None }));
                }
                _ => {
                    // stop order
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
        out.push(batch);
    }
    out
}

#[derive(Clone, Copy)]
struct Combo {
    resident: bool,
    trie_cache: bool,
    member_cache: bool,
    parallel_settle: bool,
}

struct ComboResult {
    roots: Vec<B256>,
    dumps: Vec<(&'static str, Vec<u8>, Vec<u8>)>,
    oracle: B256,
}

fn run_combo_mode2(combo: Combo, restart_before: Option<u64>) -> ComboResult {
    let (_dir, db) = open_test_db();
    {
        let ctx0 = make_ctx(db.clone(), 0, BookMode::LevelAuthority);
        for t in [addr(1), addr(2), addr(3), addr(4)] {
            fund_native(&ctx0, &t, fp(100_000_000));
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    let mut books = ResidentBooks::default();
    let mut trie_cache = NativeTrieCache::default();
    let mut member_cache = if combo.member_cache {
        NativeMemberCache::with_budget(8 * 1024 * 1024)
    } else {
        NativeMemberCache::with_budget(0)
    };

    let batches = adversarial_batches(40);
    let mut roots = Vec::new();

    for (i, batch) in batches.iter().enumerate() {
        let height = (i + 1) as u64;
        if restart_before == Some(height) {
            books = ResidentBooks::default();
            trie_cache.invalidate();
            member_cache.invalidate();
        }
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
            BookMode::LevelAuthority,
            if combo.resident { Some(&mut books) } else { None },
        );
        assert!(ctx.fatal_error.is_none(), "load {height}: {:?}", ctx.fatal_error);

        // Failures (cancel of an already-gone id etc.) are deterministic —
        // only fatal errors are disallowed.
        let _ = NativeExecutor::execute_batch_settle_mode(&mut ctx, batch, combo.parallel_settle);
        assert!(ctx.fatal_error.is_none(), "exec {height}: {:?}", ctx.fatal_error);
        ctx.save_order_books();
        ctx.stash_resident(&mut books);

        overlay
            .flush_with_native_trie_stats(
                &db,
                Some(height),
                if combo.trie_cache { Some(&mut trie_cache) } else { None },
                if member_cache.is_enabled() { Some(&mut member_cache) } else { None },
            )
            .expect("flush");
        roots.push(persisted_native_root(&db).unwrap());
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
    ComboResult {
        roots,
        dumps,
        oracle: native_root_full(&db).unwrap(),
    }
}

#[test]
fn mode2_combo_matrix_byte_identical_with_midrun_restart() {
    let mut results: Vec<(Combo, ComboResult)> = Vec::new();
    for resident in [false, true] {
        for trie_cache in [false, true] {
            for member_cache in [false, true] {
                for parallel_settle in [false, true] {
                    // Prune to 8 combos: parallel_settle only varied with
                    // member_cache off (it is exec-side, orthogonal to the
                    // flush knobs) — keeps runtime sane.
                    if parallel_settle && member_cache {
                        continue;
                    }
                    let c = Combo { resident, trie_cache, member_cache, parallel_settle };
                    // Mid-run restart at block 20 for the resident combos.
                    let restart = if resident { Some(20) } else { None };
                    results.push((c, run_combo_mode2(c, restart)));
                }
            }
        }
    }

    let (_, base) = &results[0];
    assert_eq!(
        base.oracle,
        *base.roots.last().unwrap(),
        "persisted root != full-scan oracle"
    );
    for (c, r) in &results[1..] {
        let label = format!(
            "resident={} trie_cache={} member_cache={} parallel_settle={}",
            c.resident, c.trie_cache, c.member_cache, c.parallel_settle
        );
        assert_eq!(base.roots, r.roots, "{label}: per-block roots diverged");
        assert_eq!(base.dumps.len(), r.dumps.len(), "{label}: CF row counts diverged");
        assert_eq!(base.dumps, r.dumps, "{label}: persisted CF bytes diverged");
        assert_eq!(base.oracle, r.oracle, "{label}: oracle diverged");
    }
}

// ============================================================================
// 6. Dirty-entry bound witness
// ============================================================================

#[test]
fn mode2_dirty_entries_scale_with_levels_not_orders() {
    let (_dir, db) = open_test_db();
    {
        let ctx0 = make_ctx(db.clone(), 0, BookMode::LevelAuthority);
        for t in [addr(1), addr(2), addr(3), addr(4)] {
            fund_native(&ctx0, &t, fp(100_000_000));
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    // One block: 600 resting orders across 3 markets × 10 bid levels.
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
        BookMode::LevelAuthority,
        None,
    );
    assert!(ctx.fatal_error.is_none());
    let traders = [addr(1), addr(2), addr(3), addr(4)];
    let batch: Vec<(Address, NativeAction)> = (0..600)
        .map(|i| {
            let market = 1 + (i % 3) as u64;
            let price = 50 + ((i / 3) % 10) as i64;
            place(traders[i % 4], gtc(market, true, price, 1))
        })
        .collect();
    let r = NativeExecutor::execute_batch(&mut ctx, &batch);
    assert!(r.results.iter().all(|x| x.success));
    ctx.save_order_books();

    let stats = overlay
        .flush_with_native_trie_stats(&db, Some(1), None, None)
        .expect("flush");

    // THE 3c bound: root-CF book entries = touched levels + meta rows, NOT
    // touched orders. 3 markets × 10 levels + 3 meta = 33.
    let book_dirty = stats.dirty_entries_by_cf[1];
    assert!(
        book_dirty <= 33,
        "book-CF dirty entries must be O(levels+meta): got {book_dirty} for 600 orders"
    );
    // Total dirty buckets ≪ touched orders (balances for 4 traders + fee
    // rows + 33 book entries — nowhere near 600).
    assert!(
        stats.dirty_buckets < 100,
        "dirty buckets must be O(levels), got {}",
        stats.dirty_buckets
    );
    // All 600 orders landed in the node-local store (not the root).
    assert_eq!(dump_cf(&db, CF_BOOK_ORDER_ROWS).len(), 600);
    // Root equals oracle under the small dirty set.
    assert_eq!(persisted_native_root(&db).unwrap(), native_root_full(&db).unwrap());
}

// ============================================================================
// 7. Crash between save and stash (resident mode 2)
// ============================================================================

#[test]
fn crash_between_save_and_stash_rebuilds_and_verifies() {
    let (_dir, db) = open_test_db();
    {
        let ctx0 = make_ctx(db.clone(), 0, BookMode::LevelAuthority);
        for t in [addr(1), addr(2)] {
            fund_native(&ctx0, &t, fp(1_000_000));
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    let mut books = ResidentBooks::default();

    // Block 1: exec + save + flush, then "crash" BEFORE stash (holder empty).
    {
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
            BookMode::LevelAuthority,
            Some(&mut books),
        );
        assert!(ctx.fatal_error.is_none());
        let r = NativeExecutor::execute_batch(
            &mut ctx,
            &[place(addr(1), gtc(1, true, 100, 2)), place(addr(2), gtc(1, false, 105, 1))],
        );
        assert!(r.results.iter().all(|x| x.success));
        ctx.save_order_books();
        overlay
            .flush_with_native_trie_stats(&db, Some(1), None, None)
            .unwrap();
        // NO stash_resident — simulated crash.
    }
    assert!(!books.is_populated(), "holder must be empty after the crash");

    // Block 2: rebuild from the DB (boot verify runs), then execute + fill.
    {
        let overlay = NativeStateOverlay::new(db.clone());
        let mut ctx = NativeExecContext::new_with_mode(
            overlay.clone(),
            2,
            1002,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            BookMode::LevelAuthority,
            Some(&mut books),
        );
        assert!(
            ctx.fatal_error.is_none(),
            "post-crash rebuild must verify green: {:?}",
            ctx.fatal_error
        );
        assert!(!ctx.resident_reused(), "crash must force a rebuild");
        let r = NativeExecutor::execute_batch(
            &mut ctx,
            &[place(addr(2), gtc(1, false, 100, 1))], // fills the resting bid
        );
        assert!(r.results.iter().all(|x| x.success));
        ctx.save_order_books();
        ctx.stash_resident(&mut books);
        overlay
            .flush_with_native_trie_stats(&db, Some(2), None, None)
            .unwrap();
    }
    assert_eq!(persisted_native_root(&db).unwrap(), native_root_full(&db).unwrap());

    // And a fresh boot still verifies green.
    let ctx = make_ctx(db, 3, BookMode::LevelAuthority);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
}

// ============================================================================
// 8. save_book_full == accumulated deltas
// ============================================================================

#[test]
fn save_book_full_equals_accumulated_deltas() {
    let (_da, db_delta) = open_test_db();
    let (_db_, db_full) = open_test_db();

    let _ = run_script(db_delta.clone(), BookMode::LevelAuthority);

    // Reload the delta-built state, then full-save the same books into a
    // fresh DB; the book CFs must be byte-identical.
    let mut ctx = make_ctx(db_delta.clone(), 5, BookMode::LevelAuthority);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    let mut mids: Vec<MarketId> = ctx.order_books.keys().copied().collect();
    mids.sort_unstable();
    for mid in mids {
        let book = ctx.order_books.get_mut(&mid).unwrap();
        NativeExecContext::<StateDb>::save_book_full(&db_full, book, BookMode::LevelAuthority);
    }

    // Compare only the book rows (the delta DB also carries balances etc.).
    assert_eq!(
        dump_cf(&db_delta, CF_NATIVE_ORDER_BOOKS),
        dump_cf(&db_full, CF_NATIVE_ORDER_BOOKS),
        "root book CF: full save != accumulated deltas"
    );
    assert_eq!(
        dump_cf(&db_delta, CF_BOOK_ORDER_ROWS),
        dump_cf(&db_full, CF_BOOK_ORDER_ROWS),
        "node-local store: full save != accumulated deltas"
    );
}
