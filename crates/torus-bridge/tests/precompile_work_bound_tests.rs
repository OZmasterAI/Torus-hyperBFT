//! Review of fix/read-precompile-gas (blocking): a reader precompile's WORK is
//! bounded by the gas its caller pays, not only its charge. The EVM provider
//! hands each reader a [`ReadMeter`] of `gas_limit - GAS_PRECOMPILE_READ` gas;
//! a reader stops and returns `PrecompileOutOfGas` as soon as it would exceed
//! it. So a contract looping `staticcall{gas: base + small}` cannot make the
//! node scan a whole market list / book per call.
//!
//! Re-review: the charge (hence gas_used / out-of-gas, i.e. block results)
//! depends only on hashed consensus rows — never on the node-local
//! `__book_mode__` marker — a scan reads at most `rows allowed + 1` rows (+
//! the node-local rows it skips), and a classic blob is sized before it is
//! read.
//!
//! s99 owner decisions (final): a reader pays `GAS_PRECOMPILE_READ` (16,400:
//! single reads at HL level, getPosition = 16,500) + 500 per scanned row + 20
//! per returned word or 32 B blob chunk. getOpenOrders is removed;
//! getOrderBook answers the 64 best levels per side in modes 2/3, reverts on
//! a mode-1 market (unsupported), classic unchanged. RocksDB deletion markers
//! in a scanned range change neither gas nor answer (node-local), and block
//! flushes that delete level rows of a market compact them in the background
//! once enough accumulate.

#[path = "common/counting_backend.rs"]
mod counting_backend;

use std::sync::atomic::Ordering;

use alloy_primitives::{Address, U256};
use counting_backend::CountingBackend;
use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
use torus_core::error::CoreError;
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, reader_gas, ReadMeter, ADDR_BALANCE_READER,
    ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER, GAS_PRECOMPILE_READ,
    GAS_PRECOMPILE_READ_PER_ROW, GAS_PRECOMPILE_READ_PER_WORD, READER_MAX_LEVELS_PER_SIDE,
};
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_STAKING_DELEGATIONS,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const ROW: u64 = GAS_PRECOMPILE_READ_PER_ROW;
const WORD: u64 = GAS_PRECOMPILE_READ_PER_WORD;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn call(
    state: &impl StateBackend,
    id: u16,
    sig: &str,
    words: &[[u8; 32]],
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let mut input = alloy_primitives::keccak256(sig.as_bytes())[..4].to_vec();
    for w in words {
        input.extend_from_slice(w);
    }
    execute_precompile_metered(
        &precompile_address(id),
        &input,
        &Address::ZERO,
        U256::ZERO,
        state,
        100,
        0,
        false,
        meter,
    )
}

/// The answer and the gas a call charges (`reader_gas` of its meter).
fn gas_of(state: &impl StateBackend, id: u16, sig: &str, words: &[[u8; 32]]) -> (Vec<u8>, u64) {
    let mut m = ReadMeter::with_max(30_000_000);
    let out = call(state, id, sig, words, &mut m).expect("reader answers");
    (out, reader_gas(m.used()))
}

fn market_word(m: MarketId) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&m.to_be_bytes());
    w
}

