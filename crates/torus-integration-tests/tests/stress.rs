//! Stress tests (task 2.10.7).
//!
//! High-throughput tests verifying correctness invariants under sustained load.
//! All tests in this file are marked `#[ignore]` — run with:
//!   cargo test -p torus-integration-tests -- --ignored

mod common;

use std::time::Instant;

use alloy_primitives::Address;
use torus_bridge::native_executor::NativeExecutor;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

fn fp(n: i64) -> FixedPoint {
    TestHarness::fp(n)
}

fn addr(n: u8) -> Address {
    TestHarness::addr(n)
}

/// Generate a deterministic order based on a sequential seed.
/// Distribution: 40% limit buy, 40% limit sell, 10% market, 10% cancel.
fn generate_action(seed: usize, market_id: u64) -> NativeAction {
    let kind = seed % 10;
    match kind {
        // 40% limit buy — prices spread across 49000..49990
        0..=3 => {
            let price_offset = ((seed / 10) % 100) as i64;
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id,
                is_buy: true,
                price: fp(49000 + price_offset * 10),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            })
        }
        // 40% limit sell — prices spread across 50000..50990
        4..=7 => {
            let price_offset = ((seed / 10) % 100) as i64;
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id,
                is_buy: false,
                price: fp(50000 + price_offset * 10),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            })
        }
        // 10% market order (alternating buy/sell)
        8 => {
            let is_buy = (seed / 100) % 2 == 0;
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id,
                is_buy,
                price: if is_buy { fp(999_999) } else { fp(1) },
                quantity: fp(1),
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                reduce_only: false,
                client_order_id: None,
            })
        }
        // 10% cancel (may fail gracefully if order doesn't exist)
        _ => {
            let order_id = ((seed / 10) % 500) as u128 + 1;
            NativeAction::CancelOrder { order_id }
        }
    }
}

/// Verify order book invariants for all books in the context.
fn verify_all_book_invariants(ctx: &torus_bridge::native_executor::NativeExecContext) {
    for (_market_id, book) in &ctx.order_books {
        book.verify_invariants();
    }
}

/// Check value conservation: sum(available) + sum(unrealized PnL) ≈ sum(initial funding).
///
/// Allows a small epsilon for FixedPoint rounding from repeated multiply/divide
/// in volume-weighted average entry price calculations.
fn verify_value_conservation(
    h: &TestHarness,
    traders: &[Address],
    mark_price: FixedPoint,
    total_initial: FixedPoint,
) {
    let mut total_available = FixedPoint::ZERO;
    let mut total_unrealized = FixedPoint::ZERO;

    for trader in traders {
        let bal = h.positions.get_native_balance(trader).unwrap();
        total_available = total_available + bal.available;

        let positions = h.positions.positions_for_trader(trader).unwrap();
        for pos in &positions {
            total_unrealized = total_unrealized + pos.unrealized_pnl(mark_price);
        }
    }

    let actual = total_available + total_unrealized;
    let diff_raw = (actual - total_initial).raw();
    let abs_diff = diff_raw.unsigned_abs();
    // Allow up to 0.00001 (1000 raw units) of rounding drift from
    // repeated FixedPoint multiply/divide in entry price averaging.
    assert!(
        abs_diff < 1000,
        "Value conservation violated beyond epsilon: available({total_available}) + unrealized({total_unrealized}) = {actual}, expected {total_initial}, diff_raw={diff_raw}"
    );
}

// ============================================================================
// Tests
// ============================================================================

/// Sustained throughput: 1 market, 100 traders, 10,000 orders in 50 blocks.
/// Verifies no panics, book invariants, and value conservation.
#[test]
#[ignore]
fn test_sustained_throughput() {
    let h = TestHarness::new();
    let market = 1u64;
    let num_traders = 100usize;
    let orders_per_block = 200usize;
    let num_blocks = 50usize;
    let funding_per_trader = fp(1_000_000);

    // Create and fund 100 traders (seeds 1..=100).
    let traders: Vec<Address> = (1..=num_traders).map(|i| addr(i as u8)).collect();
    for t in &traders {
        h.fund_native(t, funding_per_trader);
    }
    let total_initial = fp(num_traders as i64 * 1_000_000);

    let mut ctx = h.exec_context(1);
    let mut block_times = Vec::with_capacity(num_blocks);
    let total_start = Instant::now();

    // Execute 50 blocks of 200 actions each.
    for block in 0..num_blocks {
        let actions: Vec<(Address, NativeAction)> = (0..orders_per_block)
            .map(|i| {
                let seed = block * orders_per_block + i;
                let trader = traders[seed % num_traders];
                let action = generate_action(seed, market);
                (trader, action)
            })
            .collect();

        let block_start = Instant::now();
        let batch = NativeExecutor::execute_batch(&mut ctx, &actions);
        let block_elapsed = block_start.elapsed();
        block_times.push(block_elapsed);

        assert_eq!(batch.results.len(), orders_per_block);

        ctx.block_height += 1;
    }

    let total_elapsed = total_start.elapsed();

    // Verify book invariants.
    verify_all_book_invariants(&ctx);

    // Verify value conservation.
    let mark_price = fp(50000);
    verify_value_conservation(&h, &traders, mark_price, total_initial);

    // Print performance metrics (no timing assertions).
    let total_orders = num_blocks * orders_per_block;
    let avg_block_ms = block_times.iter().map(|d| d.as_millis()).sum::<u128>() / num_blocks as u128;
    let orders_per_sec = total_orders as f64 / total_elapsed.as_secs_f64();
    println!(
        "Sustained throughput: {total_orders} orders in {:.2?} | avg block {avg_block_ms}ms | {orders_per_sec:.0} orders/sec",
        total_elapsed
    );
}

