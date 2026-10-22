//! Integration tests for cross-VM precompiles, lockbox, and CoreWriterQueue (task 2.4).

use alloy_primitives::keccak256;
use torus_core::error::CoreError;
use torus_core::lockbox::Lockbox;
use torus_core::position::{NativeBalance, Position, PositionManager};
use torus_core::precompiles::*;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{Address, FixedPoint, MarketId, U256};

// ============================================================================
// Helpers
// ============================================================================

fn setup() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, db)
}

/// [`setup`] with the CoreWriter order delay K pinned to 1 (item 7 step 2): for the tests
/// whose subject is not the delay and that expect an order kind at `block_queued + 1`.
fn setup_k1() -> (tempfile::TempDir, StateDb) {
    let (dir, db) = setup();
    db.put_cf_raw(
        torus_state::cf::CF_FEE_CONFIG,
        b"core_writer_order_delay",
        b"1",
    )
    .unwrap();
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn selector_of(sig: &str) -> [u8; 4] {
    let hash = keccak256(sig.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Build ABI input: selector + words.
fn build_input(sig: &str, words: &[[u8; 32]]) -> Vec<u8> {
    let mut input = Vec::with_capacity(4 + words.len() * 32);
    input.extend_from_slice(&selector_of(sig));
    for w in words {
        input.extend_from_slice(w);
    }
    input
}

fn encode_market_id(mid: MarketId) -> [u8; 32] {
    abi::encode_u64(mid)
}

fn encode_addr(a: &Address) -> [u8; 32] {
    abi::encode_address(a)
}

/// Set EVM balance directly in CF_ACCOUNTS (72-byte record).
fn set_evm_balance(db: &StateDb, address: &Address, balance: U256) {
    let keccak_empty: [u8; 32] = [
        0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03,
        0xc0, 0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85,
        0xa4, 0x70,
    ];
    let mut data = vec![0u8; 72];
    data[..32].copy_from_slice(&balance.to_be_bytes::<32>());
    data[40..72].copy_from_slice(&keccak_empty);
    db.put_cf_raw("cf_accounts", address.as_slice(), &data)
        .unwrap();
}

fn get_evm_balance(db: &StateDb, address: &Address) -> U256 {
    match db.get_cf_raw("cf_accounts", address.as_slice()).unwrap() {
        Some(data) if data.len() >= 32 => U256::from_be_slice(&data[..32]),
        _ => U256::ZERO,
    }
}

/// Write an aggregated oracle price to CF_NATIVE_ORACLE: the 36-byte row
/// price(16) || block(8) || num_reporters(4) || block timestamp(8).
fn write_oracle_price(db: &StateDb, market_id: MarketId, price: FixedPoint, block: u64, ts: u64) {
    let mut key = Vec::with_capacity(11);
    key.extend_from_slice(b"agg");
    key.extend_from_slice(&market_id.to_be_bytes());

    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&price.raw().to_be_bytes());
    data.extend_from_slice(&block.to_be_bytes());
    data.extend_from_slice(&3u32.to_be_bytes()); // num_reporters
    data.extend_from_slice(&ts.to_be_bytes());

    db.put_cf_raw("cf_native_oracle", &key, &data).unwrap();
}

// ============================================================================
// Lockbox Tests
// ============================================================================

/// `n` whole tokens in EVM wei (18 decimals; native FixedPoint is 8 decimals).
fn wei(n: u64) -> U256 {
    U256::from(n) * U256::from(1_000_000_000_000_000_000u128)
}

#[test]
fn lockbox_deposit_to_native() {
    let (_dir, db) = setup();
    let trader = addr(1);

    // Give trader 10,000 tokens of EVM balance (18-decimal wei)
    set_evm_balance(&db, &trader, wei(10_000));

    // Deposit 3,000 to native
    Lockbox::deposit_to_native(&db, &trader, fp(3_000)).unwrap();

    // Verify EVM decreased by 3,000 × 10^18 wei
    let evm = get_evm_balance(&db, &trader);
    assert_eq!(evm, wei(7_000));

    // Verify native increased
    let pm = PositionManager::new(db);
    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, fp(3_000));
}

#[test]
fn lockbox_withdraw_from_native() {
    let (_dir, db) = setup();
    let trader = addr(2);

    // Give trader native balance
    let pm = PositionManager::new(db.clone());
    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(5_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    // Withdraw 2,000 to EVM
    Lockbox::withdraw_from_native(&db, &trader, fp(2_000)).unwrap();

    // Verify native decreased
    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, fp(3_000));

    // Verify EVM increased by 2,000 × 10^18 wei
    let evm = get_evm_balance(&db, &trader);
    assert_eq!(evm, wei(2_000));
}

#[test]
fn lockbox_insufficient_evm_balance() {
    let (_dir, db) = setup();
    let trader = addr(3);

    set_evm_balance(&db, &trader, U256::from(100u64));

    let result = Lockbox::deposit_to_native(&db, &trader, fp(1_000));
    assert!(matches!(
        result,
        Err(CoreError::InsufficientEvmBalance { .. })
    ));
}

#[test]
fn lockbox_insufficient_native_balance() {
    let (_dir, db) = setup();
    let trader = addr(4);

    let result = Lockbox::withdraw_from_native(&db, &trader, fp(1_000));
    assert!(matches!(
        result,
        Err(CoreError::InsufficientNativeBalance { .. })
    ));
}

// ============================================================================
// OrderBookReader Precompile Tests
// ============================================================================

/// Item 7 step 1 (owner s122): getOrderBook is removed from 0x0800 like
/// getOpenOrders (s99). Its selector reverts exactly like any unknown
/// selector, even when the market has a book.
#[test]
fn order_book_reader_get_order_book_is_an_unknown_selector() {
    let (_dir, db) = setup();

    // A book the removed selector must not read.
    let snapshot = OrderBookSnapshot {
        bids: vec![
            PriceLevel {
                price: fp(50_000),
                quantity: fp(10),
            },
            PriceLevel {
                price: fp(49_900),
                quantity: fp(20),
            },
        ],
        asks: vec![PriceLevel {
            price: fp(50_100),
            quantity: fp(5),
        }],
    };
    write_order_book_snapshot(&db, 1, &snapshot).unwrap();

    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(1)]);
    let sel = u32::from_be_bytes(input[..4].try_into().unwrap());
    let result = execute_precompile(&address, &input, &addr(0), &db, 100, 0);
    assert!(
        matches!(result, Err(CoreError::UnknownSelector(s)) if s == sel),
        "{result:?}"
    );
}

#[test]
fn order_book_reader_get_position() {
    let (_dir, db) = setup();
    let trader = addr(1);

    // Write a position
    let pm = PositionManager::new(db.clone());
    pm.put_position(&Position {
        trader,
        market_id: 1,
        is_long: true,
        size: fp(5),
        entry_price: fp(50_000),
        cost_basis: fp(50_000) * fp(5),
        realized_pnl: fp(100),
        isolated_margin: fp(2_500),
        margin_type: torus_core::position::MarginType::Isolated,
        funding: None,
    })
    .unwrap();

    // Write oracle price for unrealized PnL computation
    write_oracle_price(&db, 1, fp(51_000), 100, 1_000);

    // Call precompile
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input(
        "getPosition(address,bytes32)",
        &[encode_addr(&trader), encode_market_id(1)],
    );
    let output = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();

    // 5 words = 160 bytes
    assert_eq!(output.len(), 160);

    // Decode size (signed i128): should be positive (long)
    let size = i128::from_be_bytes(output[16..32].try_into().unwrap());
    assert_eq!(size, fp(5).raw());

    // Decode entry_price
    let entry = u128::from_be_bytes(output[48..64].try_into().unwrap());
    assert_eq!(entry, fp(50_000).raw() as u128);
}

/// Codex P1 (s138): getPosition takes the pending funding off the UPnL with
/// or without a usable price (it needs only the index). Long 2 entered at
/// index 1, index 4: owes 6. RED before the fix: without a price the UPnL
/// word was 0.
#[test]
fn order_book_reader_get_position_counts_pending_funding_without_a_price() {
    use torus_core::funding::{write_row, FundingParams, FundingRow, INDEX_SCALE};
    use torus_core::position::{MarginType, PositionFunding};
    let (_dir, db) = setup();
    let trader = addr(1);
    PositionManager::new(db.clone())
        .put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(2),
            entry_price: fp(100),
            cost_basis: fp(200),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
            funding: Some(PositionFunding { entry_index: INDEX_SCALE, paid: FixedPoint::ZERO }),
        })
        .unwrap();
    let mut row = FundingRow::new(1_000, &FundingParams::default());
    row.cum_index = 4 * INDEX_SCALE;
    write_row(&db, 1, &row).unwrap();
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getPosition(address,bytes32)", &[encode_addr(&trader), encode_market_id(1)]);
    let upnl = || {
        let out = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();
        i128::from_be_bytes(out[80..96].try_into().unwrap())
    };
    assert_eq!(upnl(), -fp(6).raw(), "no price: trade UPnL 0, the funding debt stays");
    write_oracle_price(&db, 1, fp(110), 100, 1_000);
    assert_eq!(upnl(), (fp(20) - fp(6)).raw(), "priced: 2 x (110 - 100) - 6");
}

