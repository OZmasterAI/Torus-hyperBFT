//! Running state hash (docs/plans/running-state-hash-impl.md, Task 5):
//! `AttestStateHash { height, hash }` — validator-only, checkpoint heights only,
//! first vote per validator wins, stake-weighted > 2/3 records the quorum hash,
//! votes / quorum pruned to the last `STATE_HASH_CHECKPOINT_RETAIN` checkpoints,
//! and serial vs parallel engine land identical state.

use alloy_primitives::{Address, B256, U256};

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_economics::types::{ValidatorState, ValidatorStatus};
use torus_economics::StakingManager;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_STATE_HASH_VOTES};
use torus_state::running_hash::{STATE_HASH_CHECKPOINT_INTERVAL, STATE_HASH_CHECKPOINT_RETAIN};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

/// Three equal-stake active validators (1, 2, 3) — like the testnet — plus a
/// fourth with `extra` stake (0 = absent).
fn seed_validators(db: &StateDb, fourth_stake: u64) {
    let staking = StakingManager::new(db.clone());
    let put = |n: u8, stake: u64| {
        let v = ValidatorState {
            address: addr(n),
            pubkey: [n; 32],
            commission_bps: 500,
            self_stake: U256::from(stake),
            total_delegated: U256::ZERO,
            status: ValidatorStatus::Active,
            jailed_until: None,
            last_commission_change_block: None,
        };
        staking.put_validator(&addr(n), &v).unwrap();
    };
    for n in 1..=3 {
        put(n, 1_000);
    }
    if fourth_stake > 0 {
        put(4, fourth_stake);
    }
}

fn ctx_at(db: &StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(db.clone(), height, 1000 + height, 0, 100, 10, addr(99), addr(100), addr(101))
}

fn attest(height: u64, byte: u8) -> NativeAction {
    NativeAction::AttestStateHash {
        height,
        hash: B256::repeat_byte(byte),
    }
}

/// Execute `(sender, action)` pairs in one block at `block_height`; returns
/// per-action success.
fn run(db: &StateDb, block_height: u64, actions: Vec<(Address, NativeAction)>) -> Vec<bool> {
    let mut ctx = ctx_at(db, block_height);
    let r = NativeExecutor::execute_batch(&mut ctx, &actions);
    ctx.save_order_books();
    r.results.iter().map(|a| a.success).collect()
}

fn votes_dump(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.iterate_cf(CF_STATE_HASH_VOTES, None).unwrap()
}

fn staking(db: &StateDb) -> StakingManager {
    StakingManager::new(db.clone())
}

#[test]
fn attest_state_hash_rejects_non_validators_and_bad_heights() {
    let (_d, db) = open_test_db();
    seed_validators(&db, 0);
    // Non-validator, non-checkpoint height, future / current height, too old.
    let old = 200 + STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN;
    let ok = run(
        &db,
        250,
        vec![
            (addr(9), attest(200, 1)),
            (addr(1), attest(150, 1)),
            (addr(1), attest(300, 1)),
            (addr(1), attest(0, 1)),
        ],
    );
    assert_eq!(ok, vec![false, false, false, false]);
    assert!(votes_dump(&db).is_empty(), "rejected attestations write nothing");
    assert_eq!(run(&db, old, vec![(addr(1), attest(200, 1))]), vec![false], "older than the retained window");
    // A jailed / candidate validator is not active.
    let mut v = staking(&db).get_validator(&addr(3)).unwrap().unwrap();
    v.status = ValidatorStatus::Candidate;
    staking(&db).put_validator(&addr(3), &v).unwrap();
    assert_eq!(run(&db, 250, vec![(addr(3), attest(200, 1))]), vec![false]);
    assert!(votes_dump(&db).is_empty());
    // A valid one lands.
    assert_eq!(run(&db, 250, vec![(addr(1), attest(200, 1))]), vec![true]);
    assert_eq!(
        staking(&db).state_hash_votes(200).unwrap(),
        vec![(addr(1), [1u8; 32])]
    );
}

