//! s63 `TORUS_CANCEL_BATCH` executor differential: the batched Phase-1
//! cancel-all runs must produce byte-identical results, gas, persisted state
//! and native state root to the per-action loop.

use super::*;
use crate::state_root::compute_native_state_root;
use alloy_primitives::U256;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_BBO, CF_NATIVE_MARKETS,
    CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

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

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

fn cancel_all(market_id: Option<MarketId>) -> NativeAction {
    NativeAction::CancelAllOrders { market_id }
}

const MARKETS: [MarketId; 4] = [1, 2, 3, 4];
const SENDERS: u8 = 40;

/// Resting depth like the s63 profile: two deep levels per side per market,
/// senders' orders interleaved through each FIFO queue.
fn setup_blocks() -> Vec<Vec<(Address, NativeAction)>> {
    let mut block = Vec::new();
    for round in 0..12i64 {
        for s in 1..=SENDERS {
            for &m in &MARKETS {
                let is_buy = (s as u64 + m) % 2 == 0;
                let price = if is_buy {
                    99 + round % 2
                } else {
                    110 + round % 2
                };
                block.push((addr(s), gtc(m, is_buy, price, 1 + (round % 3))));
            }
        }
    }
    vec![block]
}

/// Pre-testnet P0: a market with no `CF_NATIVE_MARKETS` row (the fixtures
/// list 1..=1024): its cancel-all is rejected (`UnknownMarket`) and is not
/// part of a run.
const UNKNOWN_MARKET: MarketId = 9_999;

/// Mixed blocks: runs of cancel-alls (None / Some(m) / unknown market /
/// duplicates / orderless senders) interleaved with places (which must not
/// break a run) and broken by CancelOrder, ModifyOrder and a transfer.
fn mixed_blocks(seed: u64, resting: &[(Address, u128)]) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let mut blocks = Vec::new();
    for _ in 0..3 {
        let mut block = Vec::new();
        for _ in 0..160 {
            let mut s = addr(1 + rng.below(SENDERS as u64 + 4) as u8);
            let m = MARKETS[rng.below(4) as usize];
            // CancelOrder always, ModifyOrder half the time, targets a real
            // setup (owner, id) pair — which misses once an earlier cancel
            // removed it; the rest are random ids (not found / wrong owner).
            let (owner, id) = resting[rng.below(resting.len() as u64) as usize];
            let owned = rng.below(2) == 0;
            let order_id = if owned {
                id
            } else {
                1 + rng.below(2_500) as u128
            };
            let action = match rng.below(20) {
                0..=3 => cancel_all(None),
                4 => cancel_all(Some(m)),
                5 => cancel_all(Some(UNKNOWN_MARKET)),
                6 => {
                    s = owner;
                    NativeAction::CancelOrder { order_id: id }
                }
                7 => {
                    if owned {
                        s = owner;
                    }
                    NativeAction::ModifyOrder {
                        order_id,
                        new_price: None,
                        new_qty: Some(fp(1)),
                    }
                }
                8 if rng.below(4) == 0 => NativeAction::TransferToSpot {
                    amount: U256::from(1_000u64),
                },
                // Crossing places: fills against the deep levels.
                9 => {
                    let is_buy = rng.below(2) == 0;
                    gtc(m, is_buy, if is_buy { 111 } else { 99 }, 2)
                }
                _ => {
                    let is_buy = rng.below(2) == 0;
                    let price = if is_buy {
                        99 + rng.below(2)
                    } else {
                        110 + rng.below(2)
                    };
                    gtc(m, is_buy, price as i64, 1 + rng.below(3) as i64)
                }
            };
            block.push((s, action));
            // Occasionally repeat the same sender's cancel-all right away.
            if rng.below(15) == 0 {
                block.push((s, cancel_all(None)));
            }
        }
        blocks.push(block);
    }
    blocks
}

#[derive(PartialEq, Eq, Debug)]
struct RunFingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<(&'static str, bool, Option<String>, u64)>>,
    total_gas: Vec<u64>,
    dirty: Vec<Vec<MarketId>>,
    books: Vec<(MarketId, Vec<u8>)>,
    pending_stops: usize,
    trade_index: u32,
    next_global_order_id: u128,
    state_root: B256,
}

/// A resident context (P2-1's cancel-all index is used only with resident
/// books, row 23). The holder is not kept: the context carries the books
/// and the index across the test's blocks itself.
fn new_ctx(dir: &tempfile::TempDir, mode: BookMode) -> NativeExecContext {
    new_ctx_with(dir, mode, true)
}

/// `resident: false`: the off path (`TORUS_RESIDENT_BOOKS=0`), every
/// cancel-all scans every book (row 23).
fn new_ctx_with(dir: &tempfile::TempDir, mode: BookMode, resident: bool) -> NativeExecContext {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    let db = StateDb::open(dir.path()).expect("open db");
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=1024u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut holder = ResidentBooks::default();
    let ctx = NativeExecContext::new_with_mode(
        db,
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        resident.then_some(&mut holder),
    );
    for s in 1..=SENDERS + 4 {
        let bal = NativeBalance {
            available: fp(10_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(s), &bal).unwrap();
    }
    ctx
}

/// `(owner, id)` of every order resting after the setup block.
fn resting_after_setup() -> Vec<(Address, u128)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx(&dir, BookMode::Classic);
    NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup_blocks()[0], false);
    let mut out = Vec::new();
    for id in 0..ctx.next_global_order_id {
        for m in MARKETS {
            if let Some(order) = ctx.order_books.get(&m).and_then(|b| b.get_order(id)) {
                out.push((order.trader, id));
            }
        }
    }
    assert!(out.len() > 1_000);
    out
}

fn run(blocks: &[Vec<(Address, NativeAction)>], mode: BookMode, batch: bool) -> RunFingerprint {
    run_with(blocks, mode, batch, false)
}

/// `full_scan`: the step 0.4 reference cancel-all (`test_cancel_all_full_scan`).
fn run_with(
    blocks: &[Vec<(Address, NativeAction)>],
    mode: BookMode,
    batch: bool,
    full_scan: bool,
) -> RunFingerprint {
    run_observed(blocks, mode, batch, full_scan, &mut |_| {})
}

/// [`run_with`], calling `observe` with the context after every block
/// (before its books are saved).
fn run_observed(
    blocks: &[Vec<(Address, NativeAction)>],
    mode: BookMode,
    batch: bool,
    full_scan: bool,
    observe: &mut dyn FnMut(&NativeExecContext),
) -> RunFingerprint {
    run_observed_in(blocks, mode, batch, full_scan, true, observe)
}

/// [`run_observed`] with `resident` books or without (row 23).
fn run_observed_in(
    blocks: &[Vec<(Address, NativeAction)>],
    mode: BookMode,
    batch: bool,
    full_scan: bool,
    resident: bool,
    observe: &mut dyn FnMut(&NativeExecContext),
) -> RunFingerprint {
    run_observed_par(blocks, mode, batch, full_scan, resident, None, observe)
}

/// [`run_observed_in`] with the parallel cancel-all book pass pinned
/// (`TORUS_PARALLEL_CANCEL_ALL`): `par = Some((workers, min_visits))` sets
/// the context's `cancel_all_workers` / `cancel_all_min_visits` (`None`:
/// the env defaults, i.e. what every other test runs).
fn run_observed_par(
    blocks: &[Vec<(Address, NativeAction)>],
    mode: BookMode,
    batch: bool,
    full_scan: bool,
    resident: bool,
    par: Option<(usize, usize)>,
    observe: &mut dyn FnMut(&NativeExecContext),
) -> RunFingerprint {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx_with(&dir, mode, resident);
    ctx.test_cancel_all_full_scan = full_scan;
    if let Some((workers, min_visits)) = par {
        ctx.cancel_all_workers = workers;
        ctx.cancel_all_min_visits = min_visits;
    }
    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    let mut dirty = Vec::new();
    for (b, block) in blocks.iter().enumerate() {
        let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, block, batch);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        // P2-1: the index (once built) covers the books after every block.
        assert_index_covers_books(&ctx, &format!("{mode:?} batch={batch} block {b}"));
        observe(&ctx);
        results.push(
            r.results
                .iter()
                .map(|a| {
                    (
                        a.action_type,
                        a.success,
                        a.error.as_deref().map(str::to_string),
                        a.gas_used,
                    )
                })
                .collect(),
        );
        total_gas.push(r.total_gas);
        let mut marked: Vec<_> = ctx.dirty_books.iter().copied().collect();
        marked.sort_unstable();
        dirty.push(marked);
        ctx.save_order_books();
        // Production builds a fresh context (empty dirty set) per block.
        ctx.dirty_books.clear();
    }
    let mut books: Vec<_> = ctx
        .order_books
        .iter()
        .map(|(&m, b)| (m, borsh::to_vec(b).unwrap()))
        .collect();
    books.sort_unstable();
    let pending_stops = ctx
        .order_books
        .values()
        .map(|b| b.pending_stop_count())
        .sum();
    let mut cf_dump = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        CF_BOOK_ORDER_ROWS,
        // Item 7 step 2 B16: the bbo row of every dirty market.
        CF_NATIVE_BBO,
    ] {
        for (k, v) in ctx.state.iterate_cf(cf, None).expect("iterate cf") {
            cf_dump.push((cf.to_string(), k, v));
        }
    }
    RunFingerprint {
        cf_dump,
        results,
        total_gas,
        dirty,
        books,
        pending_stops,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
    }
}

