//! s87 crab perf fixes (`docs/plans/crab-perf-fixes-s87.md`, Task 0.2): golden
//! per-block digests pinned on `c93c579`, before any of fixes 1-3. The fixes
//! only change HOW MUCH work execution does, never what it writes or
//! returns, so these digests must stay identical after every fix commit.
//! Option B (s87) changes outcomes on purpose: it re-pinned scenario A only
//! (scenario B — GTC bids and liquidation orders — is unchanged).
//!
//! Each block runs like the node's pipelined exec path (and `ubench_econ`):
//! a `NativeStateOverlay` over the previous block's frozen set, then
//! `freeze` + `flush` with the running state hash on. Item 6 Phase 1 (C1):
//! the resident rows R are attached exactly as app.rs does
//! (`begin_resident` / `end_resident`, and item 6 step 2's
//! `end_resident_on_worker`); every scenario also runs without R
//! (`begin_resident(None, ..)`, today's path) against the same digests. A block's digest =
//! keccak256 over the DB after its flush (every `HASHED_CFS` CF, the native
//! state root, the running hash — which also covers tombstones) ‖ its
//! results ‖ funnel metrics ‖ `trade_index` ‖ `fatal_error`.
//!
//! Re-capture (only when behaviour changes ON PURPOSE, e.g. Fix 4):
//!   GOLDEN_PRINT=1 cargo test -p torus-bridge --test perf_equivalence_golden -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::{keccak256, Address, U256};
use torus_bridge::native_executor::{
    begin_resident, end_resident, end_resident_on_worker, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::liquidation as liq;
use torus_core::order_book::OrderBook;
use torus_core::position::{MarginType, NativeBalance};
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{
    CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS,
    META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::running_hash::{configure_activation, read_running_hash, HASHED_CFS};
use torus_state::{FrozenPending, NativeStateOverlay, StateBackend, StateDb};
use torus_telemetry::Metrics;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// A result's fields as the digests pin them: the four the struct had at
/// pin time, `Debug`-formatted exactly as then (same type name, same field
/// order). The typed `reason` (v2 action status) is covered by
/// `action_reason_tests`.
#[derive(Debug)]
#[allow(dead_code)] // read through `Debug` only
struct NativeActionResult {
    action_type: &'static str,
    success: bool,
    error: Option<String>,
    gas_used: u64,
}

fn pinned(results: &[torus_bridge::native_executor::NativeActionResult]) -> Vec<NativeActionResult> {
    results
        .iter()
        .map(|r| NativeActionResult {
            action_type: r.action_type,
            success: r.success,
            error: r.error.clone(),
            gas_used: r.gas_used,
        })
        .collect()
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn sender(i: u64) -> Address {
    let mut b = [0xA7u8; 20];
    b[12..20].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

const REPORTERS: [u8; 3] = [150, 151, 152];

/// Three Active validators (the mark reporters) and `markets` listed.
fn listed_db(markets: &[MarketId]) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    configure_activation(&db, Some(1)).unwrap();
    for n in REPORTERS {
        StakingManager::new(db.clone())
            .put_validator(
                &addr(n),
                &ValidatorState {
                    address: addr(n),
                    pubkey: [n; 32],
                    commission_bps: 0,
                    self_stake: MIN_SELF_DELEGATION,
                    total_delegated: U256::ZERO,
                    status: ValidatorStatus::Active,
                    jailed_until: None,
                    last_commission_change_block: None,
                    oracle_signer: None,
                },
            )
            .unwrap();
    }
    for m in markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    (dir, db)
}

fn seed_ctx(db: &StateDb) -> NativeExecContext {
    NativeExecContext::new(db.clone(), 0, 1_000, 0, 1_000_000, 100, addr(99), addr(100), addr(101))
}

fn fund(ctx: &NativeExecContext, t: &Address, amount: i64) {
    ctx.positions
        .put_native_balance(t, &NativeBalance { available: fp(amount), order_margin: FixedPoint::ZERO })
        .unwrap();
}

/// One block of a scenario: header time, the marks the reporters submit
/// (whole units), liquidation rows written at block start (`(tag, trader)`:
/// pending / cooldown), the actions, and the liquidation budgets (`None` =
/// default).
struct Block {
    ts: u64,
    marks: Vec<(MarketId, i64)>,
    rows: Vec<(u8, Address)>,
    actions: Vec<(Address, NativeAction)>,
    liq: Option<(usize, usize)>,
}

/// keccak256 over every hashed CF (DB rows), the native state root and the
/// running hash — the DB right after a block's flush.
fn db_digest(db: &StateDb) -> Vec<u8> {
    let mut data = Vec::new();
    for (id, cf) in HASHED_CFS {
        for (k, v) in db.iterate_cf(cf, None).unwrap() {
            data.push(*id);
            data.extend_from_slice(&(k.len() as u32).to_be_bytes());
            data.extend_from_slice(&k);
            data.extend_from_slice(&(v.len() as u32).to_be_bytes());
            data.extend_from_slice(&v);
        }
    }
    data.extend_from_slice(compute_native_state_root(db).unwrap().as_slice());
    data.extend_from_slice(format!("{:?}", read_running_hash(db)).as_bytes());
    data
}

/// adl-budget re-pin evidence (18c review): [`db_digest`] without what rule H
/// and P2 change on purpose — `0x03` values cut to `last` (16 bytes), no
/// `0x07` row, no ADL-escrow position / balance row — and without what
/// covers those rows: the native state root, `CF_CONSENSUS_META` (the
/// running hash and the native trie's nodes) and the running hash.
fn reduced_digest(db: &StateDb) -> Vec<u8> {
    let escrow = |k: &[u8]| [liq::ADL_ESCROW_LONG, liq::ADL_ESCROW_SHORT].iter().any(|e| k.starts_with(e.as_slice()));
    let mut data = Vec::new();
    for (id, cf) in HASHED_CFS {
        if *cf == CF_CONSENSUS_META {
            continue;
        }
        for (k, mut v) in db.iterate_cf(cf, None).unwrap() {
            if *cf == CF_NATIVE_LIQUIDATION {
                match k.first() {
                    Some(&liq::ADL_OBLIGATION_TAG) => continue,
                    Some(&liq::PREV_MARK_TAG) => v.truncate(16),
                    _ => {}
                }
            } else if (*cf == CF_NATIVE_POSITIONS || *cf == CF_NATIVE_BALANCES) && escrow(&k) {
                continue;
            }
            data.push(*id);
            data.extend_from_slice(&(k.len() as u32).to_be_bytes());
            data.extend_from_slice(&k);
            data.extend_from_slice(&(v.len() as u32).to_be_bytes());
            data.extend_from_slice(&v);
        }
    }
    data
}

/// How a run keeps the resident rows R.
#[derive(Clone, Copy, PartialEq, Debug)]
enum R {
    /// Today's path (`begin_resident(None, ..)`).
    Off,
    /// R attached, `end_resident` inline.
    Inline,
    /// Item 6 step 2 (app.rs): `end_resident_on_worker`, joined by the next
    /// block's `begin_resident`.
    Worker,
}

const R_MODES: [R; 3] = [R::Inline, R::Worker, R::Off];

/// Runs `blocks` on the pipelined overlay path; `threads` = `None` runs
/// `execute_batch`, `Some(t)` `execute_batch_engine_mode(.., t)`; `r`: how
/// the resident rows R are kept. Returns one hex digest per block.
fn run(db: &StateDb, blocks: &[Block], threads: Option<usize>, r: R) -> Vec<String> {
    run_both(db, blocks, threads, r).0
}

/// [`run`], also returning each block's [`reduced_digest`] (+ its outputs).
fn run_both(db: &StateDb, blocks: &[Block], threads: Option<usize>, r: R) -> (Vec<String>, Vec<String>) {
    let resident = r != R::Off;
    torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
    let mut holder = ResidentBooks::default();
    let metrics = Arc::new(Metrics::new());
    let mut books: HashMap<MarketId, OrderBook> = HashMap::new();
    let mut next_id: u128 = 1;
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut outputs: Vec<String> = Vec::new();
    let mut digests: Vec<(String, String)> = Vec::new();
    let flush = |p: Arc<FrozenPending>, outputs: &[String], digests: &mut Vec<(String, String)>| {
        let h = p.height();
        p.flush_with_native_trie_stats(db, Some(h), None, None).expect("flush");
        let out = outputs[h as usize - 1].as_bytes();
        let (mut data, mut reduced) = (db_digest(db), reduced_digest(db));
        data.extend_from_slice(out);
        reduced.extend_from_slice(out);
        digests.push((keccak256(&data).to_string(), keccak256(&reduced).to_string()));
    };
    for (i, b) in blocks.iter().enumerate() {
        let h = i as u64 + 1;
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rows = begin_resident(resident.then_some(&mut holder), &mut overlay, h, Some(&metrics));
        let mut ctx = NativeExecContext::new(
            overlay.clone(), h, b.ts, 0, 1_000_000, 100, addr(99), addr(100), addr(101),
        );
        ctx.attach_resident_block(&mut rows);
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        ctx.metrics = Some(metrics.clone());
        for &(m, units) in &b.marks {
            for n in REPORTERS {
                ctx.oracle.submit_price(&addr(n), m, fp(units), h, b.ts).unwrap();
            }
        }
        for &(tag, t) in &b.rows {
            match tag {
                liq::PENDING_TAG => {
                    liq::set_pending(&ctx.state, &t, true).unwrap();
                }
                _ => liq::set_cooldown(&ctx.state, &t, b.ts - 5).unwrap(),
            }
        }
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        let res = match threads {
            None => NativeExecutor::execute_batch(&mut ctx, &b.actions),
            Some(t) => NativeExecutor::execute_batch_engine_mode(&mut ctx, &b.actions, t),
        };
        let liq_res = match b.liq {
            None => NativeExecutor::run_liquidations(&mut ctx),
            Some((scan, act)) => NativeExecutor::run_liquidations_with(&mut ctx, scan, act, liq::ADL_WORK_PER_BLOCK),
        };
        ctx.save_order_books();
        let (agg, liq_res) = (pinned(&agg), pinned(&liq_res));
        outputs.push(format!(
            "agg={agg:?}|res={:?}|gas={}|liq={liq_res:?}|trades={}|next_id={}|fatal={:?}|acc={} rc={} rm={} ol={} rb={} cpf={} stp={} oth={} liqs={}",
            pinned(&res.results),
            res.total_gas,
            ctx.trade_index,
            ctx.next_global_order_id,
            ctx.fatal_error,
            metrics.orders_placed_accepted.get(),
            metrics.orders_rejected_cancelled.get(),
            metrics.orders_rejected_margin.get(),
            metrics.orders_rejected_open_limit.get(),
            metrics.orders_rejected_book.get(),
            metrics.orders_cancelled_partial_fill.get(),
            metrics.orders_self_trade_cancels.get(),
            metrics.orders_rejected_other.get(),
            metrics.liquidations_triggered.get(),
        ));
        assert!(ctx.fatal_error.is_none(), "block {h}: {:?}", ctx.fatal_error);
        if std::env::var("GOLDEN_PRINT").is_ok() {
            let rows = |tag: u8| ctx.state.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[tag])).unwrap().len();
            println!(
                "  block {h}: liquidations={} adl={} vault_positions={} cooldown={} prev_mark={} cursor={} pending={} trades={}",
                metrics.liquidations_triggered.get(),
                metrics.liquidations_adl.get(),
                ctx.positions.positions_for_trader(&liq::LIQUIDATOR_VAULT).unwrap().len(),
                rows(liq::COOLDOWN_TAG),
                rows(liq::PREV_MARK_TAG),
                rows(liq::CURSOR_KEY[0]),
                rows(liq::PENDING_TAG),
                ctx.trade_index,
            );
        }
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut rows);
        drop(ctx);
        // As app.rs on the pipelined path: the marker rides the frozen set (the
        // next block's guard reads it through the parent layer).
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        // Inline: the delta taken before the freeze; worker: item 6 cut 5,
        // the worker takes it from the frozen set (app.rs, pipelined).
        let delta = (r != R::Worker).then(|| overlay.own_pending_delta());
        let frozen = overlay.freeze(h);
        if r == R::Worker {
            let delta = torus_bridge::native_executor::BlockDelta::Frozen(frozen.clone());
            end_resident_on_worker(&mut holder, rows, &mut overlay, delta, true, Some(metrics.clone()));
        } else {
            end_resident(&mut holder, rows, &mut overlay, delta.unwrap_or_default(), true, Some(&metrics));
        }
        if let Some(p) = parent.take() {
            flush(p, &outputs, &mut digests);
        }
        parent = Some(frozen);
    }
    if let Some(p) = parent.take() {
        flush(p, &outputs, &mut digests);
    }
    if resident {
        assert_eq!(holder.rows_builds(), 1, "R built once, carried through every block");
        assert_eq!(holder.rows_shared_fallbacks(), 0);
        let r = holder.rows().expect("R stashed after the last block");
        for cf in torus_state::resident_rows::RESIDENT_CFS {
            let rows: Vec<_> = r.rows(cf).unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            assert_eq!(rows, db.iterate_cf(cf, None).unwrap(), "R == DB after the last flush ({cf})");
        }
    } else {
        assert_eq!(holder.rows_builds(), 0);
    }
    if std::env::var("GOLDEN_PRINT").is_ok() {
        println!(
            "summary threads={threads:?}: accepted={} rejected_cancelled={} rejected_margin={} liquidations={} positions={} liq_rows={}",
            metrics.orders_placed_accepted.get(),
            metrics.orders_rejected_cancelled.get(),
            metrics.orders_rejected_margin.get(),
            metrics.liquidations_triggered.get(),
            db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap().len(),
            db.iterate_cf(CF_NATIVE_LIQUIDATION, None).unwrap().len(),
        );
        // s92: sell cuts as [pool / non-pool][zero / partial] totals (print
        // only; not part of the digests).
        let cuts: Vec<Vec<u64>> = metrics
            .sell_margin_cuts
            .iter()
            .map(|by_fill| by_fill.iter().map(|b| b.iter().map(|c| c.get()).sum()).collect())
            .collect();
        println!(
            "s92 threads={threads:?}: sell_cuts pool(zero, partial)={:?} non_pool(zero, partial)={:?} maker_margin_cancels={} reduce_only_cuts={} top_ups(full, partial, none)=({}, {}, {}) non_pool zero by bucket={:?} partial by bucket={:?}",
            cuts[0],
            cuts[1],
            metrics.maker_margin_cancels.get(),
            metrics.reduce_only_cuts.get(),
            metrics.sell_top_ups_full.get(),
            metrics.sell_top_ups_partial.get(),
            metrics.sell_top_ups_none.get(),
            metrics.sell_margin_cuts[1][0].iter().map(|c| c.get()).collect::<Vec<_>>(),
            metrics.sell_margin_cuts[1][1].iter().map(|c| c.get()).collect::<Vec<_>>(),
        );
    }
    digests.into_iter().unzip()
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, p_milli: u64) -> bool {
        self.below(1000) < p_milli
    }
}

