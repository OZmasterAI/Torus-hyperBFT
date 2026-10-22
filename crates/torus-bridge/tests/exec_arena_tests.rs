//! s80 exec-block-arena (`TORUS_S80_EXEC_ARENA`, node-local, default off):
//! the engine's per-market scratch maps (pass A's plan position caches, the
//! books' batch reduce-only and account-margin maps, the pool workers' maker
//! cache) are cleared and kept between `execute_batch` calls instead of
//! dropped (`torus_core::recycle`).
//!
//! THE contract under test: the flag changes capacities only, never what is
//! written, in which order, or returned. Over streams of multi-market blocks
//! (each block the two `execute_batch` calls of a block on one overlay,
//! flushed to the DB after it; the harness of `settle_no_merge_tests.rs`),
//! at pass-A caps 1, 2 and 8, with L2.1 streamed settle off and on and L2.2
//! no-merge on and off, the flag-on run must equal the flag-off run in the
//! backend write sequence, every result field, gas, fills and extras, the
//! pending native set, the fatal error, the DB and the native state root.
//! Each flag-on arm runs TWICE in a row, so the second run starts from maps
//! the first one left on the free lists (no entry may survive). The flag-on
//! arms must have recycled (`torus_core::recycle::stats` hits grew).
//!
//! The flag is pinned process-wide (`pin_exec_arena`, the pool workers read
//! it), so this binary's tests take one lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    parse_exec_arena, pin_exec_arena, settle_no_merge_flushes_on_this_thread,
    settle_no_merge_fallbacks_on_this_thread, settle_streamed_calls_on_this_thread,
    with_settle_no_merge, with_settle_streamed, NativeActionResult, NativeExecContext,
    NativeExecutor,
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
#[allow(dead_code)]
const ALIASES: [AliasRow; 4] = [(5, 1, 2, 2), (5, 2, 3, 4), (6, 1, 2, 2), (6, 2, 2, 4)];

/// Pre-testnet "aliased opaque position rows": one row, under key (5, 1),
/// naming market 2.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
            // The block's fail-stop as the committer sees it
            // (`take_fatal_error`): `fatal_error`, else a reader fault (R02).
            let fatal = ctx.fatal_error.clone().or_else(|| ctx.reader_fault.get().cloned());
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
        let fatal = ctx.fatal_error.is_some() || ctx.reader_fault.get().is_some();
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

/// One test at a time: the flag pin is process-wide.
static LOCK: Mutex<()> = Mutex::new(());

/// Restores the env reading of the flag on drop (also on a failed assert).
struct Unpin;
impl Drop for Unpin {
    fn drop(&mut self) {
        pin_exec_arena(None);
    }
}

fn arm(on: bool, blocks: &[Block], cap: usize, streamed: bool, no_merge: bool, fault: &LastBlockFault) -> (RunOut, RunStats, u64) {
    pin_exec_arena(Some(on));
    let hits0 = torus_core::recycle::stats().0;
    let streamed0 = settle_streamed_calls_on_this_thread();
    let (out, stats) = with_settle_streamed(streamed, || {
        with_settle_no_merge(no_merge, || run_seeded(blocks, cap, true, fault, None, true))
    });
    let streamed_calls = settle_streamed_calls_on_this_thread() - streamed0;
    assert_eq!(streamed_calls > 0, streamed, "streamed settle {streamed}: {streamed_calls} streamed calls");
    (out, stats, torus_core::recycle::stats().0 - hits0)
}