#[test]
fn cancel_batch_flag_on_matches_off_mixed_blocks() {
    let resting = resting_after_setup();
    for (seed, mode) in [
        (1u64, BookMode::Classic),
        (2, BookMode::LevelAuthority),
        (3, BookMode::LevelAuthorityChunked),
        (4, BookMode::Classic),
    ] {
        let mut blocks = setup_blocks();
        blocks.extend(mixed_blocks(seed, &resting));
        let off = run(&blocks, mode, false);
        let on = run(&blocks, mode, true);
        // Sanity: the scenario cancels real depth, fills, and has both
        // successful and failing run-breaking actions.
        let flat: Vec<_> = off.results.iter().flatten().collect();
        assert!(flat.iter().filter(|r| r.0 == "cancel_all").count() > 60);
        assert!(flat.iter().any(|r| r.0 == "cancel_order" && r.1));
        assert!(flat.iter().any(|r| r.0 == "cancel_order" && !r.1));
        assert!(flat.iter().any(|r| r.0 == "modify_order" && r.1));
        assert!(off.trade_index > 10, "scenario must fill: {} fills", off.trade_index);
        assert_eq!(off, on, "TORUS_CANCEL_BATCH diverged (seed {seed})");
    }
}

/// The setup, then one long run: every sender cancels everything, twice,
/// with places mixed in — books empty out, then the run's places rest on
/// fresh levels.
fn long_emptying_run_blocks() -> Vec<Vec<(Address, NativeAction)>> {
    let mut blocks = setup_blocks();
    let mut block = Vec::new();
    for s in (1..=SENDERS + 2).rev() {
        block.push((addr(s), cancel_all(None)));
        if s % 7 == 0 {
            block.push((addr(s), gtc(2, true, 98, 1)));
        }
    }
    for s in 1..=SENDERS {
        block.push((addr(s), cancel_all(Some(MARKETS[s as usize % 4]))));
    }
    blocks.push(block);
    blocks
}

/// One long run: every sender cancels everything, twice, with places mixed
/// in — books empty out, then the run's places rest on fresh levels.
#[test]
fn cancel_batch_flag_on_matches_off_long_run_emptying_books() {
    let blocks = long_emptying_run_blocks();
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let off = run(&blocks, mode, false);
        let on = run(&blocks, mode, true);
        assert_eq!(off, on);
    }
}

/// A cancel-all from a sender with only pending stops removes the stops and
/// marks the book dirty, so the removal is persisted (a reload has no stop).
#[test]
fn cancel_all_of_stop_only_senders_persists_stop_removal() {
    let stop = |trigger: i64| {
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            // s515: a stop-market needs a positive price cap.
            price: fp(trigger + 10),
            quantity: fp(1),
            order_type: OrderType::StopMarket {
                trigger: fp(trigger),
            },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        })
    };
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
        BookMode::LevelAuthorityChunked,
    ] {
        for batch in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut ctx = new_ctx(&dir, mode);
            let setup = vec![
                (addr(1), stop(200)),
                (addr(1), stop(210)),
                (addr(2), stop(220)),
                (addr(3), gtc(1, true, 99, 1)),
            ];
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, batch);
            ctx.save_order_books();
            ctx.dirty_books.clear();
            assert_eq!(ctx.order_books[&1].open_order_count(&addr(1)), 2);

            let block = vec![
                (addr(1), cancel_all(None)),
                (addr(2), cancel_all(Some(1))),
            ];
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, batch);
            assert!(ctx.dirty_books.contains(&1), "{mode:?} batch={batch}");
            assert_eq!(ctx.order_books[&1].pending_stop_count(), 0);
            ctx.save_order_books();

            let reloaded = NativeExecContext::new_with_mode(
                ctx.state.clone(),
                3,
                1000,
                0,
                100,
                10,
                addr(99),
                addr(100),
                addr(101),
                mode,
                None,
            );
            let book = &reloaded.order_books[&1];
            assert_eq!(book.pending_stop_count(), 0, "{mode:?} batch={batch}");
            assert_eq!(book.open_order_count(&addr(3)), 1);
        }
    }
}

fn stop_buy(market_id: MarketId, trigger: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: true,
        // s515: a stop-market needs a positive price cap.
        price: fp(trigger + 10),
        quantity: fp(1),
        order_type: OrderType::StopMarket {
            trigger: fp(trigger),
        },
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// Four books; addr(1) rests in markets 1 and 2, addr(2) has only a stop in
/// market 3, addr(3) has nothing; addr(4) rests in every market.
fn counter_setup(mode: BookMode, batch: bool) -> (tempfile::TempDir, NativeExecContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx(&dir, mode);
    let mut setup = vec![
        (addr(1), gtc(1, true, 99, 1)),
        (addr(1), gtc(2, false, 110, 1)),
        (addr(2), stop_buy(3, 200)),
    ];
    for m in MARKETS {
        setup.push((addr(4), gtc(m, true, 98, 1)));
    }
    NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, batch);
    ctx.save_order_books();
    ctx.dirty_books.clear();
    ctx.phase_accum = ExecPhaseAccum::default();
    (dir, ctx)
}

/// Item 6 Phase 2 step 0.2 (P2-1): every cancel-all counts the books it
/// visited and the books where its sender had orders or stops, the same on
/// the batched run and the per-action path. P2-1: a cancel-all visits only
/// the books its sender's index entry lists (was every book: 4 + 4 + 1 + 4
/// = 13), and a visit prunes them, so the repeat visits none.
#[test]
fn cancel_all_counters_count_visited_and_hit_books() {
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        for batch in [false, true] {
            let (_dir, mut ctx) = counter_setup(mode, batch);
            let block = vec![
                (addr(1), cancel_all(None)),
                (addr(2), cancel_all(None)),
                (addr(3), cancel_all(Some(4))),
                (addr(1), cancel_all(None)),
                (addr(3), cancel_all(Some(UNKNOWN_MARKET))),
            ];
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, batch);
            let a = ctx.phase_accum;
            let what = format!("{mode:?} batch={batch}");
            // Pre-testnet P0: the cancel-all of `UNKNOWN_MARKET` (not listed) is
            // rejected before it reaches the book pass, so it is not counted.
            assert_eq!(a.cancel_alls, 4, "{what}");
            // 2 + 1 + 0 + 0: addr(3) has nothing in market 4; the repeat
            // finds its sender's entry pruned.
            assert_eq!(a.cancel_all_books_visited, 3, "{what}");
            // addr(1): markets 1 and 2; addr(2): its stop in 3; the repeat
            // finds nothing left.
            assert_eq!(a.cancel_all_books_hit, 3, "{what}");
            assert_eq!((a.by_id_actions, a.by_id_books_probed), (0, 0), "{what}");
        }
    }
}

/// P2-1 counter test (plan section 3): with 300 books, a cancel-all visits
/// only the books where its sender rests or has a stop (3 orders, 1 stop),
/// on the single path, in a batched run and in the per-action loop (the
/// full scan visited all 300). The other sender rests everywhere and is
/// untouched.
#[test]
fn cancel_all_visits_only_the_senders_books() {
    const BOOKS: u64 = 300;
    for (batch, with_other_canceller) in
        [(false, false), (true, false), (true, true), (false, true)]
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ctx = new_ctx(&dir, BookMode::LevelAuthorityChunked);
        let mut setup: Vec<_> = (1..=BOOKS)
            .map(|m| (addr(2), gtc(m, true, 98, 1)))
            .collect();
        setup.extend([
            (addr(1), gtc(7, true, 99, 1)),
            (addr(1), gtc(150, false, 110, 1)),
            (addr(1), gtc(299, true, 97, 1)),
            (addr(1), stop_buy(42, 200)),
        ]);
        NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, batch);
        ctx.save_order_books();
        ctx.dirty_books.clear();
        assert_eq!(ctx.order_books.len() as u64, BOOKS);
        ctx.phase_accum = ExecPhaseAccum::default();

        let mut block = vec![(addr(1), cancel_all(None))];
        if with_other_canceller {
            // A run of two: addr(3) has nothing anywhere.
            block.push((addr(3), cancel_all(None)));
        }
        let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, batch);
        assert!(r.results.iter().all(|r| r.success), "{:?}", r.results);
        let a = ctx.phase_accum;
        let what = format!("batch={batch} run={with_other_canceller}");
        assert_eq!(a.cancel_alls, block.len() as u64, "{what}");
        assert_eq!(a.cancel_all_books_visited, 4, "{what}: books visited");
        assert_eq!(a.cancel_all_books_hit, 4, "{what}");
        let mut dirty: Vec<_> = ctx.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        assert_eq!(dirty, vec![7, 42, 150, 299], "{what}");
        assert!(ctx
            .order_books
            .values()
            .all(|b| b.open_order_count(&addr(1)) == 0));
        assert!(ctx
            .order_books
            .values()
            .all(|b| b.open_order_count(&addr(2)) == 1));
        assert_eq!(ctx.order_books[&42].pending_stop_count(), 0, "{what}");
    }
}