const A_SENDERS: u64 = 40;
const A_MARKETS: u64 = 12;
/// Markets 1..=8 get a mark (a ±3% random walk); 9..=12 never do.
const A_MARKED: u64 = 8;
const A_BLOCKS: u64 = 12;

/// Scenario A (fixes 1 / 3): `ubench_econ`'s generator (target 1500 x 20,
/// band 5, cross 0.5) around each market's mark, plus market / IOC orders
/// (match-checked takers: D2 pool vs taker-only) and reduce-only orders, cancel-alls;
/// balances from one order's margin to rich, so marks moving against thin
/// makers cancel them for margin (F1 `marginCanceled`).
fn scenario_a(db: &StateDb) -> Vec<Block> {
    let ctx = seed_ctx(db);
    for s in 0..A_SENDERS {
        let amount = match s % 5 {
            0 => 1_600,
            1 => 4_000,
            2 => 12_000,
            _ => 100_000_000,
        };
        fund(&ctx, &sender(s), amount);
    }
    let mut rng = Lcg(0x5EED_0087_601D);
    let mut px: Vec<i64> = vec![30_000; A_MARKETS as usize + 1];
    let mut blocks = Vec::new();
    for h in 1..=A_BLOCKS {
        let mut marks = Vec::new();
        for m in 1..=A_MARKED {
            px[m as usize] += rng.below(1_801) as i64 - 900;
            marks.push((m, px[m as usize]));
        }
        let mut actions = Vec::new();
        for _ in 0..30 {
            let s = rng.below(A_SENDERS);
            let a = if rng.chance(40) {
                NativeAction::CancelAllOrders { market_id: None }
            } else if rng.chance(30) {
                NativeAction::CancelAllOrders { market_id: Some(1 + rng.below(A_MARKETS)) }
            } else {
                let n = 1 + rng.below(24);
                let orders = (0..n)
                    .map(|_| {
                        let m = 1 + rng.below(A_MARKETS);
                        let mid = px[m as usize] as i128;
                        let mut is_buy = (s + m).is_multiple_of(2);
                        let aggressive = rng.chance(500);
                        let d = 1 + rng.below(5) as i128;
                        let units = if is_buy == aggressive { mid + d } else { mid - d };
                        let (mut price, mut reduce_only) = (units, false);
                        let (order_type, tif) = match rng.below(100) {
                            0..=9 => {
                                price = if is_buy { mid + 60 } else { mid - 60 };
                                (OrderType::Market, TimeInForce::IOC)
                            }
                            10..=14 => (OrderType::Limit, TimeInForce::IOC),
                            15..=19 => {
                                is_buy = !is_buy;
                                reduce_only = true;
                                price = if is_buy { mid + 2 } else { mid - 2 };
                                (OrderType::Limit, TimeInForce::GTC)
                            }
                            _ => (OrderType::Limit, TimeInForce::GTC),
                        };
                        PlaceOrderParams {
                            market_id: m,
                            is_buy,
                            price: FixedPoint::from_raw(price * FixedPoint::SCALE),
                            quantity: FixedPoint::ONE + FixedPoint::from_raw(rng.below(3) as i128 * FixedPoint::SCALE / 2),
                            order_type,
                            time_in_force: tif,
                            reduce_only,
                            client_order_id: None,
                        }
                    })
                    .collect();
                NativeAction::PlaceOrderBatch(orders)
            };
            actions.push((sender(s), a));
        }
        blocks.push(Block { ts: 1_000 + h, marks, rows: vec![], actions, liq: None });
    }
    blocks
}

