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

#[test]
fn order_book_reader_get_order_book() {
    let (_dir, db) = setup();

    // Populate order book snapshot
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

    // Call precompile
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(1)]);
    let output = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();

    // Decode: 4 offsets + arrays. Verify we got data back.
    assert!(output.len() > 128); // At least 4 offset words
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
    let output = execute_precompile(&address, &input, &addr(0), &db, 100, 0).unwrap();

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
/// zero: no id; contracts read their orders back via getOpenOrders.
#[test]
fn core_writer_place_order_returns_no_synthetic_order_id() {
    let (_dir, db) = setup();
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
    let (_dir, db) = setup();
    let address = precompile_address(ADDR_CORE_WRITER);
    for order_type in [2u8, 3] {
        let err = execute_precompile(&address, &place_order_input(order_type), &addr(1), &db, 100, 0);
        assert!(err.is_err(), "order_type {order_type} must be rejected");
    }
    assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 0, "nothing queued");
}

#[test]
fn core_writer_cancel_order_queues_action() {
    let (_dir, db) = setup();
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
    let (_dir, db) = setup();
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
    let queued = CoreWriterQueue::drain(&db, 101).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].trader, trader);
    assert!(matches!(
        queued[0].kind,
        QueuedActionKind::LockboxDeposit { amount } if amount == fp(3_000)
    ));
}

#[test]
fn lockbox_precompile_deposit_rejects_bad_value() {
    let (_dir, db) = setup();
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
    let queued = CoreWriterQueue::drain(&db, 101).unwrap();
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
    let input = build_input("getOrderBook(bytes32)", &[encode_market_id(1)]);

    let ro = execute_precompile_read_only(&address, &input, &addr(0), &db, 100, 0);
    let normal = execute_precompile(&address, &input, &addr(0), &db, 100, 0);
    assert!(
        ro.is_ok(),
        "reader precompile must still run in read-only mode"
    );
    assert_eq!(ro.unwrap(), normal.unwrap());
}
