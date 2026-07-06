//! O1 micro-bench: execute_batch margin-phase cost over the prod-shaped
//! NativeStateOverlay backend. 400 non-crossing resting limit orders from
//! 100 senders in one market — mirrors the bs400 native-order-flood block
//! shape where Phase 2 (margin reserve) dominates and Phase 4 releases
//! nothing. A/B this before/after the balance cache.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{NativeBalance, PositionManager};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{Address, FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const NUM_SENDERS: usize = 100;
const ORDERS_PER_BLOCK: usize = 400;
const MARKET: u64 = 1;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn sender(i: usize) -> Address {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&(i as u64 + 1).to_be_bytes());
    Address::new(b)
}

/// Non-crossing resting book: buys 80..85, sells 120..125.
fn build_actions() -> Vec<(Address, NativeAction)> {
    (0..ORDERS_PER_BLOCK)
        .map(|i| {
            let is_buy = i % 2 == 0;
            let price = if is_buy {
                fp(80 + (i as i64 % 6))
            } else {
                fp(120 + (i as i64 % 6))
            };
            (
                sender(i % NUM_SENDERS),
                NativeAction::PlaceOrder(PlaceOrderParams {
                    market_id: MARKET,
                    is_buy,
                    price,
                    quantity: fp(1),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                }),
            )
        })
        .collect()
}

fn bench_execute_batch(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");

    // Fund senders once, directly in RocksDB (first touch per block still
    // falls through the overlay to the DB, as in prod).
    let funder = PositionManager::new(db.clone());
    for i in 0..NUM_SENDERS {
        funder
            .put_native_balance(
                &sender(i),
                &NativeBalance {
                    available: fp(1_000_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .expect("fund");
    }

    let actions = build_actions();

    c.bench_function("execute_batch/400orders_100senders_resting", |b| {
        b.iter_batched(
            || {
                // Per-block shape: fresh overlay + fresh ctx (books empty —
                // overlay writes are never flushed, so the DB stays clean).
                let overlay = NativeStateOverlay::new(db.clone());
                NativeExecContext::new(
                    overlay,
                    1,         // block_height
                    1_000_000, // timestamp
                    0,         // epoch
                    1000,      // epoch_length
                    100,       // max_validators
                    Address::new([0xAA; 20]),
                    Address::new([0xBB; 20]),
                    Address::new([0xCC; 20]),
                )
            },
            |mut ctx| {
                let out = NativeExecutor::execute_batch(&mut ctx, &actions);
                criterion::black_box(out)
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_execute_batch);
criterion_main!(benches);
