//! Integration tests for cross-VM precompiles, lockbox, and CoreWriterQueue (task 2.4).

use alloy_primitives::keccak256;
use torus_core::error::CoreError;
use torus_core::lockbox::Lockbox;
use torus_core::position::{NativeBalance, Position, PositionManager};
use torus_core::precompiles::*;
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, MarketId, U256};

// ============================================================================
// Helpers
// ============================================================================

fn setup() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
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

/// Write an aggregated oracle price to CF_NATIVE_ORACLE.
fn write_oracle_price(db: &StateDb, market_id: MarketId, price: FixedPoint, block: u64) {
    let mut key = Vec::with_capacity(11);
    key.extend_from_slice(b"agg");
    key.extend_from_slice(&market_id.to_be_bytes());

    let mut data = Vec::with_capacity(28);
    data.extend_from_slice(&price.raw().to_be_bytes());
    data.extend_from_slice(&block.to_be_bytes());
    data.extend_from_slice(&3u32.to_be_bytes()); // num_reporters

    db.put_cf_raw("cf_native_oracle", &key, &data).unwrap();
}

// ============================================================================
// Lockbox Tests
// ============================================================================

#[test]
fn lockbox_deposit_to_native() {
    let (_dir, db) = setup();
    let trader = addr(1);

    // Give trader 10,000 EVM balance (raw FixedPoint units)
    set_evm_balance(
        &db,
        &trader,
        U256::from(10_000u64 * FixedPoint::SCALE as u64),
    );

    // Deposit 3,000 to native
    Lockbox::deposit_to_native(&db, &trader, fp(3_000)).unwrap();

    // Verify EVM decreased
    let evm = get_evm_balance(&db, &trader);
    assert_eq!(evm, U256::from(7_000u64 * FixedPoint::SCALE as u64));

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

    // Verify EVM increased
    let evm = get_evm_balance(&db, &trader);
    assert_eq!(evm, U256::from(2_000u64 * FixedPoint::SCALE as u64));
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

#[test]
fn order_book_reader_get_order_book() {
    use torus_core::order_book::OrderBook;
    use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

    let (_dir, db) = setup();

    // Populate a real book producing the levels 50_000×10, 49_900×20 / 50_100×5
    // and persist it through the production per-order-row store.
    let limit = |is_buy: bool, price: i64, qty: i64| PlaceOrderParams {
        market_id: 1,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let mut book = OrderBook::new(1, fp(100), fp(1));
    book.place_order(limit(true, 50_000, 10), addr(1), 1);
    book.place_order(limit(true, 49_900, 20), addr(2), 1);
    book.place_order(limit(false, 50_100, 5), addr(3), 1);
    torus_core::order_book_store::save_book_full(&db, &mut book).unwrap();

    // Call precompile
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(1)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();

    // Decode: 4 offsets + arrays. Verify we got data back.
    assert!(output.len() > 128); // At least 4 offset words
}

/// Decode an `encode_arrays_response` payload of N u128 arrays (the getOrderBook
/// shape). Returns each array as a `Vec<u128>`.
fn decode_u128_arrays(output: &[u8], n: usize) -> Vec<Vec<u128>> {
    let read_u32 = |off: usize| -> usize {
        u32::from_be_bytes(output[off + 28..off + 32].try_into().unwrap()) as usize
    };
    let read_u128 = |off: usize| -> u128 {
        u128::from_be_bytes(output[off + 16..off + 32].try_into().unwrap())
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let arr_off = read_u32(i * 32);
        let len = read_u32(arr_off);
        let mut arr = Vec::with_capacity(len);
        for j in 0..len {
            arr.push(read_u128(arr_off + 32 + j * 32));
        }
        out.push(arr);
    }
    out
}

/// CONSENSUS-VISIBLE oracle: the reader precompile must decode the format
/// `save_order_books` actually writes (since the deep-book round: per-order
/// rows), aggregating resting orders into price levels. The expected level
/// values are UNCHANGED from the monolithic-blob baseline — only the persist
/// helper moved to the row store.
#[test]
fn order_book_reader_decodes_persisted_orderbook_blob() {
    use torus_core::order_book::OrderBook;
    use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

    let (_dir, db) = setup();

    let limit = |is_buy: bool, price: i64, qty: i64| PlaceOrderParams {
        market_id: 7,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };

    let mut book = OrderBook::new(7, fp(1), fp(1));
    book.place_order(limit(true, 100, 5), addr(1), 1);
    book.place_order(limit(true, 100, 3), addr(2), 1); // same level -> sums to 8
    book.place_order(limit(true, 99, 2), addr(3), 1);
    book.place_order(limit(false, 101, 4), addr(4), 1);
    book.place_order(limit(false, 102, 1), addr(5), 1);

    // Write the ACTUAL persisted format (per-order rows).
    torus_core::order_book_store::save_book_full(&db, &mut book).unwrap();

    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(7)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100)
        .expect("persisted book rows must decode, not revert");

    let arrays = decode_u128_arrays(&output, 4);
    let (bid_prices, bid_qtys, ask_prices, ask_qtys) =
        (&arrays[0], &arrays[1], &arrays[2], &arrays[3]);

    // Bids descending, quantities summed.
    assert_eq!(bid_prices, &[fp(100).raw() as u128, fp(99).raw() as u128]);
    assert_eq!(bid_qtys, &[fp(8).raw() as u128, fp(2).raw() as u128]);
    // Asks ascending.
    assert_eq!(ask_prices, &[fp(101).raw() as u128, fp(102).raw() as u128]);
    assert_eq!(ask_qtys, &[fp(4).raw() as u128, fp(1).raw() as u128]);
}

/// Deep-book round: a LEGACY monolithic value (pre-round layout) makes the
/// reader precompile revert LOUDLY — it must never be silently misread as an
/// empty/partial book.
#[test]
fn order_book_reader_reverts_on_legacy_value() {
    use torus_core::order_book::OrderBook;
    use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

    let (_dir, db) = setup();
    let mut legacy = OrderBook::new(9, fp(1), fp(1));
    legacy.place_order(
        PlaceOrderParams {
            market_id: 9,
            is_buy: true,
            price: fp(100),
            quantity: fp(5),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        },
        addr(1),
        1,
    );
    db.put_cf_raw(
        torus_state::cf::CF_NATIVE_ORDER_BOOKS,
        &9u64.to_be_bytes(),
        &borsh::to_vec(&legacy).unwrap(),
    )
    .unwrap();

    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(9)]);
    let result = execute_precompile(&address, &input, &addr(0), &db, 100);
    assert!(result.is_err(), "legacy monolithic value must revert loudly");
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
        realized_pnl: fp(100),
        isolated_margin: fp(2_500),
        margin_type: torus_core::position::MarginType::Isolated,
    })
    .unwrap();

    // Write oracle price for unrealized PnL computation
    write_oracle_price(&db, 1, fp(51_000), 100);

    // Call precompile
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input(
        "getPosition(address,bytes32)",
        &[encode_addr(&trader), encode_market_id(1)],
    );
    let output = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();

    // 5 words = 160 bytes
    assert_eq!(output.len(), 160);

    // Decode size (signed i128): should be positive (long)
    let size = i128::from_be_bytes(output[16..32].try_into().unwrap());
    assert_eq!(size, fp(5).raw());

    // Decode entry_price
    let entry = u128::from_be_bytes(output[48..64].try_into().unwrap());
    assert_eq!(entry, fp(50_000).raw() as u128);
}