fn addr_word(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

fn oog(r: &Result<Vec<u8>, CoreError>) -> bool {
    matches!(r, Err(CoreError::PrecompileOutOfGas))
}

fn rows_read<T: StateBackend>(cb: &CountingBackend<T>) -> usize {
    cb.counts.rows_read.load(Ordering::SeqCst)
}

/// A meter allowing `n` scanned rows (and no words).
fn rows(n: u64) -> ReadMeter {
    ReadMeter::with_max(n * ROW)
}

fn order_book(state: &impl StateBackend, meter: &mut ReadMeter) -> Result<Vec<u8>, CoreError> {
    call(state, ADDR_ORDER_BOOK_READER, "getOrderBook(bytes32)", &[market_word(1)], meter)
}

/// The first `n` dynamic arrays of a reader answer, low 16 bytes per element.
fn arrays(out: &[u8], n: usize) -> Vec<Vec<u128>> {
    let word = |at: usize| u128::from_be_bytes(out[at + 16..at + 32].try_into().unwrap());
    (0..n)
        .map(|i| {
            let off = word(i * 32) as usize;
            (0..word(off) as usize)
                .map(|j| word(off + 32 + j * 32))
                .collect()
        })
        .collect()
}

fn raw(v: i64) -> u128 {
    fp(v).raw() as u128
}

/// RocksDB tombstones the current thread's iterators skipped during `f`.
fn deletes_skipped<R>(f: impl FnOnce() -> R) -> (R, u64) {
    set_perf_stats(PerfStatsLevel::EnableCount);
    let mut ctx = PerfContext::default();
    ctx.reset();
    let r = f();
    let n = ctx.metric(PerfMetric::InternalDeleteSkippedCount);
    set_perf_stats(PerfStatsLevel::Disable);
    (r, n)
}

fn ctx_on<T: StateBackend + Clone>(state: T, height: u64, mode: BookMode) -> NativeExecContext<T> {
    NativeExecContext::new_with_mode(
        state,
        height,
        1_000 + height,
        0,
        100,
        10,
        Address::new([99; 20]),
        Address::new([100; 20]),
        Address::new([101; 20]),
        mode,
        None,
    )
}

const MAKER: Address = Address::new([1; 20]);

fn place(is_buy: bool, price: i64) -> (Address, NativeAction) {
    (
        MAKER,
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: fp(price),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

fn run_block<T: StateBackend + Clone>(
    ctx: &mut NativeExecContext<T>,
    block: &[(Address, NativeAction)],
) {
    let r = NativeExecutor::execute_batch(ctx, block);
    assert!(
        r.results.iter().all(|x| x.success),
        "block failed: {:?}",
        r.results.iter().find(|x| !x.success).map(|x| &x.error)
    );
    ctx.save_order_books();
}

/// MAKER rests one bid per entry of `bids` and one ask per entry of `asks` on
/// market 1 under `mode`, saved with the real `save_order_books` (which writes
/// the node-local `__book_mode__` marker).
fn seed_sides(db: &StateDb, mode: BookMode, bids: &[i64], asks: &[i64]) {
    let mut ctx = ctx_on(db.clone(), 1, mode);
    ctx.positions
        .put_native_balance(
            &MAKER,
            &NativeBalance {
                available: fp(100_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let block: Vec<_> = bids
        .iter()
        .map(|&p| place(true, p))
        .chain(asks.iter().map(|&p| place(false, p)))
        .collect();
    run_block(&mut ctx, &block);
}

fn seed_book(db: &StateDb, mode: BookMode, prices: &[i64]) {
    seed_sides(db, mode, prices, &[]);
}

fn levels(n: i64) -> Vec<i64> {
    (1..=n).collect()
}

/// MAKER's resting orders on market 1 below `price` (the churn the tests add).
fn churn_ids<T: StateBackend + Clone>(ctx: &NativeExecContext<T>, below: i64) -> Vec<u128> {
    let book = ctx.order_books.get(&1).expect("book loaded");
    book.orders_for_trader(&MAKER)
        .iter()
        .filter(|o| o.price < fp(below))
        .map(|o| o.id)
        .collect()
}

/// getMarkets over 5,000 markets with a 20-row budget: out of gas after
/// reading at most 21 rows (it used to read all 5,000 for 2,600 gas). With a
/// large budget the answer equals the unmetered one and the meter holds
/// 500 per row read + 20 per word returned.
#[test]
fn get_markets_work_is_bounded_by_the_budget() {
    let (_dir, db) = open_test_db();
    for m in 1..=5_000u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8])
            .unwrap();
    }
    let cb = CountingBackend::new(db.clone());

    let r = call(&cb, ADDR_BALANCE_READER, "getMarkets()", &[], &mut rows(20));
    assert!(oog(&r), "{r:?}");
    assert!(
        rows_read(&cb) <= 21,
        "read {} rows on a 20-row budget",
        rows_read(&cb)
    );

    let want = call(
        &db,
        ADDR_BALANCE_READER,
        "getMarkets()",
        &[],
        &mut ReadMeter::unlimited(),
    )
    .unwrap();
    let mut big = ReadMeter::with_max(30_000_000);
    assert_eq!(
        call(&db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut big).unwrap(),
        want
    );
    assert_eq!(
        big.used(),
        5_000 * ROW + (4 + 2 * 5_000) * WORD,
        "rows read + words returned"
    );
}

/// getOrderBook on a 150-level (mode 2) book: a 20-row budget runs out of gas
/// after at most 21 rows; a large budget returns the best 64 bid levels.
#[test]
fn get_order_book_work_is_bounded_by_the_budget() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &levels(150));
    let cb = CountingBackend::new(db.clone());

    let r = order_book(&cb, &mut rows(20));
    assert!(oog(&r), "{r:?}");
    assert!(
        rows_read(&cb) <= 21,
        "read {} rows on a 20-row budget",
        rows_read(&cb)
    );

    let mut big = ReadMeter::with_max(30_000_000);
    let out = order_book(&db, &mut big).unwrap();
    assert_eq!(
        out.len(),
        (8 + 64 * 2) * 32,
        "the best 64 bid levels, no asks"
    );
    assert_eq!(out, order_book(&db, &mut ReadMeter::unlimited()).unwrap());
    assert_eq!(big.used(), 64 * ROW + (8 + 128) * WORD);
}

/// A stipend below the answer's fixed head (8 words) reads nothing at all.
#[test]
fn get_order_book_with_no_budget_reads_nothing() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &levels(10));
    let cb = CountingBackend::new(db.clone());
    let r = order_book(&cb, &mut ReadMeter::with_max(8 * WORD - 1));
    assert!(oog(&r), "{r:?}");
    assert_eq!(rows_read(&cb), 0);
}