/// Step 0.2 (P2-1b): `CancelOrder` / `ModifyOrder` count the books probed to
/// find the order: a missing id probes every book (twice for a cancel: the
/// ownership pass and the cancel pass).
#[test]
fn by_id_counters_count_books_probed() {
    let (_dir, mut ctx) = counter_setup(BookMode::Classic, true);
    let own = (0..ctx.next_global_order_id)
        .find(|&id| {
            ctx.order_books[&1]
                .get_order(id)
                .is_some_and(|o| o.trader == addr(1))
        })
        .expect("addr(1) rests in market 1");
    let missing = ctx.next_global_order_id + 1_000;
    let modify = |order_id| NativeAction::ModifyOrder {
        order_id,
        new_price: None,
        new_qty: Some(fp(1)),
    };
    let block = vec![
        (addr(1), NativeAction::CancelOrder { order_id: missing }),
        (addr(1), modify(missing)),
    ];
    let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, true);
    assert!(r.results.iter().all(|r| !r.success), "{:?}", r.results);
    assert_eq!(ctx.phase_accum.by_id_actions, 2);
    assert_eq!(ctx.phase_accum.by_id_books_probed, 8 + 4);

    ctx.phase_accum = ExecPhaseAccum::default();
    let block = vec![(addr(1), NativeAction::CancelOrder { order_id: own })];
    let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, true);
    assert!(r.results[0].success, "{:?}", r.results);
    assert_eq!(ctx.phase_accum.by_id_actions, 1);
    // Found: each pass stops at market 1, wherever the map puts it.
    let probed = ctx.phase_accum.by_id_books_probed;
    assert!((2..=8).contains(&probed), "{probed}");
    assert_eq!(ctx.phase_accum.cancel_alls, 0);
}

/// Item 6 Phase 2 step 0.4: the production cancel-all (batched runs, the
/// single-action path) and the frozen full-scan reference
/// (`test_cancel_all_full_scan`: one action at a time over every book) give
/// identical results, gas, dirty marks, books, pending stops, CF dumps and
/// state roots in all four book modes. P2-1's book index must keep this.
#[test]
fn cancel_all_matches_the_full_scan_reference() {
    let resting = resting_after_setup();
    for (seed, mode) in [
        (5u64, BookMode::Classic),
        (6, BookMode::OrderRows),
        (7, BookMode::LevelAuthority),
        (8, BookMode::LevelAuthorityChunked),
    ] {
        let mut blocks = setup_blocks();
        // Pending stops for ten senders across the books.
        let stops: Vec<_> = (1..=10u8)
            .flat_map(|s| {
                [
                    (addr(s), stop_buy(MARKETS[s as usize % 4], 200 + s as i64)),
                    (addr(s), stop_buy(1, 300)),
                ]
            })
            .collect();
        let placed = stops.len();
        blocks.push(stops);
        blocks.extend(mixed_blocks(seed, &resting));
        let reference = run_with(&blocks, mode, true, true);
        let current = run_with(&blocks, mode, true, false);
        let flat: Vec<_> = current.results.iter().flatten().collect();
        assert!(flat.iter().filter(|r| r.0 == "cancel_all").count() > 60);
        assert!(
            current.pending_stops < placed,
            "cancel-alls must take stops: {}",
            current.pending_stops
        );
        assert!(current.trade_index > 10);
        assert_eq!(
            reference, current,
            "{mode:?}: cancel-all diverged from the full-scan reference"
        );
    }
}

/// Row 23 (18c s104): without resident books (`TORUS_RESIDENT_BOOKS=0`; the
/// code default before plan 9.13) every block loads its books, so an index would be
/// rebuilt in every block with a cancel-all. That path keeps the full scan:
/// every cancel-all visits every book (4 + 4 + 1 + 4, the step 0 count of
/// `cancel_all_counters_count_visited_and_hit_books`) and no index is built.
#[test]
fn non_resident_cancel_all_keeps_the_full_scan() {
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        for batch in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut ctx = new_ctx_with(&dir, mode, false);
            assert!(!ctx.resident_mode());
            let mut setup = vec![
                (addr(1), gtc(1, true, 99, 1)),
                (addr(1), gtc(2, false, 110, 1)),
                (addr(2), stop_buy(3, 200)),
            ];
            for m in MARKETS {
                setup.push((addr(4), gtc(m, true, 98, 1)));
            }
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, batch);
            ctx.save_order_books();
            ctx.dirty_books.clear();
            ctx.phase_accum = ExecPhaseAccum::default();
            let block = vec![
                (addr(1), cancel_all(None)),
                (addr(2), cancel_all(None)),
                (addr(3), cancel_all(Some(4))),
                (addr(1), cancel_all(None)),
                (addr(3), cancel_all(Some(UNKNOWN_MARKET))),
            ];
            let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, batch);
            // Pre-testnet P0: the last one names a market that is not listed.
            assert!(r.results[..4].iter().all(|r| r.success), "{:?}", r.results);
            assert_eq!(r.results[4].reason, FailureReason::UnknownMarket, "{:?}", r.results);
            let a = ctx.phase_accum;
            let what = format!("{mode:?} batch={batch}");
            assert_eq!(a.cancel_alls, 4, "{what}");
            assert_eq!(a.cancel_all_books_visited, 4 + 4 + 1 + 4, "{what}: every book");
            assert_eq!(a.cancel_all_books_hit, 3, "{what}");
            assert!(ctx.trader_index().0.is_none(), "{what}: index built");
            assert_eq!(ctx.cancel_index_size(), (0, 0), "{what}");
        }
    }
}

/// Row 23: the non-resident full scan (batched runs and the single path)
/// matches the frozen reference in all four book modes, and never builds the
/// cancel-all index.
#[test]
fn non_resident_cancel_all_matches_the_full_scan_reference() {
    let resting = resting_after_setup();
    for (seed, mode) in [
        (5u64, BookMode::Classic),
        (6, BookMode::OrderRows),
        (7, BookMode::LevelAuthority),
        (8, BookMode::LevelAuthorityChunked),
    ] {
        let mut blocks = setup_blocks();
        blocks.push(
            (1..=10u8)
                .map(|s| (addr(s), stop_buy(MARKETS[s as usize % 4], 200 + s as i64)))
                .collect(),
        );
        blocks.extend(mixed_blocks(seed, &resting));
        let no_index = &mut |ctx: &NativeExecContext| {
            assert!(ctx.trader_index().0.is_none(), "{mode:?}: index built");
        };
        let reference = run_observed_in(&blocks, mode, true, true, false, no_index);
        let current = run_observed_in(&blocks, mode, true, false, false, no_index);
        let flat: Vec<_> = current.results.iter().flatten().collect();
        assert!(flat.iter().filter(|r| r.0 == "cancel_all").count() > 60);
        assert_eq!(
            reference, current,
            "{mode:?}: non-resident cancel-all diverged from the full-scan reference"
        );
    }
}

/// P2-1 invariant (plan section 3): every (trader, market) where the book
/// holds an order, a stop or a reduce-only entry of the trader's is listed
/// by the cancel-all index, once built (a superset is allowed). Returns
/// whether the index was built. Also: the index size the gauges export
/// (s104) is the index's (trader, market) entries and traders (0 / 0 when
/// not built).
fn assert_index_covers_books(ctx: &NativeExecContext, what: &str) -> bool {
    let (carried, rebuilt) = ctx.trader_index();
    let size = carried.as_ref().map_or((0, 0), |c| {
        let entries: usize = c.values().map(Vec::len).sum();
        (entries as u64, c.len() as u64)
    });
    assert_eq!(ctx.cancel_index_size(), size, "{what}: index size");
    let Some(carried) = carried else {
        return false;
    };
    for (trader, markets) in &rebuilt {
        let listed = carried.get(trader).map_or(&[][..], Vec::as_slice);
        for m in markets {
            assert!(
                listed.binary_search(m).is_ok(),
                "{what}: {trader} has orders or stops in market {m}, the index lists {listed:?}"
            );
        }
    }
    true
}