#[test]
fn attest_state_hash_first_vote_wins_and_quorum_needs_more_than_two_thirds() {
    let (_d, db) = open_test_db();
    seed_validators(&db, 0);
    // Validator 1 votes twice (same block and a later block): first wins.
    let ok = run(&db, 101, vec![(addr(1), attest(100, 0xaa)), (addr(1), attest(100, 0xbb))]);
    assert_eq!(ok, vec![true, false]);
    assert_eq!(run(&db, 102, vec![(addr(1), attest(100, 0xcc))]), vec![false]);
    // 2 of 3 equal stakes = exactly 2/3: NOT > 2/3, no quorum.
    assert_eq!(run(&db, 103, vec![(addr(2), attest(100, 0xaa))]), vec![true]);
    assert_eq!(staking(&db).state_hash_quorum(100).unwrap(), None);
    // Third vote agrees: quorum recorded.
    assert_eq!(run(&db, 104, vec![(addr(3), attest(100, 0xaa))]), vec![true]);
    assert_eq!(staking(&db).state_hash_quorum(100).unwrap(), Some([0xaa; 32]));
    let votes = staking(&db).state_hash_votes(100).unwrap();
    assert_eq!(votes.len(), 3);
    assert!(votes.iter().all(|(_, h)| *h == [0xaa; 32]), "first vote kept");

    // Split votes: no quorum (2 vs 1).
    run(
        &db,
        201,
        vec![(addr(1), attest(200, 1)), (addr(2), attest(200, 1)), (addr(3), attest(200, 2))],
    );
    assert_eq!(staking(&db).state_hash_votes(200).unwrap().len(), 3);
    assert_eq!(staking(&db).state_hash_quorum(200).unwrap(), None);
}

#[test]
fn attest_state_hash_quorum_is_stake_weighted() {
    let (_d, db) = open_test_db();
    seed_validators(&db, 10_000); // validator 4 holds 10k of 13k
    assert_eq!(run(&db, 101, vec![(addr(1), attest(100, 7))]), vec![true]);
    assert_eq!(staking(&db).state_hash_quorum(100).unwrap(), None);
    assert_eq!(run(&db, 102, vec![(addr(4), attest(100, 7))]), vec![true]);
    // 11k of 13k > 2/3.
    assert_eq!(staking(&db).state_hash_quorum(100).unwrap(), Some([7; 32]));
}

#[test]
fn attest_state_hash_prunes_to_retained_checkpoints() {
    let (_d, db) = open_test_db();
    seed_validators(&db, 0);
    let n = STATE_HASH_CHECKPOINT_RETAIN + 3;
    for i in 1..=n {
        let h = i * STATE_HASH_CHECKPOINT_INTERVAL;
        let ok = run(&db, h + 1, (1..=3).map(|v| (addr(v), attest(h, i as u8))).collect());
        assert_eq!(ok, vec![true, true, true], "checkpoint {h}");
    }
    let s = staking(&db);
    let first_kept = (n - STATE_HASH_CHECKPOINT_RETAIN + 1) * STATE_HASH_CHECKPOINT_INTERVAL;
    for i in 1..=n {
        let h = i * STATE_HASH_CHECKPOINT_INTERVAL;
        let kept = h >= first_kept;
        assert_eq!(s.state_hash_votes(h).unwrap().len(), if kept { 3 } else { 0 }, "votes at {h}");
        assert_eq!(s.state_hash_quorum(h).unwrap().is_some(), kept, "quorum at {h}");
    }
    // 64 checkpoints x (3 votes + 1 quorum).
    assert_eq!(votes_dump(&db).len() as u64, STATE_HASH_CHECKPOINT_RETAIN * 4);
}

#[test]
fn attest_state_hash_serial_vs_parallel_engine_identical() {
    let order = |is_buy: bool, price: i64| PlaceOrderParams {
        market_id: 1,
        is_buy,
        price: FixedPoint::from_raw(price as i128 * FixedPoint::SCALE),
        quantity: FixedPoint::from_raw(FixedPoint::SCALE),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let run_mode = |threads: usize| {
        let (_d, db) = open_test_db();
        seed_validators(&db, 0);
        let mut ctx = ctx_at(&db, 101);
        for n in 10..=12u8 {
            let bal = torus_core::position::NativeBalance {
                available: FixedPoint::from_raw(1_000_000 * FixedPoint::SCALE),
                order_margin: FixedPoint::ZERO,
            };
            ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
        }
        let batch = vec![
            (addr(10), NativeAction::PlaceOrder(order(false, 100))),
            (addr(1), attest(100, 9)),
            (addr(11), NativeAction::PlaceOrder(order(true, 100))),
            (addr(2), attest(100, 9)),
            (addr(12), NativeAction::PlaceOrder(order(true, 99))),
            (addr(3), attest(100, 9)),
        ];
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &batch, threads);
        ctx.save_order_books();
        let results: Vec<bool> = r.results.iter().map(|a| a.success).collect();
        (results, votes_dump(&db), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap())
    };
    let serial = run_mode(0);
    assert!(serial.0.iter().all(|ok| *ok), "{:?}", serial.0);
    assert_eq!(serial.1.len(), 4, "3 votes + quorum");
    for threads in [2, 4] {
        assert_eq!(run_mode(threads), serial, "threads={threads}");
    }
}
