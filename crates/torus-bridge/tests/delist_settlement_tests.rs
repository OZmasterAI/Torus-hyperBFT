//! Delist option C (owner, s131) and the P0/P1 test gaps.
//!
//! Part A: a governance delist executed at height H leaves the market
//! reduce-only for N blocks (`delist_grace_blocks`, default 43,200); from
//! H + N every remaining position in it is force-closed at ONE settlement
//! price (the usable oracle mark at the first settlement block, else the last
//! aggregated oracle price; no price at all: wait), against the liquidator
//! vault (`liq::transfer`, Σ long == Σ short after every block), with a
//! bounded walk per block behind a persisted cursor.
//!
//! Part B: executor-level coverage the P0/P1 merge lacked: the governance
//! cancel with resident books and resident rows (node lifecycle) equals the
//! Classic path byte for byte, and reduce-only trading on a delisted market
//! (clamp, increasing side, stop triggers).

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{
    begin_resident, end_resident, NativeActionResult, NativeExecContext, NativeExecutor,
    ResidentBooks,
};
use torus_core::liquidation::{self as liq, DelistSettlement, LIQUIDATOR_VAULT};
use torus_core::position::{MarginType, NativeBalance};
use torus_economics::governance::{GovernanceParams, ProposalStatus};
use torus_economics::MIN_SELF_DELEGATION;
use torus_state::action_status::FailureReason;
use torus_state::cf::{
    CF_FEE_CONFIG, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS,
    CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, MarketParams, NativeAction, OracleSubmission, OrderType,
    PlaceOrderParams, Proposal, ProposalAction, TimeInForce,
};

const M: MarketId = 1;
const OTHER: MarketId = 2;
const PROPOSER: u8 = 2;
const FUND: i64 = 1_000_000;
/// Delist execution height (`pass_and_execute`).
const H: u64 = 200;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn ts(height: u64) -> u64 {
    1_000 + height
}

fn ctx_at<T: StateBackend>(state: T, height: u64) -> NativeExecContext<T> {
    NativeExecContext::new(
        state,
        height,
        ts(height),
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

fn agg_key(m: MarketId) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8).
fn agg_row(price: FixedPoint, at: u64) -> Vec<u8> {
    [
        price.raw().to_be_bytes().as_slice(),
        &7u64.to_be_bytes(),
        &3u32.to_be_bytes(),
        &at.to_be_bytes(),
    ]
    .concat()
}

/// A fresh mark of `price` for `m` at block `height`.
fn set_mark<S: StateBackend>(s: &S, m: MarketId, price: i64, height: u64) {
    s.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(fp(price), ts(height)))
        .unwrap();
}

fn set_grace(db: &StateDb, n: u64) {
    db.put_cf_raw(
        CF_FEE_CONFIG,
        torus_types::DELIST_GRACE_PARAM.as_bytes(),
        n.to_string().as_bytes(),
    )
    .unwrap();
}

/// Markets 1 and 2 listed (tick 1, lot 1, 20x), governance with a 10-block
/// timelock, a proposer with stake, `traders` funded with `FUND`.
fn setup(traders: &[u8]) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let s = FixedPoint::SCALE;
    for m in [M, OTHER] {
        let row = borsh::to_vec(&("BTC".to_string(), "USD".to_string(), s, s, 5 * s)).unwrap();
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &row).unwrap();
    }
    let ctx = ctx_at(db.clone(), 1);
    ctx.governance
        .set_governance_params(&GovernanceParams {
            voting_period_blocks: 100,
            quorum_bps: 3300,
            min_proposal_stake: wei(100),
            permanent_weight_multiplier_num: 3,
            permanent_weight_multiplier_den: 2,
            treasury_address: addr(100),
            timelock_blocks: 10,
            permanent_unlock_threshold_bps: 8000,
        })
        .unwrap();
    let val = addr(1);
    db.put_account(&val, &AccountInfo { balance: MIN_SELF_DELEGATION, ..Default::default() })
        .unwrap();
    ctx.staking.register_validator(val, [1; 32], 500, MIN_SELF_DELEGATION).unwrap();
    db.put_account(&addr(PROPOSER), &AccountInfo { balance: wei(500), ..Default::default() })
        .unwrap();
    ctx.staking.delegate(addr(PROPOSER), val, wei(500)).unwrap();
    for &t in traders {
        fund(&db, t, FUND);
    }
    (dir, db)
}

fn fund(db: &StateDb, t: u8, v: i64) {
    ctx_at(db.clone(), 1)
        .positions
        .put_native_balance(&addr(t), &NativeBalance { available: fp(v), order_margin: FixedPoint::ZERO })
        .unwrap();
}

/// `long` buys `qty` from `short` at `price` (a direct fill pair).
fn open_pair(db: &StateDb, m: MarketId, long: u8, short: u8, qty: i64, price: i64) {
    let ctx = ctx_at(db.clone(), 1);
    ctx.positions.apply_fill(&addr(long), m, true, fp(qty), fp(price), MarginType::Cross).unwrap();
    ctx.positions.apply_fill(&addr(short), m, false, fp(qty), fp(price), MarginType::Cross).unwrap();
}

fn submit(db: &StateDb, title: &str, action: ProposalAction) {
    let mut ctx = ctx_at(db.clone(), 3);
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(PROPOSER),
        &NativeAction::SubmitProposal(Proposal { title: title.into(), description: "d".into(), action }),
    );
    assert!(r.success, "{r:?}");
}

/// Lifecycle runs: a proposal voted through at seed time executes at the
/// first governance step from height `E` (vote ends 104, timelock 10).
const E: u64 = 114;

/// Submits proposal `id` and votes it through (finalized at 104).
fn submit_passed(db: &StateDb, id: u64, title: &str, action: ProposalAction) {
    submit(db, title, action);
    let ctx = ctx_at(db.clone(), 3);
    ctx.governance.cast_vote(addr(PROPOSER), id, true, 10).unwrap();
    ctx.governance.finalize_proposal(id, 104).unwrap();
}

/// The node's block tail after the liquidation step: governance, then the
/// delist settlement step.
fn gov_tail<T: StateBackend>(ctx: &mut NativeExecContext<T>) -> (Vec<NativeActionResult>, Vec<NativeActionResult>) {
    let gov = NativeExecutor::process_governance(ctx);
    let settle = NativeExecutor::run_delist_settlements(ctx);
    (gov, settle)
}

/// Votes proposals `ids` through and runs the tail at `H` (past the vote and
/// the timelock). Classic path (no resident state).
fn pass_and_execute(db: &StateDb, ids: &[u64]) -> Vec<NativeActionResult> {
    let mut ctx = ctx_at(db.clone(), H);
    for &id in ids {
        ctx.governance.cast_vote(addr(PROPOSER), id, true, 10).unwrap();
        ctx.governance.finalize_proposal(id, 104).unwrap();
    }
    let (gov, _) = gov_tail(&mut ctx);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    for &id in ids {
        assert_eq!(status(db, id), ProposalStatus::Executed, "proposal {id}: {gov:?}");
    }
    gov
}