fn limit_at(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> NativeAction {
    gtc(market_id, is_buy, price, qty)
}

fn stop_limit(market_id: MarketId, is_buy: bool, trigger: i64, limit: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(limit),
        quantity: fp(1),
        order_type: OrderType::StopLimit {
            trigger: fp(trigger),
            limit: fp(limit),
        },
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// Books 1-4 are the deep setup books (every sender rests there); 5-12
/// are sparse (only this generator's places rest there).
const INDEX_MARKETS: u64 = 12;

/// Review gap 2: sender `n`'s home book (a deep setup book).
fn ro_home(n: u8) -> MarketId {
    MARKETS[n as usize % 4]
}

/// Review gap 2 prefix (after the setup): every sender opens a position of
/// 3 in its home book (long for even `n`, short for odd; the setup's resting
/// makers take the other side), then rests two reduce-only orders of 2 on
/// the reducing side (the sweep cuts the second to 1).
fn ro_prefix_blocks() -> Vec<Vec<(Address, NativeAction)>> {
    let open = (1..=SENDERS + 4)
        .map(|n| {
            let long = n.is_multiple_of(2);
            let price = if long { 111 } else { 99 };
            (addr(n), limit_at(ro_home(n), long, price, 3))
        })
        .collect();
    let reduce_only = (1..=SENDERS + 4)
        .flat_map(|n| {
            let long = n.is_multiple_of(2);
            [0, 1].map(|k| {
                let price = if long { 112 + k } else { 98 - k };
                let NativeAction::PlaceOrder(mut p) = limit_at(ro_home(n), !long, price, 2) else {
                    unreachable!()
                };
                p.reduce_only = true;
                (addr(n), NativeAction::PlaceOrder(p))
            })
        })
        .collect();
    vec![open, reduce_only]
}

/// `(owner, id)` of every reduce-only order resting after the setup and
/// [`ro_prefix_blocks`].
fn ro_resting_after_prefix() -> Vec<(Address, u128)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx(&dir, BookMode::Classic);
    let mut blocks = setup_blocks();
    blocks.extend(ro_prefix_blocks());
    for block in &blocks {
        NativeExecutor::execute_batch_cancel_mode(&mut ctx, block, true);
    }
    let mut out = Vec::new();
    for id in 0..ctx.next_global_order_id {
        for m in MARKETS {
            if let Some(o) = ctx.order_books[&m].get_order(id).filter(|o| o.reduce_only) {
                out.push((o.trader, id));
            }
        }
    }
    assert!(out.len() >= SENDERS as usize, "{}", out.len());
    out
}

/// P2-1 blocks over the setup: places resting across 12 books, crossing
/// places in the deep books (partial fills, rests), stop-limits near the
/// touch (crossing trades fire them into resting orders: buy limits below
/// the asks, sell limits above the bids), modifies (quantity, and a new
/// price: re-inserted at the back), cancels by id, cancel-alls `None` /
/// `Some(m)` / unknown market, a sender's second cancel-all right after the
/// first, a transfer breaking runs.
///
/// `ro` (review gap 2, reduce-only): `Some(the reduce-only orders resting
/// after the prefix)` ([`ro_resting_after_prefix`]). The blocks start with
/// [`ro_prefix_blocks`] and a block of modifies that grow every sender's
/// first reduce-only order past the budget its pair shares (below); then a third of the places and stops become
/// home-book actions: reduce-only orders resting on the reducing side,
/// crossing to close, on the increasing side (rejected while the position
/// has not flipped), reduce-only stop-limits, modifies of the prefix's
/// reduce-only orders up to 3 (plan row 22: each modify sweeps, so the pair
/// never exceeds the position), plus plain crossing
/// places of 1-6 that reduce, close, flip or grow the position the resting
/// reduce-only orders depend on. The extra draws happen only with `ro`, so
/// `ro: None` keeps the existing seeds' sequences.
fn index_blocks(
    seed: u64,
    resting: &[(Address, u128)],
    ro: Option<&[(Address, u128)]>,
) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let mut blocks = Vec::new();
    if let Some(ro_ids) = ro {
        blocks.extend(ro_prefix_blocks());
        // Each sender's first reduce-only order modified up to 3: with the
        // second (1) the pair would exceed the position (3); plan row 22:
        // the modify's sweep cancels the second.
        let mut seen = std::collections::BTreeSet::new();
        blocks.push(
            ro_ids
                .iter()
                .filter(|(owner, _)| seen.insert(*owner))
                .map(|&(owner, id)| {
                    let modify = NativeAction::ModifyOrder {
                        order_id: id,
                        new_price: None,
                        new_qty: Some(fp(3)),
                    };
                    (owner, modify)
                })
                .collect(),
        );
    }
    for _ in 0..4 {
        let mut block = Vec::new();
        for _ in 0..150 {
            let n = 1 + rng.below(SENDERS as u64 + 4) as u8;
            let mut s = addr(n);
            let m = 1 + rng.below(INDEX_MARKETS);
            let deep = MARKETS[rng.below(4) as usize];
            let (owner, id) = resting[rng.below(resting.len() as u64) as usize];
            let is_buy = rng.below(2) == 0;
            let mut action = match rng.below(24) {
                0..=3 => cancel_all(None),
                4 | 5 => cancel_all(Some(m)),
                6 => cancel_all(Some(UNKNOWN_MARKET)),
                7 | 17 => {
                    s = owner;
                    NativeAction::CancelOrder { order_id: id }
                }
                8 | 9 => {
                    s = owner;
                    NativeAction::ModifyOrder {
                        order_id: id,
                        // 105 crosses neither side of the setup books.
                        new_price: (rng.below(2) == 0).then(|| fp(105)),
                        new_qty: Some(fp(1)),
                    }
                }
                10..=12 => {
                    let t = 102 + rng.below(7) as i64;
                    let mk = if rng.below(2) == 0 { deep } else { m };
                    if is_buy {
                        stop_limit(mk, true, t, t + 1)
                    } else {
                        stop_limit(mk, false, t, t - 1)
                    }
                }
                13..=15 => {
                    let mk = if rng.below(3) == 0 { m } else { deep };
                    limit_at(
                        mk,
                        is_buy,
                        if is_buy { 111 } else { 99 },
                        2 + rng.below(3) as i64,
                    )
                }
                16 if rng.below(4) == 0 => NativeAction::TransferToSpot {
                    amount: U256::from(1_000u64),
                },
                _ => {
                    let price = if is_buy {
                        96 + rng.below(5)
                    } else {
                        110 + rng.below(5)
                    };
                    limit_at(m, is_buy, price as i64, 1 + rng.below(3) as i64)
                }
            };
            let ro_ids = ro.filter(|_| matches!(action, NativeAction::PlaceOrder(_)));
            if let Some(ro_ids) = ro_ids.filter(|_| rng.below(3) == 0) {
                // `long`: the side the position was opened on (it may have
                // flipped since: then the sides below swap roles).
                let (hm, long) = (ro_home(n), n.is_multiple_of(2));
                let qty = 1 + rng.below(4) as i64;
                let reduce_only = |mut a: NativeAction| {
                    if let NativeAction::PlaceOrder(p) = &mut a {
                        p.reduce_only = true;
                    }
                    a
                };
                action = match rng.below(8) {
                    // Rests on the reducing side.
                    0 | 1 => {
                        let off = rng.below(5) as i64;
                        reduce_only(if long {
                            limit_at(hm, false, 110 + off, qty)
                        } else {
                            limit_at(hm, true, 100 - off, qty)
                        })
                    }
                    // Crosses to close (part of) the position.
                    2 => reduce_only(limit_at(hm, !long, if long { 99 } else { 111 }, qty)),
                    // The increasing side.
                    3 => reduce_only(limit_at(hm, long, if long { 97 } else { 113 }, qty)),
                    // A reduce-only stop-limit on the reducing side.
                    4 => {
                        let t = 102 + rng.below(7) as i64;
                        reduce_only(if long {
                            stop_limit(hm, false, t, t - 1)
                        } else {
                            stop_limit(hm, true, t, t + 1)
                        })
                    }
                    // A prefix reduce-only order modified up to 3 (re-inserted).
                    5 | 6 => {
                        let (owner, id) = ro_ids[rng.below(ro_ids.len() as u64) as usize];
                        s = owner;
                        NativeAction::ModifyOrder {
                            order_id: id,
                            new_price: None,
                            new_qty: Some(fp(3)),
                        }
                    }
                    // A plain crossing place: reduces, closes, flips or grows it.
                    _ => {
                        let buy = rng.below(2) == 0;
                        limit_at(hm, buy, if buy { 111 } else { 99 }, 1 + rng.below(6) as i64)
                    }
                };
            }
            block.push((s, action));
            if rng.below(12) == 0 {
                block.push((s, cancel_all(None)));
            }
            if rng.below(12) == 0 {
                block.push((s, cancel_all(Some(m))));
            }
        }
        blocks.push(block);
    }
    blocks
}

/// P2-1 differential (plan section 3): the cancel-all index vs the frozen
/// full scan on `index_blocks`, in all four book modes, on the batched runs
/// and on the per-action loop: identical results, gas, dirty marks, books,
/// stops, CF dumps and state roots, and the index covers the books after
/// every block (`run_with`).
#[test]
fn cancel_all_index_matches_the_full_scan_reference_random() {
    let resting = resting_after_setup();
    for (seed, mode) in [
        (11u64, BookMode::Classic),
        (12, BookMode::OrderRows),
        (13, BookMode::LevelAuthority),
        (14, BookMode::LevelAuthorityChunked),
    ] {
        let mut blocks = setup_blocks();
        blocks.extend(index_blocks(seed, &resting, None));
        for batch in [true, false] {
            let reference = run_with(&blocks, mode, batch, true);
            let current = run_with(&blocks, mode, batch, false);
            let flat: Vec<_> = current.results.iter().flatten().collect();
            let ok = |kind: &str| flat.iter().filter(|r| r.0 == kind && r.1).count();
            assert!(ok("cancel_all") > 150, "{}", ok("cancel_all"));
            assert!(
                ok("modify_order") >= 5 && ok("cancel_order") >= 5,
                "modify {} cancel {}",
                ok("modify_order"),
                ok("cancel_order")
            );
            assert!(current.trade_index > 100, "{}", current.trade_index);
            assert!(current.pending_stops > 0);
            assert_eq!(
                reference, current,
                "{mode:?} batch={batch}: index diverged from the full scan"
            );
        }
    }
}

