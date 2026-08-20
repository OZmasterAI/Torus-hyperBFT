//! bl1 exec-chain-sub-100-attribution: the PRODUCTION (always-compiled) split
//! of `save_order_books` into its two passes.
//!
//! `save_books` is 101–139 ms per native block on the campaign's 10-market
//! cells — 11 % of the exec critical chain — but only ONE of its two passes
//! could ever leave the exec thread:
//!
//!   * pass 1, the journal DRAIN (`take_row_ops` / `take_level_ops` + level
//!     digests), reads the LIVE book levels, which the next block's engine
//!     mutates — it can never be deferred;
//!   * pass 2, the overlay WRITES (order rows, level rows, stop diff, meta),
//!     touches only the overlay and is the half a flush worker could take.
//!
//! Deciding whether moving pass 2 is worth anything needs the production
//! share, not the `save-timings` µbench feature (compiled out of the node).
//! Contract under test:
//!   * both accumulators fill on the two-pass (level-authority) path;
//!   * they are ADDITIVE across saves and bounded by the measured wall time of
//!     `save_order_books` (they are two disjoint spans inside it);
//!   * they stay 0 in Classic mode, which never runs the two-pass save;
//!   * the timers are node-local: an identical action sequence still produces
//!     an identical native state root.

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb, mode: BookMode) -> NativeExecContext {
    NativeExecContext::new_with_mode(
        state_db,
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        None,
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

type SignedAction = (Address, NativeAction);

fn place(sender: Address, p: PlaceOrderParams) -> SignedAction {
    (sender, NativeAction::PlaceOrder(p))
}

/// Resting ladder across three markets, then a crossing storm — enough row and
/// level journal work that both passes are measurable.
fn batches() -> (Vec<SignedAction>, Vec<SignedAction>) {
    let mut seed = Vec::new();
    for m in 1..=3u64 {
        for lvl in 0..6i64 {
            seed.push(place(addr(1 + lvl as u8), gtc(m, true, 97 - lvl, 5)));
            seed.push(place(addr(9 + lvl as u8), gtc(m, false, 103 + lvl, 5)));
        }
    }
    let mut cross = Vec::new();
    for m in 1..=3u64 {
        cross.push(place(addr(21), gtc(m, true, 120, 20)));
        cross.push(place(addr(22), gtc(m, false, 80, 20)));
    }
    (seed, cross)
}

/// Run one "block": seed batch, save, cross batch, save. Returns the context's
/// save split and the resulting native state root.
fn run(mode: BookMode) -> (u128, u128, alloy_primitives::B256, u128) {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, mode);
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    let (seed, cross) = batches();

    NativeExecutor::execute_batch(&mut ctx, &seed);
    let wall0 = std::time::Instant::now();
    ctx.save_order_books();
    let wall_first = wall0.elapsed().as_nanos();
    let after_first = ctx.save_split;

    NativeExecutor::execute_batch(&mut ctx, &cross);
    assert!(ctx.fatal_error.is_none(), "fatal: {:?}", ctx.fatal_error);
    let wall1 = std::time::Instant::now();
    ctx.save_order_books();
    let wall_total = wall_first + wall1.elapsed().as_nanos();

    let a = ctx.save_split;
    assert!(
        a.drain_ns >= after_first.drain_ns && a.write_ns >= after_first.write_ns,
        "the save split must ACCUMULATE across saves (first {after_first:?}, total {a:?})",
    );

    let root = compute_native_state_root(&ctx.state).expect("state root");
    (a.drain_ns, a.write_ns, root, wall_total)
}

/// The two-pass (mode 3) save must fill BOTH halves, and the two halves are
/// disjoint spans inside `save_order_books`, so their sum cannot exceed the
/// measured wall time of the calls that produced them.
#[test]
fn save_split_fills_both_passes_and_is_bounded_by_wall() {
    let (drain_ns, write_ns, _root, wall_ns) = run(BookMode::LevelAuthorityChunked);
    assert!(
        drain_ns > 0,
        "pass 1 (journal drain) must be timed on the level-authority path",
    );
    assert!(
        write_ns > 0,
        "pass 2 (overlay writes) must be timed on the level-authority path",
    );
    assert!(
        drain_ns + write_ns <= wall_ns,
        "drain ({drain_ns} ns) + write ({write_ns} ns) are disjoint spans INSIDE \
         save_order_books and must not exceed its wall ({wall_ns} ns)",
    );
}

/// Classic mode never runs the two-pass save, so a node on mode 0 must report
/// 0/0 rather than a misattributed number. summarize.py reports these as
/// `save_books.drain_ms` / `write_ms` and they are only meaningful on modes 2/3
/// (the campaign runs `TORUS_BOOK_ROWS=3`).
#[test]
fn save_split_is_zero_in_classic_mode() {
    let (drain_ns, write_ns, _root, _wall) = run(BookMode::Classic);
    assert_eq!(
        (drain_ns, write_ns),
        (0, 0),
        "Classic mode has no two-pass save to attribute",
    );
}

/// The timers are node-local instrumentation: two identical runs of the same
/// action sequence must still produce byte-identical native state.
#[test]
fn save_split_timers_do_not_perturb_state() {
    let (_d1, _w1, root_a, _wall_a) = run(BookMode::LevelAuthorityChunked);
    let (_d2, _w2, root_b, _wall_b) = run(BookMode::LevelAuthorityChunked);
    assert_eq!(
        root_a, root_b,
        "the save-split timers must not change any persisted byte",
    );
}