/// B1 (determinism): the node-local `__book_mode__` marker (not hashed,
/// absent after a restore) must not change a reader's answer or charge —
/// gas_used / out-of-gas are block results. Same getOrderBook and getMarkets
/// with and without it, and without it the work stays bounded.
#[test]
fn the_node_local_marker_changes_neither_gas_nor_answer() {
    for mode in [BookMode::LevelAuthority, BookMode::OrderRows, BookMode::Classic] {
        let (_dir, db) = open_test_db();
        seed_book(&db, mode, &levels(30));
        for m in 1..=3u64 {
            db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8]).unwrap();
        }
        assert!(db.get_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY).unwrap().is_some(), "{mode:?}");

        let run = |db: &StateDb| {
            let mut mb = ReadMeter::with_max(30_000_000);
            let book = order_book(db, &mut mb);
            let mut mm = ReadMeter::with_max(30_000_000);
            let markets = call(db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut mm).unwrap();
            (book.map_err(|e| e.to_string()), mb.used(), markets, mm.used())
        };
        let with = run(&db);
        db.delete_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY).unwrap();
        let without = run(&db);
        assert_eq!(with, without, "{mode:?}: marker changed the answer or the charge");
        // Charged rows = the hashed rows of cf_native_markets (the 3 markets +
        // the executor's consensus rows), never the marker.
        let hashed = db.iterate_cf(CF_NATIVE_MARKETS, None).unwrap().len() as u64;
        assert_eq!(
            with.3,
            hashed * ROW + (4 + 2 * 3) * WORD,
            "{mode:?}: getMarkets charges hashed rows only"
        );

        let cb = CountingBackend::new(db.clone());
        assert!(
            oog(&order_book(&cb, &mut ReadMeter::with_max(12 * WORD))),
            "{mode:?}"
        );
        assert!(
            rows_read(&cb) <= 1,
            "{mode:?}: read {} rows without the marker",
            rows_read(&cb)
        );
    }
}

/// Classic layout (the default, TORUS_BOOK_ROWS unset): the whole-book blob is
/// sized with a length probe and charged (20 gas per 32 bytes) BEFORE it is
/// read. An under-budget call reads no value bytes at all; with enough budget
/// the production blob still fails to decode, and the revert pays for the
/// blob.
#[test]
fn classic_blob_is_sized_and_charged_before_it_is_read() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::Classic, &levels(50));
    let chunks = db
        .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes())
        .unwrap()
        .expect("classic blob")
        .len()
        .div_ceil(32) as u64;
    assert!(chunks > 50, "a 50-order blob is several hundred bytes");

    let cb = CountingBackend::new(db.clone());
    let r = order_book(&cb, &mut ReadMeter::with_max((chunks - 1) * WORD));
    assert!(oog(&r), "blob larger than the budget: out of gas, got {r:?}");
    assert_eq!(cb.counts.bytes_read.load(Ordering::SeqCst), 0, "no blob byte read under budget");

    let mut big = ReadMeter::with_max(30_000_000);
    let r = order_book(&db, &mut big);
    assert!(
        matches!(r, Err(CoreError::DeterministicDecode(_))),
        "production blob still reverts: {r:?}"
    );
    // R02 branch 3: deterministic (not a local fault), same text as `Borsh`.
    let e = r.unwrap_err();
    assert!(!e.is_local_fault());
    assert_eq!(e.to_string(), "borsh error: Not all bytes read");
    assert_eq!(
        big.used(),
        chunks * WORD,
        "the revert pays for the blob it read"
    );
}