/// s99 owner decision: getOpenOrders is removed from 0x0800 (it read
/// cf_native_orders, which no production code writes, so it always answered
/// empty). Its selector now reverts exactly like any unknown selector.
#[test]
fn order_book_reader_get_open_orders_is_an_unknown_selector() {
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input(
        "getOpenOrders(address,bytes32)",
        &[encode_addr(&addr(1)), encode_market_id(1)],
    );
    let sel = u32::from_be_bytes(input[..4].try_into().unwrap());
    let result = execute_precompile(&address, &input, &addr(0), &db, 100, 0);
    assert!(
        matches!(result, Err(CoreError::UnknownSelector(s)) if s == sel),
        "{result:?}"
    );
    let bogus = build_input(
        "noSuchReader(address,bytes32)",
        &[encode_addr(&addr(1)), encode_market_id(1)],
    );
    let other = execute_precompile(&address, &bogus, &addr(0), &db, 100, 0);
    assert!(
        matches!(other, Err(CoreError::UnknownSelector(_))),
        "{other:?}"
    );
}

// ============================================================================
// BalanceReader Precompile Tests
// ============================================================================

#[test]
fn balance_reader_get_balances() {
    let (_dir, db) = setup();
    let trader = addr(1);

    // Set native balance
    let pm = PositionManager::new(db.clone());
    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(10_000),
            order_margin: fp(500),
        },
    )
    .unwrap();

    // Set EVM balance
    set_evm_balance(
        &db,
        &trader,
        U256::from(5_000u64 * FixedPoint::SCALE as u64),
    );

    // Call precompile
    let address = precompile_address(ADDR_BALANCE_READER);
    let input = build_input("getBalances(address)", &[encode_addr(&trader)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();

    // 4 static values = 128 bytes
    assert_eq!(output.len(), 128);

    // Native balance
    let native = u128::from_be_bytes(output[16..32].try_into().unwrap());
    assert_eq!(native, fp(10_000).raw() as u128);

    // EVM balance
    let evm = u128::from_be_bytes(output[48..64].try_into().unwrap());
    assert_eq!(evm, 5_000u128 * FixedPoint::SCALE as u128);
}

/// F1/D1 (s517): `available` may be negative (UPnL-funded reservations). The
/// uint128 ABI fields report it as 0 — `raw as u128` used to wrap −200 to
/// ~3.4e38. Order margin is reported as is.
#[test]
fn balance_reader_clamps_negative_available_to_zero() {
    let (_dir, db) = setup();
    let trader = addr(1);
    PositionManager::new(db.clone())
        .put_native_balance(&trader, &NativeBalance { available: -fp(200), order_margin: fp(300) })
        .unwrap();
    let address = precompile_address(ADDR_BALANCE_READER);
    let input = build_input("getBalances(address)", &[encode_addr(&trader)]);
    let out = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();
    let word = |i: usize| u128::from_be_bytes(out[32 * i + 16..32 * i + 32].try_into().unwrap());
    assert_eq!(word(0), 0, "native_balance");
    assert_eq!(word(2), fp(300).raw() as u128, "total_margin_used");
    assert_eq!(word(3), 0, "available");
}

// ============================================================================
// OracleReader Precompile Tests
// ============================================================================

#[test]
fn oracle_reader_get_price() {
    let (_dir, db) = setup();

    write_oracle_price(&db, 1, fp(50_000), 95, 1_000);

    let address = precompile_address(ADDR_ORACLE_READER);
    let input = build_input("getPrice(bytes32)", &[encode_market_id(1)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100, 1_005).unwrap();

    // 3 values = 96 bytes
    assert_eq!(output.len(), 96);

    // Price
    let price = u128::from_be_bytes(output[16..32].try_into().unwrap());
    assert_eq!(price, fp(50_000).raw() as u128);

    // Block number
    let block = u64::from_be_bytes(output[56..64].try_into().unwrap());
    assert_eq!(block, 95);

    // Stale: 1_005 - 1_000 = 5 s, not > 60, so not stale
    assert_eq!(output[95], 0); // false
}

#[test]
fn oracle_reader_stale_price() {
    let (_dir, db) = setup();

    write_oracle_price(&db, 1, fp(50_000), 10, 1_000);

    let address = precompile_address(ADDR_ORACLE_READER);
    let input = build_input("getPrice(bytes32)", &[encode_market_id(1)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 200, 1_061).unwrap();

    // Stale: 1_061 - 1_000 = 61 s, > 60, so stale=true
    assert_eq!(output[95], 1); // true
}

/// 0x0802 getAllPrices: stale flags from timestamps (60 s).
#[test]
fn oracle_reader_get_all_prices_uses_timestamps() {
    let (_dir, db) = setup();
    write_oracle_price(&db, 1, fp(50_000), 5, 1_000);
    write_oracle_price(&db, 2, fp(3_000), 9, 1_050);
    let address = precompile_address(ADDR_ORACLE_READER);
    let input = build_input("getAllPrices()", &[]);
    let out = execute_precompile(&address, &input, &addr(0), &db, 10, 1_061).unwrap();
    // three dynamic arrays; the stale-flag array is the third — decode its 2 elements
    let off = u64::from_be_bytes(out[88..96].try_into().unwrap()) as usize; // 3rd head word
    assert_eq!(u64::from_be_bytes(out[off + 24..off + 32].try_into().unwrap()), 2, "len");
    assert_eq!(out[off + 63], 1, "market 1: age 61 -> stale");
    assert_eq!(out[off + 95], 0, "market 2: age 11 -> fresh");
}

/// 0x0800 getPosition UPnL uses the price only while usable (ABI unchanged).
#[test]
fn order_book_reader_get_position_ignores_a_stale_oracle_price() {
    let (_dir, db) = setup();
    let trader = addr(1);
    PositionManager::new(db.clone())
        .put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(5),
            entry_price: fp(50_000),
            cost_basis: fp(50_000) * fp(5),
            realized_pnl: fp(100),
            isolated_margin: fp(2_500),
            margin_type: torus_core::position::MarginType::Isolated,
            funding: None,
        })
        .unwrap();
    write_oracle_price(&db, 1, fp(51_000), 100, 1_000);
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input =
        build_input("getPosition(address,bytes32)", &[encode_addr(&trader), encode_market_id(1)]);
    let upnl = |ts: u64| {
        let out = execute_precompile(&address, &input, &addr(0), &db, 200, ts).unwrap();
        i128::from_be_bytes(out[80..96].try_into().unwrap())
    };
    assert_eq!(upnl(1_060), fp(5_000).raw(), "age 60: 5 x (51,000 - 50,000)");
    assert_eq!(upnl(1_061), 0, "age 61: stale -> 0");
    assert_eq!(upnl(900), fp(5_000).raw(), "clock behind the row: age clamps at 0 (usable)");
}

// ============================================================================
// StakingReader Precompile Tests
// ============================================================================

#[test]
fn staking_reader_get_staking_info() {
    let (_dir, db) = setup();
    let staker = addr(1);
    let _validator = addr(10);

    // Write permanent stake directly to CF
    // Format: staker(20) + amount(U256 32 BE) + locked_at_block(u64 8 LE borsh)
    let amount = U256::from(5_000u64);
    let mut data = Vec::new();
    data.extend_from_slice(staker.as_slice());
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    data.extend_from_slice(&100u64.to_le_bytes()); // borsh u64 is LE
    db.put_cf_raw("cf_staking_permanent", staker.as_slice(), &data)
        .unwrap();

    // Write pending rewards
    let rewards = U256::from(250u64);
    let mut rdata = Vec::new();
    rdata.extend_from_slice(staker.as_slice());
    rdata.extend_from_slice(&rewards.to_be_bytes::<32>());
    db.put_cf_raw("cf_staking_rewards", staker.as_slice(), &rdata)
        .unwrap();

    let address = precompile_address(ADDR_STAKING_READER);
    let input = build_input("getStakingInfo(address)", &[encode_addr(&staker)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();

    // 4 values = 128 bytes
    assert_eq!(output.len(), 128);

    // Permanent stake
    let perm = u128::from_be_bytes(output[48..64].try_into().unwrap());
    assert_eq!(perm, 5_000);

    // Rewards
    let rew = u128::from_be_bytes(output[80..96].try_into().unwrap());
    assert_eq!(rew, 250);
}

// ============================================================================
// CoreWriter Precompile Tests
// ============================================================================

#[test]
fn core_writer_place_order_queues_action() {
    let (_dir, db) = setup_k1();
    let caller = addr(1);

    let address = precompile_address(ADDR_CORE_WRITER);
    let input = build_input(
        "placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)",
        &[
            encode_market_id(1),
            abi::encode_u8(0),                          // Buy
            abi::encode_u8(0),                          // Limit
            abi::encode_u128(fp(50_000).raw() as u128), // price
            abi::encode_u128(fp(5).raw() as u128),      // quantity
            abi::encode_u8(0),                          // GTC
        ],
    );

    let output = execute_precompile(&address, &input, &caller, &db, 100, 0).unwrap();

    // Output should be a bytes32 (order_id) = 32 bytes
    assert_eq!(output.len(), 32);

    // Verify action is queued for block 101 (current_block + 1)
    let count = CoreWriterQueue::pending_count(&db, 101).unwrap();
    assert_eq!(count, 1);

    // Action should NOT be in current block (anti-frontrunning)
    let count_current = CoreWriterQueue::pending_count(&db, 100).unwrap();
    assert_eq!(count_current, 0);
}

fn place_order_input(order_type: u8) -> Vec<u8> {
    build_input(
        "placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)",
        &[
            encode_market_id(1),
            abi::encode_u8(0),
            abi::encode_u8(order_type),
            abi::encode_u128(fp(50_000).raw() as u128),
            abi::encode_u128(fp(5).raw() as u128),
            abi::encode_u8(0),
        ],
    )
}

/// HL-parity: the real order id is assigned by the native executor when the
/// queue drains next block (global counter), so it cannot be known here. The
/// precompile used to return a synthetic `(block + 1) << 64 | seq` that never
/// matched it (a later cancelOrder with it cancelled nothing). It now returns
/// zero: no id (the 0x0800 getOpenOrders reader was removed in s99).
#[test]
fn core_writer_place_order_returns_no_synthetic_order_id() {
    let (_dir, db) = setup_k1();
    let address = precompile_address(ADDR_CORE_WRITER);
    let output = execute_precompile(&address, &place_order_input(0), &addr(1), &db, 100, 0).unwrap();
    assert_eq!(output, vec![0u8; 32], "no fabricated order id");
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 1, "still queued");
}

/// HL-parity: placeOrder carries no trigger price, so StopMarket (2) and
/// StopLimit (3) cannot be expressed; they used to be queued and run as plain
/// Limit orders. They are rejected (the EVM call reverts) and nothing queues.
#[test]
fn core_writer_rejects_stop_order_types() {
    let (_dir, db) = setup_k1();
    let address = precompile_address(ADDR_CORE_WRITER);
    for order_type in [2u8, 3] {
        let err = execute_precompile(&address, &place_order_input(order_type), &addr(1), &db, 100, 0);
        assert!(err.is_err(), "order_type {order_type} must be rejected");
    }
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 0, "nothing queued");
}

#[test]
fn core_writer_cancel_order_queues_action() {
    let (_dir, db) = setup_k1();
    let caller = addr(1);

    let address = precompile_address(ADDR_CORE_WRITER);
    let input = build_input("cancelOrder(bytes32)", &[abi::encode_order_id(42)]);

    let output = execute_precompile(&address, &input, &caller, &db, 100, 0).unwrap();
    assert_eq!(output.len(), 32);
    assert_eq!(output[31], 1); // true

    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 1);
}

