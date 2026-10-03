//! s87 Fix 1 (`docs/plans/crab-perf-fixes-s87.md`): one maker account
//! snapshot per `execute_batch` call. A maker filling in K markets of one
//! batch used to load its whole account (every position + a mark per
//! position) once per market; its start-of-batch free margin is the same in
//! every market (nothing writes the backend during matching), so it is now
//! computed once and shared, and each market's mark is read once per batch.

#[path = "common/counting_backend.rs"]
mod counting_backend;

use alloy_primitives::Address;
use counting_backend::CountingBackend;
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{MarginType, NativeBalance};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// A listed market row with initial margin 5% (20x config).
fn market_row() -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), fp(5).raw())).unwrap()
}

fn limit(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

const MARKETS: u64 = 20;
const BOOKS: u64 = 5;

/// Maker M (addr 1) holds a long in each of 20 marked markets and rests a bid
/// in markets 1-5; takers addr(11..=15) are funded. Returns the counting
/// context (block 2, marks aggregated at block 1).
fn fixture() -> (tempfile::TempDir, CountingBackend<StateDb>, NativeExecContext<CountingBackend<StateDb>>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    for m in 1..=MARKETS {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row()).unwrap();
    }
    let state = CountingBackend::new(db);
    let mut ctx = NativeExecContext::new(state.clone(), 2, 1_002, 0, 1_000, 10, addr(99), addr(100), addr(101));
    assert_eq!(ctx.margin_configs.len(), MARKETS as usize);
    let (maker, other) = (addr(1), addr(2));
    let fund = |t: &Address, a: i64| {
        ctx.positions
            .put_native_balance(t, &NativeBalance { available: fp(a), order_margin: FixedPoint::ZERO })
            .unwrap();
    };
    fund(&maker, 10_000_000);
    fund(&other, 10_000_000);
    for t in 11..=15 {
        fund(&addr(t), 10_000_000);
    }
    let reporters = [addr(150), addr(151), addr(152)];
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    for m in 1..=MARKETS {
        ctx.positions.apply_fill(&maker, m, true, fp(2), fp(1_000), MarginType::Cross).unwrap();
        ctx.positions.apply_fill(&other, m, false, fp(2), fp(1_000), MarginType::Cross).unwrap();
        for v in &reporters {
            ctx.oracle.submit_price(v, m, fp(1_000), 1, 1_001).unwrap();
        }
        ctx.oracle.aggregate_price(m, 1, 1_001, &stakes).unwrap();
    }
    for m in 1..=BOOKS {
        let r = NativeExecutor::execute(&mut ctx, &maker, &NativeAction::PlaceOrder(limit(m, true, 1_000, 10)));
        assert!(r.success, "{:?}", r.error);
    }
    (dir, state, ctx)
}

/// Fix 1 RED: five takers each sell into one of M's five bids in ONE batch:
/// M's account is loaded once (one positions scan) and every market's mark
/// is read at most once. c93c579: 5 scans and 100+ mark reads (20 per view).
#[test]
fn a_maker_filling_in_five_markets_is_valued_once_per_batch() {
    for threads in [0usize, 4] {
        let (_d, state, mut ctx) = fixture();
        let actions: Vec<(Address, NativeAction)> = (1..=BOOKS)
            .map(|m| (addr(10 + m as u8), NativeAction::PlaceOrder(limit(m, false, 1_000, 1))))
            .collect();
        state.arm();
        let r = if threads == 0 {
            NativeExecutor::execute_batch(&mut ctx, &actions)
        } else {
            NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, threads)
        };
        state.disarm();
        assert!(r.results.iter().all(|x| x.success), "{:?}", r.results);
        println!(
            "threads {threads}: M positions scans {}, mark reads {}",
            state.position_scans(addr(1).as_slice()),
            state.oracle_reads()
        );
        assert_eq!(ctx.trade_index, BOOKS as u32, "every taker filled against M");
        assert_eq!(state.position_scans(addr(1).as_slice()), 1, "threads {threads}: M's positions scans");
        assert!(
            state.oracle_reads() <= MARKETS as usize,
            "threads {threads}: {} mark reads for {MARKETS} markets",
            state.oracle_reads()
        );
    }
}
