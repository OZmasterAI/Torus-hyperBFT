//! s58 experiment `exec-pool-fine-grain-jobs` (`TORUS_EXEC_POOL_GRAIN=K`,
//! plan `docs/plans/exp-archy-s58-exec-pool-fine-grain-jobs.md`).
//!
//! With K in 2..=16, matching and settle pass A split their markets into
//! `K x cap` LPT chunks instead of `cap` (only when the cap is >= 2), so idle
//! exec-pool threads can take work that a late-starting thread has not begun.
//! The chunk layout is scheduling only: match results are sorted by market
//! id before settlement and pass-A plans are scattered back by input index.
//!
//! THE contract under test: for any batch, every K (pinned per thread with
//! `with_exec_pool_grain`) produces BYTE-IDENTICAL state to K = 1 and to the
//! sequential settle: every native CF, per-action results, gas, fills and
//! fill extras, `trade_index`, the next global order id and the native state
//! root. The fixtures have more active markets than the small caps, so
//! `K x cap` really changes the chunk count at caps 2 and 3 (pass A) and, on
//! a host with fewer than 40 threads, at the production match cap too.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use alloy_primitives::map::HashMap;
use alloy_primitives::{Address, B256};

use torus_bridge::market_workers::{
    with_exec_pool_grain, MarketBatchResult, MarketWorkerPool, MatchRequest,
};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::order_book::OrderBook;
use torus_core::position::{position_key, NativeBalance};
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
    CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::trade_rows::{FillExtras, TradeFill};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderId, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers (same idiom as parallel_settle_tests.rs) ----

const SENDERS: u8 = 32;

/// A funded trader that the fixtures never use: the planted-panic test's
/// lone taker (it never rests, so matching never reads its position).
const FRESH_TAKER: u8 = 50;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx<T: StateBackend>(state: T) -> NativeExecContext<T> {
    NativeExecContext::new(
        state,
        1,         // block_height
        1000,      // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
        addr(100), // treasury
        addr(101), // dev_pool
    )
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

/// Tiny deterministic LCG so the fuzz scenario is reproducible with no deps.
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

/// Full observable outcome of a run. Byte-identical or bust.
#[derive(PartialEq, Eq, Debug)]
struct RunFingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<(bool, Option<String>)>>,
    total_gas: Vec<u64>,
    fills: Vec<Vec<TradeFill>>,
    extras: Vec<Vec<FillExtras>>,
    trade_index: u32,
    next_global_order_id: u128,
    state_root: B256,
}