/// Final review N1: a mode-2 market whose last order was cancelled keeps only
/// its meta row (no level rows). It reads back exactly like a market that
/// never had a book: the empty 8-word answer, metered = unmetered.
#[test]
fn a_mode2_market_with_only_a_meta_row_reads_like_an_empty_book() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &[100]);
    let mut ctx = ctx_on(db.clone(), 2, BookMode::LevelAuthority);
    let ids = churn_ids(&ctx, 1_000);
    assert_eq!(ids.len(), 1);
    run_block(
        &mut ctx,
        &[(MAKER, NativeAction::CancelOrder { order_id: ids[0] })],
    );

    let rows = db.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&1u64.to_be_bytes())).unwrap();
    assert!(!rows.is_empty(), "the meta row stays");
    assert!(rows.iter().all(|(k, _)| k.len() != 26), "no level rows left: {rows:?}");

    let mut metered = ReadMeter::with_max(30_000_000);
    let out = order_book(&db, &mut metered).unwrap();
    assert_eq!(out, order_book(&db, &mut ReadMeter::unlimited()).unwrap());
    let never = call(
        &db,
        ADDR_ORDER_BOOK_READER,
        "getOrderBook(bytes32)",
        &[market_word(2)],
        &mut ReadMeter::with_max(30_000_000),
    )
    .unwrap();
    assert_eq!(out, never, "same answer as a market that never had a book");
    assert_eq!(out.len(), 8 * 32);
}

/// Final review N2: the bounds hold over the EVM's real backend, a
/// `NativeStateOverlay` journal with its own pending writes / deletes, and the
/// metered answer equals the unmetered one there.
#[test]
fn bounds_hold_over_the_evm_overlay_journal() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &levels(150));
    for m in 1..=500u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8]).unwrap();
    }
    let ov = NativeStateOverlay::new(db.clone());
    ov.put_cf_raw(CF_NATIVE_MARKETS, &501u64.to_be_bytes(), &[1u8]).unwrap();
    ov.delete_cf_raw(CF_NATIVE_MARKETS, &7u64.to_be_bytes()).unwrap();

    let cb = CountingBackend::new(ov.clone());
    assert!(oog(&call(
        &cb,
        ADDR_BALANCE_READER,
        "getMarkets()",
        &[],
        &mut rows(20)
    )));
    assert!(
        rows_read(&cb) <= 21,
        "read {} rows on a 20-row budget",
        rows_read(&cb)
    );

    let cb = CountingBackend::new(ov.clone());
    assert!(oog(&order_book(&cb, &mut rows(20))));
    assert!(
        rows_read(&cb) <= 21,
        "read {} rows on a 20-row budget",
        rows_read(&cb)
    );

    let mut big = ReadMeter::with_max(30_000_000);
    let markets = call(&ov, ADDR_BALANCE_READER, "getMarkets()", &[], &mut big).unwrap();
    assert_eq!(markets, call(&ov, ADDR_BALANCE_READER, "getMarkets()", &[], &mut ReadMeter::unlimited()).unwrap());
    let hashed = ov
        .iterate_cf(CF_NATIVE_MARKETS, None)
        .unwrap()
        .iter()
        .filter(|(k, _)| k.as_slice() != BOOK_MODE_MARKER_KEY)
        .count() as u64;
    assert_eq!(
        big.used(),
        hashed * ROW + (4 + 2 * 500) * WORD,
        "500 markets (+1 pending, -1 deleted)"
    );

    let mut big = ReadMeter::with_max(30_000_000);
    assert_eq!(order_book(&ov, &mut big).unwrap(), order_book(&ov, &mut ReadMeter::unlimited()).unwrap());
}

// ---------------------------------------------------------------------------
// s99 owner pricing: exact gas.
// ---------------------------------------------------------------------------

