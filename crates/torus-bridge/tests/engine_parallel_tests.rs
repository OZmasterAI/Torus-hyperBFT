//! L3-ENG (perf/l3-engine-par): TORUS_PARALLEL_ENGINE differential tests.
//!
//! THE contract under test: `execute_batch_engine_mode(_, _, threads)` must
//! produce BYTE-IDENTICAL observable state for ANY thread count — balances,
//! positions, books, trade history, counters, per-action results (including
//! error strings), and the consensus-authoritative native state root. The
//! thread matrix {off, 2, 4, 8} is rerun >=20x to hunt schedule
//! nondeterminism, and named adversarial shapes pin the cross-market
//! shared-balance couplings the design isolates (Phase-2 per-sender folds,
//! Phase-4 canonical apply). See docs/design-parallel-engine.md.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_telemetry::Metrics;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers (same idiom as parallel_settle_tests.rs) ----

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
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

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

#[allow(clippy::too_many_arguments)]
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

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, fp(price), fp(qty), TimeInForce::GTC)
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// Full observable outcome of a run — byte-identical or bust.
#[derive(PartialEq, Eq, Debug)]
struct RunFingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<(bool, Option<String>)>>,
    total_gas: Vec<u64>,
    trade_index: u32,
    next_global_order_id: u128,
    state_root: B256,
}