fn state_dump(db: &StateDb) -> Vec<(String, Vec<u8>, Vec<u8>)> {
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

/// Run `batches` through one fresh context with the settle mode and pass-A
/// cap pinned (a stream subscriber is on, so fill extras are compared too),
/// then fingerprint the world. The exec-pool grain is whatever this thread
/// has pinned (or the process env when nothing is pinned).
fn run_batches(
    batches: &[Vec<(Address, NativeAction)>],
    parallel: bool,
    settle_workers: Option<usize>,
) -> RunFingerprint {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    let (fingerprint, fallbacks) =
        run_on(db.clone(), &db, batches, parallel, settle_workers, |_| {});
    assert_eq!(fallbacks, 0, "no settle fallback without a planted fault");
    fingerprint
}

/// [`run_batches`] over any backend wrapping `db` (state is read back from
/// `db`); `before_batch(i)` runs right before batch `i`. Also returns the
/// context's settle fallback count.
fn run_on<T: StateBackend>(
    backend: T,
    db: &StateDb,
    batches: &[Vec<(Address, NativeAction)>],
    parallel: bool,
    settle_workers: Option<usize>,
    mut before_batch: impl FnMut(usize),
) -> (RunFingerprint, u64) {
    let mut ctx = make_ctx(backend);
    ctx.record_fills = true;
    for n in (1..=SENDERS).chain([FRESH_TAKER]) {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }

    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    let mut fills = Vec::new();
    let mut extras = Vec::new();
    for (i, batch) in batches.iter().enumerate() {
        before_batch(i);
        let r =
            NativeExecutor::execute_batch_settle_workers(&mut ctx, batch, parallel, settle_workers);
        assert!(
            ctx.fatal_error.is_none(),
            "no batch here may go fatal: {:?}",
            ctx.fatal_error
        );
        results.push(
            r.results
                .iter()
                .map(|a| (a.success, a.error.clone().map(String::from)))
                .collect(),
        );
        total_gas.push(r.total_gas);
        let (f, e) = ctx.take_pending_fills_and_extras();
        assert_eq!(e.len(), f.len(), "extras aligned with fills");
        fills.push(f);
        extras.push(e);
    }
    ctx.save_order_books();
    let fingerprint = RunFingerprint {
        cf_dump: state_dump(db),
        results,
        total_gas,
        fills,
        extras,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(db).expect("state root"),
    };
    (fingerprint, ctx.phase_accum.settle_fallbacks)
}

/// Many-market cross-margin scenario: `n_markets` markets (skewed load: a
/// third of the storm hits markets 1..=4, so LPT packing has real weights),
/// 32 senders trading on many markets each.
/// - batch 1: resting ladders on both sides of 100 in every market;
/// - batch 2: a crossing storm over all markets (GTC/IOC/FOK/PostOnly, dust
///   and tick-violation rejects, self-trade cancels);
/// - batch 3: a 12-market batch (more markets than caps 2 and 3, fewer than
///   the production match cap);
/// - batch 4: a single-market batch (the no-pool fast paths).
fn fuzz_batches(seed: u64, n_markets: u64) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let sender = |rng: &mut Lcg| addr((1 + rng.below(SENDERS as u64)) as u8);
    let market = |rng: &mut Lcg| -> MarketId {
        if rng.below(3) == 0 {
            1 + rng.below(4)
        } else {
            1 + rng.below(n_markets)
        }
    };

    let mut batch1 = Vec::new();
    for m in 1..=n_markets {
        for lvl in 0..3i64 {
            let s = sender(&mut rng);
            batch1.push(place(
                s,
                order(
                    m,
                    true,
                    fp(97 + lvl),
                    fp(1 + rng.below(4) as i64),
                    TimeInForce::GTC,
                ),
            ));
            let s = sender(&mut rng);
            batch1.push(place(
                s,
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

    let mut batch2 = Vec::new();
    for i in 0..(12 * n_markets) {
        let m = market(&mut rng);
        let s = sender(&mut rng);
        let is_buy = rng.below(2) == 0;
        let tif = match rng.below(10) {
            0 => TimeInForce::IOC,
            1 => TimeInForce::FOK,
            2 => TimeInForce::PostOnly,
            _ => TimeInForce::GTC,
        };
        let price = 96 + rng.below(9) as i64;
        let qty = FixedPoint::from_raw(
            (1 + rng.below(5)) as i128 * FixedPoint::SCALE
                + (rng.below(FixedPoint::SCALE as u64 / 2)) as i128,
        );
        match i % 29 {
            7 => batch2.push(place(
                s,
                order(
                    m,
                    is_buy,
                    fp(price),
                    FixedPoint::from_raw(FixedPoint::SCALE / 2),
                    TimeInForce::GTC,
                ),
            )),
            13 => batch2.push(place(
                s,
                order(
                    m,
                    is_buy,
                    FixedPoint::from_raw(price as i128 * FixedPoint::SCALE + 7),
                    qty,
                    TimeInForce::GTC,
                ),
            )),
            19 => {
                batch2.push(place(s, order(m, true, fp(100), qty, TimeInForce::GTC)));
                batch2.push(place(s, order(m, false, fp(100), qty, tif)));
            }
            _ => batch2.push(place(s, order(m, is_buy, fp(price), qty, tif))),
        }
    }

    let mut batch3 = Vec::new();
    for _ in 0..120 {
        let m = 1 + rng.below(12.min(n_markets));
        let s = sender(&mut rng);
        let is_buy = rng.below(2) == 0;
        batch3.push(place(
            s,
            order(
                m,
                is_buy,
                fp(96 + rng.below(9) as i64),
                fp(1 + rng.below(3) as i64),
                TimeInForce::GTC,
            ),
        ));
    }

    let mut batch4 = Vec::new();
    for _ in 0..10 {
        let s = sender(&mut rng);
        let is_buy = rng.below(2) == 0;
        batch4.push(place(
            s,
            order(
                3,
                is_buy,
                fp(99 + rng.below(3) as i64),
                fp(2),
                TimeInForce::GTC,
            ),
        ));
    }

    vec![batch1, batch2, batch3, batch4]
}

/// Every pass-A cap (1 = serial pass A, 2 and 3 = fewer chunks than active
/// markets, 32 = the archy production cap) x every grain (pinned 1, the
/// process env, pinned 2 / 4 / 16) must equal the sequential settle with the
/// grain pinned off.
fn assert_grain_invariant(seed: u64, n_markets: u64) {
    let batches = fuzz_batches(seed, n_markets);
    let golden = with_exec_pool_grain(1, || run_batches(&batches, false, None));
    assert!(
        golden.trade_index as u64 > 4 * n_markets,
        "scenario too weak: only {} trades",
        golden.trade_index
    );
    for cap in [1usize, 2, 3, 32] {
        let off = with_exec_pool_grain(1, || run_batches(&batches, true, Some(cap)));
        assert_eq!(golden, off, "seed {seed:#x} cap {cap}: K=1 != sequential");
        let env = run_batches(&batches, true, Some(cap));
        assert_eq!(
            golden, env,
            "seed {seed:#x} cap {cap}: process-env grain != K=1"
        );
        for k in [2usize, 4, 16] {
            let on = with_exec_pool_grain(k, || run_batches(&batches, true, Some(cap)));
            assert_eq!(golden, on, "seed {seed:#x} cap {cap}: K={k} != K=1");
        }
    }
}

#[test]
fn grain_settle_identical_across_caps_40_markets() {
    assert_grain_invariant(0x5EED_0058, 40);
}

#[test]
fn grain_settle_identical_across_caps_12_markets_second_seed() {
    assert_grain_invariant(0xDEAD_BEEF_0058, 12);
}

/// Scheduling nondeterminism hunt: at K=16 and a small cap the pool runs many
/// tiny jobs; repeated runs must all match K=1.
#[test]
fn grain_settle_repeated_runs_identical() {
    let batches = fuzz_batches(0x0058_0058, 40);
    let golden = with_exec_pool_grain(1, || run_batches(&batches, true, Some(2)));
    for run in 0..8 {
        let k = [4usize, 16][run % 2];
        let on = with_exec_pool_grain(k, || run_batches(&batches, true, Some(2)));
        assert_eq!(golden, on, "run {run} K={k} diverged");
    }
}

// ---- A planted pass-A panic at K > 1 ----

/// The armed `CF_NATIVE_POSITIONS` point read panics ONCE, and only inside a
/// recovering scope on an exec-pool thread: that is settle pass A's
/// `catch_recoverable` (matching uses plain `catch_unwind`, and the
/// sequential fallback runs on the calling thread). `hits` counts panics.
#[derive(Clone)]
struct PassAPanicOnce {
    inner: StateDb,
    armed: Arc<Mutex<Option<Vec<u8>>>>,
    hits: Arc<AtomicUsize>,
}

impl StateBackend for PassAPanicOnce {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_POSITIONS
            && torus_bridge::panic_scope::in_recoverable_scope()
            && std::thread::current()
                .name()
                .is_some_and(|n| n.starts_with("torus-exec-pool-"))
        {
            let mut armed = self.armed.lock().unwrap();
            if armed.as_deref() == Some(key) {
                *armed = None;
                drop(armed);
                self.hits.fetch_add(1, Ordering::SeqCst);
                panic!("planted pass-A panic (exec_pool_grain_tests)");
            }
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
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
        self.inner.atomic_write(ops)
    }
}

/// With 12 markets split into K x cap pass-A chunks, a panic in one fine
/// chunk is still contained: the block falls back to the sequential settle
/// once and its state equals the fault-free K = 1 run.
#[test]
fn grain_planted_pass_a_panic_falls_back_to_sequential() {
    let mut batches = fuzz_batches(0x5EED_FA11, 12);
    // The fresh taker's IOC buy opens batch 2 on market 7, so it crosses the
    // batch-1 asks (101..=103) before the storm touches them.
    batches[1].insert(
        0,
        place(
            addr(FRESH_TAKER),
            order(7, true, fp(104), fp(1), TimeInForce::IOC),
        ),
    );
    let golden = with_exec_pool_grain(1, || run_batches(&batches, true, Some(2)));
    for cap in [2usize, 3] {
        for k in [1usize, 4, 16] {
            let dir = tempfile::tempdir().expect("create temp dir");
            let db = StateDb::open(dir.path()).expect("open db");
            let backend = PassAPanicOnce {
                inner: db.clone(),
                armed: Arc::default(),
                hits: Arc::default(),
            };
            let (armed, hits) = (backend.armed.clone(), backend.hits.clone());
            let key = position_key(&addr(FRESH_TAKER), 7).to_vec();
            let (got, fallbacks) = with_exec_pool_grain(k, || {
                run_on(backend, &db, &batches, true, Some(cap), |i| {
                    if i == 1 {
                        *armed.lock().unwrap() = Some(key.clone());
                    }
                })
            });
            assert_eq!(
                hits.load(Ordering::SeqCst),
                1,
                "cap {cap} K={k}: the planted panic fired once"
            );
            assert_eq!(
                fallbacks, 1,
                "cap {cap} K={k}: pass A fell back to sequential once"
            );
            assert_eq!(
                golden, got,
                "cap {cap} K={k}: fallback state != fault-free run"
            );
        }
    }
}

// ---- Matching through the public capped entry point ----

/// Owned request params per market (borrowed by `MatchRequest`). Market `m`
/// gets `1 + (7m mod 11)` requests, so LPT weights differ across markets:
/// alternating crossing buys / sells, resting orders and an STP pair.
fn match_req_params(n_markets: u64) -> Vec<Vec<(Address, PlaceOrderParams)>> {
    (1..=n_markets)
        .map(|m| {
            let n = 1 + (7 * m % 11);
            (0..n)
                .map(|j| {
                    let s = addr((1 + (m + j) % 5) as u8);
                    let p = match j % 4 {
                        0 => order(m, true, fp(101), fp(2), TimeInForce::GTC),
                        1 => order(m, false, fp(99), fp(3), TimeInForce::IOC),
                        2 => order(m, true, fp(95), fp(1), TimeInForce::GTC),
                        _ => order(m, false, fp(105), fp(1), TimeInForce::PostOnly),
                    };
                    (s, p)
                })
                .collect()
        })
        .collect()
}

/// Fresh books (resting liquidity on both sides from `addr(200)`/`addr(201)`)
/// with each market's requests; order ids globally unique.
fn match_batches(
    rp: &[Vec<(Address, PlaceOrderParams)>],
) -> HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> {
    let mut batches = HashMap::default();
    for (idx, reqs) in rp.iter().enumerate() {
        let m = idx as MarketId + 1;
        let mut book = OrderBook::new(m, fp(1), fp(1));
        book.place_order(
            order(m, false, fp(100), fp(4), TimeInForce::GTC),
            addr(200),
            999,
        );
        book.place_order(
            order(m, false, fp(102), fp(4), TimeInForce::GTC),
            addr(200),
            999,
        );
        book.place_order(
            order(m, true, fp(98), fp(4), TimeInForce::GTC),
            addr(201),
            999,
        );
        let requests = reqs
            .iter()
            .enumerate()
            .map(|(j, (s, p))| MatchRequest {
                sender: *s,
                params: p,
                order_id: (m as OrderId) * 1000 + j as OrderId + 1,
                margin: None,
            })
            .collect();
        batches.insert(m, (book, requests));
    }
    batches
}

/// Everything a `MarketBatchResult` carries, as bytes / debug text: the
/// borsh-encoded book, next order id and every request's full `PlaceResult`.
fn match_digest(v: Vec<MarketBatchResult>) -> Vec<(MarketId, OrderId, Vec<u8>, Vec<String>)> {
    let mut v = v;
    v.sort_by_key(|r| r.market_id);
    v.into_iter()
        .map(|r| {
            (
                r.market_id,
                r.next_order_id,
                borsh::to_vec(&r.book).expect("borsh book"),
                r.results
                    .iter()
                    .map(|x| format!("{:?} {:?} {:?}", x.sender, x.order_id, x.result))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn grain_match_results_identical_after_sort() {
    let n = 24u64;
    let rp = match_req_params(n);
    let serial = with_exec_pool_grain(1, || {
        MarketWorkerPool::match_parallel_capped(match_batches(&rp), 1000, 1).expect("no panic")
    });
    let fills: usize = serial
        .iter()
        .flat_map(|r| r.results.iter())
        .map(|x| x.result.fills.len())
        .sum();
    assert!(fills > n as usize, "scenario too weak: {fills} fills");
    let golden = match_digest(serial);
    for cap in [1usize, 2, 3, 5, 32] {
        for k in [1usize, 4, 16] {
            let got = with_exec_pool_grain(k, || {
                match_digest(
                    MarketWorkerPool::match_parallel_capped(match_batches(&rp), 1000, cap)
                        .expect("no panic"),
                )
            });
            assert_eq!(golden, got, "cap {cap} K={k}: match results differ");
        }
        let env = match_digest(
            MarketWorkerPool::match_parallel_capped(match_batches(&rp), 1000, cap)
                .expect("no panic"),
        );
        assert_eq!(golden, env, "cap {cap}: process-env grain differs");
    }
}
