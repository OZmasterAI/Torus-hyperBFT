//! 8k L2.1 streamed in-order settle (`TORUS_SETTLE_STREAMED=1`, node-local,
//! off by default): each market's pool job matches the market AND computes
//! its settle plan (pass A), and the exec thread runs settle pass B market by
//! market, in market-id order, while the pool still works on later markets,
//! instead of waiting for every match, then every plan, then running pass B.
//!
//! THE contract under test: the flag changes WHEN pass B runs, never what it
//! does or in which order. Over streams of multi-market blocks (each block
//! the two `execute_batch` calls of a block, pre- and post-EVM, on one
//! `NativeStateOverlay` per block, flushed to the DB after it), with L2.2
//! (`TORUS_SETTLE_NO_MERGE`) off AND on, at worker caps 1, 2 and 8, with and
//! without a fill subscriber, and under a scheduling perturbation seam
//! (per-market sleeps in the pool jobs and on the exec thread), the flag-on
//! run must equal the flag-off run in:
//! - the backend write sequence: every put and delete of every CF, in issue
//!   order, recorded by a wrapper around the block overlay;
//! - the overlay's pending native set per block (`dirty_native_keys`);
//! - every `NativeActionResult` field, the gas, the fills and their extras;
//! - the fatal error, the DB at the end and the native state root.
//!
//! Fault streams: a plan panic (the parallel settle falls back to the
//! sequential loop; streamed, after a prefix of markets already went through
//! pass B), a one-shot position read failure in pass A (fallback, then the
//! R02 fail-stop), a failing position flush, and a match panic (the block's
//! fatal exit). Aliased opaque position rows, cancel-alls, the Auto settle
//! gate (sequential below the fill gate, parallel above) and the engine path
//! are covered too.
//!
//! The flags are pinned per thread (`with_settle_streamed`,
//! `with_settle_no_merge`), so all arms run in one process whatever the env
//! says. The flag-on arm must have streamed (`settle_streamed_*` counters).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    parse_settle_streamed, settle_streamed_calls_on_this_thread,
    settle_streamed_early_on_this_thread, settle_streamed_hybrid_on_this_thread,
    with_settle_no_merge, with_settle_stream_probe, with_settle_streamed, NativeActionResult,
    NativeExecContext, NativeExecutor, SettleStreamProbe,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_state::action_status::FailureReason;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
    CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::trade_rows::{FillExtras, TradeFill};
