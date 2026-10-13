//! R24: public reads stop after `TORUS_RPC_MAX_SCAN_ROWS` examined rows, or
//! seek only the rows they answer from. Every test seeds more rows than the
//! budget and checks the rows the request examined
//! (`RpcState::scan_rows_examined`), not only the answer.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use jsonrpsee::types::ErrorObjectOwned;
use tempfile::TempDir;
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
use torus_economics::governance::{Proposal, ProposalStatus, ProposalType};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_state::cf::CF_GOVERNANCE_PROPOSALS;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

use super::*;
use crate::torus::TorusApiServer;
use crate::types::dec_fp;
use crate::{BlockNotifier, RpcServer};

const MODES: [BookMode; 3] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
];

const MAKER: u8 = 0xA1;
/// The maker's resting order with a cloid (market 1).
const CLOID: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0007;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

/// Distinct traders sorting before `addr(MAKER)`.
fn other(i: u32) -> Address {
    let mut a = [0u8; 20];
    a[16..].copy_from_slice(&i.to_be_bytes());
    Address::new(a)
}

fn hex_addr(a: Address) -> String {
    format!("0x{}", hex::encode(a.as_slice()))
}

fn db() -> (TempDir, StateDb) {
    let dir = TempDir::new().unwrap();
    let state = StateDb::open(dir.path()).unwrap();
    (dir, state)
}

fn server(state: &StateDb, max_scan_rows: usize) -> RpcServer {
    let mut s = RpcServer::new(
        state.clone(),
        Arc::new(Mempool::new(state.clone(), MempoolConfig::default())),
        Arc::new(EvmExecutor::new(TORUS_CHAIN_ID)),
        TORUS_CHAIN_ID,
        100,
        BlockNotifier::new(),
    );
    s.set_max_scan_rows(max_scan_rows);
    s
}

fn examined(s: &RpcServer) -> u64 {
    s.state().scan_rows_examined.load(Relaxed)
}

fn assert_scan_limit(e: &ErrorObjectOwned, what: &str) {
    assert_eq!(e.code(), -32005, "{what}: {e:?}");
    assert!(e.message().contains(ENV_MAX_SCAN_ROWS), "{what}: {e:?}");
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, cloid: Option<u128>) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    }
}