fn delist_m(db: &StateDb) {
    submit(db, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
    pass_and_execute(db, &[1]);
}

fn status(db: &StateDb, id: u64) -> ProposalStatus {
    ctx_at(db.clone(), 1).governance.get_proposal(id).unwrap().unwrap().status
}

/// One Classic block at `height`: `actions` (engine, `threads` workers),
/// then the tail. Returns the action results and the settlement results.
fn block(
    db: &StateDb,
    height: u64,
    threads: usize,
    actions: &[(Address, NativeAction)],
) -> (Vec<NativeActionResult>, Vec<NativeActionResult>) {
    let mut ctx = ctx_at(db.clone(), height);
    let r = if actions.is_empty() {
        Vec::new()
    } else {
        NativeExecutor::execute_batch_engine_mode(&mut ctx, actions, threads).results
    };
    let (_, settle) = gov_tail(&mut ctx);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    (r, settle)
}

fn bal(db: &StateDb, a: &Address) -> NativeBalance {
    ctx_at(db.clone(), 1).positions.get_native_balance(a).unwrap()
}

fn pos_of(db: &StateDb, a: &Address, m: MarketId) -> FixedPoint {
    match ctx_at(db.clone(), 1).positions.get_position(a, m).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

fn pos(db: &StateDb, t: u8, m: MarketId) -> FixedPoint {
    pos_of(db, &addr(t), m)
}

/// Σ signed size of every position row in `m` (vault and escrows included).
fn net_oi(s: &StateDb, m: MarketId) -> (FixedPoint, usize) {
    let ctx = ctx_at(s.clone(), 1);
    let mut sum = FixedPoint::ZERO;
    let mut n = 0;
    for (k, _) in s.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap() {
        if k.len() != 28 || k[20..] != m.to_be_bytes() {
            continue;
        }
        let p = ctx.positions.get_position(&Address::from_slice(&k[..20]), m).unwrap().unwrap();
        sum += if p.is_long { p.size } else { -p.size };
        n += 1;
    }
    (sum, n)
}

fn row(db: &StateDb) -> Option<DelistSettlement> {
    liq::delist_settlement(db, M).unwrap()
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

fn ro(p: PlaceOrderParams) -> PlaceOrderParams {
    PlaceOrderParams { reduce_only: true, ..p }
}

fn place(t: u8, p: PlaceOrderParams) -> (Address, NativeAction) {
    (addr(t), NativeAction::PlaceOrder(p))
}

// ============================================================================
// Part A: grace period, then force-close at the oracle mark
// ============================================================================

/// N = 5: nothing closes before H + 5; at H + 5 both positions close at the
/// fresh mark (110), PnL realised to the accounts, OI zero, the vault flat,
/// a reduce-only order resting from the grace period cancelled and refunded,
/// the settlement row deleted.
#[test]
fn delist_grace_then_force_close_at_the_oracle_mark() {
    let (_d, db) = setup(&[13, 14, 15]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    open_pair(&db, OTHER, 15, 13, 1, 50);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    assert_eq!(
        row(&db),
        Some(DelistSettlement { market: M, close_at: H + 5, price: None, cursor: None }),
        "grace row written with the delist"
    );
    // Grace: reduce-only trading only; a reduce-only ask rests.
    let (r, _) = block(&db, H + 1, 1, &[place(14, ro(limit(M, false, 130, 1)))]);
    assert!(r[0].success, "{r:?}");
    assert!(bal(&db, &addr(14)).order_margin > FixedPoint::ZERO);
    for h in H + 2..H + 5 {
        set_mark(&db, M, 100, h);
        let (_, s) = block(&db, h, 1, &[]);
        assert!(s.is_empty(), "h {h}: nothing due: {s:?}");
        assert_eq!((pos(&db, 14, M), pos(&db, 13, M)), (fp(2), fp(-2)), "h {h}: still open");
    }
    set_mark(&db, M, 110, H + 5);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(pos(&db, 14, M), FixedPoint::ZERO, "long force-closed");
    assert_eq!(pos(&db, 13, M), FixedPoint::ZERO, "short force-closed");
    assert_eq!(pos_of(&db, &LIQUIDATOR_VAULT, M), FixedPoint::ZERO, "vault flat");
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0), "no position left in M");
    assert_eq!(pos(&db, 13, OTHER), fp(-1), "the other market untouched");
    // Long 2 @100 closed at 110: +20; short: -20. The resting ask refunded.
    let b14 = bal(&db, &addr(14));
    assert_eq!((b14.available, b14.order_margin), (fp(FUND + 20), FixedPoint::ZERO));
    assert_eq!(bal(&db, &addr(13)).available, fp(FUND - 20));
    assert_eq!(bal(&db, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO, "vault realised nothing");
    assert_eq!(ctx_at(db.clone(), H + 6).order_books()[&M].order_count(), 0, "book emptied");
    assert_eq!(row(&db), None, "settled: row deleted");
    // Settled: nothing more is due, and the market takes no exposure.
    assert!(!NativeExecutor::delist_settlement_due(&db, H + 6).unwrap());
    let (r, _) = block(&db, H + 6, 1, &[place(14, limit(M, true, 110, 1))]);
    assert_eq!(r[0].reason, FailureReason::MarketDelisted, "{r:?}");
}

/// No governance key: N = 43,200 (24 h at 2 s); a key outside the range
/// reads as the default too.
#[test]
fn default_grace_is_43200_blocks() {
    for stored in [None, Some("302401")] {
        let (_d, db) = setup(&[13, 14]);
        if let Some(v) = stored {
            db.put_cf_raw(CF_FEE_CONFIG, torus_types::DELIST_GRACE_PARAM.as_bytes(), v.as_bytes()).unwrap();
        }
        open_pair(&db, M, 14, 13, 1, 100);
        set_mark(&db, M, 100, H);
        delist_m(&db);
        assert_eq!(row(&db).map(|r| r.close_at), Some(H + 43_200), "{stored:?}");
        assert!(!NativeExecutor::delist_settlement_due(&db, H + 43_199).unwrap());
        assert!(NativeExecutor::delist_settlement_due(&db, H + 43_200).unwrap());
        set_mark(&db, M, 100, H + 1);
        block(&db, H + 1, 1, &[]);
        assert_eq!(pos(&db, 14, M), fp(1), "{stored:?}: grace");
    }
}

/// N = 0: the positions close in the delist's own block.
#[test]
fn zero_grace_force_closes_in_the_delist_block() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 0);
    open_pair(&db, M, 14, 13, 3, 100);
    set_mark(&db, M, 90, H);
    delist_m(&db);
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND - 30));
    assert_eq!(bal(&db, &addr(13)).available, fp(FUND + 30));
    assert_eq!(row(&db), None);
}

/// A stale mark at the settlement block: the positions close at the last
/// aggregated oracle price (90), not at entry and not later.
#[test]
fn stale_mark_settles_at_the_last_aggregate() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    // Last fresh aggregate 120 s of block time before H + 5: stale.
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(M), &agg_row(fp(90), ts(H + 5) - 120)).unwrap();
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND - 20));
    assert_eq!(bal(&db, &addr(13)).available, fp(FUND + 20));
}

/// No oracle price at all: nothing closes, the row waits (still due, no
/// price chosen) and the step reports it; the first price settles it.
#[test]
fn no_oracle_price_waits_for_one() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    delist_m(&db);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(!s[0].success);
    assert_eq!(
        s[0].error.as_deref(),
        Some("delist settlement: market 1 has no oracle price: waiting")
    );
    assert_eq!((pos(&db, 14, M), pos(&db, 13, M)), (fp(2), fp(-2)));
    assert_eq!(row(&db), Some(DelistSettlement { market: M, close_at: H + 5, price: None, cursor: None }));
    assert!(NativeExecutor::delist_settlement_due(&db, H + 6).unwrap());
    set_mark(&db, M, 95, H + 6);
    block(&db, H + 6, 1, &[]);
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND - 10));
    assert_eq!(bal(&db, &addr(13)).available, fp(FUND + 10));
    assert_eq!(row(&db), None);
}

