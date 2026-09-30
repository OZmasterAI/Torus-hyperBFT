//! EVM-PF-05 end-to-end: lockbox precompile 0x0820 → CoreWriter queue → next-block drain.
//!
//! Hyperliquid model: the precompile never writes `CF_ACCOUNTS` or native balances
//! mid-EVM. The EVM leg moves through revm (deposit = payable call whose value is
//! burned in-frame); the native leg is queued and applied next block by
//! `NativeExecutor::drain_core_writer`, which runs on the native path after the
//! block's EVM bundle has been committed. Each block here is executed with
//! `execute_block` and committed with `evm_block_batch_incremental` (the consensus
//! commit path); fees are zero so value conservation is exact.

use alloy_primitives::{Address, Bytes, B256, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::lockbox::WEI_PER_NATIVE_UNIT;
use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
use torus_evm::{BlockEnvCfg, EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;
use torus_types::FixedPoint;

const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

const ALICE: Address = Address::new([0xAA; 20]);
const BOB: Address = Address::new([0xBB; 20]);
const BENEFICIARY: Address = Address::new([0xFF; 20]);
const LOCKBOX: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x20,
]);

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

/// `n` whole native tokens as a FixedPoint (8 decimals).
fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

/// `n` whole native tokens in wei (18 decimals).
fn wei(n: u64) -> U256 {
    U256::from(n) * U256::from(1_000_000_000_000_000_000u128)
}

fn fund_evm(db: &StateDb, who: &Address, balance: U256) {
    db.put_account(
        who,
        &AccountInfo {
            balance,
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            code: None,
            account_id: None,
        },
    )
    .unwrap();
}

fn evm_balance(db: &StateDb, who: &Address) -> U256 {
    db.get_account(who)
        .unwrap()
        .map(|a| a.balance)
        .unwrap_or(U256::ZERO)
}

fn native(db: &StateDb, who: &Address) -> FixedPoint {
    PositionManager::new(db.clone())
        .get_native_balance(who)
        .unwrap()
        .available
}