use torus_state::{AtomicWriteOp, NativeStateOverlay, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Recording, fault-injecting backend over the block overlay ----

/// One write, in issue order: (cf, key, `Some(value)` or a delete).
type Write = (String, Vec<u8>, Option<Vec<u8>>);

#[derive(Default)]
struct Faults {
    /// The n-th (1-based) point read of this position row in the block
    /// fails, once.
    read_nth: Option<(Vec<u8>, usize)>,
    /// Every write of this `(cf, key)` fails.
    write: Option<(String, Vec<u8>)>,
    hits: usize,
}

/// Wraps the block's `NativeStateOverlay`: records every write in order and
/// injects the armed faults (as `settle_no_merge_tests.rs`).
#[derive(Clone)]
struct Recorder {
    inner: NativeStateOverlay,
    log: Arc<Mutex<Vec<Write>>>,
    faults: Arc<Mutex<Faults>>,
    position_reads: Arc<Mutex<BTreeMap<Vec<u8>, usize>>>,
}

impl Recorder {
    fn write_fault(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        let mut f = self.faults.lock().unwrap();
        if f.write
            .as_ref()
            .is_some_and(|(c, k)| c == cf && k.as_slice() == key)
        {
            f.hits += 1;
            return Err(StateError::Io(std::io::Error::other(
                "injected write failure",
            )));
        }
        Ok(())
    }
}

impl StateBackend for Recorder {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_POSITIONS {
            let n = {
                let mut reads = self.position_reads.lock().unwrap();
                let n = reads.entry(key.to_vec()).or_default();
                *n += 1;
                *n
            };
            let mut f = self.faults.lock().unwrap();
            if f.read_nth
                .as_ref()
                .is_some_and(|(k, nth)| k.as_slice() == key && *nth == n)
            {
                f.read_nth = None;
                f.hits += 1;
                return Err(StateError::Io(std::io::Error::other(
                    "injected one-shot read failure",
                )));
            }
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.write_fault(cf, key)?;
        self.log
            .lock()
            .unwrap()
            .push((cf.to_string(), key.to_vec(), Some(value.to_vec())));
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.write_fault(cf, key)?;
        self.log
            .lock()
            .unwrap()
            .push((cf.to_string(), key.to_vec(), None));
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        for op in ops {
            match op {
                AtomicWriteOp::Put { cf, key, value } => {
                    self.write_fault(cf, key)?;
                    self.log.lock().unwrap().push((
                        cf.to_string(),
                        key.to_vec(),
                        Some(value.to_vec()),
                    ));
                }
                AtomicWriteOp::Delete { cf, key } => {
                    self.write_fault(cf, key)?;
                    self.log
                        .lock()
                        .unwrap()
                        .push((cf.to_string(), key.to_vec(), None));
                }
            }
        }
        self.inner.atomic_write(ops)
    }
}

// ---- Scenario helpers ----

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn order(
    market_id: MarketId,
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// Tiny deterministic LCG so the scenarios are reproducible with no deps.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One block: its pre-EVM and post-EVM batches.
type Block = [Vec<(Address, NativeAction)>; 2];

const TRADERS: u64 = 40;

/// `blocks` blocks over `markets` markets and 40 interleaved traders (each
/// trading many markets). Block 1 rests a ladder on every market; every later
/// batch is a crossing storm of `storm` actions drawing a market PER ORDER
/// (the bench shape): GTC / IOC / FOK / PostOnly, sizes that open, extend,
/// reduce, flip and fully close positions, STP self-trades, cancels by id
/// and cancel-alls (one market or all).
fn stream(seed: u64, markets: u64, blocks: usize, storm: u64) -> Vec<Block> {
    stream_sized(seed, markets, blocks, &|_| storm)
}

/// [`stream`] with the storm size of batch `k` (0-based over the stream,
/// two per block, the ladder batch excluded) from `storm`.
fn stream_sized(
    seed: u64,
    markets: u64,
    blocks: usize,
    storm: &dyn Fn(usize) -> u64,
) -> Vec<Block> {
    let mut rng = Lcg(seed);
    let markets: Vec<MarketId> = (1..=markets).collect();
    let mut ladder = Vec::new();
    for &m in &markets {
        for lvl in 0..4i64 {
            let maker = addr((1 + rng.below(TRADERS)) as u8);
            ladder.push(place(
                maker,
                order(
                    m,
                    true,
                    fp(97 + lvl),
                    fp(1 + rng.below(4) as i64),
                    TimeInForce::GTC,
                ),
            ));
            let maker = addr((1 + rng.below(TRADERS)) as u8);
            ladder.push(place(
                maker,
                order(
                    m,
                    false,
                    fp(101 + lvl),
                    fp(1 + rng.below(4) as i64),
                    TimeInForce::GTC,
                ),
            ));
        }
    }
    let storm_batch = |rng: &mut Lcg, size: u64| {
        let mut batch = Vec::new();
        for i in 0..size {
            let m = markets[rng.below(markets.len() as u64) as usize];
            let sender = addr((1 + rng.below(TRADERS)) as u8);
            let is_buy = rng.below(2) == 0;
            let tif = match rng.below(10) {
                0 | 1 => TimeInForce::IOC,
                2 => TimeInForce::FOK,
                3 => TimeInForce::PostOnly,
                _ => TimeInForce::GTC,
            };
            let price = 96 + rng.below(9) as i64;
            let qty = fp(1 + rng.below(5) as i64);
            match i % 37 {
                // Same sender both sides of one market: STP cancels.
                19 => {
                    batch.push(place(
                        sender,
                        order(m, true, fp(100), qty, TimeInForce::GTC),
                    ));
                    batch.push(place(sender, order(m, false, fp(100), qty, tif)));
                }
                // Cancel an earlier order by id (often unknown: a failure).
                29 => batch.push((
                    sender,
                    NativeAction::CancelOrder {
                        order_id: 1 + rng.below(400) as u128,
                    },
                )),
                // Cancel-all, one market or every market.
                7 if i % 3 == 0 => batch.push((
                    sender,
                    NativeAction::CancelAllOrders {
                        market_id: (rng.below(2) == 0).then_some(m),
                    },
                )),
                _ => batch.push(place(sender, order(m, is_buy, fp(price), qty, tif))),
            }
        }
        batch
    };
    let mut k = 0usize;
    let mut next = |rng: &mut Lcg| {
        let b = storm_batch(rng, storm(k));
        k += 1;
        b
    };
    let first = next(&mut rng);
    let mut out: Vec<Block> = vec![[ladder, first]];
    for _ in 1..blocks {
        let a = next(&mut rng);
        let b = next(&mut rng);
        out.push([a, b]);
    }
    out
}

/// Every field of one `NativeActionResult`.
type ResultRow = (&'static str, bool, Option<String>, u64, FailureReason);

fn row(a: &NativeActionResult) -> ResultRow {
    (
        a.action_type,
        a.success,
        a.error.clone().map(String::from),
        a.gas_used,
        a.reason,
    )
}

#[derive(PartialEq, Eq, Debug)]
struct CallOut {
    writes: Vec<Write>,
    results: Vec<ResultRow>,
    total_gas: u64,
    fills: Vec<TradeFill>,
    extras: Vec<FillExtras>,
    fatal: Option<String>,
}

#[derive(PartialEq, Eq, Debug)]
struct BlockOut {
    calls: Vec<CallOut>,
    /// The overlay's pending native set after the block (book save included).
    pending: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>,
    /// The book save's writes, sorted (their order follows a HashSet).
    save_writes: Vec<Write>,
}

#[derive(PartialEq, Eq, Debug)]
struct RunOut {
    blocks: Vec<BlockOut>,
    db_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    state_root: B256,
}

/// Counted besides the outcome (not compared).
#[derive(Default, Debug, Clone)]
struct RunStats {
    fallbacks: u64,
    /// Calls that ran pass A (the parallel settle, streamed or not).
    parallel_calls: u64,
    sequential_calls: u64,
    /// Calls whose pass B streamed (`settle_streamed_calls_on_this_thread`).
    streamed_calls: u64,
    /// Markets that went through pass B while a later market's job was
    /// still running (`settle_streamed_early_on_this_thread`).
    early_markets: u64,
    /// Plan-failure fallbacks after a non-empty streamed prefix.
    hybrid: u64,
    position_deletes: usize,
    position_puts: usize,
    hits: usize,
    /// Position point reads per block (pass A's included). Compared only for
    /// fault-free forced runs: a plan-failure fallback re-reads fewer rows
    /// streamed (the sequential loop resumes after the prefix instead of
    /// restarting), and in Auto mode the streamed jobs compute plans before
    /// the settle gate is known (discarded below it).
    position_reads: Reads,
}

/// Per-block position reads; `Debug` prints only the sizes.
#[derive(Default, Clone, PartialEq, Eq)]
struct Reads(Vec<BTreeMap<Vec<u8>, usize>>);

impl std::fmt::Debug for Reads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Reads(rows per block {:?})",
            self.0.iter().map(BTreeMap::len).collect::<Vec<_>>()
        )
    }
}

