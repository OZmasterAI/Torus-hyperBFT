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

/// T long 200 @ 1,000 (198k notional at 990 > 100k), collateral 5,500:
/// AV 3,500 < MM 4,950, 3 x 3,500 >= 2 x 4,950 -> stage 1 in 20% chunks.
/// Chunk 1 at ts 1001; nothing until ts 1031 (age 30); chunk 2 = 20% of 160.
#[test]
fn chunks_of_20_percent_with_a_30s_block_time_cooldown() {
    let (_d, db) = liq_db(&[1]);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(5_500));
    fund(&ctx, &s, fp(10_000_000));
    fund(&ctx, &m, fp(10_000_000));
    open_pair(&ctx, &t, &s, 1, 200, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 100));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), fp(160), "chunk 1 = 40");
    assert_eq!(liq_rows(&ctx, 0x02).len(), 1, "cooldown row");
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), fp(160), "same block: cooldown");
    ctx.save_order_books();
    for h in [2u64, 30] {
        let mut c = ctx_at(db.clone(), h); // ts 1002 / 1030: still < 30 s
        NativeExecutor::run_liquidations(&mut c);
        assert_eq!(pos(&c, &t, 1), fp(160), "h {h}");
        c.save_order_books();
    }
    let mut c = ctx_at(db.clone(), 31); // ts 1031: 30 s after 1001, mark age 30 (usable)
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), fp(128), "chunk 2 = 20% of 160");
    assert_eq!(oi(&c, 1), (fp(200), fp(200)));
}

/// HL: during the cooldown only the backstop can act. Mark 975 at ts 1005:
/// AV 4,900 - 4,000 = 900, 3 x 900 < 2 x 3,900 -> backstop; cooldown cleared.
#[test]
fn backstop_acts_during_the_cooldown() {
    let (_d, db) = liq_db(&[1]);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(5_500));
    fund(&ctx, &s, fp(10_000_000));
    fund(&ctx, &m, fp(10_000_000));
    open_pair(&ctx, &t, &s, 1, 200, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 100));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx); // chunk 1 -> 160 left, available 4,900
    ctx.save_order_books();
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
/// MM 99), previous mark 990 stored. Block 2 mark 900: T's AV -200 -> ADL at
/// 990 (previous mark): S2 ranked first (2.06 vs 0.38) closes 3, S1 closes 1.
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
    assert_eq!(bal(&c2, &t).available, fp(160), "200 + 4 x (990 - 1,000): no deficit");
    assert_eq!(bal(&c2, &s2).available, fp(1_330));
    assert_eq!(bal(&c2, &s1).available, fp(10_010));
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

/// D8: the vault (exempt from stage 1 / backstop) is ADL'd when its AV < 0, at
/// the previous mark: backstop at 975 (block 1), mark 900 (block 2): vault AV
/// 50 - 750 < 0 -> closes long 10 against S at 975.
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
    assert_eq!(bal(&c2, &LIQUIDATOR_VAULT).available, fp(50), "closed at its entry 975");
    assert_eq!(bal(&c2, &s).available, fp(1_000_250), "10 x (1,000 - 975)");
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
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
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