/// Flag off vs on (twice) at caps 1, 2, 8 x streamed off/on x no-merge
/// on/off. Returns the flag-off stats.
fn assert_arena_equivalent(blocks: &[Block], fault: &dyn Fn(&RunOut) -> LastBlockFault, what: &str) -> Vec<RunStats> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _unpin = Unpin;
    let mut all = Vec::new();
    for streamed in [false, true] {
        for no_merge in [true, false] {
            for cap in [1usize, 2, 8] {
                let label = format!("{what}, cap {cap}, streamed {streamed}, no_merge {no_merge}");
                let (clean, _, _) = arm(false, blocks, cap, streamed, no_merge, &LastBlockFault::None);
                let fault = fault(&clean);
                let (off, off_stats, _) = arm(false, blocks, cap, streamed, no_merge, &fault);
                for round in 0..2 {
                    let (on, on_stats, hits) = arm(true, blocks, cap, streamed, no_merge, &fault);
                    assert_eq!(
                        (off_stats.fallbacks, off_stats.parallel_calls, off_stats.hits, off_stats.no_merge_flushes),
                        (on_stats.fallbacks, on_stats.parallel_calls, on_stats.hits, on_stats.no_merge_flushes),
                        "{label} round {round}: same settle paths and faults"
                    );
                    if off_stats.parallel_calls > 1 {
                        assert!(hits > 0, "{label} round {round}: flag on never reused a map");
                    }
                    assert_eq!(off.blocks.len(), on.blocks.len(), "{label}: blocks run");
                    for (h, (a, b)) in off.blocks.iter().zip(&on.blocks).enumerate() {
                        for (c, (x, y)) in a.calls.iter().zip(&b.calls).enumerate() {
                            assert!(
                                x.writes == y.writes,
                                "{label} round {round}: block {} call {c}: write sequence diverged",
                                h + 1
                            );
                        }
                        assert_eq!(a, b, "{label} round {round}: block {} diverged", h + 1);
                    }
                    assert_eq!(off, on, "{label} round {round}: run diverged");
                }
                all.push(off_stats);
            }
        }
    }
    all
}

/// The first position write of the LAST block's last call (a row the flush
/// writes and pass A reads).
fn last_block_position_key(clean: &RunOut, pick: impl Fn(&[&Write]) -> usize) -> Vec<u8> {
    let last = clean.blocks.last().expect("blocks");
    let call = last.calls.last().expect("calls");
    let rows: Vec<&Write> = call.writes.iter().filter(|w| w.0 == CF_NATIVE_POSITIONS).collect();
    assert!(rows.len() > 4, "the last call writes positions");
    rows[pick(&rows)].1.clone()
}

#[test]
fn parse_exec_arena_is_off_unless_set() {
    assert!(!parse_exec_arena(None));
    for v in ["", "0", "off", "false", "no"] {
        assert!(!parse_exec_arena(Some(v)), "{v:?}");
    }
    for v in ["1", "on", "true", " 1 "] {
        assert!(parse_exec_arena(Some(v)), "{v:?}");
    }
}

/// A fault-free stream: 8 blocks of 2 calls, 24 markets, a market per order.
#[test]
fn exec_arena_identical_block_stream() {
    let blocks = stream(0x5_80A2_E7A, 24, 8, 500);
    for s in assert_arena_equivalent(&blocks, &|_| LastBlockFault::None, "clean") {
        assert_eq!((s.fallbacks, s.hits), (0, 0));
        assert_eq!(s.parallel_calls, 16, "every call ran the parallel settle");
        assert!(s.position_puts > 1_000, "{s:?}");
        assert!(s.position_deletes > 20, "{s:?}");
    }
}

/// Second seed, fewer and wider markets (bigger maps per market).
#[test]
fn exec_arena_identical_second_seed() {
    let blocks = stream(0xDEAD_5080_0A2E, 6, 6, 400);
    for s in assert_arena_equivalent(&blocks, &|_| LastBlockFault::None, "seed 2") {
        assert_eq!(s.fallbacks, 0);
        assert!(s.position_deletes > 0, "{s:?}");
    }
}

/// The last block's pass A hits a one-shot position read failure (plan
/// fallback to the sequential loop, then the block fail-stops): the plans'
/// caches are abandoned mid-call; same outcome with the flag on.
#[test]
fn exec_arena_identical_with_plan_failure_fallback() {
    let blocks = stream(0x5_80A2_FA11, 12, 5, 400);
    let fault = |clean: &RunOut| {
        let key = last_block_position_key(clean, |_| 0);
        let reads = clean.blocks.last().unwrap().position_reads[&key];
        assert!(reads >= 1, "pass A reads the row");
        LastBlockFault::ReadNth(key, reads)
    };
    for s in assert_arena_equivalent(&blocks, &fault, "fallback") {
        assert_eq!(s.hits, 1, "the fault fired once: {s:?}");
    }
}

/// The last block's position flush fails at a row in the middle (fatal):
/// the runs are recycled after a failed flush too; same outcome.
#[test]
fn exec_arena_identical_with_fatal_flush_failure() {
    let blocks = stream(0x5_80A2_DEAD, 12, 5, 400);
    let fault = |clean: &RunOut| LastBlockFault::Write(last_block_position_key(clean, |r| r.len() / 2));
    for s in assert_arena_equivalent(&blocks, &fault, "fatal flush") {
        assert!(s.hits >= 1, "the write fault fired: {s:?}");
    }
}
