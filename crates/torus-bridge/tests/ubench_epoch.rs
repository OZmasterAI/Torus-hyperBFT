//! s89 µbench: per-step cost of EMPTY blocks around epoch boundaries
//! (epoch_length 100) after an econ load, with the oracle feed on. Emulates
//! `execute_committed_block_with` for a block without actions: due checks
//! (oracle_due / liquidation_due) and, when the native phase runs (boundary or
//! due), begin_block_oracle -> run_liquidations -> process_governance ->
//! process_epoch_boundary. Boundary blocks are serial (parent flushed first),
//! others layer on the previous frozen set.
//!
//! Keeps the s89 cost measurable: when the feed pauses, `prune_submissions`
//! deletes every `sub‖market‖validator` row; per-market submission scans then
//! walked the tombstones of all later markets (fix A: prefix scans bounded by
//! the prefix successor) and the `sub` existence check (`oracle_due`) walked
//! all of them on every block (fix B: background compaction of the pruned
//! range after the flush).
//! The SUMMARY lines give x00/x01/x02/other ms per block class.
//!
//! UB_DRAIN = stale | fresh | none
//!   stale: load fed, then block time jumps past the 60 s mark age (the s89 cell drain)
//!   fresh: the feed keeps submitting every UB_FEED_EVERY blocks during the drain
//!   none : never fed (no marks, the oracle-off cell)
//! UB_MARK_WALK=<bp> (item 6 step 0.5): each feed round moves every market's
//! submitted mark `±bp` around the mid (`common/econ_load.rs` `MarkWalk`);
//! default 0 = the mid every round, as before.
//! Item 6 Phase 1 (C1): every native block attaches the resident rows R like
//! app.rs (`begin_resident` / `end_resident`; R's upkeep is in `flush`), and
//! a block without the native phase is a marker-only job as on the node (its
//! 1-key marker set becomes the parent, the previous parent is flushed
//! outside the block's timing, R advances). `UB_NO_R=1`: without R.
//! UB_SEED_TRADERS>0 replaces the econ load with directly written positions
//! (UB_SEED_POS per trader) for scaling runs. Sizes: UB_SENDERS (5000),
//! UB_MARKETS (300), UB_ACTIONS (60), UB_LOAD (150), UB_DRAIN_TO (420).
//!
//!   cargo test -p torus-bridge --release --test ubench_epoch -- --ignored --nocapture

#[path = "common/econ_load.rs"]
mod econ_load;