/// The fault armed for the LAST block.
#[derive(Clone, Debug)]
enum LastBlockFault {
    None,
    ReadNth(Vec<u8>, usize),
    Write(Vec<u8>),
    /// This market's plan (pass A) panics in every call of the last block
    /// (recovered: the sequential fallback).
    PlanPanic(MarketId),
    /// This market's match job panics (the block's fatal exit).
    MatchPanic(MarketId),
}

/// How each call runs.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// `execute_batch_settle_workers(parallel, Some(cap))`.
    Forced { parallel: bool, cap: usize },
    /// Production `execute_batch` (env settle gate, env caps).
    Auto,
    /// `execute_batch_engine_mode(threads)`.
    Engine(usize),
}

fn db_dump(db: &StateDb) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
    ] {
        for (k, v) in db.iterate_cf(cf, None).expect("iterate cf") {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

/// Codex P1 (L2.2 review): stored "opaque" position rows whose fields name
/// another market than their key, so two markets' pass-A caches hold the
/// same key (`settle_no_merge_tests.rs`).
const ALIASES: [(u8, MarketId, MarketId, i64); 4] =
    [(5, 1, 2, 2), (5, 2, 3, 4), (6, 1, 2, 2), (6, 2, 2, 4)];

fn seed_aliases(db: &StateDb) {
    for (t, key_market, market_id, size) in ALIASES {
        let row = Position {
            trader: addr(t),
            market_id,
            is_long: true,
            size: fp(size),
            entry_price: fp(100),
            cost_basis: fp(100 * size),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        };
        db.put_cf_raw(
            CF_NATIVE_POSITIONS,
            &position_key(&addr(t), key_market),
            &borsh::to_vec(&row).unwrap(),
        )
        .unwrap();
    }
}

/// One run's settings.
#[derive(Clone, Copy, Debug)]
struct Run {
    mode: Mode,
    record_fills: bool,
    aliases: bool,
    /// Scheduling perturbation seed (0 = none).
    jitter: u64,
}

/// Feeds `blocks` through one context per block over a fresh overlay on one
/// DB (flushed after each block, as the committer does). A block that goes
/// fatal is never flushed and ends the run. The flags are whatever the
/// caller pinned.
fn run(blocks: &[Block], r: Run, fault: &LastBlockFault) -> (RunOut, RunStats) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    for n in 1..=TRADERS as u8 {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        torus_core::position::PositionManager::new(db.clone())
            .put_native_balance(&addr(n), &bal)
            .unwrap();
    }
    if r.aliases {
        seed_aliases(&db);
    }
    let mut stats = RunStats::default();
    let mut out = Vec::new();
    let streamed_before = settle_streamed_calls_on_this_thread();
    let early_before = settle_streamed_early_on_this_thread();
    let hybrid_before = settle_streamed_hybrid_on_this_thread();
    for (h, block) in blocks.iter().enumerate() {
        let last = h + 1 == blocks.len();
        let rec = Recorder {
            inner: NativeStateOverlay::new(db.clone()),
            log: Arc::default(),
            faults: Arc::default(),
            position_reads: Arc::default(),
        };
        let mut probe = SettleStreamProbe {
            jitter_seed: if r.jitter == 0 {
                0
            } else {
                r.jitter.wrapping_add(h as u64 * 0x9E37) | 1
            },
            ..SettleStreamProbe::default()
        };
        if last {
            let mut f = rec.faults.lock().unwrap();
            match fault {
                LastBlockFault::None => {}
                LastBlockFault::ReadNth(k, n) => f.read_nth = Some((k.clone(), *n)),
                LastBlockFault::Write(k) => {
                    f.write = Some((CF_NATIVE_POSITIONS.to_string(), k.clone()))
                }
                LastBlockFault::PlanPanic(m) => probe.plan_panic = Some(*m),
                LastBlockFault::MatchPanic(m) => probe.match_panic = Some(*m),
            }
        }
        let mut ctx = NativeExecContext::new(
            rec.clone(),
            1 + h as u64,
            1000 + h as u64,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
        );
        ctx.record_fills = r.record_fills;
        let mut calls = Vec::new();
        for batch in block {
            let pass_a = ctx.phase_accum.settle_pass_a_ns;
            let pass_b = ctx.phase_accum.settle_pass_b_ns;
            let res = with_settle_stream_probe(probe, || match r.mode {
                Mode::Forced { parallel, cap } => NativeExecutor::execute_batch_settle_workers(
                    &mut ctx,
                    batch,
                    parallel,
                    Some(cap),
                ),
                Mode::Auto => NativeExecutor::execute_batch(&mut ctx, batch),
                Mode::Engine(t) => NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, t),
            });
            if ctx.phase_accum.settle_pass_a_ns > pass_a {
                stats.parallel_calls += 1;
            } else if ctx.phase_accum.settle_pass_b_ns > pass_b {
                stats.sequential_calls += 1;
            }
            let (fills, extras) = ctx.take_pending_fills_and_extras();
            let writes = std::mem::take(&mut *rec.log.lock().unwrap());
            stats.position_puts += writes
                .iter()
                .filter(|w| w.0 == CF_NATIVE_POSITIONS && w.2.is_some())
                .count();
            stats.position_deletes += writes
                .iter()
                .filter(|w| w.0 == CF_NATIVE_POSITIONS && w.2.is_none())
                .count();
            let fatal = ctx.fatal_error.clone();
            calls.push(CallOut {
                writes,
                results: res.results.iter().map(row).collect(),
                total_gas: res.total_gas,
                fills,
                extras,
                fatal: fatal.clone(),
            });
            if fatal.is_some() {
                break;
            }
        }
        stats.fallbacks += ctx.phase_accum.settle_fallbacks;
        let fatal = ctx.fatal_error.is_some();
        if !fatal {
            ctx.save_order_books();
        }
        let mut save_writes = std::mem::take(&mut *rec.log.lock().unwrap());
        save_writes.sort();
        out.push(BlockOut {
            calls,
            pending: rec.inner.dirty_native_keys(),
            save_writes,
        });
        stats
            .position_reads
            .0
            .push(std::mem::take(&mut *rec.position_reads.lock().unwrap()));
        stats.hits += rec.faults.lock().unwrap().hits;
        drop(ctx);
        if fatal {
            assert!(last, "only the last block may go fatal (block {})", h + 1);
            break;
        }
        rec.inner.flush(&db).expect("flush block overlay");
    }
    stats.streamed_calls = settle_streamed_calls_on_this_thread() - streamed_before;
    stats.early_markets = settle_streamed_early_on_this_thread() - early_before;
    stats.hybrid = settle_streamed_hybrid_on_this_thread() - hybrid_before;
    let out = RunOut {
        blocks: out,
        db_dump: db_dump(&db),
        state_root: compute_native_state_root(&db).expect("state root"),
    };
    (out, stats)
}