/// `trader`'s signed position in `market` (+long / -short, zero when flat).
fn signed_position(ctx: &NativeExecContext, trader: &Address, market: MarketId) -> FixedPoint {
    match ctx.positions.get_position(trader, market).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

/// Reduce-only coverage of a run (review gap 2), from the context after
/// every block: what happened by the next block to the positions that
/// resting reduce-only orders depend on.
#[derive(Default, Debug)]
struct RoStats {
    /// (trader, market) -> (signed position, resting reduce-only orders)
    /// after the previous block.
    last: std::collections::BTreeMap<(Address, MarketId), (FixedPoint, usize)>,
    resting_max: usize,
    reduced: u64,
    closed: u64,
    flipped: u64,
    /// Resting reduce-only orders a sweep would cut (position flat, on
    /// their side, or already covered by the trader's other reduce-only
    /// orders) after a block: left for a sweep or a cancel-all.
    leftovers: u64,
    /// Resting reduce-only orders gone by the next block (filled, cut,
    /// cancelled).
    removed: usize,
}

impl RoStats {
    fn observe(&mut self, ctx: &NativeExecContext) {
        use torus_core::order_book::reduce_only_allowance;
        let mut now = std::collections::BTreeMap::new();
        for (&m, book) in &ctx.order_books {
            for t in book.reduce_only_traders() {
                let orders: Vec<_> = book
                    .orders_for_trader(&t)
                    .into_iter()
                    .filter(|o| o.reduce_only)
                    .collect();
                if orders.is_empty() {
                    continue;
                }
                let pos = signed_position(ctx, &t, m);
                // What a sweep would cut: orders on the increasing side, or
                // beyond the position (the sweep's budget).
                let mut budget = [true, false].map(|is_buy| reduce_only_allowance(pos, is_buy));
                for o in &orders {
                    let left = &mut budget[usize::from(o.side != torus_types::Side::Buy)];
                    if o.remaining_qty > *left {
                        self.leftovers += 1;
                    }
                    *left = (*left - o.remaining_qty).max(FixedPoint::ZERO);
                }
                now.insert((t, m), (pos, orders.len()));
            }
        }
        for (&(t, m), &(pos, n)) in &self.last {
            let (new_pos, new_n) = now
                .get(&(t, m))
                .copied()
                .unwrap_or_else(|| (signed_position(ctx, &t, m), 0));
            self.removed += n.saturating_sub(new_n);
            let abs = |p: FixedPoint| if p < FixedPoint::ZERO { -p } else { p };
            let flipped = pos != FixedPoint::ZERO
                && new_pos != FixedPoint::ZERO
                && (new_pos > FixedPoint::ZERO) != (pos > FixedPoint::ZERO);
            if new_pos == FixedPoint::ZERO && pos != FixedPoint::ZERO {
                self.closed += 1;
            } else if flipped {
                self.flipped += 1;
            } else if abs(new_pos) < abs(pos) {
                self.reduced += 1;
            }
        }
        self.resting_max = self.resting_max.max(now.values().map(|v| v.1).sum());
        self.last = now;
    }
}

/// Review gap 2 (GPT-6.1-sol on C1): the P2-1 differential with reduce-only
/// orders. `index_blocks` with `ro` (a third of the places and stops
/// reduce-only) vs the frozen full scan, in all four book modes, batched and
/// per-action: identical results, gas, dirty marks, books, stops, CF dumps,
/// state roots, and the index covers the books (reduce-only entries
/// included) after every block. Non-vacuous: reduce-only orders rest, get
/// rejected (flat / increasing side), and positions they depend on are
/// reduced, closed and flipped while they rest. Plan row 22: no leftover
/// (a resting reduce-only order a sweep would cut) survives a block.
#[test]
fn cancel_all_index_matches_the_full_scan_reference_reduce_only() {
    let resting = resting_after_setup();
    let ro_resting = ro_resting_after_prefix();
    for (seed, mode) in [
        (31u64, BookMode::Classic),
        (32, BookMode::OrderRows),
        (33, BookMode::LevelAuthority),
        (34, BookMode::LevelAuthorityChunked),
    ] {
        let mut blocks = setup_blocks();
        blocks.extend(index_blocks(seed, &resting, Some(&ro_resting)));
        for batch in [true, false] {
            let what = format!("{mode:?} batch={batch}");
            let reference = run_with(&blocks, mode, batch, true);
            let mut stats = RoStats::default();
            let current = run_observed(&blocks, mode, batch, false, &mut |ctx| stats.observe(ctx));
            let (mut ro_ok, mut ro_failed) = (0, 0);
            for (block, results) in blocks.iter().zip(&current.results) {
                assert_eq!(block.len(), results.len(), "{what}");
                for ((_, action), r) in block.iter().zip(results) {
                    if matches!(action, NativeAction::PlaceOrder(p) if p.reduce_only) {
                        if r.1 {
                            ro_ok += 1;
                        } else {
                            ro_failed += 1;
                        }
                    }
                }
            }
            let flat: Vec<_> = current.results.iter().flatten().collect();
            let cancel_alls = flat.iter().filter(|r| r.0 == "cancel_all" && r.1).count();
            println!(
                "{what}: reduce-only placed {ro_ok}, rejected {ro_failed}, cancel-alls {cancel_alls}, \
                 fills {}, {stats:?}",
                current.trade_index
            );
            assert!(
                ro_ok >= 20 && ro_failed >= 20,
                "{what}: {ro_ok} / {ro_failed}"
            );
            assert!(cancel_alls > 150, "{what}: {cancel_alls}");
            assert!(current.trade_index > 100, "{what}");
            assert!(stats.resting_max >= 5, "{what}: {stats:?}");
            assert!(
                stats.reduced >= 1 && stats.closed >= 1 && stats.flipped >= 1,
                "{what}: {stats:?}"
            );
            assert!(
                stats.leftovers == 0 && stats.removed >= 20,
                "{what}: {stats:?}"
            );
            assert_eq!(
                reference, current,
                "{what}: index diverged from the full scan (reduce-only)"
            );
        }
    }
}

/// P2-1: every way an order comes to rest or a stop is stored feeds the
/// index after it was built, and a later cancel-all `None` of the sender
/// finds it (visiting only the sender's books): a resting place through the
/// batch matching and through the single-action path, the rest of a
/// partially filled order, a stop on both paths, a stop-limit fired into a
/// resting order, a modify (re-insert, new price). Each sender's only
/// presence in its book comes from that path; the index is built first (a
/// warm-up cancel-all), so the build's scan cannot cover a missed site.
#[test]
fn every_insert_path_feeds_the_index() {
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        for batch in [false, true] {
            let what = format!("{mode:?} batch={batch}");
            let dir = tempfile::tempdir().expect("tempdir");
            let mut ctx = new_ctx(&dir, mode);
            // Books 1-9, each with a resting bid at 90 and an ask at 120 of addr(40).
            let mut setup = Vec::new();
            for m in 1..=9 {
                setup.push((addr(40), gtc(m, true, 90, 5)));
                setup.push((addr(40), gtc(m, false, 120, 5)));
            }
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, batch);
            // Warm-up: builds the index from the books.
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[(addr(39), cancel_all(None))],
                batch,
            );
            assert!(
                assert_index_covers_books(&ctx, &what),
                "{what}: index not built"
            );

            // 1: batch matching rests addr(1) in market 1.
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[(addr(1), gtc(1, true, 95, 1))],
                batch,
            );
            // 2: the single-action path rests addr(2) in market 2.
            let r = NativeExecutor::execute(&mut ctx, &addr(2), &gtc(2, true, 95, 1));
            assert!(r.success, "{what}: {r:?}");
            // 3: addr(3) buys 8 at 120 in market 3: 5 fill, 3 rest.
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[(addr(3), gtc(3, true, 120, 8))],
                batch,
            );
            assert_eq!(
                ctx.order_books[&3].open_order_count(&addr(3)),
                1,
                "{what}: rest of a partial fill"
            );
            // 4: stops: addr(4) in market 4 (batch), addr(5) in market 5 (single path).
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[(addr(4), stop_buy(4, 200))],
                batch,
            );
            let r = NativeExecutor::execute(&mut ctx, &addr(5), &stop_buy(5, 200));
            assert!(r.success, "{what}: {r:?}");
            // 5: addr(6)'s buy stop-limit (trigger 101, limit 102) in market 6
            // fires on the trade at 101 between addr(7) and addr(8) and rests
            // at 102 (a new resting entry for addr(6); the stop logged it too).
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[(addr(6), stop_limit(6, true, 101, 102))],
                batch,
            );
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[
                    (addr(8), gtc(6, false, 101, 1)),
                    (addr(7), gtc(6, true, 101, 1)),
                ],
                batch,
            );
            assert_eq!(
                ctx.order_books[&6].pending_stop_count(),
                0,
                "{what}: stop fired"
            );
            assert_eq!(
                ctx.order_books[&6].open_order_count(&addr(6)),
                1,
                "{what}: fired stop rests"
            );
            // 6: addr(9) rests in market 7, then a modify moves it to 100 (re-insert).
            NativeExecutor::execute_batch_cancel_mode(
                &mut ctx,
                &[(addr(9), gtc(7, true, 95, 2))],
                batch,
            );
            let id = ctx.order_books[&7].orders_for_trader(&addr(9))[0].id;
            let r = NativeExecutor::execute(
                &mut ctx,
                &addr(9),
                &NativeAction::ModifyOrder {
                    order_id: id,
                    new_price: Some(fp(100)),
                    new_qty: None,
                },
            );
            assert!(r.success, "{what}: {r:?}");
            assert!(assert_index_covers_books(&ctx, &what));

            for (sender, market) in [(1u8, 1u64), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (9, 7)] {
                ctx.phase_accum = ExecPhaseAccum::default();
                ctx.dirty_books.clear();
                let r = NativeExecutor::execute_batch_cancel_mode(
                    &mut ctx,
                    &[(addr(sender), cancel_all(None))],
                    batch,
                );
                assert!(r.results[0].success, "{what}");
                let book = &ctx.order_books[&market];
                assert_eq!(
                    book.open_order_count(&addr(sender)),
                    0,
                    "{what}: addr({sender}) left in {market}"
                );
                assert!(
                    book.traders_present().all(|t| *t != addr(sender)),
                    "{what}: addr({sender}) in {market}"
                );
                assert_eq!(
                    ctx.phase_accum.cancel_all_books_visited, 1,
                    "{what}: addr({sender})"
                );
                assert_eq!(
                    ctx.phase_accum.cancel_all_books_hit, 1,
                    "{what}: addr({sender})"
                );
                assert!(ctx.dirty_books.contains(&market), "{what}: addr({sender})");
            }
            assert!(assert_index_covers_books(&ctx, &what));
        }
    }
}