#[test]
fn order_book_reader_get_open_orders() {
    let (_dir, db) = setup();
    let trader = addr(1);

    // Write stored orders
    let order1 = StoredOrder {
        order_id: 1001,
        price: fp(49_000),
        remaining_qty: fp(3),
        side: 0, // Buy
    };
    let order2 = StoredOrder {
        order_id: 1002,
        price: fp(51_000),
        remaining_qty: fp(7),
        side: 1, // Sell
    };
    write_stored_order(&db, &trader, 1, &order1).unwrap();
    write_stored_order(&db, &trader, 1, &order2).unwrap();

    // Call precompile
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input(
        "getOpenOrders(address,bytes32)",
        &[encode_addr(&trader), encode_market_id(1)],
    );
    let output = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();

    // Should have 4 dynamic arrays with 2 elements each
    assert!(output.len() > 128);
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
    let output = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();

    // 4 static values = 128 bytes
    assert_eq!(output.len(), 128);

    // Native balance
    let native = u128::from_be_bytes(output[16..32].try_into().unwrap());
    assert_eq!(native, fp(10_000).raw() as u128);

    // EVM balance
    let evm = u128::from_be_bytes(output[48..64].try_into().unwrap());
    assert_eq!(evm, 5_000u128 * FixedPoint::SCALE as u128);
}

