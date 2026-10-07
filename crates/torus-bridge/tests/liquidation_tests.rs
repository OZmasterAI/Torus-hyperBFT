//! Item 3: Hyperliquid-style liquidation — `docs/plans/liquidation.md`,
//! `docs/plans/liquidation-impl.md`. Helpers copied from oracle_block_tests.rs,
//! account_margin_tests.rs and market_order_margin_tests.rs.

use std::collections::BTreeMap;

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::liquidation::{
    adl_escrow, next_obligation, Obligation, ADL_ESCROW_LONG, ADL_ESCROW_SHORT, ADL_OBLIGATION_TAG, ADL_TRANSFER_UNITS,
    ADL_WORK_PER_BLOCK, LIQUIDATOR_VAULT,
};
use torus_core::position::{MarginType, NativeBalance, Position};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

#[path = "common/counting_backend.rs"]
mod counting_backend;
use counting_backend::CountingBackend;

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

/// Block `height`, timestamp `1_000 + height` (one second per block).
fn ctx_at(db: StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(db, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

/// Markets listed with the test-fixture row (undecodable ⇒ default 20x config).
fn liq_db(markets: &[MarketId]) -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    for m in markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    (dir, db)
}

fn market_row(initial_margin: FixedPoint) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), initial_margin.raw()))
        .unwrap()
}

fn fund(ctx: &NativeExecContext, t: &Address, amount: FixedPoint) {
    ctx.positions
        .put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn bal(ctx: &NativeExecContext, t: &Address) -> NativeBalance {
    ctx.positions.get_native_balance(t).unwrap()
}

/// (available, order_margin) — `NativeBalance` has no `PartialEq`.
fn ab(ctx: &NativeExecContext, t: &Address) -> (FixedPoint, FixedPoint) {
    let b = bal(ctx, t);
    (b.available, b.order_margin)
}

/// `long` buys / `short` sells `qty` at `px` — a matched pair (OI stays symmetric).
fn open_pair(ctx: &NativeExecContext, long: &Address, short: &Address, m: MarketId, qty: i64, px: i64) {
    ctx.positions.apply_fill(long, m, true, fp(qty), fp(px), MarginType::Cross).unwrap();
    ctx.positions.apply_fill(short, m, false, fp(qty), fp(px), MarginType::Cross).unwrap();
}

fn pos(ctx: &NativeExecContext, t: &Address, m: MarketId) -> FixedPoint {
    match ctx.positions.get_position(t, m).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

fn all_positions(ctx: &NativeExecContext) -> Vec<Position> {
    ctx.state
        .iterate_cf(CF_NATIVE_POSITIONS, None)
        .unwrap()
        .into_iter()
        .map(|(_, v)| borsh::from_slice(&v).unwrap())
        .collect()
}

/// (Σ long size, Σ short size) in market `m`.
fn oi(ctx: &NativeExecContext, m: MarketId) -> (FixedPoint, FixedPoint) {
    let (mut l, mut s) = (FixedPoint::ZERO, FixedPoint::ZERO);
    for p in all_positions(ctx).into_iter().filter(|p| p.market_id == m) {
        if p.is_long { l += p.size } else { s += p.size }
    }
    (l, s)
}

/// Net signed size (long +, short -) per market over EVERY position row
/// (traders, both ADL escrows, the vault, the liquidator): markets with a
/// position only. 18c s99: the price-0 value sum equals the sum at the marks
/// only while each market nets to 0, so its tests assert this map is all 0.
fn net_size_per_market(ctx: &NativeExecContext) -> BTreeMap<MarketId, FixedPoint> {
    let mut net = BTreeMap::new();
    for p in all_positions(ctx) {
        *net.entry(p.market_id).or_insert(FixedPoint::ZERO) += if p.is_long { p.size } else { -p.size };
    }
    net
}

/// `markets`, each with net size 0 ([`net_size_per_market`]'s expected map).
fn net_zero(markets: &[MarketId]) -> BTreeMap<MarketId, FixedPoint> {
    markets.iter().map(|&m| (m, FixedPoint::ZERO)).collect()
}

/// Σ over every balance row (available + order margin) + Σ UPnL at `marks`.
fn total_value(ctx: &NativeExecContext, marks: &BTreeMap<MarketId, FixedPoint>) -> FixedPoint {
    let mut v = FixedPoint::ZERO;
    for (k, b) in ctx.state.iterate_cf(CF_NATIVE_BALANCES, None).unwrap() {
        if k.len() == 20 {
            let b: NativeBalance = borsh::from_slice(&b).unwrap();
            v += b.available + b.order_margin;
        }
    }
    for p in all_positions(ctx) {
        v += marks.get(&p.market_id).map_or(FixedPoint::ZERO, |mk| p.unrealized_pnl(*mk));
    }
    v
}

/// Distinct traders with position rows, ascending — the candidate walk's input.
fn traders(ctx: &NativeExecContext) -> Vec<Address> {
    let mut v: Vec<Address> = all_positions(ctx).iter().map(|p| p.trader).collect();
    v.dedup();
    v
}

/// adl-budget s99 ranking units: the traders other than the two escrows
/// with a position row in `m` (what a ranking of `m` charges).
fn holders(ctx: &NativeExecContext, m: MarketId) -> u64 {
    all_positions(ctx).iter().filter(|p| p.market_id == m && p.trader != ADL_ESCROW_LONG && p.trader != ADL_ESCROW_SHORT).count() as u64
}

fn liq_rows(ctx: &NativeExecContext, tag: u8) -> Vec<(Vec<u8>, Vec<u8>)> {
    ctx.state.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[tag])).unwrap()
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

fn stop_market(m: MarketId, is_buy: bool, trigger: i64, cap: i64, qty: i64, reduce_only: bool) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(cap),
        quantity: fp(qty),
        order_type: OrderType::StopMarket { trigger: fp(trigger) },
        time_in_force: TimeInForce::IOC,
        reduce_only,
        client_order_id: None,
    }
}

fn place(ctx: &mut NativeExecContext, t: &Address, p: PlaceOrderParams) {
    let r = NativeExecutor::execute(ctx, t, &NativeAction::PlaceOrder(p));
    assert!(r.success, "{:?}", r.error);
}

/// The aggregated mark of `m` at the context's block (3 equal reporters).
fn set_mark<T: StateBackend>(ctx: &NativeExecContext<T>, m: MarketId, price: FixedPoint) {
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle.submit_price(v, m, price, ctx.block_height, ctx.timestamp).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    assert_eq!(ctx.oracle.aggregate_price(m, ctx.block_height, ctx.timestamp, &stakes).unwrap(), price);
}

fn marks(v: &[(MarketId, i64)]) -> BTreeMap<MarketId, FixedPoint> {
    v.iter().map(|&(m, p)| (m, fp(p))).collect()
}

// ---- T4: margin configs from market rows ----

/// F2/F8: configs come from CF_NATIVE_MARKETS — max leverage = floor(100 /
/// initial_margin %), one flat tier; undecodable / non-positive rows: none.
#[test]
fn margin_configs_load_from_market_rows() {
    let (_d, db) = open_test_db();
    let rows: [(u64, Vec<u8>); 6] = [
        (1, market_row(fp(5))),             // 20x
        (2, market_row(fp(2))),             // 50x
        (3, market_row(fp(3))),             // 33.3 -> 33x
        (4, market_row(fp(200))),           // 0.5 -> clamped to 1x
        (5, market_row(FixedPoint::ZERO)),  // none
        (6, b"listed".to_vec()),            // undecodable: none
    ];
    for (m, row) in &rows {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), row).unwrap();
    }
    let ctx = ctx_at(db, 1);
    assert!(ctx.fatal_error.is_none());
    let cfg = |m: MarketId| {
        ctx.margin_configs.get(&m).map(|c| {
            assert_eq!(c.tiers.len(), 1, "one flat tier");
            assert_eq!(c.tiers[0].max_notional, FixedPoint::MAX);
            assert_eq!(c.tiers[0].max_leverage, c.max_leverage);
            c.max_leverage
        })
    };
    assert_eq!([cfg(1), cfg(2), cfg(3), cfg(4), cfg(5), cfg(6)], [Some(20), Some(50), Some(33), Some(1), None, None]);
}

/// D11: for a 20x market the loaded config is byte-identical to no config
/// (today's production IM) — single and sharded batch paths.
#[test]
fn twenty_x_market_config_is_byte_identical_to_no_config() {
    let run = |row: &[u8], workers: usize| {
        let (dir, db) = open_test_db();
        db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), row).unwrap();
        let mut ctx = ctx_at(db.clone(), 1);
        for t in [addr(1), addr(2), addr(3), addr(200)] {
            fund(&ctx, &t, fp(1_000_000));
        }
        set_mark(&ctx, 1, fp(1_000));
        let mkt = |is_buy: bool, cap: i64, qty: i64| PlaceOrderParams {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            ..limit(1, is_buy, cap, qty)
        };
        let acts = vec![
            (addr(1), NativeAction::PlaceOrder(limit(1, true, 990, 7))),
            (addr(2), NativeAction::PlaceOrder(limit(1, false, 1_010, 5))),
            (addr(3), NativeAction::PlaceOrder(mkt(false, 900, 4))),
            (addr(3), NativeAction::PlaceOrder(mkt(true, 1_100, 6))),
            (addr(200), NativeAction::PlaceOrder(limit(9, true, 1, 1))), // filler (2 senders)
        ];
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &acts, workers).results;
        assert!(r.iter().all(|x| x.success));
        let dump = |cf| db.iterate_cf(cf, None).unwrap();
        (dump(CF_NATIVE_BALANCES), dump(CF_NATIVE_POSITIONS), dir)
    };
    for workers in [1, 4] {
        let (b1, p1, _d1) = run(&market_row(fp(5)), workers);
        let (b2, p2, _d2) = run(b"listed", workers);
        assert_eq!(b1, b2, "balances, {workers} workers");
        assert_eq!(p1, p2, "positions, {workers} workers");
    }
}

// ---- T7a: scan, skip, cancel, stage 1 ----

/// Stage 1: AV 200 < MM 247.5 (and >= 2/3 MM): one reduce-only market sell
/// into the book closes the long at 985; the trader keeps 300 - 150 = 150.
#[test]
fn stage1_closes_into_the_book_and_the_trader_keeps_the_rest() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990));
    let before = total_value(&ctx, &marks(&[(1, 990)]));
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &m, 1), fp(10), "the maker bought it");
    assert_eq!(ab(&ctx, &t), (fp(150), FixedPoint::ZERO));
    assert_eq!(oi(&ctx, 1), (fp(10), fp(10)));
    assert_eq!(total_value(&ctx, &marks(&[(1, 990)])), before);
    assert_eq!(traders(&ctx), vec![s, m], "t has no position rows left");
}

/// "Remaining collateral stays once MM is met": the larger-MM market goes
/// first; afterwards AV 140 >= MM 24.75, so market 2 stays open.
#[test]
fn stage1_stops_once_maintenance_is_met() {
    let (_d, db) = liq_db(&[1, 2]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &t, &s, 2, 1, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    place(&mut ctx, &m, limit(2, true, 985, 1));
    set_mark(&ctx, 1, fp(990));
    set_mark(&ctx, 2, fp(990));
    // AV 300 - 100 - 10 = 190 < MM 272.25; 3 x 190 >= 2 x 272.25 -> stage 1
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &t, 2), fp(1), "MM met after market 1: market 2 untouched");
    assert_eq!(bal(&ctx, &t).available, fp(150));
}

/// Review H2 (user decision s517, replaces the old "decision 9" skip): a
/// position in a market WITHOUT a usable mark is valued at its entry price
/// (UPnL 0, its IM / MM still count) and is NOT acted on; the account is
/// liquidated through its MARKED positions. Was: one unmarked dust position
/// shielded the whole account forever. Here: m1 long 10 (mark 990) + dust long
/// 1 in m2 (no mark): AV 300 - 100 + 0 = 200 < MM 247.5 + 25, 3 x 200 >= 2 x
/// 272.5 -> stage 1 sells m1 into the book; the m2 position stays.
#[test]
fn an_unmarked_position_does_not_shield_the_account() {
    let (_d, db) = liq_db(&[1, 2]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &t, &s, 2, 1, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990)); // market 2: no mark
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO, "the marked position is liquidated");
    assert_eq!(pos(&ctx, &t, 2), fp(1), "the unmarked position stays");
    assert_eq!(pos(&ctx, &m, 1), fp(10));
    assert_eq!(bal(&ctx, &t).available, fp(150));
    assert_eq!(oi(&ctx, 1), (fp(10), fp(10)));
    assert_eq!(oi(&ctx, 2), (fp(1), fp(1)));
}

/// Review H2: the backstop moves only the MARKED positions (and the
/// collateral) to the vault; the unmarked one stays with the trader.
/// Collateral 100: AV 100 - 100 + 0 = 0 < 2/3 x 272.5 -> backstop.
#[test]
fn backstop_moves_only_marked_positions() {
    let (_d, db) = liq_db(&[1, 2]);
    let mut ctx = ctx_at(db, 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(100));
    fund(&ctx, &s, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &t, &s, 2, 1, 1_000);
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &LIQUIDATOR_VAULT, 1), fp(10));
    assert_eq!(pos(&ctx, &t, 2), fp(1), "unmarked: stays with the trader");
    assert_eq!(pos(&ctx, &LIQUIDATOR_VAULT, 2), FixedPoint::ZERO);
    assert_eq!(oi(&ctx, 1), (fp(10), fp(10)));
    assert_eq!(oi(&ctx, 2), (fp(1), fp(1)));
}

/// Review H2: an account whose ONLY positions are unmarked (stale / absent
/// mark) is not liquidated — nothing moves, even far under water at entry.
/// Market 1's aggregate (ts 1001) is stale at block 62 (age 61).
#[test]
fn an_account_with_only_unmarked_positions_is_not_liquidated() {
    let (_d, db) = liq_db(&[1, 2]);
    let ctx = ctx_at(db.clone(), 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(1));
    fund(&ctx, &s, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    set_mark(&ctx, 1, fp(990));
    let mut late = ctx_at(db.clone(), 62);
    set_mark(&late, 2, fp(990)); // only market 2 (where t has nothing) is marked
    let snap = |db: &StateDb| (db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
    let before = snap(&db);
    NativeExecutor::run_liquidations(&mut late);
    assert!(late.fatal_error.is_none());
    assert_eq!(snap(&db), before);
}

/// D4 + F9: before stage 1, ALL the account's orders go — resting orders AND
/// pending stops with their reservations (T3's shared helper). order_margin
/// returns to 0.
#[test]
fn cancels_resting_orders_and_pending_stops_with_their_reservations() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    place(&mut ctx, &t, limit(1, true, 900, 1)); // reserves 45
    place(&mut ctx, &t, stop_market(1, true, 1_100, 1_200, 1, false)); // reserves 60
    assert_eq!(ab(&ctx, &t), (fp(195), fp(105)));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990)); // AV 195 + 105 - 100 = 200 -> stage 1
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.order_books[&1].orders_for_trader(&t).is_empty());
    assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "t's stop removed");
    assert_eq!(ab(&ctx, &t), (fp(150), FixedPoint::ZERO));
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
}