/// Traders 20..36: 8 pairs (long `20 + 2i`, short `21 + 2i`, size i + 1 @ 100).
const PAIRS: u8 = 8;

fn seed_pairs(db: &StateDb) {
    for i in 0..PAIRS {
        fund(db, 20 + 2 * i, FUND);
        fund(db, 21 + 2 * i, FUND);
        open_pair(db, M, 20 + 2 * i, 21 + 2 * i, i64::from(i) + 1, 100);
    }
}

/// Bounded work: a scan budget of 3 traders and a close budget of 2 per
/// block spread the 16 positions over several blocks. After every block OI
/// is symmetric (the vault holds the closed positions' other side), the row
/// keeps the ONE settlement price chosen at the first block (110) although
/// the mark moves, and its cursor advances; at the end every trader closed
/// at 110, the vault is flat and realised nothing, the row is gone.
#[test]
fn settlement_over_several_blocks_closes_at_one_price_behind_a_cursor() {
    let (_d, db) = setup(&[]);
    set_grace(&db, 5);
    seed_pairs(&db);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    let mut blocks = 0;
    let mut last_cursor = None;
    for h in H + 5..H + 40 {
        set_mark(&db, M, 110 + 10 * (h - H - 5) as i64, h);
        let mut ctx = ctx_at(db.clone(), h);
        let s = NativeExecutor::run_delist_settlements_with(&mut ctx, 3, 2);
        assert!(s.iter().all(|r| r.success), "h {h}: {s:?}");
        assert_eq!(ctx.take_fatal_error(), None);
        drop(ctx);
        blocks += 1;
        assert_eq!(net_oi(&db, M).0, FixedPoint::ZERO, "h {h}: OI symmetric");
        match row(&db) {
            Some(r) => {
                assert_eq!(r.price, Some(fp(110)), "h {h}: one settlement price");
                assert!(r.cursor.is_some() && r.cursor > last_cursor, "h {h}: cursor advances");
                last_cursor = r.cursor;
            }
            None => break,
        }
    }
    assert!(blocks >= 8, "16 positions at 2 per block: {blocks} blocks");
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0), "every position closed");
    for i in 0..PAIRS {
        let pnl = 10 * (i64::from(i) + 1);
        assert_eq!(bal(&db, &addr(20 + 2 * i)).available, fp(FUND + pnl), "long {i}");
        assert_eq!(bal(&db, &addr(21 + 2 * i)).available, fp(FUND - pnl), "short {i}");
    }
    assert_eq!(bal(&db, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
}

/// A position liquidated during the grace period is simply gone: trader 13
/// (short 10 @100, collateral 110) is backstopped to the vault at mark 110;
/// at H + 5 the settlement closes trader 14 against the vault, which ends
/// flat; 13 is not touched again.
#[test]
fn position_liquidated_during_grace_is_skipped() {
    let (_d, db) = setup(&[14]);
    fund(&db, 13, 110);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 10, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    set_mark(&db, M, 110, H + 2);
    {
        let mut ctx = ctx_at(db.clone(), H + 2);
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(pos(&db, 13, M), FixedPoint::ZERO, "13 backstopped");
    assert_eq!(pos_of(&db, &LIQUIDATOR_VAULT, M), fp(-10), "vault took the short");
    let b13 = bal(&db, &addr(13));
    set_mark(&db, M, 110, H + 5);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 100));
    let after = bal(&db, &addr(13));
    assert_eq!((after.available, after.order_margin), (b13.available, b13.order_margin), "13 untouched");
    assert_eq!(row(&db), None);
}

/// ADL during the grace period: trader 13 (short 10 @100, collateral 50)
/// goes bankrupt at mark 120; its short moves to the ADL escrow and the drain
/// is held back (work 0), so the obligation is still open when the
/// settlement runs. The settlement closes 14 against the vault (vault long
/// 10 = minus the escrow's short: OI symmetric, no "OI asymmetry" fatal), and
/// the next liquidation step's drain closes the escrow against the vault:
/// nothing is left in the market, no obligation row remains.
#[test]
fn adl_obligation_open_at_settlement_drains_against_the_vault() {
    let (_d, db) = setup(&[14]);
    fund(&db, 13, 50);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 10, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    set_mark(&db, M, 120, H + 2);
    {
        let mut ctx = ctx_at(db.clone(), H + 2);
        let _ = NativeExecutor::run_liquidations_with(&mut ctx, 2_048, 64, 0);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(pos(&db, 13, M), FixedPoint::ZERO, "13 ADL'd to the escrow");
    assert_eq!(pos_of(&db, &liq::ADL_ESCROW_SHORT, M), fp(-10));
    assert!(liq::next_obligation(&db, &[liq::ADL_OBLIGATION_TAG]).unwrap().is_some());
    set_mark(&db, M, 120, H + 5);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(pos(&db, 14, M), FixedPoint::ZERO, "14 closed at 120");
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 200));
    assert_eq!(pos_of(&db, &LIQUIDATOR_VAULT, M), fp(10), "vault mirrors the escrow");
    assert_eq!(net_oi(&db, M).0, FixedPoint::ZERO);
    assert_eq!(row(&db), None, "walk done (protocol accounts are not walked)");
    set_mark(&db, M, 120, H + 6);
    {
        let mut ctx = ctx_at(db.clone(), H + 6);
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0), "escrow closed against the vault");
    assert!(liq::next_obligation(&db, &[liq::ADL_OBLIGATION_TAG]).unwrap().is_none());
}

/// Codex P2 (s131): the same ADL obligation, but the mark is stale from the
/// settlement block on (the oracle stopped for the delisted market). The
/// settlement closes 14 at the last aggregate and deletes its row; the drain
/// must not wait for a fresh mark that may never come: a governance-delisted
/// market ranks at the obligation's stored price (owner s96, as a removed
/// market), so the next liquidation step closes the escrow against the vault.
#[test]
fn adl_obligation_drains_after_a_stale_price_settlement() {
    let (_d, db) = setup(&[14]);
    fund(&db, 13, 50);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 10, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    set_mark(&db, M, 120, H + 2);
    {
        let mut ctx = ctx_at(db.clone(), H + 2);
        let _ = NativeExecutor::run_liquidations_with(&mut ctx, 2_048, 64, 0);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(pos_of(&db, &liq::ADL_ESCROW_SHORT, M), fp(-10));
    // Last aggregate 120 s of block time before H + 5: stale from here on.
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(M), &agg_row(fp(120), ts(H + 5) - 120)).unwrap();
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(pos(&db, 14, M), FixedPoint::ZERO, "14 closed at the stale 120");
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 200));
    assert_eq!(pos_of(&db, &LIQUIDATOR_VAULT, M), fp(10), "vault mirrors the escrow");
    assert_eq!(row(&db), None);
    {
        let mut ctx = ctx_at(db.clone(), H + 6);
        assert!(NativeExecutor::liquidation_due(&ctx.state).unwrap());
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0), "escrow closed against the vault at its stored price");
    assert!(liq::next_obligation(&db, &[liq::ADL_OBLIGATION_TAG]).unwrap().is_none());
    assert_eq!(bal(&db, &liq::ADL_ESCROW_SHORT).available, FixedPoint::ZERO, "escrow dust swept");
}