/// Asserts `on == off` block by block (the write sequence first, for a
/// readable failure).
fn assert_same(off: &RunOut, on: &RunOut, label: &str) {
    assert_eq!(off.blocks.len(), on.blocks.len(), "{label}: blocks run");
    for (h, (a, b)) in off.blocks.iter().zip(&on.blocks).enumerate() {
        assert_eq!(
            a.calls.len(),
            b.calls.len(),
            "{label}: block {} calls",
            h + 1
        );
        for (c, (x, y)) in a.calls.iter().zip(&b.calls).enumerate() {
            assert!(
                x.writes == y.writes,
                "{label}: block {} call {c}: write sequence diverged ({} vs {} writes)",
                h + 1,
                x.writes.len(),
                y.writes.len()
            );
            assert_eq!(x, y, "{label}: block {} call {c} diverged", h + 1);
        }
        assert_eq!(a, b, "{label}: block {} diverged", h + 1);
    }
    assert_eq!(off, on, "{label}: run diverged");
}

/// The fault of a run, chosen from a fault-free flag-off run.
type FaultPick<'a> = &'a dyn Fn(&RunOut, &RunStats) -> LastBlockFault;

/// Flag off vs on, for L2.2 off and on, over `blocks` with `r`'s settings
/// and each jitter seed of `jitters` (the flag-on arm only: the flag-off arm
/// does not stream). Returns `(L2.2 on?, flag-off stats, flag-on stats per
/// jitter)`.
fn assert_streamed_equivalent(
    blocks: &[Block],
    r: Run,
    jitters: &[u64],
    fault: FaultPick<'_>,
    what: &str,
) -> Vec<(bool, RunStats, Vec<RunStats>)> {
    let mut all = Vec::new();
    for no_merge in [false, true] {
        let label = format!("{what}, L2.2 {no_merge}, {r:?}");
        let off_run = |fault: &LastBlockFault| {
            with_settle_no_merge(no_merge, || {
                with_settle_streamed(false, || run(blocks, Run { jitter: 0, ..r }, fault))
            })
        };
        let (clean, clean_stats) = off_run(&LastBlockFault::None);
        let fault = fault(&clean, &clean_stats);
        let (off, off_stats) = off_run(&fault);
        assert_eq!(
            off_stats.streamed_calls, 0,
            "{label}: flag off never streams"
        );
        let mut ons = Vec::new();
        for &jitter in jitters {
            let label = format!("{label}, jitter {jitter:#x}");
            let (on, on_stats) = with_settle_no_merge(no_merge, || {
                with_settle_streamed(true, || run(blocks, Run { jitter, ..r }, &fault))
            });
            assert_same(&off, &on, &label);
            assert_eq!(
                (
                    off_stats.fallbacks,
                    off_stats.parallel_calls,
                    off_stats.sequential_calls,
                    off_stats.hits
                ),
                (
                    on_stats.fallbacks,
                    on_stats.parallel_calls,
                    on_stats.sequential_calls,
                    on_stats.hits
                ),
                "{label}: same settle paths and faults"
            );
            if matches!(fault, LastBlockFault::None) && !matches!(r.mode, Mode::Auto) {
                // Same reads too: the fused jobs' plans read exactly what
                // pass A read.
                assert_eq!(
                    clean_stats.position_reads, on_stats.position_reads,
                    "{label}: position reads"
                );
            }
            ons.push(on_stats);
        }
        all.push((no_merge, off_stats, ons));
    }
    all
}