/// Healthy accounts: positions / balances / books unchanged; only the
/// previous-mark row is written (no cooldown, no cursor).
#[test]
fn healthy_accounts_are_untouched() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db.clone(), 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(1_000));
    fund(&ctx, &s, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    set_mark(&ctx, 1, fp(990)); // AV 900 >= MM 247.5
    let before = (db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!((db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap()), before);
    assert!(liq_rows(&ctx, 0x02).is_empty() && liq_rows(&ctx, 0x04).is_empty());
    let prev = liq_rows(&ctx, 0x03);
    assert_eq!(prev.len(), 1);
    assert_eq!(prev[0].1, fp(990).raw().to_be_bytes().to_vec());
}

// ---- T7b: chunks, cooldown ----

/// The cooldown row of `t` (`0x02 ‖ t`): the block time of its last chunk.
fn cooldown_ts(ctx: &NativeExecContext, t: &Address) -> Option<u64> {
    let k = [[0x02u8].as_slice(), t.as_slice()].concat();
    ctx.state.get_cf_raw(CF_NATIVE_LIQUIDATION, &k).unwrap().map(|v| u64::from_be_bytes(v.try_into().unwrap()))
}

/// HL parity (s88 owner item): "After a block where any position of a user is
/// partially liquidated, there is a cooldown period of 30 seconds. During this
/// cooldown period, all market liquidation orders for that user will be for
/// the entire position." T long 400 @ 1,000 (396k notional at 990 > 100k),
/// collateral 11,000: AV 7,000 < MM 9,900, 3 x 7,000 >= 2 x 9,900 -> stage 1.
/// ts 1001: 20% chunk (80) into M's 985 bid -> 320, cooldown row 1001.
/// ts 1002 (in cooldown): M bids 100 @ 970; the order is for all 320 (cap
/// 965.25) and fills 100 -> 220 (a chunk would be 64; the old rule skipped).
/// AV 4,600 < MM 5,445: still stage 1. The full order does NOT restart the
/// cooldown (row stays 1001). ts 1030 (age 29, last cooldown second): bid
/// 60 @ 970 -> full order fills 60 -> 160 (a chunk would be 44). ts 1031
/// (age 30: expired): 20% chunk again (32 of 160) -> 128, cooldown row 1031.
#[test]
fn during_the_cooldown_stage1_orders_the_entire_position() {
    let (_d, db) = liq_db(&[1]);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(11_000));
    fund(&ctx, &s, fp(10_000_000));
    fund(&ctx, &m, fp(10_000_000));
    open_pair(&ctx, &t, &s, 1, 400, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 80));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(pos(&ctx, &t, 1), fp(320), "chunk 1 = 20% of 400");
    assert_eq!(cooldown_ts(&ctx, &t), Some(1_001), "a chunk starts the cooldown");
    ctx.save_order_books();
    for (h, bid, left) in [(2u64, 100, 220), (30, 60, 160)] {
        let mut c = ctx_at(db.clone(), h); // ts 1002 / 1030: age 1 / 29 < 30
        place(&mut c, &m, limit(1, true, 970, bid));
        NativeExecutor::run_liquidations(&mut c);
        assert!(c.fatal_error.is_none());
        assert_eq!(pos(&c, &t, 1), fp(left), "h {h}: in cooldown the order is for the entire position");
        assert!(c.order_books[&1].orders_for_trader(&m).is_empty(), "h {h}: the whole bid was taken");
        assert_eq!(cooldown_ts(&c, &t), Some(1_001), "h {h}: a full-position order does not restart the cooldown");
        assert_eq!(liq_rows(&c, 0x06).len(), 1, "h {h}: still under MM -> pending");
        c.save_order_books();
    }
    let mut c = ctx_at(db.clone(), 31); // ts 1031: 30 s after 1001, mark age 30 (usable)
    place(&mut c, &m, limit(1, true, 985, 100));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), fp(128), "cooldown over: chunk 2 = 20% of 160");
    assert_eq!(cooldown_ts(&c, &t), Some(1_031), "the new chunk starts a new cooldown");
    assert_eq!(oi(&c, 1), (fp(400), fp(400)));
}

/// HL parity, rule B (owner decision s91; HL public API, 44 liquidated
/// accounts, 270 orders, orderStatus origSz): in ONE block every position of
/// the user is ordered by its own rule — above 100,000 notional a 20% chunk,
/// otherwise the whole position — largest MM first, until AV >= MM; the
/// cooldown starts after that block. (HL account 0xb0fb: HYPE 1.25M, NEAR
/// 285k, MNT 211k, ETH 206k, XPL 188k, LINK 155k, MON 109k: seven 20% orders
/// in one block.) Here: T long @ 1,000 in m1..m7 = 1,250 / 285 / 210 / 205 /
/// 190 / 155 / 88 (m7: 87,120 notional at 990, <= 100k), marks 990.
const SEVEN: [(MarketId, i64); 7] = [(1, 1_250), (2, 285), (3, 210), (4, 205), (5, 190), (6, 155), (7, 88)];

/// [`SEVEN`] for T (collateral `collateral`) against S, every mark 990, M's
/// bids `(market, price, qty)` resting; books saved (block 1, ts 1001).
fn seven_positions(collateral: i64, bids: &[(MarketId, i64, i64)]) -> (tempfile::TempDir, StateDb) {
    let ms: Vec<MarketId> = SEVEN.iter().map(|p| p.0).collect();
    let (d, db) = liq_db(&ms);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(collateral));
    fund(&ctx, &s, fp(100_000_000));
    fund(&ctx, &m, fp(100_000_000));
    for (mk, size) in SEVEN {
        open_pair(&ctx, &t, &s, mk, size, 1_000);
        set_mark(&ctx, mk, fp(990));
    }
    for &(mk, px, q) in bids {
        place(&mut ctx, &m, limit(mk, true, px, q));
    }
    ctx.save_order_books();
    (d, db)
}

/// One liquidation step at block `h` over a counting wrapper: the cooldown
/// row puts it made (books saved afterwards).
fn step_counting_cooldown_puts(db: &StateDb, h: u64) -> usize {
    let state = CountingBackend::new(db.clone());
    let mut ctx = NativeExecContext::new(state.clone(), h, 1_000 + h, 0, 1_000, 10, addr(99), addr(100), addr(101));
    state.arm();
    NativeExecutor::run_liquidations(&mut ctx);
    state.disarm();
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    ctx.save_order_books();
    state.cooldown_puts()
}

/// Rule B, no early stop. Collateral 70,000: AV 46,170 < MM 58,979.25
/// (>= 2/3) -> stage 1; a sale at 985 closes AV - MM by 19.75 per unit, so
/// it needs 649 units and block 1 sells 547 (never healthy in between).
/// M bids, per market, the 20% chunk @ 985 (m7: all 88) and 40% of the
/// size @ 966 (a whole order would reach those; cap 965.25).
/// ts 1001: m1..m6 each a 20% chunk (250, 57, 42, 41, 38, 31), m7 whole
/// (88); ONE cooldown write (1001) for the six chunks; available 70,000 -
/// 15 x 547 = 61,795; AV 43,435 < MM 45,441 -> pending.
/// ts 1002 (cooldown, rule A's "next block"): every remaining position is
/// ordered whole in this one block and takes its 966 bid (40% of the size =
/// 50% of what is left, above a 20% chunk of it); a 966 sale closes AV - MM
/// by 0.75 per unit (918 units: 689 < 2,006), so no stop. Available 61,795
/// - 34 x 918 = 30,583; AV 21,403 < MM 22,720.5 -> pending. No cooldown
/// write: whole orders neither restart nor extend it.
#[test]
fn same_block_every_position_is_ordered_by_its_own_rule() {
    let mut bids: Vec<(MarketId, i64, i64)> = SEVEN[..6].iter().map(|&(mk, size)| (mk, 985, size / 5)).collect();
    bids.push((7, 985, 88));
    bids.extend(SEVEN[..6].iter().map(|&(mk, size)| (mk, 966, size * 2 / 5)));
    let (_d, db) = seven_positions(70_000, &bids);
    let t = addr(1);
    assert_eq!(step_counting_cooldown_puts(&db, 1), 1, "the cooldown is written once for the block, not per chunk");
    let c = ctx_at(db.clone(), 1);
    let left: Vec<FixedPoint> = SEVEN.iter().map(|&(mk, _)| pos(&c, &t, mk)).collect();
    let want: Vec<FixedPoint> = [1_000, 228, 168, 164, 152, 124, 0].into_iter().map(fp).collect();
    assert_eq!(left, want, "one block: every > 100k position a 20% chunk, the <= 100k one whole");
    assert_eq!(bal(&c, &t).available, fp(61_795));
    assert_eq!(cooldown_ts(&c, &t), Some(1_001), "the chunks start the cooldown at the block's time");
    assert_eq!(liq_rows(&c, 0x06).len(), 1, "still under MM -> pending");
    drop(c);
    assert_eq!(step_counting_cooldown_puts(&db, 2), 0, "whole orders write no cooldown row");
    let c = ctx_at(db.clone(), 2);
    let left: Vec<FixedPoint> = SEVEN.iter().map(|&(mk, _)| pos(&c, &t, mk)).collect();
    let want: Vec<FixedPoint> = [500, 114, 84, 82, 76, 62, 0].into_iter().map(fp).collect();
    assert_eq!(left, want, "cooldown: every remaining position ordered whole in one block");
    assert_eq!(bal(&c, &t).available, fp(30_583));
    assert_eq!(cooldown_ts(&c, &t), Some(1_001), "not restarted");
    assert_eq!(liq_rows(&c, 0x06).len(), 1, "still under MM -> pending");
    for (mk, size) in SEVEN {
        assert_eq!(oi(&c, mk), (fp(size), fp(size)), "m{mk}: OI unchanged (M bought what T sold)");
    }
}

/// Rule B keeps the stop: positions largest MM first, the step stops once
/// AV >= MM. Collateral 77,300: AV 53,470 < MM 58,979.25 (gap 5,509.25 =
/// 279 units at 19.75). M bids every whole position @ 985. ts 1001: m1 chunk
/// 250 (AV 52,220 < MM 52,791.75), m2 chunk 57 (AV 51,935 >= MM 51,381):
/// healthy -> m3..m7 untouched; cooldown 1001; not pending.
#[test]
fn same_block_stage1_still_stops_once_maintenance_is_met() {
    let bids: Vec<(MarketId, i64, i64)> = SEVEN.iter().map(|&(mk, size)| (mk, 985, size)).collect();
    let (_d, db) = seven_positions(77_300, &bids);
    let t = addr(1);
    assert_eq!(step_counting_cooldown_puts(&db, 1), 1);
    let c = ctx_at(db.clone(), 1);
    let left: Vec<FixedPoint> = SEVEN.iter().map(|&(mk, _)| pos(&c, &t, mk)).collect();
    let want: Vec<FixedPoint> = [1_000, 228, 210, 205, 190, 155, 88].into_iter().map(fp).collect();
    assert_eq!(left, want, "m1, m2 chunked; MM met -> the rest untouched");
    assert_eq!(bal(&c, &t).available, fp(77_300 - 15 * 307));
    assert_eq!(cooldown_ts(&c, &t), Some(1_001));
    assert!(liq_rows(&c, 0x06).is_empty(), "healthy -> not pending");
}

/// The backstop keeps its role in the cooldown. ts 1001: chunk 1 (40) takes
/// the whole bid -> 160, available 4,900. ts 1002: the full-position order
/// meets an empty book: nothing fills, still stage 1 -> pending. Mark 975 at
/// ts 1005: AV 4,900 - 4,000 = 900, 3 x 900 < 2 x 3,900 -> backstop; the
/// cooldown row goes with the last position.
#[test]
fn backstop_acts_during_the_cooldown() {
    let (_d, db) = liq_db(&[1]);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(5_500));
    fund(&ctx, &s, fp(10_000_000));
    fund(&ctx, &m, fp(10_000_000));
    open_pair(&ctx, &t, &s, 1, 200, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 40));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx); // chunk 1 -> 160 left, available 4,900
    assert_eq!(pos(&ctx, &t, 1), fp(160));
    ctx.save_order_books();
    let mut c = ctx_at(db.clone(), 2);
    NativeExecutor::run_liquidations(&mut c);
    assert!(c.fatal_error.is_none());
    assert_eq!(pos(&c, &t, 1), fp(160), "empty book: the full-position order fills nothing");
    assert_eq!(liq_rows(&c, 0x06).len(), 1, "still under MM -> pending");
    assert_eq!(cooldown_ts(&c, &t), Some(1_001));
    c.save_order_books();
    let mut c = ctx_at(db.clone(), 5);
    set_mark(&c, 1, fp(975));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), FixedPoint::ZERO);
    let v = c.positions.get_position(&LIQUIDATOR_VAULT, 1).unwrap().unwrap();
    assert_eq!((v.is_long, v.size, v.entry_price), (true, fp(160), fp(975)));
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, fp(900));
    assert!(liq_rows(&c, 0x02).is_empty(), "flat -> cooldown cleared");
    assert_eq!(oi(&c, 1), (fp(200), fp(200)));
}

// ---- T7c: backstop ----

/// Decision 4 through the block step: collateral 345 (45 of it reserved by a
/// resting bid), mark 975: AV 95 < 2/3 MM (162.5): the vault takes long 10 @ 975
/// and the 95; the trader's order is cancelled; the vault now holds positions;
/// value conserved.
#[test]
fn backstop_through_the_step_moves_everything_to_the_vault() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(345));
    fund(&ctx, &s, fp(1_000_000));
    place(&mut ctx, &t, limit(1, true, 900, 1)); // 45 reserved
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    set_mark(&ctx, 1, fp(975));
    let before = total_value(&ctx, &marks(&[(1, 975)]));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &LIQUIDATOR_VAULT, 1), fp(10));
    assert_eq!(ab(&ctx, &t), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, fp(95));
    assert!(ctx.order_books[&1].orders_for_trader(&t).is_empty());
    assert_eq!(traders(&ctx), { let mut v = vec![s, LIQUIDATOR_VAULT]; v.sort(); v });
    assert_eq!(total_value(&ctx, &marks(&[(1, 975)])), before);
}

// ---- T7d: ADL ----

/// T long 4 @ 1,000 (200); L long 3 @ 1,100; S1 short 4 @ 1,000 (10,000);
/// S2 short 3 @ 1,100 (1,000). Block 1 mark 990: all healthy (T: AV 160 >=
/// MM 99), previous mark 990 stored. Block 2 mark 900: T's AV -200 -> ADL:
/// S2 ranked first (2.06 vs 0.38) closes 3, S1 closes 1.
fn adl_fixture() -> (tempfile::TempDir, StateDb, [Address; 4]) {
    let (d, db) = liq_db(&[1]);
    let ctx = ctx_at(db.clone(), 1);
    let w @ [t, s1, s2, l] = [addr(1), addr(2), addr(3), addr(4)];
    for (who, a) in [(t, 200), (s1, 10_000), (s2, 1_000), (l, 10_000)] {
        fund(&ctx, &who, fp(a));
    }
    open_pair(&ctx, &t, &s1, 1, 4, 1_000);
    open_pair(&ctx, &l, &s2, 1, 3, 1_100);
    (d, db, w)
}

/// Review H1 (user decision s517): the ADL price is the previous mark CLAMPED
/// to T's bankruptcy price (200 / 4 below entry = 950): closing at 990 left
/// the bankrupt T with +160. T ends at exactly 0; the counterparties are paid
/// the difference (S2 3 x (1,100 - 950), S1 1 x 50). Was: T kept 160.
#[test]
fn adl_closes_against_ranked_counterparties_at_the_previous_mark() {
    let (_d, db, [t, s1, s2, l]) = adl_fixture();
    let mut c1 = ctx_at(db.clone(), 1);
    set_mark(&c1, 1, fp(990));
    NativeExecutor::run_liquidations(&mut c1);
    assert_eq!(pos(&c1, &t, 1), fp(4), "AV 160 >= MM 99: healthy");
    let mut c2 = ctx_at(db.clone(), 2);
    set_mark(&c2, 1, fp(900));
    let before = total_value(&c2, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c2);
    assert_eq!(pos(&c2, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c2, &s2, 1), FixedPoint::ZERO, "ranked first: fully closed");
    assert_eq!(pos(&c2, &s1, 1), -fp(3));
    assert_eq!(pos(&c2, &l, 1), fp(3));
    assert_eq!(ab(&c2, &t), (FixedPoint::ZERO, FixedPoint::ZERO), "bankrupt: ends at 0");
    assert_eq!(bal(&c2, &s2).available, fp(1_450));
    assert_eq!(bal(&c2, &s1).available, fp(10_050));
    assert_eq!(bal(&c2, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO, "nothing left over");
    assert_eq!(oi(&c2, 1), (fp(3), fp(3)));
    assert_eq!(total_value(&c2, &marks(&[(1, 900)])), before);
}

/// D10 + D9: the first step ever has no previous mark -> ADL at the current
/// mark (900); T's -200 then moves to the vault (conserved, not written off).
#[test]
fn adl_without_a_previous_mark_uses_the_mark_and_the_deficit_goes_to_the_vault() {
    let (_d, db, [t, ..]) = adl_fixture();
    let mut c = ctx_at(db.clone(), 2);
    set_mark(&c, 1, fp(900));
    let before = total_value(&c, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), FixedPoint::ZERO);
    assert_eq!(bal(&c, &t).available, FixedPoint::ZERO);
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, -fp(200));
    assert_eq!(oi(&c, 1), (fp(3), fp(3)));
    assert_eq!(total_value(&c, &marks(&[(1, 900)])), before);
}