fn state_dump_db(db: &StateDb) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let cfs = [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        // Node-local order-row store (mode-2 combo cell; empty otherwise).
        CF_BOOK_ORDER_ROWS,
    ];
    let mut out = Vec::new();
    for cf in cfs {
        let entries = db.iterate_cf(cf, None).expect("iterate cf");
        for (k, v) in entries {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

fn state_dump(ctx: &NativeExecContext) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    state_dump_db(&ctx.state)
}

/// Run `batches` through one context (fresh DB) at the given engine thread
/// count, then fingerprint the world.
fn run_batches(batches: &[Vec<(Address, NativeAction)>], threads: usize) -> RunFingerprint {
    run_with_volumes(batches, threads, &[]).0
}

/// `run_batches` with `cum_volume` rows preset before the first batch; also returns the
/// `orders_rejected_open_limit` funnel counter.
fn run_with_volumes(
    batches: &[Vec<(Address, NativeAction)>],
    threads: usize,
    volumes: &[(Address, i64)],
) -> (RunFingerprint, u64) {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let metrics = Arc::new(Metrics::new());
    ctx.metrics = Some(metrics.clone());
    for n in 1..=48u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    for (trader, volume) in volumes {
        ctx.positions.put_cum_volume(trader, fp(*volume)).unwrap();
    }

    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    for batch in batches {
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, threads);
        assert!(
            ctx.fatal_error.is_none(),
            "no batch here may go fatal: {:?}",
            ctx.fatal_error
        );
        results.push(
            r.results
                .iter()
                .map(|a| (a.success, a.error.clone()))
                .collect(),
        );
        total_gas.push(r.total_gas);
    }
    ctx.save_order_books();

    let run = RunFingerprint {
        cf_dump: state_dump(&ctx),
        results,
        total_gas,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
    };
    (run, metrics.orders_rejected_open_limit.get())
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

/// Heavy mixed-market scenario (same generator family as
/// parallel_settle_tests): 8 markets, 32 senders trading across markets
/// (cross-margin balance coupling), a seeded resting book, a ~320-order
/// crossing storm with GTC/IOC/FOK/PostOnly, sub-lot dust rejects, tick
/// rejects, STP self-trade cancels, and in-batch open->close PnL flows —
/// plus a single-market tail batch (degenerate settle path).
fn fuzz_batches(seed: u64) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let markets: [MarketId; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

    // Batch 1: resting liquidity — makers ladder both sides of 100.
    let mut batch1 = Vec::new();
    for &m in &markets {
        for lvl in 0..4i64 {
            let maker = addr((1 + rng.below(32)) as u8);
            batch1.push(place(maker, gtc(m, true, 97 + lvl, 1 + rng.below(4) as i64)));
            let maker2 = addr((1 + rng.below(32)) as u8);
            batch1.push(place(
                maker2,
                gtc(m, false, 101 + lvl, 1 + rng.below(4) as i64),
            ));
        }
    }

    // Batch 2: the storm — ~320 orders crossing hard across all markets.
    let mut batch2 = Vec::new();
    for i in 0..320u64 {
        let m = markets[rng.below(8) as usize];
        let sender = addr((1 + rng.below(32)) as u8);
        let is_buy = rng.below(2) == 0;
        let tif = match rng.below(10) {
            0 => TimeInForce::IOC,
            1 => TimeInForce::FOK,
            2 => TimeInForce::PostOnly,
            _ => TimeInForce::GTC,
        };
        let price = 96 + rng.below(9) as i64;
        let qty_raw = (1 + rng.below(5)) as i128 * FixedPoint::SCALE
            + (rng.below(FixedPoint::SCALE as u64 / 2)) as i128;
        let qty = FixedPoint::from_raw(qty_raw);

        match i % 29 {
            7 => batch2.push(place(
                sender,
                order(
                    m,
                    is_buy,
                    fp(price),
                    FixedPoint::from_raw(FixedPoint::SCALE / 2),
                    TimeInForce::GTC,
                ),
            )),
            13 => batch2.push(place(
                sender,
                order(
                    m,
                    is_buy,
                    FixedPoint::from_raw(price as i128 * FixedPoint::SCALE + 7),
                    qty,
                    TimeInForce::GTC,
                ),
            )),
            19 => {
                batch2.push(place(sender, order(m, true, fp(100), qty, TimeInForce::GTC)));
                batch2.push(place(sender, order(m, false, fp(100), qty, tif)));
            }
            _ => batch2.push(place(sender, order(m, is_buy, fp(price), qty, tif))),
        }
    }

    // Batch 3: single-market batch (degenerate parallel-settle case; Phase-2
    // sharding still active with many senders on one market).
    let mut batch3 = Vec::new();
    for _ in 0..24 {
        let sender = addr((1 + rng.below(32)) as u8);
        let is_buy = rng.below(2) == 0;
        batch3.push(place(sender, gtc(3, is_buy, 99 + rng.below(3) as i64, 2)));
    }

    vec![batch1, batch2, batch3]
}

// ============================================================================
// 1. Thread-count differential matrix: {off, 2, 4, 8}, >=20 reruns
// ============================================================================

#[test]
fn engine_thread_matrix_byte_identity_20x() {
    let batches = fuzz_batches(0x13EE_5EED);
    let golden = run_batches(&batches, 0);

    // Sanity: the scenario exercises fills, rejects, and multiple markets.
    assert!(
        golden.trade_index > 40,
        "scenario too weak: only {} trades",
        golden.trade_index
    );

    // >=20 reruns per parallel thread count — any schedule nondeterminism in
    // the sharded prepare or the settle pipeline shows up as a mismatch.
    for run in 0..20 {
        for threads in [2usize, 4, 8] {
            let par = run_batches(&batches, threads);
            assert_eq!(
                golden, par,
                "engine threads={threads} diverged from serial on run {run}"
            );
        }
    }
}

/// A second seed, fewer reps — different fill/reject mix, same contract.
#[test]
fn engine_thread_matrix_second_seed() {
    let batches = fuzz_batches(0x0DDB_A11_0042);
    let golden = run_batches(&batches, 0);
    assert!(golden.trade_index > 40, "scenario too weak");
    for run in 0..5 {
        for threads in [2usize, 4, 8] {
            let par = run_batches(&batches, threads);
            assert_eq!(
                golden, par,
                "engine threads={threads} diverged on run {run} (seed 2)"
            );
        }
    }
}

// ============================================================================
// 2. Named adversarial shapes
// ============================================================================

/// Mixed-market batch: many senders, many markets, heavy crossing — the
/// bread-and-butter cap-400 shape.
#[test]
fn mixed_market_multi_sender_identical() {
    let mut batch1 = Vec::new();
    let mut batch2 = Vec::new();
    for m in 1..=6u64 {
        for k in 0..4u8 {
            batch1.push(place(addr(1 + k), gtc(m, false, 100 + k as i64, 3)));
        }
        for k in 0..6u8 {
            batch2.push(place(addr(10 + k), gtc(m, true, 103, 2)));
        }
    }
    let batches = vec![batch1, batch2];
    let golden = run_batches(&batches, 0);
    assert!(golden.trade_index > 10, "must produce fills");
    for threads in [2usize, 4, 8] {
        assert_eq!(golden, run_batches(&batches, threads), "threads={threads}");
    }
}

/// Single-market degenerate case: every order in ONE market. Parallel settle
/// must skip (needs >=2 markets); Phase-2 sharding is still live across the
/// senders — outcome must be byte-identical anyway.
#[test]
fn single_market_degenerate_identical() {
    let mut batch1 = Vec::new();
    let mut batch2 = Vec::new();
    for k in 0..8u8 {
        batch1.push(place(addr(1 + k), gtc(1, false, 100 + (k % 3) as i64, 2)));
    }
    for k in 0..16u8 {
        batch2.push(place(addr(9 + k), gtc(1, true, 102, 1)));
    }
    let batches = vec![batch1, batch2];
    let golden = run_batches(&batches, 0);
    assert!(golden.trade_index > 4, "must produce fills");
    for threads in [2usize, 4, 8] {
        assert_eq!(golden, run_batches(&batches, threads), "threads={threads}");
    }
}

/// Cross-market same-trader: one trader takes on several markets in ONE
/// batch — every release/credit for that trader funnels through a single
/// NativeBalance row from different per-market plans; ordering is pinned by
/// pass B, and Phase-2 reserves are the trader's own serial fold.
#[test]
fn cross_market_same_trader_identical() {
    let a = addr(1);
    let batch1 = vec![
        place(addr(2), gtc(1, false, 100, 4)),
        place(addr(3), gtc(2, true, 200, 3)),
        place(addr(4), gtc(3, false, 50, 10)),
        place(a, gtc(4, true, 80, 5)), // A's far bid rests (margin held)
    ];
    // A trades all three markets in one batch: buy mkt1, sell mkt2 (opens
    // short), buy 6 of 10 on mkt3 (partial maker consumption).
    let batch2 = vec![
        place(a, gtc(1, true, 100, 4)),
        place(a, gtc(2, false, 200, 3)),
        place(a, gtc(3, true, 50, 6)),
    ];
    // A then closes mkt1 at a profit against a fresh bid — in-batch PnL.
    let batch3 = vec![
        place(addr(5), gtc(1, true, 110, 4)),
        place(a, gtc(1, false, 110, 4)),
    ];
    let batches = vec![batch1, batch2, batch3];
    let golden = run_batches(&batches, 0);
    assert!(golden.trade_index >= 4, "must produce fills");
    for threads in [2usize, 4, 8] {
        assert_eq!(golden, run_batches(&batches, threads), "threads={threads}");
    }
}

/// Margin exhaustion MID-BATCH across markets: a poorly funded sender whose
/// later orders (on OTHER markets) must fail with the exact serial error
/// strings, while global order-id numbering skips exactly the failed orders.
/// Interleaved with a rich sender so the flat order alternates senders —
/// the stitch (not the workers) owns id assignment.
#[test]
fn cross_market_margin_exhaustion_mid_batch_identical() {
    let poor = addr(40); // funded 30: first 20-reserve passes, rest fail
    let rich = addr(41);

    let mut batch1 = Vec::new();
    for m in 1..=4u64 {
        batch1.push(place(addr(2), gtc(m, false, 100, 8))); // resting asks
    }
    let mut batch2 = Vec::new();
    for m in 1..=4u64 {
        // poor: buy 4 @ 100 => reserve 20. Only the first fits (avail 30).
        batch2.push(place(poor, gtc(m, true, 99, 4)));
        // rich interleaved on the same markets, crossing the asks.
        batch2.push(place(rich, gtc(m, true, 100, 2)));
    }
    let batches = vec![batch1, batch2];

    let run = |threads: usize| -> RunFingerprint {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db);
        fund_native(&ctx, &addr(2), fp(1_000_000));
        fund_native(&ctx, &rich, fp(1_000_000));
        fund_native(&ctx, &poor, fp(30));
        let mut results = Vec::new();
        let mut total_gas = Vec::new();
        for batch in &batches {
            let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, threads);
            assert!(ctx.fatal_error.is_none());
            results.push(
                r.results
                    .iter()
                    .map(|a| (a.success, a.error.clone()))
                    .collect(),
            );
            total_gas.push(r.total_gas);
        }
        ctx.save_order_books();
        RunFingerprint {
            cf_dump: state_dump(&ctx),
            results,
            total_gas,
            trade_index: ctx.trade_index,
            next_global_order_id: ctx.next_global_order_id,
            state_root: compute_native_state_root(&ctx.state).expect("state root"),
        }
    };

    let golden = run(0);
    // The scenario really does exhaust mid-batch: poor's first order passes,
    // the remaining three fail on margin with the serial error string.
    let b2 = &golden.results[1];
    assert!(b2[0].0, "poor's first order must pass");
    for k in 1..4 {
        let (ok, err) = &b2[2 * k];
        assert!(!ok, "poor's order {k} must fail");
        assert!(
            err.as_deref().unwrap_or("").starts_with("insufficient margin"),
            "expected margin reject, got {err:?}"
        );
    }
    for threads in [2usize, 4, 8] {
        assert_eq!(golden, run(threads), "threads={threads}");
    }
}