/// Scenario B (fixes 2 / 3): 11 traders, positions in markets 1-3, budgets
/// scan 3 / act 2 (cursor cuts). Blocks 1-4 without any mark, with pending
/// and cooldown rows pre-seeded; 5-12 a falling mark on markets 1-2 (stage 1
/// in chunks > 100k notional, a seeded cooldown, backstop, ADL of a trader
/// and later of the vault); 13-16 marks stale (block time jumps 120 s) with
/// pending / cooldown rows written at 13; 17-18 marks back. Market 3 is never marked.
fn scenario_b(db: &StateDb) -> Vec<Block> {
    let ctx = seed_ctx(db);
    let (s, m) = (addr(61), addr(60));
    fund(&ctx, &s, 100_000_000);
    fund(&ctx, &m, 100_000_000);
    let pair = |long: &Address, short: &Address, mkt: MarketId, qty: i64| {
        ctx.positions.apply_fill(long, mkt, true, fp(qty), fp(1_000), MarginType::Cross).unwrap();
        ctx.positions.apply_fill(short, mkt, false, fp(qty), fp(1_000), MarginType::Cross).unwrap();
    };
    let t = |n: u8| addr(n);
    for (n, amount) in [(1, 5_500), (2, 345), (3, 200), (4, 300), (5, 1_000), (6, 500), (7, 2_000), (8, 1_000_000), (9, 700), (10, 100_000), (11, 4_500)] {
        fund(&ctx, &t(n), amount);
    }
    pair(&t(1), &s, 1, 200); // chunks (198k notional at 990)
    pair(&t(2), &s, 1, 10);
    pair(&t(3), &s, 1, 4); // ADL at 900
    pair(&t(4), &s, 2, 10); // backstop at 975 -> vault
    pair(&t(5), &s, 1, 10);
    pair(&t(5), &s, 3, 5);
    pair(&t(6), &s, 3, 3); // unmarked only: skipped
    pair(&t(7), &s, 2, 20);
    pair(&s, &t(7), 1, 10);
    pair(&t(8), &s, 1, 50); // healthy, seeded cooldown
    pair(&t(9), &s, 2, 10);
    pair(&t(9), &s, 3, 2);
    pair(&s, &t(10), 2, 50); // short: an ADL counterparty
    pair(&t(11), &s, 1, 150); // seeded cooldown (no block reaches its stage 1 inside it)
    for n in [5, 6, 9] {
        liq::set_pending(db, &t(n), true).unwrap();
    }
    liq::set_cooldown(db, &t(11), 1_000).unwrap();
    liq::set_cooldown(db, &t(8), 1_001).unwrap();
    let bid = |mkt: MarketId, px: i64, qty: i64| PlaceOrderParams {
        market_id: mkt,
        is_buy: true,
        price: fp(px),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let book1 = vec![(
        m,
        NativeAction::PlaceOrderBatch(vec![
            bid(1, 985, 30),
            bid(1, 984, 60),
            bid(1, 975, 40),
            bid(1, 960, 100),
            bid(1, 940, 100),
            bid(2, 980, 10),
            bid(2, 950, 20),
        ]),
    )];
    let book8 = vec![(m, NativeAction::PlaceOrderBatch(vec![bid(1, 920, 50), bid(1, 900, 50), bid(2, 910, 30)]))];
    let falling: [(i64, i64); 8] = [(990, 990), (985, 980), (975, 975), (960, 965), (950, 950), (930, 940), (910, 920), (900, 900)];
    let mut blocks = Vec::new();
    for h in 1..=18u64 {
        let (ts, marks) = match h {
            1..=4 => (1_000 + h, vec![]),
            5..=12 => {
                let (a, b) = falling[h as usize - 5];
                (1_000 + h, vec![(1, a), (2, b)])
            }
            13..=16 => (1_120 + h, vec![]),
            17 => (1_120 + h, vec![(1, 890), (2, 905)]),
            _ => (1_120 + h, vec![(1, 880), (2, 900)]),
        };
        let actions = match h {
            1 => book1.clone(),
            8 => book8.clone(),
            _ => vec![],
        };
        // Stale phase: pending / cooldown rows of traders with and without positions.
        let rows = match h {
            13 => vec![
                (liq::PENDING_TAG, t(2)),
                (liq::PENDING_TAG, t(9)),
                (liq::PENDING_TAG, t(10)),
                (liq::COOLDOWN_TAG, t(5)),
                (liq::COOLDOWN_TAG, t(10)),
            ],
            _ => vec![],
        };
        blocks.push(Block { ts, marks, rows, actions, liq: Some((3, 2)) });
    }
    blocks
}

/// Scenario A — serial and engine-forced (4 threads) are identical. Pinned on
/// c93c579; re-pinned at option B (s87): an intended outcome change (a
/// non-pool batch sell reserves at the start-of-batch best bid; taker-only
/// rounding allowance) and again at the same-batch bid bound (s87: a
/// non-pool sell topped up for an earlier funded bid of the batch) and at
/// s89 (only a bid that will rest counts). Fixes 1-3 were proven against
/// the c93c579 digests. Re-pinned at B-blind (s92, owner decision: a
/// non-pool sell is topped up to reserve(B0 x (1 + 10 bps)) from the free
/// margin left after Phase 2, partially, never from other traders' bids;
/// replaces the s89 same-batch bound). Serial and engine (4) equal; vs s89:
/// fills 1,069 -> 1,075, accepted 1,702 -> 1,707, rejected_cancelled 179 ->
/// 180, non-pool sell cuts (zero, partial) (35, 14) -> (40, 7), pool cuts
/// (8, 0) unchanged; top-ups (full, partial, none) = (393, 0, 2). The marks
/// here walk up to ±900 per block (3% of the mid), so same-batch bids sit
/// far above the start bid B0: the s89 bound followed them, B-blind covers
/// B0 + 30 ticks only. Re-pinned by adl-budget A3 (rule H): from block 2 (the
/// first mark change) the `0x03` rows are `last ‖ prev`; every block's
/// results and position / balance / liquidation rows (the `0x03` values cut
/// to `last`) checked equal to the D10 digests' run before re-pinning.
const GOLDEN_A: [&str; A_BLOCKS as usize] = [
    "0xc89a22e0fea0bc6f60a62e6f94b1599a68c07b33b5f17431538843383b80a0b5",
    "0x7960e867a35c8fdce5f56e5d3d6661162f8a96b14a350628d83604b434cf3e59",
    "0x0deec3de2ab24b612cca75d3c47e0011738c35dabe404af3a65a43577c261fc0",
    "0x129722e4f2903ebe050545391ae83abcad213281ec8c23be16d8b3bc9d8bf433",
    "0xcdbdfe174f649470f76be10c682bbe8303cea0d95d185cf47687df2fc90b6756",
    "0x721844dce07776e4592fff8226d4a4b43276ca41dcf09b499de02ad48356713c",
    "0x5604445b0b78069641ee2b8c7672c1618a1453099204819edca910f679920db9",
    "0x964a678de1c4ea0e0b9e41105df3869f1f8b485933624e205f9db2329d776285",
    "0x19463cac6e154c0a1ffba1373e36fe9075a51d42e9681baf0442fc63cab47fc1",
    "0xbf6055429f8f1a82473be75c259f142c1332e4cf11396afe861f1cf8c58f96c2",
    "0x92fe19c4cec83034d4ea3837faf04d350798c12eb8d2cdcba6f85240452a0970",
    "0x214002d6bcf9c104b7361973e1b70e9712d334c55ca4829d32483374496b4047",
];
/// Scenario B on c93c579; re-pinned by adl-budget A3 (rule H): from block 6
/// (the first mark change) the `0x03` rows are `last ‖ prev`; every block's
/// results and position / balance / liquidation rows (the `0x03` values cut
/// to `last`) checked equal to the D10 digests' run before re-pinning.
/// Re-pinned by adl-budget A5+A6 (P2, on purpose): from block 8 (the first
/// ADL block) the ADL'd positions pass through the escrows (`0x07` rows
/// written and deleted in the block: tombstones in the running hash, escrow
/// balance rows); [`golden_repins_change_only_rule_h_and_p2_rows`] proves
/// every other row and every block's outputs equal to 56318a9's (D10, no P2).
const GOLDEN_B: [&str; 18] = [
    "0x01ad98e2504ea6d07d86d94eb488ea2f620b593effec4284b1cb37a3cf07cdbd",
    "0xd5ce9dd0a968a2016bb69dc20bc5e12ba6e9bf600539c7cc2989087b49e98562",
    "0xd8abb0cf2659f07e75b8727065ad904f5216d93f0ea257066e0c0b35336e9268",
    "0x4993dfb40e8b399bd26259a06dcf3b3cc8863a3234b2a4470cdbfefd3156f85e",
    "0xdad5dd048374e566c584bd587745d949e7aa053bd1b0e36e43d36e61c76010f2",
    "0x08d89c1339ddf8b3da9246c698dd2673ad0398da8d70bdfa0985c17ae9604636",
    "0xb86de24be5bbd879d94fbef2787cb3d24813d99e7d141e570b23d0917a83a4be",
    "0xe56b53c9c75fa016fbd55e849f7fd71f75d412b94a0a51cf339d4585c99dbc0f",
    "0x005f8e8c234f1c1c52ff405e93f3d91c3678c31147a3846a54749556656afd73",
    "0x529970a5d09713b43c2972318e79c5a300016f1d26e20bf19a47c70a627878b0",
    "0x3332a37809b93f4bb770cf0879083e31a453b0cf19a62f7b6d249bad559ed42a",
    "0x831a18eb3ce615493024f86bed6426c50408945b6fc64ce0b5333aa37e6e884b",
    "0xdb7b953bade90906fee455b7a502e27f09c4e980f0f2f723f8a38e027c0f8984",
    "0x932f47d6158e82abaf81b2eabdf3a7843fe5d78a8c9cde8111c76df67b3dc601",
    "0x530fe2e8ee6823ecc00939450c914dc090538b683f584a50895222accbc1b01e",
    "0x5abd2a1432860f381db233de5715719a617b11a159363689bdd7e9cca45c5fb2",
    "0x194865aa6001ef32b7982be69f08e62e6f1e824ec0a0ab087f689219c87f408a",
    "0xab7e76a736438367521abc81cdf93c13060e37669dd681824f0e26ba11b5dad0",
];
/// [`golden_repins_change_only_rule_h_and_p2_rows`]: pinned on 56318a9.
const REDUCED_A: [&str; A_BLOCKS as usize] = [
    "0x1e216ac310a4dc30f9d20b20dc0804ddcb92139f362779b4ef72777a5c256395",
    "0xdcf49a1ce6009f620d6ff1266adfa6f23cfcaf30fef509a3aa59a83178b798c4",
    "0x95b5b46d895d255e59a4d8db1140916820173420257c30f27dcf476089c7d9b1",
    "0x3a1a0425da7327bf46cc94d9fcfc985ff0b729772caba374a466fa62455c4ebb",
    "0xd25f195f0051ee647a33c4fe7dde50b2fa183080695599f08aae12b77db2d28f",
    "0xb5bfbf3ed1d60b6d7fdf24653ad365947d8fe34e78cf0814f686c6336d8c51dc",
    "0x5b42210a9ab79087a0215784812c3c23a520e07bf51af9b19662c6f3153e3022",
    "0x389cd1a820427a59fb1a1edb458d08f2a1cc80ebaeb9270b473d1d08e60dc2df",
    "0x1568b3e98c89657bffed3f3b32f9c2b2ee7467af6aa937ae3037a052abb7771e",
    "0xcffe661162f742bfef8479065dcb5970bd12585817fd35ef8d4c1a97d37dde62",
    "0xf1a5fcfa71a63b7ac1debc6f4824bb4fd0fdde6865525094c57aeba638482d52",
    "0x2c386f2dfc1c57fef323e35abe851fdef467326897b07075749f0cd6e751bacd",
];
const REDUCED_B: [&str; 18] = [
    "0x86b5bbc4e7b12940fcaddd409473acc127055700930c7e2c01982c3995c1d2a6",
    "0x76d1fca7edae343d092286720cafee54839d4fb5cff2c00f140fabc02b04cca2",
    "0xd4b9f654dd63fd9489fea94ccf57f3f723b9d4d9bbcd8ae0124d6d931caf63d3",
    "0xf02634d7adc94e535b13832909c7b9c26ba2269b1a9c0a6519a84b20f10b67f7",
    "0xfa8a5ab7f9a2747ac39b83e8288c2f77f7553c0d47c1a88994cb57d1d538b2d4",
    "0x5cd4fcc1eef97ef3a242faeda00a7d014df0a3f67a5b7236314844f90d3fac96",
    "0x108f3b38bcd0c60eab535eef5e306e38fd4a44c22d580e45a44336f647bcf085",
    "0x2ce64fc511d84ecf8ec00999634e68940cd5d259938b38d418629cccb676facc",
    "0x12aeb2450213a440321509e37a628f2f6dbc5e095230f59c29b1d7107b12b8fe",
    "0xbdc5885774cfc04aafcfe239b123f78c035acd82b288dc128e9e8f16dc285298",
    "0x1983d61d95707772e8e5816d5856d93aaa9f8dc888557ebb035c08bca7a28ac0",
    "0x93b7296ab71602ae128a4bc76187c88cf3e981202d227a79f187b75d0c2777ba",
    "0xfbe0deac489a3c9394627d3ca84c29b42fd8ced5c125418780f04e820ef6575b",
    "0xf6fba44ba5aebcf7fa1abfe4e8b2bb7d58dabb289fd858711f27f612e885d55d",
    "0xdff904f5b8d4d6861ef61f8c0ff267ecf0c0ed36743eea3ad99e6ef7d578d278",
    "0x4958522c33a537ea933e990150a60c66b6fb444deca45f245e7f821489416ca0",
    "0x70667a0748eaa57168bd62b47f3093867d61835ca7bcd455e7873fe17bb429c1",
    "0x0c263752661d6caa0b40181ba74be4af244bd4faf5f94bd180b262d0b67908a4",
];

fn check(name: &str, got: &[String], want: &[&str]) {
    if std::env::var("GOLDEN_PRINT").is_ok() {
        println!("const {name}: [&str; {}] = {got:#?};", got.len());
        return;
    }
    assert_eq!(got.len(), want.len(), "{name}: block count");
    for (h, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g, w, "{name}: block {} digest differs from the pinned one", h + 1);
    }
}

