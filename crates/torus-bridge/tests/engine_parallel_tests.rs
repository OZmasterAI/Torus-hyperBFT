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

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
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
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=48u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
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

    RunFingerprint {
        cf_dump: state_dump(&ctx),
        results,
        total_gas,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
    }
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