/// Multi-market: 5 markets, 50 traders, 1000 orders per market.
/// Verifies per-market invariants independently and no cross-market contamination.
#[test]
#[ignore]
fn test_multi_market_stress() {
    let h = TestHarness::new();
    let num_markets = 5u64;
    let num_traders = 50usize;
    let orders_per_market = 1000usize;
    let funding_per_trader = fp(10_000_000);

    let traders: Vec<Address> = (1..=num_traders).map(|i| addr(i as u8)).collect();
    for t in &traders {
        h.fund_native(t, funding_per_trader);
    }
    let total_initial = fp(num_traders as i64 * 10_000_000);

    let mut ctx = h.exec_context(1);

    // Execute orders for each market.
    for market_id in 1..=num_markets {
        let actions: Vec<(Address, NativeAction)> = (0..orders_per_market)
            .map(|i| {
                let trader = traders[i % num_traders];
                let action = generate_action(i + (market_id as usize) * 10000, market_id);
                (trader, action)
            })
            .collect();

        let batch = NativeExecutor::execute_batch(&mut ctx, &actions);
        assert_eq!(batch.results.len(), orders_per_market);
    }

    // Verify per-market invariants independently.
    for market_id in 1..=num_markets {
        if let Some(book) = ctx.order_books.get(&market_id) {
            book.verify_invariants();
        }
    }

    // Verify value conservation across all markets.
    let mark_price = fp(50000);
    verify_value_conservation(&h, &traders, mark_price, total_initial);

    // Verify no cross-market contamination: positions reference valid markets.
    for t in &traders {
        let positions = h.positions.positions_for_trader(t).unwrap();
        for pos in &positions {
            assert!(
                pos.market_id >= 1 && pos.market_id <= num_markets,
                "Position references unknown market {}",
                pos.market_id
            );
        }
    }
}

/// Position accumulation: 2 traders, 500 back-and-forth trades.
/// Verifies entry price has no drift and realized PnL is exact.
#[test]
#[ignore]
fn test_position_accumulation_precision() {
    let h = TestHarness::new();
    let market = 1u64;
    let trader_a = addr(1);
    let trader_b = addr(2);
    let initial = fp(100_000_000);

    h.fund_native(&trader_a, initial);
    h.fund_native(&trader_b, initial);

    let mut ctx = h.exec_context(1);

    let buy_price = fp(50000);
    let qty = fp(1);

    // Phase 1: 250 small buys from A, matched by sells from B.
    // All at price 50000, qty 1. A accumulates a long position.
    for _ in 0..250 {
        let buy = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: true,
            price: buy_price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let sell = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: false,
            price: buy_price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let batch = NativeExecutor::execute_batch(&mut ctx, &[(trader_a, buy), (trader_b, sell)]);
        assert!(batch.results[0].success);
        assert!(batch.results[1].success);
    }

    // Verify A has a long position with entry = 50000 (no drift from accumulation).
    let pos_a = h
        .positions
        .get_position(&trader_a, market)
        .unwrap()
        .expect("trader_a should have a position");
    assert!(pos_a.is_long);
    assert_eq!(pos_a.size, fp(250));
    assert_eq!(pos_a.entry_price, buy_price, "Entry price should not drift");

    // Phase 2: Close 125 units at a higher price (50100).
    let close_price = fp(50100);
    for _ in 0..125 {
        let sell = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: false,
            price: close_price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let buy = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: true,
            price: close_price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let batch = NativeExecutor::execute_batch(&mut ctx, &[(trader_a, sell), (trader_b, buy)]);
        assert!(batch.results[0].success);
        assert!(batch.results[1].success);
    }

    // Verify partial close: A has 125 remaining, entry unchanged.
    let pos_a = h
        .positions
        .get_position(&trader_a, market)
        .unwrap()
        .expect("trader_a position after partial close");
    assert_eq!(pos_a.size, fp(125));
    assert_eq!(
        pos_a.entry_price, buy_price,
        "Entry should not change on partial close"
    );

    // Realized PnL for A: (50100 - 50000) * 125 = 12,500
    let expected_pnl = fp(12500);
    let bal_a = h.positions.get_native_balance(&trader_a).unwrap();
    assert_eq!(
        bal_a.available,
        initial + expected_pnl,
        "Realized PnL should be exact"
    );

    // Phase 3: Close remaining 125 units at the same close price.
    for _ in 0..125 {
        let sell = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: false,
            price: close_price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let buy = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: true,
            price: close_price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let batch = NativeExecutor::execute_batch(&mut ctx, &[(trader_a, sell), (trader_b, buy)]);
        assert!(batch.results[0].success);
        assert!(batch.results[1].success);
    }

    // A should have no position now.
    assert!(
        h.positions
            .get_position(&trader_a, market)
            .unwrap()
            .is_none(),
        "Position should be fully closed"
    );

    // Final balance: initial + total PnL = initial + 100 * 250 = initial + 25,000
    let total_pnl = fp(25000);
    let final_bal_a = h.positions.get_native_balance(&trader_a).unwrap();
    assert_eq!(
        final_bal_a.available,
        initial + total_pnl,
        "Final balance should equal initial + net PnL"
    );

    // Conservation: A's gain = B's loss.
    let final_bal_b = h.positions.get_native_balance(&trader_b).unwrap();
    // B has no remaining position (closed all shorts).
    // B's total realized PnL = -(A's PnL) = -25,000
    assert_eq!(
        final_bal_b.available,
        initial - total_pnl,
        "B's loss should equal A's gain"
    );

    // Value conservation: A + B = 2 * initial.
    assert_eq!(
        final_bal_a.available + final_bal_b.available,
        initial + initial,
        "Total value should be conserved"
    );
}