/// Books persisted by the real executor save path in `mode`: `orders` (no
/// crossing), every trader funded.
fn seed_books(state: &StateDb, mode: BookMode, orders: Vec<(Address, PlaceOrderParams)>) {
    let mut ctx = NativeExecContext::new_with_mode(
        state.clone(),
        1,
        1_001,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        None,
    );
    assert!(ctx.fatal_error.is_none(), "{mode:?}: {:?}", ctx.fatal_error);
    let mut traders: Vec<Address> = orders.iter().map(|(t, _)| *t).collect();
    traders.sort();
    traders.dedup();
    for t in traders {
        ctx.positions
            .put_native_balance(
                &t,
                &NativeBalance {
                    available: fp(10_000_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
    }
    let block: Vec<_> = orders
        .into_iter()
        .map(|(t, p)| (t, NativeAction::PlaceOrder(p)))
        .collect();
    let r = NativeExecutor::execute_batch(&mut ctx, &block);
    assert!(r.results.iter().all(|x| x.success), "{mode:?}: seed failed");
    ctx.save_order_books();
}

/// Market 1: the maker's 3 bids (one with [`CLOID`]) and one stop. Market 2:
/// 1,000 bids of 20 other traders (more rows than any budget below).
fn seed_crowded(state: &StateDb, mode: BookMode) {
    let mut orders = vec![
        (addr(MAKER), gtc(1, true, 100, Some(CLOID))),
        (addr(MAKER), gtc(1, true, 99, None)),
        (addr(MAKER), gtc(1, true, 98, None)),
        (
            addr(MAKER),
            PlaceOrderParams {
                order_type: OrderType::StopMarket { trigger: fp(200) },
                ..gtc(1, true, 210, None)
            },
        ),
    ];
    for i in 0..1000u32 {
        orders.push((other(i % 20), gtc(2, true, 1 + (i % 50) as i64, None)));
    }
    seed_books(state, mode, orders);
}

// ---------------------------------------------------------------------------
// Config, error, weights
// ---------------------------------------------------------------------------

#[test]
fn max_scan_rows_env_parses_with_the_default_on_empty_bad_or_zero() {
    assert_eq!(resolve_max_scan_rows(None), DEFAULT_MAX_SCAN_ROWS);
    for raw in ["", "  ", "0", "abc", "-5"] {
        assert_eq!(
            resolve_max_scan_rows(Some(raw)),
            DEFAULT_MAX_SCAN_ROWS,
            "{raw:?}"
        );
    }
    assert_eq!(resolve_max_scan_rows(Some(" 500 ")), 500);
}

#[test]
fn a_budget_allows_max_rows_and_stops_on_the_next() {
    let mut b = ScanBudget::new(3);
    for _ in 0..3 {
        b.step().unwrap();
    }
    let e = ErrorObjectOwned::from(b.step().unwrap_err());
    assert_scan_limit(&e, "4th row");
    assert_eq!(b.examined(), 4);
}

/// Reads that may examine up to the scan budget are not point reads.
#[test]
fn scanning_reads_are_not_weighted_as_cheap_point_reads() {
    use crate::ip_limit::method_weight;
    for m in ["torus_getOpenInterest", "torus_getUserLimits"] {
        assert_eq!(method_weight(m, None), 20, "{m}");
    }
    // Bounded by its depth (one market's best levels).
    assert_eq!(method_weight("torus_getOrderBook", None), 2);
}

// ---------------------------------------------------------------------------
// getOpenInterest
// ---------------------------------------------------------------------------

fn position(trader: Address, market_id: MarketId, is_long: bool, size: i64) -> Position {
    Position {
        trader,
        market_id,
        is_long,
        size: fp(size),
        entry_price: fp(100),
        cost_basis: fp(100) * fp(size),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    }
}

#[tokio::test]
async fn open_interest_stops_at_the_scan_budget() {
    let (_d, state) = db();
    let pm = PositionManager::new(state.clone());
    for i in 0..1000 {
        pm.put_position(&position(other(i), 2, true, 1)).unwrap();
    }
    pm.put_position(&position(addr(MAKER), 1, true, 10))
        .unwrap();
    pm.put_position(&position(addr(MAKER + 1), 1, false, 7))
        .unwrap();

    let s = server(&state, 100);
    let e = s.state().get_open_interest("0x1".into()).await.unwrap_err();
    assert_scan_limit(&e, "1,002 position rows, budget 100");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    let s = server(&state, 5_000);
    let oi = s.state().get_open_interest("0x1".into()).await.unwrap();
    assert_eq!(oi.long_oi, dec_fp(fp(10)));
    assert_eq!(oi.short_oi, dec_fp(fp(7)));
    assert_eq!(examined(&s), 1002, "every position row, once");
}

// ---------------------------------------------------------------------------
// getUserLimits / getOpenOrders / getOrderByCloid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn user_limits_stop_at_the_scan_budget() {
    for mode in MODES {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        let maker = hex_addr(addr(MAKER));

        let s = server(&state, 100);
        let e = s.state().get_user_limits(maker.clone()).await.unwrap_err();
        assert_scan_limit(&e, &format!("{mode:?}"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "{mode:?}: examined {}",
            examined(&s)
        );

        let s = server(&state, 100_000);
        let l = s.state().get_user_limits(maker).await.unwrap();
        assert_eq!(l.open_orders, 4, "{mode:?}: 3 resting + 1 stop");
        assert!(examined(&s) > 0, "{mode:?}: the read is metered");
    }
}

#[tokio::test]
async fn open_orders_of_one_market_read_only_that_market() {
    for mode in MODES {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        let maker = hex_addr(addr(MAKER));

        let s = server(&state, 100);
        let one = s
            .state()
            .get_open_orders(maker.clone(), Some("0x1".into()))
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: {e:?}"));
        assert_eq!(one.len(), 3, "{mode:?}");
        assert!(
            (1..=100).contains(&examined(&s)),
            "{mode:?}: examined {}",
            examined(&s)
        );

        // All markets must read market 2's 1,000 rows: refused at the budget.
        let s = server(&state, 100);
        let e = s
            .state()
            .get_open_orders(maker.clone(), None)
            .await
            .unwrap_err();
        assert_scan_limit(&e, &format!("{mode:?} all markets"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "{mode:?}: examined {}",
            examined(&s)
        );

        let s = server(&state, 100_000);
        let all = s.state().get_open_orders(maker, None).await.unwrap();
        assert_eq!(all.len(), 3, "{mode:?}");
        assert!(all.iter().all(|o| o.market_id == "0x1"), "{mode:?}");
    }
}

#[tokio::test]
async fn order_by_cloid_stops_at_the_first_hit() {
    for mode in MODES {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        let s = server(&state, 100);
        let hit = s
            .state()
            .get_order_by_cloid(
                hex_addr(addr(MAKER)),
                torus_types::cloid_serde::to_hex(CLOID),
            )
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: {e:?}"))
            .expect("resting order found");
        assert_eq!(hit.status, "open", "{mode:?}");
        assert_eq!(hit.order.market_id, "0x1", "{mode:?}");
        assert!(
            (1..=100).contains(&examined(&s)),
            "{mode:?}: examined {}",
            examined(&s)
        );

        // A miss must read every order: refused at the budget.
        let s = server(&state, 100);
        let e = s
            .state()
            .get_order_by_cloid(
                hex_addr(addr(MAKER)),
                torus_types::cloid_serde::to_hex(CLOID + 1),
            )
            .await
            .unwrap_err();
        assert_scan_limit(&e, &format!("{mode:?} miss"));
    }
}

// ---------------------------------------------------------------------------
// getOrderBook
// ---------------------------------------------------------------------------

/// Market 1: 300 bid levels (1000..1299) and 300 ask levels (2000..2299).
fn seed_deep_book(state: &StateDb, mode: BookMode) {
    let mut orders = Vec::new();
    for i in 0..300 {
        orders.push((other(i % 3), gtc(1, true, 1000 + i as i64, None)));
        orders.push((other(3 + i % 3), gtc(1, false, 2000 + i as i64, None)));
    }
    seed_books(state, mode, orders);
}

#[tokio::test]
async fn order_book_depth_bounds_the_levels_read() {
    for mode in MODES {
        let (_d, state) = db();
        seed_deep_book(&state, mode);

        let s = server(&state, 100_000);
        let full = s.state().get_order_book("0x1".into(), None).await.unwrap();
        assert_eq!(
            (full.bids.len(), full.asks.len()),
            (300, 300),
            "{mode:?}: default depth"
        );

        let s = server(&state, 100_000);
        let top = s
            .state()
            .get_order_book("0x1".into(), Some(10))
            .await
            .unwrap();
        assert_eq!((top.bids.len(), top.asks.len()), (10, 10), "{mode:?}");
        assert_eq!(
            top.bids[0].price,
            dec_fp(fp(1299)),
            "{mode:?}: best bid first"
        );
        assert_eq!(top.bids[9].price, dec_fp(fp(1290)), "{mode:?}");
        assert_eq!(
            top.asks[0].price,
            dec_fp(fp(2000)),
            "{mode:?}: best ask first"
        );
        assert_eq!(top.asks[9].price, dec_fp(fp(2009)), "{mode:?}");
        assert!(examined(&s) > 0, "{mode:?}: the read is metered");

        let s = server(&state, 100);
        let got = s.state().get_order_book("0x1".into(), Some(10)).await;
        match mode {
            // Level rows: two seeks of 10 rows, whatever the book's size.
            BookMode::LevelAuthority => {
                assert_eq!(got.unwrap().bids.len(), 10);
                assert!(
                    (1..=30).contains(&examined(&s)),
                    "examined {}",
                    examined(&s)
                );
            }
            // Mode 1: order rows carry no price index, every order of the
            // market is read. Classic: one whole-book blob (cannot be read in
            // part), charged by its size before it is decoded.
            _ => {
                assert_scan_limit(&got.unwrap_err(), &format!("{mode:?}: 600 orders"));
                assert!(
                    (1..=101).contains(&examined(&s)),
                    "examined {}",
                    examined(&s)
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// getProposals
// ---------------------------------------------------------------------------

fn put_proposal(state: &StateDb, id: u64, status: ProposalStatus) {
    let p = Proposal {
        id,
        proposer: addr(1),
        title: format!("p{id}"),
        description: String::new(),
        proposal_type: ProposalType::TextProposal,
        status,
        votes_for: U256::ZERO,
        votes_against: U256::ZERO,
        start_block: 1,
        end_block: 2,
        executable_after: 3,
        snapshot_block: 1,
        execution_payload: None,
    };
    state
        .put_cf_raw(
            CF_GOVERNANCE_PROPOSALS,
            &id.to_be_bytes(),
            &borsh::to_vec(&p).unwrap(),
        )
        .unwrap();
}

#[tokio::test]
async fn proposals_stop_at_the_scan_budget() {
    let (_d, state) = db();
    for id in 0..300 {
        let status = if id % 3 == 0 {
            ProposalStatus::Passed
        } else {
            ProposalStatus::Active
        };
        put_proposal(&state, id, status);
    }
    for status in [None, Some("Passed".to_string())] {
        let s = server(&state, 100);
        let e = s.state().get_proposals(status.clone()).await.unwrap_err();
        assert_scan_limit(&e, &format!("{status:?}"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "{status:?}: examined {}",
            examined(&s)
        );
    }
    let s = server(&state, 1_000);
    assert_eq!(s.state().get_proposals(None).await.unwrap().len(), 300);
    assert_eq!(
        s.state()
            .get_proposals(Some("Passed".into()))
            .await
            .unwrap()
            .len(),
        100
    );
    assert_eq!(examined(&s), 600);
}

// ---------------------------------------------------------------------------
// getUserTrades
// ---------------------------------------------------------------------------

#[tokio::test]
async fn user_trades_with_a_market_filter_stop_at_the_scan_budget() {
    use torus_state::trade_rows::TradeFill;
    let (_d, state) = db();
    let me = addr(MAKER);
    let fill = |market, i| TradeFill {
        trade_index: i,
        market,
        maker: me,
        taker: addr(2),
        price_raw: 10,
        qty_raw: 1,
        taker_side: 0,
    };
    // Oldest block: market 1; then 500 blocks of market 2 (newer).
    let write = |block: u64, f: TradeFill| {
        let mut rows = torus_state::PackedCfBatch::default();
        torus_state::trade_rows::encode_block(block, block * 10, &[f], &mut rows);
        for (cf, key, value) in rows.iter() {
            state.put_cf_raw(cf, key, value).unwrap();
        }
    };
    write(1, fill(1, 0));
    for b in 2..502 {
        write(b, fill(2, 0));
    }

    let s = server(&state, 100);
    let e = s
        .state()
        .get_user_trades(hex_addr(me), Some("0x1".into()), Some(10))
        .await
        .unwrap_err();
    assert_scan_limit(&e, "market 1 is behind 500 rows of market 2");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    // Unfiltered: the limit stops it after 10 rows.
    let s = server(&state, 100);
    let t = s
        .state()
        .get_user_trades(hex_addr(me), None, Some(10))
        .await
        .unwrap();
    assert_eq!(t.len(), 10);
    assert!(
        (1..=10).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );
}

// ---------------------------------------------------------------------------
// Review round 1 (Codex): layout sniff, packed trade rows, one snapshot,
// corrupt meta rows
// ---------------------------------------------------------------------------

/// [P1] A DB without the `__book_mode__` marker is sniffed by reading every
/// key of the book CF: that sniff is charged to the request budget like any
/// other row, never run uncharged before the budget applies.
#[tokio::test]
async fn layout_detection_without_a_marker_is_charged_to_the_scan_budget() {
    use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
    use torus_state::cf::CF_NATIVE_MARKETS;
    let (_d, state) = db();
    seed_deep_book(&state, BookMode::LevelAuthority);
    state
        .delete_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY)
        .unwrap();

    // 1 meta + 600 level rows to sniff: refused at 100.
    let s = server(&state, 100);
    let e = s
        .state()
        .get_order_book("0x1".into(), Some(1))
        .await
        .unwrap_err();
    assert_scan_limit(&e, "marker-less sniff of 601 rows");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );
    let s = server(&state, 100);
    let e = s
        .state()
        .get_user_limits(hex_addr(other(0)))
        .await
        .unwrap_err();
    assert_scan_limit(&e, "getUserLimits: marker-less sniff");

    // With room: the same answer as with the marker, the sniff counted.
    let s = server(&state, 10_000);
    let top = s
        .state()
        .get_order_book("0x1".into(), Some(1))
        .await
        .unwrap();
    assert_eq!(top.bids[0].price, dec_fp(fp(1299)));
    assert_eq!(top.asks[0].price, dec_fp(fp(2000)));
    assert!(examined(&s) > 601, "examined {}", examined(&s));
}

/// [P1] One packed user-trade row can hold a whole block of fills: it is
/// charged by its size (one row per started 128 B) before it is decoded,
/// not as one row whatever it holds.
#[tokio::test]
async fn packed_user_trade_rows_are_charged_by_size_before_decoding() {
    use torus_state::trade_rows::TradeFill;
    let (_d, state) = db();
    let me = addr(MAKER);
    let fills: Vec<TradeFill> = (0..2_000)
        .map(|i| TradeFill {
            trade_index: i,
            market: 2,
            maker: me,
            taker: addr(2),
            price_raw: 10,
            qty_raw: 1,
            taker_side: 0,
        })
        .collect();
    let mut rows = torus_state::PackedCfBatch::default();
    torus_state::trade_rows::encode_block(1, 10, &fills, &mut rows);
    let mut row_bytes = 0;
    for (cf, key, value) in rows.iter() {
        if key.starts_with(me.as_slice()) && cf == torus_state::cf::CF_NATIVE_USER_TRADES {
            row_bytes = value.len();
        }
        state.put_cf_raw(cf, key, value).unwrap();
    }
    assert!(row_bytes > 100 * CLASSIC_BLOB_BYTES_PER_ROW, "{row_bytes} B");

    for market in [Some("0x1".to_string()), None] {
        let s = server(&state, 100);
        let e = s
            .state()
            .get_user_trades(hex_addr(me), market.clone(), Some(10))
            .await
            .unwrap_err();
        assert_scan_limit(&e, &format!("{market:?}: one {row_bytes} B row"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "{market:?}: examined {}",
            examined(&s)
        );
    }
    let s = server(&state, 10_000);
    let t = s
        .state()
        .get_user_trades(hex_addr(me), None, Some(10))
        .await
        .unwrap();
    assert_eq!(t.len(), 10);
}

fn level_value() -> Vec<u8> {
    torus_core::book_rows::LevelRowData {
        total_qty_raw: fp(1).raw(),
        order_count: 1,
        level_hash: [0; 32],
    }
    .encode()
    .to_vec()
}

/// [P2] Mode 2 reads the meta row and both sides from ONE snapshot: a
/// commit that moves both sides at once is seen whole or not at all, so a
/// book that is never crossed on disk is never served crossed.
#[test]
fn level_book_sides_are_read_from_one_snapshot() {
    use std::sync::atomic::AtomicBool;
    use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
    use torus_core::book_rows::{level_row_key, BookMetaRow};
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};
    use torus_types::Side;
    let (_d, state) = db();
    let meta = BookMetaRow {
        next_seq: 1,
        tick_size: fp(1),
        lot_size: fp(1),
        next_id: 1,
        last_trade_price: None,
    };
    state
        .put_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY, &[2])
        .unwrap();
    state
        .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(1), &meta.encode())
        .unwrap();
    // Two uncrossed books, swapped atomically: (bid 120, ask 130) and
    // (bid 100, ask 110). Bid 120 with ask 110 never exists on disk.
    let levels = |high: bool| {
        if high {
            [(Side::Buy, 120), (Side::Sell, 130)]
        } else {
            [(Side::Buy, 100), (Side::Sell, 110)]
        }
    };
    for (side, p) in levels(true) {
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &level_row_key(1, side, fp(p)), &level_value())
            .unwrap();
    }
    let run = Arc::new(AtomicBool::new(true));
    let writer = {
        let (state, run) = (state.clone(), run.clone());
        std::thread::spawn(move || {
            let cf = state.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap();
            let mut high = false;
            while run.load(Relaxed) {
                let mut batch = rocksdb::WriteBatch::default();
                for (side, p) in levels(!high) {
                    batch.delete_cf(cf, level_row_key(1, side, fp(p)));
                }
                for (side, p) in levels(high) {
                    batch.put_cf(cf, level_row_key(1, side, fp(p)), level_value());
                }
                state
                    .write_with(batch, &rocksdb::WriteOptions::default())
                    .unwrap();
                high = !high;
            }
        })
    };
    let mut crossed = 0;
    for _ in 0..3_000 {
        let d = row_book_depth(
            &state,
            1,
            BookLayout::LevelAuthority,
            1,
            &mut ScanBudget::new(100),
        )
        .unwrap();
        assert_eq!((d.bids.len(), d.asks.len()), (1, 1), "one level per side");
        if d.bids[0].price >= d.asks[0].price {
            crossed += 1;
        }
    }
    run.store(false, Relaxed);
    writer.join().unwrap();
    assert_eq!(crossed, 0, "books served crossed out of 3000");
}

/// [P2] A meta row that does not decode is the same `BookCorrupt` error the
/// unbounded reader returns, never an empty book (the market key alone is
/// not proof of a book).
#[tokio::test]
async fn a_corrupt_meta_row_is_an_error_not_an_empty_book() {
    use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
    use torus_core::book_rows::BookMetaRow;
    use torus_core::error::CoreError;
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};
    for mode in [1u8, 2] {
        let (_d, state) = db();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY, &[mode])
            .unwrap();
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(1), &[0])
            .unwrap();
        let want = CoreError::BookCorrupt(format!(
            "market 1: {}",
            BookMetaRow::decode(&[0]).unwrap_err()
        ))
        .to_string();
        let s = server(&state, 100);
        let e = s
            .state()
            .get_order_book("0x1".into(), Some(10))
            .await
            .unwrap_err();
        assert!(e.message().contains(&want), "mode {mode}: {e:?}");
        let e = s
            .state()
            .get_open_orders(hex_addr(addr(MAKER)), Some("0x1".into()))
            .await
            .unwrap_err();
        assert!(e.message().contains(&want), "mode {mode} open orders: {e:?}");
    }
}
