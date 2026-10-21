//! Codex P1 (pre-testnet review): a governance params update changes a
//! market's margin config in the middle of a block (the governance step,
//! after the orders and the liquidation step). The C2 / C3 rule: the mark
//! version (which keys the sums cache) changes whenever the table or the
//! margin configs the cached valuations were made with change. The update
//! used to leave the block's version as it was, and the block's configs
//! (already the new ones) went to the slot, so the next block reused the
//! version and served sums valued under the OLD config: a continuous
//! validator accepted an order a restarted one (empty slot, new version,
//! fresh valuation) rejected.
//!
//! The chain runs through the node's lifecycle (R and the slot attached,
//! pipelined overlay over the previous block's frozen set). At the block
//! after the update the same order is executed twice on the same state: by
//! the continuous validator (slot reused) and by a restarted one (new
//! holder); both must decide alike, and the version must have moved.

use super::*;
use alloy_primitives::U256;
use revm::state::AccountInfo;
use torus_core::position::Position;
use torus_economics::governance::GovernanceParams;
use torus_economics::MIN_SELF_DELEGATION;
use torus_state::cf::{
    CF_CONSENSUS_META, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::FrozenPending;
use torus_types::{MarketParams, Proposal, ProposalAction};

const M: MarketId = 1;
const OTHER: MarketId = 2;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

/// A listed market row: tick 1, lot 1, initial margin 5% (20x).
fn market_row() -> Vec<u8> {
    let s = FixedPoint::SCALE;
    borsh::to_vec(&("BTC".to_string(), "USD".to_string(), s, s, 5 * s)).unwrap()
}

fn agg_key(m: MarketId) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8).
fn agg_row(price: FixedPoint, ts: u64) -> Vec<u8> {
    [
        price.raw().to_be_bytes().as_slice(),
        &7u64.to_be_bytes(),
        &3u32.to_be_bytes(),
        &ts.to_be_bytes(),
    ]
    .concat()
}

const TRADER: u8 = 10;
const PROPOSER: u8 = 2;
const T0: u64 = 10_000;

fn ctx_on<T: StateBackend>(state: T, h: u64) -> NativeExecContext<T> {
    NativeExecContext::new(state, h, T0 + h, 0, 100, 10, addr(99), addr(100), addr(101))
}