/// Metric: the liquidator vault's deficit (its negative cash, `-available`
/// when < 0, in tokens) is published after every liquidation step. The step
/// of the test above leaves the vault at -200.
#[test]
fn the_liquidation_step_publishes_the_vault_deficit() {
    let (_d, db, _) = adl_fixture();
    let m = std::sync::Arc::new(torus_telemetry::Metrics::new());
    let mut c = ctx_at(db.clone(), 2);
    c.metrics = Some(m.clone());
    set_mark(&c, 1, fp(900));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, -fp(200));
    assert_eq!(m.liquidator_vault_deficit.get(), 200.0);
}

/// D8: the vault (exempt from stage 1 / backstop) is ADL'd when its AV < 0:
/// backstop at 975 (block 1), mark 900 (block 2): vault AV 50 - 750 < 0 ->
/// closes long 10 against S. Review H1: at the previous mark 975 clamped to
/// the vault's bankruptcy price 975 - 50 / 10 = 970 (was 975: the vault kept 50).
#[test]
fn the_vault_is_adld_when_its_value_goes_negative() {
    let (_d, db) = liq_db(&[1]);
    let (t, s) = (addr(1), addr(2));
    let mut c1 = ctx_at(db.clone(), 1);
    fund(&c1, &t, fp(300));
    fund(&c1, &s, fp(1_000_000));
    open_pair(&c1, &t, &s, 1, 10, 1_000);
    set_mark(&c1, 1, fp(975));
    NativeExecutor::run_liquidations(&mut c1);
    assert_eq!(pos(&c1, &LIQUIDATOR_VAULT, 1), fp(10));
    let mut c2 = ctx_at(db.clone(), 2);
    set_mark(&c2, 1, fp(900));
    let before = total_value(&c2, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c2);
    assert_eq!(pos(&c2, &LIQUIDATOR_VAULT, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c2, &s, 1), FixedPoint::ZERO);
    assert_eq!(bal(&c2, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO, "closed at 970");
    assert_eq!(bal(&c2, &s).available, fp(1_000_300), "10 x (1,000 - 970)");
    assert_eq!(oi(&c2, 1), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(total_value(&c2, &marks(&[(1, 900)])), before);
}

/// Q1 (s96): ADL counterparties = EVERY opposite-side holder. 230 padding
/// traders (low addresses) x 301 rows = 69,230 rows > 65,536, each short 1 in
/// market 1 with a huge AV (ranks low). `top` (0xFF.., highest address) is
/// short 4 with AV 500 at 900 -> rank (1000/900) x (3600/500) = 8: first.
/// U long 4 @ 1,000, collateral 200: AV -200 at 900 -> ADL; 4 close against top.
#[test]
fn adl_reaches_a_top_ranked_counterparty_past_65536_rows() {
    let (_d, db) = liq_db(&[1]);
    let ctx = ctx_at(db.clone(), 1);
    let pad = |i: u32| {
        let mut a = [0x01u8; 20];
        a[16..].copy_from_slice(&i.to_be_bytes());
        Address::new(a)
    };
    let (u, top, sink) = (addr(0x02), Address::new([0xFF; 20]), Address::new([0xEE; 20]));
    fund(&ctx, &sink, fp(1_000_000_000));
    for i in 0..230 {
        fund(&ctx, &pad(i), fp(1_000_000));
        open_pair(&ctx, &sink, &pad(i), 1, 1, 1_000);
        for m in 2..=301 {
            open_pair(&ctx, &pad(i), &sink, m, 1, 100); // unlisted: valued at entry
        }
    }
    fund(&ctx, &u, fp(200));
    fund(&ctx, &top, fp(100));
    open_pair(&ctx, &u, &top, 1, 4, 1_000);
    assert!(all_positions(&ctx).len() > 65_536);
    let mut c = ctx_at(db.clone(), 2);
    set_mark(&c, 1, fp(900));
    let before = total_value(&c, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &u, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c, &top, 1), FixedPoint::ZERO, "top-ranked, highest address: closed first");
    assert!((0..230).all(|i| pos(&c, &pad(i), 1) == -fp(1)), "no padding trader touched");
    assert_eq!(oi(&c, 1), (fp(230), fp(230)));
    assert_eq!(total_value(&c, &marks(&[(1, 900)])), before);
}

/// Every tracing event (all levels): its level and fields (`message`
/// included), the values as recorded (`%` Display, `?` Debug).
#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, BTreeMap<&'static str, String>)>>>);

impl Captured {
    fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        tracing::subscriber::with_default(self.clone(), f)
    }

    /// The fields of every event whose message is `msg`.
    fn events(&self, msg: &str) -> Vec<BTreeMap<&'static str, String>> {
        let all = self.0.lock().unwrap();
        all.iter().filter(|(_, f)| f.get("message").is_some_and(|m| m == msg)).map(|(_, f)| f.clone()).collect()
    }

    fn errors(&self) -> Vec<BTreeMap<&'static str, String>> {
        let all = self.0.lock().unwrap();
        all.iter().filter(|(l, _)| *l == tracing::Level::ERROR).map(|(_, f)| f.clone()).collect()
    }
}

impl tracing::Subscriber for Captured {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        struct V(BTreeMap<&'static str, String>);
        impl tracing::field::Visit for V {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                self.0.insert(f.name(), format!("{v:?}"));
            }
        }
        let mut v = V(BTreeMap::new());
        e.record(&mut v);
        self.0.lock().unwrap().push((*e.metadata().level(), v.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Rule H (owner s96), the S=750-like case: u1 and u2 long 10 @ 1,000
/// (collateral 1,000 each), S short 20. Block 1 at 1,000: healthy. u1 cut to
/// 600, block 2 at 900 with scan 1 / act 1: only u1 is reached (ADL). u2 cut
/// to 600, block 3 at 900 again (no mark change): u2 ADL. Both get the base
/// 1,000 (the last DIFFERENT mark) clamped to their bankruptcy price 940; with
/// D10 (the previous step's mark) u2 got base 900. Read from the obligation
/// rows (W = 0: no drain).
#[test]
fn same_mark_interval_gives_the_same_pre_clamp_price() {
    use torus_core::liquidation::{adl_price, bankruptcy_price};
    let (_d, db) = liq_db(&[1]);
    let (u1, u2, s) = (addr(1), addr(2), addr(3));
    let c = ctx_at(db.clone(), 1);
    for (who, a) in [(u1, 1_000), (u2, 1_000), (s, 10_000_000)] {
        fund(&c, &who, fp(a));
    }
    open_pair(&c, &u1, &s, 1, 10, 1_000);
    open_pair(&c, &u2, &s, 1, 10, 1_000);
    let step = |h: u64, mark: i64, cut: Option<&Address>, scan: usize, act: usize| {
        let mut c = ctx_at(db.clone(), h);
        if let Some(t) = cut {
            fund(&c, t, fp(600));
        }
        set_mark(&c, 1, fp(mark));
        NativeExecutor::run_liquidations_with(&mut c, scan, act, 0);
        assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
        c
    };
    let c1 = step(1, 1_000, None, 2_048, 64);
    assert_eq!((pos(&c1, &u1, 1), pos(&c1, &u2, 1)), (fp(10), fp(10)), "healthy");
    let c2 = step(2, 900, Some(&u1), 1, 1);
    assert_eq!((pos(&c2, &u1, 1), pos(&c2, &u2, 1)), (FixedPoint::ZERO, fp(10)), "only u1 reached");
    let c3 = step(3, 900, Some(&u2), 2_048, 64);
    assert_eq!(pos(&c3, &u2, 1), FixedPoint::ZERO);
    let want = adl_price(
        fp(1_000),
        bankruptcy_price(fp(600), true, fp(10), fp(10_000)),
        fp(900),
        true,
    );
    assert_eq!(want, fp(940));
    let o = |height, trader| Obligation { height, market: 1, is_long: true, trader, size: fp(10), price: want };
    assert_eq!(obligations(&c3), vec![o(2, u1), o(3, u2)]);
}

/// 18c review nit: a `0x03` row of a bad length is malformed state — the
/// step is a node fault (`fatal_error`), never a silent reset.
#[test]
fn a_malformed_mark_row_is_fatal() {
    let (_d, db) = liq_db(&[1]);
    db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x03u8].as_slice(), &1u64.to_be_bytes()].concat(), &[1, 2, 3]).unwrap();
    let mut c = ctx_at(db.clone(), 1);
    set_mark(&c, 1, fp(1_000));
    NativeExecutor::run_liquidations(&mut c);
    assert!(c.fatal_error.as_deref().is_some_and(|e| e.contains("malformed")), "{:?}", c.fatal_error);
}

// ---- adl-budget P2 ----

/// `c(i)`, i < 4: short 3 in every market of [`p2_fixture`].
fn p2c(i: u8) -> Address {
    addr(0x40 + i)
}

/// [`p2_fixture`]'s accounts: u1, u2 (ADL at block 2), u3 (ADL once cut).
fn p2_bankrupt() -> [Address; 3] {
    [addr(0x29), addr(0x28), addr(0x21)]
}

/// Markets 1..=4 listed; block 1 at mark 1,000, then 900 (the clamp binds).
/// u1 (0x29) collateral 100 (AV 100 >= MM 100 at 1,000; -300 at 900); u2
/// (0x28) 137.00000001 (its market-3 price 962.99999999 makes the escrow's
/// average inexact: dust); u3 (0x21) 1,000 (healthy at 900; a test cuts it
/// to 100 for an ADL at block 3: same mark interval, base 1,000); each long
/// 1 @ 1,000 in every market. c(i) short 3 and sink (0x60) long 9 in every
/// market, 10^7 each (OI 12 / 12).
fn p2_fixture() -> (tempfile::TempDir, StateDb) {
    let (d, db) = liq_db(&[1, 2, 3, 4]);
    let ctx = ctx_at(db.clone(), 1);
    let [u1, u2, u3] = p2_bankrupt();
    let sink = addr(0x60);
    fund(&ctx, &u1, fp(100));
    fund(&ctx, &u2, FixedPoint::from_raw(fp(137).raw() + 1));
    fund(&ctx, &u3, fp(1_000));
    fund(&ctx, &sink, fp(10_000_000));
    for i in 0..4 {
        fund(&ctx, &p2c(i), fp(10_000_000));
    }
    for m in 1..=4 {
        for u in [u1, u2, u3] {
            open_pair(&ctx, &u, &p2c(0), m, 1, 1_000);
        }
        for i in 1..4 {
            open_pair(&ctx, &sink, &p2c(i), m, 3, 1_000);
        }
    }
    (d, db)
}

/// One step at block `h` with the marks `mv` and drain budget `w`, metrics
/// attached (fresh per block). The caller checks `fatal_error`.
fn step_marks(db: &StateDb, h: u64, mv: &[(MarketId, i64)], w: u64) -> NativeExecContext {
    let mut c = ctx_at(db.clone(), h);
    for &(m, p) in mv {
        set_mark(&c, m, fp(p));
    }
    c.metrics = Some(std::sync::Arc::new(torus_telemetry::Metrics::new()));
    NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, w);
    c
}

/// [`step_marks`] with markets 1..=4 at `mark`; no fatal error.
fn step(db: &StateDb, h: u64, mark: i64, w: u64) -> NativeExecContext {
    let c = step_marks(db, h, &[(1, mark), (2, mark), (3, mark), (4, mark)], w);
    assert!(c.fatal_error.is_none(), "block {h}: {:?}", c.fatal_error);
    c
}

fn p2_marks(mark: i64) -> BTreeMap<MarketId, FixedPoint> {
    marks(&[(1, mark), (2, mark), (3, mark), (4, mark)])
}

fn obligations(ctx: &NativeExecContext) -> Vec<Obligation> {
    let (mut out, mut start) = (Vec::new(), vec![ADL_OBLIGATION_TAG]);
    while let Some(o) = next_obligation(&ctx.state, &start).unwrap() {
        start = [o.key().as_slice(), &[0]].concat();
        out.push(o);
    }
    out
}

/// The queue's keys `(height, market, trader)` in order.
fn keys(ctx: &NativeExecContext) -> Vec<(u64, MarketId, Address)> {
    obligations(ctx).iter().map(|o| (o.height, o.market, o.trader)).collect()
}

/// OI symmetric (escrows included), escrow size = Σ rows per (market, side),
/// Σ value over ALL accounts EXACTLY `before` (mark-independent under OI
/// symmetry, so comparable across blocks; s100 item 2: the escrows' exact
/// cost basis leaves no dust: the plan's *Dust bound* is now 0).
fn invariants(ctx: &NativeExecContext, mark: i64, before: FixedPoint) {
    let rows = obligations(ctx);
    for m in 1..=4 {
        let (l, s) = oi(ctx, m);
        assert_eq!(l, s, "OI symmetric in {m}");
        for side in [true, false] {
            let owed = rows.iter().filter(|o| o.market == m && o.is_long == side).fold(FixedPoint::ZERO, |a, o| a + o.size);
            let held = pos(ctx, &adl_escrow(side), m);
            assert_eq!(if side { held } else { -held }, owed, "escrow {side} in {m} = Σ rows");
        }
    }
    let now = total_value(ctx, &p2_marks(mark));
    assert_eq!(now, before, "Σ over ALL accounts (vault, escrows)");
}

/// No position and (available, order margin) == (0, 0).
fn flat_at_zero(ctx: &NativeExecContext, t: &Address) {
    assert!(ctx.positions.positions_for_trader(t).unwrap().is_empty(), "{t}: positions");
    assert_eq!(ab(ctx, t), (FixedPoint::ZERO, FixedPoint::ZERO), "{t}: cash");
}

fn has_row(ctx: &NativeExecContext, tag: u8, t: &Address) -> bool {
    ctx.state.get_cf_raw(CF_NATIVE_LIQUIDATION, &[[tag].as_slice(), t.as_slice()].concat()).unwrap().is_some()
}

/// P2 at B (W = 0): u1 and u2 are flat at exactly 0; the long escrow holds 2
/// in every market, one row per (account, market) at the clamped price the
/// test computes sequentially (`rest` = cash + the other positions at 900):
/// u1 1,000, 1,000, 1,000, 900 (the clamp binds at m4); u2 1,000, 1,000,
/// 962.99999999, 900. D9 = 0 for both (under H the deficit is ~0). Σ value
/// exact (s100 item 2: was within 1 raw, the escrow's market-3 average
/// (962.99999999 + 1,000) / 2 truncated; its cost basis is exact).
#[test]
fn p2_a_bankrupt_account_is_flat_with_zero_collateral_at_b() {
    use torus_core::liquidation::{adl_price, bankruptcy_price};
    let (_d, db) = p2_fixture();
    let [u1, u2, _] = p2_bankrupt();
    let before = total_value(&ctx_at(db.clone(), 1), &p2_marks(900));
    step(&db, 1, 1_000, 0);
    let c = step(&db, 2, 900, 0);
    for u in [u1, u2] {
        flat_at_zero(&c, &u);
        assert!(!has_row(&c, 0x06, &u), "{u}: not pending");
    }
    for m in 1..=4 {
        assert_eq!(pos(&c, &ADL_ESCROW_LONG, m), fp(2), "m{m}");
    }
    let prices = |collateral: FixedPoint| -> Vec<FixedPoint> {
        let mut cash = collateral;
        (0..4i64)
            .map(|k| {
                let rest = cash + fp(-100 * (3 - k));
                let px = adl_price(fp(1_000), bankruptcy_price(rest, true, fp(1), fp(1_000)), fp(900), true);
                cash += px - fp(1_000);
                px
            })
            .collect()
    };
    let (p1, p2) = (prices(fp(100)), prices(FixedPoint::from_raw(fp(137).raw() + 1)));
    assert_eq!(p1, vec![fp(1_000), fp(1_000), fp(1_000), fp(900)]);
    assert_eq!(p2, vec![fp(1_000), fp(1_000), FixedPoint::from_raw(fp(963).raw() - 1), fp(900)]);
    let row = |m: MarketId, trader, price| Obligation { height: 2, market: m, is_long: true, trader, size: fp(1), price };
    let want: Vec<Obligation> =
        (1..=4).flat_map(|m| [row(m, u2, p2[m as usize - 1]), row(m, u1, p1[m as usize - 1])]).collect();
    assert_eq!(obligations(&c), want);
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO, "D9: 0 for both");
    invariants(&c, 900, before);
    assert!(NativeExecutor::liquidation_due(&c.state).unwrap(), "rows keep the step due");
    assert_eq!(c.metrics.as_ref().unwrap().liquidations_adl.get(), 2);
}

