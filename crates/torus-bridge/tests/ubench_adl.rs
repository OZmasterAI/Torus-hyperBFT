//! adl-budget A8 µbench (sizing W, profiling): the P2 ADL flow — block B
//! (every marked position of a bankrupt account moves to the ADL escrow of
//! its side, one `0x07` obligation row per (account, market)) and the drain
//! (the escrows close the rows against the ranked real holders under
//! `UB_ADL_WORK` units per block, default `ADL_WORK_PER_BLOCK`).
//!
//! Setup: markets `1..=300` listed with the oracle feed of `ubench_econ`
//! (`econ_load::feed_setup`); `UB_ADL_TRADERS` traders (`sender(i)`) with one
//! Cross position (size 1 @ the mid) in every market, long when `i + m` is
//! even; `UB_ADL_BANKRUPT` accounts at the lowest addresses (scanned first)
//! long size 1 in markets `1..=UB_ADL_POSITIONS` (one side: every row goes to
//! the long escrow), balanced by a short sink account at the top of the key
//! space. Block 1 (marks at the mid) runs the step on a healthy book (the
//! scan baseline; it also writes the rule-H mark rows); from block 2 on
//! (marks 1,000 below the mid: every bankrupt account's AV < 0 -> ADL, every
//! trader healthy) `run_liquidations_with(scan, act, UB_ADL_WORK)` runs
//! through `NativeStateOverlay` with the resident rows R attached, like the
//! node (`UB_NO_R=1`: without R), until no obligation row is left and every
//! bankrupt account was ADL'd (cap 100,000 blocks), then one more block (the
//! post-drain scan baseline).
//!
//! Per block: `B_ms` = step start -> the block's last `liquidation: ADL to
//! escrow` event (classification + transfers of the ADL'd accounts, which
//! sort first); `step_ms` / `adl_work` / `rows` = the `liquidation step`
//! line's `ms` / `adl_work` / `adl_obligations`. Summary: drain ms = step ms
//! − B ms − the healthy traders the block classified × the post-drain
//! block's ms per classified trader; ns/unit = drain ms / adl_work.
//!
//! `UB_ADL_HL=1` (the W sizing case, adl-budget-impl A8): 3 bankrupt accounts
//! in markets 1..=100 (300 rows over 100 (market, long) keys); asserts the
//! escrows are closed after block B.
//!
//! Asserts: per-block units <= W + the largest row cost (1 visit + a ranking
//! of every trader + reads; no edge rows here); afterwards OI symmetric in
//! every market, both escrows and every bankrupt account flat at exactly 0.
//!
//!   UB_ADL_HL=1 UB_ADL_TRADERS=5000 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture
//!   UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270 cargo test ... (S=750-like)

#[path = "common/econ_load.rs"]
mod econ_load;