/// (a) Single reads at HL level: the base is 16,400 and a single reader pays
/// only its answer's words on top (its point reads are in the base):
/// getPosition 16,500, getBalances 16,480, getPrice 16,460, getStakingInfo
/// 16,480 with no delegation (+500 per delegation row it scans).
#[test]
fn single_readers_cost_the_base_plus_their_words() {
    assert_eq!(GAS_PRECOMPILE_READ, 16_400);
    assert_eq!((ROW, WORD), (500, 20));
    let (_dir, db) = open_test_db();
    let t = Address::new([7; 20]);
    let pos = Position {
        trader: t,
        market_id: 1,
        is_long: true,
        size: fp(3),
        entry_price: fp(100),
        cost_basis: fp(300),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    };
    db.put_cf_raw(
        CF_NATIVE_POSITIONS,
        &position_key(&t, 1),
        &borsh::to_vec(&pos).unwrap(),
    )
    .unwrap();
    let bal = NativeBalance {
        available: fp(10),
        order_margin: FixedPoint::ZERO,
    };
    db.put_cf_raw(
        CF_NATIVE_BALANCES,
        t.as_slice(),
        &borsh::to_vec(&bal).unwrap(),
    )
    .unwrap();
    let mut agg = fp(100).raw().to_be_bytes().to_vec();
    agg.extend_from_slice(&[0u8; 20]);
    db.put_cf_raw(
        CF_NATIVE_ORACLE,
        &[b"agg".as_slice(), &1u64.to_be_bytes()].concat(),
        &agg,
    )
    .unwrap();

    let gas = |id, sig, words: &[[u8; 32]]| gas_of(&db, id, sig, words).1;
    let pos_args = [addr_word(&t), market_word(1)];
    assert_eq!(
        gas(
            ADDR_ORDER_BOOK_READER,
            "getPosition(address,bytes32)",
            &pos_args
        ),
        16_500
    );
    let none = [addr_word(&Address::new([8; 20])), market_word(1)];
    assert_eq!(
        gas(
            ADDR_ORDER_BOOK_READER,
            "getPosition(address,bytes32)",
            &none
        ),
        16_500,
        "no position: same 5 words"
    );
    assert_eq!(
        gas(
            ADDR_BALANCE_READER,
            "getBalances(address)",
            &[addr_word(&t)]
        ),
        16_480
    );
    assert_eq!(
        gas(ADDR_ORACLE_READER, "getPrice(bytes32)", &[market_word(1)]),
        16_460
    );
    assert_eq!(
        gas(
            ADDR_STAKING_READER,
            "getStakingInfo(address)",
            &[addr_word(&t)]
        ),
        16_480
    );
    for v in 1..=2u8 {
        let validator = Address::new([0x50 + v; 20]);
        let mut row = t.as_slice().to_vec();
        row.extend_from_slice(validator.as_slice());
        row.extend_from_slice(&U256::from(1_000u64).to_be_bytes::<32>());
        db.put_cf_raw(
            CF_STAKING_DELEGATIONS,
            &[t.as_slice(), validator.as_slice()].concat(),
            &row,
        )
        .unwrap();
    }
    assert_eq!(
        gas(
            ADDR_STAKING_READER,
            "getStakingInfo(address)",
            &[addr_word(&t)]
        ),
        16_480 + 2 * 500
    );

    // The exact boundary: the meter holds the gas above the base.
    let mut exact = ReadMeter::with_max(100);
    assert!(call(
        &db,
        ADDR_ORDER_BOOK_READER,
        "getPosition(address,bytes32)",
        &pos_args,
        &mut exact
    )
    .is_ok());
    let r = call(
        &db,
        ADDR_ORDER_BOOK_READER,
        "getPosition(address,bytes32)",
        &pos_args,
        &mut ReadMeter::with_max(99),
    );
    assert!(oog(&r), "{r:?}");
}

/// (b) Scans: 500 per scanned row + 20 per returned word. getOrderBook (mode
/// 2) over 3 bid + 2 ask levels = 5 rows + 18 words.
#[test]
fn scans_cost_500_per_row_and_20_per_word() {
    let (_dir, db) = open_test_db();
    seed_sides(&db, BookMode::LevelAuthority, &[10, 11, 12], &[20, 21]);
    let (out, gas) = gas_of(
        &db,
        ADDR_ORDER_BOOK_READER,
        "getOrderBook(bytes32)",
        &[market_word(1)],
    );
    assert_eq!(
        arrays(&out, 4)[0],
        vec![raw(12), raw(11), raw(10)],
        "bids best-first"
    );
    assert_eq!(
        arrays(&out, 4)[2],
        vec![raw(20), raw(21)],
        "asks best-first"
    );
    assert_eq!(gas, 16_400 + 5 * 500 + 18 * 20);
}