/// Design step 3: under the one-sided clamp the D9 remainder is never
/// positive. 50 accounts (seeded), each ADL'd at its own block B (W = 0)
/// with positions of both sides in markets 1..=4 at random entries, one in
/// market 5 (listed, never marked: valued at entry in `rest`, not acted on,
/// H2), a resting bid whose order margin D4 releases, and random marks per
/// block (some repeated, so rule H's base is the old `last` or `prev`, above
/// and below the mark). Per account: the vault's delta at B <= 0, the
/// account's cash (0, 0) with only the unmarked position left, and no
/// error line. (The running-`rest` shadow is `#[cfg(test)]` inside the
/// bridge: it runs in the lib's seeded L1 test, not in this binary.)
#[test]
fn p2_d9_remainder_is_never_positive() {
    let (_d, db) = liq_db(&[1, 2, 3, 4, 5]);
    let x = addr(0xF0);
    let acct = |k: u8| addr(0x80 + k);
    let mut rng = 0x5EED_u64;
    let mut next = |n: u64| {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng >> 33) % n
    };
    {
        let mut c = ctx_at(db.clone(), 1);
        fund(&c, &x, fp(1_000_000_000));
        for k in 0..50 {
            let a = acct(k);
            fund(&c, &a, fp(1_000_000));
            for m in 1..=5 {
                let (qty, px) = (1 + next(3) as i64, 900 + next(200) as i64);
                if next(2) == 0 {
                    open_pair(&c, &a, &x, m, qty, px);
                } else {
                    open_pair(&c, &x, &a, m, qty, px);
                }
            }
            place(&mut c, &a, limit(1 + next(4), true, 500, 1));
        }
        for m in 1..=4 {
            set_mark(&c, m, fp(1_000));
        }
        NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, 0);
        assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
        c.save_order_books();
    }
    // Rule H mirror per market: (last, prev).
    let (mut last, mut prev) = ([1_000i64; 5], [None::<i64>; 5]);
    let (mut above, mut below, mut deficits) = (0, 0, 0);
    let events = Captured::default();
    for k in 0..50u8 {
        let (h, a) = (k as u64 + 2, acct(k));
        let mv: Vec<(MarketId, i64)> =
            (1..=4).map(|m| (m, if next(3) == 0 { last[m as usize] } else { 850 + next(300) as i64 })).collect();
        for &(m, mk) in &mv {
            let m = m as usize;
            match if mk != last[m] { Some(last[m]) } else { prev[m] } {
                Some(b) if b > mk => above += 1,
                Some(b) if b < mk => below += 1,
                _ => {}
            }
            if mk != last[m] {
                (prev[m], last[m]) = (Some(last[m]), mk);
            }
        }
        let mut c = ctx_at(db.clone(), h);
        for &(m, mk) in &mv {
            set_mark(&c, m, fp(mk));
        }
        let at = marks(&mv);
        let b = bal(&c, &a);
        let upnl = c.positions.positions_for_trader(&a).unwrap().iter().fold(FixedPoint::ZERO, |s, p| {
            s + at.get(&p.market_id).map_or(FixedPoint::ZERO, |mk| p.unrealized_pnl(*mk))
        });
        assert!(b.order_margin > FixedPoint::ZERO, "{a}: a resting bid");
        let av = -fp(1 + next(300) as i64);
        c.positions
            .put_native_balance(&a, &NativeBalance { available: av - b.order_margin - upnl, order_margin: b.order_margin })
            .unwrap();
        let vault = bal(&c, &LIQUIDATOR_VAULT).available;
        events.with(|| NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, 0));
        assert!(c.fatal_error.is_none(), "{a}: {:?}", c.fatal_error);
        let d9 = bal(&c, &LIQUIDATOR_VAULT).available - vault;
        assert!(d9 <= FixedPoint::ZERO, "{a}: D9 remainder {d9:?} > 0");
        deficits += usize::from(d9 < FixedPoint::ZERO);
        assert_eq!(ab(&c, &a), (FixedPoint::ZERO, FixedPoint::ZERO), "{a}: cash");
        let left: Vec<MarketId> = c.positions.positions_for_trader(&a).unwrap().iter().map(|p| p.market_id).collect();
        assert_eq!(left, vec![5], "{a}: marked positions flat, the unmarked one stays (H2)");
        assert!((1..=4).all(|m| c.order_books[&m].orders_for_trader(&a).is_empty()), "{a}: D4");
        c.save_order_books();
    }
    assert!(events.errors().is_empty(), "no error line: {:?}", events.errors());
    assert_eq!(events.events("liquidation: ADL to escrow").len(), 50 * 4);
    assert!(above > 10 && below > 10, "rule-H base above ({above}) and below ({below}) the mark");
    println!("p2 D9: {deficits} of 50 accounts handed a deficit to the vault");
}

/// Plan invariant 8: the escrows are never classified. Blocks 1-3 (W = 0):
/// `liquidation_scanned` / `liquidation_acted` count exactly the
/// non-protocol accounts (8 / 0, 8 / 2, then 6 / 0: u3, c0..c3, sink); the
/// escrows get no `0x06` / `0x02` row and their positions stay, although
/// the long escrow's AV at 900 is negative.
#[test]
fn p2_escrows_are_never_classified() {
    let (_d, db) = p2_fixture();
    let counters = |c: &NativeExecContext| {
        let m = c.metrics.as_ref().unwrap();
        (m.liquidation_scanned.get(), m.liquidation_acted.get())
    };
    assert_eq!(counters(&step(&db, 1, 1_000, 0)), (8, 0));
    let c2 = step(&db, 2, 900, 0);
    assert_eq!(counters(&c2), (8, 2));
    let held = c2.positions.positions_for_trader(&ADL_ESCROW_LONG).unwrap();
    assert_eq!(held.len(), 4);
    let rows = |c: &NativeExecContext| c.positions.positions_for_trader(&ADL_ESCROW_LONG).unwrap().iter().map(|p| format!("{p:?}")).collect::<Vec<_>>();
    let held_rows = rows(&c2);
    let av = held.iter().fold(bal(&c2, &ADL_ESCROW_LONG).available, |s, p| s + p.unrealized_pnl(fp(900)));
    assert!(av < FixedPoint::ZERO, "the long escrow's AV at 900: {av:?}");
    drop(c2);
    let c3 = step(&db, 3, 900, 0);
    assert_eq!(counters(&c3), (6, 0));
    assert_eq!(rows(&c3), held_rows, "unchanged by the pass");
    for e in [ADL_ESCROW_LONG, ADL_ESCROW_SHORT] {
        assert!(!has_row(&c3, 0x06, &e) && !has_row(&c3, 0x02, &e), "{e}: no pending / cooldown row");
    }
}

