//! Review of fix/read-precompile-gas (blocking): a reader precompile's WORK is
//! bounded by the gas its caller pays, not only its charge. The EVM provider
//! hands each reader a [`ReadMeter`] of `gas_limit - GAS_PRECOMPILE_READ` gas;
//! a reader stops and returns `PrecompileOutOfGas` as soon as it would exceed
//! it. So a contract looping `staticcall{gas: base + small}` cannot make the
//! node scan a whole market list per call.
//!
//! Re-review: the charge (hence gas_used / out-of-gas, i.e. block results)
//! depends only on hashed consensus rows — never on the node-local
//! `__book_mode__` marker — and a scan reads at most `rows allowed + 1` rows
//! (+ the node-local rows it skips).
//!
//! s99 owner decisions (final): a reader pays `GAS_PRECOMPILE_READ` (16,400:
//! single reads at HL level, getPosition = 16,500) + 500 per scanned row + 20
//! per returned word. getOpenOrders is removed (s99), and so is getOrderBook
//! (item 7 step 1, owner s122).

#[path = "common/counting_backend.rs"]
mod counting_backend;

use std::sync::atomic::Ordering;

use alloy_primitives::{Address, U256};
use counting_backend::CountingBackend;
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
use torus_core::error::CoreError;
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, reader_gas, ReadMeter, ADDR_BALANCE_READER,
    ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER, GAS_PRECOMPILE_READ,
    GAS_PRECOMPILE_READ_PER_ROW, GAS_PRECOMPILE_READ_PER_WORD,
};
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS,
    CF_STAKING_DELEGATIONS,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const ROW: u64 = GAS_PRECOMPILE_READ_PER_ROW;
const WORD: u64 = GAS_PRECOMPILE_READ_PER_WORD;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
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
    // Pre-testnet P0: orders need a listed market (a placeholder row where
    // the test has none; the tests count the market rows they charge for).
    if db.get_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes()).unwrap().is_none() {
        db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), &[1u8]).unwrap();
    }
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

/// B1 (determinism): the node-local `__book_mode__` marker (not hashed,
/// absent after a restore) must not change a reader's answer or charge —
/// gas_used / out-of-gas are block results. Same getMarkets with and
/// without it.
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
            let mut mm = ReadMeter::with_max(30_000_000);
            let markets = call(db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut mm).unwrap();
            (markets, mm.used())
        };
        let with = run(&db);
        db.delete_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY).unwrap();
        let without = run(&db);
        assert_eq!(with, without, "{mode:?}: marker changed the answer or the charge");
        // Charged rows = the hashed rows of cf_native_markets (the 3 markets +
        // the executor's consensus rows), never the marker.
        let hashed = db.iterate_cf(CF_NATIVE_MARKETS, None).unwrap().len() as u64;
        assert_eq!(
            with.1,
            hashed * ROW + (4 + 2 * 3) * WORD,
            "{mode:?}: getMarkets charges hashed rows only"
        );
    }
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
        funding: None,
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

/// (b) Scans: 500 per scanned row + 20 per returned word. getMarkets over 3
/// markets = 3 rows + 10 words (a 4-word head, 2 words per market).
#[test]
fn scans_cost_500_per_row_and_20_per_word() {
    let (_dir, db) = open_test_db();
    for m in 1..=3u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8])
            .unwrap();
    }
    let (out, gas) = gas_of(&db, ADDR_BALANCE_READER, "getMarkets()", &[]);
    assert_eq!(out.len(), (4 + 2 * 3) * 32);
    assert_eq!(gas, 16_400 + 3 * 500 + 10 * 20);
}
