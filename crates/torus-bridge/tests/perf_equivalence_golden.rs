//! s87 crab perf fixes (`docs/plans/crab-perf-fixes-s87.md`, Task 0.2): golden
//! per-block digests pinned on `c93c579`, before any of fixes 1-3. The fixes
//! only change HOW MUCH work execution does, never what it writes or
//! returns, so these digests must stay identical after every fix commit.
//! Option B (s87) changes outcomes on purpose: it re-pinned scenario A only
//! (scenario B — GTC bids and liquidation orders — is unchanged).
//! Row 50 (s96) re-pinned scenario A for the results' success / error only
//! (book rejections reported rejected); the digests over the pre-row-50 view
//! of the results (`pre_row50`) must still equal the previous pins.
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
    CF_CONSENSUS_META, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS,
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

/// Row 50: the results as they read before row 50, when an order the book
/// refused or cancelled without a fill was reported ok. Such a result is the
/// only failure that keeps its gas (a book outcome: `gas_used > 0`); it
/// reads back as ok, and in the liquidation step's list (which keeps only
/// failures) it is dropped. Digesting this view against the pre-row-50 pins
/// proves the label is ALL row 50 changed: state, gas, metrics, trade index.
fn pre_row50(results: &[torus_bridge::native_executor::NativeActionResult], failures_only: bool) -> Vec<NativeActionResult> {
    let book_outcome = |r: &torus_bridge::native_executor::NativeActionResult| !r.success && r.gas_used > 0;
    pinned(results)
        .into_iter()
        .zip(results)
        .filter(|(_, r)| !(failures_only && book_outcome(r)))
        .map(|(mut p, r)| {
            if book_outcome(r) {
                p.success = true;
                p.error = None;
            }
            p
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
/// the resident rows R are kept. Returns one hex digest per block, twice:
/// over the results as they are, and over their pre-row-50 view
/// ([`pre_row50`]).
fn run(db: &StateDb, blocks: &[Block], threads: Option<usize>, r: R) -> (Vec<String>, Vec<String>) {
    let resident = r != R::Off;
    torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
    let mut holder = ResidentBooks::default();
    let metrics = Arc::new(Metrics::new());
    let mut books: HashMap<MarketId, OrderBook> = HashMap::new();
    let mut next_id: u128 = 1;
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut outputs: Vec<[String; 2]> = Vec::new();
    let mut digests: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    let flush = |p: Arc<FrozenPending>, outputs: &[[String; 2]], digests: &mut [Vec<String>; 2]| {
        let h = p.height();
        p.flush_with_native_trie_stats(db, Some(h), None, None).expect("flush");
        let state = db_digest(db);
        for (k, out) in outputs[h as usize - 1].iter().enumerate() {
            let mut data = state.clone();
            data.extend_from_slice(out.as_bytes());
            digests[k].push(keccak256(&data).to_string());
        }
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
            Some((scan, act)) => NativeExecutor::run_liquidations_with(&mut ctx, scan, act),
        };
        ctx.save_order_books();
        let agg = pinned(&agg);
        let output = |res_view: Vec<NativeActionResult>, liq_view: Vec<NativeActionResult>| format!(
            "agg={agg:?}|res={res_view:?}|gas={}|liq={liq_view:?}|trades={}|next_id={}|fatal={:?}|acc={} rc={} rm={} ol={} rb={} cpf={} stp={} oth={} liqs={}",
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
        );
        outputs.push([
            output(pinned(&res.results), pinned(&liq_res)),
            output(pre_row50(&res.results, false), pre_row50(&liq_res, true)),
        ]);
        assert!(ctx.fatal_error.is_none(), "block {h}: {:?}", ctx.fatal_error);
        if std::env::var("GOLDEN_PRINT").is_ok() {
            let rows = |tag: u8| ctx.state.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[tag])).unwrap().len();
            println!(
                "  block {h}: liquidations={} vault_positions={} cooldown={} prev_mark={} cursor={} pending={} trades={}",
                metrics.liquidations_triggered.get(),
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
    let [digests, pre_row50_digests] = digests;
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
    (digests, pre_row50_digests)
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
/// B0 + 30 ticks only.
///
/// Row 50 keeps these as the pre-row-50 pins: the digests over the
/// [`pre_row50`] view of the results must stay exactly these (state, gas,
/// metrics and trade index unchanged); [`GOLDEN_A`] re-pins the results'
/// success / error only.
const PRE_ROW50_A: [&str; A_BLOCKS as usize] = [
    "0xc89a22e0fea0bc6f60a62e6f94b1599a68c07b33b5f17431538843383b80a0b5",
    "0x2284087928a8c8753efa5355fb90d3993a781ccd6dc93642fea91528732cbdad",
    "0xbd5bd04f2e1e9b445ec102b461fbf506a92a2054b1301013d42f11af2adff0f8",
    "0x26775ae3d5baec11446832b8dbcecdf1f2ca3286ab23c6a12d824c029c28b6c9",
    "0x366884dbde98bcf0308d6248644d0ac91ebf211e42a99c15c4c5c673f932744f",
    "0x04b7be2cb2d2c80957563a8fc0855b4f3212a6ea96f5088537988324a8f9a11a",
    "0x5ba4d2fdaa7c9f95e1182c11fdc08391ee44e0b9e0dc485b72ca5b0cc54d9bed",
    "0xba8206944edc83ec4f0d4aa8d6d906905bcd462df719282dfd497fd8145c4b5e",
    "0x079961f4d69af475d3724614f78c3c05d3002c2d5a76dbad6deb91a519f50086",
    "0x580ac36cac8cf8295e1f711152502335c55b0d59187ecf7b951f5dd5806027e6",
    "0x1151116c5cc5f7a22af8f0299d36f89864d0ba505a3676ada8696c5c116a0599",
    "0x1456c3b4d0de478bcefd4bed519c5288d9946861e7c37914e99e327e9187f923",
];
/// Scenario A at row 50 (s96, owner decision): an order the book refuses or
/// cancels without a fill is rejected with its HL reason (success false, an
/// error), not executed; gas and everything else as [`PRE_ROW50_A`].
const GOLDEN_A: [&str; A_BLOCKS as usize] = [
    "0x8e809cbd45abcc0545c71fa1e509b7ab261510773ea274bcfa4c429ff1104dac",
    "0x9fce81bd56ac0fe851bd363547599bdf8d8e345f6f0a41b668a711e855e2539b",
    "0xa5c2913771cabf06653dc40068a32f05ad644c08d597b1023e6938310f868c7b",
    "0xc3369ad3810c8cace12fd2f462bb0e1d8c1e3b81171d2019cd15bac04b228e06",
    "0x3f78722d7737d12048b66021062f7503ffaf1c2c61cee31e9887540a1c6b2f15",
    "0x8efdc745286e9202b6fc5d52d1f2349dd7135e896248baf3ed994c782cb431c3",
    "0xd53420062afa2f890c26b53df03f4cfb66a05699a28dbae5e3fd6090a304e833",
    "0x3c6e4857a5f542c7ec73890ecde1034a4fa315ef019c84c8c2565fe4914cf7fb",
    "0x2398b3bd6fda7614e49238a10d4dc6f931c584cbda437df57a5e36cece40bda3",
    "0x8733aa91e2759daed4a9beb7cf7a3803fc9a36e49ce9d5b33bce055f27a5be76",
    "0x90fab3384498cb2db3ab07d0b2ccb5c1082f7e6f22be338347f707aac4f79fc7",
    "0xf1f68bbd98bdae97a8de1daa1ddb877d2e5746f4f17aeac5d8b57c0451d2b7bc",
];
/// Scenario B on c93c579 (no book rejection: the same with and without the
/// pre-row-50 view).
const GOLDEN_B: [&str; 18] = [
    "0x01ad98e2504ea6d07d86d94eb488ea2f620b593effec4284b1cb37a3cf07cdbd",
    "0xd5ce9dd0a968a2016bb69dc20bc5e12ba6e9bf600539c7cc2989087b49e98562",
    "0xd8abb0cf2659f07e75b8727065ad904f5216d93f0ea257066e0c0b35336e9268",
    "0x4993dfb40e8b399bd26259a06dcf3b3cc8863a3234b2a4470cdbfefd3156f85e",
    "0xdad5dd048374e566c584bd587745d949e7aa053bd1b0e36e43d36e61c76010f2",
    "0xc0d047fa5ae5b4641924063d9d67f203c2788a0a91e2301e6e9e3634b5ddc7dc",
    "0x82219ea69717ef004a6b291d775c328fc12af6c33ff6eec3f703e8ee5ae87b6f",
    "0x7f639e61703da2cce928adbad2b036b139c94e620fee028f7850e821982a51ca",
    "0xd4399be1f08f404e26c96ae4cebdd280bf211e0c81f809ab0179c23b06dd0f20",
    "0xda0330908ca7cf9b53873f0e5b0786c2dfb49d1b260c1c705ff320d6d6157179",
    "0xe199aa106ea73caf7a394cf3a3a139f159083e336835dc5d713604cc8c74e91f",
    "0x977df580684dafa431097fd40494ef5e9c13b619e16cc64e51c2787ceb96170b",
    "0x931b70afb6c0dda49742a5cfd88efd5e838f59656f0f46ea9cceeef8d1d25050",
    "0xaf52e01b7f6e0024dba69213f932e4925b7af4e0b4b235402f5c3e16b314f1f6",
    "0x0d8de3f9c2113cb5fbd1cba0e6f284d96b585bed9948671fde57f1db0369477d",
    "0x9f21b7d6fab6f78ae5b753fa3751fcd8ff462332e777063692ef0974024a0fa8",
    "0x1661b87681327eea7728c912ef4a63965082b2e15fb24ca6b4f071ff5eb974b7",
    "0xddb569290db77e72f597a706895d10fca15687cefd1b1f053d2aa878fa819545",
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
        let (digests, pre_row50) = run(&db, &blocks, None, r);
        check("PRE_ROW50_A", &pre_row50, &PRE_ROW50_A);
        check("GOLDEN_A", &digests, &GOLDEN_A);
    }
}

#[test]
fn scenario_a_engine_digests_golden() {
    for r in R_MODES {
        let markets: Vec<MarketId> = (1..=A_MARKETS).collect();
        let (_d, db) = listed_db(&markets);
        let blocks = scenario_a(&db);
        let (digests, pre_row50) = run(&db, &blocks, Some(4), r);
        check("PRE_ROW50_A", &pre_row50, &PRE_ROW50_A);
        check("GOLDEN_A", &digests, &GOLDEN_A);
    }
}

#[test]
fn scenario_b_liquidation_digests_equal_c93c579() {
    for r in R_MODES {
        let (_d, db) = listed_db(&[1, 2, 3]);
        let blocks = scenario_b(&db);
        let (digests, pre_row50) = run(&db, &blocks, None, r);
        check("GOLDEN_B", &digests, &GOLDEN_B);
        check("GOLDEN_B", &pre_row50, &GOLDEN_B);
    }
}