/// D8 under P2: the setup of `the_vault_is_adld_when_its_value_goes_negative`,
/// block 2 with W = 0: the vault's long 10 moves to the long escrow at 970
/// (base 975 clamped to the vault's bankruptcy price 975 - 50 / 10), one row;
/// the vault keeps its own balance (0, no D9 for the vault).
#[test]
fn p2_the_vault_moves_its_positions_to_the_escrow() {
    let (_d, db) = liq_db(&[1]);
    let (t, s) = (addr(1), addr(2));
    let mut c1 = ctx_at(db.clone(), 1);
    fund(&c1, &t, fp(300));
    fund(&c1, &s, fp(1_000_000));
    open_pair(&c1, &t, &s, 1, 10, 1_000);
    set_mark(&c1, 1, fp(975));
    NativeExecutor::run_liquidations(&mut c1);
    assert_eq!(pos(&c1, &LIQUIDATOR_VAULT, 1), fp(10));
    let c2 = step_marks(&db, 2, &[(1, 900)], 0);
    assert!(c2.fatal_error.is_none(), "{:?}", c2.fatal_error);
    assert_eq!(pos(&c2, &LIQUIDATOR_VAULT, 1), FixedPoint::ZERO);
    let e = c2.positions.get_position(&ADL_ESCROW_LONG, 1).unwrap().unwrap();
    assert_eq!((e.is_long, e.size, e.entry_price), (true, fp(10), fp(970)));
    let row = Obligation { height: 2, market: 1, is_long: true, trader: LIQUIDATOR_VAULT, size: fp(10), price: fp(970) };
    assert_eq!(obligations(&c2), vec![row]);
    assert_eq!(bal(&c2, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
}

/// Q2 / Q3 (A6) with the s99 units (adl-budget.md §12), W = 63. Block B's
/// transfers come first: T = `ADL_TRANSFER_UNITS` (6) each. A row costs 1
/// (visit) + H_h if it is the first row of its (market, side) in block h
/// (the ranking cache is per block; H = the holders of the market, escrows
/// out) + 1 per candidate valued for the first time in the block (c0..c3, at
/// the block's first ranking) + 1 read (each row is size 1; the top-ranked
/// short always has >= 1 left). H_2 = 6 (c0..c3, sink, u3); H_3.. = 5 (u3
/// flat at B = 3).
/// * Block 2: B = 8 x 6 = 48; (2,1,u2) 60, (2,1,u1) 62, (2,2,u2) 70 >= 63:
///   3 rows.
/// * Block 3 (u3's rows at height 3 sort after every height-2 row): B = 4 x
///   6 = 24; (2,2,u1) 35, (2,3,u2) 42, (2,3,u1) 44, (2,4,u2) 51, (2,4,u1)
///   53, (3,1,u3) 60, then markets 2 and 3 are ranked already: (3,2,u3) 62,
///   (3,3,u3) 64 >= 63: 8 rows.
/// * Block 4: (3,4,u3) 1 + 5 + 4 + 1 = 11: the queue is empty.
#[test]
fn p2_drain_stops_at_w_and_resumes_in_fifo_order() {
    const W: u64 = 63;
    let (_d, db) = p2_fixture();
    let [u1, u2, u3] = p2_bankrupt();
    let mut before = total_value(&ctx_at(db.clone(), 1), &p2_marks(900));
    let work = |c: &NativeExecContext| c.metrics.as_ref().unwrap().liquidation_adl_work_total.get();
    step(&db, 1, 1_000, W);
    let c = step(&db, 2, 900, W);
    assert_eq!(holders(&c, 1), 6, "H_2");
    assert_eq!(keys(&c), vec![(2, 2, u1), (2, 3, u2), (2, 3, u1), (2, 4, u2), (2, 4, u1)]);
    assert_eq!(work(&c), 70);
    invariants(&c, 900, before);
    drop(c);
    fund(&ctx_at(db.clone(), 3), &u3, fp(100));
    before -= fp(900);
    let c = step(&db, 3, 900, W);
    assert_eq!(holders(&c, 1), 5, "H_3");
    assert_eq!(keys(&c), vec![(3, 4, u3)]);
    assert_eq!(work(&c), 64);
    invariants(&c, 900, before);
    drop(c);
    let c = step(&db, 4, 900, W);
    assert_eq!(keys(&c), vec![]);
    assert_eq!(work(&c), 11);
    invariants(&c, 900, before);
    assert!(c.positions.positions_for_trader(&ADL_ESCROW_LONG).unwrap().is_empty());
}

/// A step starts only while used < W and then runs to its end: block 2
/// (B = 48, the first row costs 1 + 6 + 4 + 1 = 12), W = 60 drains exactly
/// one row (60 is not < 60); W = 61 drains two (the second starts at 60 and
/// ends at 62).
#[test]
fn p2_drain_overshoots_by_at_most_one_step() {
    for (w, drained) in [(60u64, 1usize), (61, 2)] {
        let (_d, db) = p2_fixture();
        step(&db, 1, 1_000, w);
        let c = step(&db, 2, 900, w);
        assert_eq!(obligations(&c).len(), 8 - drained, "W {w}");
    }
}

/// Owner s99 decision 3: block B's own work is charged into W first
/// (`ADL_TRANSFER_UNITS` per position moved to an escrow) and B is never cut
/// by it (atomic, as designed: every account the pass ADLs is flat at B).
/// p2 fixture, block 2: u1 and u2 move 8 positions, B = 8 x 6 = 48.
/// * W = 47 (B alone exceeds W) and W = 48 (B uses all of it): no drain in
///   B — 8 rows, u1 / u2 flat at 0, the long escrow holds Σ rows, the step
///   stays due, the block's units = 48. Block 3 (no B, same W) drains all 8
///   rows for 12 + 2 + 3 x (8 + 2) = 44 units.
/// * W = 49: the drain gets the 1 unit left and runs its first row
///   (overshoot by one step): 7 rows left, 48 + 12 = 60 units.
#[test]
fn block_b_work_is_charged_into_w_before_the_drain() {
    let [u1, u2, _] = p2_bankrupt();
    for (w, left, units) in [(47u64, 8usize, 48u64), (48, 8, 48), (49, 7, 60)] {
        let (_d, db) = p2_fixture();
        let before = total_value(&ctx_at(db.clone(), 1), &p2_marks(900));
        step(&db, 1, 1_000, w);
        let c = step(&db, 2, 900, w);
        let met = c.metrics.as_ref().unwrap();
        assert_eq!(met.liquidations_adl.get(), 2, "W {w}");
        assert_eq!(met.liquidation_adl_work_total.get(), units, "W {w}: B's units (+ the drain)");
        assert_eq!(obligations(&c).len(), left, "W {w}");
        for u in [u1, u2] {
            flat_at_zero(&c, &u);
        }
        invariants(&c, 900, before);
        assert!(NativeExecutor::liquidation_due(&c.state).unwrap(), "W {w}: rows keep the step due");
        drop(c);
        if left == 8 {
            let c = step(&db, 3, 900, w);
            assert!(obligations(&c).is_empty(), "W {w}: block 3 drains the rest");
            assert_eq!(c.metrics.as_ref().unwrap().liquidation_adl_work_total.get(), 44, "W {w}");
            invariants(&c, 900, before);
        }
    }
}

/// Owner s99 decision 2: a ranking charges the HOLDERS of its market (live
/// position rows in it, the escrows never counted), not the trader set.
/// Market 1: u long 1 against c short 1; market 2: 30 bystanders long 1
/// against a sink short 30. u goes bankrupt (B = 1 transfer); the drain
/// ranks market 1 once: 1 visit + 1 holder (c) + 1 first-sight valuation
/// (c) + 1 read = 4, so the block costs T + 4 whatever the 32 traders of
/// market 2.
#[test]
fn a_ranking_charges_the_holders_of_its_market_not_the_trader_set() {
    let (_d, db) = liq_db(&[1, 2]);
    let (u, c, sink) = (addr(0x0A), addr(0x0B), addr(0xF0));
    let ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &u, fp(60)); // MM 50 at 1,000: healthy; AV 60 - 100 < 0 at 900
    fund(&ctx, &c, fp(10_000_000));
    fund(&ctx, &sink, fp(10_000_000));
    open_pair(&ctx, &u, &c, 1, 1, 1_000);
    for i in 0..30u8 {
        let b = addr(0x80 + i);
        fund(&ctx, &b, fp(10_000_000));
        open_pair(&ctx, &b, &sink, 2, 1, 1_000);
    }
    assert_eq!(traders(&ctx).len(), 33, "u, c, the sink and 30 bystanders");
    drop(ctx);
    assert!(step_marks(&db, 1, &[(1, 1_000), (2, 1_000)], ADL_WORK_PER_BLOCK).fatal_error.is_none());
    let ctx = step_marks(&db, 2, &[(1, 900), (2, 1_000)], ADL_WORK_PER_BLOCK);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    let met = ctx.metrics.as_ref().unwrap();
    assert_eq!(met.liquidations_adl.get(), 1);
    assert!(obligations(&ctx).is_empty(), "drained in B");
    flat_at_zero(&ctx, &u);
    assert_eq!(met.liquidation_adl_work_total.get(), ADL_TRANSFER_UNITS + 4, "T + 1 + 1 holder + 1 valuation + 1 read");
}

/// Default W: the drain finishes in B; both escrows end with no position
/// and (0, 0); the vault holds D9 at B (0, see the B test) + the swept dust,
/// which is 0 (s100 item 2: was 1 raw, u2's 962.99999999 made market 3's
/// average inexact; the exact cost basis closes each row at its own
/// notional): no sweep line; Σ value exact.
#[test]
fn p2_escrows_end_flat_with_zero_balance_and_the_vault_holds_the_deficit() {
    let (_d, db) = p2_fixture();
    let before = total_value(&ctx_at(db.clone(), 1), &p2_marks(900));
    step(&db, 1, 1_000, ADL_WORK_PER_BLOCK);
    let ev = Captured::default();
    let c = ev.with(|| step(&db, 2, 900, ADL_WORK_PER_BLOCK));
    assert!(obligations(&c).is_empty());
    for e in [ADL_ESCROW_LONG, ADL_ESCROW_SHORT] {
        flat_at_zero(&c, &e);
    }
    assert_eq!(
        bal(&c, &LIQUIDATOR_VAULT).available,
        FixedPoint::ZERO,
        "no dust"
    );
    assert!(ev
        .events("liquidation: ADL escrow dust to the vault")
        .is_empty());
    invariants(&c, 900, before);
}

/// Terms fixed at B: rows written at B (W = 0, mark 900); the next block has
/// mark 800 and the default W, and every close is at the row's stored price:
/// each counterparty (short 3 @ 1,000) gains (1,000 - price) x q per close.
#[test]
fn p2_counterparties_are_paid_at_the_stored_price() {
    let (_d, db) = p2_fixture();
    step(&db, 1, 1_000, 0);
    let c2 = step(&db, 2, 900, 0);
    let price_of: BTreeMap<String, FixedPoint> = obligations(&c2).iter().map(|o| (o.price.to_string(), o.price)).collect();
    let cash: Vec<FixedPoint> = (0..4).map(|i| bal(&c2, &p2c(i)).available).collect();
    let sink = bal(&c2, &addr(0x60)).available;
    drop(c2);
    let ev = Captured::default();
    let c = ev.with(|| step(&db, 3, 800, ADL_WORK_PER_BLOCK));
    assert!(obligations(&c).is_empty());
    let closes = ev.events("liquidation: ADL close");
    assert_eq!(closes.len(), 8, "one close per row");
    let mut total = FixedPoint::ZERO;
    for i in 0..4 {
        let t = p2c(i);
        let paid = closes.iter().filter(|e| e["counterparty"] == t.to_string()).fold(FixedPoint::ZERO, |s, e| {
            assert_eq!(e["size"], fp(1).to_string());
            s + fp(1_000) - price_of[&e["price"]]
        });
        assert_eq!(bal(&c, &t).available - cash[i as usize], paid, "{t}: (entry - stored price) x q");
        total += paid;
    }
    assert_eq!(total, FixedPoint::from_raw(fp(237).raw() + 1), "Σ (1,000 - price) over the 8 rows");
    assert_eq!(bal(&c, &addr(0x60)).available, sink, "the long sink is never a counterparty");
}

/// P2 edge: real holders exhausted. Market 1 only: A long 10 @ 1,000 and B
/// short 10 @ 800 are its only holders (X sold 10 @ 1,000 to A and bought
/// 10 @ 800 from B: X is flat, +2,000). Block 1 at 990 (all funded 10,000);
/// A and B cut to 500; block 2 at 900: both ADL with base 990. A: bankruptcy
/// 950 -> min(990, 950) = 950, cash 0. B: bankruptcy 850 -> max(990, 850) =
/// 990, cash -1,400 -> the vault (D9). The drain: the short row sorts first
/// and has no real long holder, so it pairs with A's row: q = 10, the vault
/// gets (990 - 950) x 10 = +400 and ends at -1,000. Both escrows flat at 0,
/// no row, OI (0, 0), Σ value unchanged.
#[test]
fn p2_exhausted_counterparties_pair_the_escrows() {
    let (_d, db) = liq_db(&[1]);
    let (a, b, x) = (addr(0x0A), addr(0x0B), addr(0x0C));
    let c = ctx_at(db.clone(), 1);
    for t in [a, b, x] {
        fund(&c, &t, fp(10_000));
    }
    open_pair(&c, &a, &x, 1, 10, 1_000);
    open_pair(&c, &x, &b, 1, 10, 800);
    assert!(c.positions.get_position(&x, 1).unwrap().is_none(), "X flat");
    drop(c);
    assert!(step_marks(&db, 1, &[(1, 990)], ADL_WORK_PER_BLOCK).fatal_error.is_none());
    let c = ctx_at(db.clone(), 2);
    fund(&c, &a, fp(500));
    fund(&c, &b, fp(500));
    let before = total_value(&c, &marks(&[(1, 900)]));
    drop(c);
    let ev = Captured::default();
    let c = ev.with(|| step_marks(&db, 2, &[(1, 900)], ADL_WORK_PER_BLOCK));
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    for t in [a, b, ADL_ESCROW_LONG, ADL_ESCROW_SHORT] {
        flat_at_zero(&c, &t);
    }
    assert!(obligations(&c).is_empty());
    assert_eq!(oi(&c, 1), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, -fp(1_000), "D9 -1,400 + pairing +400");
    let pairing = ev.events("liquidation: ADL escrow pairing");
    assert_eq!(pairing.len(), 1);
    assert_eq!(pairing[0]["vault"], fp(400).to_string());
    assert_eq!(c.metrics.as_ref().unwrap().liquidation_adl_pairing.get(), 400.0, "A7 gauge");
    assert_eq!(total_value(&c, &marks(&[(1, 900)])), before);
}

/// 18c review: the pairing never flips an escrow. Rows claiming 5 per side
/// while each escrow holds 1 (state inconsistent with the invariant escrow
/// size = Σ rows), no real holder: `cross_close` refuses q = 5 and the step
/// latches `fatal_error` (a node fault, never a silent flip).
#[test]
fn p2_a_pairing_beyond_the_escrows_size_is_fatal() {
    use torus_core::liquidation::put_obligation;
    let (_d, db) = liq_db(&[1]);
    let c = ctx_at(db.clone(), 1);
    c.positions.apply_fill(&ADL_ESCROW_LONG, 1, true, fp(1), fp(950), MarginType::Cross).unwrap();
    c.positions.apply_fill(&ADL_ESCROW_SHORT, 1, false, fp(1), fp(990), MarginType::Cross).unwrap();
    for (is_long, n, price) in [(true, 1, 950), (false, 2, 990)] {
        let o = Obligation { height: 1, market: 1, is_long, trader: addr(n), size: fp(5), price: fp(price) };
        put_obligation(&db, &o).unwrap();
    }
    drop(c);
    let c = step_marks(&db, 2, &[(1, 900)], ADL_WORK_PER_BLOCK);
    assert!(c.fatal_error.as_deref().is_some_and(|e| e.contains("cross")), "{:?}", c.fatal_error);
}

/// Review M1: a row still open after the ranked holders AND the escrow
/// pairing are exhausted breaks the invariant escrow size = Σ rows (OI
/// symmetry): fatal, never a row retried every block. A long row of 3 @ 950,
/// a real short holder with 10+; the long escrow holds (a) nothing
/// (`adl_close` closes nothing) or (b) 1 of the 3; no short row to pair.
#[test]
fn p2_a_row_left_open_after_the_holders_and_the_pairing_is_fatal() {
    use torus_core::liquidation::put_obligation;
    for held in [0, 1] {
        let (_d, db) = liq_db(&[1]);
        let (y, x) = (addr(0x0A), addr(0x0B));
        let c = ctx_at(db.clone(), 1);
        fund(&c, &y, fp(10_000));
        fund(&c, &x, fp(10_000));
        open_pair(&c, &y, &x, 1, 10, 1_000);
        if held > 0 {
            open_pair(&c, &ADL_ESCROW_LONG, &x, 1, held, 950);
        }
        let o = Obligation { height: 1, market: 1, is_long: true, trader: addr(0x10), size: fp(3), price: fp(950) };
        put_obligation(&db, &o).unwrap();
        drop(c);
        let c = step_marks(&db, 2, &[(1, 900)], ADL_WORK_PER_BLOCK);
        assert!(c.fatal_error.as_deref().is_some_and(|e| e.contains("left open")), "held {held}: {:?}", c.fatal_error);
    }
}

/// Review M2: a market's pairing scan continues where its last one stopped
/// (a per-block position, never before the row itself), not at the queue
/// head. K short rows (990) and K long rows (950) of size 1 in one market, no
/// real holder, each escrow holding K. Units: K visits (the long rows go as
/// partners before the drain reaches them) + one ranking of 0 holders (only
/// the escrows hold the market; s99: escrows never count) + the pairing
/// reads: the first scan reads the K - 1 other short rows and l1, each later
/// one only its partner: 3K - 1, linear (from the head it was K + 2 + K(K +
/// 1)/2 + K with the A6 units).
#[test]
fn p2_escrow_pairing_units_are_linear_in_the_rows() {
    use torus_core::liquidation::put_obligation;
    const K: u8 = 20;
    let (_d, db) = liq_db(&[1]);
    let c = ctx_at(db.clone(), 1);
    c.positions.apply_fill(&ADL_ESCROW_LONG, 1, true, fp(K as i64), fp(950), MarginType::Cross).unwrap();
    c.positions.apply_fill(&ADL_ESCROW_SHORT, 1, false, fp(K as i64), fp(990), MarginType::Cross).unwrap();
    for i in 0..K {
        for (is_long, price) in [(true, 950), (false, 990)] {
            let o = Obligation { height: 1, market: 1, is_long, trader: addr(0x20 + i), size: fp(1), price: fp(price) };
            put_obligation(&db, &o).unwrap();
        }
    }
    drop(c);
    let c = step_marks(&db, 2, &[(1, 900)], ADL_WORK_PER_BLOCK);
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert!(obligations(&c).is_empty());
    assert_eq!(oi(&c, 1), (FixedPoint::ZERO, FixedPoint::ZERO));
    let k = u64::from(K);
    assert_eq!(c.metrics.as_ref().unwrap().liquidation_adl_work_total.get(), 3 * k - 1);
}

/// Per block: the positions, balances and liquidation rows of a P2 run
/// (blocks 1-5 at W; u3 cut before block 3).
fn p2_run(w: u64) -> Vec<Vec<Vec<(Vec<u8>, Vec<u8>)>>> {
    let (_d, db) = p2_fixture();
    let mut out = Vec::new();
    for (h, mark) in [(1, 1_000), (2, 900), (3, 900), (4, 900), (5, 900)] {
        if h == 3 {
            fund(&ctx_at(db.clone(), 3), &p2_bankrupt()[2], fp(100));
        }
        drop(step(&db, h, mark, w));
        out.push([CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION].map(|cf| db.iterate_cf(cf, None).unwrap()).to_vec());
    }
    out
}

/// Two fresh runs give identical rows block by block (default W and W = 10:
/// no drain in B, then one row per block, a multi-block drain).
#[test]
fn p2_drain_is_deterministic() {
    for w in [ADL_WORK_PER_BLOCK, 10] {
        assert!(p2_run(w) == p2_run(w), "W {w}");
    }
}

/// W sizing (A6 #8): 200 traders hold positions in 100 listed markets; 3
/// accounts long 1 in all 100 go bankrupt in one block (300 rows). With the
/// default W the step closes everything in that block: no `0x07` row, both
/// escrows flat at 0, every ADL'd account at 0, OI symmetric, Σ value
/// exact (s100 item 2: was within the dust bound).
#[test]
fn an_hl_sized_event_closes_in_its_own_block() {
    let markets: Vec<MarketId> = (1..=100).collect();
    let (_d, db) = liq_db(&markets);
    let t = |i: u32| {
        let mut a = [0x50u8; 20];
        a[16..].copy_from_slice(&i.to_be_bytes());
        Address::new(a)
    };
    let bankrupt = [addr(0x10), addr(0x11), addr(0x12)];
    let sink = addr(0xF0);
    let c = ctx_at(db.clone(), 1);
    fund(&c, &sink, fp(1_000_000_000));
    for i in 0..200 {
        fund(&c, &t(i), fp(10_000_000));
    }
    for b in &bankrupt {
        fund(&c, b, fp(2_500)); // AV 2,500 >= MM 2,500 at 1,000; -7,500 at 900
    }
    for m in 1..=100u64 {
        let i = m as u32 - 1;
        for b in &bankrupt {
            open_pair(&c, b, &t(i), m, 1, 1_000);
        }
        open_pair(&c, &sink, &t(i + 100), m, 3, 1_000);
    }
    drop(c);
    let all = |p: i64| markets.iter().map(|&m| (m, p)).collect::<Vec<_>>();
    assert!(step_marks(&db, 1, &all(1_000), ADL_WORK_PER_BLOCK).fatal_error.is_none());
    let before = total_value(&ctx_at(db.clone(), 2), &marks(&all(900)));
    let c = step_marks(&db, 2, &all(900), ADL_WORK_PER_BLOCK);
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert_eq!(c.metrics.as_ref().unwrap().liquidations_adl.get(), 3);
    assert!(obligations(&c).is_empty(), "the queue is empty after B");
    for a in bankrupt.iter().chain(&[ADL_ESCROW_LONG, ADL_ESCROW_SHORT]) {
        flat_at_zero(&c, a);
    }
    for &m in &markets {
        let (l, s) = oi(&c, m);
        assert_eq!(l, s, "OI symmetric in {m}");
    }
    assert_eq!(total_value(&c, &marks(&all(900))), before);
}

/// A listed market without a usable mark (stale oracle): its rows wait, cost
/// 1 unit per visit and keep the step due. Rows at B = 2 in markets 1..=4;
/// blocks 70-71 mark 2..=4 only (block 2's market-1 aggregate is 68 s old).
/// W = 1: the first row (market 1) is visited and the budget is spent:
/// nothing closes. Default W: market 1's rows stay, the others drain. Block
/// 72 (market 1 marked again): the rest drains.
#[test]
fn p2_rows_of_an_unmarked_market_wait_and_cost_a_unit() {
    let (_d, db) = p2_fixture();
    let [u1, u2, _] = p2_bankrupt();
    step(&db, 1, 1_000, 0);
    drop(step(&db, 2, 900, 0));
    let some = [(2, 900), (3, 900), (4, 900)];
    let c = step_marks(&db, 70, &some, 1);
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert_eq!(obligations(&c).len(), 8, "W = 1: the market-1 row's visit spends the budget");
    drop(c);
    let c = step_marks(&db, 71, &some, ADL_WORK_PER_BLOCK);
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert_eq!(keys(&c), vec![(2, 1, u2), (2, 1, u1)], "market 1 waits");
    assert!(NativeExecutor::liquidation_due(&c.state).unwrap());
    drop(c);
    let c = step(&db, 72, 900, ADL_WORK_PER_BLOCK);
    assert!(obligations(&c).is_empty());
    assert!(c.positions.positions_for_trader(&ADL_ESCROW_LONG).unwrap().is_empty());
}

/// A delisted market (open question 1, owner s96: rank at the stored price):
/// after B, market 2's `CF_NATIVE_MARKETS` row goes; the next drain ranks its
/// rows with the stored price standing in for the mark and closes them at
/// the stored price: the escrow goes flat; OI and value invariants hold.
#[test]
fn p2_rows_of_a_delisted_market_close_at_the_stored_price() {
    let (_d, db) = p2_fixture();
    let before = total_value(&ctx_at(db.clone(), 1), &p2_marks(900));
    step(&db, 1, 1_000, 0);
    drop(step(&db, 2, 900, 0));
    db.delete_cf_raw(CF_NATIVE_MARKETS, &2u64.to_be_bytes()).unwrap();
    let ev = Captured::default();
    let c = ev.with(|| step_marks(&db, 3, &[(1, 900), (3, 900), (4, 900)], ADL_WORK_PER_BLOCK));
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert!(obligations(&c).is_empty());
    assert!(c.positions.positions_for_trader(&ADL_ESCROW_LONG).unwrap().is_empty(), "the escrow is flat");
    let m2: Vec<String> = ev.events("liquidation: ADL close").iter().filter(|e| e["market"] == "2").map(|e| e["price"].clone()).collect();
    assert_eq!(m2, vec![fp(1_000).to_string(); 2], "market 2 closed at the stored prices");
    invariants(&c, 900, before);
}

/// M2 for the vault after the drain: the vault is market 1's only short
/// holder (short 5 @ 900, cash 10, long 1 in market 2 at its entry). A
/// long-escrow row of 5 at stored price 1,000 closes against it: the vault
/// pays 5 x 100, its AV < 0 after the drain, so its pending row exists and
/// the step stays due (with the row set before the drain it would be
/// missing). The next (empty) block (W = 0) moves the vault's market-2 long
/// to the long escrow.
#[test]
fn p2_a_drain_that_sinks_the_vault_keeps_the_step_due() {
    use torus_core::liquidation::put_obligation;
    let (_d, db) = liq_db(&[1, 2]);
    let (u, s) = (addr(0x30), addr(0x31));
    let c = ctx_at(db.clone(), 1);
    c.positions.apply_fill(&ADL_ESCROW_LONG, 1, true, fp(5), fp(1_000), MarginType::Cross).unwrap();
    c.positions.apply_fill(&LIQUIDATOR_VAULT, 1, false, fp(5), fp(900), MarginType::Cross).unwrap();
    put_obligation(&db, &Obligation { height: 1, market: 1, is_long: true, trader: u, size: fp(5), price: fp(1_000) }).unwrap();
    fund(&c, &LIQUIDATOR_VAULT, fp(10));
    fund(&c, &s, fp(1_000_000));
    open_pair(&c, &LIQUIDATOR_VAULT, &s, 2, 1, 1_000);
    drop(c);
    let c = step_marks(&db, 2, &[(1, 900), (2, 1_000)], ADL_WORK_PER_BLOCK);
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert!(obligations(&c).is_empty());
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, -fp(490));
    assert!(has_row(&c, 0x06, &LIQUIDATOR_VAULT), "the vault is pending after the drain");
    assert!(NativeExecutor::liquidation_due(&c.state).unwrap());
    drop(c);
    let mut c = ctx_at(db.clone(), 3);
    NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, 0);
    assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
    assert_eq!(pos(&c, &LIQUIDATOR_VAULT, 2), FixedPoint::ZERO);
    assert_eq!(pos(&c, &ADL_ESCROW_LONG, 2), fp(1));
    assert_eq!(keys(&c), vec![(3, 2, LIQUIDATOR_VAULT)]);
}