// ============================================================================
// OracleReader Precompile Tests
// ============================================================================

#[test]
fn oracle_reader_get_price() {
    let (_dir, db) = setup();

    write_oracle_price(&db, 1, fp(50_000), 95);

    let address = precompile_address(ADDR_ORACLE_READER);
    let input = build_input("getPrice(bytes32)", &[encode_market_id(1)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();

    // 3 values = 96 bytes
    assert_eq!(output.len(), 96);

    // Price
    let price = u128::from_be_bytes(output[16..32].try_into().unwrap());
    assert_eq!(price, fp(50_000).raw() as u128);

    // Block number
    let block = u64::from_be_bytes(output[56..64].try_into().unwrap());
    assert_eq!(block, 95);

    // Stale: 100 - 95 = 5, not > 100, so not stale
    assert_eq!(output[95], 0); // false
}

#[test]
fn oracle_reader_stale_price() {
    let (_dir, db) = setup();

    write_oracle_price(&db, 1, fp(50_000), 10);

    let address = precompile_address(ADDR_ORACLE_READER);
    let input = build_input("getPrice(bytes32)", &[encode_market_id(1)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 200).unwrap();

    // Stale: 200 - 10 = 190, > 100, so stale=true
    assert_eq!(output[95], 1); // true
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
    let output = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();

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
    let (_dir, db) = setup();
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

    let output = execute_precompile(&address, &input, &caller, &db, 100).unwrap();

    // Output should be a bytes32 (order_id) = 32 bytes
    assert_eq!(output.len(), 32);

    // Verify action is queued for block 101 (current_block + 1)
    let count = CoreWriterQueue::pending_count(&db, 101).unwrap();
    assert_eq!(count, 1);

    // Action should NOT be in current block (anti-frontrunning)
    let count_current = CoreWriterQueue::pending_count(&db, 100).unwrap();
    assert_eq!(count_current, 0);
}

#[test]
fn core_writer_cancel_order_queues_action() {
    let (_dir, db) = setup();
    let caller = addr(1);

    let address = precompile_address(ADDR_CORE_WRITER);
    let input = build_input("cancelOrder(bytes32)", &[abi::encode_order_id(42)]);

    let output = execute_precompile(&address, &input, &caller, &db, 100).unwrap();
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

    let output = execute_precompile(&address, &input, &caller, &db, 50).unwrap();
    assert_eq!(output[31], 1); // success

    assert_eq!(CoreWriterQueue::pending_count(&db, 51).unwrap(), 1);
}

// ============================================================================
// CoreWriterQueue Tests
// ============================================================================

#[test]
fn queue_enqueue_and_drain() {
    let (_dir, db) = setup();

    let action1 = QueuedAction {
        trader: addr(1),
        kind: QueuedActionKind::PlaceOrder {
            market_id: 1,
            side: 0,
            order_type: 0,
            price: fp(50_000),
            quantity: fp(5),
            time_in_force: 0,
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
    let drained = CoreWriterQueue::drain(&db, 101).unwrap();
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
    let drained = CoreWriterQueue::drain(&db, 5).unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].trader, addr(1));

    // Block 6 still has its action
    assert_eq!(CoreWriterQueue::pending_count(&db, 6).unwrap(), 1);
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

#[test]
fn lockbox_precompile_deposit() {
    let (_dir, db) = setup();
    let trader = addr(5);

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

    let output = execute_precompile(&address, &input, &trader, &db, 100).unwrap();
    assert_eq!(output[31], 1); // true

    // Verify balances changed
    let evm = get_evm_balance(&db, &trader);
    assert_eq!(evm, U256::from(7_000u64 * FixedPoint::SCALE as u64));

    let pm = PositionManager::new(db);
    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, fp(3_000));
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
        &[abi::encode_u128(fp(2_000).raw() as u128)],
    );

    let output = execute_precompile(&address, &input, &trader, &db, 100).unwrap();
    assert_eq!(output[31], 1); // true

    // Verify native decreased
    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, fp(6_000));

    // Verify EVM increased
    let evm = get_evm_balance(&db, &trader);
    assert_eq!(evm, U256::from(2_000u64 * FixedPoint::SCALE as u64));
}

// ============================================================================
// Unknown selector error
// ============================================================================

#[test]
fn unknown_selector_returns_error() {
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = vec![0xDE, 0xAD, 0xBE, 0xEF]; // bogus selector

    let result = execute_precompile(&address, &input, &addr(0), &db, 100);
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
    let result = execute_precompile_read_only(&address, &input, &trader, &db, 100);
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
    let (_dir, db) = setup();
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

    let result = execute_precompile_read_only(&address, &input, &caller, &db, 100);
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

    let result = execute_precompile_read_only(&address, &input, &addr(1), &db, 50);
    assert!(result.is_err());
    assert_eq!(CoreWriterQueue::pending_count(&db, 51).unwrap(), 0);
}

#[test]
fn read_only_is_transparent_for_reader_precompiles() {
    let (_dir, db) = setup();
    // The guard must not affect reader precompiles: same result on both paths.
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(1)]);

    let ro = execute_precompile_read_only(&address, &input, &addr(0), &db, 100);
    let normal = execute_precompile(&address, &input, &addr(0), &db, 100);
    assert!(
        ro.is_ok(),
        "reader precompile must still run in read-only mode"
    );
    assert_eq!(ro.unwrap(), normal.unwrap());
}

