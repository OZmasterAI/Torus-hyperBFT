//! O3 micro-bench: execute_batch under a fills-heavy block over the
//! prod-shaped NativeStateOverlay backend — 200 resting sells crossed by 200
//! buys (200 fills = 600 trade-history KVs per block). A/B the per-fill
//! trade-persist cost: inline overlay PUTs (pre-O3) vs deferred buffering
//! (`ctx.defer_trades`, the live path with a background writer). The deferred
//! variant times `take_pending_trades` too — the handoff the exec thread pays;
//! the RocksDB write itself is off-thread and intentionally not measured.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{NativeBalance, PositionManager};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{Address, FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const NUM_SENDERS: usize = 100;
const FILLS_PER_BLOCK: usize = 200;
const MARKET: u64 = 1;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn sender(i: usize) -> Address {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&(i as u64 + 1).to_be_bytes());
    Address::new(b)
}

fn order(i: usize, is_buy: bool, price: i64) -> (Address, NativeAction) {
    // Disjoint pools — sellers 0..50, buyers 50..100 — so no buy ever
    // self-matches its sender's own resting sell (a suppressed fill).
    let s = if is_buy {
        NUM_SENDERS / 2 + (i % (NUM_SENDERS / 2))
    } else {
        i % (NUM_SENDERS / 2)
    };
    (
        sender(s),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: MARKET,
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

/// Resting sells at 100, then buys at 100 that cross them 1:1.
fn resting_sells() -> Vec<(Address, NativeAction)> {
    (0..FILLS_PER_BLOCK).map(|i| order(i, false, 100)).collect()
}

fn crossing_buys() -> Vec<(Address, NativeAction)> {
    (0..FILLS_PER_BLOCK)
        .map(|i| order(i + FILLS_PER_BLOCK, true, 100))
        .collect()
}

fn bench_trades(c: &mut Criterion, name: &str, defer: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");

    // Fund senders once, directly in RocksDB (overlay writes are never
    // flushed, so the DB stays clean across iterations).
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

    let sells = resting_sells();
    let buys = crossing_buys();

    c.bench_function(name, |b| {
        b.iter_batched(
            || {
                // Per-block shape: fresh overlay + ctx, resting book seeded
                // untimed so the timed batch is pure crossing fills.
                let overlay = NativeStateOverlay::new(db.clone());
                let mut ctx = NativeExecContext::new(
                    overlay,
                    1,         // block_height
                    1_000_000, // timestamp
                    0,         // epoch
                    1000,      // epoch_length
                    100,       // max_validators
                    sender(0),
                    sender(1),
                    sender(2),
                );
                NativeExecutor::execute_batch(&mut ctx, &sells);
                ctx.defer_trades = defer;
                ctx
            },
            |mut ctx| {
                let result = NativeExecutor::execute_batch(&mut ctx, &buys);
                assert!(result.results.iter().all(|r| r.success));
                // The exec thread's full O3 cost includes the buffer handoff.
                let pending = ctx.take_pending_trades();
                if defer {
                    assert_eq!(pending.len(), FILLS_PER_BLOCK * 3);
                } else {
                    assert!(pending.is_empty());
                }
                (ctx, pending)
            },
            BatchSize::LargeInput,
        );
    });
}

fn bench_inline(c: &mut Criterion) {
    bench_trades(c, "execute_batch/200fills_trades_inline", false);
}

fn bench_deferred(c: &mut Criterion) {
    bench_trades(c, "execute_batch/200fills_trades_deferred", true);
}

criterion_group!(benches, bench_inline, bench_deferred);
criterion_main!(benches);
