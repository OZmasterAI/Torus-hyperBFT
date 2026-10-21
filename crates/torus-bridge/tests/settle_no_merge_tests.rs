//! 8k L2.2 settle-no-merge (`TORUS_SETTLE_NO_MERGE`, node-local, on by
//! default since s131; `0` turns it off): the parallel settle's pass B keeps
//! each market's plan cache as a run instead of `merge_disjoint`-ing it into
//! the batch cache, and the end-of-call flush writes the batch cache and the
//! runs together (`PositionCache::flush_with_runs`) in the same global key
//! order.
//!
//! THE contract under test: the flag changes where pass B's position entries
//! sit until the flush, never what is written or in which order. Over a
//! stream of multi-market blocks (each block the two `execute_batch` calls of
//! a block, pre- and post-EVM, on one `NativeStateOverlay` per block, flushed
//! to the DB after it), at pass-A worker caps 1, 2 and 8, the flag-on run
//! must equal the flag-off run in:
//! - the backend write sequence: every put and delete of every CF, in issue
//!   order, recorded by a wrapper around the block overlay;
//! - the overlay's pending native set per block (`dirty_native_keys`);
//! - every `NativeActionResult` field, the gas, the fills and their extras;
//! - the fatal error, the DB at the end and the native state root.
//!
//! Two streams end in a fault: (1) a block whose pass A hits a one-shot
//! position read failure (plan-failure fallback to the sequential loop, which
//! then fail-stops the block, R02), and (2) a block whose position flush
//! fails at one row (fatal "position cache flush failed").
//!
//! The flag is pinned per thread (`with_settle_no_merge`) so both arms run in
//! one process whatever the env says. The flag-on arm must have taken the
//! unmerged flush (`settle_no_merge_flushes_on_this_thread`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    parse_settle_no_merge, settle_no_merge_fallbacks_on_this_thread,
    settle_no_merge_flushes_on_this_thread, with_settle_no_merge,
    NativeActionResult, NativeExecContext, NativeExecutor,
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

/// What the recorder injects.
#[derive(Default)]
struct Faults {
    /// The n-th (1-based) point read of this position row in the block
    /// fails, once.
    read_nth: Option<(Vec<u8>, usize)>,
    /// Every write of this `(cf, key)` fails.
    write: Option<(String, Vec<u8>)>,
    /// Injected failures so far.
    hits: usize,
}