// ============================================================================
// Top-N bounded read + per-level gas (0x0800 top-N gas round)
// ============================================================================

fn encode_u32_word(v: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[28..].copy_from_slice(&v.to_be_bytes());
    w
}

/// Seed a book with 5 bid levels (100..96) and 3 ask levels (101..103),
/// one order each, qty = 10+level index for uniqueness.
fn seed_multilevel_book(db: &StateDb, market_id: u64) {
    use torus_core::order_book::OrderBook;
    use torus_types::{OrderType, PlaceOrderParams, TimeInForce};
    let limit = |is_buy: bool, price: i64, qty: i64| PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let mut book = OrderBook::new(market_id, fp(1), fp(1));
    for (i, p) in [100i64, 99, 98, 97, 96].iter().enumerate() {
        book.place_order(limit(true, *p, 10 + i as i64), addr(i as u8 + 1), 1);
    }
    for (i, p) in [101i64, 102, 103].iter().enumerate() {
        book.place_order(limit(false, *p, 20 + i as i64), addr(i as u8 + 10), 1);
    }
    torus_core::order_book_store::save_book_full(db, &mut book).unwrap();
}

#[test]
fn get_order_book_explicit_n_truncates_best_first_and_charges_per_level() {
    use torus_core::precompiles::{
        execute_precompile_with_gas, GAS_PER_BOOK_LEVEL, GAS_PRECOMPILE_READ,
    };
    let (_dir, db) = setup();
    seed_multilevel_book(&db, 11);
    let address = precompile_address(ADDR_ORDER_BOOK_READER);

    // n=2: best 2 bids (100, 99) and best 2 asks (101, 102).
    let input = build_input(
        "getOrderBook(bytes32,uint32)",
        &[encode_market_id(11), encode_u32_word(2)],
    );
    let out = execute_precompile_with_gas(&address, &input, &addr(0), &db, 100).unwrap();
    let arrays = decode_u128_arrays(&out.data, 4);
    assert_eq!(
        arrays[0],
        vec![fp(100).raw() as u128, fp(99).raw() as u128],
        "top-2 bids best-first"
    );
    assert_eq!(arrays[1], vec![fp(10).raw() as u128, fp(11).raw() as u128]);
    assert_eq!(
        arrays[2],
        vec![fp(101).raw() as u128, fp(102).raw() as u128],
        "top-2 asks best-first"
    );
    assert_eq!(arrays[3], vec![fp(20).raw() as u128, fp(21).raw() as u128]);
    assert_eq!(
        out.gas_used,
        GAS_PRECOMPILE_READ + GAS_PER_BOOK_LEVEL * 4,
        "base + k×(2 bids + 2 asks)"
    );
}