/// An underwater account: its close at the settlement price leaves negative
/// collateral; when flat it moves to the vault (D9, conserved), as after a
/// liquidation.
#[test]
fn underwater_close_moves_the_flat_deficit_to_the_vault() {
    let (_d, db) = setup(&[14]);
    fund(&db, 13, 50);
    set_grace(&db, 0);
    open_pair(&db, M, 14, 13, 10, 100);
    set_mark(&db, M, 110, H);
    delist_m(&db);
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    let b13 = bal(&db, &addr(13));
    assert_eq!(b13.available + b13.order_margin, FixedPoint::ZERO, "deficit moved");
    assert_eq!(bal(&db, &LIQUIDATOR_VAULT).available, fp(-50), "vault absorbed -50");
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 100));
}

// ============================================================================
// Node lifecycle (resident books + resident rows R) vs Classic
// ============================================================================

/// One serial node block at `h` (R and resident books of `holder`), `f` run
/// on the context between the oracle step and the save; then the flush with
/// the marker and `end_resident`. Returns what `f` returned and whether the
/// books were reused.
fn node_block<R>(
    db: &StateDb,
    holder: &mut ResidentBooks,
    h: u64,
    mark: i64,
    f: impl FnOnce(&mut NativeExecContext<NativeStateOverlay>) -> R,
) -> (R, bool) {
    let mut overlay = NativeStateOverlay::new(db.clone());
    let mut rb = begin_resident(Some(holder), &mut overlay, h, None);
    assert!(rb.attached());
    // Oracle rows are R rows: written through the block's overlay.
    set_mark(&overlay, M, mark, h);
    set_mark(&overlay, OTHER, 50, h);
    let mut ctx = NativeExecContext::new_with_modes(
        overlay.clone(),
        h,
        ts(h),
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        false,
        Some(holder),
    );
    let reused = ctx.resident_reused();
    ctx.attach_resident_block(&mut rb);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let out = f(&mut ctx);
    assert_eq!(ctx.take_fatal_error(), None, "h {h}");
    ctx.save_order_books();
    ctx.stash_resident(holder);
    ctx.detach_resident_block(&mut rb);
    drop(ctx);
    let delta = overlay.own_pending_delta();
    overlay.flush_with_native_trie_and_marker(db, h).unwrap();
    end_resident(holder, rb, &mut overlay, delta, true, None);
    (out, reused)
}

/// The same block on the Classic path (a fresh context over the DB).
fn classic_block<R>(db: &StateDb, h: u64, f: impl FnOnce(&mut NativeExecContext<StateDb>) -> R) -> R {
    let mut ctx = ctx_at(db.clone(), h);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let out = f(&mut ctx);
    assert_eq!(ctx.take_fatal_error(), None, "h {h}");
    ctx.save_order_books();
    out
}

type CfRows = Vec<(Vec<u8>, Vec<u8>)>;

fn dump(db: &StateDb) -> Vec<(&'static str, CfRows)> {
    [
        CF_NATIVE_POSITIONS,
        CF_NATIVE_BALANCES,
        CF_NATIVE_LIQUIDATION,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_FEE_CONFIG,
        CF_NATIVE_ORACLE,
    ]
    .into_iter()
    .map(|cf| (cf, db.iterate_cf(cf, None).unwrap()))
    .collect()
}

/// What a block does, on either path.
#[derive(Clone)]
enum Step {
    Actions(Vec<(Address, NativeAction)>),
    /// The settlement step with explicit budgets.
    Settle(usize, usize),
    /// The liquidation step, then the tail.
    Liquidate,
}

fn run_step<T: StateBackend>(ctx: &mut NativeExecContext<T>, step: &Step) -> String {
    let out = match step {
        Step::Actions(a) => {
            let r = NativeExecutor::execute_batch_engine_mode(ctx, a, 1).results;
            let (g, s) = gov_tail(ctx);
            (r, g, s)
        }
        Step::Settle(scan, close) => {
            (Vec::new(), Vec::new(), NativeExecutor::run_delist_settlements_with(ctx, *scan, *close))
        }
        Step::Liquidate => {
            let l = NativeExecutor::run_liquidations(ctx);
            let (g, s) = gov_tail(ctx);
            (l, g, s)
        }
    };
    format!("{:?}", [out.0, out.1, out.2].iter().map(|v| v.iter().map(|r| (r.success, r.reason, r.error.clone())).collect::<Vec<_>>()).collect::<Vec<_>>())
}

/// Runs `script` (height, mark of M, step) on both paths from the same
/// seeded DB; asserts equal results block by block and equal final state.
/// `check` runs on the resident path's DB after each block.
fn lifecycle_equals_classic(
    seed: impl Fn(&StateDb),
    script: &[(u64, i64, Step)],
    check: impl Fn(u64, &StateDb),
) {
    let (_d1, classic) = setup(&[]);
    let (_d2, node) = setup(&[]);
    seed(&classic);
    seed(&node);
    let mut holder = ResidentBooks::default();
    let mut reused_at = Vec::new();
    for (h, mark, step) in script {
        set_mark(&classic, M, *mark, *h);
        set_mark(&classic, OTHER, 50, *h);
        let a = classic_block(&classic, *h, |ctx| run_step(ctx, step));
        let (b, reused) = node_block(&node, &mut holder, *h, *mark, |ctx| run_step(ctx, step));
        if reused {
            reused_at.push(*h);
        }
        assert_eq!(a, b, "h {h}: results");
        check(*h, &node);
    }
    // Every block after a predecessor reuses the resident books.
    let want: Vec<u64> = script.windows(2).filter(|w| w[1].0 == w[0].0 + 1).map(|w| w[1].0).collect();
    assert_eq!(reused_at, want, "resident books reused");
    assert_eq!(dump(&classic), dump(&node), "resident lifecycle == Classic");
}