/// (b) Cap, modes 2/3: the 64 best levels per side. 64 levels per side answer
/// all of them; 65 per side answer the best 64 of each, at the same gas:
/// 16,400 + 128 x 500 + (8 + 4 x 64) x 20 = 85,680. That exact gas limit
/// answers; one gas less runs out of gas, never a shorter answer.
#[test]
fn order_book_answers_the_64_best_levels_per_side() {
    assert_eq!(READER_MAX_LEVELS_PER_SIDE, 64);
    let exact = 128 * ROW + (8 + 4 * 64) * WORD;
    assert_eq!(GAS_PRECOMPILE_READ + exact, 85_680);
    for n in [64i64, 65] {
        let (_dir, db) = open_test_db();
        let bids: Vec<i64> = (1..=n).collect();
        let asks: Vec<i64> = (1_001..=1_000 + n).collect();
        seed_sides(&db, BookMode::LevelAuthority, &bids, &asks);
        let cb = CountingBackend::new(db.clone());
        let mut m = ReadMeter::with_max(30_000_000);
        let out = order_book(&cb, &mut m).unwrap();
        let a = arrays(&out, 4);
        assert_eq!(
            a[0],
            (n - 63..=n).rev().map(raw).collect::<Vec<_>>(),
            "n={n}: best 64 bids, best first"
        );
        assert_eq!(
            a[2],
            (1_001..=1_064).map(raw).collect::<Vec<_>>(),
            "n={n}: best 64 asks, best first"
        );
        assert_eq!(m.used(), exact, "n={n}");
        assert!(rows_read(&cb) <= 128, "n={n}: read {} rows", rows_read(&cb));
        assert_eq!(out, order_book(&db, &mut ReadMeter::unlimited()).unwrap());

        assert_eq!(
            order_book(&db, &mut ReadMeter::with_max(exact)).unwrap(),
            out
        );
        assert!(
            oog(&order_book(&db, &mut ReadMeter::with_max(exact - 1))),
            "n={n}"
        );
        assert!(
            oog(&order_book(&db, &mut rows(127))),
            "n={n}: 127 rows of gas"
        );
    }
}

/// s99 owner decision (final): mode 1 (order rows by id, no price index) is
/// not served. A mode-1 market with resting orders reverts, deterministically:
/// the two empty level scans read nothing, one order row is probed and
/// charged, so the revert costs 16,400 + 500 = 16,900 gas. A mode-1 market
/// whose orders are all gone (meta row only) answers the empty book.
#[test]
fn order_book_reverts_on_a_mode1_market() {
    let (_dir, db) = open_test_db();
    let prices: Vec<i64> = (0..30).map(|i| 10 + i % 3).collect();
    seed_book(&db, BookMode::OrderRows, &prices);

    let cb = CountingBackend::new(db.clone());
    let mut m = ReadMeter::with_max(30_000_000);
    let r = order_book(&cb, &mut m);
    assert!(
        matches!(r, Err(CoreError::BookLayout(_))),
        "mode 1 reverts: {r:?}"
    );
    assert_eq!(
        reader_gas(m.used()),
        16_900,
        "the revert pays the one probed row"
    );
    assert_eq!(rows_read(&cb), 1);
    assert_eq!(m.used(), {
        let mut again = ReadMeter::with_max(30_000_000);
        let _ = order_book(&db, &mut again);
        again.used()
    });
    // The head is checked, not charged: exactly one row of gas reaches the
    // probe and the revert; one gas less runs out of gas.
    let r = order_book(&db, &mut ReadMeter::with_max(ROW));
    assert!(matches!(r, Err(CoreError::BookLayout(_))), "{r:?}");
    assert!(oog(&order_book(&db, &mut ReadMeter::with_max(ROW - 1))));

    let mut ctx = ctx_on(db.clone(), 2, BookMode::OrderRows);
    let ids = churn_ids(&ctx, 1_000);
    let cancels: Vec<_> = ids
        .iter()
        .map(|&order_id| (MAKER, NativeAction::CancelOrder { order_id }))
        .collect();
    run_block(&mut ctx, &cancels);
    let out = order_book(&db, &mut ReadMeter::with_max(30_000_000)).unwrap();
    assert_eq!(out.len(), 8 * 32, "no order row left: the empty book");
}

