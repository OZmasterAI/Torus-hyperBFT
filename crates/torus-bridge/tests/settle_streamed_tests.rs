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
    in_maker_snapshot_read_on_this_thread, parse_settle_streamed,
    settle_streamed_calls_on_this_thread, settle_streamed_early_on_this_thread,
    settle_streamed_hybrid_on_this_thread, with_settle_no_merge, with_settle_stream_probe,
    with_settle_stream_wake_batch, with_settle_streamed, BookMode, NativeActionResult,
    NativeExecContext, NativeExecutor, ResidentBooks, SettleStreamProbe,
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

/// Point reads per `(cf, key)`.
type ReadCounts = BTreeMap<(String, Vec<u8>), usize>;

#[derive(Default)]
struct Faults {
    /// One-shot read faults: the n-th (1-based, counted per block) point
    /// read of `(cf, key)` fails, once.
    reads: Vec<(String, Vec<u8>, usize)>,
    /// Every write of these `(cf, key)` fails.
    writes: Vec<(String, Vec<u8>)>,
    /// Every maker snapshot read of these `(cf, key)` fails (matching:
    /// counts of those reads depend on scheduling, so no n-th read).
    snapshot: Vec<(String, Vec<u8>)>,
    /// Read and write faults that fired (compared flag off vs on).
    hits: usize,
    /// Snapshot faults that fired (scheduling-dependent: not compared).
    snapshot_hits: usize,
}

/// Wraps the block's `NativeStateOverlay`: records every write in order and
/// injects the armed faults (as `settle_no_merge_tests.rs`).
#[derive(Clone)]
struct Recorder {
    inner: NativeStateOverlay,
    log: Arc<Mutex<Vec<Write>>>,
    faults: Arc<Mutex<Faults>>,
    /// Point reads per `(cf, key)` in the block, maker snapshot reads left
    /// out (below).
    reads: Arc<Mutex<ReadCounts>>,
    /// Matching's maker snapshot reads per `(cf, key)` in the block
    /// ([`in_maker_snapshot_read_on_this_thread`]): once per maker per Phase
    /// 3 worker thread, so their number depends on which thread matched
    /// which market (flag off vs flag off too). Neither counted in `reads`
    /// nor faulted: "the n-th read of a key" stays the same read whatever
    /// the scheduling.
    snapshot_reads: Arc<Mutex<ReadCounts>>,
}

impl Recorder {
    fn new(inner: NativeStateOverlay) -> Self {
        Self {
            inner,
            log: Arc::default(),
            faults: Arc::default(),
            reads: Arc::default(),
            snapshot_reads: Arc::default(),
        }
    }