/// Part A in the node lifecycle: the multi-block settlement with resting
/// reduce-only orders placed between settlement blocks.
#[test]
fn settlement_resident_lifecycle_equals_classic() {
    let script = vec![
        (E - 1, 100, Step::Actions(vec![])),
        (E, 100, Step::Actions(vec![])), // the delist executes
        (E + 1, 100, Step::Actions(vec![place(20, ro(limit(M, false, 130, 1)))])),
        (E + 2, 100, Step::Liquidate),
        (E + 5, 110, Step::Settle(3, 2)),
        (E + 6, 120, Step::Actions(vec![place(35, ro(limit(M, true, 90, 2)))])),
        (E + 7, 120, Step::Settle(3, 2)),
        (E + 8, 130, Step::Settle(3, 2)),
        (E + 9, 130, Step::Settle(3, 2)),
        (E + 10, 130, Step::Settle(64, 64)),
    ];
    let seed = |db: &StateDb| {
        set_grace(db, 5);
        seed_pairs(db);
        submit_passed(db, 1, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
    };
    lifecycle_equals_classic(seed, &script, |h, db| {
        if h == E {
            assert_eq!(row(db).map(|r| r.close_at), Some(E + 5), "delist executed at E");
        }
        if h == E + 10 {
            assert_eq!(net_oi(db, M), (FixedPoint::ZERO, 0), "settled");
            assert_eq!(row(db), None);
        }
        if h >= E + 5 {
            assert_eq!(net_oi(db, M).0, FixedPoint::ZERO, "h {h}: OI symmetric");
        }
    });
}

// ============================================================================
// Part B (1): governance cancel with resident books and rows
// ============================================================================

/// Delist of M and a leverage update of OTHER (20x -> 10x) executed in one
/// block with resting orders, pending stops and positions in both markets,
/// through the node lifecycle: every resting order and stop of both books
/// is cancelled in the RESIDENT books (the next block reuses them and finds
/// nothing), reservations are refunded exactly, and the state equals the
/// Classic path byte for byte.
#[test]
fn resident_governance_cancel_equals_classic() {
    let stop = |m| PlaceOrderParams {
        order_type: OrderType::StopLimit { trigger: fp(120), limit: fp(121) },
        ..limit(m, true, 121, 1)
    };
    let seed_orders = vec![
        place(10, limit(M, true, 99, 2)),
        place(11, limit(M, false, 110, 3)),
        place(12, stop(M)),
        place(13, limit(M, false, 100, 1)),
        place(14, limit(M, true, 100, 1)), // 14 long 1, 13 short 1
        place(10, limit(OTHER, true, 40, 1)),
        place(11, limit(OTHER, false, 60, 2)),
        place(12, stop(OTHER)),
    ];
    let script = vec![
        (3, 100, Step::Actions(seed_orders)),
        (4, 100, Step::Actions(vec![])),
        (E - 1, 100, Step::Actions(vec![])),
        (E, 100, Step::Actions(vec![])), // both proposals execute
        (
            E + 1,
            100,
            Step::Actions(vec![
                place(10, limit(M, true, 99, 1)),
                place(14, ro(limit(M, false, 101, 1))),
                place(15, limit(OTHER, true, 45, 1)),
            ]),
        ),
        (E + 2, 100, Step::Actions(vec![])),
    ];
    let seed = |db: &StateDb| {
        for t in 10..=15 {
            fund(db, t, FUND);
        }
        submit_passed(db, 1, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
        submit_passed(
            db,
            2,
            "OTHER 10x",
            ProposalAction::UpdateMarketParams {
                market_id: OTHER,
                params: MarketParams {
                    tick_size: fp(1),
                    lot_size: fp(1),
                    max_leverage: 10,
                    maintenance_margin_bps: 500,
                    max_funding_rate_bps: 100,
                },
            },
        );
    };
    lifecycle_equals_classic(seed, &script, |h, db| {
        if h == E {
            assert_eq!(status(db, 1), ProposalStatus::Executed);
            assert_eq!(status(db, 2), ProposalStatus::Executed);
            let ctx = ctx_at(db.clone(), E + 1);
            for m in [M, OTHER] {
                assert_eq!(ctx.order_books()[&m].order_count(), 0, "book {m} emptied");
                assert!(ctx.order_books()[&m].pending_stops().is_empty(), "stops {m} cancelled");
            }
            drop(ctx);
            for t in [10, 11, 12] {
                let b = bal(db, &addr(t));
                assert_eq!(b.order_margin, FixedPoint::ZERO, "trader {t} refunded");
                assert_eq!(b.available, fp(FUND), "trader {t} refunded in full");
            }
            assert_eq!((pos(db, 14, M), pos(db, 13, M)), (fp(1), fp(-1)), "positions kept");
        }
        if h == E + 1 {
            // Under the delist only the reduce-only ask rests in M; OTHER
            // took a new order at 10x (45 / 10 = 4.5).
            let ctx = ctx_at(db.clone(), E + 2);
            assert_eq!(ctx.order_books()[&M].order_count(), 1);
            assert_eq!(ctx.order_books()[&OTHER].order_count(), 1);
            drop(ctx);
            assert_eq!(bal(db, &addr(15)).order_margin, FixedPoint::from_raw(45 * FixedPoint::SCALE / 10));
        }
    });
}

// ============================================================================
// Part B (2): reduce-only on a delisted market
// ============================================================================

/// Single path and the engine on 1 and 4 workers.
const PATHS: [usize; 3] = [0, 1, 4];

fn run(db: &StateDb, h: u64, path: usize, actions: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    let mut ctx = ctx_at(db.clone(), h);
    let r = if path == 0 {
        actions.iter().map(|(s, a)| NativeExecutor::execute(&mut ctx, s, a)).collect()
    } else {
        NativeExecutor::execute_batch_engine_mode(&mut ctx, actions, path).results
    };
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    r
}

fn resting(db: &StateDb, t: u8) -> Vec<FixedPoint> {
    let ctx = ctx_at(db.clone(), 1);
    ctx.order_books()
        .get(&M)
        .map(|b| b.orders_for_trader(&addr(t)).iter().map(|o| o.remaining_qty).collect())
        .unwrap_or_default()
}

/// Delisted M (default grace: no settlement), 14 long 4 @100 and 13 short 4.
fn delisted_with_positions() -> (tempfile::TempDir, StateDb) {
    let (d, db) = setup(&[13, 14, 15, 16]);
    open_pair(&db, M, 14, 13, 4, 100);
    open_pair(&db, M, 15, 16, 2, 100);
    delist_m(&db);
    (d, db)
}

/// Exposure-increasing orders are rejected (`market_delisted` when not
/// reduce-only, `ReduceOnly` when reduce-only on the increasing side);
/// reduce-only orders are accepted and clamped to the position, resting
/// and taking.
#[test]
fn reduce_only_orders_on_a_delisted_market_clamp_and_increasing_ones_are_rejected() {
    for path in PATHS {
        let (_d, db) = delisted_with_positions();
        let r = run(&db, H + 1, path, &[place(14, limit(M, true, 100, 1)), place(13, limit(M, false, 100, 1))]);
        for x in &r {
            assert_eq!((x.success, x.reason), (false, FailureReason::MarketDelisted), "path {path}: {r:?}");
        }
        let r = run(&db, H + 2, path, &[place(14, ro(limit(M, true, 100, 1)))]);
        if path == 0 {
            assert_eq!(r[0].reason, FailureReason::ReduceOnly, "{r:?}");
        }
        assert_eq!(pos(&db, 14, M), fp(4), "path {path}: long not increased");
        assert!(resting(&db, 14).is_empty(), "path {path}");
        // 13 (short 4) bids 10 reduce-only: clamped to 4 at rest.
        let r = run(&db, H + 3, path, &[place(13, ro(limit(M, true, 100, 10)))]);
        assert!(r[0].success, "path {path}: {r:?}");
        assert_eq!(resting(&db, 13), vec![fp(4)], "path {path}: resting bid clamped");
        // 14 (long 4) sells 6 reduce-only: clamped to 4, both flat.
        let r = run(&db, H + 4, path, &[place(14, ro(limit(M, false, 100, 6)))]);
        assert!(r[0].success, "path {path}: {r:?}");
        assert_eq!((pos(&db, 14, M), pos(&db, 13, M)), (FixedPoint::ZERO, FixedPoint::ZERO), "path {path}");
        assert!(resting(&db, 13).is_empty() && resting(&db, 14).is_empty(), "path {path}");
        for t in [13, 14] {
            assert_eq!(bal(&db, &addr(t)).order_margin, FixedPoint::ZERO, "path {path}: {t}");
        }
        // Flat now: reduce-only from a flat account is rejected too.
        let r = run(&db, H + 5, path, &[place(14, ro(limit(M, false, 100, 1)))]);
        if path == 0 {
            assert_eq!(r[0].reason, FailureReason::ReduceOnly, "{r:?}");
        }
        assert_eq!(pos(&db, 14, M), FixedPoint::ZERO, "path {path}: no short opened");
    }
}

/// Stops on a delisted market: a non-reduce-only stop is rejected
/// (`market_delisted`); a reduce-only stop placed during the grace period
/// rests, fires on a trade through its trigger and closes the position
/// (clamped to it) against the book.
#[test]
fn reduce_only_stop_triggers_on_a_delisted_market() {
    let ro_stop_sell = |trigger: i64, cap: i64, qty: i64| PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(trigger) },
        time_in_force: TimeInForce::IOC,
        reduce_only: true,
        ..limit(M, false, cap, qty)
    };
    for path in PATHS {
        let (_d, db) = delisted_with_positions();
        let plain_stop = PlaceOrderParams { reduce_only: false, ..ro_stop_sell(95, 90, 1) };
        let r = run(&db, H + 1, path, &[place(15, plain_stop)]);
        assert_eq!((r[0].success, r[0].reason), (false, FailureReason::MarketDelisted), "path {path}: {r:?}");
        let r = run(
            &db,
            H + 2,
            path,
            &[
                place(14, ro_stop_sell(95, 90, 6)),
                place(13, ro(limit(M, true, 94, 4))),
                place(16, ro(limit(M, true, 95, 1))),
            ],
        );
        assert!(r.iter().all(|x| x.success), "path {path}: {r:?}");
        let ctx = ctx_at(db.clone(), 1);
        assert_eq!(ctx.order_books()[&M].pending_stop_count(), 1, "path {path}: stop pending");
        drop(ctx);
        // 15 (long 2) sells 1 into 16's bid at 95: the trade fires 14's stop,
        // which sells 4 (clamped from 6) into 13's bid at 94.
        let r = run(&db, H + 3, path, &[place(15, ro(limit(M, false, 95, 1)))]);
        assert!(r[0].success, "path {path}: {r:?}");
        let ctx = ctx_at(db.clone(), 1);
        assert_eq!(ctx.order_books()[&M].pending_stop_count(), 0, "path {path}: stop fired");
        drop(ctx);
        assert_eq!(pos(&db, 14, M), FixedPoint::ZERO, "path {path}: the stop closed the long");
        assert_eq!(pos(&db, 13, M), FixedPoint::ZERO, "path {path}: counterparty");
        assert_eq!((pos(&db, 15, M), pos(&db, 16, M)), (fp(1), fp(-1)), "path {path}");
        // Long 4 @100 closed at 94: -24; nothing reserved.
        let b = bal(&db, &addr(14));
        assert_eq!((b.available, b.order_margin), (fp(FUND - 24), FixedPoint::ZERO), "path {path}");
    }
}