use alloy_primitives::{Address, U256};
use econ_load::MarkWalk;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use torus_bridge::native_executor::{
    begin_resident, end_resident, NativeExecContext, NativeExecutor, ResidentBlock, ResidentBooks,
};
use torus_core::liquidation as liq;
use torus_core::position::{MarginType, NativeBalance, Position};
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{FrozenPending, NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn env(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn sender(i: u64) -> Address {
    let mut b = [0xA7u8; 20];
    b[12..20].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}
fn special(n: u8) -> Address {
    Address::new([n; 20])
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
    fn chance(&mut self, p: u64) -> bool {
        self.below(1000) < p
    }
}
const TARGET: i128 = 1500;
const LEV: i128 = 20;
const REPORTERS: [u8; 3] = [150, 151, 152];
const EPOCH: u64 = 100;

fn feed_setup(db: &StateDb, markets: u64) {
    for n in REPORTERS {
        StakingManager::new(db.clone())
            .put_validator(
                &special(n),
                &ValidatorState {
                    address: special(n),
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
    for m in 1..=markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
}
fn econ_order(rng: &mut Lcg, s: u64, m: u64) -> PlaceOrderParams {
    let is_buy = s.wrapping_add(m).is_multiple_of(2);
    let aggressive = rng.chance(500);
    let d = 1 + rng.below(5) as i128;
    let mid = TARGET * LEV;
    let units = if is_buy == aggressive { mid + d } else { mid - d };
    let price = FixedPoint::from_raw(units * FixedPoint::SCALE);
    let mut quantity = FixedPoint::from_raw(TARGET * FixedPoint::SCALE)
        * FixedPoint::from_raw(LEV * FixedPoint::SCALE)
        / price;
    if quantity < FixedPoint::ONE {
        quantity = FixedPoint::ONE;
    }
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price,
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

struct Chain {
    db: StateDb,
    parent: Option<Arc<FrozenPending>>,
    books: HashMap<u64, torus_core::order_book::OrderBook>,
    next_id: u128,
    holder: ResidentBooks,
    resident: bool,
}

impl Chain {
    fn ctx(&mut self, overlay: &NativeStateOverlay, h: u64, ts: u64) -> NativeExecContext<NativeStateOverlay> {
        let mut ctx = NativeExecContext::new(
            overlay.clone(), h, ts, h / EPOCH, EPOCH, 100, special(99), special(100), special(101),
        );
        ctx.order_books = std::mem::take(&mut self.books);
        ctx.next_global_order_id = self.next_id;
        ctx
    }
    /// Item 6 C1: attach R to the block's overlay (before `ctx`).
    fn begin(&mut self, overlay: &mut NativeStateOverlay, h: u64) -> ResidentBlock {
        begin_resident(self.resident.then_some(&mut self.holder), overlay, h, None)
    }
    fn finish(
        &mut self,
        ctx: NativeExecContext<NativeStateOverlay>,
        mut overlay: NativeStateOverlay,
        h: u64,
        rows: ResidentBlock,
    ) -> f64 {
        let mut ctx = ctx;
        self.books = std::mem::take(&mut ctx.order_books);
        self.next_id = ctx.next_global_order_id;
        drop(ctx);
        let t = Instant::now();
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = if rows.attached() { overlay.own_pending_delta() } else { Default::default() };
        let frozen = overlay.freeze(h);
        end_resident(&mut self.holder, rows, &mut overlay, delta, true, None);
        if let Some(p) = self.parent.take() {
            p.flush_with_native_trie_stats(&self.db, None, None, None).expect("flush");
        }
        self.parent = Some(frozen);
        ms(t)
    }
    /// A block without the native phase (app.rs: `Job::Marker`): its 1-key
    /// marker set becomes the parent behind the previous one, R advances.
    fn marker_only(&mut self, h: u64) {
        let frozen = Arc::new(FrozenPending::marker_only(h));
        if let Some(p) = self.parent.take() {
            p.flush_with_native_trie_stats(&self.db, None, None, None).expect("flush");
        }
        self.parent = Some(frozen);
        self.holder.advance_untouched(h);
    }
    fn barrier(&mut self) {
        if let Some(p) = self.parent.take() {
            p.flush_with_native_trie_stats(&self.db, None, None, None).expect("flush");
        }
    }
}

fn submit_all(ctx: &NativeExecContext<NativeStateOverlay>, markets: u64, walk: &mut MarkWalk) {
    let base = FixedPoint::from_raw(TARGET * LEV * FixedPoint::SCALE);
    walk.step();
    for m in 1..=markets {
        let mark = walk.mark(base, m);
        for n in REPORTERS {
            ctx.oracle.submit_price(&special(n), m, mark, ctx.block_height, ctx.timestamp).unwrap();
        }
    }
}

/// Read-only split of the step reads on a scratch overlay (same layering).
fn split(db: &StateDb, parent: Option<Arc<FrozenPending>>, markets: u64, h: u64, ts: u64) -> String {
    let ov = NativeStateOverlay::with_parent(db.clone(), parent);
    let ctx = NativeExecContext::new(ov.clone(), h, ts, h / EPOCH, EPOCH, 100, special(99), special(100), special(101));
    let t = Instant::now();
    let _ = ov.prefix_exists(CF_NATIVE_ORACLE, b"sub").unwrap();
    let t_pe = ms(t);
    let t = Instant::now();
    let subs = ov.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub")).unwrap().len();
    let t_prune_read = ms(t);
    let t = Instant::now();
    let listed = ctx.governance.listed_market_ids().unwrap();
    let t_listed = ms(t);
    let t = Instant::now();
    for m in 1..=markets {
        let mut p = b"sub".to_vec();
        p.extend_from_slice(&m.to_be_bytes());
        let _ = ov.iterate_cf(CF_NATIVE_ORACLE, Some(&p)).unwrap();
    }
    let t_iter = ms(t);
    let t = Instant::now();
    let mut usable = 0;
    for m in 1..=markets {
        if ctx.oracle.get_price(m, ts).ok().and_then(|p| p.usable()).is_some() {
            usable += 1;
        }
    }
    let t_markets = ms(t);
    let t = Instant::now();
    let traders = liq::traders_after(&ov, None, liq::LIQ_SCAN_PER_BLOCK + 2).unwrap();
    let t_walk = ms(t);
    let t = Instant::now();
    let mut npos = 0usize;
    for tr in &traders {
        npos += ctx.positions.positions_for_trader(tr).unwrap().len();
        let _ = ctx.positions.get_native_balance(tr).unwrap();
    }
    let t_val = ms(t);
    format!(
        "split: prefix_exists(sub)={t_pe:.2} prune_read={t_prune_read:.2}(rows {subs}) listed={t_listed:.2}(n {}) \
         per_market_sub_iter={t_iter:.2} per_market_get_price={t_markets:.2}(usable {usable}) traders_after={t_walk:.2}(n {}) \
         positions+balance={t_val:.2}(positions {npos})",
        listed.len(),
        traders.len()
    )
}

fn run() {
    let senders = env("UB_SENDERS", 5000);
    let markets = env("UB_MARKETS", 300);
    let actions = env("UB_ACTIONS", 60);
    let load = env("UB_LOAD", 150);
    let drain_to = env("UB_DRAIN_TO", 420);
    let feed_every = env("UB_FEED_EVERY", 26);
    let seed_traders = env("UB_SEED_TRADERS", 0);
    let seed_pos = env("UB_SEED_POS", 250);
    let mode = std::env::var("UB_DRAIN").unwrap_or_else(|_| "stale".into());
    let fed = mode != "none";
    let mut walk = MarkWalk::new(markets, env("UB_MARK_WALK", 0));

    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    feed_setup(&db, markets); // markets listed in every mode (as on the cells)
    let resident = env("UB_NO_R", 0) != 1;
    let mut chain = Chain {
        db: db.clone(),
        parent: None,
        books: HashMap::new(),
        next_id: 1,
        holder: ResidentBooks::default(),
        resident,
    };
    let mut ts: u64 = 1_000_000;
    let t_load = Instant::now();
    if seed_traders > 0 {
        let pm = torus_core::position::PositionManager::new(db.clone());
        let mut rng = Lcg(7);
        for i in 0..seed_traders {
            let t = sender(i);
            pm.put_native_balance(
                &t,
                &NativeBalance { available: FixedPoint::from_raw(100_000_000 * FixedPoint::SCALE), order_margin: FixedPoint::ZERO },
            )
            .unwrap();
            for k in 0..seed_pos {
                let m = 1 + (i * 7 + k) % markets;
                pm.put_position(&Position {
                    trader: t,
                    market_id: m,
                    is_long: rng.chance(500),
                    size: FixedPoint::from_raw((1 + rng.below(5) as i128) * FixedPoint::SCALE),
                    entry_price: FixedPoint::from_raw(TARGET * LEV * FixedPoint::SCALE),
                    realized_pnl: FixedPoint::ZERO,
                    isolated_margin: FixedPoint::ZERO,
                    margin_type: MarginType::Cross,
                })
                .unwrap();
            }
        }
        // one fed block so marks exist
        let mut ov = NativeStateOverlay::with_parent(db.clone(), None);
        let rows = chain.begin(&mut ov, 1);
        let mut ctx = chain.ctx(&ov, 1, ts);
        if fed {
            submit_all(&ctx, markets, &mut walk);
        }
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        chain.finish(ctx, ov, 1, rows);
        chain.barrier();
    } else {
        {
            let pm = torus_core::position::PositionManager::new(db.clone());
            for i in 0..senders {
                pm.put_native_balance(
                    &sender(i),
                    &NativeBalance { available: FixedPoint::from_raw(100_000_000 * FixedPoint::SCALE), order_margin: FixedPoint::ZERO },
                )
                .unwrap();
            }
        }
        let mut rng = Lcg(0x5EED_0089);
        let mut open: HashMap<u64, u64> = HashMap::new();
        // round-robin senders so every sender trades (cell: 5000 senders all active)
        let mut next_s = 0u64;
        for h in 1..=load {
            let mut block = Vec::new();
            for _ in 0..actions {
                let s = next_s % senders;
                next_s += 1;
                let o = open.entry(s).or_insert(0);
                let a = if *o + 400 > 900 || rng.chance(50) {
                    *o = 0;
                    NativeAction::CancelAllOrders { market_id: None }
                } else {
                    *o += 400;
                    NativeAction::PlaceOrderBatch(
                        (0..400)
                            .map(|_| {
                                let m = 1 + rng.below(markets);
                                econ_order(&mut rng, s, m)
                            })
                            .collect(),
                    )
                };
                block.push((sender(s), a));
            }
            if h % 4 == 0 {
                ts += 1;
            }
            let boundary = h % EPOCH == 0;
            if boundary {
                chain.barrier();
            }
            let mut ov = NativeStateOverlay::with_parent(db.clone(), chain.parent.clone());
            let rows = chain.begin(&mut ov, h);
            let mut ctx = chain.ctx(&ov, h, ts);
            if fed && h % env("UB_LOAD_FEED_EVERY", 8) == 1 % env("UB_LOAD_FEED_EVERY", 8) {
                submit_all(&ctx, markets, &mut walk);
            }
            let _ = NativeExecutor::begin_block_oracle(&mut ctx);
            NativeExecutor::execute_batch(&mut ctx, &block);
            let _ = NativeExecutor::run_liquidations(&mut ctx);
            NativeExecutor::process_governance(&mut ctx);
            let _ = NativeExecutor::process_epoch_boundary(&mut ctx);
            assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
            if h % 25 == 0 {
                eprintln!("load h={h} fills={} t={:.1}s", ctx.trade_index, t_load.elapsed().as_secs_f64());
            }
            chain.finish(ctx, ov, h, rows);
        }
    }
    let holders = liq::traders_after(&db, None, usize::MAX).unwrap().len();
    println!(
        "UBENCH mode={mode} load={:.1}s position_holders={holders} markets={markets} resident_rows={resident} R rows={} bytes={} builds={}",
        t_load.elapsed().as_secs_f64(),
        chain.holder.rows().map_or(0, |r| r.len()),
        chain.holder.rows().map_or(0, |r| r.bytes()),
        chain.holder.rows_builds()
    );

    // ---- drain: empty blocks ----
    let first = if seed_traders > 0 { 2 } else { load + 1 };
    if mode == "stale" {
        ts += 300; // > 60 s mark age (cell: drain blocks minutes after the last submission)
    }
    let mut rows: Vec<(u64, bool, f64, f64, f64, f64, f64, f64, f64)> = Vec::new();
    for h in first..=drain_to {
        if h % 13 == 0 {
            ts += 1;
        }
        let boundary = h % EPOCH == 0;
        if boundary || h % EPOCH <= 2 {
            println!("  h={h} {}", split(&db, chain.parent.clone(), markets, h, ts));
        }
        let t_block = Instant::now();
        let t = Instant::now();
        if boundary {
            chain.barrier();
        }
        let barrier_ms = ms(t);
        let mut ov = NativeStateOverlay::with_parent(db.clone(), chain.parent.clone());
        let feed_now = mode == "fresh" && h % feed_every == 0;
        let t = Instant::now();
        let due = NativeExecutor::oracle_due(&ov).unwrap() || NativeExecutor::liquidation_due(&ov).unwrap();
        let due_ms = ms(t);
        let run_native = boundary || due || feed_now;
        if !run_native {
            // marker-only block (the parent flush is W's work on the node)
            rows.push((h, false, due_ms, 0.0, 0.0, 0.0, 0.0, 0.0, ms(t_block)));
            drop(ov);
            chain.marker_only(h);
            continue;
        }
        let t = Instant::now();
        let resident_block = chain.begin(&mut ov, h);
        let mut ctx = chain.ctx(&ov, h, ts);
        let ctx_ms = ms(t);
        if feed_now {
            submit_all(&ctx, markets, &mut walk);
        }
        // begin_block_oracle, split (same calls in the same order as oracle_inputs +
        // aggregate_oracle_prices)
        let t = Instant::now();
        let pruned = ctx.oracle.prune_submissions(ctx.timestamp).unwrap();
        let prune_ms = ms(t);
        let t = Instant::now();
        let listed = ctx.governance.listed_market_ids().unwrap();
        let stakes: Vec<(Address, FixedPoint)> = ctx
            .staking
            .all_validators()
            .unwrap()
            .into_iter()
            .filter(|v| v.status == ValidatorStatus::Active)
            .map(|v| {
                let wei = U256::from(10u64).pow(U256::from(18u64));
                let p: u64 = (v.total_stake() / wei).try_into().unwrap_or(u64::MAX);
                (v.address, FixedPoint::from_raw(i128::from(p) * FixedPoint::SCALE))
            })
            .collect();
        let inputs_ms = ms(t);
        let t = Instant::now();
        let agg = NativeExecutor::aggregate_oracle_prices(&mut ctx, &listed, &stakes);
        let agg_ms = ms(t);
        let oracle_ms = prune_ms + inputs_ms + agg_ms;
        let agg_ok = agg.iter().filter(|r| r.success).count();
        let t = Instant::now();
        let liq_res = NativeExecutor::run_liquidations(&mut ctx);
        let liq_ms = ms(t);
        let t = Instant::now();
        NativeExecutor::process_governance(&mut ctx);
        let _ = NativeExecutor::distribute_fees(&mut ctx, 0);
        let gov_ms = ms(t);
        let t = Instant::now();
        let _ = NativeExecutor::process_epoch_boundary(&mut ctx);
        let epoch_ms = ms(t);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let flush_ms = chain.finish(ctx, ov, h, resident_block);
        let total = ms(t_block);
        if boundary || h % EPOCH <= 3 || h < first + 3 {
            println!(
                "  h={h} native barrier={barrier_ms:.2} ctx={ctx_ms:.2} due={due_ms:.2} oracle={oracle_ms:.2}[prune={prune_ms:.2}({pruned}) inputs={inputs_ms:.2} agg={agg_ms:.2}(ok {agg_ok})] liq={liq_ms:.2}(results {}) gov={gov_ms:.2} epoch={epoch_ms:.2} flush={flush_ms:.2} total={total:.2}",
                liq_res.len()
            );
        }
        rows.push((h, true, due_ms, oracle_ms, liq_ms, gov_ms, epoch_ms, flush_ms, total));
    }
    println!(
        "UBENCH R after drain: builds={} shared_fallbacks={} height={:?}",
        chain.holder.rows_builds(),
        chain.holder.rows_shared_fallbacks(),
        chain.holder.rows_height()
    );
    // Cold R build at this size, as a node builds it (DB + parent layer).
    let ov = NativeStateOverlay::with_parent(db.clone(), chain.parent.clone());
    let t = Instant::now();
    let cold = torus_state::ResidentRows::build(&ov).expect("R build");
    let build_ms = ms(t);
    println!(
        "UBENCH R cold build: rows={} bytes={} build_ms={build_ms:.1} ms_per_1M_rows={:.0}",
        cold.len(),
        cold.bytes(),
        build_ms * 1e6 / cold.len().max(1) as f64
    );
    if let Some(carried) = chain.holder.rows() {
        assert_eq!(carried, &cold, "R carried through the drain == cold build");
    }
    let class = |r: &(u64, bool, f64, f64, f64, f64, f64, f64, f64)| match r.0 % EPOCH {
        0 => "x00",
        1 => "x01",
        2 => "x02",
        _ => "other",
    };
    // The stale-drain prune rides block `first`'s frozen set, which marker-only
    // blocks never flush: it reaches the DB at the first drain boundary.
    // `other-late` = the `other` blocks after it (post-flush steady state).
    let first_boundary = first.div_ceil(EPOCH) * EPOCH;
    for c in ["x00", "x01", "x02", "other", "other-late"] {
        let v: Vec<_> = rows
            .iter()
            .filter(|r| match c {
                "other-late" => class(r) == "other" && r.0 > first_boundary,
                _ => class(r) == c,
            })
            .collect();
        if v.is_empty() {
            continue;
        }
        let n = v.len() as f64;
        let nat = v.iter().filter(|r| r.1).count();
        let avg = |f: fn(&&(u64, bool, f64, f64, f64, f64, f64, f64, f64)) -> f64| v.iter().map(f).sum::<f64>() / n;
        println!(
            "SUMMARY mode={mode} resident_rows={resident} {c}: n={} native={nat} due={:.3} oracle={:.2} liq={:.2} gov={:.3} epoch={:.2} flush={:.2} total={:.2} ms (avg)",
            v.len(),
            avg(|r| r.2),
            avg(|r| r.3),
            avg(|r| r.4),
            avg(|r| r.5),
            avg(|r| r.6),
            avg(|r| r.7),
            avg(|r| r.8)
        );
    }
}

#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn ubench_epoch() {
    run();
}