fn no_fault(_: &RunOut, _: &RunStats) -> LastBlockFault {
    LastBlockFault::None
}

fn forced(cap: usize, record_fills: bool) -> Run {
    Run {
        mode: Mode::Forced {
            parallel: true,
            cap,
        },
        record_fills,
        aliases: false,
        jitter: 0,
    }
}

/// Forced parallel settle at caps 1, 2 and 8, record_fills on and off.
fn forced_runs() -> Vec<Run> {
    let mut v = Vec::new();
    for record_fills in [true, false] {
        for cap in [1usize, 2, 8] {
            v.push(forced(cap, record_fills));
        }
    }
    v
}

/// The `pick`-th position write of the LAST block's last call (a row pass A
/// reads and the flush writes).
fn last_block_position_key(clean: &RunOut, pick: impl Fn(&[&Write]) -> usize) -> Vec<u8> {
    let last = clean.blocks.last().expect("blocks");
    let call = last.calls.last().expect("calls");
    let rows: Vec<&Write> = call
        .writes
        .iter()
        .filter(|w| w.0 == CF_NATIVE_POSITIONS)
        .collect();
    assert!(rows.len() > 4, "the last call writes positions");
    rows[pick(&rows)].1.clone()
}

/// Off by default; `1` / `true` (trimmed, any case) turn it on.
#[test]
fn settle_streamed_flag_parse() {
    assert!(!parse_settle_streamed(None));
    for v in ["1", " 1", "true", "TRUE", " True "] {
        assert!(parse_settle_streamed(Some(v)), "{v:?}");
    }
    for v in ["", "0", "false", "2", "on", "yes"] {
        assert!(!parse_settle_streamed(Some(v)), "{v:?}");
    }
}

