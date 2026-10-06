//! s87 µbench: econ-shaped load like tools/bench-throughput `--econ` (GTC
//! limits only, one side per (sender, market), mid = 20 x target, band 5,
//! cross 0.5, cancel-all 5% + open-order budget 900, batches of 400 orders per
//! action), run block by block through NativeStateOverlay layered over the
//! previous block's frozen set (the node's pipelined exec path).
//! Prints engine ms per 1k fills (execute_batch [+ run_liquidations]).
//!
//! `UB_MARKS=1`: markets 1..=UB_MARKETS are listed and three Active validators
//! submit the mid (`TARGET * LEV`) for every market each block, so
//! `begin_block_oracle` aggregates a usable mark (a fed chain: margin and the
//! liquidation scan value accounts). Default: no listed market, no marks.
//! `UB_MARK_WALK=<bp>` (with `UB_MARKS=1`, item 6 step 0.5): the submitted
//! mark of every market walks `±bp` per block around the mid (bounded, mean-
//! reverting, deterministic: `common/econ_load.rs` `MarkWalk`), so the mark
//! changes every block. Default 0: the mid every block, as before.
//!
//! Item 6 Phase 1 (C1): each block attaches the resident rows R like app.rs
//! (`begin_resident` / `end_resident`, outside the timed window; their cost
//! is printed as `r_end/blk`). `UB_NO_R=1` runs without R (today's path) for
//! A/B pairs on one binary. Each run also times a cold R build over the final
//! DB (`R rows / bytes / build_ms`).
//! `UB_R_WORKER=1` (item 6 step 2): `end_resident_on_worker` instead of
//! `end_resident`, joined by the next block's `begin_resident` (`end_resident/blk` is
//! then the hand-over only; `timer/blk` and `wait/blk` come from the
//! worker's and the join's timers). Item 6 cut 5: the worker takes the
//! block's delta from the frozen set, as app.rs does pipelined (`r_end/blk`
//! then holds no `own_pending_delta`). The harness has no pre-native work
//! between the blocks, so this shows equal results, not the node's overlap.
//!
//!   cargo test -p torus-bridge --release --test ubench_econ -- --ignored --nocapture

#[path = "common/econ_load.rs"]
mod econ_load;