use alloy_primitives::Address;
use econ_load::{base_mark, env, feed_setup, sender, special, REPORTERS};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use torus_bridge::native_executor::{begin_resident, end_resident, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::liquidation as liq;
use torus_core::position::{MarginType, NativeBalance, Position};
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::FixedPoint;

const MARKETS: u64 = 300;

/// One block's events: the instants of the `liquidation: ADL to escrow`
/// events (the only info event with a `bankruptcy` field) and the
/// `liquidation step` line's fields (the only one with `adl_work`). Debug
/// events are disabled, as on the rig.
#[derive(Default)]
struct Seen {
    escrow: Vec<Instant>,
    step: Option<StepLine>,
}

#[derive(Default, Clone, Copy)]
struct StepLine {
    scanned: u64,
    adl: u64,
    adl_work: u64,
    adl_obligations: u64,
    ms: f64,
}

impl tracing::field::Visit for StepLine {
    fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
        match f.name() {
            "scanned" => self.scanned = v,
            "adl" => self.adl = v,
            "adl_work" => self.adl_work = v,
            "adl_obligations" => self.adl_obligations = v,
            _ => {}
        }
    }
    fn record_f64(&mut self, f: &tracing::field::Field, v: f64) {
        if f.name() == "ms" {
            self.ms = v;
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

struct AdlClock(Arc<Mutex<Seen>>);

impl tracing::Subscriber for AdlClock {
    fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
        *m.level() <= tracing::Level::INFO
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        let now = Instant::now();
        let fields = e.metadata().fields();
        if fields.field("bankruptcy").is_some() {
            self.0.lock().unwrap().escrow.push(now);
        } else if fields.field("adl_work").is_some() {
            let mut s = StepLine::default();
            e.record(&mut s);
            self.0.lock().unwrap().step = Some(s);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn bankrupt(k: u64) -> Address {
    let mut b = [0x10u8; 20];
    b[12..20].copy_from_slice(&(k + 1).to_be_bytes());
    Address::new(b)
}

fn fp(v: i128) -> FixedPoint {
    FixedPoint::from_raw(v * FixedPoint::SCALE)
}

fn pct(v: &[f64], p: usize) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[(s.len() - 1) * p / 100]
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

#[test]
#[ignore = "µbench (profiling) — run with --ignored --nocapture on a quiet box"]
fn ubench_adl_p2() {
    let hl = env("UB_ADL_HL", 0) == 1;
    let n = env("UB_ADL_TRADERS", 1000);
    let k = if hl { 3 } else { env("UB_ADL_BANKRUPT", 1) };
    let pu = if hl { 100 } else { env("UB_ADL_POSITIONS", 270) };
    let w = env("UB_ADL_WORK", liq::ADL_WORK_PER_BLOCK);
    let resident = env("UB_NO_R", 0) != 1;
    let (scan, act) = (liq::LIQ_SCAN_PER_BLOCK, liq::LIQ_ACT_PER_BLOCK);
    let mid = base_mark();
    let low = mid - fp(1_000);

    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    feed_setup(&db, MARKETS, false); // placeholder rows, as measured (§9)
    let t = Instant::now();
    {
        let ctx = NativeExecContext::new(
            db.clone(), 1, 1000, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        let fund = |a: &Address, amount: FixedPoint| {
            ctx.positions
                .put_native_balance(a, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
                .unwrap();
        };
        let fill = |a: &Address, m: u64, long: bool| {
            ctx.positions.apply_fill(a, m, long, FixedPoint::ONE, mid, MarginType::Cross).unwrap();
        };
        for i in 0..n {
            fund(&sender(i), fp(100_000_000));
            for m in 1..=MARKETS {
                fill(&sender(i), m, (i + m) % 2 == 0);
            }
        }
        let sink = special(0xF0);
        fund(&sink, fp(100_000_000));
        for b in 0..k {
            // MM = notional / 40 (default 20x): healthy at the mid (AV
            // 800 pu >= MM 750 pu), AV = 800 pu - 1,000 pu < 0 at `low` -> ADL.
            fund(&bankrupt(b), fp(800 * pu as i128));
            for m in 1..=pu {
                fill(&bankrupt(b), m, true);
                fill(&sink, m, false);
            }
        }
    }
    let rows = db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap().len();
    println!(
        "ADL setup: hl={hl} traders={n} bankrupt={k} positions/bankrupt={pu} obligations={} W={w} R={resident} \
         position rows={rows} setup_ms={:.0}",
        k * pu,
        ms(t)
    );

    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut holder = ResidentBooks::default();
    let mut parent = None;
    let (mut drained, mut tail) = (false, 0);
    let mut per_trader = Vec::new(); // baseline ms per classified healthy trader
    let mut blocks: Vec<(usize, f64, StepLine)> = Vec::new(); // (transfers, B ms, line)
    let mut h = 0u64;
    while tail < 1 {
        h += 1;
        assert!(h <= 100_000, "drain cap");
        let mark = if h == 1 { mid } else { low };
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rrows = begin_resident(resident.then_some(&mut holder), &mut overlay, h, None);
        let mut ctx = NativeExecContext::new(
            overlay.clone(), h + 1, 1000 + h, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        ctx.attach_resident_block(&mut rrows);
        ctx.metrics = Some(metrics.clone());
        for m in 1..=MARKETS {
            for r in REPORTERS {
                ctx.oracle.submit_price(&special(r), m, mark, ctx.block_height, ctx.timestamp).unwrap();
            }
        }
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(agg.iter().all(|r| r.success), "mark aggregation: {agg:?}");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let scanned0 = metrics.liquidation_scanned.get();
        let t0 = Instant::now();
        tracing::subscriber::with_default(AdlClock(seen.clone()), || {
            let _ = NativeExecutor::run_liquidations_with(&mut ctx, scan, act, w);
        });
        let total = ms(t0);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let seen = std::mem::take(&mut *seen.lock().unwrap());
        if drained {
            tail += 1;
        }
        match seen.step {
            None => {
                // Nothing acted on (block 1, the post-drain block): the scan
                // baseline.
                let scanned = metrics.liquidation_scanned.get() - scanned0;
                per_trader.push(total / scanned.max(1) as f64);
                println!("h={h} baseline: scanned={scanned} step_ms={total:.1}");
            }
            Some(s) => {
                let b_ms = seen.escrow.last().map_or(0.0, |e| (*e - t0).as_secs_f64() * 1e3);
                let row_cost = 1 + (n + k + 4) + 2 * (n + 4);
                assert!(s.adl_work <= w + row_cost, "h={h}: {} units > W {w} + {row_cost}", s.adl_work);
                println!(
                    "h={h} transfers={} B_ms={b_ms:.1} step_ms={:.1} adl={} scanned={} rows={} adl_work={}",
                    seen.escrow.len(),
                    s.ms,
                    s.adl,
                    s.scanned,
                    s.adl_obligations,
                    s.adl_work
                );
                blocks.push((seen.escrow.len(), b_ms, s));
            }
        }
        let queue_empty = liq::next_obligation(&ctx.state, &[liq::ADL_OBLIGATION_TAG]).unwrap().is_none();
        if !drained && h >= 2 && queue_empty && metrics.liquidations_adl.get() >= k {
            if hl {
                assert_eq!(blocks.len(), 1, "an HL-sized event closes the escrows in block B");
            }
            for a in (0..k).map(bankrupt).chain([liq::ADL_ESCROW_LONG, liq::ADL_ESCROW_SHORT]) {
                assert!(ctx.positions.positions_for_trader(&a).unwrap().is_empty(), "{a}: positions");
                let b = ctx.positions.get_native_balance(&a).unwrap();
                assert_eq!((b.available, b.order_margin), (FixedPoint::ZERO, FixedPoint::ZERO), "{a}: cash");
            }
            drained = true;
        }
        ctx.detach_resident_block(&mut rrows);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = if rrows.attached() { overlay.own_pending_delta() } else { Default::default() };
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rrows, &mut overlay, delta, true, Some(&metrics));
        if let Some(p) = parent.take() {
            let p: Arc<torus_state::FrozenPending> = p;
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    if let Some(p) = parent.take() {
        p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
    }

    // OI symmetric in every market.
    let mut oi: BTreeMap<u64, i128> = BTreeMap::new();
    for (key, v) in db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap() {
        if key.len() == 28 {
            let p: Position = borsh::from_slice(&v).unwrap();
            *oi.entry(p.market_id).or_default() += if p.is_long { p.size.raw() } else { -p.size.raw() };
        }
    }
    let skew: Vec<_> = oi.iter().filter(|(_, x)| **x != 0).collect();
    assert!(skew.is_empty(), "OI asymmetric: {skew:?}");

    // Summary. A block's healthy classified traders cost the baseline per
    // trader; the rest after B is the drain.
    // The post-drain block (warm, same marks); block 1 runs cold.
    let base = *per_trader.last().unwrap();
    let drain: Vec<(f64, u64)> = blocks
        .iter()
        .filter(|(_, _, s)| s.adl_work > 0)
        .map(|(_, b, s)| (s.ms - b - base * s.scanned.saturating_sub(s.adl) as f64, s.adl_work))
        .collect();
    let drain_ms: Vec<f64> = drain.iter().map(|d| d.0).collect();
    let ns_unit: Vec<f64> = drain.iter().map(|(d, u)| d * 1e6 / *u as f64).collect();
    let b_ms: Vec<String> = blocks.iter().filter(|b| b.0 > 0).map(|b| format!("{:.1}", b.1)).collect();
    let units: u64 = blocks.iter().map(|b| b.2.adl_work).sum();
    let steps: Vec<f64> = blocks.iter().map(|b| b.2.ms).collect();
    println!(
        "ADL summary: W={w} blocks_to_drain={} units_total={units} | step ms p50={:.1} p90={:.1} max={:.1} \
         | B ms {b_ms:?} | drain ms p50={:.1} p90={:.1} max={:.1} | ns/unit p50={:.1} min={:.1} max={:.1} \
         | scan baseline {base:.4} ms/trader",
        blocks.len(),
        pct(&steps, 50),
        pct(&steps, 90),
        pct(&steps, 100),
        pct(&drain_ms, 50),
        pct(&drain_ms, 90),
        pct(&drain_ms, 100),
        pct(&ns_unit, 50),
        pct(&ns_unit, 0),
        pct(&ns_unit, 100),
    );
}