// ============================================================================
// CoreWriterStaking Precompile Tests
// ============================================================================

#[test]
fn core_writer_staking_delegate_queues_action() {
    let (_dir, db) = setup();
    let caller = addr(1);
    let validator = addr(10);

    let address = precompile_address(ADDR_CORE_WRITER_STAKING);
    let input = build_input(
        "delegate(address,uint128)",
        &[
            encode_addr(&validator),
            abi::encode_u128(fp(1_000).raw() as u128),
        ],
    );

    let output = execute_precompile(&address, &input, &caller, &db, 50, 0).unwrap();
    assert_eq!(output[31], 1); // success

    assert_eq!(CoreWriterQueue::pending_count(&db, 51).unwrap(), 1);
}

// ============================================================================
// T4.4: writer precompiles through a journaled overlay
// ============================================================================

/// T4.4 RED-FIRST: `execute_precompile` accepts any `StateBackend`, so writer side
/// effects can buffer in a `NativeStateOverlay` journal — invisible to the base DB
/// until commit (discard = revert), with read-your-writes sequence numbering.
/// (Does not compile before T4.4: `execute_precompile` demanded a concrete `&StateDb`
/// and the overlay had no `commit_tx`.)
#[test]
fn writer_precompile_journals_through_overlay() {
    let (_dir, db) = setup_k1();
    let caller = addr(1);
    let overlay = NativeStateOverlay::new(db.clone());

    let address = precompile_address(ADDR_CORE_WRITER);
    let input = build_input(
        "placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)",
        &[
            encode_market_id(1),
            abi::encode_u8(0),                          // Buy
            abi::encode_u8(0),                          // Limit
            abi::encode_u128(fp(50_000).raw() as u128), // price
            abi::encode_u128(fp(5).raw() as u128),      // quantity
            abi::encode_u8(0),                          // GTC
        ],
    );

    // Two placeOrder calls in the same journal: read-your-writes gives seq 0 then
    // 1 (queue key = target block(8 BE) ‖ seq(8 BE)). placeOrder returns no id
    // since the HL-parity fix, so read the journaled rows' keys.
    execute_precompile(&address, &input, &caller, &overlay, 100, 0).unwrap();
    execute_precompile(&address, &input, &caller, &overlay, 100, 0).unwrap();
    let seqs: Vec<u64> = torus_state::StateBackend::iterate_cf(
        &overlay,
        torus_state::cf::CF_CORE_WRITER_QUEUE,
        Some(&101u64.to_be_bytes()),
    )
    .unwrap()
    .iter()
    .map(|(k, _)| u64::from_be_bytes(k[8..16].try_into().unwrap()))
    .collect();
    assert_eq!(
        seqs,
        vec![0, 1],
        "second enqueue must see the first one's journal write and get seq 1"
    );

    // Nothing durable yet — a discard here would be a clean revert.
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 0);

    // Commit (= calling tx succeeded): both actions become durable, in order.
    overlay.commit_tx(&db).unwrap();
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 2);
}

// ============================================================================
// CoreWriterQueue Tests
// ============================================================================

#[test]
fn queue_enqueue_and_drain() {
    let (_dir, db) = setup_k1();

    let action1 = QueuedAction {
        trader: addr(1),
        kind: QueuedActionKind::PlaceOrder {
            market_id: 1,
            side: 0,
            order_type: 0,
            price: fp(50_000),
            quantity: fp(5),
            time_in_force: 0,
            cloid: None,
        },
        block_queued: 100,
    };

    let action2 = QueuedAction {
        trader: addr(2),
        kind: QueuedActionKind::CancelOrder { order_id: 42 },
        block_queued: 100,
    };

    // Enqueue both (target block = 101)
    CoreWriterQueue::enqueue(&db, &action1).unwrap();
    CoreWriterQueue::enqueue(&db, &action2).unwrap();

    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 2);
    assert_eq!(CoreWriterQueue::pending_count(&db, 100).unwrap(), 0);
    assert_eq!(CoreWriterQueue::pending_count(&db, 102).unwrap(), 0);

    // Drain block 101
    let drained = drain_ok(&db, 101);
    assert_eq!(drained.len(), 2);

    // Verify first action
    assert_eq!(drained[0].trader, addr(1));
    assert!(matches!(
        drained[0].kind,
        QueuedActionKind::PlaceOrder { market_id: 1, .. }
    ));

    // Verify second action
    assert_eq!(drained[1].trader, addr(2));
    assert!(matches!(
        drained[1].kind,
        QueuedActionKind::CancelOrder { order_id: 42 }
    ));

    // Queue should be empty after drain
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 0);
}

#[test]
fn queue_drain_correct_block_only() {
    let (_dir, db) = setup();

    let action_block_5 = QueuedAction {
        trader: addr(1),
        kind: QueuedActionKind::ClaimRewards,
        block_queued: 4, // target = 5
    };
    let action_block_6 = QueuedAction {
        trader: addr(2),
        kind: QueuedActionKind::ClaimRewards,
        block_queued: 5, // target = 6
    };

    CoreWriterQueue::enqueue(&db, &action_block_5).unwrap();
    CoreWriterQueue::enqueue(&db, &action_block_6).unwrap();

    // Drain block 5 only
    let drained = drain_ok(&db, 5);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].trader, addr(1));

    // Block 6 still has its action
    assert_eq!(CoreWriterQueue::pending_count(&db, 6).unwrap(), 1);
}

/// Drain `block`, every row expected to decode.
fn drain_ok(db: &StateDb, block: u64) -> Vec<QueuedAction> {
    CoreWriterQueue::drain(db, block)
        .unwrap()
        .into_iter()
        .map(|a| a.expect("decodable queue row"))
        .collect()
}

/// Queue key: target block (8 BE) ‖ sequence (8 BE).
fn queue_key(block: u64, seq: u64) -> Vec<u8> {
    let mut key = block.to_be_bytes().to_vec();
    key.extend_from_slice(&seq.to_be_bytes());
    key
}