#[test]
fn scenario_a_serial_digests_golden() {
    for r in R_MODES {
        let markets: Vec<MarketId> = (1..=A_MARKETS).collect();
        let (_d, db) = listed_db(&markets);
        let blocks = scenario_a(&db);
        check("GOLDEN_A", &run(&db, &blocks, None, r), &GOLDEN_A);
    }
}

#[test]
fn scenario_a_engine_digests_golden() {
    for r in R_MODES {
        let markets: Vec<MarketId> = (1..=A_MARKETS).collect();
        let (_d, db) = listed_db(&markets);
        let blocks = scenario_a(&db);
        check("GOLDEN_A", &run(&db, &blocks, Some(4), r), &GOLDEN_A);
    }
}

#[test]
fn scenario_b_liquidation_digests_equal_c93c579() {
    for r in R_MODES {
        let (_d, db) = listed_db(&[1, 2, 3]);
        let blocks = scenario_b(&db);
        check("GOLDEN_B", &run(&db, &blocks, None, r), &GOLDEN_B);
    }
}

/// 18c review (adl-budget A3 nit c; A5/A6): the GOLDEN_A / GOLDEN_B re-pins
/// change only rule H's `0x03` rows and P2's rows. The reduced digests
/// ([`reduced_digest`] + the block's outputs) were pinned on 56318a9 (D10, no
/// P2) with this same function and stay equal after rule H and P2.
#[test]
fn golden_repins_change_only_rule_h_and_p2_rows() {
    let markets: Vec<MarketId> = (1..=A_MARKETS).collect();
    let (_d, db) = listed_db(&markets);
    let blocks = scenario_a(&db);
    check("REDUCED_A", &run_both(&db, &blocks, None, R::Off).1, &REDUCED_A);
    let (_d, db) = listed_db(&[1, 2, 3]);
    let blocks = scenario_b(&db);
    check("REDUCED_B", &run_both(&db, &blocks, None, R::Off).1, &REDUCED_B);
}
