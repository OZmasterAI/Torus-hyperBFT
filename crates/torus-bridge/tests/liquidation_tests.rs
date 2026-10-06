//! Item 3: Hyperliquid-style liquidation — `docs/plans/liquidation.md`,
//! `docs/plans/liquidation-impl.md`. Helpers copied from oracle_block_tests.rs,
//! account_margin_tests.rs and market_order_margin_tests.rs.

use std::collections::BTreeMap;

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::liquidation::LIQUIDATOR_VAULT;
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
        v += p.unrealized_pnl(marks[&p.market_id]);
    }
    v
}

/// Distinct traders with position rows, ascending — the candidate walk's input.
fn traders(ctx: &NativeExecContext) -> Vec<Address> {
    let mut v: Vec<Address> = all_positions(ctx).iter().map(|p| p.trader).collect();
    v.dedup();
    v
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
fn set_mark(ctx: &NativeExecContext, m: MarketId, price: FixedPoint) {
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
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2);
    assert_eq!(acted(&ctx), [true, true, false, false, false]);
    assert_eq!(cursor(&ctx), Some(accts[1]));
    assert!(NativeExecutor::liquidation_due(&db).unwrap(), "a cut pass is due");
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2);
    assert_eq!(acted(&ctx), [true, true, true, true, false]);
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2);
    assert_eq!(acted(&ctx), [true; 5]);
    assert_eq!(cursor(&ctx), None, "a5 then s (healthy) reached the end: cursor deleted");
    // Review M2 (s517): the five are still under MM (empty book), so their
    // pending rows keep the step due (was: nothing due once the cursor went).
    assert_eq!(liq_rows(&ctx, 0x06).len(), 5);
    assert!(NativeExecutor::liquidation_due(&db).unwrap());
}

/// `liquidation_due`: false on an empty CF, true with a cooldown row.
#[test]
fn liquidation_due_reads_cooldown_and_cursor_rows() {
    let (_d, db) = liq_db(&[1]);
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
    db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x02u8].as_slice(), &[7u8; 20]].concat(), &1_001u64.to_be_bytes())
        .unwrap();
    assert!(NativeExecutor::liquidation_due(&db).unwrap());
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
    NativeExecutor::run_liquidations_with(&mut ctx, 1, 1);
    assert!(ctx.order_books[&1].orders_for_trader(&b).is_empty(), "b acted");
    assert_eq!(ctx.order_books[&1].orders_for_trader(&c).len(), 1, "c not yet");
    let cursor = liq_rows(&ctx, 0x04).first().map(|(_, v)| Address::from_slice(v));
    assert_eq!(cursor, Some(b), "cut after b: the pass continues at c next block");
    NativeExecutor::run_liquidations_with(&mut ctx, 1, 1);
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
        NativeExecutor::run_liquidations_with(&mut ctx, 100, 64);
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
        NativeExecutor::run_liquidations_with(&mut ctx, 100, 64);
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
