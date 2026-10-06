//! s18 µbench (profiling only): the cost of one ADL (account, market) step of
//! the liquidation step (`liquidation_step.rs` `adl_account`) as the S=750
//! liq-stress cell saw it (~26 ms/step).
//!
//! Setup: markets `1..=300` listed with the oracle feed of `ubench_econ`
//! (`econ_load::feed_setup`); `UB_ADL_TRADERS` traders (`sender(i)`) with one
//! Cross position (size 1 @ the mid) in every market, long when `i + m` is
//! even; `UB_ADL_BANKRUPT` accounts at the lowest addresses (scanned first)
//! long size 1 in markets `1..=UB_ADL_POSITIONS`, balanced by a short sink
//! account at the top of the key space. Block 1 (marks at the mid) is frozen
//! as the parent layer; block 2 (marks 1,000 below the mid: every bankrupt
//! account's AV < 0 -> ADL, every trader healthy) runs `run_liquidations`
//! through `NativeStateOverlay` with the resident rows R attached, like the
//! node (`UB_NO_R=1`: without R).
//!
//! Per-step time: the gaps between consecutive `liquidation: ADL` info events
//! of one account (one event per (account, market) step, the same deltas the
//! liq-stress logs gave). Also prints direct timings of the step's parts over
//! the block-2 state before the step: `liq::adl_candidates` (65,536-row
//! window, AV closure returning 0) and the `adl_rest` reads
//! (`positions_for_trader` + `AccountView::build`).
//!
//!   UB_ADL_TRADERS=5000 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture

#[path = "common/econ_load.rs"]
mod econ_load;

use alloy_primitives::Address;
use econ_load::{base_mark, env, feed_setup, sender, special, REPORTERS};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use torus_bridge::native_executor::{begin_resident, end_resident, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::liquidation::{self as liq, ADL_MAX_SCAN_ROWS};
use torus_core::margin::AccountView;
use torus_core::position::{MarginType, NativeBalance};
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::FixedPoint;

const MARKETS: u64 = 300;

/// Records the instant of every `liquidation: ADL` info event (the only event
/// with a `counterparties` field); debug events are disabled, as on the rig.
struct AdlClock(Arc<Mutex<Vec<Instant>>>);

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
        if e.metadata().fields().field("counterparties").is_some() {
            self.0.lock().unwrap().push(Instant::now());
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
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[(s.len() - 1) * p / 100]
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

#[test]
#[ignore = "µbench (profiling) — run with --ignored --nocapture on a quiet box"]
fn ubench_adl_step() {
    let n = env("UB_ADL_TRADERS", 1000);
    let k = env("UB_ADL_BANKRUPT", 1);
    let pu = env("UB_ADL_POSITIONS", 270);
    let resident = env("UB_NO_R", 0) != 1;
    let mid = base_mark();
    let low = mid - fp(1_000);

    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    feed_setup(&db, MARKETS);
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
            // AV = 100,000 - pu x 1,000 < 0 at `low` -> ADL.
            fund(&bankrupt(b), fp(100_000));
            for m in 1..=pu {
                fill(&bankrupt(b), m, true);
                fill(&sink, m, false);
            }
        }
    }
    let rows = db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap().len();
    println!(
        "ADL setup: traders={n} bankrupt={k} positions/bankrupt={pu} R={resident} position rows={rows} \
         (window {ADL_MAX_SCAN_ROWS}: {}) setup_ms={:.0}",
        if rows > ADL_MAX_SCAN_ROWS { "capped" } else { "whole CF" },
        ms(t)
    );

    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut holder = ResidentBooks::default();
    let mut parent = None;
    for h in 1..=2u64 {
        let mark = if h == 1 { mid } else { low };
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rows = begin_resident(resident.then_some(&mut holder), &mut overlay, h, None);
        let mut ctx = NativeExecContext::new(
            overlay.clone(), h + 1, 1000 + h, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        ctx.attach_resident_block(&mut rows);
        ctx.metrics = Some(metrics.clone());
        for m in 1..=MARKETS {
            for r in REPORTERS {
                ctx.oracle.submit_price(&special(r), m, mark, ctx.block_height, ctx.timestamp).unwrap();
            }
        }
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(agg.iter().all(|r| r.success), "mark aggregation: {agg:?}");
        if h == 2 {
            parts(&ctx, low, pu);
            let clock = Arc::new(Mutex::new(Vec::new()));
            let t = Instant::now();
            tracing::subscriber::with_default(AdlClock(clock.clone()), || {
                let _ = NativeExecutor::run_liquidations(&mut ctx);
            });
            let total = ms(t);
            assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
            report(&clock.lock().unwrap(), k as usize, pu as usize, total);
            assert_eq!(metrics.liquidations_adl.get(), k, "every bankrupt account ADL'd");
        }
        ctx.detach_resident_block(&mut rows);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = if rows.attached() { overlay.own_pending_delta() } else { Default::default() };
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rows, &mut overlay, delta, true, Some(&metrics));
        if let Some(p) = parent.take() {
            let p: Arc<torus_state::FrozenPending> = p;
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
}

/// Direct timings of the step's parts over the block-2 state (before the
/// step): the counterparty walk alone and the `adl_rest` reads.
fn parts<T: StateBackend>(ctx: &NativeExecContext<T>, low: FixedPoint, pu: u64) {
    let u = bankrupt(0);
    let reps = 20u64.min(pu);
    let t = Instant::now();
    let mut found = 0;
    for m in 1..=reps {
        found += liq::adl_candidates(&ctx.positions, m, &u, false, ADL_MAX_SCAN_ROWS, |_| Ok(FixedPoint::ZERO))
            .unwrap()
            .len();
    }
    let walk = ms(t) / reps as f64;
    let marks: BTreeMap<u64, FixedPoint> = (1..=MARKETS).map(|m| (m, low)).collect();
    let t = Instant::now();
    for m in 1..=reps {
        let bal = ctx.positions.get_native_balance(&u).unwrap();
        let others: Vec<_> =
            ctx.positions.positions_for_trader(&u).unwrap().into_iter().filter(|q| q.market_id != m).collect();
        let _ = AccountView::build(&bal, &others, |x| marks.get(&x).copied(), |_| None);
    }
    let rest = ms(t) / reps as f64;
    println!(
        "ADL parts (direct, {reps} calls): adl_candidates walk={walk:.3} ms/call ({} candidates/call) \
         adl_rest reads+build={rest:.3} ms/call",
        found as u64 / reps
    );
}

/// Per-step gaps (ms) between consecutive ADL events of the same account.
fn report(events: &[Instant], k: usize, pu: usize, total: f64) {
    assert_eq!(events.len(), k * pu, "one ADL event per (account, market)");
    let gaps = |a: usize| -> Vec<f64> {
        events[a * pu..(a + 1) * pu].windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1e3).collect()
    };
    let all: Vec<f64> = (0..k).flat_map(gaps).collect();
    let mean = all.iter().sum::<f64>() / all.len() as f64;
    let (first, last) = (gaps(0), gaps(k - 1));
    println!(
        "ADL steps={} ms/step: median={:.3} p10={:.3} p90={:.3} mean={:.3} | first account median={:.3} \
         last account median={:.3} (x{:.2}) | run_liquidations total_ms={total:.0}",
        all.len(),
        pct(&all, 50),
        pct(&all, 10),
        pct(&all, 90),
        mean,
        pct(&first, 50),
        pct(&last, 50),
        pct(&last, 50) / pct(&first, 50),
    );
}