/// A fault-free stream: 8 blocks of 2 calls, 24 markets, a market per order.
#[test]
fn settle_streamed_identical_block_stream() {
    let blocks = stream(0x8C_0021_5EED, 24, 8, 500);
    for r in forced_runs() {
        for (no_merge, off, ons) in assert_streamed_equivalent(&blocks, r, &[0], &no_fault, "clean")
        {
            assert_eq!((off.fallbacks, off.hits), (0, 0));
            assert_eq!(off.parallel_calls, 16, "every call ran the parallel settle");
            assert!(off.position_puts > 1_000, "{off:?}");
            assert!(off.position_deletes > 20, "{off:?}");
            for on in ons {
                assert_eq!(
                    on.streamed_calls, 16,
                    "L2.2 {no_merge}: every call streamed ({on:?})"
                );
                assert_eq!(on.hybrid, 0);
            }
        }
    }
}

/// Second seed, fewer and wider markets.
#[test]
fn settle_streamed_identical_second_seed() {
    let blocks = stream(0xDEAD_BEEF_8C21, 6, 6, 400);
    for r in forced_runs() {
        for (_, off, ons) in assert_streamed_equivalent(&blocks, r, &[0], &no_fault, "seed 2") {
            assert_eq!(off.fallbacks, 0);
            assert!(off.position_deletes > 0, "{off:?}");
            assert!(ons.iter().all(|on| on.streamed_calls == 12), "{ons:?}");
        }
    }
}

/// Randomized shapes: market counts 2..=48 and storm sizes 20..=900, each
/// under no and one scheduling perturbation.
#[test]
fn settle_streamed_identical_randomized_shapes() {
    let mut rng = Lcg(0x8C21_0A4D);
    for case in 0..6 {
        let markets = 2 + rng.below(47);
        let blocks = 2 + rng.below(4) as usize;
        let storm = 20 + rng.below(881);
        let seed = rng.next();
        let cap = [1usize, 3, 8, 32][rng.below(4) as usize];
        let what = format!("case {case}: {markets} markets, {blocks} blocks, storm {storm}");
        let shape = stream(seed, markets, blocks, storm);
        let r = forced(cap, rng.below(2) == 0);
        let jitter = rng.next() | 1;
        for (_, off, ons) in assert_streamed_equivalent(&shape, r, &[0, jitter], &no_fault, &what) {
            for on in ons {
                assert_eq!(on.streamed_calls, off.parallel_calls, "{what}: {on:?}");
            }
        }
    }
}

