//! s87 crab perf fixes (`docs/plans/crab-perf-fixes-s87.md`, Task 0.2): golden
//! per-block digests pinned on `c93c579`, before any of fixes 1-3. The fixes
//! only change HOW MUCH work execution does, never what it writes or
//! returns, so these digests must stay identical after every fix commit.
//! Option B (s87) changes outcomes on purpose: it re-pinned scenario A only
//! (scenario B — GTC bids and liquidation orders — is unchanged).
//! Row 50 (s96) re-pinned scenario A for the results' success / error only
//! (book rejections reported rejected); the digests over the pre-row-50 view
//! of the results (`pre_row50`) must still equal the previous pins, and the
//! row-50 outputs alone (`ROW50_OUT_A`) must equal main's.
//! s100 item 2 (exact cost basis, Position layout v2) re-pinned EVERY
//! const here, on purpose: each position row gains `cost_basis` (and the
//! native state root covers them), so every DB digest changes, the reduced
//! ones too (they keep the non-escrow position and balance rows). Checked
//! against main f1e41975 (per-block dump of every hashed row but
//! `CF_CONSENSUS_META`, scenarios A and B, serial, R off): the same keys in
//! every block and the same outcomes (success, gas, trades, metrics,
//! liquidation results, obligation and other rows byte-equal); only
//! position entry (1,056 row versions, <= 10 raw) and realized PnL (129,
//! <= 15 raw), balance `available` (62, <= 14 raw) and the free-margin
//! amounts in 375 "insufficient margin" error texts of scenario A
//! (<= 5 raw; blocks 3-12 of `ROW50_OUT_A`) differ. Scenario B's outputs
//! are unchanged.
//!
//! Re-run that comparison: in a worktree of the base and one of the change
//! (each with its OWN `CARGO_TARGET_DIR`: a shared one can run the other
//! tree's stale build), run
//!   GOLDEN_PRINT=1 REPIN_DUMP=/tmp/<name>.txt cargo nextest run -p torus-bridge
//!     --test perf_equivalence_golden -E 'test(golden_repins_change_only)'
//! (scenarios A and B, serial, R off; the dump appends, so start from no
//! file), then `python3 tools/golden-repin-diff.py /tmp/base.txt /tmp/new.txt`.
//! The dump needs the same `repin_dump` (field names) in both trees.
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

use alloy_primitives::map::HashMap;
use std::sync::Arc;