/// A-exception (owner s111): an undecodable row in `CF_CORE_WRITER_QUEUE`
/// is drained as an `Err` entry at its sequence position (never silently
/// dropped), its valid neighbours keep their order, and every row of the
/// block (the bad one included) is deleted. RED before: the drain skipped
/// the row (2 entries) and deleted it without a trace.
#[test]
fn queue_drain_reports_an_undecodable_row_in_place_and_deletes_it() {
    use torus_state::cf::CF_CORE_WRITER_QUEUE;
    let (_dir, db) = setup();
    let qa = |n: u8| QueuedAction {
        trader: addr(n),
        kind: QueuedActionKind::ClaimRewards,
        block_queued: 6,
    };
    assert_eq!(CoreWriterQueue::enqueue(&db, &qa(1)).unwrap(), 0);
    // seq 1: present, but not a QueuedAction (3 bytes < a 20-byte trader).
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(7, 1), &[0xFF, 0x00, 0x01])
        .unwrap();
    assert_eq!(CoreWriterQueue::enqueue(&db, &qa(3)).unwrap(), 2);
    // A row of the next block is untouched.
    CoreWriterQueue::enqueue(
        &db,
        &QueuedAction {
            block_queued: 7,
            ..qa(4)
        },
    )
    .unwrap();

    let drained = CoreWriterQueue::drain(&db, 7).unwrap();
    assert_eq!(drained.len(), 3, "one entry per row: {drained:?}");
    assert_eq!(drained[0].as_ref().unwrap().trader, addr(1));
    let bad = drained[1].as_ref().expect_err("seq 1 does not decode");
    assert_eq!(bad.key, queue_key(7, 1));
    assert!(!bad.error.is_empty());
    assert!(
        bad.to_string()
            .starts_with("undecodable queued action (block 7, seq 1): "),
        "{bad}"
    );
    assert_eq!(drained[2].as_ref().unwrap().trader, addr(3));

    assert_eq!(
        CoreWriterQueue::pending_count(&db, 7).unwrap(),
        0,
        "all deleted"
    );
    assert_eq!(CoreWriterQueue::pending_count(&db, 8).unwrap(), 1);
    // Deterministic: the same bytes give the same entry.
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(9, 0), &[0xFF, 0x00, 0x01])
        .unwrap();
    let again = CoreWriterQueue::drain(&db, 9).unwrap();
    assert_eq!(again[0].as_ref().unwrap_err().error, bad.error);
}

/// Item 7 step 2 (owner decision 4): order actions wait K blocks (default 10, governance
/// `core_writer_order_delay`), other kinds one; an invalid stored K keeps 10. RED before: all +1.
#[test]
fn order_actions_wait_k_blocks_others_one() {
    use torus_state::cf::CF_FEE_CONFIG;
    let (_dir, db) = setup();
    let queue = |kind| {
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind,
                block_queued: 100,
            },
        )
        .unwrap()
    };
    queue(QueuedActionKind::PlaceOrder {
        market_id: 1,
        side: 0,
        order_type: 0,
        price: fp(50_000),
        quantity: fp(5),
        time_in_force: 0,
        cloid: None,
    });
    queue(QueuedActionKind::CancelOrder { order_id: 1 });
    queue(QueuedActionKind::CancelAll { market_id: 1 });
    queue(QueuedActionKind::CancelByCloid {
        market_id: 1,
        cloid: 7,
    });
    queue(QueuedActionKind::ClaimRewards);
    queue(QueuedActionKind::LockboxDeposit { amount: fp(1) });
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 2);
    assert_eq!(CoreWriterQueue::pending_count(&db, 110).unwrap(), 4);
    for (stored, target) in [("3", 103), ("0", 110), ("101", 110), ("x", 110)] {
        let (_dir, db) = setup();
        db.put_cf_raw(
            CF_FEE_CONFIG,
            torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(),
            stored.as_bytes(),
        )
        .unwrap();
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind: QueuedActionKind::CancelOrder { order_id: 1 },
                block_queued: 100,
            },
        )
        .unwrap();
        assert_eq!(
            CoreWriterQueue::pending_count(&db, target).unwrap(),
            1,
            "{stored}"
        );
    }
}

/// Item 7 step 2 (owner decision 4, cap at enqueue): a target block already holding 200
/// rows sends the next row to the following block, in queue order. RED before: one block
/// took every row (and all at +1).
#[test]
fn a_full_target_block_sends_later_rows_to_the_next_block() {
    let (_dir, db) = setup();
    let enqueue = |kind, block_queued| {
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind,
                block_queued,
            },
        )
        .unwrap()
    };
    let seqs: Vec<u64> = (0..=200)
        .map(|order_id| enqueue(QueuedActionKind::CancelOrder { order_id }, 100))
        .collect();
    assert_eq!(seqs[..200], (0..200).collect::<Vec<u64>>()[..]);
    assert_eq!(seqs[200], 0, "the 201st row opens block 111");
    assert_eq!(CoreWriterQueue::pending_count(&db, 110).unwrap(), 200);
    assert_eq!(CoreWriterQueue::pending_count(&db, 111).unwrap(), 1);
    // A ClaimRewards queued at 109 targets 110, which is full: 111, after the cancel.
    assert_eq!(enqueue(QueuedActionKind::ClaimRewards, 109), 1);
    let drained = drain_ok(&db, 111);
    assert_eq!(drained.len(), 2);
    assert!(matches!(
        drained[0].kind,
        QueuedActionKind::CancelOrder { order_id: 200 }
    ));
    assert!(matches!(drained[1].kind, QueuedActionKind::ClaimRewards));
}

/// Item 7 step 2 (owner answer 3, Codex plan check s125): K is read at enqueue and the drain
/// takes a target prefix, so lowering K must not let a later cancel drain before an earlier
/// order: an order kind never targets a block before the last order kind's target. Other
/// kinds ignore it. RED before: all +1 (and without the last-target row the cancel drains
/// at 103).
#[test]
fn lowering_k_never_lets_a_cancel_overtake_an_earlier_order() {
    use torus_state::cf::CF_FEE_CONFIG;
    let (_dir, db) = setup();
    let enqueue = |kind, block_queued| {
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind,
                block_queued,
            },
        )
        .unwrap()
    };
    enqueue(
        QueuedActionKind::PlaceOrder {
            market_id: 1,
            side: 0,
            order_type: 0,
            price: fp(50_000),
            quantity: fp(5),
            time_in_force: 0,
            cloid: None,
        },
        100,
    );
    assert_eq!(CoreWriterQueue::pending_count(&db, 110).unwrap(), 1);
    db.put_cf_raw(
        CF_FEE_CONFIG,
        torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(),
        b"1",
    )
    .unwrap();
    assert_eq!(
        enqueue(QueuedActionKind::CancelOrder { order_id: 1 }, 102),
        1
    );
    assert_eq!(
        CoreWriterQueue::pending_count(&db, 103).unwrap(),
        0,
        "the cancel waits for the order's target"
    );
    // Other kinds ignore the last order target: K = 1 does not apply, + 1 does.
    assert_eq!(enqueue(QueuedActionKind::ClaimRewards, 102), 0);
    assert_eq!(CoreWriterQueue::pending_count(&db, 103).unwrap(), 1);
    let drained = drain_ok(&db, 110);
    assert_eq!(drained.len(), 2);
    assert!(matches!(
        drained[0].kind,
        QueuedActionKind::PlaceOrder { market_id: 1, .. }
    ));
    assert!(matches!(
        drained[1].kind,
        QueuedActionKind::CancelOrder { order_id: 1 }
    ));
    // Once the last target has passed, the delay is K again.
    enqueue(QueuedActionKind::CancelOrder { order_id: 2 }, 115);
    assert_eq!(CoreWriterQueue::pending_count(&db, 116).unwrap(), 1);
    // The row is hashed state in CF_FEE_CONFIG (u64 BE), not a queue row.
    assert_eq!(
        db.get_cf_raw(CF_FEE_CONFIG, b"core_writer_last_order_target")
            .unwrap(),
        Some(116u64.to_be_bytes().to_vec())
    );
}