/// P2-1 restart (plan section 3): `index_blocks` on two identical DBs. On
/// A the context carries its index across the blocks; B restarts (a fresh
/// context loads the saved books). The index B's load builds equals the
/// books' exact index, A's carried index covers it (and still lists markets
/// emptied since: a superset) and, restricted to live entries, equals it.
/// A final block (every sender cancels everything, some twice) gives
/// identical results, dirty marks, CF dumps and state roots on A and B, and
/// leaves nothing of anyone on any book.
#[test]
fn cancel_all_index_rebuilt_at_load_equals_the_carried_one() {
    let resting = resting_after_setup();
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
        BookMode::LevelAuthorityChunked,
    ] {
        let what = format!("{mode:?}");
        let mut blocks = setup_blocks();
        blocks.extend(index_blocks(21, &resting, None));
        let run_blocks = |dir: &tempfile::TempDir| {
            let mut ctx = new_ctx(dir, mode);
            for block in &blocks {
                NativeExecutor::execute_batch_cancel_mode(&mut ctx, block, true);
                assert!(ctx.fatal_error.is_none());
                ctx.save_order_books();
                ctx.dirty_books.clear();
            }
            ctx
        };
        let (dir_a, dir_b) = (
            tempfile::tempdir().expect("tempdir"),
            tempfile::tempdir().expect("tempdir"),
        );
        let mut carried = run_blocks(&dir_a);
        let state_b = run_blocks(&dir_b).state.clone();
        let mut loaded = NativeExecContext::new_with_mode(
            state_b,
            2,
            1000,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            mode,
            None,
        );

        let (index, exact) = carried.trader_index();
        let index = index.expect("built by the blocks' cancel-alls");
        let (none, rebuilt) = loaded.trader_index();
        assert!(none.is_none(), "{what}: a load starts without an index");
        assert_eq!(
            rebuilt, exact,
            "{what}: the reloaded books give the carried books' index"
        );
        assert!(assert_index_covers_books(&carried, &what));
        let is_live = |t: &Address, m: &MarketId| exact.get(t).is_some_and(|e| e.contains(m));
        let stale: usize = index
            .iter()
            .map(|(t, ms)| ms.iter().filter(|m| !is_live(t, m)).count())
            .sum();
        assert!(
            stale > 0,
            "{what}: the carried index should still list some emptied markets"
        );
        let live: TraderIndexSnapshot = index
            .iter()
            .map(|(t, ms)| {
                (
                    *t,
                    ms.iter()
                        .copied()
                        .filter(|m| is_live(t, m))
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, ms)| !ms.is_empty())
            .collect();
        assert_eq!(
            live, exact,
            "{what}: carried index restricted to live entries == rebuilt"
        );

        let mut last: Vec<_> = (1..=SENDERS + 4)
            .map(|s| (addr(s), cancel_all(None)))
            .collect();
        last.push((addr(3), cancel_all(Some(2))));
        last.push((addr(5), cancel_all(None)));
        let mut outcome = Vec::new();
        for ctx in [&mut carried, &mut loaded] {
            ctx.dirty_books.clear();
            let r = NativeExecutor::execute_batch_cancel_mode(ctx, &last, true);
            assert!(ctx.fatal_error.is_none());
            let results: Vec<_> = r
                .results
                .iter()
                .map(|a| (a.action_type, a.success, a.error.clone(), a.gas_used))
                .collect();
            let mut dirty: Vec<_> = ctx.dirty_books.iter().copied().collect();
            dirty.sort_unstable();
            ctx.save_order_books();
            assert!(
                ctx.order_books
                    .values()
                    .all(|b| b.traders_present().next().is_none()),
                "{what}: all cancelled"
            );
            let mut cf_dump = Vec::new();
            for cf in [
                CF_NATIVE_BALANCES,
                CF_NATIVE_POSITIONS,
                CF_NATIVE_ORDER_BOOKS,
                CF_BOOK_ORDER_ROWS,
            ] {
                cf_dump.push(ctx.state.iterate_cf(cf, None).expect("iterate cf"));
            }
            let root = compute_native_state_root(&ctx.state).expect("state root");
            outcome.push((results, dirty, cf_dump, root));
        }
        assert!(outcome[0].0.iter().all(|r| r.1), "{what}");
        assert!(
            outcome[0].1.len() >= 8,
            "{what}: the final cancel-alls touched the books"
        );
        assert!(
            outcome[0] == outcome[1],
            "{what}: carried and rebuilt index diverged"
        );
    }
}

// ---- `TORUS_PARALLEL_CANCEL_ALL`: a batched cancel-all run's book pass on
// the exec pool, one job group per market (`cancel_all_groups_parallel`).
// Every job count must give the serial loop's results, gas, dirty marks,
// books, stops, CF dumps, state roots and counters; a job panic must
// fail-stop the block before Phase 2.

/// What a [`run_par`] run observed after every block.
#[derive(Default)]
struct ParObs {
    /// Cumulative `(cancel_alls, books visited, books hit)` after each block.
    counters: Vec<(u64, u64, u64)>,
    /// Runs that took the parallel book pass, after the last block.
    parallel_runs: u64,
}

/// The cross-market cloid index, once built, holds every live cloid of the
/// books at its market and id (removes may still wait in a book's log, so
/// the carried index may hold more).
fn assert_cloid_index_covers_books(ctx: &NativeExecContext, what: &str) {
    let (carried, rebuilt) = ctx.cloid_index();
    if let Some(carried) = carried {
        for (key, slot) in &rebuilt {
            assert_eq!(carried.get(key), Some(slot), "{what}: cloid {key:?}");
        }
    }
}

/// `blocks` on batched runs and resident books (the parallel pass is on the
/// P2-1 path only) with the parallel cancel-all pinned to `workers` jobs
/// (1 = the serial loop, whatever the env says) and the work gate off
/// (MARKETS has 4 books: every run is below the default 64 visits). Both
/// indexes are checked after every block (the trader index in
/// `run_observed_par`, the cloid index here).
fn run_par(
    blocks: &[Vec<(Address, NativeAction)>],
    mode: BookMode,
    workers: usize,
) -> (RunFingerprint, ParObs) {
    let mut obs = ParObs::default();
    let what = format!("{mode:?} workers={workers}");
    let print = run_observed_par(
        blocks,
        mode,
        true,
        false,
        true,
        Some((workers, 0)),
        &mut |ctx: &NativeExecContext| {
            assert_cloid_index_covers_books(ctx, &what);
            let a = ctx.phase_accum;
            obs.counters.push((
                a.cancel_alls,
                a.cancel_all_books_visited,
                a.cancel_all_books_hit,
            ));
            obs.parallel_runs = ctx.test_cancel_all_parallel_runs;
        },
    );
    (print, obs)
}

/// (a) The mixed blocks of `cancel_batch_flag_on_matches_off_mixed_blocks`
/// (runs of `None` / `Some(m)` / unknown-market / repeated cancel-alls,
/// places inside runs, run-breaking cancels, modifies and transfers), plus
/// OrderRows: 2, 3, 4 and 64 jobs equal the serial loop, and (d) count the
/// same visits and hits after every block.
#[test]
fn parallel_cancel_all_matches_serial_mixed_blocks() {
    let resting = resting_after_setup();
    for (seed, mode) in [
        (1u64, BookMode::Classic),
        (2, BookMode::LevelAuthority),
        (3, BookMode::LevelAuthorityChunked),
        (4, BookMode::Classic),
        (6, BookMode::OrderRows),
    ] {
        let mut blocks = setup_blocks();
        blocks.extend(mixed_blocks(seed, &resting));
        let (serial, serial_obs) = run_par(&blocks, mode, 1);
        assert_eq!(
            serial_obs.parallel_runs, 0,
            "{mode:?}: 1 job is the serial loop"
        );
        let flat: Vec<_> = serial.results.iter().flatten().collect();
        assert!(flat.iter().filter(|r| r.0 == "cancel_all").count() > 60);
        assert!(serial.trade_index > 10, "{mode:?}: scenario must fill");
        for workers in [2, 3, 4, 64] {
            let what = format!("{mode:?} seed {seed} workers={workers}");
            let (par, par_obs) = run_par(&blocks, mode, workers);
            assert!(par_obs.parallel_runs > 0, "{what}: the pool was never used");
            assert_eq!(par_obs.counters, serial_obs.counters, "{what}: counters");
            assert_eq!(par, serial, "{what}: parallel cancel-all diverged");
        }
    }
}

/// (b) One long run that empties the books, then rests the run's places.
#[test]
fn parallel_cancel_all_matches_serial_long_run_emptying_books() {
    let blocks = long_emptying_run_blocks();
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let (serial, serial_obs) = run_par(&blocks, mode, 1);
        for workers in [2, 4, 64] {
            let what = format!("{mode:?} workers={workers}");
            let (par, par_obs) = run_par(&blocks, mode, workers);
            assert_eq!(
                par_obs.parallel_runs, 1,
                "{what}: the one run used the pool"
            );
            assert_eq!(par_obs.counters, serial_obs.counters, "{what}: counters");
            assert_eq!(par, serial, "{what}: parallel cancel-all diverged");
        }
    }
}

