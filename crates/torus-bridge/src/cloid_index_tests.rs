//! s114 follow-up: the cross-market duplicate-cloid check reads a node-local
//! `(trader, cloid) -> (market, order id)` index ([`CloidMarkets`]) instead
//! of probing every book:
//! - one check probes at most one book, whatever the market count (300
//!   here), on the single path and the batch pre-pass; a placement without
//!   a cloid runs no check;
//! - a cloid live in a market the block never touches is found after a
//!   reload (cold start, every book mode, resident books on and off);
//! - the index carried with the resident books equals the one a load would
//!   build after every block, whatever removed the cloids (fill, cancel by
//!   id / cloid, stop cancel, cancel-all, self-trade cancel, a triggered
//!   stop, modify);
//! - replicas, a resident run, a non-resident run and a restart agree on
//!   every result and the state root.

use super::*;
use crate::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

/// Above u64::MAX.
const C: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0001;
const MARKETS: u64 = 300;

const MODES: [BookMode; 4] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn params(
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    ot: OrderType,
    cloid: Option<u128>,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: ot,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn buy(market_id: MarketId, price: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(params(market_id, true, price, OrderType::Limit, cloid))
}

fn sell(market_id: MarketId, price: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(params(market_id, false, price, OrderType::Limit, cloid))
}

/// A buy stop far above the market.
fn stop(market_id: MarketId, trigger: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(params(
        market_id,
        true,
        trigger + 5,
        OrderType::StopMarket {
            trigger: fp(trigger),
        },
        cloid,
    ))
}

fn by_cloid(market_id: MarketId, cloid: u128) -> NativeAction {
    NativeAction::CancelByCloid { market_id, cloid }
}

fn ctx_at(
    db: &StateDb,
    height: u64,
    mode: BookMode,
    holder: Option<&mut ResidentBooks>,
) -> NativeExecContext {
    let ctx = NativeExecContext::new_with_mode(
        db.clone(),
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        holder,
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    ctx
}

fn fund(ctx: &NativeExecContext, n: u8) {
    let bal = NativeBalance {
        available: fp(100_000_000),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Single,
    Batch,
    BatchCancelMode,
}

const PATHS: [Path; 3] = [Path::Single, Path::Batch, Path::BatchCancelMode];

fn run(
    ctx: &mut NativeExecContext,
    path: Path,
    block: &[(Address, NativeAction)],
) -> Vec<NativeActionResult> {
    match path {
        Path::Single => block
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Path::Batch => NativeExecutor::execute_batch(ctx, block).results,
        Path::BatchCancelMode => {
            NativeExecutor::execute_batch_cancel_mode(ctx, block, true).results
        }
    }
}

fn run1(
    ctx: &mut NativeExecContext,
    path: Path,
    sender: Address,
    action: NativeAction,
) -> NativeActionResult {
    run(ctx, path, &[(sender, action)]).remove(0)
}

fn ok(r: &NativeActionResult, what: &str) {
    assert!(r.success, "{what}: {r:?}");
}

fn dup(r: &NativeActionResult, what: &str) {
    assert!(
        !r.success,
        "{what}: expected a duplicate-cloid reject, got {r:?}"
    );
    assert_eq!(r.reason, FailureReason::DuplicateCloid, "{what}: {r:?}");
}

/// `addr(1)`'s cloid `C + m` rests in market m (odd m) or is a pending stop
/// there (even m), for m = 1..=300.
fn setup_300(ctx: &mut NativeExecContext, path: Path) {
    let block: Vec<_> = (1..=MARKETS)
        .map(|m| {
            let c = Some(C + m as u128);
            let a = if m % 2 == 1 {
                buy(m, 100, c)
            } else {
                stop(m, 900, c)
            };
            (addr(1), a)
        })
        .collect();
    for (i, r) in run(ctx, path, &block).iter().enumerate() {
        ok(r, &format!("{path:?} setup market {}", i + 1));
    }
}

/// One duplicate check probes one book at 300 markets; a miss probes none;
/// no cloid, no check. (The pre-index scan probed all 300 books.)
#[test]
fn one_book_probe_per_check_at_300_markets() {
    for path in PATHS {
        let what = format!("{path:?}");
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut ctx = ctx_at(&db, 1, BookMode::Classic, None);
        fund(&ctx, 1);
        fund(&ctx, 2);
        setup_300(&mut ctx, path);
        assert_eq!(ctx.order_books.len(), MARKETS as usize);
        let (carried, rebuilt) = ctx.cloid_index();
        assert_eq!(rebuilt.len(), MARKETS as usize);
        assert_eq!(
            carried,
            Some(rebuilt),
            "{what}: built at the first check, fed by every add"
        );

        for (m, live_in) in [(300, 1u64), (1, 150), (77, 299), (150, 300)] {
            ctx.phase_accum = ExecPhaseAccum::default();
            let r = run1(
                &mut ctx,
                path,
                addr(1),
                sell(m, 5_000, Some(C + live_in as u128)),
            );
            dup(
                &r,
                &format!("{what}: cloid of market {live_in} placed in {m}"),
            );
            assert!(
                r.error
                    .as_deref()
                    .unwrap_or("")
                    .contains(&format!("in market {live_in} ")),
                "{what}: {r:?}"
            );
            let a = ctx.phase_accum;
            assert_eq!(
                (a.cloid_checks, a.cloid_books_probed),
                (1, 1),
                "{what}: hit in {live_in}"
            );
        }

        ctx.phase_accum = ExecPhaseAccum::default();
        ok(
            &run1(&mut ctx, path, addr(1), buy(5, 90, Some(C + 1_000))),
            &what,
        );
        ok(
            &run1(&mut ctx, path, addr(2), buy(5, 90, Some(C + 1))),
            &format!("{what}: another trader"),
        );
        let a = ctx.phase_accum;
        assert_eq!(
            (a.cloid_checks, a.cloid_books_probed),
            (2, 0),
            "{what}: misses probe nothing"
        );

        ctx.phase_accum = ExecPhaseAccum::default();
        ok(&run1(&mut ctx, path, addr(1), buy(6, 90, None)), &what);
        assert_eq!(
            ctx.phase_accum.cloid_checks, 0,
            "{what}: no cloid, no check"
        );
    }
}

/// Block 1 rests / stores cloids in 300 markets and saves. Block 2 is a
/// fresh context (cold start: the books come from the DB; with resident
/// books the holder is empty after a restart): a cloid of a market block 2
/// never touches is still a duplicate, found with one probe; a cancel frees
/// it. Every book mode, resident books on and off.
#[test]
fn a_cloid_in_an_untouched_market_is_found_after_a_reload() {
    for mode in MODES {
        for resident in [false, true] {
            let what = format!("{mode:?} resident={resident}");
            let dir = tempfile::tempdir().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            let mut holder = ResidentBooks::default();
            let mut ctx = ctx_at(&db, 1, mode, resident.then_some(&mut holder));
            fund(&ctx, 1);
            setup_300(&mut ctx, Path::Batch);
            ctx.save_order_books();
            drop(ctx);

            // Restart: a new holder (empty), a new context.
            let mut holder = ResidentBooks::default();
            let mut ctx = ctx_at(&db, 2, mode, resident.then_some(&mut holder));
            assert_eq!(
                ctx.order_books.len(),
                MARKETS as usize,
                "{what}: every book loaded"
            );
            for (live_in, path) in [
                (1u64, Path::Single),
                (2, Path::Batch),
                (299, Path::BatchCancelMode),
                (300, Path::Single),
            ] {
                ctx.phase_accum = ExecPhaseAccum::default();
                dup(
                    &run1(
                        &mut ctx,
                        path,
                        addr(1),
                        sell(7, 5_000, Some(C + live_in as u128)),
                    ),
                    &format!("{what}: cloid of market {live_in}, {path:?}"),
                );
                assert_eq!(ctx.phase_accum.cloid_books_probed, 1, "{what}");
            }
            ok(
                &run1(&mut ctx, Path::Batch, addr(1), by_cloid(300, C + 300)),
                &what,
            );
            ok(
                &run1(
                    &mut ctx,
                    Path::Single,
                    addr(1),
                    sell(7, 5_000, Some(C + 300)),
                ),
                &what,
            );
        }
    }
}

/// Blocks exercising every executor path that removes a cloid. Market 50+
/// keeps a few live ones.
fn removal_blocks() -> Vec<Vec<(Address, NativeAction)>> {
    let (a, b, c, d) = (addr(1), addr(2), addr(3), addr(4));
    let stop_limit = NativeAction::PlaceOrder(params(
        10,
        true,
        90,
        OrderType::StopLimit {
            trigger: fp(105),
            limit: fp(90),
        },
        Some(C + 9),
    ));
    vec![
        vec![
            (a, sell(1, 100, Some(C))),      // filled in block 2
            (a, buy(2, 50, Some(C + 1))),    // cancelled by id
            (a, buy(3, 50, Some(C + 2))),    // cancelled by cloid
            (a, stop(4, 900, Some(C + 3))),  // stop cancelled by id
            (a, stop(5, 900, Some(C + 4))),  // stop cancelled by cloid
            (a, buy(6, 50, Some(C + 5))),    // cancel-all of market 6
            (a, buy(7, 50, Some(C + 6))),    // cancel-all (every market)
            (a, stop(7, 900, Some(C + 7))),  // ... with a stop
            (b, sell(8, 100, Some(C))),      // self-trade cancelled
            (a, buy(9, 50, Some(C + 8))),    // modified (kept)
            (a, stop_limit),                 // triggers, rests under C + 9
            (b, buy(50, 40, Some(C + 1))),   // stays
            (c, stop(51, 900, Some(C + 2))), // stays
        ],
        vec![
            (c, buy(1, 100, None)), // fills a's C
            (a, by_cloid(3, C + 2)),
            (a, by_cloid(5, C + 4)),
            (b, buy(8, 100, None)), // self-trade: b's C goes
            (c, sell(10, 106, None)),
            (d, buy(10, 106, None)), // trade at 106: a's stop fires
        ],
        vec![
            // ids filled in at run time (`with_ids`)
            (a, NativeAction::CancelAllOrders { market_id: Some(6) }),
        ],
        vec![
            (a, NativeAction::CancelAllOrders { market_id: None }),
            // Every cloid of a is free again: reuse in other markets.
            (a, buy(60, 10, Some(C))),
            (a, buy(61, 10, Some(C + 9))),
            (b, sell(62, 9_000, Some(C))),
        ],
    ]
}

/// Block 3 also cancels market 2's order and market 4's stop by id and
/// modifies market 9's order (their ids from the books).
fn with_ids(
    block: usize,
    ctx: &NativeExecContext,
    mut actions: Vec<(Address, NativeAction)>,
) -> Vec<(Address, NativeAction)> {
    if block == 2 {
        let id = |m: MarketId, c: u128| {
            ctx.order_books[&m]
                .order_id_by_cloid(&addr(1), c)
                .unwrap_or_else(|| panic!("cloid {c:#x} live in {m}"))
        };
        actions.push((
            addr(1),
            NativeAction::CancelOrder {
                order_id: id(2, C + 1),
            },
        ));
        actions.push((
            addr(1),
            NativeAction::CancelOrder {
                order_id: id(4, C + 3),
            },
        ));
        actions.push((
            addr(1),
            NativeAction::ModifyOrder {
                order_id: id(9, C + 8),
                new_price: Some(fp(51)),
                new_qty: None,
            },
        ));
    }
    actions
}

/// After every block (saved, stashed), the cloid index carried with the
/// resident books equals the one a load would build: no removal path
/// leaves an entry behind, no add is missing.
#[test]
fn the_carried_index_equals_a_rebuild_after_every_block() {
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        for path in PATHS {
            let what = format!("{mode:?} {path:?}");
            let dir = tempfile::tempdir().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            let mut holder = ResidentBooks::default();
            let ctx = ctx_at(&db, 1, mode, Some(&mut holder));
            for n in 1..=4 {
                fund(&ctx, n);
            }
            drop(ctx);
            let mut sizes = Vec::new();
            for (i, block) in removal_blocks().into_iter().enumerate() {
                let height = i as u64 + 1;
                let mut ctx = ctx_at(&db, height, mode, Some(&mut holder));
                assert_eq!(
                    ctx.resident_rebuilt(),
                    i == 0,
                    "{what}: block {height} reuses the books"
                );
                let block = with_ids(i, &ctx, block);
                for (k, r) in run(&mut ctx, path, &block).iter().enumerate() {
                    ok(r, &format!("{what}: block {height} action {k}"));
                }
                ctx.save_order_books();
                ctx.stash_resident(&mut holder);
                let (carried, rebuilt) = holder.cloid_index().expect("resident books");
                assert_eq!(
                    carried.as_ref(),
                    Some(&rebuilt),
                    "{what}: after block {height}"
                );
                sizes.push(rebuilt.len());
            }
            // 13 placed; block 2 frees 4 (fill, two cancels by cloid, the
            // self-trade cancel; the triggered stop rests under its cloid);
            // block 3 frees 3 (cancel-all of market 6, two cancels by id;
            // the modify keeps its); block 4 frees a's last 4 and places 3.
            assert_eq!(sizes, vec![13, 9, 6, 5], "{what}: live cloids per block");
        }
    }
}

/// Blocks of cloid traffic across markets (batch calls): a duplicate
/// claimed earlier in the call, a cloid free again after last block's fill,
/// a duplicate of last block's stop, a cancel by cloid then reuse.
fn traffic() -> Vec<Vec<(Address, NativeAction)>> {
    let (a, b) = (addr(1), addr(2));
    let mut blocks = Vec::new();
    for k in 0..6u64 {
        let c = C + k as u128;
        let mut block = vec![
            (a, sell(1 + k, 100, Some(c))),        // ok
            (a, buy(20 + k, 50, Some(c))),         // dup: claimed in this call
            (a, stop(40 + k, 900, Some(c + 100))), // ok
            (b, buy(1 + k, 100, None)),            // fills a's c
            (a, buy(60 + k, 50, Some(c - 1))),     // ok: filled last block
            (a, buy(80 + k, 50, Some(c + 99))),    // dup of last block's stop
        ];
        if k >= 2 {
            // Phase 1: the stop of two blocks back, then its cloid again.
            block.push((a, by_cloid(40 + k - 2, c + 98)));
            block.push((a, buy(100 + k, 50, Some(c + 98))));
        }
        blocks.push(block);
    }
    blocks
}

/// `(results, state root)` of [`traffic`]; `resident` keeps a holder;
/// `restart_at`: the holder is dropped before that block (a restart).
fn run_traffic(
    mode: BookMode,
    resident: bool,
    restart_at: Option<usize>,
) -> (Vec<Vec<(bool, FailureReason)>>, B256) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let ctx = ctx_at(&db, 1, mode, None);
    fund(&ctx, 1);
    fund(&ctx, 2);
    drop(ctx);
    let mut out = Vec::new();
    for (i, block) in traffic().into_iter().enumerate() {
        if restart_at == Some(i) {
            holder = ResidentBooks::default();
        }
        let mut ctx = ctx_at(&db, i as u64 + 1, mode, resident.then_some(&mut holder));
        let r = NativeExecutor::execute_batch(&mut ctx, &block);
        out.push(r.results.iter().map(|r| (r.success, r.reason)).collect());
        ctx.save_order_books();
        ctx.stash_resident(&mut holder);
    }
    let ctx = ctx_at(&db, 99, mode, None);
    (out, compute_native_state_root(&ctx.state).unwrap())
}

#[test]
fn replicas_resident_non_resident_and_a_restart_agree() {
    for mode in MODES {
        let what = format!("{mode:?}");
        let one = run_traffic(mode, true, None);
        assert_eq!(one, run_traffic(mode, true, None), "{what}: two replicas");
        assert_eq!(one, run_traffic(mode, false, None), "{what}: non-resident");
        assert_eq!(
            one,
            run_traffic(mode, true, Some(3)),
            "{what}: restart before block 4"
        );
        for (k, block) in one.0.iter().enumerate() {
            let got: Vec<bool> = block.iter().map(|r| r.0).collect();
            let want: &[bool] = match k {
                0 => &[true, false, true, true, true, true],
                1 => &[true, false, true, true, true, false],
                _ => &[true, false, true, true, true, false, true, true],
            };
            assert_eq!(got, want, "{what}: block {k}");
            assert_eq!(block[1].1, FailureReason::DuplicateCloid, "{what}");
            if k > 0 {
                assert_eq!(block[5].1, FailureReason::DuplicateCloid, "{what}");
            }
        }
    }
}