/// Point reads (`get_cf_raw` + `get_cf_len`, any CF) through a backend; every other call
/// is delegated as is.
#[derive(Clone)]
struct PointReads<T> {
    inner: T,
    n: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<T> PointReads<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            n: Default::default(),
        }
    }
    fn count(&self) -> usize {
        self.n.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn bump(&self) {
        self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl<T: torus_state::StateBackend> torus_state::StateBackend for PointReads<T> {
    fn get_cf_raw(
        &self,
        cf: &str,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, torus_state::error::StateError> {
        self.bump();
        self.inner.get_cf_raw(cf, key)
    }
    fn get_cf_len(
        &self,
        cf: &str,
        key: &[u8],
    ) -> Result<Option<usize>, torus_state::error::StateError> {
        self.bump();
        self.inner.get_cf_len(cf, key)
    }
    fn put_cf_raw(
        &self,
        cf: &str,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), torus_state::error::StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), torus_state::error::StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    #[allow(clippy::type_complexity)]
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::error::StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    #[allow(clippy::type_complexity)]
    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::error::StateError> {
        self.inner.iterate_cf_from(cf, start, limit)
    }
    #[allow(clippy::type_complexity)]
    fn iterate_cf_prefix_from(
        &self,
        cf: &str,
        prefix: &[u8],
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::error::StateError> {
        self.inner.iterate_cf_prefix_from(cf, prefix, start, limit)
    }
    fn prefix_exists(
        &self,
        cf: &str,
        prefix: &[u8],
    ) -> Result<bool, torus_state::error::StateError> {
        self.inner.prefix_exists(cf, prefix)
    }
    fn atomic_write(
        &self,
        ops: &[torus_state::AtomicWriteOp<'_>],
    ) -> Result<(), torus_state::error::StateError> {
        self.inner.atomic_write(ops)
    }
}

/// The queue row count of each target in `from..to`.
fn queue_counts(db: &StateDb, from: u64, to: u64) -> Vec<usize> {
    (from..to)
        .map(|t| CoreWriterQueue::pending_count(db, t).unwrap())
        .collect()
}

fn claim(block_queued: u64) -> QueuedAction {
    QueuedAction {
        trader: addr(1),
        kind: QueuedActionKind::ClaimRewards,
        block_queued,
    }
}

/// Codex P2 on C7: an enqueue skipped every full target with one point read each, so with
/// intake above the cap the reads per enqueue grew with the backlog (for a fixed gas). With
/// the first-open-target cursor a backlog of full targets costs the same reads as none.
/// RED before: `full + 1` probe reads (6 and 26).
#[test]
fn enqueue_point_reads_do_not_grow_with_a_full_target_backlog() {
    let cap = torus_types::CORE_WRITER_DRAIN_CAP;
    let mut reads = Vec::new();
    for full in [5u64, 25] {
        let (_dir, db) = setup();
        for _ in 0..cap * full {
            CoreWriterQueue::enqueue(&db, &claim(100)).unwrap();
        }
        assert_eq!(
            queue_counts(&db, 101, 101 + full),
            vec![cap as usize; full as usize]
        );
        let counted = PointReads::new(db.clone());
        assert_eq!(CoreWriterQueue::enqueue(&counted, &claim(100)).unwrap(), 0);
        assert_eq!(
            CoreWriterQueue::pending_count(&db, 101 + full).unwrap(),
            1,
            "the first open target, after the {full} full ones"
        );
        reads.push(counted.count());
    }
    eprintln!("point reads per enqueue (5 full targets, 25 full targets): {reads:?}");
    assert_eq!(reads[0], reads[1], "reads do not grow with the backlog");
    assert!(reads[1] <= 3, "constant reads per enqueue: {reads:?}");
}

/// The full targets may be order kinds' (K blocks ahead, beyond the non-order cursor) and
/// the enqueue that walks over them may revert, dropping any cursor it wrote: the order
/// kinds' run row still bounds the walk. RED before: `full + 1` probes on every call.
#[test]
fn a_reverted_enqueue_over_an_order_run_reads_a_constant() {
    let cap = torus_types::CORE_WRITER_DRAIN_CAP;
    let full = 20u64;
    let (_dir, db) = setup();
    for order_id in 0..u128::from(cap * full) {
        CoreWriterQueue::enqueue(
            &db,
            &QueuedAction {
                trader: addr(1),
                kind: QueuedActionKind::CancelOrder { order_id },
                block_queued: 100,
            },
        )
        .unwrap();
    }
    assert_eq!(
        queue_counts(&db, 110, 110 + full),
        vec![cap as usize; full as usize]
    );
    // One ClaimRewards at 100 lands at 101: the non-order side has no backlog.
    CoreWriterQueue::enqueue(&db, &claim(100)).unwrap();
    for _ in 0..2 {
        // Queued at 109: + 1 = 110, the order run's first target; the tx reverts.
        let counted = PointReads::new(NativeStateOverlay::new(db.clone()));
        assert_eq!(CoreWriterQueue::enqueue(&counted, &claim(109)).unwrap(), 0);
        let landed = torus_state::StateBackend::iterate_cf(
            &counted.inner,
            torus_state::cf::CF_CORE_WRITER_QUEUE,
            Some(&(110 + full).to_be_bytes()),
        )
        .unwrap();
        assert_eq!(landed.len(), 1, "lands after the run");
        assert!(counted.count() <= 4, "reads: {}", counted.count());
        drop(counted); // revert: nothing durable
    }
    assert_eq!(CoreWriterQueue::pending_count(&db, 110 + full).unwrap(), 0);
    // A later order kind continues the run (queue order kept).
    CoreWriterQueue::enqueue(
        &db,
        &QueuedAction {
            trader: addr(1),
            kind: QueuedActionKind::CancelOrder { order_id: 9_999 },
            block_queued: 105,
        },
    )
    .unwrap();
    let drained = drain_ok(&db, 110 + full);
    assert!(matches!(
        drained[..],
        [QueuedAction {
            kind: QueuedActionKind::CancelOrder { order_id: 9_999 },
            ..
        }]
    ));
}

/// Cursor stale-high and stale-low: a full backlog, an enqueue from a later block still
/// lands after it (the targets below the cursor are full), and once the backlog has
/// drained an enqueue lands at block + 1 again, in queue order.
#[test]
fn the_cursor_is_right_while_the_backlog_drains_and_after() {
    let cap = torus_types::CORE_WRITER_DRAIN_CAP;
    let (_dir, db) = setup();
    for _ in 0..cap * 30 {
        CoreWriterQueue::enqueue(&db, &claim(100)).unwrap();
    }
    assert_eq!(CoreWriterQueue::enqueue(&db, &claim(100)).unwrap(), 0);
    assert_eq!(CoreWriterQueue::pending_count(&db, 131).unwrap(), 1);
    // The cursor is hashed state in CF_FEE_CONFIG (u64 BE): 131, written when 130 filled.
    assert_eq!(
        db.get_cf_raw(
            torus_state::cf::CF_FEE_CONFIG,
            CORE_WRITER_FIRST_OPEN_TARGET_KEY
        )
        .unwrap(),
        Some(131u64.to_be_bytes().to_vec())
    );
    for block in 101..=105 {
        assert_eq!(drain_ok(&db, block).len(), cap as usize);
    }
    // Queued at 105: 106..=130 are still full, so 131, after the earlier row.
    assert_eq!(CoreWriterQueue::enqueue(&db, &claim(105)).unwrap(), 1);
    for block in 106..=131 {
        drain_ok(&db, block);
    }
    // The cursor (131 or 132) is behind block + 1 now: the row lands at 141.
    assert_eq!(CoreWriterQueue::enqueue(&db, &claim(140)).unwrap(), 0);
    assert_eq!(CoreWriterQueue::pending_count(&db, 141).unwrap(), 1);
    // And with no backlog a row queued below the cursor's block still lands at + 1.
    let (_dir2, db2) = setup();
    for _ in 0..cap * 3 {
        CoreWriterQueue::enqueue(&db2, &claim(100)).unwrap();
    }
    for block in 101..=103 {
        drain_ok(&db2, block);
    }
    assert_eq!(CoreWriterQueue::enqueue(&db2, &claim(103)).unwrap(), 0);
    assert_eq!(CoreWriterQueue::pending_count(&db2, 104).unwrap(), 1);
}

/// Differential check of the cursor and the order run row against the plain walk (probe
/// every target from the candidate): random block loads above and below the cap, order
/// and non-order kinds, K changes, txs that revert (overlay dropped) or commit several
/// enqueues, and the drain of each block. Every row's target, seq and drain position must
/// match the model.
#[test]
fn enqueue_places_rows_exactly_like_the_plain_walk() {
    use std::collections::BTreeMap;
    use torus_state::cf::CF_FEE_CONFIG;
    let cap = torus_types::CORE_WRITER_DRAIN_CAP as usize;

    #[derive(Clone, Default)]
    struct Model {
        targets: BTreeMap<u64, Vec<u64>>,
        last_order: u64,
    }
    impl Model {
        fn place(&mut self, id: u64, order: bool, block: u64, k: u64, cap: usize) -> (u64, u64) {
            let mut t = if order {
                (block + k).max(self.last_order)
            } else {
                block + 1
            };
            while self.targets.get(&t).map_or(0, Vec::len) == cap {
                t += 1;
            }
            let rows = self.targets.entry(t).or_default();
            rows.push(id);
            if order {
                self.last_order = t;
            }
            (t, rows.len() as u64 - 1)
        }
    }

    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let (_dir, db) = setup();
    let mut model = Model::default();
    let mut k = 10u64;
    let mut id = 0u64;
    let mut max_reads = 0usize;
    let id_of = |a: &QueuedAction| match a.kind {
        QueuedActionKind::CancelOrder { order_id } => order_id as u64,
        QueuedActionKind::LockboxWithdraw { amount } => amount.raw() as u64,
        _ => unreachable!(),
    };
    for block in 100..160u64 {
        let drained: Vec<u64> = drain_ok(&db, block).iter().map(id_of).collect();
        assert_eq!(
            drained,
            model.targets.remove(&block).unwrap_or_default(),
            "drain of {block}"
        );
        if next(6) == 0 {
            k = 1 + next(12);
            db.put_cf_raw(
                CF_FEE_CONFIG,
                torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(),
                k.to_string().as_bytes(),
            )
            .unwrap();
        }
        // Heavy blocks (above the cap) and light ones; orders-only or non-orders-only
        // stretches too, so either side builds a backlog.
        let txs = [0, 40, 150, 400][next(4) as usize];
        let order_share = [0, 50, 100][next(3) as usize];
        for _ in 0..txs {
            let overlay = NativeStateOverlay::new(db.clone());
            let mut tentative = model.clone();
            for _ in 0..1 + next(3) {
                id += 1;
                let order = next(100) < order_share;
                let kind = if order {
                    QueuedActionKind::CancelOrder {
                        order_id: u128::from(id),
                    }
                } else {
                    QueuedActionKind::LockboxWithdraw {
                        amount: FixedPoint::from_raw(id as i128),
                    }
                };
                let counted = PointReads::new(overlay.clone());
                let seq = CoreWriterQueue::enqueue(
                    &counted,
                    &QueuedAction {
                        trader: addr(1),
                        kind,
                        block_queued: block,
                    },
                )
                .unwrap();
                max_reads = max_reads.max(counted.count());
                let (target, want) = tentative.place(id, order, block, k, cap);
                assert_eq!(seq, want, "row {id} at {target}");
                assert!(torus_state::StateBackend::get_cf_raw(
                    &overlay,
                    torus_state::cf::CF_CORE_WRITER_QUEUE,
                    &queue_key(target, seq),
                )
                .unwrap()
                .is_some());
            }
            if next(4) == 0 {
                drop(overlay); // reverted
            } else {
                overlay.commit_tx(&db).unwrap();
                model = tentative;
            }
        }
    }
    assert!(id > 10_000, "enough rows to fill targets: {id}");
    // Uncovered full targets lie below block + K (K <= 12 here): reads stay bounded.
    eprintln!("max point reads per enqueue: {max_reads}");
    assert!(
        max_reads <= 12 + 6,
        "max point reads per enqueue: {max_reads}"
    );
}

// ============================================================================
// ABI Encoding Roundtrip Tests
// ============================================================================

#[test]
fn abi_u128_roundtrip() {
    let val = 123456789u128;
    let encoded = abi::encode_u128(val);
    let decoded = abi::decode_u128(&encoded);
    assert_eq!(val, decoded);
}

#[test]
fn abi_address_roundtrip() {
    let a = addr(42);
    let encoded = abi::encode_address(&a);
    let decoded = abi::decode_address(&encoded);
    assert_eq!(a, decoded);
}

#[test]
fn abi_market_id_roundtrip() {
    let mid: MarketId = 7;
    let encoded = abi::encode_market_id(mid);
    let decoded = abi::decode_market_id(&encoded);
    assert_eq!(mid, decoded);
}

#[test]
fn abi_order_id_roundtrip() {
    let oid: u128 = 999_888_777;
    let encoded = abi::encode_order_id(oid);
    let decoded = abi::decode_order_id(&encoded);
    assert_eq!(oid, decoded);
}

#[test]
fn abi_i128_encoding() {
    let neg = -500i128;
    let encoded = abi::encode_i128(neg);
    // High bytes should be 0xFF for negative
    assert_eq!(encoded[0], 0xFF);
    // Decode back
    let decoded = i128::from_be_bytes(encoded[16..32].try_into().unwrap());
    assert_eq!(neg, decoded);
}

#[test]
fn abi_arrays_response_encoding() {
    let arr1: Vec<[u8; 32]> = vec![abi::encode_u128(100), abi::encode_u128(200)];
    let arr2: Vec<[u8; 32]> = vec![abi::encode_u128(300)];
    let encoded = abi::encode_arrays_response(&[&arr1, &arr2]);

    // Head: 2 offsets = 64 bytes
    // arr1: length(32) + 2 elements(64) = 96 bytes
    // arr2: length(32) + 1 element(32) = 64 bytes
    // Total: 64 + 96 + 64 = 224 bytes
    assert_eq!(encoded.len(), 224);

    // First offset should point to 64 (past the 2 offset words)
    let off1 = u32::from_be_bytes(encoded[28..32].try_into().unwrap());
    assert_eq!(off1, 64);

    // Second offset should point to 64 + 96 = 160
    let off2 = u32::from_be_bytes(encoded[60..64].try_into().unwrap());
    assert_eq!(off2, 160);
}

// ============================================================================
// Lockbox via Precompile Interface
// ============================================================================

/// EVM-PF-05: the precompile never writes CF_ACCOUNTS / native balances — it only
/// queues. `depositToNative` is payable (amount == msg.value, in wei); the queued
/// native credit is floor(value / 10^10) and the dust is burned (by the EVM
/// provider, together with the value — not visible at this layer).
#[test]
fn lockbox_precompile_deposit() {
    let (_dir, db) = setup();
    let trader = addr(5);
    set_evm_balance(&db, &trader, wei(10_000));

    let address = precompile_address(ADDR_LOCKBOX);
    let value = wei(3_000) + U256::from(123u64); // 123 wei of sub-unit dust
    let input = build_input(
        "depositToNative(uint128)",
        &[abi::encode_u128(value.to::<u128>())],
    );

    let output = execute_precompile_with_value(&address, &input, &trader, value, &db, 100, 0).unwrap();
    assert_eq!(output[31], 1); // true = queued

    // No direct balance writes at the precompile layer.
    assert_eq!(get_evm_balance(&db, &trader), wei(10_000));
    let pm = PositionManager::new(db.clone());
    assert_eq!(pm.get_native_balance(&trader).unwrap().available, FixedPoint::ZERO);

    // Native credit queued for the next block, dust floored away.
    let queued = drain_ok(&db, 101);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].trader, trader);
    assert!(matches!(
        queued[0].kind,
        QueuedActionKind::LockboxDeposit { amount } if amount == fp(3_000)
    ));
}

#[test]
fn lockbox_precompile_deposit_rejects_bad_value() {
    let (_dir, db) = setup_k1();
    let trader = addr(5);
    let address = precompile_address(ADDR_LOCKBOX);
    let deposit = |amount: U256| {
        build_input(
            "depositToNative(uint128)",
            &[abi::encode_u128(amount.to::<u128>())],
        )
    };

    // amount argument != msg.value
    assert!(
        execute_precompile_with_value(&address, &deposit(wei(1)), &trader, U256::ZERO, &db, 100, 0)
            .is_err()
    );
    // all dust: < one native unit (10^10 wei) would credit nothing
    let dust = U256::from(9_999_999_999u64);
    assert!(execute_precompile_with_value(&address, &deposit(dust), &trader, dust, &db, 100, 0)
        .is_err());
    // zero: no-op success, nothing queued
    assert!(execute_precompile_with_value(
        &address,
        &deposit(U256::ZERO),
        &trader,
        U256::ZERO,
        &db,
        100,
        0,
    )
    .is_ok());
    // value to a non-payable precompile / selector
    let withdraw = build_input("withdrawFromNative(uint128)", &[abi::encode_u128(0)]);
    assert!(execute_precompile_with_value(&address, &withdraw, &trader, wei(1), &db, 100, 0).is_err());
    let core_writer = precompile_address(ADDR_CORE_WRITER);
    let cancel = build_input("cancelOrder(bytes32)", &[abi::encode_u128(1)]);
    assert!(
        execute_precompile_with_value(&core_writer, &cancel, &trader, wei(1), &db, 100, 0).is_err()
    );

    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 0);
}

#[test]
fn lockbox_precompile_withdraw() {
    let (_dir, db) = setup();
    let trader = addr(6);

    // Set native balance
    let pm = PositionManager::new(db.clone());
    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(8_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    let address = precompile_address(ADDR_LOCKBOX);
    let input = build_input(
        "withdrawFromNative(uint128)",
        &[abi::encode_u128(wei(2_000).to::<u128>())],
    );

    let output = execute_precompile(&address, &input, &trader, &db, 100, 0).unwrap();
    assert_eq!(output[31], 1); // true = queued

    // Nothing moves at the precompile layer (EVM-PF-05) ...
    assert_eq!(pm.get_native_balance(&trader).unwrap().available, fp(8_000));
    assert_eq!(get_evm_balance(&db, &trader), U256::ZERO);

    // ... the native debit / EVM credit is queued for the next block.
    let queued = drain_ok(&db, 101);
    assert_eq!(queued.len(), 1);
    assert!(matches!(
        queued[0].kind,
        QueuedActionKind::LockboxWithdraw { amount } if amount == fp(2_000)
    ));

    // Non-round wei amounts cannot be debited from an 8-decimal ledger: rejected.
    let odd = build_input(
        "withdrawFromNative(uint128)",
        &[abi::encode_u128(wei(1).to::<u128>() + 1)],
    );
    assert!(execute_precompile(&address, &odd, &trader, &db, 100, 0).is_err());
}

#[test]
fn lockbox_queued_kinds_borsh_round_trip() {
    use borsh::BorshDeserialize;
    for kind in [
        QueuedActionKind::LockboxDeposit { amount: fp(7) },
        QueuedActionKind::LockboxWithdraw { amount: fp(9) },
    ] {
        let qa = QueuedAction {
            trader: addr(3),
            kind,
            block_queued: 42,
        };
        let bytes = borsh::to_vec(&qa).unwrap();
        let back = QueuedAction::try_from_slice(&bytes).unwrap();
        assert_eq!(format!("{qa:?}"), format!("{back:?}"));
    }
}

// ============================================================================
// Unknown selector error
// ============================================================================

#[test]
fn unknown_selector_returns_error() {
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = vec![0xDE, 0xAD, 0xBE, 0xEF]; // bogus selector

    let result = execute_precompile(&address, &input, &addr(0), &db, 100, 0);
    assert!(matches!(result, Err(CoreError::UnknownSelector(_))));
}

// ============================================================================
// SECURITY: read-only (eth_call / eth_estimateGas) denies writer precompiles
//
// In call-simulation the Torus writer precompiles must NOT mutate the shared
// StateDb — doing so out of consensus diverges the node's state root (fork/halt)
// and, for CoreWriter, enqueues an action the next block drains on this node only.
// ============================================================================

#[test]
fn read_only_denies_lockbox_deposit_no_state_change() {
    let (_dir, db) = setup();
    let trader = addr(7);
    set_evm_balance(
        &db,
        &trader,
        U256::from(10_000u64 * FixedPoint::SCALE as u64),
    );

    let address = precompile_address(ADDR_LOCKBOX);
    let input = build_input(
        "depositToNative(uint128)",
        &[abi::encode_u128(fp(3_000).raw() as u128)],
    );

    // Read-only: must be denied AND leave both balances untouched.
    let result = execute_precompile_read_only(&address, &input, &trader, &db, 100, 0);
    assert!(
        result.is_err(),
        "writer precompile must be denied in read-only mode"
    );

    assert_eq!(
        get_evm_balance(&db, &trader),
        U256::from(10_000u64 * FixedPoint::SCALE as u64),
        "EVM balance must be unchanged by a denied read-only call",
    );
    let bal = PositionManager::new(db)
        .get_native_balance(&trader)
        .unwrap();
    assert_eq!(
        bal.available,
        FixedPoint::ZERO,
        "native balance must be unchanged by a denied read-only call",
    );
}

#[test]
fn read_only_denies_core_writer_no_enqueue() {
    let (_dir, db) = setup_k1();
    let caller = addr(1);
    let address = precompile_address(ADDR_CORE_WRITER);
    let input = build_input(
        "placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)",
        &[
            encode_market_id(1),
            abi::encode_u8(0),
            abi::encode_u8(0),
            abi::encode_u128(fp(50_000).raw() as u128),
            abi::encode_u128(fp(5).raw() as u128),
            abi::encode_u8(0),
        ],
    );

    let result = execute_precompile_read_only(&address, &input, &caller, &db, 100, 0);
    assert!(
        result.is_err(),
        "core_writer must be denied in read-only mode"
    );
    // Nothing enqueued for the next block (would otherwise be drained on-chain).
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 0);
}

#[test]
fn read_only_denies_core_writer_staking_no_enqueue() {
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_CORE_WRITER_STAKING);
    let input = build_input(
        "delegate(address,uint128)",
        &[
            encode_addr(&addr(10)),
            abi::encode_u128(fp(1_000).raw() as u128),
        ],
    );

    let result = execute_precompile_read_only(&address, &input, &addr(1), &db, 50, 0);
    assert!(result.is_err());
    assert_eq!(CoreWriterQueue::pending_count(&db, 51).unwrap(), 0);
}