// ---- T7e: budgets, carry-over, due, stops ----

/// D5: 5 underwater accounts, an empty book (stage-1 orders do not fill, so
/// they stay liquidatable), budgets scan 2 / act 2: blocks act on a1-a2,
/// a3-a4, a5 (end of the positions CF -> cursor deleted), then a1-a2 again. "Acted" is
/// visible as the account's resting bid being cancelled.
#[test]
fn budgets_carry_over_through_the_cursor_round_robin() {
    let (_d, db) = liq_db(&[1]);
    let s = addr(50);
    let accts: Vec<Address> = (1..=5).map(addr).collect();
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &s, fp(1_000_000));
    for a in &accts {
        fund(&ctx, a, fp(345));
        place(&mut ctx, a, limit(1, true, 900, 1)); // the "not yet acted" marker
        open_pair(&ctx, a, &s, 1, 10, 1_000);
    }
    set_mark(&ctx, 1, fp(990)); // each: AV 245 < MM 247.5, 3 x 245 >= 495 -> stage 1; bids @ 900 < cap 965.25: no fill
    let acted = |c: &NativeExecContext| -> Vec<bool> {
        accts.iter().map(|a| c.order_books[&1].orders_for_trader(a).is_empty()).collect()
    };
    let cursor = |c: &NativeExecContext| liq_rows(c, 0x04).first().map(|(_, v)| Address::from_slice(v));
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2, ADL_WORK_PER_BLOCK);
    assert_eq!(acted(&ctx), [true, true, false, false, false]);
    assert_eq!(cursor(&ctx), Some(accts[1]));
    assert!(NativeExecutor::liquidation_due(&db).unwrap(), "a cut pass is due");
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2, ADL_WORK_PER_BLOCK);
    assert_eq!(acted(&ctx), [true, true, true, true, false]);
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2, ADL_WORK_PER_BLOCK);
    assert_eq!(acted(&ctx), [true; 5]);
    assert_eq!(cursor(&ctx), None, "a5 then s (healthy) reached the end: cursor deleted");
    // Review M2 (s517): the five are still under MM (empty book), so their
    // pending rows keep the step due (was: nothing due once the cursor went).
    assert_eq!(liq_rows(&ctx, 0x06).len(), 5);
    assert!(NativeExecutor::liquidation_due(&db).unwrap());
}

/// `liquidation_due`: false on an empty CF, true with a cooldown row; P2: true
/// with an ADL obligation row (`0x07`), false again once its size is 0.
#[test]
fn liquidation_due_reads_cooldown_and_cursor_rows() {
    use torus_core::liquidation::{put_obligation, Obligation};
    let (_d, db) = liq_db(&[1]);
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
    let cooldown = [[0x02u8].as_slice(), &[7u8; 20]].concat();
    db.put_cf_raw(CF_NATIVE_LIQUIDATION, &cooldown, &1_001u64.to_be_bytes()).unwrap();
    assert!(NativeExecutor::liquidation_due(&db).unwrap());
    db.delete_cf_raw(CF_NATIVE_LIQUIDATION, &cooldown).unwrap();
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
    let o = Obligation { height: 2, market: 1, is_long: true, trader: addr(7), size: fp(3), price: fp(950) };
    put_obligation(&db, &o).unwrap();
    assert!(NativeExecutor::liquidation_due(&db).unwrap(), "an obligation row keeps the step due");
    put_obligation(&db, &Obligation { size: FixedPoint::ZERO, ..o }).unwrap();
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
}

/// D7: a stop fired by a liquidation fill runs in the same step. X's
/// reduce-only stop sell (trigger 986) fires on T's liquidation trade at 985
/// and sells X's 2 into the next bid (980).
#[test]
fn stops_fired_by_liquidation_fills_run_in_the_step() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m, x) = (addr(1), addr(2), addr(3), addr(4));
    fund(&ctx, &t, fp(300));
    for w in [s, m, x] {
        fund(&ctx, &w, fp(1_000_000));
    }
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &x, &s, 1, 2, 1_000);
    place(&mut ctx, &x, stop_market(1, false, 986, 900, 2, true));
    place(&mut ctx, &m, limit(1, true, 985, 10));
    place(&mut ctx, &m, limit(1, true, 980, 2));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &x, 1), FixedPoint::ZERO, "x's stop fired and filled at 980");
    assert_eq!(pos(&ctx, &m, 1), fp(12));
    assert_eq!(oi(&ctx, 1), (fp(12), fp(12)));
}

/// Correction s517 (T7e): the vault is skipped AFTER the candidate fetch, so
/// the fetch takes SCAN + 2 traders. With SCAN + 1 a window `[vault, b]`
/// (scan 1) would end "naturally" after `b` and delete the cursor although
/// `c` was never scanned. Vault = "torus-liquidator-vlt" (first byte 0x74)
/// sorts before b = 0x80.., c = 0x81..; both b and c are stage 1 (empty book:
/// their bids are the "acted" markers).
#[test]
fn the_vault_in_the_window_does_not_end_a_pass_early() {
    let (_d, db) = liq_db(&[1]);
    let (b, c, s) = (addr(0x80), addr(0x81), addr(0x90));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &LIQUIDATOR_VAULT, fp(1_000_000));
    open_pair(&ctx, &LIQUIDATOR_VAULT, &s, 1, 1, 1_000);
    for a in [b, c] {
        fund(&ctx, &a, fp(345));
        place(&mut ctx, &a, limit(1, true, 900, 1));
        open_pair(&ctx, &a, &s, 1, 10, 1_000);
    }
    set_mark(&ctx, 1, fp(990)); // b, c: AV 245 < MM 247.5 -> stage 1
    assert_eq!(traders(&ctx), vec![LIQUIDATOR_VAULT, b, c, s]);
    NativeExecutor::run_liquidations_with(&mut ctx, 1, 1, ADL_WORK_PER_BLOCK);
    assert!(ctx.order_books[&1].orders_for_trader(&b).is_empty(), "b acted");
    assert_eq!(ctx.order_books[&1].orders_for_trader(&c).len(), 1, "c not yet");
    let cursor = liq_rows(&ctx, 0x04).first().map(|(_, v)| Address::from_slice(v));
    assert_eq!(cursor, Some(b), "cut after b: the pass continues at c next block");
    NativeExecutor::run_liquidations_with(&mut ctx, 1, 1, ADL_WORK_PER_BLOCK);
    assert!(ctx.order_books[&1].orders_for_trader(&c).is_empty(), "c acted next");
}

/// Review H1 (user decision s517): a previous-mark row counts only if the
/// previous step had a USABLE mark — a step with a stale mark deletes it. Block
/// 1 stores 990; block 62 (market 1's aggregate aged 61: stale) deletes it;
/// block 63 marks 900: T's AV -200 -> ADL at the CURRENT mark 900 (worse for
/// T than its bankruptcy price 950, so no clamp), the -200 deficit goes to
/// the vault. With the old row T would have closed at 990 (-> 950). Value and
/// OI conserved.
#[test]
fn a_prev_mark_older_than_the_previous_usable_mark_is_ignored() {
    let (_d, db, [t, s1, s2, _l]) = adl_fixture();
    let mut c1 = ctx_at(db.clone(), 1);
    set_mark(&c1, 1, fp(990));
    NativeExecutor::run_liquidations(&mut c1);
    assert_eq!(liq_rows(&c1, 0x03).len(), 1);
    let mut stale = ctx_at(db.clone(), 62);
    NativeExecutor::run_liquidations(&mut stale);
    assert!(liq_rows(&stale, 0x03).is_empty(), "unusable mark: the row is deleted");
    let mut c3 = ctx_at(db.clone(), 63);
    set_mark(&c3, 1, fp(900));
    let before = total_value(&c3, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c3);
    assert_eq!(pos(&c3, &t, 1), FixedPoint::ZERO);
    assert_eq!(ab(&c3, &t), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(bal(&c3, &LIQUIDATOR_VAULT).available, -fp(200));
    assert_eq!(bal(&c3, &s2).available, fp(1_000) + fp(600), "3 x (1,100 - 900)");
    assert_eq!(bal(&c3, &s1).available, fp(10_100));
    assert_eq!(oi(&c3, 1), (fp(3), fp(3)));
    assert_eq!(total_value(&c3, &marks(&[(1, 900)])), before);
}

// ---- s87 Fix 2a: a step with no usable mark in any listed market ----

/// Trader `i` of the unmarked fixture (ascending in `i`).
fn ut(i: u32) -> Address {
    let mut b = [0x20u8; 20];
    b[16..].copy_from_slice(&i.to_be_bytes());
    Address::new(b)
}

const UNMARKED_PENDING: [u32; 5] = [0, 50, 150, 250, 299];

/// 300 traders with positions in the 3 listed markets, none marked; pending
/// rows for 5 of them, a cooldown row for one, previous-mark rows of markets 1-2.
fn unmarked_fixture() -> (tempfile::TempDir, StateDb) {
    let (d, db) = liq_db(&[1, 2, 3]);
    let ctx = ctx_at(db.clone(), 1);
    for i in 0..300 {
        fund(&ctx, &ut(i), fp(1_000));
    }
    for k in 0..150 {
        for m in 1..=3 {
            open_pair(&ctx, &ut(2 * k), &ut(2 * k + 1), m, 1, 1_000);
        }
    }
    for i in UNMARKED_PENDING {
        db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x06u8].as_slice(), ut(i).as_slice()].concat(), &[1]).unwrap();
    }
    db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x02u8].as_slice(), ut(10).as_slice()].concat(), &1_000u64.to_be_bytes())
        .unwrap();
    for m in [1u64, 2] {
        db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x03u8].as_slice(), &m.to_be_bytes()].concat(), &fp(990).raw().to_be_bytes())
            .unwrap();
    }
    (d, db)
}

fn cursor_row<T: StateBackend>(state: &T) -> Option<Address> {
    state.get_cf_raw(CF_NATIVE_LIQUIDATION, &[0x04]).unwrap().map(|v| Address::from_slice(&v))
}

/// Fix 2a (s87) RED: with no usable mark in any listed market every account
/// is unvaluable, so the step reads nobody's positions — and still walks the
/// same window (cursor after each step of 100: the 100th, the 200th, none —
/// the third pass consumed the last trader —, the 100th again).
/// c93c579: one positions scan per scanned trader + the vault, twice (400+).
#[test]
fn an_unmarked_step_reads_no_positions() {
    let (_d, db) = unmarked_fixture();
    let state = CountingBackend::new(db);
    let mut ctx = NativeExecContext::new(state.clone(), 2, 1_002, 0, 1_000, 10, addr(99), addr(100), addr(101));
    let mut cursors = Vec::new();
    state.arm();
    for _ in 0..4 {
        NativeExecutor::run_liquidations_with(&mut ctx, 100, 64, ADL_WORK_PER_BLOCK);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        cursors.push(cursor_row(&state));
    }
    state.disarm();
    assert_eq!(cursors, vec![Some(ut(99)), Some(ut(199)), None, Some(ut(99))]);
    assert_eq!(state.all_position_scans(), 0, "positions scans during 4 unmarked steps");
}

/// Fix 2a (s87) guard (green before and after): an unmarked step deletes the
/// pending rows of exactly the traders it scanned and the previous-mark rows
/// of the listed markets, keeps the cooldown row, moves the cursor, and writes
/// nothing else — no tombstone for an absent key (the frozen set's entry
/// count is exactly the rows that changed).
#[test]
fn unmarked_step_rows_equal_the_full_scan_rules() {
    let (_d, db) = unmarked_fixture();
    let pending = |db: &StateDb| -> Vec<u32> {
        UNMARKED_PENDING
            .into_iter()
            .filter(|&i| db.get_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x06u8].as_slice(), ut(i).as_slice()].concat()).unwrap().is_some())
            .collect()
    };
    // (pending rows left, frozen entries: pending deletes + prev-mark deletes + cursor)
    let want: [(Vec<u32>, usize); 4] = [
        (vec![150, 250, 299], 2 + 2 + 1),
        (vec![250, 299], 1 + 1),
        (vec![], 2 + 1),
        (vec![], 1),
    ];
    for (step, (left, entries)) in want.into_iter().enumerate() {
        let h = step as u64 + 2;
        let overlay = torus_state::NativeStateOverlay::new(db.clone());
        let mut ctx = NativeExecContext::new(overlay.clone(), h, 1_000 + h, 0, 1_000, 10, addr(99), addr(100), addr(101));
        NativeExecutor::run_liquidations_with(&mut ctx, 100, 64, ADL_WORK_PER_BLOCK);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        drop(ctx);
        let frozen = overlay.freeze(h);
        assert_eq!(frozen.entry_count(), entries, "step {}: changed rows", step + 1);
        frozen.flush_with_native_trie_stats(&db, None, None, None).unwrap();
        assert_eq!(pending(&db), left, "step {}: pending rows", step + 1);
        assert_eq!(liq_rows_db(&db, 0x02).len(), 1, "step {}: the cooldown row is kept", step + 1);
        assert!(liq_rows_db(&db, 0x03).is_empty(), "step {}: previous marks deleted", step + 1);
    }
}