#[test]
fn get_order_book_legacy_selector_returns_all_levels_under_cap_with_surcharge() {
    use torus_core::precompiles::{
        execute_precompile_with_gas, GAS_PER_BOOK_LEVEL, GAS_PRECOMPILE_READ,
    };
    let (_dir, db) = setup();
    seed_multilevel_book(&db, 12);
    let address = precompile_address(ADDR_ORDER_BOOK_READER);

    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(12)]);
    let out = execute_precompile_with_gas(&address, &input, &addr(0), &db, 100).unwrap();
    let arrays = decode_u128_arrays(&out.data, 4);
    assert_eq!(arrays[0].len(), 5, "all 5 bid levels (below N cap)");
    assert_eq!(arrays[2].len(), 3, "all 3 ask levels");
    assert_eq!(
        arrays[0][0],
        fp(100).raw() as u128,
        "bids best(high)-first"
    );
    assert_eq!(arrays[2][0], fp(101).raw() as u128, "asks best(low)-first");
    assert_eq!(
        out.gas_used,
        GAS_PRECOMPILE_READ + GAS_PER_BOOK_LEVEL * 8,
        "base + k×(5+3)"
    );
}

#[test]
fn get_order_book_absent_market_is_empty_at_base_gas() {
    use torus_core::precompiles::{execute_precompile_with_gas, GAS_PRECOMPILE_READ};
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_ORDER_BOOK_READER);

    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(999)]);
    let out = execute_precompile_with_gas(&address, &input, &addr(0), &db, 100).unwrap();
    let arrays = decode_u128_arrays(&out.data, 4);
    assert!(arrays.iter().all(|a| a.is_empty()), "absent market = empty");
    assert_eq!(out.gas_used, GAS_PRECOMPILE_READ, "no levels → base gas only");
}

#[test]
fn get_order_book_n_zero_is_empty_and_n_is_capped() {
    use torus_core::precompiles::{
        execute_precompile_with_gas, GAS_PRECOMPILE_READ, TOP_N_LEVELS_PER_SIDE,
    };
    let (_dir, db) = setup();
    seed_multilevel_book(&db, 13);
    let address = precompile_address(ADDR_ORDER_BOOK_READER);

    // n=0 → empty arrays, base gas.
    let input = build_input(
        "getOrderBook(bytes32,uint32)",
        &[encode_market_id(13), encode_u32_word(0)],
    );
    let out = execute_precompile_with_gas(&address, &input, &addr(0), &db, 100).unwrap();
    assert!(decode_u128_arrays(&out.data, 4).iter().all(|a| a.is_empty()));
    assert_eq!(out.gas_used, GAS_PRECOMPILE_READ);

    // n=u32::MAX → clamped to TOP_N (all 5+3 levels here), never over cap.
    let input = build_input(
        "getOrderBook(bytes32,uint32)",
        &[encode_market_id(13), encode_u32_word(u32::MAX)],
    );
    let out = execute_precompile_with_gas(&address, &input, &addr(0), &db, 100).unwrap();
    let arrays = decode_u128_arrays(&out.data, 4);
    assert_eq!(arrays[0].len(), 5);
    assert!(TOP_N_LEVELS_PER_SIDE >= 5);
}