#[test]
fn read_only_is_transparent_for_reader_precompiles() {
    let (_dir, db) = setup();
    // The guard must not affect reader precompiles: same result on both paths.
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input(
        "getPosition(address,bytes32)",
        &[encode_addr(&addr(1)), encode_market_id(1)],
    );

    let ro = execute_precompile_read_only(&address, &input, &addr(0), &db, 100, 0);
    let normal = execute_precompile(&address, &input, &addr(0), &db, 100, 0);
    assert!(
        ro.is_ok(),
        "reader precompile must still run in read-only mode"
    );
    assert_eq!(ro.unwrap(), normal.unwrap());
}

// ============================================================================
// s114 cloid: placeOrder with a cloid, cancelByCloid
// ============================================================================

const CLOID_PLACE_SIG: &str = "placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8,uint128)";
/// Above u64::MAX.
const CW_CLOID: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0007;

fn queued(db: &StateDb, block: u64) -> Vec<QueuedAction> {
    CoreWriterQueue::drain_keyed(db, block)
        .unwrap()
        .into_iter()
        .map(|(_, r)| r.expect("row decodes"))
        .collect()
}

/// The 7-word selector queues the cloid (0 = none); the 6-word one still
/// queues an order without one.
#[test]
fn core_writer_place_order_with_cloid_queues_it() {
    let (_dir, db) = setup_k1();
    let address = precompile_address(ADDR_CORE_WRITER);
    let words = |cloid: u128| {
        build_input(
            CLOID_PLACE_SIG,
            &[
                encode_market_id(1),
                abi::encode_u8(0),
                abi::encode_u8(0),
                abi::encode_u128(fp(50_000).raw() as u128),
                abi::encode_u128(fp(5).raw() as u128),
                abi::encode_u8(0),
                abi::encode_u128(cloid),
            ],
        )
    };
    let out = execute_precompile(&address, &words(CW_CLOID), &addr(1), &db, 100, 0).unwrap();
    assert_eq!(out, vec![0u8; 32]);
    execute_precompile(&address, &words(0), &addr(1), &db, 100, 0).unwrap();
    execute_precompile(&address, &place_order_input(0), &addr(1), &db, 100, 0).unwrap();
    let cloids: Vec<Option<u128>> = queued(&db, 101)
        .into_iter()
        .map(|qa| match qa.kind {
            QueuedActionKind::PlaceOrder { cloid, .. } => cloid,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(cloids, vec![Some(CW_CLOID), None, None]);
}

#[test]
fn core_writer_cancel_by_cloid_queues_it() {
    let (_dir, db) = setup_k1();
    let address = precompile_address(ADDR_CORE_WRITER);
    let input = build_input(
        "cancelByCloid(bytes32,uint128)",
        &[encode_market_id(3), abi::encode_u128(CW_CLOID)],
    );
    let out = execute_precompile(&address, &input, &addr(1), &db, 100, 0).unwrap();
    assert_eq!(out, abi::encode_bool(true).to_vec());
    let rows = queued(&db, 101);
    assert_eq!(rows.len(), 1);
    match &rows[0].kind {
        QueuedActionKind::CancelByCloid { market_id, cloid } => {
            assert_eq!((*market_id, *cloid), (3, CW_CLOID))
        }
        other => panic!("{other:?}"),
    }
}

/// Row formats: a placement row ends with its cloid option (`0` or
/// `1 ‖ 16 BE`); cancel-by-cloid is tag 8 (`market 8 BE ‖ cloid 16 BE`).
#[test]
fn cloid_queue_rows_round_trip() {
    use borsh::BorshDeserialize;
    let place = |cloid| QueuedActionKind::PlaceOrder {
        market_id: 1,
        side: 1,
        order_type: 0,
        price: fp(10),
        quantity: fp(1),
        time_in_force: 3,
        cloid,
    };
    for kind in [
        place(Some(CW_CLOID)),
        place(None),
        QueuedActionKind::CancelByCloid {
            market_id: 9,
            cloid: CW_CLOID,
        },
    ] {
        let qa = QueuedAction {
            trader: addr(3),
            kind,
            block_queued: 42,
        };
        let bytes = borsh::to_vec(&qa).unwrap();
        let back = QueuedAction::try_from_slice(&bytes).unwrap();
        assert_eq!(format!("{qa:?}"), format!("{back:?}"));
        match qa.kind {
            QueuedActionKind::PlaceOrder { cloid: Some(c), .. } => {
                let mut tail = vec![1u8];
                tail.extend_from_slice(&c.to_be_bytes());
                assert!(bytes.ends_with(&tail));
            }
            QueuedActionKind::PlaceOrder { cloid: None, .. } => {
                assert_eq!(*bytes.last().unwrap(), 0)
            }
            QueuedActionKind::CancelByCloid { .. } => {
                let mut want = vec![8u8];
                want.extend_from_slice(&9u64.to_be_bytes());
                want.extend_from_slice(&CW_CLOID.to_be_bytes());
                assert_eq!(&bytes[28..], &want[..]);
            }
            _ => unreachable!(),
        }
    }
}

// ============================================================================
// BboReader (0x080e) — item 7 step 2 B17
// ============================================================================

/// A `cf_native_bbo` row: bid raw ‖ ask raw (i128 BE).
fn put_bbo_row(db: &StateDb, market_id: MarketId, bid: i128, ask: i128) {
    let mut row = bid.to_be_bytes().to_vec();
    row.extend_from_slice(&ask.to_be_bytes());
    db.put_cf_raw(
        torus_state::cf::CF_NATIVE_BBO,
        &market_id.to_be_bytes(),
        &row,
    )
    .unwrap();
}

/// `bbo(bytes32)` answers the row's bid and ask as two uint128 words (0 for
/// an empty side or an unknown market); anything but that call is an error.
#[test]
fn bbo_reads_the_rows_bid_and_ask() {
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_BBO_READER);
    let call = |market: MarketId| {
        let input = build_input("bbo(bytes32)", &[encode_market_id(market)]);
        execute_precompile(&address, &input, &addr(0), &db, 100, 0)
    };
    let words = |bid: i128, ask: i128| {
        let mut out = abi::encode_u128(bid as u128).to_vec();
        out.extend_from_slice(&abi::encode_u128(ask as u128));
        out
    };

    put_bbo_row(&db, 1, fp(99).raw(), fp(101).raw());
    put_bbo_row(&db, 2, fp(7).raw(), 0);
    assert_eq!(call(1).unwrap(), words(fp(99).raw(), fp(101).raw()));
    assert_eq!(
        call(2).unwrap(),
        words(fp(7).raw(), 0),
        "an empty ask side is 0"
    );
    assert_eq!(call(3).unwrap(), words(0, 0), "no row: both sides 0");

    // Another selector.
    let input = build_input("getPrice(bytes32)", &[encode_market_id(1)]);
    let sel = u32::from_be_bytes(input[..4].try_into().unwrap());
    let r = execute_precompile(&address, &input, &addr(0), &db, 100, 0);
    assert!(
        matches!(r, Err(CoreError::UnknownSelector(s)) if s == sel),
        "{r:?}"
    );

    // Shorter than selector + one word.
    let full = build_input("bbo(bytes32)", &[encode_market_id(1)]);
    for len in [0, 3, 4, 35] {
        let r = execute_precompile(&address, &full[..len], &addr(0), &db, 100, 0);
        assert!(
            matches!(r, Err(CoreError::InvalidPrecompileInput(_))),
            "len {len}: {r:?}"
        );
    }

    // A row of the wrong length is this node's corruption (the CF is
    // unhashed): a local fault, never zeros.
    db.put_cf_raw(
        torus_state::cf::CF_NATIVE_BBO,
        &4u64.to_be_bytes(),
        &[1u8; 31],
    )
    .unwrap();
    let r = call(4);
    assert!(
        matches!(&r, Err(CoreError::PrecompileLocalFault(_))),
        "{r:?}"
    );
}

/// 0x080e is a reader: known, eth_call-safe, priced like the other point
/// readers (base 16,400 with the point read inside, + 20 per returned word).
#[test]
fn bbo_is_a_reader_at_0x080e() {
    let (_dir, db) = setup();
    assert_eq!(ADDR_BBO_READER, 0x080e);
    let address = precompile_address(ADDR_BBO_READER);
    assert!(is_precompile(&address));
    assert!(is_reader_precompile(ADDR_BBO_READER));
    assert_eq!(precompile_gas(ADDR_BBO_READER), GAS_PRECOMPILE_READ);
    assert!(ALL_PRECOMPILE_ADDRESSES.contains(&address));
    for id in [0x0804u16, 0x080d, 0x080f] {
        assert!(!is_precompile(&precompile_address(id)), "0x{id:04x}");
        assert!(!is_reader_precompile(id), "0x{id:04x}");
    }

    put_bbo_row(&db, 1, fp(99).raw(), fp(101).raw());
    let input = build_input("bbo(bytes32)", &[encode_market_id(1)]);
    let mut meter = ReadMeter::unlimited();
    let out = execute_precompile_metered(
        &address,
        &input,
        &addr(0),
        U256::ZERO,
        &db,
        100,
        0,
        true,
        &mut meter,
    )
    .expect("a reader runs in a read-only (eth_call) context");
    assert_eq!(out.len(), 64);
    // Owner s125: no row charge; two words.
    assert_eq!(meter.used(), 2 * GAS_PRECOMPILE_READ_PER_WORD);
    assert_eq!(reader_gas(meter.used()), 16_440);
}

/// Item 7 option 0 task 1.3 (guard on the new body): `pending_count` takes any
/// backend and `pending_count > 0` is exactly the due-check's
/// `prefix_exists(CF_CORE_WRITER_QUEUE, target)`, over the DB and over an
/// overlay whose own pending set drains one target and queues another.
#[test]
fn pending_count_matches_prefix_exists() {
    use torus_state::cf::CF_CORE_WRITER_QUEUE;
    use torus_state::StateBackend;
    let (_dir, db) = setup();
    let key = |t: u64, s: u64| {
        let mut k = t.to_be_bytes().to_vec();
        k.extend_from_slice(&s.to_be_bytes());
        k
    };
    for (t, n) in [(3u64, 1u64), (4, 3), (6, 2), (u64::MAX, 1)] {
        for s in 0..n {
            db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(t, s), b"row")
                .unwrap();
        }
    }
    let ov = NativeStateOverlay::new(db.clone());
    ov.delete_cf_raw(CF_CORE_WRITER_QUEUE, &key(3, 0)).unwrap();
    ov.delete_cf_raw(CF_CORE_WRITER_QUEUE, &key(4, 1)).unwrap();
    ov.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(5, 0), b"row")
        .unwrap();
    let expect_db = [
        (2u64, 0usize),
        (3, 1),
        (4, 3),
        (5, 0),
        (6, 2),
        (7, 0),
        (u64::MAX, 1),
    ];
    let expect_ov = [
        (2u64, 0usize),
        (3, 0),
        (4, 2),
        (5, 1),
        (6, 2),
        (7, 0),
        (u64::MAX, 1),
    ];
    for (t, n) in expect_db {
        assert_eq!(
            CoreWriterQueue::pending_count(&db, t).unwrap(),
            n,
            "db target {t}"
        );
        assert_eq!(
            db.prefix_exists(CF_CORE_WRITER_QUEUE, &t.to_be_bytes())
                .unwrap(),
            n > 0,
            "db target {t}"
        );
    }
    for (t, n) in expect_ov {
        assert_eq!(
            CoreWriterQueue::pending_count(&ov, t).unwrap(),
            n,
            "overlay target {t}"
        );
        assert_eq!(
            ov.prefix_exists(CF_CORE_WRITER_QUEUE, &t.to_be_bytes())
                .unwrap(),
            n > 0,
            "overlay target {t}"
        );
    }
}