fn liq_rows_db(db: &StateDb, tag: u8) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[tag])).unwrap()
}

// ---- Telemetry (node-local; never read by execution) ----

/// The step's metrics, read back: (stage 1, backstop, ADL, scanned, acted,
/// pending, deferred) and the step histogram's sample count.
#[derive(Debug, PartialEq)]
struct LiqTel {
    stage1: u64,
    backstop: u64,
    adl: u64,
    scanned: u64,
    acted: u64,
    pending: i64,
    deferred: i64,
    steps: u64,
}

fn liq_tel(m: &torus_telemetry::Metrics) -> LiqTel {
    let text = m.encode();
    let steps = text
        .lines()
        .find_map(|l| l.strip_prefix("torus_liquidation_step_seconds_count "))
        .map_or(0, |v| v.trim().parse().unwrap());
    LiqTel {
        stage1: m.liquidations_stage1.get(),
        backstop: m.liquidations_backstop.get(),
        adl: m.liquidations_adl.get(),
        scanned: m.liquidation_scanned.get(),
        acted: m.liquidation_acted.get(),
        pending: m.liquidation_pending.get(),
        deferred: m.liquidation_deferred.get(),
        steps,
    }
}

fn metered(ctx: &mut NativeExecContext) -> std::sync::Arc<torus_telemetry::Metrics> {
    let m = std::sync::Arc::new(torus_telemetry::Metrics::new());
    ctx.metrics = Some(m.clone());
    m
}

/// Stage 1 (the fixture of `stage1_closes_into_the_book_and_the_trader_keeps_the_rest`):
/// t and s are scanned (m has only a bid), t is acted on by stage 1 and ends
/// flat: nothing pending. One step = one histogram sample.
#[test]
fn telemetry_counts_a_stage1_account() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990));
    let met = metered(&mut ctx);
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    let want = LiqTel { stage1: 1, backstop: 0, adl: 0, scanned: 2, acted: 1, pending: 0, deferred: 0, steps: 1 };
    assert_eq!(liq_tel(&met), want);
    assert_eq!(met.liquidations_triggered.get(), 1);
}

/// Backstop (the fixture of `backstop_moves_only_marked_positions`): t goes to
/// the vault; t keeps only an unmarked position (not valuable: no pending
/// row) and the vault's AV >= 0 (no ADL).
#[test]
fn telemetry_counts_a_backstop_account() {
    let (_d, db) = liq_db(&[1, 2]);
    let mut ctx = ctx_at(db, 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(100));
    fund(&ctx, &s, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &t, &s, 2, 1, 1_000);
    set_mark(&ctx, 1, fp(990));
    let met = metered(&mut ctx);
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &LIQUIDATOR_VAULT, 1), fp(10));
    let want = LiqTel { stage1: 0, backstop: 1, adl: 0, scanned: 2, acted: 1, pending: 0, deferred: 0, steps: 1 };
    assert_eq!(liq_tel(&met), want);
}

/// ADL (`adl_fixture`): block 1 at 990 scans 4 healthy accounts; block 2 at
/// 900 ADLs t (acted, class ADL). Two steps, two samples.
#[test]
fn telemetry_counts_an_adl_account() {
    let (_d, db, [t, ..]) = adl_fixture();
    let met = std::sync::Arc::new(torus_telemetry::Metrics::new());
    let mut c1 = ctx_at(db.clone(), 1);
    c1.metrics = Some(met.clone());
    set_mark(&c1, 1, fp(990));
    NativeExecutor::run_liquidations(&mut c1);
    let want = LiqTel { stage1: 0, backstop: 0, adl: 0, scanned: 4, acted: 0, pending: 0, deferred: 0, steps: 1 };
    assert_eq!(liq_tel(&met), want, "block 1: all healthy");
    let mut c2 = ctx_at(db.clone(), 2);
    c2.metrics = Some(met.clone());
    set_mark(&c2, 1, fp(900));
    NativeExecutor::run_liquidations(&mut c2);
    assert_eq!(pos(&c2, &t, 1), FixedPoint::ZERO);
    let want = LiqTel { stage1: 0, backstop: 0, adl: 1, scanned: 8, acted: 1, pending: 0, deferred: 0, steps: 2 };
    assert_eq!(liq_tel(&met), want, "block 2: t ADL'd");
}

/// The vault's own ADL (`the_vault_is_adld_when_its_value_goes_negative`):
/// counted as ADL, NOT as acted (outside the act budget).
#[test]
fn telemetry_counts_the_vault_adl_outside_the_act_budget() {
    let (_d, db) = liq_db(&[1]);
    let (t, s) = (addr(1), addr(2));
    let mut c1 = ctx_at(db.clone(), 1);
    fund(&c1, &t, fp(300));
    fund(&c1, &s, fp(1_000_000));
    open_pair(&c1, &t, &s, 1, 10, 1_000);
    set_mark(&c1, 1, fp(975));
    NativeExecutor::run_liquidations(&mut c1);
    let mut c2 = ctx_at(db.clone(), 2);
    set_mark(&c2, 1, fp(900));
    let met = metered(&mut c2);
    NativeExecutor::run_liquidations(&mut c2);
    assert_eq!(pos(&c2, &LIQUIDATOR_VAULT, 1), FixedPoint::ZERO, "the vault was ADL'd");
    // Scanned: s only (t is flat after block 1's backstop; the vault is excluded).
    let want = LiqTel { stage1: 0, backstop: 0, adl: 1, scanned: 1, acted: 0, pending: 0, deferred: 0, steps: 1 };
    assert_eq!(liq_tel(&met), want);
}

/// 70 stage-1 accounts (a1..a70) against s = addr(200), an empty book (no
/// fill: every acted account stays under MM -> pending row), default budgets
/// (act 64):
/// * block 1: a1..a64 acted, cut; pending rows a1..a64; deferred = a65..a70
///   and s = 7 (unclassified: s is healthy, the upper bound counts it);
///   pending = 64 + 7 = 71.
/// * block 2 (from the cursor): a65..a70 acted, s healthy, end: deferred 0,
///   pending = the 70 rows.
/// * block 3 (wraps to a1): a1..a64 acted again, cut; deferred 7, of which
///   a65..a70 already hold pending rows: pending = |70 rows ∪ 7| = 71.
fn budget_fixture(metrics: bool) -> (tempfile::TempDir, StateDb, Vec<LiqTel>, Vec<String>) {
    let (d, db) = liq_db(&[1]);
    let s = addr(200);
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &s, fp(10_000_000));
    for a in (1..=70).map(addr) {
        fund(&ctx, &a, fp(345));
        place(&mut ctx, &a, limit(1, true, 900, 1));
        open_pair(&ctx, &a, &s, 1, 10, 1_000);
    }
    set_mark(&ctx, 1, fp(990));
    let met = metrics.then(|| metered(&mut ctx));
    let (mut tel, mut results) = (Vec::new(), Vec::new());
    for _ in 0..3 {
        let r = NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        results.push(format!("{r:?}"));
        if let Some(m) = &met {
            tel.push(liq_tel(m));
        }
    }
    ctx.save_order_books();
    (d, db, tel, results)
}

#[test]
fn telemetry_pending_counts_work_the_act_budget_deferred() {
    let (_d, _db, tel, _) = budget_fixture(true);
    let want = [
        LiqTel { stage1: 64, backstop: 0, adl: 0, scanned: 64, acted: 64, pending: 71, deferred: 7, steps: 1 },
        LiqTel { stage1: 70, backstop: 0, adl: 0, scanned: 71, acted: 70, pending: 70, deferred: 0, steps: 2 },
        LiqTel { stage1: 134, backstop: 0, adl: 0, scanned: 135, acted: 134, pending: 71, deferred: 7, steps: 3 },
    ];
    assert_eq!(tel, want);
}

/// A P2 run over a counting backend: blocks 1-5 of [`p2_fixture`] (W = 19,
/// a multi-block drain), with or without metrics and the value-sum flag.
/// Returns the step results and the touched CFs per block, and the
/// `CF_NATIVE_BALANCES` iterations the steps made.
fn p2_telemetry_run(metrics: bool, value_sum: bool) -> (Vec<String>, Vec<Vec<Vec<(Vec<u8>, Vec<u8>)>>>, usize) {
    let (_d, db) = p2_fixture();
    let state = CountingBackend::new(db.clone());
    let met = metrics.then(|| std::sync::Arc::new(torus_telemetry::Metrics::new()));
    let (mut results, mut rows) = (Vec::new(), Vec::new());
    state.arm_storage_probe();
    for (h, mark) in [(1u64, 1_000), (2, 900), (3, 900), (4, 900), (5, 900)] {
        let mut c = NativeExecContext::new(state.clone(), h, 1_000 + h, 0, 1_000, 10, addr(99), addr(100), addr(101));
        for m in 1..=4 {
            set_mark(&c, m, fp(mark));
        }
        c.metrics = met.clone();
        c.liq_value_sum = value_sum;
        results.push(format!("{:?}", NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, 19)));
        assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
        rows.push([CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS, CF_NATIVE_LIQUIDATION].map(|cf| db.iterate_cf(cf, None).unwrap()).to_vec());
    }
    state.disarm_storage_probe();
    let walks = state.take_layer_calls().iter().filter(|((cf, op), _)| *cf == CF_NATIVE_BALANCES && op.starts_with("iterate")).map(|(_, n)| n).sum();
    (results, rows, walks)
}

/// Node-local: attaching metrics changes neither the step's results nor any
/// state row (every CF the step touches, dumped after the three blocks).
/// adl-budget A7: neither does the value sum; without metrics or with the
/// flag off the step walks no `CF_NATIVE_BALANCES` (a P2 multi-block drain).
#[test]
fn telemetry_does_not_change_results_or_state() {
    let (_d1, db1, _, r1) = budget_fixture(true);
    let (_d2, db2, _, r2) = budget_fixture(false);
    assert_eq!(r1, r2, "step results");
    for cf in [CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS, CF_NATIVE_LIQUIDATION] {
        assert_eq!(db1.iterate_cf(cf, None).unwrap(), db2.iterate_cf(cf, None).unwrap(), "{cf}");
    }
    let (r_on, rows_on, walks_on) = p2_telemetry_run(true, true);
    assert!(walks_on > 0, "the value sum walks the balances");
    for (metrics, flag) in [(true, false), (false, true), (false, false)] {
        let (r, rows, walks) = p2_telemetry_run(metrics, flag);
        assert_eq!(r, r_on, "results, metrics {metrics} flag {flag}");
        assert!(rows == rows_on, "rows, metrics {metrics} flag {flag}");
        assert_eq!(walks, 0, "no value-sum walk, metrics {metrics} flag {flag}");
    }
}

/// adl-budget A7 (node-local): with metrics the step reports the ADL queue
/// (obligation ROWS), the escrows' notional at the step's marks, the queue
/// deficit (Σ over both escrows of available + UPnL at the marks), the
/// drain's work units and the swept dust (cumulative, signed); with
/// `liq_value_sum` on, the value sum over ALL accounts (= the test's
/// `total_value` after every block, constant across the drain: exactly,
/// s100 item 2, so the swept dust is 0). B = block 2 with W = 0: 8 rows, the long escrow long 2 in
/// each market (8 x 900 notional), B's own 8 transfers counted (48 units,
/// s99); then W = 19: block 3 drains 3 rows for 22 units (12 + 2 + 8: H = 6
/// holders, 4 first-sight valuations), and later blocks the rest. Every
/// block: each market nets to 0 over every holder, escrows and vault
/// included (18c s99).
#[test]
fn telemetry_reports_the_adl_queue_escrow_and_value_sum() {
    let (_d, db) = p2_fixture();
    let met = std::sync::Arc::new(torus_telemetry::Metrics::new());
    let step = |h: u64, mark: i64, w: u64| {
        let mut c = ctx_at(db.clone(), h);
        for m in 1..=4 {
            set_mark(&c, m, fp(mark));
        }
        c.metrics = Some(met.clone());
        c.liq_value_sum = true;
        NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, w);
        assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
        assert_eq!(net_size_per_market(&c), net_zero(&[1, 2, 3, 4]), "block {h}: every market nets to 0");
        c
    };
    let tokens = |v: FixedPoint| v.raw() as f64 / FixedPoint::SCALE as f64;
    let escrows = |c: &NativeExecContext| {
        [ADL_ESCROW_LONG, ADL_ESCROW_SHORT].iter().fold(FixedPoint::ZERO, |s, e| {
            let held = c.positions.positions_for_trader(e).unwrap();
            held.iter().fold(s + bal(c, e).available, |s, p| s + p.unrealized_pnl(fp(900)))
        })
    };
    drop(step(1, 1_000, 0));
    let c = step(2, 900, 0);
    assert_eq!(met.liquidation_adl_queue.get(), 8, "rows");
    assert_eq!(met.liquidation_adl_escrow_notional.get(), 7_200.0);
    let deficit = escrows(&c);
    assert_eq!(met.liquidation_adl_queue_deficit.get(), tokens(deficit));
    let owed = obligations(&c).iter().fold(FixedPoint::ZERO, |s, o| s + fp(900) - o.price);
    assert_eq!(deficit, owed, "Σ (900 - price) over the rows (s100: exact, was within 1 raw)");
    assert_eq!(met.liquidation_adl_work_total.get(), 8 * ADL_TRANSFER_UNITS, "W = 0: no drain, B's transfers only");
    let before = total_value(&c, &p2_marks(900));
    assert_eq!(met.liquidation_value_sum.get(), tokens(before));
    drop(c);
    let mut h = 3;
    loop {
        let c = step(h, 900, 19);
        if h == 3 {
            assert_eq!((met.liquidation_adl_work_total.get(), met.liquidation_adl_queue.get()), (8 * ADL_TRANSFER_UNITS + 22, 5));
        }
        assert_eq!(met.liquidation_adl_queue.get(), obligations(&c).len() as i64, "block {h}");
        let now = total_value(&c, &p2_marks(900));
        assert_eq!(met.liquidation_value_sum.get(), tokens(now), "block {h}");
        assert_eq!(now, before, "block {h}: constant");
        if obligations(&c).is_empty() {
            assert_eq!(met.liquidation_adl_escrow_notional.get(), 0.0);
            assert_eq!(met.liquidation_adl_queue_deficit.get(), 0.0);
            // D9 at B = 0 (the B test), and no dust.
            assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
            assert_eq!(met.liquidation_adl_dust.get(), 0.0);
            break;
        }
        h += 1;
        assert!(h < 10, "drained");
    }
}

/// Seven stage-1 accounts a1..a7 and s = addr(200), an empty book (every
/// acted account stays pending), mark 990; books saved.
fn seven_under_mm() -> (tempfile::TempDir, StateDb) {
    let (d, db) = liq_db(&[1]);
    let s = addr(200);
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &s, fp(10_000_000));
    for a in (1..=7).map(addr) {
        fund(&ctx, &a, fp(345));
        place(&mut ctx, &a, limit(1, true, 900, 1));
        open_pair(&ctx, &a, &s, 1, 10, 1_000);
    }
    set_mark(&ctx, 1, fp(990));
    ctx.save_order_books();
    (d, db)
}

/// Review nit: `deferred` is bounded by the scan window (the walk fetches
/// scan + 2 candidates). Scan 4 / act 4: the budget runs out exactly at the
/// 4th (= scan-th) account: nothing in the window was deferred (a5, a6 are
/// lookahead), pending = the 4 rows. Scan 4 / act 2: a3, a4 deferred.
#[test]
fn telemetry_deferred_stays_inside_the_scan_window() {
    for (act, deferred, pending) in [(4usize, 0i64, 4i64), (2, 2, 4)] {
        let (_d, db) = seven_under_mm();
        let mut ctx = ctx_at(db, 1);
        let met = metered(&mut ctx);
        NativeExecutor::run_liquidations_with(&mut ctx, 4, act, ADL_WORK_PER_BLOCK);
        assert!(ctx.fatal_error.is_none());
        let t = liq_tel(&met);
        assert_eq!((t.acted, t.deferred, t.pending), (act as u64, deferred, pending), "act {act}");
    }
}