    fn write_fault(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        let mut f = self.faults.lock().unwrap();
        if f.writes.iter().any(|(c, k)| c == cf && k.as_slice() == key) {
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
        if in_maker_snapshot_read_on_this_thread() {
            let mut snap = self.snapshot_reads.lock().unwrap();
            *snap.entry((cf.to_string(), key.to_vec())).or_default() += 1;
            drop(snap);
            let mut f = self.faults.lock().unwrap();
            if f.snapshot
                .iter()
                .any(|(c, k)| c == cf && k.as_slice() == key)
            {
                f.snapshot_hits += 1;
                return Err(StateError::Io(std::io::Error::other(
                    "injected snapshot read failure",
                )));
            }
            drop(f);
            return self.inner.get_cf_raw(cf, key);
        }
        let n = {
            let mut reads = self.reads.lock().unwrap();
            let n = reads.entry((cf.to_string(), key.to_vec())).or_default();
            *n += 1;
            *n
        };
        let mut f = self.faults.lock().unwrap();
        if let Some(at) = f
            .reads
            .iter()
            .position(|(c, k, nth)| c == cf && k.as_slice() == key && *nth == n)
        {
            f.reads.remove(at);
            f.hits += 1;
            return Err(StateError::Io(std::io::Error::other(
                "injected one-shot read failure",
            )));
        }
        drop(f);
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
    /// The reader fault channel after the call (R02: a read error a reader
    /// cannot return, e.g. matching's maker snapshot; fail-stops the block
    /// at commit, as `fatal`, without changing the call's control flow).
    reader_fault: Option<String>,
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
    /// Snapshot faults that fired ([`Faults::snapshot_hits`]).
    snapshot_hits: usize,
    /// The fatal error that ended the run, if any (also compared in the
    /// outcome; kept here so tests can assert the fail-stop happened).
    fatal: Option<String>,
    /// Position point reads per block (pass A's included). Compared only for
    /// fault-free runs: a plan-failure fallback re-reads fewer rows streamed
    /// (the sequential loop resumes after the prefix instead of restarting).
    /// Auto mode included: the streamed jobs compute a plan only once the
    /// settle gate is certain to pass, so below it nothing reads positions
    /// for plans (Codex P2).
    position_reads: Reads,
    /// Point reads per `(cf, key)` per block, every CF (fault picks),
    /// matching's maker snapshot reads left out ([`Recorder`]).
    reads: Vec<ReadCounts>,
    /// Matching's maker snapshot reads per `(cf, key)` per block (not
    /// compared: scheduling-dependent).
    snapshot_reads: Vec<ReadCounts>,
    /// The same counts after each call of each block (cumulative).
    call_reads: Vec<Vec<ReadCounts>>,
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
    /// The n-th read of this position row fails, once.
    ReadNth(Vec<u8>, usize),
    /// Every write of this position row fails.
    Write(Vec<u8>),
    /// The n-th read of `(cf, key)` fails, once.
    ReadCfNth(&'static str, Vec<u8>, usize),
    /// Every write of `(cf, key)` fails.
    WriteCf(&'static str, Vec<u8>),
    /// Every maker snapshot read of `(cf, key)` fails (matching; a reader
    /// fault).
    SnapshotCf(&'static str, Vec<u8>),
    /// All of these.
    Many(Vec<LastBlockFault>),
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

/// Arms `fault` (the backend's faults and the probe's panics).
fn arm(fault: &LastBlockFault, f: &mut Faults, probe: &mut SettleStreamProbe) {
    match fault {
        LastBlockFault::None => {}
        LastBlockFault::ReadNth(k, n) => {
            f.reads
                .push((CF_NATIVE_POSITIONS.to_string(), k.clone(), *n))
        }
        LastBlockFault::Write(k) => f.writes.push((CF_NATIVE_POSITIONS.to_string(), k.clone())),
        LastBlockFault::ReadCfNth(cf, k, n) => f.reads.push((cf.to_string(), k.clone(), *n)),
        LastBlockFault::WriteCf(cf, k) => f.writes.push((cf.to_string(), k.clone())),
        LastBlockFault::SnapshotCf(cf, k) => f.snapshot.push((cf.to_string(), k.clone())),
        LastBlockFault::PlanPanic(m) => probe.plan_panic = Some(*m),
        LastBlockFault::MatchPanic(m) => probe.match_panic = Some(*m),
        LastBlockFault::Many(all) => {
            for one in all {
                arm(one, f, probe);
            }
        }
    }
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
        let rec = Recorder::new(NativeStateOverlay::new(db.clone()));
        let mut probe = SettleStreamProbe {
            jitter_seed: if r.jitter == 0 {
                0
            } else {
                r.jitter.wrapping_add(h as u64 * 0x9E37) | 1
            },
            ..SettleStreamProbe::default()
        };
        if last {
            arm(fault, &mut rec.faults.lock().unwrap(), &mut probe);
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
        let mut call_reads = Vec::new();
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
            let reader_fault = ctx.reader_fault.get().cloned();
            // The committer's view (`take_fatal_error`): `fatal_error`, else
            // the reader fault.
            if let Some(f) = fatal.clone().or_else(|| reader_fault.clone()) {
                stats.fatal = Some(f);
            }
            call_reads.push(rec.reads.lock().unwrap().clone());
            calls.push(CallOut {
                writes,
                results: res.results.iter().map(row).collect(),
                total_gas: res.total_gas,
                fills,
                extras,
                fatal: fatal.clone(),
                reader_fault,
            });
            if fatal.is_some() {
                break;
            }
        }
        stats.fallbacks += ctx.phase_accum.settle_fallbacks;
        // A reader fault fail-stops the block as `fatal_error` does (the
        // committer never flushes it).
        let fatal = ctx.fatal_error.is_some() || ctx.reader_fault.get().is_some();
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
        let reads = std::mem::take(&mut *rec.reads.lock().unwrap());
        stats.position_reads.0.push(
            reads
                .iter()
                .filter(|((cf, _), _)| cf == CF_NATIVE_POSITIONS)
                .map(|((_, k), n)| (k.clone(), *n))
                .collect(),
        );
        stats.reads.push(reads);
        stats
            .snapshot_reads
            .push(std::mem::take(&mut *rec.snapshot_reads.lock().unwrap()));
        stats.call_reads.push(call_reads);
        stats.hits += rec.faults.lock().unwrap().hits;
        stats.snapshot_hits += rec.faults.lock().unwrap().snapshot_hits;
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
            "{label}: block {} calls (fatal off {:?}, on {:?})",
            h + 1,
            a.calls.iter().find_map(|c| c.fatal.as_ref()),
            b.calls.iter().find_map(|c| c.fatal.as_ref()),
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

/// Wake batches the flag-on arm cycles through, one per jitter seed (the
/// `k`-th seed runs under `WAKE_BATCHES[k % 5]`; `None`: the policy, which
/// is already above 1 for 16+ markets). Scheduling only: every batch must
/// give the same results.
const WAKE_BATCHES: [Option<usize>; 5] = [None, Some(1), Some(2), Some(5), Some(64)];

/// Flag off vs on, for L2.2 off and on, over `blocks` with `r`'s settings
/// and each jitter seed of `jitters` (the flag-on arm only: the flag-off arm
/// does not stream), each seed under the next wake batch of
/// [`WAKE_BATCHES`]. Returns `(L2.2 on?, flag-off stats, flag-on stats per
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
        let label = format!("{label}, fault {fault:?}");
        let (off, off_stats) = off_run(&fault);
        assert_eq!(
            off_stats.streamed_calls, 0,
            "{label}: flag off never streams"
        );
        let mut ons = Vec::new();
        for (k, &jitter) in jitters.iter().enumerate() {
            let batch = WAKE_BATCHES[k % WAKE_BATCHES.len()];
            let label = format!("{label}, jitter {jitter:#x}, wake batch {batch:?}");
            let (on, on_stats) = with_settle_no_merge(no_merge, || {
                with_settle_streamed(true, || {
                    with_settle_stream_wake_batch(batch, || {
                        run(blocks, Run { jitter, ..r }, &fault)
                    })
                })
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
            if matches!(fault, LastBlockFault::None) {
                // Same reads too: the fused jobs' plans read exactly what
                // pass A read (Auto: nothing below the gate).
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

/// Codex P2 (L2.1 review): production `execute_batch` (Auto) below the
/// settle gate settles sequentially. A one-shot position read failure at the
/// row's last read of the clean block must hit that sequential loop as with
/// the flag off (R02 fail-stop, same writes): the streamed jobs must not read
/// positions for plans the gate never uses (a speculative plan would consume
/// the fault, and the sequential retry would run on without it).
#[test]
fn settle_streamed_below_gate_read_fault_fail_stops_as_flag_off() {
    let blocks = stream(0x8C21_B10E_FA11, 6, 4, 40);
    let r = Run {
        mode: Mode::Auto,
        record_fills: true,
        aliases: false,
        jitter: 0,
    };
    let pick = |clean: &RunOut, stats: &RunStats| {
        let key = last_block_position_key(clean, |_| 0);
        let n = stats.position_reads.0.last().unwrap()[&key];
        LastBlockFault::ReadNth(key, n)
    };
    for (_, off, ons) in
        assert_streamed_equivalent(&blocks, r, &[0, 0xB10E], &pick, "below-gate read fault")
    {
        assert_eq!(off.parallel_calls, 0, "every call below the gate: {off:?}");
        assert_eq!(off.hits, 1, "the fault fired once: {off:?}");
        assert!(
            off.fatal
                .as_deref()
                .is_some_and(|f| f.contains("injected one-shot read failure")),
            "R02 fail-stop: {off:?}"
        );
        for on in ons {
            assert_eq!(on.fatal, off.fatal, "{on:?}");
            assert_eq!(on.streamed_calls, 0, "{on:?}");
        }
    }
}

/// The context after one call, as far as later calls and the book save see
/// it (the trade buffers are compared through `CallOut` and the final take).
#[derive(PartialEq, Eq, Debug)]
struct CtxView {
    books: Vec<MarketId>,
    dirty: Vec<MarketId>,
    cancel_index: (u64, u64),
    cloid_index: Option<usize>,
}

#[derive(PartialEq, Eq, Debug)]
struct ReusedOut {
    calls: Vec<CallOut>,
    views: Vec<CtxView>,
    save_writes: Vec<Write>,
    fills: Vec<TradeFill>,
    extras: Vec<FillExtras>,
    trade_rows: Vec<(String, Vec<u8>, Vec<u8>)>,
}

/// Every resting GTC placement of `batch` at an index divisible by 3 gets a
/// cloid (unique per batch salt), so the cloid index is built and fed.
fn with_cloids(batch: &[(Address, NativeAction)], salt: u128) -> Vec<(Address, NativeAction)> {
    batch
        .iter()
        .enumerate()
        .map(|(i, (s, a))| match a {
            NativeAction::PlaceOrder(p) if i % 3 == 0 && p.time_in_force == TimeInForce::GTC => {
                let mut p = p.clone();
                p.client_order_id = Some(salt + i as u128);
                (*s, NativeAction::PlaceOrder(p))
            }
            _ => (*s, a.clone()),
        })
        .collect()
}

/// `batch`'s placements in markets `<= max` (cancels kept).
fn only_markets(batch: &[(Address, NativeAction)], max: MarketId) -> Vec<(Address, NativeAction)> {
    batch
        .iter()
        .filter(|(_, a)| !matches!(a, NativeAction::PlaceOrder(p) if p.market_id > max))
        .cloned()
        .collect()
}

/// One resident context REUSED for heights 1 and 2 (inline trade history: rows are
/// written after each call, and the first fill of a new height clears the
/// earlier height's buffered fills). Height 1 runs `blocks[0]` in markets
/// 1-3 only (so markets 4-6 enter height 2 not dirty); height 2 runs
/// `blocks[1][0]` with `market`'s match job panicking (the fatal exit), then
/// `blocks[1][1]` on the same context (the committer runs the post-EVM call
/// after a fatal pre-EVM one), then the book save.
fn reused_ctx_run(blocks: &[Block], cap: usize, record_fills: bool, market: MarketId) -> ReusedOut {
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
    let rec = Recorder::new(NativeStateOverlay::new(db.clone()));
    // Resident books: the cancel-all index is built (at the first
    // cancel-all) only in resident mode.
    let mut resident = ResidentBooks::default();
    let mut ctx = NativeExecContext::new_with_mode(
        rec.clone(),
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        BookMode::Classic,
        Some(&mut resident),
    );
    ctx.record_fills = record_fills;
    assert!(
        ctx.trade_history && !ctx.defer_trades,
        "inline trade history"
    );
    let view = |ctx: &NativeExecContext<Recorder>| {
        let mut books: Vec<MarketId> = ctx.order_books().keys().copied().collect();
        books.sort_unstable();
        let mut dirty: Vec<MarketId> = ctx.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        CtxView {
            books,
            dirty,
            cancel_index: ctx.cancel_index_size(),
            cloid_index: ctx.cloid_index_len(),
        }
    };
    let mut calls = Vec::new();
    let mut views = Vec::new();
    let mut call = |ctx: &mut NativeExecContext<Recorder>,
                    batch: &[(Address, NativeAction)],
                    probe: SettleStreamProbe| {
        let res = with_settle_stream_probe(probe, || {
            NativeExecutor::execute_batch_settle_workers(ctx, batch, true, Some(cap))
        });
        calls.push(CallOut {
            writes: std::mem::take(&mut *rec.log.lock().unwrap()),
            results: res.results.iter().map(row).collect(),
            total_gas: res.total_gas,
            // The buffers stay in the context (taken once, at the end).
            fills: Vec::new(),
            extras: Vec::new(),
            fatal: ctx.fatal_error.clone(),
            reader_fault: ctx.reader_fault.get().cloned(),
        });
    };
    let none = SettleStreamProbe::default();
    call(
        &mut ctx,
        &with_cloids(&only_markets(&blocks[0][0], 3), 1 << 20),
        none,
    );
    call(
        &mut ctx,
        &with_cloids(&only_markets(&blocks[0][1], 3), 2 << 20),
        none,
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    views.push(view(&ctx));
    ctx.block_height = 2;
    ctx.timestamp += 1;
    let panic = SettleStreamProbe {
        match_panic: Some(market),
        ..none
    };
    call(&mut ctx, &with_cloids(&blocks[1][0], 3 << 20), panic);
    views.push(view(&ctx));
    call(&mut ctx, &with_cloids(&blocks[1][1], 4 << 20), none);
    views.push(view(&ctx));
    ctx.save_order_books();
    let mut save_writes = std::mem::take(&mut *rec.log.lock().unwrap());
    save_writes.sort();
    let (fills, extras) = ctx.take_pending_fills_and_extras();
    let mut trade_rows = Vec::new();
    for cf in [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES] {
        for (k, v) in rec.inner.iterate_cf(cf, None).expect("iterate cf") {
            trade_rows.push((cf.to_string(), k, v));
        }
    }
    ReusedOut {
        calls,
        views,
        save_writes,
        fills,
        extras,
        trade_rows,
    }
}

/// Codex P2 (L2.1 review) and its audit: a match panic seen after lower
/// markets already went through the streamed pass B (the panicking market is
/// the highest, so with a forced parallel settle every lower market is
/// applied before the panic is taken) must leave the context exactly as flag
/// off's fatal exit leaves it, also when the context is reused for a new
/// block in inline trade-history mode: the trade log (the earlier height's
/// buffered fills and extras, the rows written after the call), the books the
/// context holds (flag off lost every book of the call), the dirty-book set,
/// the cancel-all and cloid indexes; and the next call on the same context
/// and the book save see the same state.
#[test]
fn settle_streamed_match_panic_restores_context_across_block_rollover() {
    let blocks = stream(0x8C21_B0B0_0F11, 6, 2, 300);
    let market = 6;
    for record_fills in [true, false] {
        for cap in [1usize, 8] {
            let what = format!("record_fills {record_fills}, cap {cap}");
            for no_merge in [false, true] {
                let off = with_settle_no_merge(no_merge, || {
                    with_settle_streamed(false, || {
                        reused_ctx_run(&blocks, cap, record_fills, market)
                    })
                });
                let streamed_before = settle_streamed_calls_on_this_thread();
                let on = with_settle_no_merge(no_merge, || {
                    with_settle_streamed(true, || {
                        reused_ctx_run(&blocks, cap, record_fills, market)
                    })
                });
                assert!(
                    settle_streamed_calls_on_this_thread() - streamed_before >= 2,
                    "{what}: the flag-on arm streamed"
                );
                assert!(
                    off.calls[2].fatal.as_deref().is_some_and(
                        |f| f.starts_with(&format!("market worker panicked (market {market})"))
                    ),
                    "{what}: {:?}",
                    off.calls[2].fatal
                );
                assert!(
                    !off.fills.is_empty()
                        && off.views[0].cancel_index.0 > 0
                        && off.views[0].cloid_index.is_some_and(|n| n > 0),
                    "{what}: fills recorded and both indexes built: {:?}",
                    off.views[0]
                );
                for (c, (x, y)) in off.calls.iter().zip(&on.calls).enumerate().take(3) {
                    assert!(
                        x.writes == y.writes,
                        "{what}, L2.2 {no_merge}: call {c}: write sequence diverged ({} vs {} writes)",
                        x.writes.len(),
                        y.writes.len()
                    );
                    assert_eq!(x, y, "{what}, L2.2 {no_merge}: call {c}");
                }
                // The context the panicked call left, before the next call.
                assert_eq!(
                    off.views[1], on.views[1],
                    "{what}, L2.2 {no_merge}: context after the panic"
                );
                let (x, y) = (&off.calls[3], &on.calls[3]);
                assert!(
                    x.writes == y.writes,
                    "{what}, L2.2 {no_merge}: next call: write sequence diverged ({} vs {} writes)",
                    x.writes.len(),
                    y.writes.len()
                );
                assert_eq!(x, y, "{what}, L2.2 {no_merge}: next call");
                assert_eq!(off.views, on.views, "{what}, L2.2 {no_merge}: context");
                assert_eq!(
                    (off.fills.len(), off.extras.len()),
                    (on.fills.len(), on.extras.len()),
                    "{what}, L2.2 {no_merge}: pending fills / extras"
                );
                assert_eq!(off, on, "{what}, L2.2 {no_merge}");
            }
        }
    }
}

// ---- Fallback replay (Codex round 2): faults in the streamed prefix ----

/// Codex P2 (L2.1 review, round 2): a maker's balance read fails in the
/// streamed pass B of market 1 (its first settle read: maker 1 is no sender
/// of the call, so the call's balance cache does not hold it), then a later
/// market's plan panics. Flag off falls back before any pass B and its
/// sequential loop stops the taker's order at the failed fill (taker
/// position 2 of 4); streamed, market 1 already went through pass B with
/// its whole position plan. Every read of maker 1's balance in the last
/// block is faulted in turn (one at a time), with markets 2 and 3 panicking,
/// at caps 1, 2 and 8, with and without a fill subscriber, L2.2 off and on:
/// flag on must equal flag off (writes, results, fills, fatal, DB, root).
#[test]
fn settle_streamed_prefix_balance_fault_then_plan_panic_matches_flag_off() {
    let (gtc, ioc) = (TimeInForce::GTC, TimeInForce::IOC);
    let block1: Block = [
        vec![
            // Maker 1 goes short 4 at 101 in market 1 (markets 2, 3 alike).
            place(addr(1), order(1, false, fp(101), fp(4), gtc)),
            place(addr(2), order(1, true, fp(101), fp(4), ioc)),
            place(addr(3), order(2, false, fp(101), fp(4), gtc)),
            place(addr(4), order(2, true, fp(101), fp(4), ioc)),
            place(addr(5), order(3, false, fp(101), fp(4), gtc)),
            place(addr(6), order(3, true, fp(101), fp(4), ioc)),
        ],
        vec![
            // Maker 1 rests a closing buy (first in time), maker 7 behind it.
            place(addr(1), order(1, true, fp(99), fp(2), gtc)),
            place(addr(7), order(1, true, fp(99), fp(2), gtc)),
            place(addr(3), order(2, true, fp(99), fp(2), gtc)),
            place(addr(5), order(3, true, fp(99), fp(2), gtc)),
        ],
    ];
    let block2: Block = [
        vec![
            // Taker 8 sells 4: fill 0 closes maker 1's short (a PnL credit:
            // maker 1's balance is read), fill 1 hits maker 7.
            place(addr(8), order(1, false, fp(99), fp(4), ioc)),
            place(addr(9), order(2, false, fp(99), fp(2), ioc)),
            place(addr(10), order(3, false, fp(99), fp(2), ioc)),
        ],
        vec![
            place(addr(11), order(1, true, fp(98), fp(1), gtc)),
            place(addr(12), order(2, true, fp(98), fp(1), gtc)),
            place(addr(13), order(3, true, fp(98), fp(1), gtc)),
        ],
    ];
    let blocks = vec![block1, block2];
    let maker = addr(1).as_slice().to_vec();
    let mut settle_faults = 0usize;
    for market in [2u64, 3] {
        for cap in [1usize, 2, 8] {
            for record_fills in [true, false] {
                let r = forced(cap, record_fills);
                let (_, clean) =
                    with_settle_streamed(false, || run(&blocks, r, &LastBlockFault::None));
                let reads = clean
                    .reads
                    .last()
                    .unwrap()
                    .get(&(CF_NATIVE_BALANCES.to_string(), maker.clone()))
                    .copied()
                    .unwrap_or(0);
                assert!(reads >= 1, "maker 1's balance is read in the last block");
                for n in 1..=reads + 1 {
                    let fault = LastBlockFault::Many(vec![
                        LastBlockFault::ReadCfNth(CF_NATIVE_BALANCES, maker.clone(), n),
                        LastBlockFault::PlanPanic(market),
                    ]);
                    let what = format!(
                        "maker balance read {n}/{reads} + plan panic {market}, cap {cap}, \
                         record_fills {record_fills}"
                    );
                    let pick = |_: &RunOut, _: &RunStats| fault.clone();
                    for (no_merge, off, ons) in
                        assert_streamed_equivalent(&blocks, r, &[0, 0x2F00D], &pick, &what)
                    {
                        if off
                            .fatal
                            .as_deref()
                            .is_some_and(|f| f.starts_with("settle maker fill"))
                        {
                            settle_faults += 1;
                            for on in ons {
                                assert!(
                                    on.hybrid >= 1,
                                    "{what}, L2.2 {no_merge}: the fault hit the streamed \
                                     prefix before the fallback ({on:?})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        settle_faults > 0,
        "some read hit the settle (pass B / sequential)"
    );
}

/// The last block's balance and position rows its calls write (fault picks).
fn last_block_written(clean: &RunOut) -> Vec<(&'static str, Vec<u8>)> {
    let mut keys: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for call in &clean.blocks.last().expect("blocks").calls {
        for w in &call.writes {
            for cf in [CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS] {
                if w.0 == cf && !keys.iter().any(|(c, k)| *c == cf && *k == w.1) {
                    keys.push((cf, w.1.clone()));
                }
            }
        }
    }
    keys
}

/// One random fault for the last block, picked from what the clean flag-off
/// run read and wrote there: a one-shot read failure of a balance or a
/// position row at any of its reads (Phase 2, the matching reader, pass A,
/// pass B, the sequential loop, the cum-volume flush), a plan panic, a match
/// panic, or a failing write (the end-of-call flush).
fn one_random_fault(
    rng: &mut Lcg,
    reads: &[(&'static str, Vec<u8>, usize)],
    snapshot: &[(&'static str, Vec<u8>)],
    written: &[(&'static str, Vec<u8>)],
    markets: u64,
) -> LastBlockFault {
    let read_of = |rng: &mut Lcg, cf: &str| {
        let of_cf: Vec<_> = reads.iter().filter(|r| r.0 == cf).collect();
        if of_cf.is_empty() {
            return LastBlockFault::PlanPanic(1 + rng.below(markets));
        }
        let (cf, key, count) = of_cf[rng.below(of_cf.len() as u64) as usize].clone();
        LastBlockFault::ReadCfNth(cf, key, 1 + rng.below(count as u64) as usize)
    };
    match rng.below(11) {
        0..=3 => read_of(rng, CF_NATIVE_BALANCES),
        4..=5 => read_of(rng, CF_NATIVE_POSITIONS),
        6 | 7 => LastBlockFault::PlanPanic(1 + rng.below(markets)),
        8 => LastBlockFault::MatchPanic(1 + rng.below(markets)),
        10 if snapshot.is_empty() => LastBlockFault::PlanPanic(1 + rng.below(markets)),
        10 => {
            let (cf, key) = snapshot[rng.below(snapshot.len() as u64) as usize].clone();
            LastBlockFault::SnapshotCf(cf, key)
        }
        _ if written.is_empty() => LastBlockFault::MatchPanic(1 + rng.below(markets)),
        _ => {
            let (cf, key) = written[rng.below(written.len() as u64) as usize].clone();
            LastBlockFault::WriteCf(cf, key)
        }
    }
}

/// Randomized fault / panic matrix: `cases` random streams (2-8 markets, 2-3
/// blocks, storms of 20-160 actions; forced parallel at caps 1, 2 or 8, the
/// engine path, or production Auto; with or without a fill subscriber,
/// aliased rows and a scheduling perturbation), each with one or two random
/// faults in the last block ([`one_random_fault`]); a third of the cases pair
/// a plan panic with a balance read fault (the round-2 class: a fault in the
/// streamed prefix, then a fallback). Flag on must equal flag off, L2.2 off
/// and on. Returns (cases that fell back after a streamed prefix, cases that
/// went fatal) so callers can check the matrix is not vacuous.
fn fault_matrix(seed: u64, cases: usize) -> (usize, usize) {
    let mut rng = Lcg(seed);
    let (mut replays, mut fatal) = (0usize, 0usize);
    for case in 0..cases {
        let markets = 2 + rng.below(7);
        let n_blocks = 2 + rng.below(2) as usize;
        let storm = 20 + rng.below(141);
        let stream_seed = rng.next();
        let blocks = stream(stream_seed, markets, n_blocks, storm);
        let mode = match rng.below(6) {
            0 => Mode::Auto,
            1 => Mode::Engine(4),
            k => Mode::Forced {
                parallel: true,
                cap: [1usize, 2, 8, 8][k as usize - 2],
            },
        };
        let r = Run {
            mode,
            record_fills: rng.below(2) == 0,
            aliases: rng.below(4) == 0,
            jitter: 0,
        };
        let jitter = if rng.below(2) == 0 { 0 } else { rng.next() | 1 };
        let fault_seed = rng.next();
        let pick = |clean: &RunOut, stats: &RunStats| {
            let mut rng = Lcg(fault_seed);
            let reads: Vec<(&'static str, Vec<u8>, usize)> = stats
                .reads
                .last()
                .expect("blocks")
                .iter()
                .filter_map(|((cf, k), n)| {
                    let cf = [CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS]
                        .into_iter()
                        .find(|c| c == cf)?;
                    Some((cf, k.clone(), *n))
                })
                .collect();
            let snapshot: Vec<(&'static str, Vec<u8>)> = stats
                .snapshot_reads
                .last()
                .expect("blocks")
                .keys()
                .filter_map(|(cf, k)| {
                    let cf = [CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS]
                        .into_iter()
                        .find(|c| c == cf)?;
                    Some((cf, k.clone()))
                })
                .collect();
            let written = last_block_written(clean);
            let one = |rng: &mut Lcg| one_random_fault(rng, &reads, &snapshot, &written, markets);
            match rng.below(3) {
                // The round-2 class, aimed: a maker that closes a position
                // in the last block's first call and sends nothing in it
                // (so the call's balance cache does not hold it: its first
                // settle read is a backend read, often its PnL credit in
                // pass B) fails at its last read of that call, and a higher
                // market's plan panics (so its market streams before the
                // fallback). Random if no such maker.
                0 => {
                    let call0 = &clean.blocks.last().expect("blocks").calls[0];
                    let senders: Vec<Address> = blocks.last().expect("blocks")[0]
                        .iter()
                        .map(|(s, _)| *s)
                        .collect();
                    let makers: Vec<(Address, MarketId)> = call0
                        .fills
                        .iter()
                        .enumerate()
                        .filter(|(i, f)| {
                            let maker_sold = f.taker_side == 0;
                            f.market < markets
                                && !senders.contains(&f.maker)
                                && call0.extras.get(*i).is_none_or(|x| {
                                    (maker_sold && x.maker_start_raw > 0)
                                        || (!maker_sold && x.maker_start_raw < 0)
                                })
                        })
                        .map(|(_, f)| (f.maker, f.market))
                        .collect();
                    let calls = &stats.call_reads.last().expect("blocks");
                    if makers.is_empty() {
                        one(&mut rng)
                    } else {
                        let (maker, market) = makers[rng.below(makers.len() as u64) as usize];
                        let key = maker.as_slice().to_vec();
                        let panic = market + 1 + rng.below(markets - market);
                        // Its last counted read of the call (a settle read;
                        // matching's snapshot reads are not counted).
                        match calls[0].get(&(CF_NATIVE_BALANCES.to_string(), key.clone())) {
                            Some(&n) => LastBlockFault::Many(vec![
                                LastBlockFault::PlanPanic(panic),
                                LastBlockFault::ReadCfNth(CF_NATIVE_BALANCES, key, n),
                            ]),
                            None => one(&mut rng),
                        }
                    }
                }
                1 => one(&mut rng),
                _ => LastBlockFault::Many(vec![one(&mut rng), one(&mut rng)]),
            }
        };
        let what = format!(
            "matrix {seed:#x} case {case}: {markets} markets, {n_blocks} blocks, storm {storm}, \
             fault seed {fault_seed:#x}"
        );
        for (_, off, ons) in assert_streamed_equivalent(&blocks, r, &[jitter], &pick, &what) {
            fatal += usize::from(off.fatal.is_some());
            replays += usize::from(ons.iter().any(|on| on.hybrid > 0));
        }
    }
    (replays, fatal)
}

#[test]
fn settle_streamed_fault_matrix_a() {
    let (replays, fatal) = fault_matrix(0x8C21_3A7A_0001, 12);
    assert!(replays > 0 && fatal > 0, "replays {replays}, fatal {fatal}");
}

#[test]
fn settle_streamed_fault_matrix_b() {
    let (replays, fatal) = fault_matrix(0x8C21_3A7A_0002, 12);
    assert!(replays > 0 && fatal > 0, "replays {replays}, fatal {fatal}");
}

#[test]
fn settle_streamed_fault_matrix_c() {
    let (replays, fatal) = fault_matrix(0x8C21_3A7A_0003, 12);
    assert!(replays > 0 && fatal > 0, "replays {replays}, fatal {fatal}");
}

#[test]
fn settle_streamed_fault_matrix_d() {
    let (replays, fatal) = fault_matrix(0x8C21_3A7A_0004, 12);
    assert!(replays > 0 && fatal > 0, "replays {replays}, fatal {fatal}");
}