/// (c) Senders with only pending stops, in several books: the parallel pass
/// takes their stops, releases the same reservations, marks the same books
/// dirty and persists the same rows as the serial loop, in all four modes.
#[test]
fn parallel_cancel_all_of_stop_only_senders_matches_serial() {
    let mut setup = Vec::new();
    for s in 1..=6u8 {
        for m in MARKETS {
            if !(s as u64 + m).is_multiple_of(3) {
                setup.push((addr(s), stop_buy(m, 200 + s as i64)));
            }
        }
    }
    let placed = setup.len();
    for m in MARKETS {
        setup.push((addr(7), gtc(m, true, 99, 1)));
        setup.push((addr(8), gtc(m, false, 110, 1)));
    }
    let block = vec![
        (addr(1), cancel_all(None)),
        (addr(2), cancel_all(None)),
        (addr(3), cancel_all(Some(2))),
        (addr(4), cancel_all(Some(3))),
        (addr(5), cancel_all(None)),
        (addr(7), cancel_all(None)),
        (addr(6), cancel_all(Some(UNKNOWN_MARKET))),
    ];
    let blocks = vec![setup, block];
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
        BookMode::LevelAuthorityChunked,
    ] {
        let (serial, serial_obs) = run_par(&blocks, mode, 1);
        assert!(
            serial.pending_stops > 0 && serial.pending_stops < placed,
            "{mode:?}: {} of {placed} stops left",
            serial.pending_stops
        );
        // Pre-testnet P0: all but the last (a market that is not listed).
        let (last, rest) = serial.results[1].split_last().unwrap();
        assert!(rest.iter().all(|r| r.1) && !last.1, "{mode:?}");
        for workers in [2, 4] {
            let what = format!("{mode:?} workers={workers}");
            let (par, par_obs) = run_par(&blocks, mode, workers);
            assert_eq!(par_obs.parallel_runs, 1, "{what}: the run used the pool");
            assert_eq!(par_obs.counters, serial_obs.counters, "{what}: counters");
            assert_eq!(par, serial, "{what}: stop-only cancel-alls diverged");
        }
    }
}

/// (d) The counter scenario of `cancel_all_counters_count_visited_and_hit_books`
/// on the parallel pass: 3 books visited, 3 hit, the same dirty marks.
#[test]
fn parallel_cancel_all_counts_visited_and_hit_books() {
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let (_dir, mut ctx) = counter_setup(mode, true);
        ctx.cancel_all_workers = 4;
        ctx.cancel_all_min_visits = 0;
        let block = vec![
            (addr(1), cancel_all(None)),
            (addr(2), cancel_all(None)),
            (addr(3), cancel_all(Some(4))),
            (addr(1), cancel_all(None)),
            (addr(3), cancel_all(Some(UNKNOWN_MARKET))),
        ];
        let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, true);
        let what = format!("{mode:?}");
        // Pre-testnet P0: the last one names a market that is not listed.
        assert!(
            r.results[..4].iter().all(|r| r.success),
            "{what}: {:?}",
            r.results
        );
        assert_eq!(r.results[4].reason, FailureReason::UnknownMarket, "{what}");
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        assert_eq!(ctx.test_cancel_all_parallel_runs, 1, "{what}");
        let a = ctx.phase_accum;
        assert_eq!(a.cancel_alls, 4, "{what}");
        assert_eq!(a.cancel_all_books_visited, 3, "{what}");
        assert_eq!(a.cancel_all_books_hit, 3, "{what}");
        let mut dirty: Vec<_> = ctx.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        assert_eq!(dirty, vec![1, 2, 3], "{what}");
        assert_eq!(ctx.order_books[&1].open_order_count(&addr(1)), 0, "{what}");
        assert_eq!(ctx.order_books[&2].open_order_count(&addr(1)), 0, "{what}");
        assert_eq!(ctx.order_books[&3].pending_stop_count(), 0, "{what}");
        assert!(assert_index_covers_books(&ctx, &what));
    }
}

/// Merge of main (parallel cancel-all) into item 7 step 2 (B16): the books a parallel
/// cancel-all run empties get their bbo rows deleted at the save, the books it leaves
/// non-empty carry their new top of book, exactly as the dirty marks of the serial pass
/// (the differential tests above also compare `CF_NATIVE_BBO`, serial vs parallel).
#[test]
fn parallel_cancel_all_writes_the_bbo_rows_of_its_books() {
    let bbo_rows = |ctx: &NativeExecContext| -> Vec<(Vec<u8>, Vec<u8>)> {
        ctx.state
            .iterate_cf(CF_NATIVE_BBO, None)
            .expect("iterate bbo")
    };
    let blocks = long_emptying_run_blocks();
    assert_eq!(blocks.len(), 2, "setup + one emptying run");
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
        BookMode::LevelAuthorityChunked,
    ] {
        let what = format!("{mode:?}");
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ctx = new_ctx(&dir, mode);
        ctx.cancel_all_workers = 4;
        ctx.cancel_all_min_visits = 0;

        NativeExecutor::execute_batch_cancel_mode(&mut ctx, &blocks[0], true);
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        ctx.save_order_books();
        ctx.dirty_books.clear();
        let keys: Vec<Vec<u8>> = bbo_rows(&ctx).into_iter().map(|(k, _)| k).collect();
        let want: Vec<Vec<u8>> = MARKETS.iter().map(|m| m.to_be_bytes().to_vec()).collect();
        assert_eq!(keys, want, "{what}: every setup book has a bbo row");

        NativeExecutor::execute_batch_cancel_mode(&mut ctx, &blocks[1], true);
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        assert_eq!(
            ctx.test_cancel_all_parallel_runs, 1,
            "{what}: the run used the pool"
        );
        let mut dirty: Vec<_> = ctx.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        assert_eq!(dirty, MARKETS.to_vec(), "{what}: every book was hit");
        ctx.save_order_books();
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        // The row of each book as its top now stands; an empty book has none.
        let mut want: Vec<(Vec<u8>, Vec<u8>)> = ctx
            .order_books
            .iter()
            .map(|(m, b)| (*m, b.bbo_row()))
            .filter(|(_, r)| *r != [0u8; 32])
            .map(|(m, r)| (m.to_be_bytes().to_vec(), r.to_vec()))
            .collect();
        want.sort_unstable();
        let got = bbo_rows(&ctx);
        assert_eq!(got, want, "{what}: bbo rows follow the books");
        for m in [1u64, 3, 4] {
            assert!(
                !got.iter().any(|(k, _)| *k == m.to_be_bytes()),
                "{what}: emptied book {m} keeps no bbo row"
            );
        }
    }
}

fn gtc_cloid(market_id: MarketId, is_buy: bool, price: i64, cloid: u128) -> NativeAction {
    let NativeAction::PlaceOrder(mut p) = gtc(market_id, is_buy, price, 1) else {
        unreachable!()
    };
    p.client_order_id = Some(cloid);
    NativeAction::PlaceOrder(p)
}

/// (e) Cloids freed by a parallel run are reusable in the same block: the
/// cloid pre-pass after Phase 1 sees the books the jobs left. Senders 1-20
/// cancel everything, then (places do not break a run) senders 1-24 place
/// their cloids again: 1-20 reuse them, 21-24 are duplicates. Equal to the
/// serial loop; both indexes cover the books after every block.
#[test]
fn parallel_cancel_all_then_cloid_reuse_matches_serial() {
    let cloid = |s: u8, m: MarketId| 0xC0_0000 + (s as u128) * 16 + m as u128;
    let place = |s: u8, m: MarketId, away: i64| {
        let buy = s.is_multiple_of(2);
        gtc_cloid(
            m,
            buy,
            if buy { 97 - away } else { 113 + away },
            cloid(s, m),
        )
    };
    let mut blocks = setup_blocks();
    blocks.push(
        (1..=24u8)
            .flat_map(|s| MARKETS.map(|m| (addr(s), place(s, m, 0))))
            .collect(),
    );
    let mut block: Vec<_> = (1..=20u8).map(|s| (addr(s), cancel_all(None))).collect();
    block.extend((1..=24u8).flat_map(|s| MARKETS.map(|m| (addr(s), place(s, m, 1)))));
    blocks.push(block);
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let (serial, serial_obs) = run_par(&blocks, mode, 1);
        let last = serial.results.last().expect("blocks");
        assert!(
            last[..20].iter().all(|r| r.0 == "cancel_all" && r.1),
            "{mode:?}"
        );
        let places = &last[20..];
        assert_eq!(
            places.iter().filter(|r| r.1).count(),
            20 * 4,
            "{mode:?}: reused"
        );
        assert_eq!(
            places.iter().filter(|r| !r.1).count(),
            4 * 4,
            "{mode:?}: duplicates"
        );
        for workers in [2, 4] {
            let what = format!("{mode:?} workers={workers}");
            let (par, par_obs) = run_par(&blocks, mode, workers);
            assert_eq!(par_obs.parallel_runs, 1, "{what}: the run used the pool");
            assert_eq!(par_obs.counters, serial_obs.counters, "{what}: counters");
            assert_eq!(par, serial, "{what}: cloid reuse after cancel-all diverged");
        }
    }
}