/// A key of the wrong length under a market's level prefix (a corrupt row
/// store) reverts instead of being skipped.
#[test]
fn a_wrong_length_level_key_reverts() {
    let (_dir, db) = open_test_db();
    seed_sides(&db, BookMode::LevelAuthority, &[10, 11], &[20]);
    let mut key =
        torus_core::book_rows::level_row_key_tagged(1, torus_core::book_rows::SIDE_TAG_BID, 1)
            .to_vec();
    key.push(0);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, b"junk").unwrap();
    let r = order_book(&db, &mut ReadMeter::with_max(30_000_000));
    // R02 branch 3: a corrupt row on this node (a local fault); same revert text.
    assert!(matches!(r, Err(CoreError::BookCorrupt(_))), "{r:?}");
}

// ---------------------------------------------------------------------------
// (c) Deletion markers: node-local, never in a block result.
// ---------------------------------------------------------------------------

/// MAKER places one bid at each of `prices` in one block and cancels them all
/// in the next (mode 2: level rows written, then deleted), on the DB directly.
fn churn_levels(db: &StateDb, prices: &[i64]) {
    let mut ctx = ctx_on(db.clone(), 2, BookMode::LevelAuthority);
    let block: Vec<_> = prices.iter().map(|&p| place(true, p)).collect();
    run_block(&mut ctx, &block);
    db.inner()
        .flush_cf(db.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap())
        .unwrap();
    let mut ctx = ctx_on(db.clone(), 3, BookMode::LevelAuthority);
    let ids: Vec<u128> = {
        let book = ctx.order_books.get(&1).expect("book");
        book.orders_for_trader(&MAKER)
            .iter()
            .filter(|o| o.price > fp(100))
            .map(|o| o.id)
            .collect()
    };
    assert_eq!(ids.len(), prices.len());
    let cancels: Vec<_> = ids
        .iter()
        .map(|&order_id| (MAKER, NativeAction::CancelOrder { order_id }))
        .collect();
    run_block(&mut ctx, &cancels);
}

/// The same logical book with and without RocksDB tombstones in the scanned
/// range (200 better bid levels written and deleted by real place + cancel
/// churn, flushed to an SST, never compacted) gives the same answer and gas.
#[test]
fn tombstones_change_neither_gas_nor_answer() {
    let prices: Vec<i64> = (101..=300).collect();
    let build = |churn: bool| {
        let (dir, db) = open_test_db();
        seed_book(&db, BookMode::LevelAuthority, &[10, 11, 12]);
        if churn {
            churn_levels(&db, &prices);
            db.inner()
                .flush_cf(db.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap())
                .unwrap();
        }
        let ((out, gas), skipped) = deletes_skipped(|| {
            gas_of(
                &db,
                ADDR_ORDER_BOOK_READER,
                "getOrderBook(bytes32)",
                &[market_word(1)],
            )
        });
        (dir, out, gas, skipped)
    };
    let (_d1, out_a, gas_a, skipped_a) = build(true);
    let (_d2, out_b, gas_b, skipped_b) = build(false);
    assert!(
        skipped_a >= 200,
        "the scan steps over the deleted levels ({skipped_a})"
    );
    assert_eq!(skipped_b, 0);
    assert_eq!(arrays(&out_b, 4)[0], vec![raw(12), raw(11), raw(10)]);
    assert_eq!(
        (out_a, gas_a),
        (out_b, gas_b),
        "tombstones changed the answer or the gas"
    );
}

