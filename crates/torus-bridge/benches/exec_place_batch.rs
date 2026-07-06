//! O2 micro-bench (G6): the SAME 400 orders as 400 signed single PlaceOrder
//! actions vs ONE signed PlaceOrderBatch. Timed path = the exec pipeline's
//! Phase-3 verify (`batch_verify_native_actions`: 400 ecrecovers vs 1) plus
//! `execute_batch` (flatten + margin + match + settle) over a prod-shaped
//! NativeStateOverlay. Non-crossing resting buys spread over 4 markets
//! (100/market, under MAX_ORDERS_PER_TRADER_PER_MARKET=200) isolate the
//! placement path — no fills, so the delta is pure per-action overhead.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{NativeBalance, PositionManager};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::eip712::{batch_verify_native_actions, sign_native_action};
use torus_types::{
    Address, FixedPoint, NativeAction, OrderType, PlaceOrderParams, SignedNativeAction, TimeInForce,
};

const N: usize = 400;
const MARKETS: u64 = 4;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn mm_key() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[0x42u8; 32]).unwrap()
}

/// Non-crossing resting buy i (deterministic; identical across variants).
fn order(i: usize) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1 + (i as u64 % MARKETS),
        is_buy: true,
        price: fp(50 + (i as i64 % 40)),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn signed_singles() -> Vec<SignedNativeAction> {
    let key = mm_key();
    (0..N)
        .map(|i| {
            sign_native_action(
                NativeAction::PlaceOrder(order(i)),
                1_000_000 + i as u64,
                &key,
            )
        })
        .collect()
}

fn signed_batch() -> Vec<SignedNativeAction> {
    let key = mm_key();
    vec![sign_native_action(
        NativeAction::PlaceOrderBatch((0..N).map(order).collect()),
        2_000_000,
        &key,
    )]
}

fn bench_variant(c: &mut Criterion, name: &str, actions: Vec<SignedNativeAction>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let sender = actions[0].recover_sender().expect("recover");
    // Fund once, directly in RocksDB (overlay writes never flush).
    PositionManager::new(db.clone())
        .put_native_balance(
            &sender,
            &NativeBalance {
                available: fp(100_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .expect("fund");

    // Equivalence gate: both variants carry EXACTLY the same N orders.
    let total_orders: usize = actions
        .iter()
        .map(|a| match &a.action {
            NativeAction::PlaceOrderBatch(o) => o.len(),
            _ => 1,
        })
        .sum();
    assert_eq!(total_orders, N);

    // Equivalence gate, part 2 (ONE-TIME, outside the timed loop — moved out of
    // the criterion closure so per-sample asserts don't pollute the measured
    // singles-vs-batch delta): executing the actions flattens to exactly N
    // results. Overlay writes never flush, so this dry run leaves the db clean.
    {
        let overlay = NativeStateOverlay::new(db.clone());
        let mut ctx =
            NativeExecContext::new(overlay, 1, 1_000_000, 0, 1000, 100, sender, sender, sender);
        let senders = batch_verify_native_actions(&actions, 1_000_000, |_| None);
        let pairs: Vec<(Address, NativeAction)> = actions
            .iter()
            .zip(&senders)
            .map(|(a, s)| (s.expect("valid sig"), a.action.clone()))
            .collect();
        let result = NativeExecutor::execute_batch(&mut ctx, &pairs);
        assert_eq!(result.results.len(), N, "same flattened work both variants");
    }

    c.bench_function(name, |b| {
        b.iter_batched(
            || {
                let overlay = NativeStateOverlay::new(db.clone());
                NativeExecContext::new(overlay, 1, 1_000_000, 0, 1000, 100, sender, sender, sender)
            },
            |mut ctx| {
                // Phase-3 analogue: one verify pass — N ecrecovers vs 1.
                let senders = batch_verify_native_actions(&actions, 1_000_000, |_| None);
                let pairs: Vec<(Address, NativeAction)> = actions
                    .iter()
                    .zip(&senders)
                    .map(|(a, s)| (s.expect("valid sig"), a.action.clone()))
                    .collect();
                // black_box: result equivalence is asserted once above; here we
                // only pin the value so the optimizer can't elide the work.
                black_box(NativeExecutor::execute_batch(&mut ctx, &pairs));
                ctx
            },
            BatchSize::LargeInput,
        );
    });
}

fn bench_singles(c: &mut Criterion) {
    bench_variant(c, "exec_place_batch/singles_400", signed_singles());
}

fn bench_batch(c: &mut Criterion) {
    bench_variant(c, "exec_place_batch/batch_400", signed_batch());
}

criterion_group!(benches, bench_singles, bench_batch);
criterion_main!(benches);