fn seed_native(db: &StateDb, who: &Address, amount: FixedPoint) {
    PositionManager::new(db.clone())
        .put_native_balance(
            who,
            &NativeBalance {
                available: amount,
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
}

/// EVM balances + native balances scaled to wei, over every account touched.
fn total_wei(db: &StateDb) -> U256 {
    let mut total = evm_balance(db, &LOCKBOX) + evm_balance(db, &BENEFICIARY);
    for a in [ALICE, BOB] {
        total += evm_balance(db, &a);
        total += U256::from(native(db, &a).raw() as u128) * U256::from(WEI_PER_NATIVE_UNIT);
    }
    total
}

fn lockbox_call(from: Address, sig: &str, amount: U256, value: U256, nonce: u64) -> TxEnv {
    let sig_hash = alloy_primitives::keccak256(sig.as_bytes());
    let mut data = sig_hash[..4].to_vec();
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    TxEnv {
        caller: from,
        gas_limit: 200_000,
        gas_price: 0,
        kind: TxKind::Call(LOCKBOX),
        value,
        data: Bytes::from(data),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

fn deposit(from: Address, value: U256, nonce: u64) -> TxEnv {
    lockbox_call(from, "depositToNative(uint128)", value, value, nonce)
}

fn withdraw(from: Address, amount_wei: U256, nonce: u64) -> TxEnv {
    lockbox_call(from, "withdrawFromNative(uint128)", amount_wei, U256::ZERO, nonce)
}

fn transfer(from: Address, to: Address, value: U256, nonce: u64) -> TxEnv {
    TxEnv {
        caller: from,
        gas_limit: 21_000,
        gas_price: 0,
        kind: TxKind::Call(to),
        value,
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

/// Execute `txs` as EVM block `number`, commit the bundle (consensus path).
fn evm_block(db: &StateDb, number: u64, txs: Vec<TxEnv>) -> Vec<bool> {
    let cfg = BlockEnvCfg {
        number,
        timestamp: 1_000_000 + number,
        beneficiary: BENEFICIARY,
        gas_limit: 30_000_000,
        base_fee: 0,
    };
    let result = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block(db, &cfg, txs, false)
        .unwrap();
    let (_root, batch) = torus_state::incremental::evm_block_batch_incremental(
        db,
        &result.bundle,
        None,
        Some(&result.native_writes),
    )
    .unwrap();
    db.write(batch).unwrap();
    result.receipts.iter().map(|r| r.status).collect()
}

/// Run the native-path CoreWriter drain for block `height`; returns per-action success.
fn drain(db: &StateDb, height: u64) -> Vec<(bool, Option<String>)> {
    let mut ctx = NativeExecContext::new(
        db.clone(),
        height,
        1_000_000 + height,
        0,
        100,
        10,
        Address::with_last_byte(99),
        Address::with_last_byte(100),
        Address::with_last_byte(101),
    );
    NativeExecutor::drain_core_writer(&mut ctx)
        .unwrap()
        .into_iter()
        .map(|r| (r.success, r.error))
        .collect()
}

/// Block A: deposit (with sub-unit dust) then transfer. EVM is debited by revm in
/// block 1; the native credit floor(value / 10^10) lands in block 2; dust burned.
#[test]
fn block_a_deposit_then_transfer_credits_native_next_block() {
    let (_dir, db) = open_test_db();
    let e0 = wei(1_000);
    fund_evm(&db, &ALICE, e0);
    let before = total_wei(&db);

    let dust = U256::from(9_999_999_999u64); // one wei short of a native unit
    let v = wei(300) + dust;
    let t = wei(100);
    assert_eq!(
        evm_block(&db, 1, vec![deposit(ALICE, v, 0), transfer(ALICE, BOB, t, 1)]),
        vec![true, true]
    );
    assert_eq!(evm_balance(&db, &ALICE), e0 - v - t);
    assert_eq!(evm_balance(&db, &LOCKBOX), U256::ZERO);
    assert_eq!(native(&db, &ALICE), FixedPoint::ZERO, "not credited mid-block");

    assert_eq!(drain(&db, 2), vec![(true, None)]);
    assert_eq!(native(&db, &ALICE), fp(300), "credited floor(value / 1e10)");
    assert_eq!(evm_balance(&db, &ALICE), e0 - v - t, "drain never touches the EVM side");
    assert_eq!(total_wei(&db) + dust, before, "conservation: exactly the dust is burned");
}

/// Block B: transfer-in then deposit — revm validates the deposit value against
/// the in-block balance (the pre-fix lockbox read the stale pre-block record).
#[test]
fn block_b_transfer_in_then_deposit_uses_fresh_balance() {
    let (_dir, db) = open_test_db();
    fund_evm(&db, &BOB, wei(1_000));
    let before = total_wei(&db);

    assert_eq!(
        evm_block(
            &db,
            1,
            vec![transfer(BOB, ALICE, wei(500), 0), deposit(ALICE, wei(400), 0)]
        ),
        vec![true, true]
    );
    assert_eq!(drain(&db, 2), vec![(true, None)]);
    assert_eq!(evm_balance(&db, &ALICE), wei(100));
    assert_eq!(native(&db, &ALICE), fp(400));
    assert_eq!(total_wei(&db), before, "value conservation");
}

/// Block C: withdraw then transfer. Nothing moves in block 1 (so nothing can be
/// clobbered); block 2's drain debits native and credits EVM ×10^10.
#[test]
fn block_c_withdraw_then_transfer_credits_evm_next_block() {
    let (_dir, db) = open_test_db();
    let e0 = wei(1_000);
    fund_evm(&db, &ALICE, e0);
    seed_native(&db, &ALICE, fp(900));
    let before = total_wei(&db);

    let t = wei(100);
    assert_eq!(
        evm_block(&db, 1, vec![withdraw(ALICE, wei(200), 0), transfer(ALICE, BOB, t, 1)]),
        vec![true, true]
    );
    assert_eq!(evm_balance(&db, &ALICE), e0 - t);
    assert_eq!(native(&db, &ALICE), fp(900));

    assert_eq!(drain(&db, 2), vec![(true, None)]);
    assert_eq!(native(&db, &ALICE), fp(700));
    assert_eq!(evm_balance(&db, &ALICE), e0 - t + wei(200));
    assert_eq!(total_wei(&db), before, "value conservation");

    // The next EVM block sees the drained credit (fresh CF_ACCOUNTS read).
    assert_eq!(
        evm_block(&db, 3, vec![transfer(ALICE, BOB, e0 - t + wei(200), 2)]),
        vec![true]
    );
    assert_eq!(evm_balance(&db, &ALICE), U256::ZERO);
}

/// A queued withdraw whose native balance is gone by drain time fails cleanly:
/// error result, native and EVM untouched.
#[test]
fn queued_withdraw_with_insufficient_native_at_drain_is_a_no_op_error() {
    let (_dir, db) = open_test_db();
    fund_evm(&db, &ALICE, wei(10));
    seed_native(&db, &ALICE, fp(50));

    assert_eq!(evm_block(&db, 1, vec![withdraw(ALICE, wei(40), 0)]), vec![true]);
    // Native balance spent elsewhere before the drain runs.
    seed_native(&db, &ALICE, fp(30));

    let results = drain(&db, 2);
    assert_eq!(results.len(), 1);
    assert!(!results[0].0, "insufficient native balance must fail the queued withdraw");
    assert_eq!(native(&db, &ALICE), fp(30));
    assert_eq!(evm_balance(&db, &ALICE), wei(10));
}

/// Round trip: native fp(5) → EVM 5e18 wei → native fp(5), no loss.
#[test]
fn round_trip_native_to_evm_and_back_is_lossless() {
    let (_dir, db) = open_test_db();
    fund_evm(&db, &ALICE, U256::ZERO);
    seed_native(&db, &ALICE, fp(5));

    let five_wei = U256::from(5_000_000_000_000_000_000u128);
    assert_eq!(evm_block(&db, 1, vec![withdraw(ALICE, five_wei, 0)]), vec![true]);
    assert_eq!(drain(&db, 2), vec![(true, None)]);
    assert_eq!(evm_balance(&db, &ALICE), five_wei);
    assert_eq!(native(&db, &ALICE), FixedPoint::ZERO);

    assert_eq!(evm_block(&db, 3, vec![deposit(ALICE, five_wei, 1)]), vec![true]);
    assert_eq!(drain(&db, 4), vec![(true, None)]);
    assert_eq!(native(&db, &ALICE), fp(5));
    assert_eq!(evm_balance(&db, &ALICE), U256::ZERO);
    assert_eq!(evm_balance(&db, &LOCKBOX), U256::ZERO);
}

// ---------------------------------------------------------------------------
// F1 (s515 review): the queued native leg must commit ATOMICALLY with the EVM
// bundle that burned the deposit — never before it. Pre-fix, `execute_block`
// persisted each successful tx's queue row straight to RocksDB, so (a) a block
// whose EVM execution errored AFTER a deposit (bundle dropped, burn never
// persisted) still credited native next block, and (b) a crash before the bundle
// commit replayed the tx with a fresh sequence number — a double credit.
// ---------------------------------------------------------------------------

/// Committed block whose EVM execution errors after a successful deposit (the
/// app logs "EVM execution failed for committed block" and moves on): no bundle
/// is committed, so nothing may be queued for the next block.
#[test]
fn deposit_in_errored_evm_block_credits_nothing_next_block() {
    let (_dir, db) = open_test_db();
    let e0 = wei(1_000);
    fund_evm(&db, &ALICE, e0);

    // Each tx fits the block gas limit on its own; together they exceed it, so
    // `execute_block` returns BlockGasLimitExceeded after the deposit succeeded.
    let cfg = BlockEnvCfg {
        number: 1,
        timestamp: 1_000_001,
        beneficiary: BENEFICIARY,
        gas_limit: 100_000,
        base_fee: 0,
    };
    let mut dep = deposit(ALICE, wei(300), 0);
    dep.gas_limit = 100_000;
    let txs = vec![
        dep,
        transfer(ALICE, BOB, wei(1), 1),
        transfer(ALICE, BOB, wei(1), 2),
        transfer(ALICE, BOB, wei(1), 3),
        transfer(ALICE, BOB, wei(1), 4),
    ];
    let err = EvmExecutor::new(TORUS_CHAIN_ID).execute_block(&db, &cfg, txs, true);
    assert!(
        matches!(err, Err(torus_evm::EvmError::BlockGasLimitExceeded { .. })),
        "block must error mid-way: {err:?}"
    );

    assert_eq!(drain(&db, 2), vec![], "no queued action may outlive the dropped bundle");
    assert_eq!(native(&db, &ALICE), FixedPoint::ZERO, "no native mint");
    assert_eq!(evm_balance(&db, &ALICE), e0, "burn was never persisted either");
}

/// Crash after EVM execution but before the bundle commit: restart replays the
/// same block. Exactly one native credit must result.
#[test]
fn replayed_deposit_block_credits_native_exactly_once() {
    let (_dir, db) = open_test_db();
    let e0 = wei(1_000);
    fund_evm(&db, &ALICE, e0);
    let before = total_wei(&db);

    let cfg = BlockEnvCfg {
        number: 1,
        timestamp: 1_000_001,
        beneficiary: BENEFICIARY,
        gas_limit: 30_000_000,
        base_fee: 0,
    };
    // First execution: result dropped (crash before the bundle commit).
    let crashed = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block(&db, &cfg, vec![deposit(ALICE, wei(300), 0)], true)
        .unwrap();
    drop(crashed);
    // Replay: same block, committed this time.
    assert_eq!(evm_block(&db, 1, vec![deposit(ALICE, wei(300), 0)]), vec![true]);

    assert_eq!(drain(&db, 2), vec![(true, None)], "exactly one queued credit");
    assert_eq!(native(&db, &ALICE), fp(300));
    assert_eq!(evm_balance(&db, &ALICE), e0 - wei(300));
    assert_eq!(total_wei(&db), before, "value conservation across the replay");
}

/// F1 (s517 #5): a queued `withdrawFromNative` (drains as TransferToSpot)
/// obeys the transfer margin: long 10 @100 (10% floor 100) on 150 native —
/// 51 fails at the drain, 50 passes.
#[test]
fn queued_withdraw_respects_account_transfer_margin() {
    for (amount, ok) in [(51u64, false), (50, true)] {
        let (_dir, db) = open_test_db();
        fund_evm(&db, &ALICE, wei(10));
        seed_native(&db, &ALICE, fp(150));
        PositionManager::new(db.clone())
            .put_position(&Position {
                trader: ALICE,
                market_id: 1,
                is_long: true,
                size: fp(10),
                entry_price: fp(100),
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: MarginType::Cross,
            })
            .unwrap();
        assert_eq!(evm_block(&db, 1, vec![withdraw(ALICE, wei(amount), 0)]), vec![true]);
        let results = drain(&db, 2);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, ok, "amount {amount}: {:?}", results[0].1);
        assert_eq!(native(&db, &ALICE), fp(if ok { 150 - amount as i64 } else { 150 }));
    }
}