use econ_load::{base_mark, env, feed_setup, sender, special, Gen, Lcg, MarkWalk, REPORTERS};
use std::collections::HashMap;
use std::sync::Arc;
use torus_bridge::native_executor::{
    begin_resident, end_resident, end_resident_on_worker, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{NativeStateOverlay, ResidentRows, StateBackend, StateDb};
use torus_types::FixedPoint;

struct Sample {
    exec_ms: f64,
    tail_ms: f64,
    margin_ms: f64,
    match_ms: f64,
    settle_ms: f64,
    /// `own_pending_delta` + `end_resident` (R's end-of-block upkeep).
    r_end_ms: f64,
    /// `end_resident` alone (item 6 step 1).
    end_ms: f64,
    fills: u64,
}

/// A cold R build over the final DB: rows, bytes, ms.
struct RBuild {
    rows: usize,
    bytes: usize,
    ms: f64,
}

/// Item 6 step 1: a histogram's `_sum` from the metrics text (`None`: the
/// binary does not have it).
fn hist_sum(metrics: &torus_telemetry::Metrics, name: &str) -> Option<f64> {
    let key = format!("torus_{name}_seconds_sum ");
    metrics.encode().lines().find_map(|l| l.strip_prefix(&key).and_then(|v| v.trim().parse().ok()))
}

/// Item 6 step 1: end_resident's sub-timers (ms, summed over the measured
/// blocks): R applying the delta, the decoded positions + sums carry; step 2:
/// the whole of end_resident (worker side) and the join's wait.
const END_SUBS: [&str; 4] =
    ["exec_end_resident_rows", "exec_end_resident_positions", "exec_end_resident", "exec_end_resident_wait"];

fn run_once(seed: u64) -> (Vec<Sample>, u64, u64, u64, RBuild, Vec<Option<f64>>) {
    let senders = env("UB_SENDERS", 600);
    let markets = env("UB_MARKETS", 300);
    let actions = env("UB_ACTIONS", 60);
    let warm = env("UB_WARM", 40);
    let measure = env("UB_MEASURE", 6);
    let fed = env("UB_MARKS", 0) == 1;
    let mut walk = MarkWalk::new(markets, env("UB_MARK_WALK", 0));
    let resident = env("UB_NO_R", 0) != 1;
    let worker = env("UB_R_WORKER", 0) == 1;
    let mut holder = ResidentBooks::default();

    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    {
        let ctx = NativeExecContext::new(
            db.clone(), 1, 1000, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        for i in 0..senders {
            ctx.positions
                .put_native_balance(
                    &sender(i),
                    &NativeBalance {
                        available: FixedPoint::from_raw(100_000_000 * FixedPoint::SCALE),
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
    }
    if fed {
        feed_setup(&db, markets);
    }
    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut gen = Gen {
        rng: Lcg(0x5EED_0087 ^ seed),
        senders,
        markets,
        batch: env("UB_BATCH", 400),
        budget: env("UB_BUDGET", 900),
        open: HashMap::new(),
    };
    let mut books = HashMap::new();
    let mut next_id: u128 = 1;
    let mut parent = None;
    let mut samples = Vec::new();
    let mut rc_measured = 0u64;
    let mut placed_measured = 0u64;
    let mut subs0: Vec<Option<f64>> = vec![None; END_SUBS.len()];
    for h in 1..=(warm + measure) {
        let block = gen.block(actions);
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rows = begin_resident(resident.then_some(&mut holder), &mut overlay, h, None);
        if h == warm + 1 {
            // After the join of block `warm`'s worker (step 2).
            subs0 = END_SUBS.iter().map(|n| hist_sum(&metrics, n)).collect();
        }
        let mut ctx = NativeExecContext::new(
            overlay.clone(),
            h + 1,
            1000 + h,
            0,
            1_000_000,
            100,
            special(99),
            special(100),
            special(101),
        );
        ctx.attach_resident_block(&mut rows);
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        ctx.metrics = Some(metrics.clone());
        if fed {
            // Before the timed block start, stamped with this block (s85 feeder M1b).
            walk.step();
            for m in 1..=markets {
                let mark = walk.mark(base_mark(), m);
                for n in REPORTERS {
                    ctx.oracle.submit_price(&special(n), m, mark, ctx.block_height, ctx.timestamp).unwrap();
                }
            }
        }
        let rc0 = metrics.orders_rejected_cancelled.get();
        let pa0 = metrics.orders_placed_accepted.get();
        let t0 = std::time::Instant::now();
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(!fed || agg.iter().all(|r| r.success), "mark aggregation: {agg:?}");
        NativeExecutor::execute_batch(&mut ctx, &block);
        let exec = t0.elapsed();
        let t1 = std::time::Instant::now();
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        let tail = t1.elapsed();
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let pa = ctx.phase_accum;
        let fills = ctx.trade_index as u64;
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut rows);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let t2 = std::time::Instant::now();
        // Item 6 cut 5: with the worker, the delta comes from the frozen set
        // on the worker (app.rs, pipelined); inline, before the freeze.
        let delta = if rows.attached() && !worker { overlay.own_pending_delta() } else { Default::default() };
        let frozen = overlay.freeze(h);
        let t3 = std::time::Instant::now();
        if worker {
            let delta = if rows.attached() {
                torus_bridge::native_executor::BlockDelta::Frozen(frozen.clone())
            } else {
                delta.into()
            };
            end_resident_on_worker(&mut holder, rows, &mut overlay, delta, true, Some(metrics.clone()));
        } else {
            end_resident(&mut holder, rows, &mut overlay, delta, true, Some(&metrics));
        }
        let end = t3.elapsed();
        let r_end = t2.elapsed();
        if h > warm {
            rc_measured += metrics.orders_rejected_cancelled.get() - rc0;
            placed_measured += metrics.orders_placed_accepted.get() - pa0;
            samples.push(Sample {
                exec_ms: exec.as_secs_f64() * 1e3,
                tail_ms: tail.as_secs_f64() * 1e3,
                margin_ms: pa.margin_ns as f64 / 1e6,
                match_ms: pa.match_ns as f64 / 1e6,
                settle_ms: pa.settle_ns as f64 / 1e6,
                r_end_ms: r_end.as_secs_f64() * 1e3,
                end_ms: end.as_secs_f64() * 1e3,
                fills,
            });
        }
        if let Some(p) = parent.take() {
            let p: Arc<torus_state::FrozenPending> = p;
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    if resident {
        assert_eq!(holder.rows_builds(), 1, "R built once");
        assert_eq!(holder.rows_shared_fallbacks(), 0);
    }
    if let Some(p) = parent.take() {
        p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
    }
    let t = std::time::Instant::now();
    let r = ResidentRows::build(&db).expect("R build");
    let r_build = RBuild { rows: r.len(), bytes: r.bytes(), ms: t.elapsed().as_secs_f64() * 1e3 };
    if let Some(held) = holder.rows() {
        assert_eq!(held, &r, "carried R == cold build over the final DB");
    }
    let resting: usize = books.values().map(|b: &torus_core::order_book::OrderBook| b.order_count()).sum();
    let subs = END_SUBS
        .iter()
        .zip(subs0)
        .map(|(n, a)| Some((hist_sum(&metrics, n)? - a?) * 1e3))
        .collect();
    (samples, rc_measured, placed_measured, resting as u64, r_build, subs)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore = "µbench — run with --ignored --nocapture on a quiet box"]
fn ubench_econ() {
    let runs = env("UB_RUNS", 3);
    let mut per_run = Vec::new();
    for r in 0..runs {
        let (s, rc, placed, resting, rb, subs) = run_once(r);
        let fills: u64 = s.iter().map(|x| x.fills).sum();
        let exec: f64 = s.iter().map(|x| x.exec_ms).sum();
        let tail: f64 = s.iter().map(|x| x.tail_ms).sum();
        let margin: f64 = s.iter().map(|x| x.margin_ms).sum();
        let mtch: f64 = s.iter().map(|x| x.match_ms).sum();
        let settle: f64 = s.iter().map(|x| x.settle_ms).sum();
        let r_end: f64 = s.iter().map(|x| x.r_end_ms).sum();
        let end: f64 = s.iter().map(|x| x.end_ms).sum();
        let n = s.len() as f64;
        let sub = |i: usize| subs[i].map_or("-".to_string(), |v: f64| format!("{:.2}", v / n));
        let k = fills as f64 / 1e3;
        let per1k = (exec + tail) / k;
        println!(
            "UB run={r} blocks={} fills/blk={:.0} engine_ms/blk={:.1} engine_ms/1k_fills={:.2} \
             [exec/1k={:.2} margin/1k={:.2} match/1k={:.2} settle/1k={:.2} tail(liq)/1k={:.2}] \
             rejected_cancelled={} accepted={} resting_end={} r_end/blk={:.2}ms \
             [end_resident/blk={:.2}ms rows/blk={}ms positions/blk={}ms timer/blk={}ms wait/blk={}ms] \
             R rows={} bytes={} build_ms={:.1} build_ms_per_1M_rows={:.0}",
            s.len(),
            fills as f64 / n,
            (exec + tail) / n,
            per1k,
            exec / k,
            margin / k,
            mtch / k,
            settle / k,
            tail / k,
            rc,
            placed,
            resting,
            r_end / n,
            end / n,
            sub(0),
            sub(1),
            sub(2),
            sub(3),
            rb.rows,
            rb.bytes,
            rb.ms,
            rb.ms * 1e6 / rb.rows.max(1) as f64,
        );
        per_run.push(per1k);
    }
    println!(
        "UB marks={} walk_bp={} resident_rows={} r_worker={} MEDIAN engine_ms/1k_fills={:.2} runs={:?}",
        env("UB_MARKS", 0) == 1,
        env("UB_MARK_WALK", 0),
        env("UB_NO_R", 0) != 1,
        env("UB_R_WORKER", 0) == 1,
        median(per_run.clone()),
        per_run
    );
}

/// `UB_MARK_WALK`: 0 bp submits the mid every block (the bench as before);
/// 10 bp moves every market by exactly 10 bp a block, inside ±80 bp, on the
/// offsets `bench-throughput oracle-feed --walk-bp 10` pins
/// (`walk_offsets_are_pinned` in tools/bench-throughput/src/oracle_feed.rs).
#[test]
fn mark_walk_zero_is_the_mid_and_offsets_are_pinned() {
    let mut still = MarkWalk::new(300, 0);
    let mut walk = MarkWalk::new(300, 10);
    let base = base_mark();
    let mut market_1 = Vec::new();
    let mut prev = vec![0i64; 301];
    for block in 0..2_000 {
        still.step();
        walk.step();
        for m in 1..=300 {
            assert_eq!(still.mark(base, m), base, "block {block} market {m}");
            let x = walk.offset_bp(m);
            assert!(x.abs() <= 80 && (x - prev[m as usize]).abs() == 10, "block {block} market {m}: {x}");
            prev[m as usize] = x;
            assert_eq!(walk.mark(base, m).raw(), base.raw() * i128::from(10_000 + x) / 10_000);
        }
        if block < 12 {
            market_1.push(walk.offset_bp(1));
        }
    }
    assert_eq!(market_1, [10, 0, -10, -20, -30, -20, -10, 0, -10, -20, -10, 0]);
}