fn limit(m: MarketId, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy: true,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Markets 1 and 2 listed at 20x with a fresh mark of 100; the trader holds
/// long 10 @ 100 in market 1 (IM 50 at 20x, 100 at 10x) and 80 available;
/// an UpdateMarketParams proposal for market 1 (20x -> 10x, tick and lot
/// unchanged) passed, executable from height 114.
fn setup(db: &StateDb) {
    for m in [M, OTHER] {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row())
            .unwrap();
        db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(fp(100), T0 + 140))
            .unwrap();
    }
    let ctx = ctx_on(db.clone(), 1);
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
    db.put_account(
        &val,
        &AccountInfo {
            balance: MIN_SELF_DELEGATION,
            ..Default::default()
        },
    )
    .unwrap();
    ctx.staking
        .register_validator(val, [1; 32], 500, MIN_SELF_DELEGATION)
        .unwrap();
    db.put_account(
        &addr(PROPOSER),
        &AccountInfo {
            balance: wei(500),
            ..Default::default()
        },
    )
    .unwrap();
    ctx.staking.delegate(addr(PROPOSER), val, wei(500)).unwrap();
    let t = addr(TRADER);
    ctx.positions
        .put_native_balance(
            &t,
            &NativeBalance {
                available: fp(80),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    ctx.positions
        .put_position(&Position {
            trader: t,
            market_id: M,
            is_long: true,
            size: fp(10),
            entry_price: fp(100),
            cost_basis: fp(1_000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();
    drop(ctx);
    let mut ctx = ctx_on(db.clone(), 3);
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(PROPOSER),
        &NativeAction::SubmitProposal(Proposal {
            title: "BTC 10x".into(),
            description: "d".into(),
            action: ProposalAction::UpdateMarketParams {
                market_id: M,
                params: MarketParams {
                    tick_size: fp(1),
                    lot_size: fp(1),
                    max_leverage: 10,
                    maintenance_margin_bps: 500,
                    max_funding_rate_bps: 100,
                },
            },
        }),
    );
    assert!(r.success, "{r:?}");
    ctx.governance
        .cast_vote(addr(PROPOSER), 1, true, 10)
        .unwrap();
    ctx.governance.finalize_proposal(1, 104).unwrap();
}

/// What one block decided, and the version its mark table had at the start
/// and at the end.
struct Out {
    results: Vec<NativeActionResult>,
    start_version: Option<u64>,
    end_version: Option<u64>,
}

/// One native block at `h` through the node's sequence (oracle step, the
/// batch, liquidations, governance), R and the slot of `holder` attached.
/// Returns the overlay (not yet frozen) and the block's handle.
fn exec_block(
    db: &StateDb,
    holder: &mut ResidentBooks,
    parent: &Option<Arc<FrozenPending>>,
    h: u64,
    actions: &[(Address, NativeAction)],
) -> (NativeStateOverlay, ResidentBlock, Out) {
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
    let mut rb = begin_resident(Some(holder), &mut overlay, h, None);
    assert!(rb.attached());
    let mut ctx = ctx_on(overlay.clone(), h);
    ctx.attach_resident_block(&mut rb);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let start_version = ctx.mark_version();
    let results = NativeExecutor::execute_batch_engine_mode(&mut ctx, actions, 0).results;
    let _ = NativeExecutor::run_liquidations(&mut ctx);
    let gov = NativeExecutor::process_governance(&mut ctx);
    assert!(gov.iter().all(|r| r.success), "{gov:?}");
    assert_eq!(ctx.take_fatal_error(), None);
    let end_version = ctx.mark_version();
    ctx.save_order_books();
    ctx.detach_resident_block(&mut rb);
    drop(ctx);
    overlay
        .put_cf_raw(
            CF_CONSENSUS_META,
            META_NATIVE_APPLIED_HEIGHT,
            &h.to_be_bytes(),
        )
        .unwrap();
    (
        overlay,
        rb,
        Out {
            results,
            start_version,
            end_version,
        },
    )
}

/// [`exec_block`], then the block is frozen, handed to `end_resident` and
/// the previous frozen set flushed (the next block reads this one through
/// its parent layer).
fn run_block(
    db: &StateDb,
    holder: &mut ResidentBooks,
    parent: &mut Option<Arc<FrozenPending>>,
    h: u64,
    actions: &[(Address, NativeAction)],
) -> Out {
    let (mut overlay, rb, out) = exec_block(db, holder, parent, h, actions);
    let delta = overlay.own_pending_delta();
    let frozen = overlay.freeze(h);
    end_resident(holder, rb, &mut overlay, delta, true, None);
    if let Some(p) = parent.take() {
        p.flush_with_native_trie_stats(db, None, None, None)
            .expect("flush");
    }
    *parent = Some(frozen);
    out
}

#[test]
fn margin_config_update_moves_the_mark_version_resident_equals_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    setup(&db);
    let mut holder = ResidentBooks::default();
    let mut parent = None;
    let order = |price: i64, qty: i64| {
        [(
            addr(TRADER),
            NativeAction::PlaceOrder(limit(OTHER, price, qty)),
        )]
    };

    // Warm-up: the slot holds the trader's sums at a stable version.
    run_block(&db, &mut holder, &mut parent, 110, &[]);
    let warm = run_block(&db, &mut holder, &mut parent, 111, &[]);
    // 112: the trader is valued (an order: free 80 - 50 = 30 >= 2.5);
    // nothing executes yet.
    let b = run_block(&db, &mut holder, &mut parent, 112, &order(50, 1));
    assert!(b.results[0].success, "{:?}", b.results);
    assert_eq!(
        b.start_version, warm.start_version,
        "stable marks and configs: version held"
    );
    run_block(&db, &mut holder, &mut parent, 113, &[]);
    // 114: valued again, then the governance step applies 20x -> 10x.
    let b = run_block(&db, &mut holder, &mut parent, 114, &order(50, 1));
    assert!(b.results[0].success, "{:?}", b.results);
    {
        let ctx = ctx_on(
            NativeStateOverlay::with_parent(db.clone(), parent.clone()),
            115,
        );
        assert_eq!(
            ctx.margin_configs[&M].max_leverage, 10,
            "the update executed"
        );
    }

    // 115: notional 200 needs 10. At 10x the trader's free margin is
    // 75 - 100 < 0 (rejected); at the stale 20x it was 75 - 50 = 25.
    let mut restarted = ResidentBooks::default();
    let (_o, _rb, fresh) = exec_block(&db, &mut restarted, &parent, 115, &order(100, 2));
    let (_o, _rb, resident) = exec_block(&db, &mut holder, &parent, 115, &order(100, 2));
    assert_eq!(
        fresh.results[0].reason,
        FailureReason::Margin,
        "restarted: {:?}",
        fresh.results
    );
    assert_eq!(
        (resident.results[0].success, resident.results[0].reason),
        (fresh.results[0].success, fresh.results[0].reason),
        "continuous and restarted validators decide alike: {:?} vs {:?}",
        resident.results,
        fresh.results
    );
    assert_ne!(
        b.end_version, b.start_version,
        "a margin config change in the block moves the version (C2 / C3)"
    );
    assert_ne!(
        resident.start_version, b.start_version,
        "the pre-update version is not reused"
    );
}