// ============================================================================
// 3. Full-combo cell: LevelAuthority book mode + resident books + engine
// ============================================================================

/// The bench-standard flags that touch `execute_batch`/`save_order_books`
/// are TORUS_BOOK_ROWS=2 and TORUS_RESIDENT_BOOKS=1 (root-cache/bucket-hash/
/// member-cache act at flush time, after exec returns). Pin both explicitly
/// (constructor-pinned — no env races) across TWO blocks with resident
/// handoff, and require byte-identity of the full world including the
/// node-local order-row store.
#[test]
fn full_combo_level_authority_resident_identical() {
    let batches_b1 = fuzz_batches(0xC0DE_C0DE ^ 0x1357_9BDF);
    let run = |threads: usize| -> (Vec<(String, Vec<u8>, Vec<u8>)>, B256, u128, u32) {
        let (_dir, db) = open_test_db();
        let mut resident = ResidentBooks::default();
        // NB `trade_index` is PER-BLOCK (fresh context each height) — sum it
        // across blocks for the scenario-strength assertion.
        let mut trades_total = 0u32;
        let mut next_id_last = 0u128;
        // Fund via a throwaway ctx (positions live on the shared db).
        {
            let ctx = make_ctx(db.clone());
            for n in 1..=48u8 {
                fund_native(&ctx, &addr(n), fp(1_000_000));
            }
        }
        for (height, batch_set) in [(1u64, &batches_b1[..2]), (2u64, &batches_b1[2..])] {
            let mut ctx = NativeExecContext::new_with_mode(
                db.clone(),
                height,
                1000 + height,
                0,
                100,
                10,
                addr(99),
                addr(100),
                addr(101),
                BookMode::LevelAuthority,
                Some(&mut resident),
            );
            assert!(ctx.fatal_error.is_none(), "load fatal: {:?}", ctx.fatal_error);
            for batch in batch_set {
                NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, threads);
                assert!(ctx.fatal_error.is_none(), "fatal: {:?}", ctx.fatal_error);
            }
            ctx.save_order_books();
            ctx.stash_resident(&mut resident);
            trades_total += ctx.trade_index;
            next_id_last = ctx.next_global_order_id;
        }
        let root = compute_native_state_root(&db).expect("state root");
        (state_dump_db(&db), root, next_id_last, trades_total)
    };

    let golden = run(0);
    assert!(golden.3 > 40, "combo scenario too weak: {} trades", golden.3);
    for threads in [2usize, 4, 8] {
        let par = run(threads);
        assert_eq!(golden.1, par.1, "state root diverged (threads={threads})");
        assert_eq!(golden.0, par.0, "cf dump diverged (threads={threads})");
        assert_eq!(golden.2, par.2, "next order id diverged (threads={threads})");
        assert_eq!(golden.3, par.3, "trade index diverged (threads={threads})");
    }
}

