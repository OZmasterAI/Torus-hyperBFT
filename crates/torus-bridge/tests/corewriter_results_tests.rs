//! s112 (owner option b): every CoreWriter drain result reaches the v4 block
//! action status record. `NativeExecutor::drain_core_writer_entries` returns
//! one entry per drained queue row, in queue order, with the row's identity
//! (queue sequence, kind, trader, queued block) next to its result: a
//! success, an undecodable row, the one-block delay guard, a conversion
//! failure (stop order type), a failed lockbox withdraw and a cancel of a
//! missing order. The record and the RPC are proven in torus-consensus
//! (`app.rs`) and torus-rpc.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::action_status::CoreWriterKind;
use torus_state::cf::CF_CORE_WRITER_QUEUE;
use torus_state::{StateBackend, StateDb};
use torus_types::FixedPoint;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    // Pre-testnet P0: actions need a listed market (placeholder rows).
    for m in [1u64, 7] {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .unwrap();
    }
    // Item 7 step 2: CoreWriter order delay K = 1 (these rows are filed under block 2).
    db.put_cf_raw(
        torus_state::cf::CF_FEE_CONFIG,
        b"core_writer_order_delay",
        b"1",
    )
    .unwrap();
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn enqueue(db: &StateDb, trader: u8, kind: QueuedActionKind, block_queued: u64) -> u64 {
    CoreWriterQueue::enqueue(
        db,
        &QueuedAction {
            trader: addr(trader),
            kind,
            block_queued,
        },
    )
    .unwrap()
}

fn key(block: u64, seq: u64) -> Vec<u8> {
    let mut k = block.to_be_bytes().to_vec();
    k.extend_from_slice(&seq.to_be_bytes());
    k
}

/// Block 2's queue, six rows with mixed outcomes:
/// 0 lockbox deposit (executes), 1 undecodable, 2 a CancelAll whose row says
/// it was queued in block 2 itself (delay guard), 3 a stop PlaceOrder (does
/// not convert), 4 a lockbox withdraw without native balance, 5 a cancel of
/// an order that does not exist.
fn seed(db: &StateDb) {
    assert_eq!(
        enqueue(db, 7, QueuedActionKind::LockboxDeposit { amount: fp(5) }, 1),
        0
    );
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(2, 1), &[0xFF, 0x00, 0x01])
        .unwrap();
    // Queued "in block 2": enqueue files it under block 3; move it to (2, 2).
    assert_eq!(
        enqueue(db, 12, QueuedActionKind::CancelAll { market_id: 1 }, 2),
        0
    );
    let bytes = db
        .get_cf_raw(CF_CORE_WRITER_QUEUE, &key(3, 0))
        .unwrap()
        .unwrap();
    db.delete_cf_raw(CF_CORE_WRITER_QUEUE, &key(3, 0)).unwrap();
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(2, 2), &bytes)
        .unwrap();
    // Item 7 step 2: the moved row's target is 2, so the last order target is too
    // (else the order kinds below would be held at 3, never before it).
    db.put_cf_raw(
        torus_state::cf::CF_FEE_CONFIG,
        torus_core::precompiles::CORE_WRITER_LAST_ORDER_TARGET_KEY,
        &2u64.to_be_bytes(),
    )
    .unwrap();
    let stop = QueuedActionKind::PlaceOrder {
        market_id: 1,
        side: 0,
        order_type: 2,
        price: fp(100),
        quantity: fp(1),
        time_in_force: 0,
        cloid: None,
    };
    assert_eq!(enqueue(db, 11, stop, 1), 3);
    assert_eq!(
        enqueue(
            db,
            9,
            QueuedActionKind::LockboxWithdraw { amount: fp(100) },
            1
        ),
        4
    );
    assert_eq!(
        enqueue(
            db,
            10,
            QueuedActionKind::CancelOrder { order_id: 12_345 },
            1
        ),
        5
    );
}