/// Wraps the block's `NativeStateOverlay`: records every write in order and
/// injects the armed faults. Reads delegate (it does not forward the overlay's
/// resident-row hints, so the executor reads through `get_cf_raw`).
#[derive(Clone)]
struct Recorder {
    inner: NativeStateOverlay,
    log: Arc<Mutex<Vec<Write>>>,
    faults: Arc<Mutex<Faults>>,
    /// Point reads of each position row so far (picks the n-th read).
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
            return Err(StateError::Io(std::io::Error::other("injected write failure")));
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

// ---- Scenario helpers (as in settle_defer_results_tests.rs) ----

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

/// Tiny deterministic LCG so the scenario is reproducible with no deps.
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

/// `blocks` blocks over `markets` markets and 40 interleaved traders. Block 1
/// rests a ladder on every market; every later batch is a crossing storm
/// drawing a market PER ORDER (the bench shape), mixing GTC / IOC / FOK /
/// PostOnly, sizes that open, extend, reduce, flip and fully close
/// positions, STP self-trades and cancels by order id of earlier orders.
fn stream(seed: u64, markets: u64, blocks: usize, storm: u64) -> Vec<Block> {
    let mut rng = Lcg(seed);
    let markets: Vec<MarketId> = (1..=markets).collect();
    let mut ladder = Vec::new();
    for &m in &markets {
        for lvl in 0..4i64 {
            let maker = addr((1 + rng.below(TRADERS)) as u8);
            ladder.push(place(
                maker,
                order(m, true, fp(97 + lvl), fp(1 + rng.below(4) as i64), TimeInForce::GTC),
            ));
            let maker = addr((1 + rng.below(TRADERS)) as u8);
            ladder.push(place(
                maker,
                order(m, false, fp(101 + lvl), fp(1 + rng.below(4) as i64), TimeInForce::GTC),
            ));
        }
    }
    let storm_batch = |rng: &mut Lcg| {
        let mut batch = Vec::new();
        for i in 0..storm {
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
                    batch.push(place(sender, order(m, true, fp(100), qty, TimeInForce::GTC)));
                    batch.push(place(sender, order(m, false, fp(100), qty, tif)));
                }
                // Cancel an earlier order by id (often unknown: a failure).
                29 => batch.push((
                    sender,
                    NativeAction::CancelOrder {
                        order_id: 1 + rng.below(400) as u128,
                    },
                )),
                _ => batch.push(place(sender, order(m, is_buy, fp(price), qty, tif))),
            }
        }
        batch
    };
    let mut out: Vec<Block> = vec![[ladder, storm_batch(&mut rng)]];
    for _ in 1..blocks {
        out.push([storm_batch(&mut rng), storm_batch(&mut rng)]);
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

/// One `execute_batch` call's outcome.
#[derive(PartialEq, Eq, Debug)]
struct CallOut {
    writes: Vec<Write>,
    results: Vec<ResultRow>,
    total_gas: u64,
    fills: Vec<TradeFill>,
    extras: Vec<FillExtras>,
    fatal: Option<String>,
}

/// One block's outcome.
#[derive(PartialEq, Eq, Debug)]
struct BlockOut {
    calls: Vec<CallOut>,
    /// The overlay's pending native set after the block (book save included).
    pending: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>,
    /// The book save's writes, sorted (their order follows a HashSet).
    save_writes: Vec<Write>,
    /// Point reads per position row in the block (pass A's are the last:
    /// pass B, the flush and the book save read no position).
    position_reads: BTreeMap<Vec<u8>, usize>,
}

/// The run's outcome.
#[derive(PartialEq, Eq, Debug)]
struct RunOut {
    blocks: Vec<BlockOut>,
    db_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    state_root: B256,
}

/// What the run counts besides its outcome (not part of the comparison).
#[derive(Default, Debug)]
struct RunStats {
    fallbacks: u64,
    /// Calls that ran pass A (the parallel settle).
    parallel_calls: u64,
    /// Unmerged flushes (`settle_no_merge_flushes_on_this_thread` delta).
    no_merge_flushes: u64,
    /// Flushes with runs that fell back to merge + flush_all
    /// (`settle_no_merge_fallbacks_on_this_thread` delta).
    no_merge_fallbacks: u64,
    /// Position deletes (fully closed positions) written.
    position_deletes: usize,
    position_puts: usize,
    hits: usize,
}

/// The fault armed before the LAST block.
#[derive(Clone, Debug)]
enum LastBlockFault {
    None,
    /// The n-th point read of this position row in the block fails, once.
    ReadNth(Vec<u8>, usize),
    /// Every write of this position row fails.
    Write(Vec<u8>),
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

/// One stored "opaque" position row: (trader, key market, row market, size).
type AliasRow = (u8, MarketId, MarketId, i64);

/// Rows to store raw and the block (0-based) before which they appear.
type Aliases<'a> = Option<(&'a [AliasRow], usize)>;

/// Codex P1 (review of dbe14b21): stored "opaque" position rows whose
/// fields name another market than their key, so two markets' pass-A caches
/// hold the same key. Trader 5: key (5, 1) says market 2, key (5, 2) says
/// market 3 (market 2's cache keeps (5, 2) CLEAN while market 1's dirties
/// it). Trader 6: key (6, 1) says market 2 and (6, 2) is a plain market-2
/// row (both caches dirty (6, 2)). (trader, key market, row market, size).
const ALIASES: [AliasRow; 4] = [(5, 1, 2, 2), (5, 2, 3, 4), (6, 1, 2, 2), (6, 2, 2, 4)];

/// Pre-testnet "aliased opaque position rows": one row, under key (5, 1),
/// naming market 2.
const ONE_ALIAS: [AliasRow; 1] = [(5, 1, 2, 2)];

fn seed_aliases(db: &StateDb, rows: &[AliasRow]) {
    for &(t, key_market, market_id, size) in rows {
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

/// Feeds `blocks` through one context per block over a fresh overlay on
/// one DB (flushed after each block, as the committer does), on the forced
/// parallel settle with the pass-A cap pinned. A block that goes fatal is
/// never flushed and ends the run.
fn run(blocks: &[Block], cap: usize, record_fills: bool, fault: &LastBlockFault) -> (RunOut, RunStats) {
    run_seeded(blocks, cap, record_fills, fault, None, true)
}

/// [`run`], with `aliases`' rows stored raw before their block, on the
/// forced parallel settle (`parallel`) or the sequential one.
fn run_seeded(
    blocks: &[Block],
    cap: usize,
    record_fills: bool,
    fault: &LastBlockFault,
    aliases: Aliases<'_>,
    parallel: bool,
) -> (RunOut, RunStats) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=64u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    for n in 1..=TRADERS as u8 {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        torus_core::position::PositionManager::new(db.clone())
            .put_native_balance(&addr(n), &bal)
            .unwrap();
    }
    let mut stats = RunStats::default();
    let mut out = Vec::new();
    let flushes_before = settle_no_merge_flushes_on_this_thread();
    let fallbacks_before = settle_no_merge_fallbacks_on_this_thread();
    for (h, block) in blocks.iter().enumerate() {
        let last = h + 1 == blocks.len();
        if let Some((rows, before)) = aliases {
            if before == h {
                seed_aliases(&db, rows);
            }
        }
        let rec = Recorder {
            inner: NativeStateOverlay::new(db.clone()),
            log: Arc::default(),
            faults: Arc::default(),
            position_reads: Arc::default(),
        };
        if last {
            let mut f = rec.faults.lock().unwrap();
            match fault {
                LastBlockFault::None => {}
                LastBlockFault::ReadNth(k, n) => f.read_nth = Some((k.clone(), *n)),
                LastBlockFault::Write(k) => {
                    f.write = Some((CF_NATIVE_POSITIONS.to_string(), k.clone()))
                }
            }
        }
        let mut ctx = NativeExecContext::new(
            rec.clone(),
            1 + h as u64, // block_height
            1000 + h as u64,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
        );
        ctx.record_fills = record_fills;
        let mut calls = Vec::new();
        for batch in block {
            let pass_a = ctx.phase_accum.settle_pass_a_ns;
            let r = NativeExecutor::execute_batch_settle_workers(&mut ctx, batch, parallel, Some(cap));
            if ctx.phase_accum.settle_pass_a_ns > pass_a {
                stats.parallel_calls += 1;
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
                results: r.results.iter().map(row).collect(),
                total_gas: r.total_gas,
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
        // Sorted: `write_bbo_rows` walks the `dirty_books` HashSet (seeded
        // per instance), so the book save's write ORDER differs between any
        // two contexts, flag or not (the overlay content does not). The flag
        // only touches `execute_batch`'s writes, compared in issue order.
        let mut save_writes = std::mem::take(&mut *rec.log.lock().unwrap());
        save_writes.sort();
        out.push(BlockOut {
            calls,
            pending: rec.inner.dirty_native_keys(),
            save_writes,
            position_reads: std::mem::take(&mut *rec.position_reads.lock().unwrap()),
        });
        stats.hits += rec.faults.lock().unwrap().hits;
        drop(ctx);
        if fatal {
            assert!(last, "only the last block may go fatal (block {})", h + 1);
            break;
        }
        rec.inner.flush(&db).expect("flush block overlay");
    }
    stats.no_merge_flushes = settle_no_merge_flushes_on_this_thread() - flushes_before;
    stats.no_merge_fallbacks = settle_no_merge_fallbacks_on_this_thread() - fallbacks_before;
    let out = RunOut {
        blocks: out,
        db_dump: db_dump(&db),
        state_root: compute_native_state_root(&db).expect("state root"),
    };
    (out, stats)
}

/// Flag off vs on (and the unpinned env run, which must equal flag off
/// unless the env turns it on) over `blocks`, at caps 1, 2 and 8, with and
/// without a fill subscriber. Returns the flag-off and flag-on runs' stats.
fn assert_no_merge_equivalent(
    blocks: &[Block],
    fault: &dyn Fn(&RunOut) -> LastBlockFault,
    what: &str,
) -> Vec<RunStats> {
    assert_no_merge_equivalent_seeded(blocks, fault, what, None)
        .into_iter()
        .map(|(off, on)| {
            assert_eq!(on.no_merge_fallbacks, 0, "{what}: per-market caches are disjoint ({on:?})");
            off
        })
        .collect()
}

/// [`assert_no_merge_equivalent`] over [`run_seeded`].
fn assert_no_merge_equivalent_seeded(
    blocks: &[Block],
    fault: &dyn Fn(&RunOut) -> LastBlockFault,
    what: &str,
    aliases: Aliases<'_>,
) -> Vec<(RunStats, RunStats)> {
    let mut all = Vec::new();
    for record_fills in [true, false] {
        for cap in [1usize, 2, 8] {
            let label = format!("{what}, cap {cap}, record_fills {record_fills}");
            // The fault is chosen from a fault-free flag-off run.
            let (clean, _) = with_settle_no_merge(false, || {
                run_seeded(blocks, cap, record_fills, &LastBlockFault::None, aliases, true)
            });
            let fault = fault(&clean);
            let (off, off_stats) =
                with_settle_no_merge(false, || run_seeded(blocks, cap, record_fills, &fault, aliases, true));
            let (on, on_stats) =
                with_settle_no_merge(true, || run_seeded(blocks, cap, record_fills, &fault, aliases, true));
            assert_eq!(
                (off_stats.no_merge_flushes, off_stats.no_merge_fallbacks),
                (0, 0),
                "{label}: flag off never skips the merge"
            );
            assert!(
                on_stats.no_merge_flushes > 0,
                "{label}: flag on never took the unmerged flush ({on_stats:?})"
            );
            assert_eq!(
                (off_stats.fallbacks, off_stats.parallel_calls, off_stats.hits),
                (on_stats.fallbacks, on_stats.parallel_calls, on_stats.hits),
                "{label}: same settle paths and faults"
            );
            // Every parallel call without a plan fallback flushed with runs:
            // unmerged, or merged when the runs were not provably disjoint.
            assert_eq!(
                on_stats.no_merge_flushes + on_stats.no_merge_fallbacks,
                on_stats.parallel_calls - on_stats.fallbacks,
                "{label}: flushes with runs"
            );
            assert_eq!(off.blocks.len(), on.blocks.len(), "{label}: blocks run");
            for (h, (a, b)) in off.blocks.iter().zip(&on.blocks).enumerate() {
                for (c, (x, y)) in a.calls.iter().zip(&b.calls).enumerate() {
                    assert!(
                        x.writes == y.writes,
                        "{label}: block {} call {c}: write sequence diverged ({} vs {} writes)",
                        h + 1,
                        x.writes.len(),
                        y.writes.len()
                    );
                }
                assert_eq!(a, b, "{label}: block {} diverged", h + 1);
            }
            assert_eq!(off, on, "{label}: run diverged");
            let env_on = parse_settle_no_merge(std::env::var("TORUS_SETTLE_NO_MERGE").ok().as_deref());
            let (plain, _) = run_seeded(blocks, cap, record_fills, &fault, aliases, true);
            assert_eq!(
                if env_on { &on } else { &off },
                &plain,
                "{label}: unpinned (env) run diverged"
            );
            all.push((off_stats, on_stats));
        }
    }
    all
}

/// The first position write of the LAST block's post-EVM call's flush
/// (`calls[1]`), else its pre-EVM call's: a row the flush writes and pass A
/// reads first in that call.
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

/// A fault-free stream: 8 blocks of 2 calls, 24 markets, a market per order.
#[test]
fn settle_no_merge_identical_block_stream() {
    let blocks = stream(0x8C_0022_5EED, 24, 8, 500);
    for s in assert_no_merge_equivalent(&blocks, &|_| LastBlockFault::None, "clean") {
        assert_eq!((s.fallbacks, s.hits), (0, 0));
        assert_eq!(s.parallel_calls, 16, "every call ran the parallel settle");
        // Not vacuous: positions written AND fully closed ones deleted.
        assert!(s.position_puts > 1_000, "{s:?}");
        assert!(s.position_deletes > 20, "{s:?}");
    }
}

/// Second seed, fewer and wider markets.
#[test]
fn settle_no_merge_identical_second_seed() {
    let blocks = stream(0xDEAD_BEEF_8C22, 6, 6, 400);
    for s in assert_no_merge_equivalent(&blocks, &|_| LastBlockFault::None, "seed 2") {
        assert_eq!(s.fallbacks, 0);
        assert!(s.position_deletes > 0, "{s:?}");
    }
}

/// The last block's pass A hits a one-shot position read failure: the call
/// falls back to the sequential loop (flushed through `pos_cache` alone, no
/// runs), which fail-stops the block (R02). Same writes, results and fatal
/// with the flag on; the earlier blocks took the unmerged flush. The failing
/// read is the row's LAST read in the clean block: Phase 2 reads a sender's
/// row before pass A does, and nothing after pass A reads it.
#[test]
fn settle_no_merge_identical_with_plan_failure_fallback() {
    let blocks = stream(0x8C_0022_FA11, 12, 5, 400);
    let fault = |clean: &RunOut| {
        let key = last_block_position_key(clean, |_| 0);
        let reads = clean.blocks.last().unwrap().position_reads[&key];
        assert!(reads >= 1, "pass A reads the row");
        LastBlockFault::ReadNth(key, reads)
    };
    for s in assert_no_merge_equivalent(&blocks, &fault, "fallback") {
        assert_eq!(s.fallbacks, 1, "the faulted call fell back: {s:?}");
        assert_eq!(s.hits, 1, "the fault fired once: {s:?}");
    }
}

/// The last block's position flush fails at a row in the middle: the flush
/// stops there with "position cache flush failed" (fatal), in both arms at
/// the same write.
#[test]
fn settle_no_merge_identical_with_fatal_flush_failure() {
    let blocks = stream(0x8C_0022_DEAD, 12, 5, 400);
    let fault = |clean: &RunOut| LastBlockFault::Write(last_block_position_key(clean, |r| r.len() / 2));
    for s in assert_no_merge_equivalent(&blocks, &fault, "fatal flush") {
        assert_eq!(s.fallbacks, 0, "{s:?}");
        assert!(s.hits >= 1, "the write fault fired: {s:?}");
    }
    // The fatal is the flush's.
    let (clean, _) = with_settle_no_merge(true, || run(&blocks, 2, true, &LastBlockFault::None));
    let fault = fault(&clean);
    let (on, _) = with_settle_no_merge(true, || run(&blocks, 2, true, &fault));
    let fatal = on.blocks.last().unwrap().calls.last().unwrap().fatal.clone();
    assert!(
        fatal
            .as_deref()
            .is_some_and(|f| f.starts_with("position cache flush failed")),
        "{fatal:?}"
    );
}

/// Two blocks on markets 1 and 2 (makers 1 and 2 rest asks at 101 and bids
/// at 99; traders 3 and 4 cross both markets in every call, so every call
/// before the alias block runs the parallel settle with fills). Block 1
/// also rests trader 5's asks at 100 on both markets when `maker`. Block 2:
/// trader 3 buys at 100 on both markets (filling trader 5's asks: trader 5
/// is the maker) when `maker`, else trader 5 buys at 101 on both markets
/// (trader 5 is the taker).
fn alias_blocks(maker: bool) -> Vec<Block> {
    let gtc = TimeInForce::GTC;
    let ioc = TimeInForce::IOC;
    let mut ladder = Vec::new();
    for m in [1, 2] {
        ladder.push(place(addr(m as u8), order(m, false, fp(101), fp(50), gtc)));
        ladder.push(place(addr(m as u8), order(m, true, fp(99), fp(50), gtc)));
    }
    let cross = |out: &mut Vec<(Address, NativeAction)>| {
        for m in [1, 2] {
            out.push(place(addr(3), order(m, true, fp(101), fp(1), ioc)));
            out.push(place(addr(4), order(m, false, fp(99), fp(1), ioc)));
        }
    };
    cross(&mut ladder);
    if maker {
        for m in [1, 2] {
            ladder.push(place(addr(5), order(m, false, fp(100), fp(2), gtc)));
        }
    }
    let mut second = Vec::new();
    cross(&mut second);
    let mut hit = Vec::new();
    for m in [1, 2] {
        if maker {
            hit.push(place(addr(3), order(m, true, fp(100), fp(1), ioc)));
        } else {
            hit.push(place(addr(5), order(m, true, fp(101), fp(1), ioc)));
        }
        hit.push(place(addr(4), order(m, false, fp(99), fp(1), ioc)));
    }
    let mut tail = Vec::new();
    cross(&mut tail);
    vec![[ladder, second], [hit, tail]]
}

/// Results, fills and fatal per call, the DB and the state root: what the
/// settle mode must not change (the reads and write order may).
type Outcome = (Vec<Vec<(Vec<ResultRow>, Vec<TradeFill>, Option<String>)>>, Vec<(String, Vec<u8>, Vec<u8>)>, B256);

fn outcome(run: &RunOut) -> Outcome {
    let calls = run
        .blocks
        .iter()
        .map(|b| b.calls.iter().map(|c| (c.results.clone(), c.fills.clone(), c.fatal.clone())).collect())
        .collect();
    (calls, run.db_dump.clone(), run.state_root)
}

/// The block's fatal error (its last call's), if any.
fn last_fatal(run: &RunOut) -> Option<String> {
    run.blocks.last().and_then(|b| b.calls.last()).and_then(|c| c.fatal.clone())
}

/// Pre-testnet "aliased opaque position rows" (roadmap, found s128): a
/// stored row whose fields name another market than its key, (5, 1)
/// holding a market-2 position. RED on main: the parallel settle's
/// per-market caches both file trader 5's fills under (5, 2) and the merge
/// keeps market 2's, so market 1's update is lost; the sequential settle
/// keeps both. The fix refuses the row on load: every settle mode stops the
/// block alike.
#[test]
fn serial_and_parallel_settle_agree_on_an_aliased_row() {
    for maker in [false, true] {
        // Taker: the row is there from the start. Maker: it appears before
        // block 2 (trader 5's asks rested in block 1 with no position).
        let aliases: Aliases<'_> = Some((&ONE_ALIAS, if maker { 1 } else { 0 }));
        let blocks = alias_blocks(maker);
        for cap in [1usize, 2, 8] {
            let label = format!("maker {maker}, cap {cap}");
            let (serial, _) = run_seeded(&blocks, cap, true, &LastBlockFault::None, aliases, false);
            let (parallel, stats) = run_seeded(&blocks, cap, true, &LastBlockFault::None, aliases, true);
            assert!(stats.parallel_calls >= 2, "{label}: the parallel settle ran ({stats:?})");
            assert_eq!(outcome(&serial), outcome(&parallel), "{label}: serial vs parallel settle");
        }
    }
}

/// The fix's failure mode: an aliased row is corrupt state (no writer makes
/// one), refused on load as a local fault, so the node fail-stops the block
/// (R02) before its flush, in every settle mode and with the trader as
/// taker or maker. The row stays as stored.
#[test]
fn an_aliased_position_row_fail_stops_the_block() {
    for maker in [false, true] {
        let aliases: Aliases<'_> = Some((&ONE_ALIAS, if maker { 1 } else { 0 }));
        let blocks = alias_blocks(maker);
        for parallel in [false, true] {
            let label = format!("maker {maker}, parallel {parallel}");
            let (run, _) = run_seeded(&blocks, 2, true, &LastBlockFault::None, aliases, parallel);
            assert_eq!(run.blocks.len(), 2, "{label}");
            let fatal = last_fatal(&run);
            assert!(
                fatal.as_deref().is_some_and(|f| f.contains("borsh error: position row")),
                "{label}: the block fail-stops on the aliased row: {fatal:?}"
            );
            let key = position_key(&addr(5), 1);
            let stored: Vec<_> = run
                .db_dump
                .iter()
                .filter(|(cf, k, _)| cf == CF_NATIVE_POSITIONS && k.starts_with(addr(5).as_slice()))
                .map(|(_, k, _)| k.clone())
                .collect();
            assert_eq!(stored, vec![key.to_vec()], "{label}: block 2 never flushed");
        }
    }
}

/// Codex P1 (review of dbe14b21), after the pre-testnet fix: the
/// [`ALIASES`] rows (clean in one market's cache and dirty in another's, or
/// dirty in both, before the fix) are refused on load, so no pass-A cache
/// ever holds a foreign market's key: flag on and off stop the block alike
/// and the flag-on flush never falls back to the merge.
#[test]
fn settle_no_merge_identical_with_aliased_opaque_rows() {
    for maker in [false, true] {
        let blocks = alias_blocks(maker);
        let aliases: Aliases<'_> = Some((&ALIASES, if maker { 1 } else { 0 }));
        let stats = assert_no_merge_equivalent_seeded(&blocks, &|_| LastBlockFault::None, "aliases", aliases);
        for (off, on) in stats {
            assert_eq!(off.fallbacks, on.fallbacks, "{off:?} {on:?}");
            assert_eq!(on.no_merge_fallbacks, 0, "no overlap left: {on:?}");
        }
        let (run, _) = run_seeded(&blocks, 2, true, &LastBlockFault::None, aliases, true);
        let fatal = last_fatal(&run);
        assert!(
            fatal.as_deref().is_some_and(|f| f.contains("borsh error: position row")),
            "maker {maker}: {fatal:?}"
        );
    }
}