// ============================================================================
// 4. Default env-driven path still matches forced-serial
// ============================================================================

#[test]
fn default_mode_matches_serial() {
    // Whatever TORUS_PARALLEL_ENGINE says in this environment (default OFF),
    // the plain execute_batch entry point must produce the same observable
    // outcome as the forced-serial engine mode (byte-identity holds even if
    // an env var IS set — that is the whole contract).
    let batches = fuzz_batches(0xFACE_0FF5);
    let golden = run_batches(&batches, 0);

    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=48u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    for batch in &batches {
        let r = NativeExecutor::execute_batch(&mut ctx, batch);
        results.push(
            r.results
                .iter()
                .map(|a| (a.success, a.error.clone()))
                .collect::<Vec<_>>(),
        );
        total_gas.push(r.total_gas);
    }
    ctx.save_order_books();

    assert_eq!(golden.cf_dump, state_dump(&ctx), "default-mode state diverged");
    assert_eq!(golden.results, results, "default-mode results diverged");
    assert_eq!(golden.total_gas, total_gas, "default-mode gas diverged");
    assert_eq!(golden.trade_index, ctx.trade_index);
    assert_eq!(golden.next_global_order_id, ctx.next_global_order_id);
}

// ============================================================================
// 5. Per-user open-order limit (Phase-2 prepare, serial + sharded)
// ============================================================================

/// `n` non-crossing GTC buys from `sender`, round-robin over markets 1..=5.
fn resting(sender: Address, n: usize) -> Vec<(Address, NativeAction)> {
    (0..n)
        .map(|k| {
            let price = 41 + (k / 5 % 10) as i64;
            place(sender, gtc(1 + (k % 5) as u64, true, price, 1))
        })
        .collect()
}