/// Determinism under thread scheduling: the same stream under 10
/// perturbation seeds (per-market sleeps in the jobs and on the exec thread,
/// so markets finish in many different orders) equals flag off every time,
/// and pass B did run while later markets' jobs were still working.
#[test]
fn settle_streamed_identical_under_scheduling_perturbation() {
    let blocks = stream(0x8C21_5C4E_D000, 32, 4, 600);
    let jitters: Vec<u64> = (1..=10u64)
        .map(|k| k.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .collect();
    for cap in [2usize, 8] {
        for (no_merge, _, ons) in
            assert_streamed_equivalent(&blocks, forced(cap, true), &jitters, &no_fault, "perturbed")
        {
            let early: u64 = ons.iter().map(|s| s.early_markets).sum();
            assert!(
                early > 0,
                "cap {cap}, L2.2 {no_merge}: pass B overlapped the pool: {ons:?}"
            );
        }
    }
}

/// The last block's plan for one market panics (recovered): flag off falls
/// back to the sequential loop for the whole call; streamed, every lower
/// market already went through pass B, so the sequential loop resumes at
/// the failed market. Same writes, results, roots. The lowest market is the
/// edge with nothing streamed before the fallback.
#[test]
fn settle_streamed_identical_with_plan_panic_fallback() {
    let blocks = stream(0x8C21_FA11, 12, 4, 400);
    for (edge, market) in [("highest", 12u64), ("lowest", 1), ("middle", 6)] {
        for cap in [1usize, 8] {
            let what = format!("plan panic {edge}");
            let pick = |_: &RunOut, _: &RunStats| LastBlockFault::PlanPanic(market);
            for (no_merge, off, ons) in assert_streamed_equivalent(
                &blocks,
                forced(cap, cap == 1),
                &[0, 0x5EED_F00D],
                &pick,
                &what,
            ) {
                assert_eq!(off.fallbacks, 2, "{what}: both calls fell back ({off:?})");
                for on in ons {
                    let want = if market > 1 { 2 } else { 0 };
                    assert_eq!(
                        on.hybrid, want,
                        "{what}, L2.2 {no_merge}: resumed after a prefix ({on:?})"
                    );
                }
            }
        }
    }
}

/// The last block's pass A hits a one-shot position read failure (a local
/// fault) at the row's last read of the clean block: the call falls back to
/// the sequential loop and fail-stops (R02).
#[test]
fn settle_streamed_identical_with_plan_read_fault() {
    let blocks = stream(0x8C21_0022_FA11, 12, 4, 400);
    let pick = |clean: &RunOut, stats: &RunStats| {
        let key = last_block_position_key(clean, |_| 0);
        let n = stats.position_reads.0.last().unwrap()[&key];
        LastBlockFault::ReadNth(key, n)
    };
    for r in [forced(2, true), forced(8, false)] {
        for (_, off, ons) in
            assert_streamed_equivalent(&blocks, r, &[0, 0xBEEF], &pick, "read fault")
        {
            assert_eq!(off.fallbacks, 1, "the faulted call fell back: {off:?}");
            assert_eq!(off.hits, 1, "the fault fired once: {off:?}");
            assert!(ons.iter().all(|s| s.hits == 1), "{ons:?}");
        }
    }
}

/// The last block's position flush fails at a row in the middle: the flush
/// stops there with "position cache flush failed" (fatal), in every arm at
/// the same write.
#[test]
fn settle_streamed_identical_with_fatal_flush_failure() {
    let blocks = stream(0x8C21_DEAD, 12, 4, 400);
    let pick = |clean: &RunOut, _: &RunStats| {
        LastBlockFault::Write(last_block_position_key(clean, |r| r.len() / 2))
    };
    for (_, off, _) in
        assert_streamed_equivalent(&blocks, forced(4, true), &[0, 0xF1], &pick, "fatal flush")
    {
        assert_eq!(off.fallbacks, 0, "{off:?}");
        assert!(off.hits >= 1, "the write fault fired: {off:?}");
    }
}

/// A match job panics in the last block (the highest market, so lower
/// markets may already have gone through pass B when it is seen; and the
/// lowest and a middle one): the block's fatal exit, with the same results,
/// gas, fatal reason and trade rows as flag off; never flushed.
#[test]
fn settle_streamed_identical_with_match_panic() {
    let blocks = stream(0x8C21_9A41C, 10, 3, 400);
    for market in [10u64, 1, 5] {
        for cap in [1usize, 8] {
            let r = forced(cap, true);
            let what = format!("match panic market {market}");
            let pick = |_: &RunOut, _: &RunStats| LastBlockFault::MatchPanic(market);
            assert_streamed_equivalent(&blocks, r, &[0, 0xA11CE], &pick, &what);
            let (on, _) = with_settle_streamed(true, || {
                run(&blocks, r, &LastBlockFault::MatchPanic(market))
            });
            let fatal = on
                .blocks
                .last()
                .unwrap()
                .calls
                .last()
                .unwrap()
                .fatal
                .clone();
            assert!(
                fatal.as_deref().is_some_and(
                    |f| f.starts_with(&format!("market worker panicked (market {market})"))
                ),
                "{fatal:?}"
            );
        }
    }
}

/// Aliased opaque rows (a market's plan cache holds another market's key):
/// equal to flag off, also when the last block falls back after a prefix
/// (the streamed pass B stops before an impure plan, so the sequential loop
/// sees what it would have seen from scratch).
#[test]
fn settle_streamed_identical_with_aliased_opaque_rows() {
    let blocks = stream(0x0A11_A5ED_8C21, 6, 5, 400);
    for (fault, label) in [
        (LastBlockFault::None, "aliases"),
        (LastBlockFault::PlanPanic(6), "aliases + plan panic"),
        (LastBlockFault::PlanPanic(3), "aliases + plan panic mid"),
    ] {
        for cap in [1usize, 2, 8] {
            let r = Run {
                aliases: true,
                ..forced(cap, false)
            };
            let pick = |_: &RunOut, _: &RunStats| fault.clone();
            assert_streamed_equivalent(&blocks, r, &[0, 0xA1], &pick, label);
        }
    }
}

/// Production `execute_batch` (Auto): batches below the settle gate run the
/// sequential loop and are never streamed; batches above it stream. Storm
/// sizes alternate 30 and 4,000 actions, so both sides of the gate (and the
/// streamed decision made mid-stream, once the fills seen so far cross it)
/// are exercised.
#[test]
fn settle_streamed_identical_auto_gate() {
    let blocks = stream_sized(0x8C21_A070, 16, 6, &|k| if k % 2 == 0 { 30 } else { 4_000 });
    let r = Run {
        mode: Mode::Auto,
        record_fills: true,
        aliases: false,
        jitter: 0,
    };
    for (_, off, ons) in assert_streamed_equivalent(&blocks, r, &[0, 0xA070], &no_fault, "auto") {
        assert!(off.parallel_calls > 0, "{off:?}");
        assert!(off.sequential_calls > 0, "{off:?}");
        for on in ons {
            assert_eq!(on.streamed_calls, off.parallel_calls, "{on:?}");
        }
    }
}

/// The engine path (sharded Phase 2, forced parallel settle).
#[test]
fn settle_streamed_identical_engine_mode() {
    let blocks = stream(0x8C21_E461, 20, 4, 700);
    let r = Run {
        mode: Mode::Engine(4),
        record_fills: false,
        aliases: false,
        jitter: 0,
    };
    for (_, off, ons) in assert_streamed_equivalent(&blocks, r, &[0, 0xE4], &no_fault, "engine") {
        assert_eq!(off.parallel_calls, 8, "{off:?}");
        assert!(ons.iter().all(|s| s.streamed_calls == 8), "{ons:?}");
    }
}

/// Forced sequential: the flag does nothing (no stream, same output).
#[test]
fn settle_streamed_forced_sequential_is_untouched() {
    let blocks = stream(0x8C21_5E0, 8, 3, 300);
    let r = Run {
        mode: Mode::Forced {
            parallel: false,
            cap: 4,
        },
        record_fills: true,
        aliases: false,
        jitter: 0,
    };
    for (_, off, ons) in assert_streamed_equivalent(&blocks, r, &[0], &no_fault, "sequential") {
        assert_eq!(off.parallel_calls, 0);
        assert!(ons.iter().all(|s| s.streamed_calls == 0), "{ons:?}");
    }
}

/// The case the purity stop exists for: trader 5's stored row under key
/// (5, 1) names market 2 and the one under (5, 2) names market 3
/// ([`ALIASES`]). In the last block trader 5 trades in markets 1 AND 2, and
/// market 3's plan panics. Flag off reruns the call sequentially from
/// scratch, where market 2 reads the (5, 2) row market 1 just wrote. Had the
/// streamed pass B applied market 1's plan (which holds market 2's key) and
/// market 2's plan (computed from the stored row) before resuming at market
/// 3, the result would differ; the stream must stop before market 1's
/// impure plan, so nothing is applied before the fallback.
#[test]
fn settle_streamed_impure_prefix_plan_stops_the_stream() {
    let ladder = stream(0x1A0A_5ED, 4, 1, 0).remove(0)[0].clone();
    let t5 = addr(5);
    let other = addr(9);
    let mut last = Vec::new();
    for m in 1..=4u64 {
        // Trader 5 takes liquidity in markets 1 and 2 (both sides).
        if m <= 2 {
            last.push(place(t5, order(m, true, fp(103), fp(2), TimeInForce::IOC)));
            last.push(place(t5, order(m, false, fp(97), fp(1), TimeInForce::IOC)));
        }
        last.push(place(
            other,
            order(m, true, fp(104), fp(3), TimeInForce::IOC),
        ));
        last.push(place(
            addr(11),
            order(m, false, fp(96), fp(2), TimeInForce::IOC),
        ));
    }
    let blocks: Vec<Block> = vec![[ladder, Vec::new()], [last.clone(), last]];
    for market in [3u64, 4] {
        for cap in [1usize, 2, 8] {
            let r = Run {
                aliases: true,
                ..forced(cap, true)
            };
            let what = format!("impure prefix, plan panic {market}");
            let pick = |_: &RunOut, _: &RunStats| LastBlockFault::PlanPanic(market);
            for (no_merge, off, ons) in
                assert_streamed_equivalent(&blocks, r, &[0, 0x1A], &pick, &what)
            {
                assert_eq!(off.fallbacks, 2, "{what}: {off:?}");
                for on in ons {
                    assert_eq!(
                        on.hybrid, 0,
                        "{what}, L2.2 {no_merge}: stopped at market 1's impure plan ({on:?})"
                    );
                }
            }
        }
    }
}