/// Deep book sweep: 10,000 resting orders across 1,000 price levels,
/// then one large market order sweeping 100+ levels.
#[test]
#[ignore]
fn test_deep_book_sweep() {
    let h = TestHarness::new();
    let market = 1u64;
    let maker = addr(1);
    let taker = addr(2);
    let orders_per_level = 10usize;
    let num_levels = 1000usize;

    h.fund_native(&maker, fp(1_000_000_000));
    h.fund_native(&taker, fp(1_000_000_000));

    let mut ctx = h.exec_context(1);

    // Place 10,000 resting BUY orders: 10 per level, prices 1000..1999.
    for level in 0..num_levels {
        let price = fp(1000 + level as i64);
        for _ in 0..orders_per_level {
            let action = NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: market,
                is_buy: true,
                price,
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            });
            let r = NativeExecutor::execute(&mut ctx, &maker, &action);
            assert!(r.success, "Resting order should succeed");
        }
    }

    let book = ctx.order_books.get(&market).unwrap();
    assert_eq!(
        book.order_count(),
        num_levels * orders_per_level,
        "All 10,000 orders should be resting"
    );
    assert_eq!(book.bid_levels(), num_levels);

    // Sweep with a large SELL market order for 1,000 units.
    // Should sweep the top 100 levels (1999 down to 1900), 10 orders each.
    let sweep_qty = fp(1000);
    let sweep_action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: market,
        is_buy: false,
        price: fp(1),
        quantity: sweep_qty,
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    });

    let sweep_start = Instant::now();
    let r = NativeExecutor::execute(&mut ctx, &taker, &sweep_action);
    let sweep_elapsed = sweep_start.elapsed();
    assert!(r.success, "Sweep order should succeed");

    // After sweep: 9,000 orders should remain (1,000 filled).
    let book = ctx.order_books.get(&market).unwrap();
    assert_eq!(
        book.order_count(),
        (num_levels - 100) * orders_per_level,
        "9,000 orders should remain"
    );
    assert_eq!(
        book.bid_levels(),
        num_levels - 100,
        "900 bid levels should remain"
    );

    // Best bid should now be 1899 (levels 1900..1999 swept).
    assert_eq!(book.best_bid(), Some(fp(1899)));

    // Book invariants still hold.
    book.verify_invariants();

    // Taker should have a short position of size 1000.
    let pos = h
        .positions
        .get_position(&taker, market)
        .unwrap()
        .expect("taker should have position");
    assert!(!pos.is_long, "taker sold = short");
    assert_eq!(pos.size, sweep_qty);

    // Volume-weighted average entry price ≈ mean(1900..=1999) = 1949.5
    // Allow small rounding drift from repeated FixedPoint division.
    let expected_entry = TestHarness::fp_dec(19495, 1); // 1949.5
    let entry_diff = (pos.entry_price - expected_entry).raw().unsigned_abs();
    assert!(
        entry_diff < 1000,
        "Entry price too far from expected: got {} expected {expected_entry}, diff_raw={}",
        pos.entry_price,
        pos.entry_price.raw() - expected_entry.raw()
    );

    println!("Deep book sweep: 1,000 fills across 100 levels in {sweep_elapsed:.2?}");
}