/// 18c review (missing test): the same deletes held by the current block's
/// overlay or by the frozen parent block's set (the pipelined path's layer,
/// read between the overlay and the DB) give the clean book's answer and gas:
/// 200 better bid levels, durable in the DB, cancelled in block 3.
#[test]
fn parent_and_current_block_deletes_change_neither_gas_nor_answer() {
    let book = |state: &dyn Fn(&mut ReadMeter) -> Result<Vec<u8>, CoreError>| {
        let mut m = ReadMeter::with_max(30_000_000);
        (state(&mut m).expect("reader answers"), reader_gas(m.used()))
    };
    let (_d0, clean) = open_test_db();
    seed_book(&clean, BookMode::LevelAuthority, &[10, 11, 12]);
    let want = book(&|m| order_book(&clean, m));
    assert_eq!(arrays(&want.0, 4)[0], vec![raw(12), raw(11), raw(10)]);

    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &[10, 11, 12]);
    let mut ctx = ctx_on(db.clone(), 2, BookMode::LevelAuthority);
    run_block(
        &mut ctx,
        &(101..=300).map(|p| place(true, p)).collect::<Vec<_>>(),
    );
    drop(ctx);
    let ov = NativeStateOverlay::new(db.clone());
    let mut ctx = ctx_on(ov.clone(), 3, BookMode::LevelAuthority);
    let ids: Vec<u128> = {
        let book = ctx.order_books.get(&1).expect("book");
        book.orders_for_trader(&MAKER)
            .iter()
            .filter(|o| o.price > fp(100))
            .map(|o| o.id)
            .collect()
    };
    assert_eq!(ids.len(), 200);
    run_block(
        &mut ctx,
        &ids.iter()
            .map(|&order_id| (MAKER, NativeAction::CancelOrder { order_id }))
            .collect::<Vec<_>>(),
    );
    drop(ctx);
    assert_ne!(
        book(&|m| order_book(&db, m)),
        want,
        "the DB alone still holds the 200 levels"
    );

    assert_eq!(
        book(&|m| order_book(&ov, m)),
        want,
        "deletes in the current block"
    );
    let child = NativeStateOverlay::with_parent(db.clone(), Some(ov.freeze(3)));
    assert_eq!(child.parent_height(), Some(3));
    assert_eq!(
        book(&|m| order_book(&child, m)),
        want,
        "deletes in the frozen parent"
    );
}

/// (c) Node-local mitigation (s89 fix B pattern, per market, threshold): a
/// block flush that deletes at least the threshold of level rows in a market
/// compacts that market's span in the background, so the next scans step over
/// no tombstone; answer and gas unchanged.
#[test]
fn a_block_flush_that_deletes_level_rows_compacts_them_away() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &[10, 11, 12]);
    let prices: Vec<i64> = (101..=400).collect();
    let runs_before = db.wait_background_compaction();

    // Block 2: 300 better bid levels; block 3: cancel them; both flushed
    // through the block overlay like the node.
    let ov = NativeStateOverlay::new(db.clone());
    let mut ctx = ctx_on(ov.clone(), 2, BookMode::LevelAuthority);
    run_block(
        &mut ctx,
        &prices.iter().map(|&p| place(true, p)).collect::<Vec<_>>(),
    );
    drop(ctx);
    ov.flush_with_native_trie_stats(&db, None, None, None)
        .unwrap();
    db.inner()
        .flush_cf(db.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap())
        .unwrap();

    let ov = NativeStateOverlay::new(db.clone());
    let mut ctx = ctx_on(ov.clone(), 3, BookMode::LevelAuthority);
    let ids = {
        let book = ctx.order_books.get(&1).expect("book");
        book.orders_for_trader(&MAKER)
            .iter()
            .filter(|o| o.price > fp(100))
            .map(|o| o.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids.len(), 300);
    run_block(
        &mut ctx,
        &ids.iter()
            .map(|&order_id| (MAKER, NativeAction::CancelOrder { order_id }))
            .collect::<Vec<_>>(),
    );
    drop(ctx);
    let book_before = order_book(&ov, &mut ReadMeter::with_max(30_000_000)).unwrap();
    ov.flush_with_native_trie_stats(&db, None, None, None)
        .unwrap();
    let (done, failed) = db.wait_background_compaction();
    assert_eq!(failed, runs_before.1, "no compaction failed");
    assert!(
        done > runs_before.0,
        "the delete flush scheduled a compaction ({runs_before:?} -> {done})"
    );

    let ((book, gas), skipped) = deletes_skipped(|| {
        gas_of(
            &db,
            ADDR_ORDER_BOOK_READER,
            "getOrderBook(bytes32)",
            &[market_word(1)],
        )
    });
    assert_eq!(
        skipped, 0,
        "getOrderBook still walks {skipped} tombstones after the compaction"
    );
    assert_eq!(book, book_before);
    assert_eq!(arrays(&book, 4)[0], vec![raw(12), raw(11), raw(10)]);
    assert_eq!(gas, 16_400 + 3 * ROW + (8 + 6) * WORD);
}