/// Item 7 option 0 task 1.4: `enqueue` reads every row it decides on (the order delay, the
/// last-order target, the first-open cursor, the full-order run, the full-target probes,
/// `next_sequence`) through its backend, so with those rows held only in a layer of the
/// block overlay (the parent, L, or C, over an empty DB) it places the same rows at the same
/// targets and seqs, and writes the same cursor rows, as with them in the DB.
#[test]
fn enqueue_reads_cursor_rows_from_layers() {
    use std::sync::Arc;
    use torus_state::cf::{CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG};
    use torus_state::checkpoint_layer::CheckpointLayer;
    use torus_state::StateBackend;
    const CAP: u64 = torus_types::CORE_WRITER_DRAIN_CAP;
    let order = |block: u64, id: u128| QueuedAction {
        trader: addr(1),
        kind: QueuedActionKind::CancelOrder { order_id: id },
        block_queued: block,
    };
    let other = |block: u64| QueuedAction {
        trader: addr(2),
        kind: QueuedActionKind::ClaimRewards,
        block_queued: block,
    };
    // The DB state: target 101 full of orders (cursor rows written), one more order and a
    // non-order row placed past it.
    let (_dir, db) = setup_k1();
    for i in 0..CAP {
        CoreWriterQueue::enqueue(&db, &order(100, i.into())).unwrap();
    }
    CoreWriterQueue::enqueue(&db, &order(100, 9_999)).unwrap();
    CoreWriterQueue::enqueue(&db, &other(100)).unwrap();
    let rows: Vec<(&str, Vec<u8>, Vec<u8>)> = [CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG]
        .into_iter()
        .flat_map(|cf| {
            StateBackend::iterate_cf(&db, cf, None)
                .unwrap()
                .into_iter()
                .map(move |(k, v)| (cf, k, v))
        })
        .collect();
    assert!(rows
        .iter()
        .any(|(_, k, _)| k.as_slice() == CORE_WRITER_FIRST_OPEN_TARGET_KEY));
    assert!(rows
        .iter()
        .any(|(_, k, _)| k.as_slice() == CORE_WRITER_LAST_ORDER_TARGET_KEY));
    let next = |state: &NativeStateOverlay| {
        let seqs = vec![
            CoreWriterQueue::enqueue(state, &order(100, 1)).unwrap(),
            CoreWriterQueue::enqueue(state, &other(100)).unwrap(),
            CoreWriterQueue::enqueue(state, &order(105, 2)).unwrap(),
            CoreWriterQueue::enqueue(state, &other(101)).unwrap(),
        ];
        let view: Vec<_> = [CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG]
            .into_iter()
            .map(|cf| StateBackend::iterate_cf(state, cf, None).unwrap())
            .collect();
        (seqs, view)
    };
    let want = next(&NativeStateOverlay::new(db.clone()));
    assert_eq!(
        want.0,
        vec![2, 3, 0, 4],
        "past the full target, then K = 1 at 106"
    );

    let frozen = |empty: &StateDb| {
        let ov = NativeStateOverlay::new(empty.clone());
        for (cf, k, v) in &rows {
            ov.put_cf_raw(cf, k, v).unwrap();
        }
        ov.freeze(99)
    };
    let layer = |empty: &StateDb| {
        let mut l = CheckpointLayer::new();
        l.merge(&frozen(empty));
        Arc::new(l)
    };
    for place in ["parent", "L", "C"] {
        let (_d, empty) = setup();
        let state = match place {
            "parent" => NativeStateOverlay::with_parent(empty.clone(), Some(frozen(&empty))),
            "L" => NativeStateOverlay::with_layers(empty.clone(), None, Some(layer(&empty)), None),
            _ => NativeStateOverlay::with_layers(empty.clone(), None, None, Some(layer(&empty))),
        };
        assert_eq!(next(&state), want, "rows in {place}");
    }
}
