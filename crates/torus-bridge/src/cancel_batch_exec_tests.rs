//! s63 `TORUS_CANCEL_BATCH` executor differential: the batched Phase-1
//! cancel-all runs must produce byte-identical results, gas, persisted state
//! and native state root to the per-action loop.

use super::*;
use crate::state_root::compute_native_state_root;
use alloy_primitives::U256;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
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
                5 => cancel_all(Some(77)),
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

fn new_ctx(dir: &tempfile::TempDir, mode: BookMode) -> NativeExecContext {
    let db = StateDb::open(dir.path()).expect("open db");
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
        None,
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
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx(&dir, mode);
    ctx.test_cancel_all_full_scan = full_scan;
    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    let mut dirty = Vec::new();
    for (b, block) in blocks.iter().enumerate() {
        let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, block, batch);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        // P2-1: the index (once built) covers the books after every block.
        assert_index_covers_books(&ctx, &format!("{mode:?} batch={batch} block {b}"));
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

/// One long run: every sender cancels everything, twice, with places mixed
/// in — books empty out, then the run's places rest on fresh levels.
#[test]
fn cancel_batch_flag_on_matches_off_long_run_emptying_books() {
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
                (addr(3), cancel_all(Some(77))),
            ];
            NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, batch);
            let a = ctx.phase_accum;
            let what = format!("{mode:?} batch={batch}");
            assert_eq!(a.cancel_alls, 5, "{what}");
            // 2 + 1 + 0 + 0 + 0: addr(3) has nothing in market 4 (and market
            // 77 has no book); the repeat finds its sender's entry pruned.
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

/// P2-1 invariant (plan section 3): every (trader, market) where the book
/// holds an order, a stop or a reduce-only entry of the trader's is listed
/// by the cancel-all index, once built (a superset is allowed). Returns
/// whether the index was built.
fn assert_index_covers_books(ctx: &NativeExecContext, what: &str) -> bool {
    let (carried, rebuilt) = ctx.trader_index();
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

/// P2-1 blocks over the setup: places resting across 12 books, crossing
/// places in the deep books (partial fills, rests), stop-limits near the
/// touch (crossing trades fire them into resting orders: buy limits below
/// the asks, sell limits above the bids), modifies (quantity, and a new
/// price: re-inserted at the back), cancels by id, cancel-alls `None` /
/// `Some(m)` / unknown market, a sender's second cancel-all right after the
/// first, a transfer breaking runs.
fn index_blocks(seed: u64, resting: &[(Address, u128)]) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let mut blocks = Vec::new();
    for _ in 0..4 {
        let mut block = Vec::new();
        for _ in 0..150 {
            let mut s = addr(1 + rng.below(SENDERS as u64 + 4) as u8);
            let m = 1 + rng.below(INDEX_MARKETS);
            let deep = MARKETS[rng.below(4) as usize];
            let (owner, id) = resting[rng.below(resting.len() as u64) as usize];
            let is_buy = rng.below(2) == 0;
            let action = match rng.below(24) {
                0..=3 => cancel_all(None),
                4 | 5 => cancel_all(Some(m)),
                6 => cancel_all(Some(77)),
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
        blocks.extend(index_blocks(seed, &resting));
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
        blocks.extend(index_blocks(21, &resting));
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