/// Round-robin merge, so the flat order alternates senders.
fn interleave(lists: Vec<Vec<(Address, NativeAction)>>) -> Vec<(Address, NativeAction)> {
    let mut iters: Vec<_> = lists.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::new();
    loop {
        let before = out.len();
        out.extend(iters.iter_mut().filter_map(Iterator::next));
        if out.len() == before {
            return out;
        }
    }
}

fn market_buy(market_id: MarketId, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        price: FixedPoint::ZERO,
        order_type: OrderType::Market,
        ..order(market_id, true, FixedPoint::ZERO, fp(qty), TimeInForce::IOC)
    }
}

fn is_open_limit(r: &(bool, Option<String>)) -> bool {
    !r.0 && r.1.as_deref().is_some_and(|e| e.contains("open order limit"))
}

#[test]
fn open_limit_rejects_the_1001st_restable_order_across_markets() {
    let a = addr(1);
    let mut block1 = resting(a, 1000);
    block1.push(place(addr(2), gtc(1, false, 60, 5)));
    let block2 = vec![
        place(a, gtc(6, true, 50, 1)),
        place(a, order(1, true, fp(40), fp(1), TimeInForce::IOC)),
        place(a, market_buy(1, 1)),
    ];
    let (run, rejected) = run_with_volumes(&[block1, block2], 0, &[]);
    assert!(run.results[0].iter().all(|r| r.0), "all 1000 must rest");
    let b2 = &run.results[1];
    assert!(is_open_limit(&b2[0]), "1001st: {:?}", b2[0]);
    assert!(!is_open_limit(&b2[1]), "IOC is exempt: {:?}", b2[1]);
    assert!(b2[2].0, "market order is exempt: {:?}", b2[2]);
    assert!(run.trade_index >= 1, "the market order filled");
    assert_eq!(rejected, 1);
}

#[test]
fn open_limit_counts_this_blocks_accepted_orders() {
    let d = addr(4);
    let batch = vec![gtc(6, true, 50, 1), gtc(6, true, 49, 1), gtc(1, true, 48, 1)];
    let blocks = vec![
        resting(d, 999),
        vec![(d, NativeAction::PlaceOrderBatch(batch))],
    ];
    let (run, rejected) = run_with_volumes(&blocks, 0, &[]);
    assert!(run.results[0].iter().all(|r| r.0));
    let b2 = &run.results[1];
    assert!(b2[0].0, "{:?}", b2[0]);
    assert!(is_open_limit(&b2[1]) && is_open_limit(&b2[2]), "{b2:?}");
    assert_eq!(rejected, 2);
}

#[test]
fn open_limit_grows_with_block_start_cum_volume() {
    let (c, e) = (addr(3), addr(5));
    let blocks = vec![interleave(vec![resting(c, 1002), resting(e, 1001)])];
    let volumes = [(c, 5_000_000), (e, 4_999_999)];
    let (run, rejected) = run_with_volumes(&blocks, 0, &volumes);
    let rejects: Vec<_> = run.results[0]
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.0)
        .map(|(i, r)| (i, is_open_limit(r)))
        .collect();
    // Flat order: c,e,c,e,...,c (2003 entries); c's 1002nd = index 2002,
    // e's 1001st = index 2001.
    assert_eq!(rejects, vec![(2001, true), (2002, true)]);
    assert_eq!(rejected, 2);
}

/// Serial and sharded Phase-2 prepare (and sequential / parallel settle)
/// must agree byte for byte with the limit active for several senders.
#[test]
fn open_limit_serial_and_sharded_prepare_identical() {
    let (a, c, d) = (addr(1), addr(3), addr(4));
    let mut liquidity = Vec::new();
    for m in 1..=5u64 {
        liquidity.push(place(addr(2), gtc(m, false, 60, 5)));
    }
    let block1 = interleave(vec![
        resting(a, 1000),
        resting(c, 1002),
        resting(d, 999),
        liquidity,
    ]);
    let mut block2 = vec![
        place(a, gtc(6, true, 50, 1)),
        (
            d,
            NativeAction::PlaceOrderBatch(vec![gtc(6, true, 50, 1), gtc(2, true, 49, 1)]),
        ),
        place(a, market_buy(2, 1)),
        place(c, gtc(3, true, 45, 1)),
        place(d, market_buy(3, 1)),
    ];
    for k in 0..8u8 {
        block2.push(place(addr(10 + k), gtc(1 + k as u64 % 5, true, 60, 1)));
    }
    let blocks = vec![block1, block2];
    let volumes = [(c, 5_000_000)];
    let golden = run_with_volumes(&blocks, 0, &volumes);
    // c's 1002nd (block 1); a's GTC, d's second batch order and c's GTC
    // (c is at its 1001 limit) in block 2.
    assert_eq!(golden.1, 4);
    assert!(golden.0.trade_index >= 2);
    for threads in [2usize, 4, 8] {
        assert_eq!(golden, run_with_volumes(&blocks, threads, &volumes), "threads={threads}");
    }
}