fn ctx(db: &StateDb) -> NativeExecContext<StateDb> {
    NativeExecContext::new(
        db.clone(),
        2,
        1_002,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

/// RED before: the entries carried no identity (seq 0, kind unknown, zero
/// trader) for every row.
#[test]
fn drain_entries_carry_each_rows_identity_and_result_in_queue_order() {
    let (_dir, db) = open_test_db();
    seed(&db);
    let mut ctx = ctx(&db);
    let entries = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
    assert_eq!(ctx.take_fatal_error(), None);
    let got: Vec<_> = entries
        .iter()
        .map(|e| (e.seq, e.kind, e.trader, e.queued_block, e.result.success))
        .collect();
    use CoreWriterKind as K;
    assert_eq!(
        got,
        vec![
            (0, K::LockboxDeposit, addr(7), 1, true),
            (1, K::Undecodable, Address::ZERO, 0, false),
            (2, K::CancelAll, addr(12), 2, false),
            (3, K::PlaceOrder, addr(11), 1, false),
            (4, K::LockboxWithdraw, addr(9), 1, false),
            (5, K::CancelOrder, addr(10), 1, false),
        ]
    );
    let msg = |i: usize| {
        entries[i]
            .result
            .error
            .as_deref()
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        msg(1).starts_with("undecodable queued action (block 2, seq 1): "),
        "{}",
        msg(1)
    );
    assert_eq!(msg(2), "action queued in block 2 cannot execute in block 2");
    assert!(
        msg(3).contains("unsupported CoreWriter order_type 2"),
        "{}",
        msg(3)
    );
    assert!(!msg(4).is_empty() && !msg(5).is_empty());
    assert!(
        StateBackend::iterate_cf(&db, CF_CORE_WRITER_QUEUE, None)
            .unwrap()
            .is_empty(),
        "every row of block 2 deleted"
    );
}

/// `drain_core_writer` is the entries' results, unchanged.
#[test]
fn drain_core_writer_is_the_entries_results() {
    let run = |entries: bool| {
        let (dir, db) = open_test_db();
        seed(&db);
        let mut ctx = ctx(&db);
        let results: Vec<_> = if entries {
            NativeExecutor::drain_core_writer_entries(&mut ctx)
                .unwrap()
                .into_iter()
                .map(|e| e.result)
                .collect()
        } else {
            NativeExecutor::drain_core_writer(&mut ctx).unwrap()
        };
        drop(ctx);
        let out: Vec<_> = results
            .into_iter()
            .map(|r| {
                (
                    r.action_type,
                    r.success,
                    r.reason,
                    r.error.map(String::from),
                )
            })
            .collect();
        (dir, out)
    };
    assert_eq!(run(true).1, run(false).1);
}

fn fund(db: &StateDb, trader: u8) {
    torus_core::position::PositionManager::new(db.clone())
        .put_native_balance(
            &addr(trader),
            &torus_core::position::NativeBalance {
                available: fp(1_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
}

fn cw_order(side: u8, price: i64, time_in_force: u8) -> QueuedActionKind {
    QueuedActionKind::PlaceOrder {
        market_id: 1,
        side,
        order_type: 0,
        price: fp(price),
        quantity: fp(1),
        time_in_force,
        cloid: None,
    }
}

/// s114 (CoreWriter follow-up 2): an executed CoreWriter placement reports
/// the order id its order got: a GTC buy that rests (the id of its resting
/// order), an IOC buy that fills (the id the fill's taker order carries,
/// the next one). A rejected placement (IOC with nothing to fill) and every
/// other row report none. A native sell rests first at 100.
#[test]
fn drain_entries_report_the_order_id_each_placement_got() {
    let (_dir, db) = open_test_db();
    for t in [20, 21, 22] {
        fund(&db, t);
    }
    assert_eq!(enqueue(&db, 21, cw_order(0, 90, 0), 1), 0); // GTC buy 90: rests
    assert_eq!(enqueue(&db, 22, cw_order(0, 100, 1), 1), 1); // IOC buy 100: fills
    assert_eq!(enqueue(&db, 22, cw_order(0, 80, 1), 1), 2); // IOC buy 80: rejected
    assert_eq!(
        enqueue(&db, 21, QueuedActionKind::CancelAll { market_id: 7 }, 1),
        3
    );
    let mut ctx = ctx(&db);
    let maker = NativeExecutor::execute(
        &mut ctx,
        &addr(20),
        &torus_types::NativeAction::PlaceOrder(torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price: fp(100),
            quantity: fp(1),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    );
    assert!(maker.success, "{:?}", maker.error);
    let maker_id = ctx.order_books()[&1].orders_for_trader(&addr(20))[0].id;

    let entries = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
    assert_eq!(ctx.take_fatal_error(), None);
    let got: Vec<_> = entries
        .iter()
        .map(|e| (e.seq, e.result.success, e.oid))
        .collect();
    let book = &ctx.order_books()[&1];
    let resting = book.orders_for_trader(&addr(21));
    assert_eq!(resting.len(), 1, "the GTC buy rests");
    let rest_id = resting[0].id;
    assert!(rest_id > maker_id);
    assert_eq!(
        got,
        vec![
            (0, true, Some(rest_id)),
            (1, true, Some(rest_id + 1)),
            (2, false, None),
            (3, true, None),
        ]
    );
    assert!(
        book.get_order(maker_id).is_none() && book.get_order(rest_id + 1).is_none(),
        "the IOC filled against the native sell and rests nowhere"
    );
}

/// Item 7 T (question 9 a): the anti-MEV delay guard is per seq range under
/// `Feature::SameBlockCoreWriter`. A same-block-range row (seq < 200) of a
/// non-order kind may drain in the block that queued it (it runs after every
/// native batch of that block); an order kind there, any row of the order
/// range (seq >= 200) and any row from a later block are still refused. With
/// the bit off nothing queued at the drain height runs (as before).
#[test]
fn t_drain_guard_is_per_seq_range() {
    let order_base = torus_types::CORE_WRITER_ORDER_SEQ_BASE;
    let row = |db: &StateDb, seq: u64, kind: QueuedActionKind, queued: u64, trader: u8| {
        let bytes = borsh::to_vec(&QueuedAction {
            trader: addr(trader),
            kind,
            block_queued: queued,
        })
        .unwrap();
        db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(2, seq), &bytes).unwrap();
    };
    let guard = "action queued in block 2 cannot execute in block 2";
    for t_on in [false, true] {
        let (_dir, db) = open_test_db();
        // Same-block range: a deposit of block 2 (the T case), a CancelAll of
        // block 2 (an order kind: never same-block), a deposit of block 3.
        row(&db, 0, QueuedActionKind::LockboxDeposit { amount: fp(5) }, 2, 7);
        row(&db, 1, QueuedActionKind::CancelAll { market_id: 1 }, 2, 12);
        row(&db, 2, QueuedActionKind::LockboxDeposit { amount: fp(1) }, 3, 8);
        // Order range: a deposit of block 2 (wrong range), a cancel of block 1.
        row(&db, order_base, QueuedActionKind::LockboxDeposit { amount: fp(2) }, 2, 9);
        row(&db, order_base + 1, QueuedActionKind::CancelOrder { order_id: 1 }, 1, 10);
        let mut ctx = ctx(&db);
        if t_on {
            ctx.features = torus_types::Feature::SameBlockCoreWriter.bit();
        }
        let entries = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
        assert_eq!(ctx.take_fatal_error(), None);
        let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, order_base, order_base + 1], "key order");
        let msg = |i: usize| entries[i].result.error.clone().unwrap_or_default();
        assert_eq!(entries[0].result.success, t_on, "t_on={t_on}: same-block deposit");
        if !t_on {
            assert_eq!(msg(0), guard);
        }
        assert_eq!(msg(1), guard, "t_on={t_on}: an order kind is never same-block");
        assert_eq!(
            msg(2),
            "action queued in block 3 cannot execute in block 2",
            "t_on={t_on}: a later block's row"
        );
        assert_eq!(msg(3), guard, "t_on={t_on}: the order range keeps h + 1");
        assert_ne!(msg(4), guard, "t_on={t_on}: an earlier block's order runs");
        let credited = torus_core::position::PositionManager::new(db.clone())
            .get_native_balance(&addr(7))
            .unwrap()
            .available;
        assert_eq!(credited, if t_on { fp(5) } else { FixedPoint::ZERO });
    }
}

/// Codex P2 (s139): a CoreWriter placement refused before the book (off
/// tick) records its `orderUpdates` rejection with the row's parameters, as
/// a native placement does; nothing while recording is off.
#[test]
fn drained_pre_book_rejection_records_an_order_event() {
    use torus_state::order_events::{OrderEventReason, OrderEventStatus};
    for record in [true, false] {
        let (_dir, db) = open_test_db();
        fund(&db, 21);
        let mut off_tick = cw_order(0, 100, 0);
        if let QueuedActionKind::PlaceOrder { price, cloid, .. } = &mut off_tick {
            *price = FixedPoint::from_raw(100 * FixedPoint::SCALE + FixedPoint::SCALE / 2);
            *cloid = Some(0x77);
        }
        enqueue(&db, 21, off_tick, 1);
        let mut ctx = ctx(&db);
        ctx.set_order_events(record);
        let entries = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].result.success);
        assert_eq!(
            entries[0].result.reason,
            torus_state::action_status::FailureReason::Tick
        );
        let events = ctx.take_order_events();
        if !record {
            assert!(events.is_empty(), "{events:?}");
            continue;
        }
        assert_eq!(events.len(), 1, "{events:?}");
        let e = &events[0];
        assert_eq!(e.status, OrderEventStatus::Rejected);
        assert_eq!(
            e.reason,
            OrderEventReason::Reject(torus_state::action_status::FailureReason::Tick)
        );
        assert_eq!((e.trader, e.market, e.order_id, e.cloid), (addr(21), 1, None, Some(0x77)));
        assert_eq!(e.price, FixedPoint::from_raw(100 * FixedPoint::SCALE + FixedPoint::SCALE / 2));
        assert_eq!(e.orig_qty, fp(1));
    }
}