/// (f) A panicking job fail-stops the block: `fatal_error` names the market,
/// Phase 2 never runs (no fill, no order id taken, nothing new rests), the
/// panicking book leaves the context, the test thread does not unwind and a
/// second batch on the context (as the post-EVM batch) runs without
/// panicking. The same block with the work gate shut (`min_visits =
/// usize::MAX`: the serial loop, where the plant never fires) runs clean.
/// A context that already carries a fault (e.g. a load error) still stops
/// at the panic, and keeps its first fault.
#[test]
fn parallel_cancel_all_worker_panic_fail_stops_before_phase_2() {
    const PLANT: MarketId = 3;
    let mut block: Vec<_> = (1..=10u8).map(|s| (addr(s), cancel_all(None))).collect();
    // Places inside the run: a crossing buy (fills against the setup's asks)
    // and a resting bid, from senders with nothing on the books.
    block.push((addr(SENDERS + 1), gtc(1, true, 111, 2)));
    block.push((addr(SENDERS + 2), gtc(2, true, 95, 1)));
    let preexisting = "margin configs: planted load error (test)";
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        for (serial, prior_fault) in [(false, false), (true, false), (false, true)] {
            let what = format!("{mode:?} serial={serial} prior_fault={prior_fault}");
            let dir = tempfile::tempdir().expect("tempdir");
            let mut ctx = new_ctx(&dir, mode);
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup_blocks()[0], true);
            ctx.save_order_books();
            ctx.dirty_books.clear();
            assert!(ctx.fatal_error.is_none(), "{what}");
            ctx.cancel_all_workers = 4;
            ctx.cancel_all_min_visits = if serial { usize::MAX } else { 0 };
            ctx.test_cancel_all_panic_market = Some(PLANT);
            if prior_fault {
                ctx.fatal_error = Some(preexisting.to_string());
            }
            let (fills, next_id) = (ctx.trade_index, ctx.next_global_order_id);

            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, true)
            }))
            .unwrap_or_else(|_| panic!("{what}: the executing thread unwound"));

            if serial {
                assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
                assert_eq!(ctx.test_cancel_all_parallel_runs, 0, "{what}");
                assert!(
                    r.results.iter().all(|r| r.success),
                    "{what}: {:?}",
                    r.results
                );
                assert!(ctx.trade_index > fills, "{what}: the crossing buy filled");
                assert!(ctx.next_global_order_id > next_id, "{what}");
                continue;
            }
            assert_eq!(ctx.test_cancel_all_parallel_runs, 1, "{what}");
            let fatal = ctx.fatal_error.clone().expect("fatal latched");
            if prior_fault {
                assert_eq!(fatal, preexisting, "{what}: the first fault is kept");
            } else {
                assert!(
                    fatal.contains(&format!("market {PLANT}"))
                        && fatal.contains("planted cancel-all worker panic"),
                    "{what}: {fatal}"
                );
            }
            for r in &r.results[..10] {
                assert!(r.action_type == "cancel_all" && !r.success, "{what}: {r:?}");
            }
            // Phase 2 never ran: no fill, no order id assigned, nothing new rests.
            assert_eq!(ctx.trade_index, fills, "{what}");
            assert_eq!(ctx.next_global_order_id, next_id, "{what}");
            for id in next_id..next_id + block.len() as u128 {
                assert!(
                    ctx.order_books.values().all(|b| b.get_order(id).is_none()),
                    "{what}: order {id} rests"
                );
            }
            // The panicking book left the context; the others stayed.
            assert!(ctx.order_book(PLANT).is_none(), "{what}");
            for m in MARKETS.into_iter().filter(|&m| m != PLANT) {
                assert!(ctx.order_book(m).is_some(), "{what}: book {m}");
            }

            // A second batch on the fatal context (the post-EVM batch runs
            // before the committer's check) does not panic either.
            let second = vec![
                (addr(11), cancel_all(None)),
                (addr(12), cancel_all(None)),
                (addr(SENDERS + 3), gtc(PLANT, true, 95, 1)),
            ];
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                NativeExecutor::execute_batch_cancel_mode(&mut ctx, &second, true)
            }))
            .unwrap_or_else(|_| panic!("{what}: the second batch unwound"));
            assert_eq!(
                ctx.fatal_error.as_deref(),
                Some(fatal.as_str()),
                "{what}: latch kept"
            );
        }
    }
}

/// Thread-scoped allocation counters for the Codex s123 flag-off witness
/// below (as torus-state's `native_trie` alloc witness: installed only in
/// this crate's `cfg(test)` lib binary, per THREAD so the harness's other
/// tests do not disturb the count).
mod alloc_count {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ALLOCS: Cell<u64> = const { Cell::new(0) };
    }

    pub struct Counting;

    #[inline]
    fn bump() {
        let _ = ALLOCS.try_with(|c| c.set(c.get().wrapping_add(1)));
    }

    // SAFETY: every method forwards to `System` unchanged; the counter is a
    // thread-local `Cell` with a const initialiser, so it never allocates and
    // never re-enters the allocator.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc_zeroed(l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            bump();
            unsafe { System.realloc(p, l, new) }
        }
    }

    /// Allocations (alloc, alloc_zeroed, realloc) `f` performs ON THIS THREAD.
    pub fn measure<R>(f: impl FnOnce() -> R) -> (u64, R) {
        let a0 = ALLOCS.with(Cell::get);
        let r = f();
        (ALLOCS.with(Cell::get) - a0, r)
    }
}

#[global_allocator]
static COUNTING_ALLOCATOR: alloc_count::Counting = alloc_count::Counting;

/// `books` books where addr(1) and addr(2) each rested one order that has
/// since left the book without the cancel-all index hearing of it (a
/// supported stale entry: the index is a superset), so a run of their two
/// cancel-alls visits every book and removes nothing ("empty hits").
fn stale_index_ctx(dir: &tempfile::TempDir, mode: BookMode, books: u64) -> NativeExecContext {
    let mut ctx = new_ctx(dir, mode);
    let setup: Vec<_> = (1..=books)
        .flat_map(|m| {
            [
                (addr(1), gtc(m, true, 98, 1)),
                (addr(2), gtc(m, false, 110, 1)),
            ]
        })
        .collect();
    NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, true);
    ctx.save_order_books();
    ctx.dirty_books.clear();
    let _ = ctx.cancel_index();
    for book in ctx.order_books.values_mut() {
        assert_eq!(book.cancel_all_many(&[addr(1), addr(2)]).concat().len(), 2);
    }
    ctx.phase_accum = ExecPhaseAccum::default();
    ctx
}

/// The run's own allocations on the flag-OFF serial path, beyond those of
/// the book calls it makes (`take_pending_stops` per member, one
/// `cancel_all_many` per book), measured on a twin context.
fn serial_run_overhead_allocs(mode: BookMode, books: u64) -> u64 {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut ctx = stale_index_ctx(&dir_a, mode, books);
    let mut twin = stale_index_ctx(&dir_b, mode, books);
    ctx.cancel_all_workers = 1;
    let run = [(0, addr(1), None), (1, addr(2), None)];
    let (run_allocs, results) =
        alloc_count::measure(|| NativeExecutor::exec_cancel_all_run(&mut ctx, &run));
    let what = format!("{mode:?} books={books}");
    let results = results.unwrap_or_else(|_| panic!("{what}: the serial run failed"));
    assert!(results.iter().all(|r| r.success), "{what}");
    assert_eq!(ctx.test_cancel_all_parallel_runs, 0, "{what}: serial path");
    assert_eq!(
        ctx.phase_accum.cancel_all_books_visited,
        2 * books,
        "{what}"
    );
    assert_eq!(ctx.phase_accum.cancel_all_books_hit, 0, "{what}");

    let mut mids: Vec<MarketId> = twin.order_books.keys().copied().collect();
    mids.sort_unstable();
    let senders = [addr(1), addr(2)];
    let (book_allocs, ()) = alloc_count::measure(|| {
        for m in &mids {
            let book = twin.order_books.get_mut(m).unwrap();
            for s in &senders {
                drop(book.take_pending_stops(s));
            }
            drop(book.cancel_all_many(&senders));
        }
    });
    run_allocs - book_allocs
}

/// Codex s123 review P2 (flag OFF, `TORUS_PARALLEL_CANCEL_ALL=1`): the
/// serial cancel-all run allocates exactly as main — its two scratch
/// buffers once per run, groups walked in place — so its own allocations
/// do not grow with the markets it visits (the flag-off branch allocated
/// two buffers per visited market plus the group list: 300 markets, main
/// 1202 allocations, branch 1808).
#[test]
fn serial_cancel_all_run_allocations_do_not_grow_with_markets() {
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let small = serial_run_overhead_allocs(mode, 60);
        let large = serial_run_overhead_allocs(mode, 300);
        assert_eq!(
            small, large,
            "{mode:?}: the serial run's own allocations grew with its markets (60 books: \
             {small}, 300 books: {large})"
        );
    }
}

/// Codex s123 review P2 (flag ON): the parallel book pass lends the run's
/// books without walking every resident book: 2 targeted markets among
/// 1000 resident books examine 2 map entries (was all 1000).
#[test]
fn parallel_cancel_all_examines_only_the_runs_books() {
    // A sender rests at most 1000 orders per block height.
    const BOOKS: u64 = 1000;
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ctx = new_ctx(&dir, mode);
        let mut setup: Vec<_> = (1..=BOOKS)
            .map(|m| (addr(2), gtc(m, true, 98, 1)))
            .collect();
        setup.extend([
            (addr(1), gtc(7, true, 99, 1)),
            (addr(1), gtc(900, false, 110, 1)),
        ]);
        NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup, true);
        ctx.save_order_books();
        ctx.dirty_books.clear();
        ctx.phase_accum = ExecPhaseAccum::default();
        ctx.cancel_all_workers = 4;
        ctx.cancel_all_min_visits = 0;
        let block = vec![(addr(1), cancel_all(None)), (addr(3), cancel_all(None))];
        let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, true);
        let what = format!("{mode:?}");
        assert!(
            r.results.iter().all(|r| r.success),
            "{what}: {:?}",
            r.results
        );
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        assert_eq!(
            ctx.test_cancel_all_parallel_runs, 1,
            "{what}: the run used the pool"
        );
        assert_eq!(
            ctx.test_cancel_all_parallel_books_examined, 2,
            "{what}: books examined to lend the run's 2 books"
        );
        assert_eq!(ctx.phase_accum.cancel_all_books_visited, 2, "{what}");
        assert_eq!(ctx.phase_accum.cancel_all_books_hit, 2, "{what}");
        assert_eq!(
            ctx.order_books.len() as u64,
            BOOKS,
            "{what}: every book stays"
        );
        assert!(ctx
            .order_books
            .values()
            .all(|b| b.open_order_count(&addr(1)) == 0));
        assert!(ctx
            .order_books
            .values()
            .all(|b| b.open_order_count(&addr(2)) == 1));
        let mut dirty: Vec<_> = ctx.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        assert_eq!(dirty, vec![7, 900], "{what}");
        assert!(assert_index_covers_books(&ctx, &what));
    }
}