// ============================================================================
// Part C: owner s134 decisions. 1c: a governance close price; 3b: the oracle
// stops for a market whose settlement is complete.
// ============================================================================

fn close_price(m: MarketId, price: i64) -> ProposalAction {
    ProposalAction::SetDelistClosePrice { market_id: m, price: fp(price) }
}

/// Runs one block's tail (governance, then the settlement step) at `h`: the
/// proposals passed with [`submit_passed`] execute here (a tail finalizes an
/// unvoted active proposal as rejected, so each test votes right after it
/// submits). The statuses are left to the caller.
fn execute_at(db: &StateDb, h: u64) -> (Vec<NativeActionResult>, Vec<NativeActionResult>) {
    let mut ctx = ctx_at(db.clone(), h);
    let out = gov_tail(&mut ctx);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None);
    out
}

/// The stored governance close price of `m` (`0x09 ‖ m`), if any.
fn close_row(db: &StateDb, m: MarketId) -> Option<FixedPoint> {
    db.get_cf_raw(CF_NATIVE_LIQUIDATION, &torus_types::delist_close_price_key(m))
        .unwrap()
        .map(|v| FixedPoint::from_raw(i128::from_be_bytes(v[..16].try_into().unwrap())))
}

fn failed_with(gov: &[NativeActionResult], needle: &str) {
    assert!(
        gov.iter().any(|r| !r.success && r.error.as_deref().unwrap_or("").contains(needle)),
        "no failure containing {needle:?}: {gov:?}"
    );
}

/// 1c: a close price set during the grace period is the settlement price,
/// ahead of a fresh mark (110); its row is consumed when the price is fixed.
#[test]
fn governance_close_price_wins_over_the_mark() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    submit_passed(&db, 2, "Close BTC at 130", close_price(M, 130));
    let (gov, _) = execute_at(&db, H + 2);
    assert_eq!(status(&db, 2), ProposalStatus::Executed, "{gov:?}");
    assert_eq!(close_row(&db, M), Some(fp(130)));
    set_mark(&db, M, 110, H + 5);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 60), "long 2 @100 closed at 130");
    assert_eq!(bal(&db, &addr(13)).available, fp(FUND - 60));
    assert_eq!(close_row(&db, M), None, "consumed with the settlement");
    assert_eq!(row(&db), None);
}

/// 1c: the close price may execute in the delist's own block (higher id):
/// with N = 0 it is the price of the same block's settlement, with N = 5 of
/// the settlement at H + 5. A close price with a LOWER id than the delist
/// runs first and fails (not delisted yet).
#[test]
fn governance_close_price_in_the_delist_block() {
    for grace in [0, 5] {
        let (_d, db) = setup(&[13, 14]);
        set_grace(&db, grace);
        open_pair(&db, M, 14, 13, 2, 100);
        set_mark(&db, M, 100, H);
        submit(&db, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
        submit(&db, "Close BTC at 80", close_price(M, 80));
        pass_and_execute(&db, &[1, 2]);
        if grace == 5 {
            assert_eq!(close_row(&db, M), Some(fp(80)), "stored for the settlement");
            set_mark(&db, M, 110, H + 5);
            block(&db, H + 5, 1, &[]);
        }
        assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0), "grace {grace}");
        assert_eq!(bal(&db, &addr(14)).available, fp(FUND - 40), "grace {grace}: closed at 80");
        assert_eq!(bal(&db, &addr(13)).available, fp(FUND + 40), "grace {grace}");
        assert_eq!(close_row(&db, M), None, "grace {grace}");
    }
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    submit_passed(&db, 1, "Close BTC at 80", close_price(M, 80));
    submit_passed(&db, 2, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
    let (gov, _) = execute_at(&db, H);
    assert_eq!(status(&db, 1), ProposalStatus::Failed, "{gov:?}");
    assert_eq!(status(&db, 2), ProposalStatus::Executed, "{gov:?}");
    failed_with(&gov, "is not delisted");
    assert_eq!(close_row(&db, M), None);
}

/// 1c: a close price executing AT `close_at` runs before that block's
/// settlement step (governance first) and is its price.
#[test]
fn governance_close_price_at_close_at_is_used() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    submit_passed(&db, 2, "Close BTC at 125", close_price(M, 125));
    set_mark(&db, M, 110, H + 5);
    let (gov, s) = execute_at(&db, H + 5);
    assert_eq!(status(&db, 2), ProposalStatus::Executed, "{gov:?}");
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 50), "closed at 125");
    assert_eq!(close_row(&db, M), None);
}