/// HL rule: at >= 1000 open orders, reduce-only and trigger orders are
/// rejected even when the volume-scaled limit (here 1200) has room.
#[test]
fn open_limit_reduce_only_and_stops_rejected_at_1000_open() {
    let (a, b) = (addr(1), addr(6));
    let stop = PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(200) },
        ..gtc(1, true, 0, 1)
    };
    let reduce_only = PlaceOrderParams {
        reduce_only: true,
        ..gtc(2, true, 45, 1)
    };
    let reduce_only_ioc = PlaceOrderParams {
        reduce_only: true,
        ..order(1, true, fp(40), fp(1), TimeInForce::IOC)
    };
    let blocks = vec![
        interleave(vec![resting(a, 1000), resting(b, 999)]),
        vec![
            place(a, reduce_only.clone()),
            place(a, stop.clone()),
            place(a, gtc(3, true, 45, 1)),
            place(a, reduce_only_ioc),
            place(b, stop),
            place(b, reduce_only),
        ],
    ];
    let volumes = [(a, 1_000_000_000), (b, 1_000_000_000)];
    let golden = run_with_volumes(&blocks, 0, &volumes);
    let r = &golden.0.results[1];
    assert!(is_open_limit(&r[0]), "reduce-only GTC: {:?}", r[0]);
    assert!(is_open_limit(&r[1]), "stop: {:?}", r[1]);
    assert!(r[2].0, "plain GTC under the 1200 limit: {:?}", r[2]);
    assert!(!is_open_limit(&r[3]), "reduce-only IOC is exempt: {:?}", r[3]);
    assert!(r[4].0, "stop at 999 open: {:?}", r[4]);
    assert!(is_open_limit(&r[5]), "reduce-only once the stop made 1000: {:?}", r[5]);
    assert_eq!(golden.1, 3);
    for threads in [2usize, 4] {
        assert_eq!(golden, run_with_volumes(&blocks, threads, &volumes), "threads={threads}");
    }
}

/// Pending stops in the books at batch start hold slots: 998 resting + 2
/// stops = 1000 open, so the next GTC is rejected; a cancel-all frees all.
#[test]
fn open_limit_counts_pending_stops_at_block_start() {
    let (a, b) = (addr(1), addr(6));
    let stop = |trigger: i64| PlaceOrderParams {
        order_type: OrderType::StopMarket {
            trigger: fp(trigger),
        },
        ..gtc(2, true, 0, 1)
    };
    let mut block1 = interleave(vec![resting(a, 998), resting(b, 10)]);
    block1.push(place(a, stop(200)));
    block1.push(place(a, stop(210)));
    let blocks = vec![
        block1,
        vec![place(a, gtc(6, true, 50, 1)), place(b, gtc(6, true, 50, 1))],
        vec![
            (a, NativeAction::CancelAllOrders { market_id: None }),
            place(a, gtc(6, true, 50, 1)),
            place(b, gtc(6, true, 49, 1)),
        ],
    ];
    let golden = run_with_volumes(&blocks, 0, &[]);
    assert!(golden.0.results[0].iter().all(|r| r.0));
    assert!(is_open_limit(&golden.0.results[1][0]), "{:?}", golden.0.results[1]);
    assert!(golden.0.results[1][1].0);
    assert!(golden.0.results[2].iter().all(|r| r.0), "{:?}", golden.0.results[2]);
    assert_eq!(golden.1, 1);
    for threads in [2usize, 4] {
        assert_eq!(golden, run_with_volumes(&blocks, threads, &[]), "threads={threads}");
    }
}

// ============================================================================
// 6. cum_volume: maker and taker add price*qty on every fill
// ============================================================================

/// Runs `blocks` at `threads` (0 = serial prepare + sequential settle, >=2 =
/// sharded prepare + parallel settle). `before_last` runs ahead of the last
/// block. Returns cum_volume of addr(1..=5), every result, the state dump
/// and the native root.
#[allow(clippy::type_complexity)]
fn run_volumes(
    blocks: &[Vec<(Address, NativeAction)>],
    threads: usize,
    before_last: impl Fn(&NativeExecContext),
) -> (
    Vec<FixedPoint>,
    Vec<Vec<(bool, Option<String>)>>,
    Vec<(String, Vec<u8>, Vec<u8>)>,
    B256,
) {
    run_volumes_counted(blocks, threads, before_last).0
}

