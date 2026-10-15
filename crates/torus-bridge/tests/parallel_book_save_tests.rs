//! Classic (`BookMode::Classic`) parallel book save
//! (`TORUS_PARALLEL_BOOK_SAVE`, exp/archy-s58-parallel-classic-book-save):
//! byte-identity witnesses.
//!
//! Contract: with the flag on, each dirty book's whole-book blob is encoded as
//! its own exec-pool job; the puts run back on the exec thread, serially, in
//! market-ascending order. Every persisted byte, every per-block `written`
//! count, the PUT SEQUENCE itself (cf, key order), the post-save in-memory
//! books and their (empty) journals, the native roots and a cold reload must
//! equal the serial loop's.
//!
//! The runs pin the flag per thread (`with_parallel_book_save`), so they hold
//! whatever `TORUS_PARALLEL_BOOK_SAVE` the process has.
//!
//! Cells:
//!   1. Seeded adversarial Classic universe (8 markets, resident books,
//!      places / fills / cancels / cancel-alls / modifies / stop-limits /
//!      stop-markets / cloid orders), serial vs parallel (gate 0): identical;
//!      parallel provably engaged (one job per present dirty book).
//!   2. Alternating flag per block within one run equals the serial run (a
//!      parallel save leaves the next block an indistinguishable start).
//!   3. Injected put faults: the parallel save latches the same first fault,
//!      attempts the same puts in the same order, and writes the same rows.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    with_parallel_book_save, BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_HASHED, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRIE,
};
use torus_state::native_trie::{
    build_native_trie_to_cf, native_root_full, persisted_native_root, NativeMemberCache,
    NativeTrieCache,
};
use torus_state::{
    AtomicWriteOp, NativeStateOverlay, OwnPendingRow, ResidentChange, StateBackend, StateDb,
    StateError,
};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const N_MARKETS: u64 = 8;
const N_TRADERS: u8 = 8;
const INJECTED: &str = "injected order book put failure";

// ---- Recording / fault-injecting backend over the production overlay ----

/// Every put / delete to the two save CFs, in call order.
type PutLog = Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>;