/// Review S1: the pending rows are re-counted only when the step changed one
/// (or on the first step of a Metrics instance, i.e. after a start). Block 2
/// sets 7 rows (scan); block 3 acts on the same 7, still pending: no row
/// changes, no scan, the gauge keeps 7; a fresh Metrics (restart) at block 4
/// scans once more.
#[test]
fn telemetry_rescans_pending_rows_only_after_a_change() {
    let (_d, db) = seven_under_mm();
    let state = CountingBackend::new(db.clone());
    let step = |h: u64, met: &std::sync::Arc<torus_telemetry::Metrics>| {
        let mut ctx = NativeExecContext::new(state.clone(), h, 1_000 + h, 0, 1_000, 10, addr(99), addr(100), addr(101));
        ctx.metrics = Some(met.clone());
        let before = state.pending_scans();
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        ctx.save_order_books();
        (state.pending_scans() - before, met.liquidation_pending.get(), met.liquidation_acted.get())
    };
    let m = std::sync::Arc::new(torus_telemetry::Metrics::new());
    assert_eq!(step(2, &m), (1, 7, 7), "block 2: 7 rows written -> one scan");
    assert_eq!(step(3, &m), (0, 7, 14), "block 3: no row changed -> no scan");
    let fresh = std::sync::Arc::new(torus_telemetry::Metrics::new());
    assert_eq!(step(4, &fresh), (1, 7, 7), "block 4, new Metrics: forced scan");
}

// ---- adl-budget s96 fix list (18c) ----

/// Fix list b (18c review): the ADL queue gauge is a running count — the
/// rows the step wrote (B's transfers) minus the rows it deleted (the drain,
/// the pairing) — re-counted only for a Metrics instance without a count (a
/// start) or when the running count is not consistent with the queue; an
/// empty queue is one seek. P2 over a counting backend: block 1 (empty
/// queue), block 2 = B + a drain at W = 19, block 3 with a FRESH Metrics and
/// W = 9 (one row), then W = 19 until the queue is empty. Only block 3 page-
/// scans the `0x07` rows (once); the gauge equals the rows after every block.
#[test]
fn telemetry_counts_the_adl_queue_without_rescanning_it() {
    let (_d, db) = p2_fixture();
    let state = CountingBackend::new(db.clone());
    let rows = || db.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[ADL_OBLIGATION_TAG])).unwrap().len() as i64;
    let step = |h: u64, mark: i64, w: u64, met: &std::sync::Arc<torus_telemetry::Metrics>| -> (usize, i64) {
        let mut c = NativeExecContext::new(state.clone(), h, 1_000 + h, 0, 1_000, 10, addr(99), addr(100), addr(101));
        for m in 1..=4 {
            set_mark(&c, m, fp(mark));
        }
        c.metrics = Some(met.clone());
        let before = state.queue_page_scans();
        NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, w);
        assert!(c.fatal_error.is_none(), "block {h}: {:?}", c.fatal_error);
        (state.queue_page_scans() - before, met.liquidation_adl_queue.get())
    };
    let met = std::sync::Arc::new(torus_telemetry::Metrics::new());
    assert_eq!(step(1, 1_000, 19, &met), (0, 0), "block 1: empty queue, one seek, no scan");
    let (scans, gauge) = step(2, 900, 19, &met);
    assert!(gauge > 0, "block 2: rows left after B + the drain");
    assert_eq!((scans, gauge), (0, rows()), "block 2: running count (+8 at B, - the drained rows)");
    let fresh = std::sync::Arc::new(torus_telemetry::Metrics::new());
    let (scans, gauge) = step(3, 900, 9, &fresh);
    assert!(gauge > 0, "block 3: rows left");
    assert_eq!((scans, gauge), (1, rows()), "block 3, new Metrics: one count");
    let mut h = 4;
    loop {
        let (scans, gauge) = step(h, 900, 19, &fresh);
        assert_eq!((scans, gauge), (0, rows()), "block {h}: running count");
        if gauge == 0 {
            break;
        }
        h += 1;
        assert!(h < 12, "drained");
    }
}

/// s99 review LOW 1: the running ADL queue count corrects an overcount (a
/// row write / delete that skipped the step's counters) for free: a drain
/// that ends at the queue's end has seen every remaining row and stores
/// their exact number. The p2 queue (8 rows at B = 2, W = 0), then a Metrics
/// whose count says 1,000 (8 + 992 phantom rows). Block 70 with market 1
/// unmarked (it waits) and W = 1: the budget ends the drain before the
/// queue's end, so the count stays the running one (1,000). Block 71, same
/// marks, the default W: markets 2..=4 drain, market 1's 2 rows wait, the
/// drain reaches the end: the gauge is 2 with no page scan of the queue.
#[test]
fn telemetry_adl_queue_count_self_corrects_at_the_queues_end() {
    let (_d, db) = p2_fixture();
    step(&db, 1, 1_000, 0);
    drop(step(&db, 2, 900, 0));
    let state = CountingBackend::new(db.clone());
    let rows = || db.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[ADL_OBLIGATION_TAG])).unwrap().len() as i64;
    assert_eq!(rows(), 8, "B = 2 wrote 8 rows");
    let met = std::sync::Arc::new(torus_telemetry::Metrics::new());
    met.liquidation_adl_queue_rows_cache.store(1_000, std::sync::atomic::Ordering::Relaxed);
    let step = |h: u64, w: u64| -> (usize, i64) {
        let mut c = NativeExecContext::new(state.clone(), h, 1_000 + h, 0, 1_000, 10, addr(99), addr(100), addr(101));
        for m in 2..=4 {
            set_mark(&c, m, fp(900));
        }
        c.metrics = Some(met.clone());
        let before = state.queue_page_scans();
        NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, w);
        assert!(c.fatal_error.is_none(), "block {h}: {:?}", c.fatal_error);
        (state.queue_page_scans() - before, met.liquidation_adl_queue.get())
    };
    assert_eq!(step(70, 1), (0, 1_000), "block 70: the budget ends the drain, the running count stays");
    assert_eq!(rows(), 8);
    assert_eq!(step(71, ADL_WORK_PER_BLOCK), (0, 2), "block 71: the drain saw every row, exact count, no scan");
    assert_eq!(rows(), 2, "market 1's rows wait");
}

/// Fix list c (18c review; s750vs: the sum dropped 1,740.69 in the step where
/// the marks went stale): the value sum values every position at ONE common
/// price per market (0: while OI is symmetric Σ UPnL is the same at any
/// common price), not at the mark with unmarked positions at entry, so it
/// does not move when a market loses its mark. Market 1: A long 10 @ 1,000,
/// B short 10 @ 800 (X sold to A and bought from B: flat); at the mark 900
/// their UPnL is -1,000 each, at entry 0. Block 2 marks 900; block 70 has no
/// usable mark (69 s old) and nothing else changes: the same sum, = Σ cash +
/// Σ UPnL at 900. Both blocks: market 1 nets to 0 over every holder (18c s99:
/// else the price-0 sum could hide an unbalanced market).
#[test]
fn value_sum_does_not_jump_when_a_market_loses_its_mark() {
    let (_d, db) = liq_db(&[1]);
    let (a, b, x) = (addr(0x0A), addr(0x0B), addr(0x0C));
    let c = ctx_at(db.clone(), 1);
    for t in [a, b, x] {
        fund(&c, &t, fp(10_000));
    }
    open_pair(&c, &a, &x, 1, 10, 1_000);
    open_pair(&c, &x, &b, 1, 10, 800);
    assert!(c.positions.get_position(&x, 1).unwrap().is_none(), "X flat");
    drop(c);
    let met = std::sync::Arc::new(torus_telemetry::Metrics::new());
    let step = |h: u64, mark: Option<i64>| {
        let mut c = ctx_at(db.clone(), h);
        if let Some(p) = mark {
            set_mark(&c, 1, fp(p));
        }
        c.metrics = Some(met.clone());
        c.liq_value_sum = true;
        NativeExecutor::run_liquidations(&mut c);
        assert!(c.fatal_error.is_none(), "block {h}: {:?}", c.fatal_error);
        assert_eq!(net_size_per_market(&c), net_zero(&[1]), "block {h}: market 1 nets to 0");
        met.liquidation_value_sum.get()
    };
    let tokens = |v: FixedPoint| v.raw() as f64 / FixedPoint::SCALE as f64;
    let marked = step(2, Some(900));
    let want = total_value(&ctx_at(db.clone(), 2), &marks(&[(1, 900)]));
    assert_eq!(marked, tokens(want), "block 2: = Σ cash + Σ UPnL at the mark");
    let stale = step(70, None);
    assert_eq!(pos(&ctx_at(db.clone(), 70), &a, 1), fp(10), "nothing acted on");
    assert_eq!(stale, marked, "block 70: the mark went stale, the sum stays");
}

/// Fix list f (18c review: W sizing was only arithmetic + an ignored bench):
/// the HL shape of `ubench_adl`'s HL mode at small N, in the default suite.
/// N = 20 traders each short 1 in every one of 100 listed markets (against a
/// sink long N), 3 accounts long 1 in all 100 (against the traders) go
/// bankrupt in one block: 300 rows, one side. Block B's units are exactly
/// the s99 formula (adl-budget.md §12): U(N) = 300 transfers x
/// `ADL_TRANSFER_UNITS` (B's own work) + 100 rankings x (N + 1 holders of
/// the market: the N shorts and the sink; the bankrupt accounts are flat,
/// the escrows never count) + N first-sight valuations (the N shorts, the
/// candidates; the sink is long) + 300 rows x (1 visit + 1 read). W = U(N)
/// closes every row in B; W = U(N) - 2 leaves the last row (cost 2: its
/// ranking is done) for the next block.
#[test]
fn an_hl_shaped_event_costs_exactly_the_sizing_formula() {
    const N: u64 = 20;
    let units = 300 * ADL_TRANSFER_UNITS + 100 * (N + 1) + N + 300 * 2;
    let markets: Vec<MarketId> = (1..=100).collect();
    let t = |i: u64| {
        let mut a = [0x50u8; 20];
        a[12..].copy_from_slice(&i.to_be_bytes());
        Address::new(a)
    };
    for (w, left) in [(units, 0usize), (units - 2, 1)] {
        let (_d, db) = liq_db(&markets);
        let bankrupt = [addr(0x10), addr(0x11), addr(0x12)];
        let sink = addr(0xF0);
        let c = ctx_at(db.clone(), 1);
        fund(&c, &sink, fp(1_000_000_000));
        for i in 0..N {
            fund(&c, &t(i), fp(10_000_000));
        }
        for b in &bankrupt {
            fund(&c, b, fp(2_500)); // AV 2,500 >= MM 2,500 at 1,000; -7,500 at 900
        }
        for &m in &markets {
            for i in 0..N {
                open_pair(&c, &sink, &t(i), m, 1, 1_000);
            }
            for (k, b) in bankrupt.iter().enumerate() {
                open_pair(&c, b, &t((m + k as u64) % N), m, 1, 1_000);
            }
        }
        drop(c);
        let all = |p: i64| markets.iter().map(|&m| (m, p)).collect::<Vec<_>>();
        assert!(step_marks(&db, 1, &all(1_000), w).fatal_error.is_none());
        let c = step_marks(&db, 2, &all(900), w);
        assert!(c.fatal_error.is_none(), "{:?}", c.fatal_error);
        let met = c.metrics.as_ref().unwrap();
        assert_eq!(met.liquidations_adl.get(), 3, "W {w}");
        assert_eq!(met.liquidation_adl_work_total.get(), units - 2 * left as u64, "W {w}: units = U(N)");
        assert_eq!(obligations(&c).len(), left, "W {w}");
    }
    let thin = 300 * ADL_TRANSFER_UNITS + 100 * (500 + 3) + (5_000 + 3) + 300 * 2;
    assert!(ADL_WORK_PER_BLOCK >= thin, "W covers U at N = 5,000 with 10 % holders per market (+3 protocol / sink)");
}

/// Fix list h (18c s99; s750 h803 and s750vs h620 logged one 'ADL escrow dust
/// to the vault' line per escrow): those lines ARE the sweep — the drain
/// moves a flat escrow's whole balance to the vault and logs the amount.
/// This pins the s96 end state on a two-sided storm shaped like S=750: B
/// over three blocks (act 2) and the drain interleaved with it (W = 1: one
/// row per block from block 3). L1 / L2 / L3 long 1 in markets 1 and 3, S1 /
/// S2 short 1 in markets 2 and 4 (collateral 100.00000001 for L1 and S1, 100
/// for the others; marks 1,000, then 900 for 1 / 3 and 1,100 for 2 / 4: AV
/// -100, ADL). L1's market-1 price 999.99999999 and S1's market-2 price
/// 1,000.00000001 made the escrows' averaged entries inexact (main: long
/// dust +2 raw, short -1 raw, the vault +1 raw). s100 item 2: the escrows'
/// cost basis is exact, so every escrow ends flat with exactly 0 and the
/// sweep moves nothing: no sweep line, the vault at 0 (D9 = 0 for all
/// five), the dust gauge 0. At the end: no row, both escrows with no
/// position and (0, 0).
#[test]
fn p2_a_two_sided_storm_sweeps_both_escrows_to_zero() {
    let (_d, db) = liq_db(&[1, 2, 3, 4]);
    let (l1, s1, l2, s2, l3) = (addr(0x31), addr(0x32), addr(0x33), addr(0x34), addr(0x35));
    let (c_short, d_long) = (addr(0x40), addr(0x41));
    let c = ctx_at(db.clone(), 1);
    let odd = FixedPoint::from_raw(fp(100).raw() + 1);
    for (t, cash) in [(l1, odd), (s1, odd), (l2, fp(100)), (s2, fp(100)), (l3, fp(100)), (c_short, fp(10_000_000)), (d_long, fp(10_000_000))] {
        fund(&c, &t, cash);
    }
    for l in [l1, l2, l3] {
        for m in [1, 3] {
            open_pair(&c, &l, &c_short, m, 1, 1_000);
        }
    }
    for s in [s1, s2] {
        for m in [2, 4] {
            open_pair(&c, &d_long, &s, m, 1, 1_000);
        }
    }
    drop(c);
    let met = std::sync::Arc::new(torus_telemetry::Metrics::new());
    let ev = Captured::default();
    let block = |h: u64, shocked: bool, act: usize, w: u64| {
        let mut c = ctx_at(db.clone(), h);
        for m in 1..=4u64 {
            let p = match (shocked, m % 2) {
                (false, _) => 1_000,
                (true, 1) => 900,
                (true, _) => 1_100,
            };
            set_mark(&c, m, fp(p));
        }
        c.metrics = Some(met.clone());
        ev.with(|| NativeExecutor::run_liquidations_with(&mut c, 2_048, act, w));
        assert!(c.fatal_error.is_none(), "block {h}: {:?}", c.fatal_error);
        c
    };
    drop(block(1, false, 64, 1));
    let c = block(2, true, 2, 0);
    assert_eq!(obligations(&c).len(), 4, "block 2: L1 and S1 at B (W = 0)");
    drop(c);
    let mut h = 3;
    loop {
        let c = block(h, true, 2, 1);
        if obligations(&c).is_empty() {
            assert_eq!(met.liquidations_adl.get(), 5);
            for t in [l1, s1, l2, s2, l3, ADL_ESCROW_LONG, ADL_ESCROW_SHORT] {
                flat_at_zero(&c, &t);
            }
            assert!(
                ev.events("liquidation: ADL escrow dust to the vault")
                    .is_empty(),
                "no dust to sweep"
            );
            assert_eq!(
                bal(&c, &LIQUIDATOR_VAULT).available,
                FixedPoint::ZERO,
                "the vault: D9 = 0, no dust"
            );
            assert_eq!(met.liquidation_adl_dust.get(), 0.0, "dust gauge");
            assert_eq!(met.liquidation_adl_queue.get(), 0);
            assert_eq!(met.liquidation_adl_escrow_notional.get(), 0.0);
            assert_eq!(met.liquidation_adl_queue_deficit.get(), 0.0);
            break;
        }
        h += 1;
        assert!(h < 20, "drained");
    }
}