/// 1c: a later close price replaces an earlier one (last executed wins).
#[test]
fn later_governance_close_price_replaces_the_earlier_one() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    submit_passed(&db, 2, "Close BTC at 130", close_price(M, 130));
    execute_at(&db, H + 1);
    submit_passed(&db, 3, "Close BTC at 140", close_price(M, 140));
    execute_at(&db, H + 2);
    assert_eq!((status(&db, 2), status(&db, 3)), (ProposalStatus::Executed, ProposalStatus::Executed));
    assert_eq!(close_row(&db, M), Some(fp(140)));
    set_mark(&db, M, 110, H + 5);
    block(&db, H + 5, 1, &[]);
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND + 80), "closed at 140");
}

/// 1c: once the settlement price is fixed (first settlement block, 110) a
/// close price arriving later fails as too late and changes nothing: the
/// walk continues at 110 to the end.
#[test]
fn governance_close_price_after_the_price_is_fixed_fails() {
    let (_d, db) = setup(&[]);
    set_grace(&db, 5);
    seed_pairs(&db);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    submit_passed(&db, 2, "Close BTC at 150", close_price(M, 150));
    set_mark(&db, M, 110, H + 5);
    {
        let mut ctx = ctx_at(db.clone(), H + 5);
        NativeExecutor::run_delist_settlements_with(&mut ctx, 3, 2);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(row(&db).and_then(|r| r.price), Some(fp(110)), "fixed, mid-walk");
    // Governance only (the settlement step would finish the walk here).
    let gov = {
        let mut ctx = ctx_at(db.clone(), H + 6);
        let gov = NativeExecutor::process_governance(&mut ctx);
        assert_eq!(ctx.take_fatal_error(), None);
        gov
    };
    assert_eq!(status(&db, 2), ProposalStatus::Failed, "{gov:?}");
    failed_with(&gov, "too late");
    assert_eq!(close_row(&db, M), None, "nothing written");
    assert_eq!(row(&db).and_then(|r| r.price), Some(fp(110)), "price unchanged mid-walk");
    block(&db, H + 7, 1, &[]);
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    for i in 0..PAIRS {
        let pnl = 10 * (i64::from(i) + 1);
        assert_eq!(bal(&db, &addr(20 + 2 * i)).available, fp(FUND + pnl), "long {i} at 110");
    }
}

/// 1c (owner rule: before or at `close_at`): a market still waiting for a
/// price after `close_at` cannot get one from governance; the first oracle
/// price settles it.
#[test]
fn governance_close_price_after_close_at_fails_even_while_waiting() {
    let (_d, db) = setup(&[13, 14]);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 2, 100);
    delist_m(&db);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(!s[0].success, "waiting: {s:?}");
    submit_passed(&db, 2, "Close BTC at 130", close_price(M, 130));
    let (gov, _) = execute_at(&db, H + 6);
    assert_eq!(status(&db, 2), ProposalStatus::Failed, "{gov:?}");
    failed_with(&gov, "too late");
    assert_eq!(close_row(&db, M), None);
    set_mark(&db, M, 95, H + 7);
    block(&db, H + 7, 1, &[]);
    assert_eq!(bal(&db, &addr(14)).available, fp(FUND - 10), "closed at the oracle 95");
}

/// 1c: submission rejects a price <= 0 and a market without a row; at
/// execution a market that is not delisted, or already settled, fails with
/// nothing written.
#[test]
fn governance_close_price_rejections() {
    let (_d, db) = setup(&[13, 14]);
    for (action, needle) in [
        (close_price(M, 0), "close price must be > 0"),
        (close_price(M, -5), "close price must be > 0"),
        (close_price(9, 100), "market 9 is not listed"),
    ] {
        let mut ctx = ctx_at(db.clone(), 3);
        let r = NativeExecutor::execute(
            &mut ctx,
            &addr(PROPOSER),
            &NativeAction::SubmitProposal(Proposal { title: "t".into(), description: "d".into(), action }),
        );
        assert!(!r.success, "{needle}: {r:?}");
        assert!(r.error.as_deref().unwrap_or("").contains(needle), "{needle}: {r:?}");
    }
    // OTHER is listed, never delisted.
    submit_passed(&db, 1, "Close ETH", close_price(OTHER, 50));
    let (gov, _) = execute_at(&db, H);
    assert_eq!(status(&db, 1), ProposalStatus::Failed, "{gov:?}");
    failed_with(&gov, "market 2 is not delisted");
    assert_eq!(close_row(&db, OTHER), None);
    // M settled at once (N = 0); a close price after that is too late.
    set_grace(&db, 0);
    open_pair(&db, M, 14, 13, 1, 100);
    set_mark(&db, M, 100, H + 1);
    submit_passed(&db, 2, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
    execute_at(&db, H + 1);
    assert_eq!(status(&db, 2), ProposalStatus::Executed);
    assert_eq!(row(&db), None, "settled");
    submit_passed(&db, 3, "Close BTC", close_price(M, 120));
    let (gov, _) = execute_at(&db, H + 2);
    assert_eq!(status(&db, 3), ProposalStatus::Failed, "{gov:?}");
    failed_with(&gov, "too late");
    assert_eq!(close_row(&db, M), None);
}

// ---------------------------------------------------------------------------
// 3b: oracle submissions after the settlement is complete
// ---------------------------------------------------------------------------

/// The oracle reporters: validator 1 and validators 5, 6 (the aggregation
/// needs 3 reporters), all Active.
const REPORTERS: [u8; 3] = [1, 5, 6];

fn activate_validator(db: &StateDb) {
    let sm = torus_economics::StakingManager::new(db.clone());
    let mut v = sm.get_validator(&addr(1)).unwrap().unwrap();
    v.status = torus_economics::ValidatorStatus::Active;
    sm.put_validator(&addr(1), &v).unwrap();
    for n in [5, 6] {
        let mut w = v.clone();
        w.address = addr(n);
        w.pubkey = [n; 32];
        sm.put_validator(&addr(n), &w).unwrap();
    }
}

/// One submission per reporter, sampled at block `h`'s time.
fn oracle_sub(h: u64, prices: &[(MarketId, i64)]) -> Vec<(Address, NativeAction)> {
    REPORTERS
        .iter()
        .map(|&n| {
            (
                addr(n),
                NativeAction::SubmitOraclePrices(OracleSubmission {
                    prices: prices.iter().map(|&(m, p)| (m, fp(p))).collect(),
                    timestamp: ts(h) * 1_000,
                }),
            )
        })
        .collect()
}

fn all_ok(r: &[NativeActionResult]) -> bool {
    r.len() == REPORTERS.len() && r.iter().all(|x| x.success)
}

/// Validator 1's stored submission row for `m` (`"sub" ‖ m ‖ validator`).
fn sub_row(db: &StateDb, m: MarketId) -> Option<Vec<u8>> {
    let k = [b"sub".as_slice(), &m.to_be_bytes(), addr(1).as_slice()].concat();
    db.get_cf_raw(CF_NATIVE_ORACLE, &k).unwrap()
}

fn agg_of(db: &StateDb, m: MarketId) -> Option<Vec<u8>> {
    db.get_cf_raw(CF_NATIVE_ORACLE, &agg_key(m)).unwrap()
}

/// A node block on the Classic path: the oracle step, `actions` (engine on
/// `threads` workers, 0 = single path), the liquidation step, governance and
/// the settlement step.
fn full_block(db: &StateDb, h: u64, threads: usize, actions: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    let mut ctx = ctx_at(db.clone(), h);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let r = if threads == 0 {
        actions.iter().map(|(s, a)| NativeExecutor::execute(&mut ctx, s, a)).collect()
    } else {
        NativeExecutor::execute_batch_engine_mode(&mut ctx, actions, threads).results
    };
    let _ = NativeExecutor::run_liquidations(&mut ctx);
    let _ = gov_tail(&mut ctx);
    ctx.save_order_books();
    assert_eq!(ctx.take_fatal_error(), None, "h {h}");
    r
}

/// 3b: during the grace period a delisted market's submissions are stored
/// and aggregated; from the block after its settlement completed, its
/// entries are ignored (not stored; the other markets' entries are, the
/// action succeeds) and it is no longer aggregated. Same on the single path
/// and the engine on 1 and 4 workers.
#[test]
fn oracle_stops_for_a_settled_market_only() {
    for path in PATHS {
        let (_d, db) = setup(&[13, 14]);
        activate_validator(&db);
        set_grace(&db, 5);
        open_pair(&db, M, 14, 13, 2, 100);
        set_mark(&db, M, 100, H);
        set_mark(&db, OTHER, 50, H);
        delist_m(&db);
        // Grace: stored, then aggregated in the next block.
        let r = full_block(&db, H + 1, path, &oracle_sub(H + 1, &[(M, 101), (OTHER, 51)]));
        assert!(all_ok(&r), "path {path}: {r:?}");
        assert!(sub_row(&db, M).is_some(), "path {path}: grace submission stored");
        full_block(&db, H + 2, path, &[]);
        assert_eq!(&agg_of(&db, M).unwrap()[..16], fp(101).raw().to_be_bytes(), "path {path}: aggregated");
        // Settlement block: the submission still lands (row present until
        // the tail), the settlement completes in the tail.
        set_mark(&db, M, 110, H + 5);
        let r = full_block(&db, H + 5, path, &oracle_sub(H + 5, &[(M, 105), (OTHER, 55)]));
        assert!(all_ok(&r), "path {path}: {r:?}");
        assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0), "path {path}: settled");
        let (sub_m, agg_m) = (sub_row(&db, M), agg_of(&db, M));
        // Next block: M not aggregated (its H + 5 submission ignored), OTHER is.
        let r = full_block(&db, H + 6, path, &oracle_sub(H + 6, &[(M, 106), (OTHER, 56)]));
        assert!(all_ok(&r), "path {path}: the action succeeds: {r:?}");
        assert_eq!(agg_of(&db, M), agg_m, "path {path}: settled market not aggregated");
        assert_eq!(&agg_of(&db, OTHER).unwrap()[..16], fp(55).raw().to_be_bytes(), "path {path}");
        assert_eq!(sub_row(&db, M), sub_m, "path {path}: settled entry ignored");
        // Only the settled market: success, nothing written.
        let r = full_block(&db, H + 7, path, &oracle_sub(H + 7, &[(M, 107)]));
        assert!(all_ok(&r), "path {path}: {r:?}");
        assert_eq!(sub_row(&db, M), sub_m, "path {path}");
        full_block(&db, H + 8, path, &[]);
        assert_eq!(agg_of(&db, M), agg_m, "path {path}");
        assert_eq!(&agg_of(&db, OTHER).unwrap()[..16], fp(56).raw().to_be_bytes(), "path {path}");
    }
}