/// `run_volumes` plus the number of parallel settles that fell back to the
/// sequential loop.
#[allow(clippy::type_complexity)]
fn run_volumes_counted(
    blocks: &[Vec<(Address, NativeAction)>],
    threads: usize,
    before_last: impl Fn(&NativeExecContext),
) -> (
    (
        Vec<FixedPoint>,
        Vec<Vec<(bool, Option<String>)>>,
        Vec<(String, Vec<u8>, Vec<u8>)>,
        B256,
    ),
    u64,
) {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=5u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    ctx.positions.put_cum_volume(&addr(1), fp(5_000_000)).unwrap();
    let mut results = Vec::new();
    for (k, block) in blocks.iter().enumerate() {
        if k + 1 == blocks.len() {
            before_last(&ctx);
        }
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, block, threads);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        results.push(r.results.iter().map(|a| (a.success, a.error.clone())).collect());
    }
    ctx.save_order_books();
    let volumes = (1..=5u8)
        .map(|n| ctx.positions.get_cum_volume(&addr(n)).unwrap())
        .collect();
    let root = compute_native_state_root(&ctx.state).expect("state root");
    let fallbacks = ctx.phase_accum.settle_fallbacks;
    ((volumes, results, state_dump(&ctx), root), fallbacks)
}

#[test]
fn cum_volume_adds_price_times_qty_for_maker_and_taker() {
    let (maker, taker) = (addr(1), addr(2));
    let blocks = vec![
        vec![
            place(maker, gtc(1, false, 30_000, 2)),
            place(addr(3), gtc(2, false, 100, 1)),
        ],
        vec![
            place(taker, gtc(1, true, 30_000, 2)),
            place(addr(4), gtc(2, true, 100, 1)),
        ],
    ];
    let golden = run_volumes(&blocks, 0, |_| {});
    // maker started at 5M; one fill 2 @ 30000 adds 60000 to both sides.
    assert_eq!(
        golden.0,
        vec![fp(5_060_000), fp(60_000), fp(100), fp(100), FixedPoint::ZERO]
    );
    assert!(golden.2.iter().any(|(_, k, _)| k.starts_with(b"cvlm")));
    for threads in [2usize, 4] {
        assert_eq!(golden, run_volumes(&blocks, threads, |_| {}), "threads={threads}");
    }
}

/// A fill-application failure stops the order where the position effects
/// stop: the taker side of the failing fill counts, its maker side does not.
#[test]
fn cum_volume_stops_at_the_failed_fill_like_positions() {
    let (m1, m2, taker) = (addr(1), addr(2), addr(3));
    let blocks = vec![
        vec![
            place(m1, gtc(1, false, 100, 1)),
            place(m2, gtc(1, false, 101, 1)),
            place(addr(4), gtc(2, false, 100, 1)),
        ],
        vec![
            place(taker, gtc(1, true, 101, 2)),
            place(addr(5), gtc(2, true, 100, 1)),
        ],
    ];
    // m2's position row is unreadable, so the second fill's maker side fails.
    let corrupt = |ctx: &NativeExecContext| {
        let key = torus_core::position::position_key(&m2, 1);
        ctx.state.put_cf_raw(CF_NATIVE_POSITIONS, &key, b"\xff").unwrap();
    };
    let golden = run_volumes(&blocks, 0, corrupt);
    let failed = &golden.1[1][0];
    assert!(
        failed.1.as_deref().is_some_and(|e| e.starts_with("maker fill failed")),
        "{failed:?}"
    );
    assert_eq!(
        golden.0,
        vec![fp(5_000_100), FixedPoint::ZERO, fp(201), fp(100), fp(100)]
    );
    for threads in [2usize, 4] {
        assert_eq!(golden, run_volumes(&blocks, threads, corrupt), "threads={threads}");
    }
}