/// `NativeStateOverlay` plus a log of the save CFs' writes (in call order)
/// and armed `CF_NATIVE_ORDER_BOOKS` keys whose puts fail. Forwards every
/// method the overlay overrides, so execution sees the overlay unchanged.
#[derive(Clone)]
struct Rec {
    inner: NativeStateOverlay,
    log: PutLog,
    fail_book_puts: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Rec {
    fn new(inner: NativeStateOverlay) -> Self {
        Self {
            inner,
            log: Arc::default(),
            fail_book_puts: Arc::default(),
        }
    }
    fn take_log(&self) -> Vec<(&'static str, Vec<u8>)> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
    fn note(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        let cf = match cf {
            CF_NATIVE_ORDER_BOOKS => CF_NATIVE_ORDER_BOOKS,
            CF_NATIVE_MARKETS => CF_NATIVE_MARKETS,
            _ => return Ok(()),
        };
        self.log.lock().unwrap().push((cf, key.to_vec()));
        if cf == CF_NATIVE_ORDER_BOOKS
            && self.fail_book_puts.lock().unwrap().iter().any(|k| k == key)
        {
            return Err(StateError::Io(std::io::Error::other(INJECTED)));
        }
        Ok(())
    }
}

impl StateBackend for Rec {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.note(cf, key)?;
        self.inner.put_cf_raw(cf, key, value)
    }
    fn put_cf_raw_owned(&self, cf: &str, key: &[u8], value: Vec<u8>) -> Result<(), StateError> {
        self.note(cf, key)?;
        self.inner.put_cf_raw_owned(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.note(cf, key)?;
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf_from(cf, start, limit)
    }
    fn get_cf_len(&self, cf: &str, key: &[u8]) -> Result<Option<usize>, StateError> {
        self.inner.get_cf_len(cf, key)
    }
    fn iterate_cf_prefix_from(
        &self,
        cf: &str,
        prefix: &[u8],
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf_prefix_from(cf, prefix, start, limit)
    }
    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        self.inner.prefix_exists(cf, prefix)
    }
    fn layer_touches(&self, cf: &str, prefix: &[u8]) -> bool {
        self.inner.layer_touches(cf, prefix)
    }
    fn layer_keys(&self, cf: &str) -> Option<Vec<Vec<u8>>> {
        self.inner.layer_keys(cf)
    }
    fn own_pending_row(&self, cf: &str, key: &[u8]) -> OwnPendingRow {
        self.inner.own_pending_row(cf, key)
    }
    fn resident_changes(&self, cf: &str, prefix: &[u8]) -> Option<Vec<ResidentChange>> {
        self.inner.resident_changes(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

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

fn fund_native<B: StateBackend>(ctx: &NativeExecContext<B>, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn params(
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
    order_type: OrderType,
    tif: TimeInForce,
    cloid: Option<u128>,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: cloid,
    }
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

fn new_ctx<B: StateBackend>(
    state: B,
    height: u64,
    books: Option<&mut ResidentBooks>,
) -> NativeExecContext<B> {
    NativeExecContext::new_with_mode(
        state,
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        BookMode::Classic,
        books,
    )
}

/// Seeded Classic blocks over `N_MARKETS` markets: resting sprays (deep
/// levels), crossing sweeps (fills, stop triggers), IOC takers, cancels,
/// price / qty modifies, cancel-alls (all markets and one market),
/// stop-limits and stop-markets, cloid orders (113 B rows: presized buffers
/// must grow). Skewed toward high market ids so book sizes differ.
fn adversarial_batches(
    blocks: usize,
    seed: u64,
    per_block: usize,
) -> Vec<Vec<(Address, NativeAction)>> {
    let mut s = seed;
    let mut next_id: u128 = 1;
    let mut next_cloid: u128 = 1;
    let mut placed: Vec<u128> = Vec::new();
    let mut out = Vec::with_capacity(blocks);
    for _ in 0..blocks {
        let n = per_block / 2 + (xorshift(&mut s) as usize % per_block.max(1));
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            let t = addr(1 + (xorshift(&mut s) % N_TRADERS as u64) as u8);
            let a = xorshift(&mut s) % N_MARKETS;
            let b = xorshift(&mut s) % N_MARKETS;
            let market = 1 + a.max(b);
            let cloid = (xorshift(&mut s).is_multiple_of(3)).then(|| {
                next_cloid += 1;
                next_cloid
            });
            let roll = xorshift(&mut s) % 16;
            match roll {
                0..=6 => {
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let price = if is_buy {
                        94 + (xorshift(&mut s) % 6) as i64
                    } else {
                        101 + (xorshift(&mut s) % 6) as i64
                    };
                    let qty = 1 + (xorshift(&mut s) % 5) as i64;
                    let p = params(
                        market,
                        is_buy,
                        price,
                        qty,
                        OrderType::Limit,
                        TimeInForce::GTC,
                        cloid,
                    );
                    batch.push(place(t, p));
                    placed.push(next_id);
                    next_id += 1;
                }
                7 | 8 => {
                    // crossing sweep: fills the best levels, may rest
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let price = if is_buy { 106 } else { 94 };
                    let qty = 2 + (xorshift(&mut s) % 8) as i64;
                    let tif = if xorshift(&mut s).is_multiple_of(3) {
                        TimeInForce::IOC
                    } else {
                        TimeInForce::GTC
                    };
                    batch.push(place(
                        t,
                        params(market, is_buy, price, qty, OrderType::Limit, tif, cloid),
                    ));
                    placed.push(next_id);
                    next_id += 1;
                }
                9 => {
                    if let Some(&id) = placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        batch.push((t, NativeAction::CancelOrder { order_id: id }));
                    }
                }
                10 | 11 => {
                    if let Some(&id) = placed.get((xorshift(&mut s) as usize) % placed.len().max(1))
                    {
                        let qty_mod = xorshift(&mut s).is_multiple_of(2);
                        batch.push((
                            t,
                            NativeAction::ModifyOrder {
                                order_id: id,
                                new_price: if qty_mod {
                                    None
                                } else {
                                    Some(fp(93 + (xorshift(&mut s) % 14) as i64))
                                },
                                new_qty: if qty_mod {
                                    Some(fp(1 + (xorshift(&mut s) % 3) as i64))
                                } else {
                                    None
                                },
                            },
                        ));
                    }
                }
                12 => {
                    let only = (xorshift(&mut s).is_multiple_of(2)).then_some(market);
                    if xorshift(&mut s).is_multiple_of(3) {
                        batch.push((t, NativeAction::CancelAllOrders { market_id: only }));
                    }
                }
                13 | 14 => {
                    // stop-limit, trigger near the trade band so sweeps fire some
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let (trigger, limit) = if is_buy {
                        (101 + (xorshift(&mut s) % 6) as i64, 104)
                    } else {
                        (94 + (xorshift(&mut s) % 6) as i64, 96)
                    };
                    let ot = OrderType::StopLimit {
                        trigger: fp(trigger),
                        limit: fp(limit),
                    };
                    batch.push(place(
                        t,
                        params(market, is_buy, limit, 1, ot, TimeInForce::GTC, cloid),
                    ));
                    placed.push(next_id);
                    next_id += 1;
                }
                _ => {
                    // stop-market (price = slippage cap)
                    let is_buy = xorshift(&mut s).is_multiple_of(2);
                    let (trigger, cap) = if is_buy { (105, 110) } else { (95, 90) };
                    let ot = OrderType::StopMarket {
                        trigger: fp(trigger),
                    };
                    batch.push(place(
                        t,
                        params(market, is_buy, cap, 1, ot, TimeInForce::GTC, cloid),
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

/// One book's post-save in-memory state: `(market, borsh bytes, journaled
/// rows, journaled levels)`.
type PostBook = (MarketId, Vec<u8>, usize, usize);

struct RunResult {
    roots: Vec<B256>,
    /// Per block: `save_order_books()`'s return.
    written: Vec<usize>,
    /// Per block: `last_save_workers()`.
    workers: Vec<usize>,
    /// Per block: dirty books present in `order_books` at save.
    dirty_present: Vec<usize>,
    /// Per block: the save's writes to the two save CFs, in call order.
    put_seq: Vec<Vec<(&'static str, Vec<u8>)>>,
    /// Per block, after the flush: every `CF_NATIVE_ORDER_BOOKS` row and
    /// every `CF_NATIVE_MARKETS` row (next order id, mode marker).
    book_rows: Vec<Vec<(Vec<u8>, Vec<u8>)>>,
    market_rows: Vec<Vec<(Vec<u8>, Vec<u8>)>>,
    post_books: Vec<Vec<PostBook>>,
    dumps: Vec<(&'static str, Vec<u8>, Vec<u8>)>,
    oracle: B256,
    /// A cold (non-resident) reload of the final DB: its books' bytes.
    reloaded: Vec<(MarketId, Vec<u8>)>,
    _dir: tempfile::TempDir,
}

/// One Classic universe (resident books, overlay per block) with the flag
/// pinned per block by `on_for(height)` (gate 0 when on).
fn run_universe(
    on_for: &dyn Fn(u64) -> bool,
    seed: u64,
    blocks: usize,
    per_block: usize,
) -> RunResult {
    torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
    let (dir, db) = open_test_db();
    {
        let ctx0 = new_ctx(db.clone(), 0, None);
        for t in 1..=N_TRADERS {
            fund_native(&ctx0, &addr(t), fp(100_000_000));
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    let mut books = ResidentBooks::default();
    let batches = adversarial_batches(blocks, seed, per_block);
    let mut r = RunResult {
        roots: Vec::new(),
        written: Vec::new(),
        workers: Vec::new(),
        dirty_present: Vec::new(),
        put_seq: Vec::new(),
        book_rows: Vec::new(),
        market_rows: Vec::new(),
        post_books: Vec::new(),
        dumps: Vec::new(),
        oracle: B256::ZERO,
        reloaded: Vec::new(),
        _dir: dir,
    };

    for (i, batch) in batches.iter().enumerate() {
        let height = (i + 1) as u64;
        let overlay = NativeStateOverlay::new(db.clone());
        let rec = Rec::new(overlay.clone());
        let mut ctx = new_ctx(rec.clone(), height, Some(&mut books));
        assert!(
            ctx.fatal_error.is_none(),
            "load {height}: {:?}",
            ctx.fatal_error
        );
        let _ = NativeExecutor::execute_batch(&mut ctx, batch);
        assert!(
            ctx.fatal_error.is_none(),
            "exec {height}: {:?}",
            ctx.fatal_error
        );
        r.dirty_present.push(
            ctx.dirty_books
                .iter()
                .filter(|m| ctx.order_books().contains_key(*m))
                .count(),
        );
        rec.take_log();
        let on = on_for(height);
        r.written
            .push(with_parallel_book_save(on, 0, || ctx.save_order_books()));
        assert!(
            ctx.fatal_error.is_none(),
            "save {height}: {:?}",
            ctx.fatal_error
        );
        r.put_seq.push(rec.take_log());
        r.workers.push(ctx.last_save_workers());
        let mut post: Vec<PostBook> = ctx
            .order_books()
            .iter()
            .map(|(mid, b)| {
                (
                    *mid,
                    borsh::to_vec(b).expect("book borsh"),
                    b.journaled_rows(),
                    b.journaled_levels(),
                )
            })
            .collect();
        post.sort_by_key(|p| p.0);
        for (mid, _, rows, levels) in &post {
            assert_eq!(
                (*rows, *levels),
                (0, 0),
                "block {height} market {mid}: journals left after save"
            );
        }
        ctx.stash_resident(&mut books);
        overlay
            .flush_with_native_trie_stats(
                &db,
                Some(height),
                None::<&mut NativeTrieCache>,
                None::<&mut NativeMemberCache>,
            )
            .expect("flush");
        r.roots.push(persisted_native_root(&db).unwrap());
        let book_rows = dump_cf(&db, CF_NATIVE_ORDER_BOOKS);
        // Every in-memory book's persisted blob is its borsh bytes.
        for (mid, bytes, _, _) in &post {
            let row = book_rows
                .iter()
                .find(|(k, _)| k.as_slice() == mid.to_be_bytes());
            assert_eq!(
                row.map(|(_, v)| v.as_slice()),
                Some(bytes.as_slice()),
                "block {height} market {mid}: blob != borsh::to_vec(book)"
            );
        }
        r.book_rows.push(book_rows);
        r.market_rows.push(dump_cf(&db, CF_NATIVE_MARKETS));
        r.post_books.push(post);
    }

    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRIE,
        CF_NATIVE_HASHED,
    ] {
        for (k, v) in dump_cf(&db, cf) {
            r.dumps.push((cf, k, v));
        }
    }
    r.oracle = native_root_full(&db).unwrap();

    // Cold reload (no resident holder): rebuilds every book from the blobs.
    let cold = new_ctx(db.clone(), blocks as u64 + 1, None);
    assert!(cold.fatal_error.is_none(), "reload: {:?}", cold.fatal_error);
    let mut reloaded: Vec<(MarketId, Vec<u8>)> = cold
        .order_books()
        .iter()
        .map(|(m, b)| (*m, borsh::to_vec(b).unwrap()))
        .collect();
    reloaded.sort_by_key(|p| p.0);
    let last: Vec<(MarketId, Vec<u8>)> = r
        .post_books
        .last()
        .unwrap()
        .iter()
        .map(|(m, b, _, _)| (*m, b.clone()))
        .collect();
    assert_eq!(reloaded, last, "cold reload != the resident books");
    r.reloaded = reloaded;
    r
}

fn assert_identical(label: &str, base: &RunResult, other: &RunResult) {
    assert_eq!(base.roots.len(), other.roots.len(), "{label}: block count");
    for h in 0..base.roots.len() {
        let b = h + 1;
        assert_eq!(
            base.written[h], other.written[h],
            "{label}: block {b} written"
        );
        assert_eq!(
            base.put_seq[h], other.put_seq[h],
            "{label}: block {b} put sequence"
        );
        assert_eq!(
            base.book_rows[h], other.book_rows[h],
            "{label}: block {b} book rows"
        );
        assert_eq!(
            base.market_rows[h], other.market_rows[h],
            "{label}: block {b} market rows"
        );
        assert_eq!(
            base.post_books[h], other.post_books[h],
            "{label}: block {b} post-save books"
        );
        assert_eq!(base.roots[h], other.roots[h], "{label}: block {b} root");
    }
    assert_eq!(base.dumps, other.dumps, "{label}: final CF dumps");
    assert_eq!(base.oracle, other.oracle, "{label}: full-scan oracle");
    assert_eq!(
        *other.roots.last().unwrap(),
        other.oracle,
        "{label}: root != oracle"
    );
    assert_eq!(base.reloaded, other.reloaded, "{label}: cold reload");
}

/// The serial run's put sequence is ascending by market id (the order the
/// parallel path must keep), so a reordered parallel put cannot match it.
fn assert_book_puts_ascending(label: &str, r: &RunResult) {
    for (h, seq) in r.put_seq.iter().enumerate() {
        let keys: Vec<&Vec<u8>> = seq
            .iter()
            .filter(|(cf, _)| *cf == CF_NATIVE_ORDER_BOOKS)
            .map(|(_, k)| k)
            .collect();
        assert!(
            keys.windows(2).all(|w| w[0] < w[1]),
            "{label}: block {} book puts not strictly ascending",
            h + 1
        );
    }
}

// ============================================================================
// 1. Serial vs parallel over a seeded adversarial universe
// ============================================================================

#[test]
fn classic_parallel_save_byte_identical_to_serial() {
    for seed in [0xC1A5_51C0_5A7Eu64, 0x0B00_C5A7_E5EE_D000] {
        let serial = run_universe(&|_| false, seed, 30, 60);
        assert!(
            serial.workers.iter().all(|&w| w == 1),
            "serial run must not engage"
        );
        assert_book_puts_ascending("serial", &serial);
        let par = run_universe(&|_| true, seed, 30, 60);
        let label = format!("seed {seed:#x}");
        assert_identical(&label, &serial, &par);
        assert_book_puts_ascending(&label, &par);
        // One job per present dirty book on every block with >= 2 of them.
        let mut engaged = 0;
        for (h, (&w, &d)) in par.workers.iter().zip(&par.dirty_present).enumerate() {
            if d >= 2 {
                assert_eq!(w, d, "{label}: block {} jobs != dirty books", h + 1);
                engaged += 1;
            } else {
                assert_eq!(w, 1, "{label}: block {} single book must be serial", h + 1);
            }
        }
        assert!(
            engaged >= 25,
            "{label}: engaged on only {engaged}/30 blocks: {:?}",
            par.workers
        );
        // The universe exercises what the blobs carry.
        assert!(
            par.written.iter().sum::<usize>() > 100,
            "{label}: {:?}",
            par.written
        );
    }
}

// ============================================================================
// 2. Flag alternating per block
// ============================================================================

#[test]
fn classic_parallel_save_alternating_equals_serial() {
    let seed = 0xA17E_C1A5_0001u64;
    let serial = run_universe(&|_| false, seed, 24, 60);
    let alt = run_universe(&|h| h % 2 == 0 || h % 5 == 0, seed, 24, 60);
    assert_identical("alternating", &serial, &alt);
    assert!(alt.workers.contains(&1));
    assert!(alt.workers.iter().any(|&w| w >= 2));
}

// ============================================================================
// 3. Injected put faults: same first fault, same attempts, same rows
// ============================================================================

struct FaultRun {
    written: usize,
    fatal: Option<String>,
    attempts: Vec<(&'static str, Vec<u8>)>,
    book_rows: Vec<(Vec<u8>, Vec<u8>)>,
    workers: usize,
}

fn fault_run(on: bool, fail: &[MarketId]) -> FaultRun {
    torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
    let (_dir, db) = open_test_db();
    {
        let ctx0 = new_ctx(db.clone(), 0, None);
        for t in 1..=N_TRADERS {
            fund_native(&ctx0, &addr(t), fp(100_000_000));
        }
    }
    build_native_trie_to_cf(&db).unwrap();
    let overlay = NativeStateOverlay::new(db.clone());
    let rec = Rec::new(overlay.clone());
    let mut ctx = new_ctx(rec.clone(), 1, None);
    let mut batch = Vec::new();
    for m in 1..=6u64 {
        for i in 0..(10 * m as i64) {
            let p = params(
                m,
                i % 2 == 0,
                if i % 2 == 0 { 90 - i % 5 } else { 110 + i % 5 },
                1,
                OrderType::Limit,
                TimeInForce::GTC,
                None,
            );
            batch.push(place(addr(1 + (i % N_TRADERS as i64) as u8), p));
        }
    }
    let res = NativeExecutor::execute_batch(&mut ctx, &batch);
    assert!(res.results.iter().all(|x| x.success), "setup");
    for m in fail {
        rec.fail_book_puts
            .lock()
            .unwrap()
            .push(m.to_be_bytes().to_vec());
    }
    rec.take_log();
    let written = with_parallel_book_save(on, 0, || ctx.save_order_books());
    let attempts = rec.take_log();
    let fatal = ctx.take_fatal_error();
    let workers = ctx.last_save_workers();
    overlay
        .flush_with_native_trie_stats(
            &db,
            Some(1),
            None::<&mut NativeTrieCache>,
            None::<&mut NativeMemberCache>,
        )
        .expect("flush");
    FaultRun {
        written,
        fatal,
        attempts,
        book_rows: dump_cf(&db, CF_NATIVE_ORDER_BOOKS),
        workers,
    }
}

#[test]
fn injected_put_fault_latches_the_same_first_fault() {
    let serial = fault_run(false, &[3, 5]);
    let par = fault_run(true, &[3, 5]);
    assert_eq!(serial.workers, 1);
    assert_eq!(par.workers, 6, "the parallel path engaged");
    let fatal = serial
        .fatal
        .clone()
        .expect("the serial save latches the put fault");
    assert!(
        fatal.starts_with("book save: order book put (market 3): ") && fatal.contains(INJECTED),
        "{fatal}"
    );
    assert_eq!(par.fatal, serial.fatal, "same first latched fault");
    assert_eq!(par.written, serial.written);
    assert_eq!(serial.written, 4, "the four other books are written");
    assert_eq!(
        par.attempts, serial.attempts,
        "same put attempts, same order"
    );
    assert_eq!(par.book_rows, serial.book_rows);
    assert_eq!(serial.book_rows.len(), 4);

    // Control: without faults both write all six books.
    let ok_s = fault_run(false, &[]);
    let ok_p = fault_run(true, &[]);
    assert!(ok_s.fatal.is_none() && ok_p.fatal.is_none());
    assert_eq!((ok_s.written, ok_p.written), (6, 6));
    assert_eq!(ok_s.book_rows, ok_p.book_rows);
}