/// 3b: a settlement walk that ends with an ADL obligation open is not
/// complete: the market keeps its oracle until the drain closes the escrow
/// against the vault (at the stored price, no mark needed); from then on its
/// entries are ignored.
#[test]
fn oracle_stops_only_after_the_adl_drain_of_a_delisted_market() {
    let (_d, db) = setup(&[14]);
    activate_validator(&db);
    fund(&db, 13, 50);
    set_grace(&db, 5);
    open_pair(&db, M, 14, 13, 10, 100);
    set_mark(&db, M, 100, H);
    delist_m(&db);
    set_mark(&db, M, 120, H + 2);
    {
        let mut ctx = ctx_at(db.clone(), H + 2);
        let _ = NativeExecutor::run_liquidations_with(&mut ctx, 2_048, 64, 0);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(pos_of(&db, &liq::ADL_ESCROW_SHORT, M), fp(-10));
    set_mark(&db, M, 120, H + 5);
    let (_, s) = block(&db, H + 5, 1, &[]);
    assert!(s.iter().all(|r| r.success), "{s:?}");
    assert_eq!(row(&db), None, "walk done");
    assert_eq!(pos_of(&db, &LIQUIDATOR_VAULT, M), fp(10), "vault mirrors the escrow");
    // Not complete: accepted.
    let r = run(&db, H + 6, 0, &oracle_sub(H + 6, &[(M, 121)]));
    assert!(all_ok(&r), "{r:?}");
    let sub = sub_row(&db, M);
    assert!(sub.is_some(), "stored while the obligation is open");
    // The drain closes the escrow against the vault.
    {
        let mut ctx = ctx_at(db.clone(), H + 7);
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        assert_eq!(ctx.take_fatal_error(), None);
    }
    assert_eq!(net_oi(&db, M), (FixedPoint::ZERO, 0));
    assert!(liq::next_obligation(&db, &[liq::ADL_OBLIGATION_TAG]).unwrap().is_none());
    let r = run(&db, H + 8, 0, &oracle_sub(H + 8, &[(M, 122)]));
    assert!(all_ok(&r), "{r:?}");
    assert_eq!(sub_row(&db, M), sub, "complete now: ignored");
}

/// 3b on every node: the resident node lifecycle (R + resident books) equals
/// the Classic path, oracle rows included, through grace, settlement and
/// ignored submissions.
#[test]
fn oracle_stop_resident_lifecycle_equals_classic() {
    let sub = |h: u64| Step::Actions(oracle_sub(h, &[(M, 100 + (h - E) as i64), (OTHER, 50)]));
    let script = vec![
        (E - 1, 100, Step::Actions(vec![])),
        (E, 100, Step::Actions(vec![])), // the delist executes
        (E + 1, 100, sub(E + 1)),
        (E + 2, 100, sub(E + 2)),
        (E + 5, 110, sub(E + 5)), // settles in the tail
        (E + 6, 110, sub(E + 6)),
        (E + 7, 110, sub(E + 7)),
    ];
    let seed = |db: &StateDb| {
        activate_validator(db);
        set_grace(db, 5);
        seed_pairs(db);
        submit_passed(db, 1, "Delist BTC", ProposalAction::DelistMarket { market_id: M });
    };
    lifecycle_equals_classic(seed, &script, |h, db| {
        if h == E + 2 {
            assert!(sub_row(db, M).is_some(), "grace: stored");
        }
        if h >= E + 6 {
            assert_eq!(net_oi(db, M), (FixedPoint::ZERO, 0), "settled");
            // The stored sample is still the one of E + 5 (ignored since).
            let stored = ctx_at(db.clone(), 1).oracle.stored_sample_ms(M, &addr(1)).unwrap();
            assert_eq!(stored, Some(ts(E + 5) * 1_000), "h {h}: M's submission row is the E + 5 one");
        }
    });
}