/// A BALANCE-row read failure on a maker's realized-PnL credit fails the
/// order in parallel pass B itself (no sequential fallback): volume stops at
/// the failing side exactly as in the sequential loop.
#[test]
fn cum_volume_pass_b_balance_failure_stops_at_the_failed_side() {
    let (m, t1, t2) = (addr(1), addr(2), addr(3));
    let blocks = vec![
        vec![
            place(m, gtc(1, true, 100, 1)),
            place(addr(4), gtc(2, false, 100, 1)),
        ],
        // m goes long 1 @ 100.
        vec![
            place(t1, gtc(1, false, 100, 1)),
            place(addr(5), gtc(2, true, 100, 1)),
        ],
        // m's closing ask rests; m sends nothing in the last block, so its
        // balance is never loaded by Phase 2 there.
        vec![place(m, gtc(1, false, 101, 1))],
        vec![
            place(t2, gtc(1, true, 101, 1)),
            place(addr(5), gtc(2, false, 99, 1)),
        ],
    ];
    // m's fill closes its long (closed_pnl = 1): crediting it reads m's
    // balance row, which is unreadable.
    let corrupt = |ctx: &NativeExecContext| {
        ctx.state.put_cf_raw(CF_NATIVE_BALANCES, m.as_slice(), b"\xff").unwrap();
    };
    let (golden, serial_fallbacks) = run_volumes_counted(&blocks, 0, corrupt);
    assert_eq!(serial_fallbacks, 0);
    let failed = &golden.1[3][0];
    assert!(
        failed.1.as_deref().is_some_and(|e| e.starts_with("maker fill failed")),
        "{failed:?}"
    );
    // t2's taker side counts, m's maker side of that fill does not.
    assert_eq!(
        golden.0,
        vec![fp(5_000_100), fp(100), fp(101), fp(100), fp(100)]
    );
    for threads in [2usize, 4] {
        let (run, fallbacks) = run_volumes_counted(&blocks, threads, corrupt);
        assert_eq!(fallbacks, 0, "threads={threads}: pass B must handle it");
        assert_eq!(golden, run, "threads={threads}");
    }
}

/// The single-action path (`execute`, used by the CoreWriter drain) applies
/// the same limit and volume rules as `execute_batch`.
#[test]
fn open_limit_and_cum_volume_on_the_single_action_path() {
    let (a, b) = (addr(1), addr(2));
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    fund_native(&ctx, &a, fp(1_000_000));
    fund_native(&ctx, &b, fp(1_000_000));
    let mut block = resting(a, 1000);
    block.push(place(b, gtc(1, false, 60, 2)));
    NativeExecutor::execute_batch_engine_mode(&mut ctx, &block, 0);

    let gtc_6 = NativeAction::PlaceOrder(gtc(6, true, 50, 1));
    let r = NativeExecutor::execute(&mut ctx, &a, &gtc_6);
    assert!(is_open_limit(&(r.success, r.error.clone())), "{r:?}");
    let buy = NativeAction::PlaceOrder(market_buy(1, 2));
    let r = NativeExecutor::execute(&mut ctx, &a, &buy);
    assert!(r.success, "{r:?}");
    assert_eq!(ctx.positions.get_cum_volume(&a).unwrap(), fp(120));
    assert_eq!(ctx.positions.get_cum_volume(&b).unwrap(), fp(120));
}

/// A block runs `execute_batch` twice (app.rs: pre-EVM, then post-EVM). The
/// count and the limit are taken at the start of EACH batch, so the second
/// batch sees the first one's resting orders and its fills' cum_volume.
#[test]
fn open_limit_is_taken_at_each_execute_batch_start() {
    let (a, b, c) = (addr(1), addr(6), addr(7));
    let mut pre_evm = interleave(vec![resting(a, 1000), resting(b, 1000)]);
    // b sells 100 @ 50000 into c's bid (IOC: exempt) = 5M of volume for b.
    pre_evm.push(place(c, gtc(7, true, 50_000, 100)));
    pre_evm.push(place(b, order(7, false, fp(50_000), fp(100), TimeInForce::IOC)));
    let post_evm = vec![
        place(a, gtc(6, true, 50, 1)),
        place(b, gtc(6, true, 50, 1)),
        place(b, gtc(6, true, 49, 1)),
    ];
    let batches = vec![pre_evm, post_evm];
    let golden = run_with_volumes(&batches, 0, &[]);
    assert!(golden.0.results[0].iter().all(|r| r.0));
    let r = &golden.0.results[1];
    assert!(is_open_limit(&r[0]), "a: 1000 resting from the first batch: {:?}", r[0]);
    assert!(r[1].0, "b: limit 1001 from the first batch's fill: {:?}", r[1]);
    assert!(is_open_limit(&r[2]), "b at 1001: {:?}", r[2]);
    assert_eq!(golden.1, 2);
    for threads in [2usize, 4] {
        assert_eq!(golden, run_with_volumes(&batches, threads, &[]), "threads={threads}");
    }
}
