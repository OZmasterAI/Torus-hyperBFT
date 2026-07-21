//! Proves the SHIPPED genesis (market 1 + funded hardhat market-makers) makes the
//! throughput bench's orders actually match — the end-to-end seam between
//! `torus-genesis` seeding and `NativeExecutor` matching.
//!
//! Regression guard for S329: before this, every `bench-throughput consensus`
//! PlaceOrder was a no-op — rejected for insufficient margin (senders had no native
//! balance) and, once funded, rejected for tick-misalignment (prices weren't whole
//! units). This test loads the REAL testnet genesis, funds through it, and confirms
//! two crossing tick-aligned orders actually fill.

mod common;

use std::path::PathBuf;

use alloy_primitives::Address;
use torus_bridge::native_executor::NativeExecutor;
use torus_genesis::Genesis;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

use crate::common::TestHarness;

fn limit(market_id: u64, is_buy: bool, price: FixedPoint, qty: FixedPoint) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// Two genesis-funded hardhat market-makers cross on market 1 with tick-aligned
/// (whole-unit) prices — exactly what the fixed `bench-throughput consensus` emits —
/// and the orders MATCH (positions are opened from the fill, not left merely resting).
#[test]
fn shipped_genesis_makes_bench_orders_match() {
    let h = TestHarness::new();

    // Seed from the REAL shipped testnet genesis BASE: market 1 + funded hardhat
    // balances. This is the tracked source that gen-weighted-genesis.sh expands into
    // genesis-weighted-full.json (gitignored) — the bulk bench accounts it adds are
    // irrelevant here, so the base is the right fixture. The former testnet/genesis.json
    // was a second lean base and was deleted; two bases produce different state roots.
    let genesis_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testnet/genesis-weighted-base.json");
    let genesis = Genesis::from_file(&genesis_path)
        .unwrap_or_else(|e| panic!("load {}: {e}", genesis_path.display()));
    genesis
        .initialize(&h.state_db)
        .expect("seed genesis into state db");

    // hardhat #0 and #1 — both pre-funded with a native balance by genesis (no deposit).
    let maker: Address = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        .parse()
        .unwrap();
    let taker: Address = "0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
        .parse()
        .unwrap();
    let market = 1u64;

    // Precondition: genesis actually funded these accounts' native (perp) balance —
    // this is the exact value exec_place_order's margin check reads.
    assert!(
        h.positions.get_native_balance(&maker).unwrap().available > FixedPoint::ZERO,
        "genesis must fund the maker's native balance"
    );

    let mut ctx = h.exec_context(1);

    // Tick-aligned whole-unit price (the bench fix): price.raw() % tick(1.0).raw() == 0.
    let price = FixedPoint::from_raw(60_000 * FixedPoint::SCALE);
    let qty = FixedPoint::from_raw(5 * FixedPoint::SCALE);

    // execute_batch is the exact path block execution drives the bench's orders through.
    let batch = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            (maker, limit(market, true, price, qty)),
            (taker, limit(market, false, price, qty)),
        ],
    );
    assert!(
        batch.results.iter().all(|r| r.success),
        "orders must not be rejected (margin/tick): {:?}",
        batch.results
    );

    // The crossing orders MATCHED: each market-maker now holds the opposite 5-unit
    // position. A position only exists if a fill occurred (a merely-resting order
    // creates none), so this is proof of matching — not just acceptance.
    let pos_maker = h
        .positions
        .get_position(&maker, market)
        .unwrap()
        .expect("maker position from fill — proves the order matched, not just rested");
    assert!(pos_maker.is_long);
    assert_eq!(pos_maker.size, qty);
    assert_eq!(pos_maker.entry_price, price);

    let pos_taker = h
        .positions
        .get_position(&taker, market)
        .unwrap()
        .expect("taker position from fill");
    assert!(!pos_taker.is_long);
    assert_eq!(pos_taker.size, qty);
    assert_eq!(pos_taker.entry_price, price);
}
