//! s118 (Phase 3 step 2 follow-up, review of c88d3073): the funnel label of a
//! pre-book margin reject ([`margin_reject_label`]: `margin_position` vs
//! `margin_reservation`) re-runs the account check with the reservations
//! released. That second check only feeds the reason metric, so it runs
//! only with metrics attached; without them every placement runs exactly one
//! account check. Everything consensus sees is the same either way (every
//! result, the write sets, the state, the running hash and the native
//! root), on the single-action path and the batch engine (serial and
//! sharded Phase 2, the latter asserted to really run).

use super::*;
use std::sync::Arc;
use torus_core::position::NativeBalance;
use torus_state::cf::ALL_CF_NAMES;
use torus_state::running_hash::{
    capture_begin, capture_take, configure_activation, read_running_hash, WriteEntry,
};
use torus_state::spawn_count::{thread_totals, SpawnSite};
use torus_telemetry::Metrics;
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(sender: Address, order: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(order))
}

fn fund<T: StateBackend>(ctx: &NativeExecContext<T>, trader: &Address, amount: i64) {
    let bal = NativeBalance {
        available: fp(amount),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

/// `None` = the single-action path, `Some(t)` = the batch engine with `t`
/// Phase 2 threads (0 = serial).
type Mode = Option<usize>;
const MODES: [Mode; 4] = [None, Some(0), Some(2), Some(4)];

fn exec<T: StateBackend>(
    ctx: &mut NativeExecContext<T>,
    mode: Mode,
    block: &[(Address, NativeAction)],
) -> Vec<NativeActionResult> {
    let r: Vec<_> = match mode {
        None => block
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    r
}

/// Account checks run so far: on this thread, and on the sharded Phase 2
/// workers it spawned. Phase 2 threads spawned by this thread.
fn probe() -> [u64; 3] {
    [
        ACCOUNT_CHECKS.with(|c| c.get()),
        WORKER_ACCOUNT_CHECKS.with(|c| c.get()),
        thread_totals()[SpawnSite::MarginPrepare as usize],
    ]
}

/// What a client and the action status see of a result.
type Seen = (bool, FailureReason, Option<String>);

/// Everything consensus sees of one block, read after its flush.
#[derive(PartialEq)]
struct BlockPrint {
    /// Every result, all fields (`Debug`).
    results: Vec<String>,
    /// The running hash `(height, h_n)`.
    running_hash: Option<(u64, [u8; 32])>,
    /// The native state root.
    root: B256,
    /// Every row of every column family.
    dump: Vec<(&'static str, Vec<u8>, Vec<u8>)>,
    next_global_order_id: u128,
    trade_index: u32,
}

struct Run {
    blocks: Vec<BlockPrint>,
    /// The hashed write set of every flushed height.
    writes: Vec<(u64, Vec<WriteEntry>)>,
    /// The reject block's results.
    seen: Vec<Seen>,
    /// The reject block's account checks: `[this thread, Phase 2 workers]`.
    checks: [u64; 2],
    /// Phase 2 worker threads the reject block spawned.
    spawns: u64,
    /// The non-zero margin reason series.
    series: Vec<(&'static str, u64)>,
}

/// Senders of the reject block: four, so 2 and 4 Phase 2 threads shard it
/// (2 resp. 4 workers).
const SENDERS: u64 = 4;
/// Placements in the reject block: one account check each.
const PLACEMENTS: u64 = 7;
/// Margin rejects in the reject block: each runs the label's second check
/// with metrics on.
const MARGIN_REJECTS: u64 = 4;

/// The reject scenario of `reject_reason_metrics_tests`'
/// `margin_rejects_split_position_vs_reservation_on_every_path` (market 3,
/// 20x, tick / lot 1), one overlay block per height, flushed with the
/// running hash on. Setup (heights 1-3): P (60) buys 5 @100 from the maker,
/// then rests a bid 2 @50. Height 4, one block of four senders,
/// interleaved: P bids 13 @50 (reservation) and 20 @50 (position); U (15)
/// bids 4, 4, 8 @50 (the first rests; reservation, position); V and W
/// (funded) rest a bid 1 @40 and an ask 1 @200. Every reject has a
/// reservation, so with metrics each runs the label's second check.
fn run(mode: Mode, metrics: bool) -> Run {
    let (mk, p, u, v, w) = (addr(1), addr(6), addr(7), addr(8), addr(9));
    let blocks = [
        vec![place(mk, gtc(3, false, 100, 5))],
        vec![place(p, gtc(3, true, 100, 5))],
        vec![place(p, gtc(3, true, 50, 2))],
        vec![
            place(p, gtc(3, true, 50, 13)),
            place(u, gtc(3, true, 50, 4)),
            place(v, gtc(3, true, 40, 1)),
            place(p, gtc(3, true, 50, 20)),
            place(u, gtc(3, true, 50, 4)),
            place(w, gtc(3, false, 200, 1)),
            place(u, gtc(3, true, 50, 8)),
        ],
    ];
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    configure_activation(&db, Some(1)).unwrap();
    capture_begin(&db);
    let m = Arc::new(Metrics::new());
    let mut books: HashMap<MarketId, OrderBook> = HashMap::default();
    let mut next_id: u128 = 1;
    let mut prints = Vec::new();
    let (mut seen, mut checks, mut spawns) = (Vec::new(), [0; 2], 0);
    for (i, block) in blocks.iter().enumerate() {
        let h = i as u64 + 1;
        let overlay = NativeStateOverlay::new(db.clone());
        let mut ctx = NativeExecContext::new(
            overlay.clone(),
            h,
            1000,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
        );
        ctx.set_order_books(std::mem::take(&mut books));
        ctx.next_global_order_id = next_id;
        if metrics {
            ctx.metrics = Some(m.clone());
        }
        if h == 1 {
            for (t, amount) in [
                (mk, 1_000_000),
                (p, 60),
                (u, 15),
                (v, 1_000_000),
                (w, 1_000_000),
            ] {
                fund(&ctx, &t, amount);
            }
        }
        let before = probe();
        let r = exec(&mut ctx, mode, block);
        let after = probe();
        if h == blocks.len() as u64 {
            seen = r
                .iter()
                .map(|r| (r.success, r.reason, r.error.clone().map(|e| e.into_owned())))
                .collect();
            checks = [after[0] - before[0], after[1] - before[1]];
            spawns = after[2] - before[2];
        } else {
            assert!(r.iter().all(|r| r.success), "{mode:?} block {h}: {r:?}");
        }
        ctx.save_order_books();
        books = ctx.take_order_books();
        next_id = ctx.next_global_order_id;
        let trade_index = ctx.trade_index;
        drop(ctx);
        overlay
            .freeze(h)
            .flush_with_native_trie_stats(&db, Some(h), None, None)
            .unwrap();
        let dump = ALL_CF_NAMES
            .iter()
            .flat_map(|cf| {
                StateBackend::iterate_cf(&db, cf, None)
                    .unwrap()
                    .into_iter()
                    .map(move |(k, v)| (*cf, k, v))
            })
            .collect();
        prints.push(BlockPrint {
            results: r.iter().map(|r| format!("{r:?}")).collect(),
            running_hash: read_running_hash(&db),
            root: crate::state_root::compute_native_state_root(&db).unwrap(),
            dump,
            next_global_order_id: next_id,
            trade_index,
        });
    }
    let writes = capture_take(&db);
    assert_eq!(
        writes.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
        [1, 2, 3, 4],
        "{mode:?}: one write set per height"
    );
    assert!(
        writes.iter().all(|(_, w)| !w.is_empty()),
        "{mode:?}: every block writes"
    );
    let series = [
        torus_telemetry::OrderRejectReason::MarginPosition,
        torus_telemetry::OrderRejectReason::MarginReservation,
    ]
    .iter()
    .map(|r| (r.as_str(), m.orders_rejected_reason[*r as usize].get()))
    .filter(|(_, n)| *n > 0)
    .collect();
    Run {
        blocks: prints,
        writes,
        seen,
        checks,
        spawns,
        series,
    }
}

/// `a` and `b` are the same to consensus, block by block.
fn assert_same(a: &Run, b: &Run, what: &str) {
    assert_eq!(a.blocks.len(), b.blocks.len(), "{what}");
    for (i, (x, y)) in a.blocks.iter().zip(&b.blocks).enumerate() {
        let h = i + 1;
        assert_eq!(x.results, y.results, "{what} height {h}: results");
        assert!(x.dump == y.dump, "{what} height {h}: state dump");
        assert_eq!(
            x.running_hash, y.running_hash,
            "{what} height {h}: running hash"
        );
        assert!(
            x.running_hash.is_some_and(|(at, _)| at == h as u64),
            "{what} height {h}"
        );
        assert_eq!(x.root, y.root, "{what} height {h}: native root");
        assert_eq!(
            x.next_global_order_id, y.next_global_order_id,
            "{what} height {h}: next order id"
        );
        assert_eq!(
            x.trade_index, y.trade_index,
            "{what} height {h}: trade index"
        );
    }
    assert!(a.writes == b.writes, "{what}: write sets");
    assert_eq!(a.seen, b.seen, "{what}: reject block results");
}

/// The reject block's checks ran where `mode` puts Phase 2: on this thread
/// (single path, serial engine) or all on the sharded workers, `min(t, 4)`
/// of them (2 and 4 threads: never the serial fallback).
fn assert_checks(r: &Run, mode: Mode, total: u64) {
    match mode {
        None | Some(0) => {
            assert_eq!(r.spawns, 0, "{mode:?}: no Phase 2 worker");
            assert_eq!(
                r.checks,
                [total, 0],
                "{mode:?}: checks [this thread, workers]"
            );
        }
        Some(t) => {
            assert_eq!(
                r.spawns,
                (t as u64).min(SENDERS),
                "{mode:?}: sharded Phase 2 workers"
            );
            assert_eq!(
                r.checks,
                [0, total],
                "{mode:?}: every check on a Phase 2 worker (no serial fallback)"
            );
        }
    }
}

/// Metrics off: one account check per placement, none for the label; the
/// block's results (reason and message included), write sets, state,
/// running hash and native root are the ones with metrics on, on every
/// path; the sharded engine also equals the serial one.
#[test]
fn metrics_off_margin_reject_runs_no_label_check() {
    let mut serial: Option<Run> = None;
    for mode in MODES {
        let off = run(mode, false);
        assert_checks(&off, mode, PLACEMENTS);
        let on = run(mode, true);
        assert_same(&off, &on, &format!("{mode:?} metrics off vs on"));
        let rejects: Vec<_> = off.seen.iter().filter(|s| !s.0).collect();
        assert_eq!(
            rejects.len() as u64,
            MARGIN_REJECTS,
            "{mode:?}: {:?}",
            off.seen
        );
        assert!(
            rejects
                .iter()
                .all(|s| s.1 == FailureReason::Margin && s.2.is_some()),
            "{mode:?}: {:?}",
            off.seen
        );
        match (mode, &serial) {
            (Some(0), _) => serial = Some(off),
            (Some(_), Some(s)) => assert_same(s, &off, &format!("{mode:?} vs serial engine")),
            _ => {}
        }
    }
}

/// Metrics on: the label's second check runs once per margin reject (4 on
/// top of the 7), on the sharded workers too, and splits the rejects as
/// before: 2 position, 2 reservation.
#[test]
fn metrics_on_margin_reject_label_splits_as_before() {
    for mode in MODES {
        let on = run(mode, true);
        assert_checks(&on, mode, PLACEMENTS + MARGIN_REJECTS);
        assert_eq!(
            on.series,
            vec![("margin_position", 2), ("margin_reservation", 2)],
            "{mode:?}"
        );
    }
}