use alloy_primitives::{keccak256, Address, U256};
use torus_bridge::native_executor::{
    begin_resident, end_resident, end_resident_on_worker, BookMode, NativeExecContext, NativeExecutor,
    ResidentBooks,
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
            error: r.error.clone().map(String::from),
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

/// Contexts pin `BookMode::Classic`: the goldens were taken on the Classic
/// layout (the code default before plan 9.13, now `TORUS_BOOK_ROWS=0`).
fn seed_ctx(db: &StateDb) -> NativeExecContext {
    NativeExecContext::new_with_mode(
        db.clone(), 0, 1_000, 0, 1_000_000, 100, addr(99), addr(100), addr(101),
        BookMode::Classic, None,
    )
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

/// s100 re-pin evidence (`REPIN_DUMP=<file>`): appends block `h`'s output
/// line and every hashed row but `CF_CONSENSUS_META` to `path`, position and
/// balance rows decoded field by field (so two layouts compare), the rest
/// hex. `tools/golden-repin-diff.py` compares two dumps.
fn repin_dump(db: &StateDb, h: u64, out: &str, path: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{h} OUT {out}").unwrap();
    for (id, cf) in HASHED_CFS {
        if *cf == CF_CONSENSUS_META {
            continue;
        }
        for (k, v) in db.iterate_cf(cf, None).unwrap() {
            let val = if *cf == CF_NATIVE_POSITIONS {
                let p: torus_core::position::Position = borsh::from_slice(&v).unwrap();
                format!(
                    "POS long={} size={} entry={} margin={:?}/{} real={}",
                    p.is_long,
                    p.size.raw(),
                    p.entry_price.raw(),
                    p.margin_type,
                    p.isolated_margin.raw(),
                    p.realized_pnl.raw()
                )
            } else if *cf == CF_NATIVE_BALANCES && k.len() == 20 {
                let b: NativeBalance = borsh::from_slice(&v).unwrap();
                format!(
                    "BAL avail={} om={}",
                    b.available.raw(),
                    b.order_margin.raw()
                )
            } else {
                alloy_primitives::hex::encode(&v)
            };
            writeln!(f, "{h} {id} {} {val}", alloy_primitives::hex::encode(&k)).unwrap();
        }
    }
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
/// the resident rows R are kept. Returns one hex digest per block, three
/// times: over the results as they are, over their pre-row-50 view
/// ([`pre_row50`]), and over the block's outputs alone as they are (no DB:
/// the row-50 outputs, [`ROW50_OUT_A`]).
fn run(db: &StateDb, blocks: &[Block], threads: Option<usize>, r: R) -> (Vec<String>, Vec<String>, Vec<String>) {
    let [digests, pre_row50, _, row50_out] = run_all(db, blocks, threads, r);
    (digests, pre_row50, row50_out)
}

/// [`run`], also returning each block's [`reduced_digest`] + its outputs in
/// the pre-row-50 view (the reduced pins predate row 50), third of four.
fn run_all(db: &StateDb, blocks: &[Block], threads: Option<usize>, r: R) -> [Vec<String>; 4] {
    let resident = r != R::Off;
    torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
    let mut holder = ResidentBooks::default();
    let metrics = Arc::new(Metrics::new());
    let mut books: HashMap<MarketId, OrderBook> = HashMap::default();
    let mut next_id: u128 = 1;
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut outputs: Vec<[String; 2]> = Vec::new();
    let mut digests: [Vec<String>; 4] = Default::default();
    let flush = |p: Arc<FrozenPending>, outputs: &[[String; 2]], digests: &mut [Vec<String>; 4]| {
        let h = p.height();
        p.flush_with_native_trie_stats(db, Some(h), None, None).expect("flush");
        let out = &outputs[h as usize - 1];
        if let Ok(path) = std::env::var("REPIN_DUMP") {
            repin_dump(db, h, &out[0], &path);
        }
        let (state, reduced) = (db_digest(db), reduced_digest(db));
        for (k, (base, out)) in [(&state, &out[0]), (&state, &out[1]), (&reduced, &out[1])].into_iter().enumerate() {
            let mut data = base.clone();
            data.extend_from_slice(out.as_bytes());
            digests[k].push(keccak256(&data).to_string());
        }
        digests[3].push(keccak256(out[0].as_bytes()).to_string());
    };
    for (i, b) in blocks.iter().enumerate() {
        let h = i as u64 + 1;
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rows = begin_resident(resident.then_some(&mut holder), &mut overlay, h, Some(&metrics));
        let mut ctx = NativeExecContext::new_with_mode(
            overlay.clone(), h, b.ts, 0, 1_000_000, 100, addr(99), addr(100), addr(101),
            BookMode::Classic, None,
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
    digests
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
///
/// Row 50 keeps these as the pre-row-50 pins: the digests over the
/// [`pre_row50`] view of the results must stay exactly these (state, gas,
/// metrics and trade index unchanged); [`GOLDEN_A`] re-pins the results'
/// success / error only.
const PRE_ROW50_A: [&str; A_BLOCKS as usize] = [
    "0x4386433aed4609e3fe307eb69e039cb84ab03da0eb4c8d916f962d792b9c3270",
    "0x75d7ec4447e7104b9b2c6a72599483737fe1c876ef33b135fd58dc17a3b4c68d",
    "0x41ddb8b10e16eda61dca97de78cb4589b2946da5682f6a58820faa8ec3d5b0a3",
    "0x00b3694b6ecd4a4f16c23ee5f3c57c81fdc04a4b3f7c515b1e5c6170123a11b4",
    "0x853a1549f5512b58258dc29d5dfdd98b523875db083a3a11f4c9724bcb2eb9d4",
    "0x599aef9d919b064f557abca7b950adaa95e4bd5ee001d88051081eb85f713b18",
    "0xf95e0f89341f38697b4869466ed7b136a08b6b49b88e9ab75d8a4ceef9492c07",
    "0xc6a1ba12d0b21941f364c2ca65965aaba3a565b1d264048fbfda00b027591a5a",
    "0x7f318bddaa40c2989137acbf46e5e7addf75157862a7e1d7c34b39773cf73ff5",
    "0x864712b5dee3b98655541aa7d59172f2a4527cfcb57a80cb52cf3a248eb75b7a",
    "0xf8bc82248cbec9b960b3a294ea2e69ed225e60f4ab185f57ee24484ab3a3d96a",
    "0xea39d111774af97ee560e25de1ab18623846e522e45c0a827bf0d4448c58cb35",
];
/// Scenario A at row 50 (s96, owner decision): an order the book refuses or
/// cancels without a fill is rejected with its HL reason (success false, an
/// error), not executed; gas and everything else as [`PRE_ROW50_A`].
/// Re-pinned at the merge of row 50 into adl-budget (s25, blocks 2-12;
/// block 1 is main's: rule H changes nothing before the first mark change):
/// the rule-H state with row 50's labels. Proof, in two parts: the state is
/// adl-budget's ([`PRE_ROW50_A`], adl-budget's pins, and
/// `golden_repins_change_only_rule_h_and_p2_rows` pass unchanged), and the
/// row-50 outputs are main's ([`ROW50_OUT_A`], checked in every run); every
/// R mode and the engine give the same digests.
const GOLDEN_A: [&str; A_BLOCKS as usize] = [
    "0xee9cfcb29cfcaaa92cf39ea0ec182858f7723d40d8ae468bfb52a5242c02a43d",
    "0x7b6cd8c5abdd43a95389085c1896babbc9e15b2dd50780aee9f9cc6e282c7610",
    "0xdfe38147efd86f0fc62c775ee2759ceb814fde9028380225d7c1228706508639",
    "0x455735df88d0eefe8c3697e56ed7604fc4bf991e9c7018c7431d10ca7c23c22b",
    "0xd441f993c882b7e79a25dc1d4e3908006c2291c5467d4cbc037623e7f17acb29",
    "0xf92c2dc331b3ef1fa0d9fc990636d301c1ae770eb173950b352a3eb6817b4b5f",
    "0xfbbea8f662b113c8f8dd505a1f8c3eac814c5595a2194d3fcc4f263710abe925",
    "0x3c7dcbfb9dc3d90b6dc3a67122d2337b709591ee189c1abc8b97368b7bc03123",
    "0x4bc981065cabb03c4371df90cf5174774ccd7ca66744ed1d8eaebf733d759d70",
    "0xa74714efbb7d518a2983ae5c58724df1190c644bad5a33e2f8e4cb7d01ad1b0d",
    "0x0c0904814ddb5cda09ac323bb2e8a7f0d6a09c111f99ae0a252507c51b3dd092",
    "0xa1c4082152acb17fbfc42dbc3f9c8b5ce0c2210c090e0f5b07ffcfc5d5770191",
];
/// Scenario A's outputs alone in the row-50 view (s99 review LOW 2):
/// keccak256 over each block's output string (results, gas, liquidation
/// results, trade index, next order id, fatal error, funnel metrics; no DB
/// row). Equal to main's (7348576a, the row-50 merge; checked against its
/// output dump, serial and engine, R Inline / Worker / Off: byte-identical
/// to this branch's), so with [`PRE_ROW50_A`] the merge's [`GOLDEN_A`]
/// re-pin is adl-budget's state plus main's row-50 outputs.
const ROW50_OUT_A: [&str; A_BLOCKS as usize] = [
    "0x23f04f7f1940fd0ea9e7199467856ff19e94fd5e9feb6a731e4ab6d7fa354ff5",
    "0xb019cecb20537d9f70418bdce16c7e98e6c79cc4f879359429c1c98b2ef1b079",
    "0x849c1549d75348fa088e085f712130dce7860666025fdc230276b6b22215efad",
    "0xad589fcc58bcea9d7fb6cad81117f557cfbfc8b564d907898e35fd6045c527d9",
    "0x1035bf9c67a19a2d5fb714e4730519eece3501d8951fe489d25cdc1e7af1f051",
    "0x2aec3b9af6fb9c706f8afc1b4f0e5d43cd7f1aa688ca4708f812a88dc49952d6",
    "0x175c4f59d4c1beda5c480e706ed10d63d2bbf9639e0d04832ab9f904049fb0a9",
    "0x4ab9e55569131cb51562b268ab635d91bd4ee0a3e581b4d11d6743481697c55c",
    "0x01c837df8b7235a67e724b4157d7aafc39b40341113e2fcb49cb12c288f7e57b",
    "0xcb909c73fcc8beee3578a22633469517f4066e263933257bd118bfc0c8ffc6c2",
    "0xce3e5a7be3957a045123c50fe2b0df1a754ce5d9fafa80a975eead569b11042f",
    "0xe491e39bc519aea7f0eb6c202055baa07af816058309b3f8080f21e093d2abbb",
];
/// Scenario B on c93c579 (no book rejection: the same with and without the
/// pre-row-50 view); re-pinned by adl-budget A3 (rule H): from block 6
/// (the first mark change) the `0x03` rows are `last ‖ prev`; every block's
/// results and position / balance / liquidation rows (the `0x03` values cut
/// to `last`) checked equal to the D10 digests' run before re-pinning.
/// Re-pinned by adl-budget A5+A6 (P2, on purpose): from block 8 (the first
/// ADL block) the ADL'd positions pass through the escrows (`0x07` rows
/// written and deleted in the block: tombstones in the running hash, escrow
/// balance rows); [`golden_repins_change_only_rule_h_and_p2_rows`] proves
/// every other row and every block's outputs equal to 56318a9's (D10, no P2).
const GOLDEN_B: [&str; 18] = [
    "0x6508c280d9bc733731f1c36b1b7dc836defa7eb79e5ad88ed15546c359a758d4",
    "0x67fa8d76b4341eed802a05cbb1eff9666292220e12f0445cf96a112735e01570",
    "0x938ecff6e39bf510a6b672a8215aec72cd1cc1ec4707c114de2ba697022e52ee",
    "0xdb34aa85e3d6c856b7dafb516a4f0d17e709f1fe0cbc5352f9a82d8556b6bd31",
    "0x1160786fcac855581da8ec6c7b24cf9684a0b84b2eba8f7a5441c188be564689",
    "0x5a9a4f8495e76a113d4b7485bdc16c3e8f9932bf5cd0469b3d9c6ea846c44c0f",
    "0xc35d9f3776241ce5c84f35cd9267059cc760c3f1902a6db659f051cb2f2cbcb6",
    "0x7d52a77e23bc8e38b6234c15731d9107041551f3ecdb5d485c7327f41d3a34af",
    "0x4480e1caa330a4e9f27ee2ece66945528452b5972072e794ab0955a61dde8503",
    "0xaa88b9e1ba36c62d8320263879e1026b4ef58b481fb50c575f217740d1e0fdec",
    "0x823d8df0b1f557ffd3430a5b00781106aa3b54e52704483c51124953f4c4696a",
    "0x0919f1b17feec14537329f70d6f5d11c17ab1c0d0335a5a64008498c1ff45990",
    "0xb83b82d4bedcbe859dca44ce122aba1258645c50e1c357801f228ec6fb56d62b",
    "0xfa2e4e077d7f185b6607a81a434b54d4ce640adf4120157e0077d963176f5be3",
    "0xdfbdb8b0bace7eb864bf8fd5acf9b2fbc4921fcdec1811046a94bab45261e25d",
    "0x861bd1c802dca594080202e91a1a2ce69ba00937ecaa0d8edb278075f6ccfeef",
    "0x9878d278ea9aba25370833278793b13665c9b8802ec56febedfe0d955e392775",
    "0xa36fcb45f6d2e86744ea9b27e317dcb1f3692ab14107e2bc6ed3bd0465021afe",
];
/// [`golden_repins_change_only_rule_h_and_p2_rows`]: pinned on 56318a9.
const REDUCED_A: [&str; A_BLOCKS as usize] = [
    "0xc616add92af77cf85acf74b1b1b17bf03beff5273130d13db7154bf757fcc171",
    "0x2b139a63be87782da59c007ea47f2bcff6b96fe66ca1dd259a929b23a7100459",
    "0x250198987e3694f39aac06fff8567d0d405de0f54dc89c4e2c83ee615fcbe8d3",
    "0x5a0be2fc72ea792e0fb8c593b45707d5ca768285e8dafb10a2df808e68a418da",
    "0xe3f16ed16893c04e47a0088dd4b8dc5ad9ad58624639d7bbbed1945447efdfc5",
    "0xda149b6927bede1b00dd98ace06f8d28ad2b6d831c087be90d857baf99b1ece1",
    "0x403184602d242e647dda5d8f0cbe49402e3d542d47295f3428b581d2f47802db",
    "0x576110b823d1e23e802ad7782d58eec16baa8f417bcc3b153041ff7249948f5a",
    "0x2044c4b481d38d9fc5dde77a69a7ad3085b3216e59e70b51cf6a08f7ca958362",
    "0x842eea796cac7d9babed48d67489251cfacdc1ac3ede14bf302f195b909eac0c",
    "0xe1e5adf61b910e1bd39ce92bb44647f06926230e37f3a7ab2010be035301486f",
    "0xc6c24ea8cf285570eb4438d7682a0b349f460bf65931b378e09eacf1b4caf764",
];
const REDUCED_B: [&str; 18] = [
    "0xada71eb46de7a1c7b4aca6d19d4fce204c5c9aa9ae9bf9c306245f0a0d77a11c",
    "0x6397c7753b9ef43698486776073f7e28276af3b8dfc5b352fb84ac45c2fbd7d8",
    "0xd77b2f80d7bb9fe21176a8c7876bf30b74dd1ebba8895fdf5dcb876c4b42c47a",
    "0xf2063736bbb7de86b8f9b6868d6c17ab53f8db3b056cb6884ba5b95f90c71955",
    "0xfece76fc902d4ffaea0f9929dff27df0f93d82fbdba5ed843972488b6cea8cf8",
    "0xadff9c80e0884640c70a0127a59fe0330b3a50966251f4cee0963e7cfb364ee5",
    "0x60a732514d1bc194e36bb2a899fbe05739e67d6cce2165c171b14c0ec5471da4",
    "0x8ff6e1858ff7a56c6466fd6e7067be196e3d9f79332ccc8e9f9387230995383a",
    "0x99d596c82b7f051d51fa0e2b52a6063aad2df09c5c077ec8b03299db0ffbdd6c",
    "0x7255c0b32493b43245ce2e680aa95738d16dfb0245ca5acf5ecd6cb76a82c18c",
    "0xeb6541be145314db76fc1bc5a590040449dfe9cd5333f06c01835fe98243200b",
    "0xa303836221614bdf5b9d44fc3b659fd906a636a2c065a75ec15ad2f40461f12b",
    "0xd7b056ce6823a787bb03b7dede59b9bf5a743144439292c3e689abcc06f83dbc",
    "0x80ac10b0abc5ba7c136f498760ddac578a01d6252da82a0cdc405dd5edbd07df",
    "0x5bab6832f800e3b737489c5d6e2ea071e916cb8f642f7862cfe9af9a173900a5",
    "0xfdae772ac064299c08ed1c079bc1a7e50b69f7caebf5643f359e4afa4d3939d4",
    "0xb1a32e289d3ef396a8b6517cd5ab764295d658f16fe53bba7b6441271b1cc154",
    "0x11947a7ad2fa6f14ab404890c39c03bf20cab72ac911852fd60b7723d0cabc16",
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
        let (digests, pre_row50, row50_out) = run(&db, &blocks, None, r);
        check("PRE_ROW50_A", &pre_row50, &PRE_ROW50_A);
        check("ROW50_OUT_A", &row50_out, &ROW50_OUT_A);
        check("GOLDEN_A", &digests, &GOLDEN_A);
    }
}

#[test]
fn scenario_a_engine_digests_golden() {
    for r in R_MODES {
        let markets: Vec<MarketId> = (1..=A_MARKETS).collect();
        let (_d, db) = listed_db(&markets);
        let blocks = scenario_a(&db);
        let (digests, pre_row50, row50_out) = run(&db, &blocks, Some(4), r);
        check("PRE_ROW50_A", &pre_row50, &PRE_ROW50_A);
        check("ROW50_OUT_A", &row50_out, &ROW50_OUT_A);
        check("GOLDEN_A", &digests, &GOLDEN_A);
    }
}

#[test]
fn scenario_b_liquidation_digests_equal_c93c579() {
    for r in R_MODES {
        let (_d, db) = listed_db(&[1, 2, 3]);
        let blocks = scenario_b(&db);
        let (digests, pre_row50, _) = run(&db, &blocks, None, r);
        check("GOLDEN_B", &digests, &GOLDEN_B);
        check("GOLDEN_B", &pre_row50, &GOLDEN_B);
    }
}

/// 18c review (adl-budget A3 nit c; A5/A6): the GOLDEN_A / GOLDEN_B re-pins
/// change only rule H's `0x03` rows and P2's rows. The reduced digests
/// ([`reduced_digest`] + the block's outputs) were pinned on 56318a9 (D10, no
/// P2) with this same function and stay equal after rule H and P2. They
/// predate row 50, so they take the outputs' pre-row-50 view ([`run_all`]).
#[test]
fn golden_repins_change_only_rule_h_and_p2_rows() {
    let markets: Vec<MarketId> = (1..=A_MARKETS).collect();
    let (_d, db) = listed_db(&markets);
    let blocks = scenario_a(&db);
    check("REDUCED_A", &run_all(&db, &blocks, None, R::Off)[2], &REDUCED_A);
    let (_d, db) = listed_db(&[1, 2, 3]);
    let blocks = scenario_b(&db);
    check("REDUCED_B", &run_all(&db, &blocks, None, R::Off)[2], &REDUCED_B);
}
