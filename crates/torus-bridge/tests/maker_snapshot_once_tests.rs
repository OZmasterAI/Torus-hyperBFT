//! s87 Fix 1 (`docs/plans/crab-perf-fixes-s87.md`): one maker account
//! snapshot per `execute_batch` call. A maker filling in K markets of one
//! batch used to load its whole account (every position + a mark per
//! position) once per market; its start-of-batch free margin is the same in
//! every market (nothing writes the backend during matching), so it is now
//! computed once and shared, and each market's mark is read once per batch.
//! Item 6 C2: the marks come from the block's mark table (read once per
//! block at the end of `begin_block_oracle`), so the batch reads none.
//! Item 6 C3: the per-batch snapshot is gone; the maker's position sums come
//! from the block's sums memo (node path: R attached through
//! `begin_resident`, the slot's sums through `attach_resident_block`), so the
//! account is still valued once.

#[path = "common/counting_backend.rs"]
mod counting_backend;

use alloy_primitives::Address;
use counting_backend::CountingBackend;
use torus_bridge::native_executor::{begin_resident, NativeExecContext, NativeExecutor, ResidentBlock, ResidentBooks};
use torus_core::position::{MarginType, NativeBalance};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::{NativeStateOverlay, StateDb};
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

/// The counting backend over block 2's overlay (R attached), its context
/// and the block's resident handle.
type Fixture = (
    tempfile::TempDir,
    CountingBackend<NativeStateOverlay>,
    NativeExecContext<CountingBackend<NativeStateOverlay>>,
    ResidentBooks,
    ResidentBlock,
);

/// Maker M (addr 1) holds a long in each of 20 marked markets and rests a
/// bid in markets 1-5 (block 1, in the DB); takers addr(11..=15) are funded.
/// Returns the counting context of block 2 (R and the slot's sums attached,
/// as the node does; the block's oracle step run: no Active validator, so it
/// only fills the mark table).
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    for m in 1..=MARKETS {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row()).unwrap();
    }
    let mut setup = NativeExecContext::new(db.clone(), 1, 1_001, 0, 1_000, 10, addr(99), addr(100), addr(101));
    let (maker, other) = (addr(1), addr(2));
    let fund = |t: &Address, a: i64| {
        setup.positions
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
        setup.positions.apply_fill(&maker, m, true, fp(2), fp(1_000), MarginType::Cross).unwrap();
        setup.positions.apply_fill(&other, m, false, fp(2), fp(1_000), MarginType::Cross).unwrap();
        for v in &reporters {
            setup.oracle.submit_price(v, m, fp(1_000), 1, 1_001).unwrap();
        }
        setup.oracle.aggregate_price(m, 1, 1_001, &stakes).unwrap();
    }
    for m in 1..=BOOKS {
        let r = NativeExecutor::execute(&mut setup, &maker, &NativeAction::PlaceOrder(limit(m, true, 1_000, 10)));
        assert!(r.success, "{:?}", r.error);
    }
    assert!(setup.fatal_error.is_none(), "{:?}", setup.fatal_error);
    let books = std::mem::take(&mut setup.order_books);
    let next_id = setup.next_global_order_id;
    drop(setup);
    let mut holder = ResidentBooks::default();
    let mut overlay = NativeStateOverlay::new(db);
    let mut block = begin_resident(Some(&mut holder), &mut overlay, 2, None);
    assert!(block.attached());
    let state = CountingBackend::new(overlay);
    let mut ctx = NativeExecContext::new(state.clone(), 2, 1_002, 0, 1_000, 10, addr(99), addr(100), addr(101));
    assert_eq!(ctx.margin_configs.len(), MARKETS as usize);
    ctx.order_books = books;
    ctx.next_global_order_id = next_id;
    ctx.attach_resident_block(&mut block);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    (dir, state, ctx, holder, block)
}

/// Fix 1 RED: five takers each sell into one of M's five bids in ONE batch:
/// M's account is loaded once (one positions scan) and every market's mark
/// is read at most once (C2: never — the block's table answers).
/// c93c579: 5 scans and 100+ mark reads (20 per view).
#[test]
fn a_maker_filling_in_five_markets_is_valued_once_per_batch() {
    for threads in [0usize, 4] {
        let (_d, state, mut ctx, _holder, _block) = fixture();
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
        assert_eq!(state.oracle_reads(), 0, "threads {threads}: mark reads for {MARKETS} markets (block table)");
    }
}
