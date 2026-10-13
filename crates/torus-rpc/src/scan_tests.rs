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
use torus_core::book_rows::book_meta_key;
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
/// The maker's pending stop's cloid (market 1).
const STOP_CLOID: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0057;

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
                ..gtc(1, true, 210, Some(STOP_CLOID))
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

/// A pending stop hit returns at once (stops are read first, one seek per
/// market). A resting hit is returned only once every order was read and
/// validated, as main's `book_reader::find_by_cloid` does (review round 3
/// sweep), so it costs what a miss costs.
#[tokio::test]
async fn order_by_cloid_stop_hits_return_early_resting_hits_read_every_order() {
    for mode in MODES {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        let maker = hex_addr(addr(MAKER));
        let cloid = torus_types::cloid_serde::to_hex;

        let s = server(&state, 100);
        let hit = s
            .state()
            .get_order_by_cloid(maker.clone(), cloid(STOP_CLOID))
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: {e:?}"))
            .expect("pending stop found");
        assert_eq!(hit.status, "triggerPending", "{mode:?}");
        assert_eq!(hit.order.market_id, "0x1", "{mode:?}");
        assert!(
            (1..=100).contains(&examined(&s)),
            "{mode:?}: examined {}",
            examined(&s)
        );

        // A resting hit and a miss read every order: refused at the budget.
        for c in [CLOID, CLOID + 1] {
            let s = server(&state, 100);
            let e = s
                .state()
                .get_order_by_cloid(maker.clone(), cloid(c))
                .await
                .unwrap_err();
            assert_scan_limit(&e, &format!("{mode:?} cloid {c:#x}"));
        }
        let s = server(&state, 100_000);
        let hit = s
            .state()
            .get_order_by_cloid(maker.clone(), cloid(CLOID))
            .await
            .unwrap()
            .expect("resting order found");
        assert_eq!(hit.status, "open", "{mode:?}");
        assert_eq!(hit.order.market_id, "0x1", "{mode:?}");
        assert!(examined(&s) > 100, "{mode:?}: examined {}", examined(&s));
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

        // Every layout reads the whole market (review round 3): mode 1 has
        // no price index, mode 2 validates every key and level row of the
        // market as main's `read_book_depth` does (an unknown key or a
        // corrupt level past the depth is an error, not skipped), classic is
        // one whole-book blob charged by its size before it is decoded.
        // `depth` bounds the reply, the budget bounds the rows.
        let s = server(&state, 100);
        let got = s.state().get_order_book("0x1".into(), Some(10)).await;
        assert_scan_limit(&got.unwrap_err(), &format!("{mode:?}: 600 orders"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "examined {}",
            examined(&s)
        );
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
    // Each proposal row costs one row per started 128 B (round 2), twice.
    let per_pass: usize =
        torus_state::StateBackend::iterate_cf(&state, CF_GOVERNANCE_PROPOSALS, None)
            .unwrap()
            .iter()
            .map(|(_, v)| packed_rows(v.len()))
            .sum();
    assert!(per_pass >= 300, "{per_pass}");
    assert_eq!(examined(&s), 2 * per_pass as u64);
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
    assert!(
        row_bytes > 100 * CLASSIC_BLOB_BYTES_PER_ROW,
        "{row_bytes} B"
    );

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
            .put_cf_raw(
                CF_NATIVE_ORDER_BOOKS,
                &level_row_key(1, side, fp(p)),
                &level_value(),
            )
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
        assert!(
            e.message().contains(&want),
            "mode {mode} open orders: {e:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Review round 2 (Codex): classic mark price, truncated order rows, proposal
// size, getMarkets / getBlockTrades
// ---------------------------------------------------------------------------

/// [P1] Classic `getMarkPrice` decodes the market's whole-book blob for one
/// scalar: the blob is charged by its size (one row per started 128 B) to
/// the request budget before it is decoded, as `getOrderBook` charges it.
/// Row layouts read only the meta row.
#[tokio::test]
async fn classic_mark_price_charges_the_blob_before_decoding() {
    for mode in MODES {
        let (_d, state) = db();
        seed_deep_book(&state, mode);

        let s = server(&state, 100);
        let got = s.state().get_mark_price("0x1".into()).await;
        if matches!(mode, BookMode::Classic) {
            assert_scan_limit(&got.unwrap_err(), "classic blob of 600 orders, budget 100");
            assert!(
                (1..=101).contains(&examined(&s)),
                "examined {}",
                examined(&s)
            );
        } else {
            let m = got.unwrap_or_else(|e| panic!("{mode:?}: {e:?}"));
            assert_eq!(m.last_trade_price, dec_fp(FixedPoint::ZERO), "{mode:?}");
            assert!(
                (1..=5).contains(&examined(&s)),
                "{mode:?}: examined {}",
                examined(&s)
            );
        }

        // Under the budget: the answer is unchanged, the blob counted.
        let s = server(&state, 100_000);
        let m = s.state().get_mark_price("0x1".into()).await.unwrap();
        assert_eq!(m.last_trade_price, dec_fp(FixedPoint::ZERO), "{mode:?}");
        if matches!(mode, BookMode::Classic) {
            assert!(examined(&s) > 100, "examined {}", examined(&s));
        }
    }
}

/// The order rows of market `mid` under `mode` (`(cf, key, value)`).
fn order_rows(
    state: &StateDb,
    mode: BookMode,
    mid: MarketId,
) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};
    use torus_state::StateBackend;
    let (cf, prefix) = match mode {
        BookMode::OrderRows => (
            CF_NATIVE_ORDER_BOOKS,
            [&mid.to_be_bytes()[..], &[ROW_TAG_ORDER]].concat(),
        ),
        _ => (CF_BOOK_ORDER_ROWS, mid.to_be_bytes().to_vec()),
    };
    let rows = StateBackend::iterate_cf(state, cf, Some(&prefix)).unwrap();
    assert!(!rows.is_empty(), "{mode:?}: market {mid} has order rows");
    rows.into_iter().map(|(k, v)| (cf, k, v)).collect()
}

/// (market of the corrupt row, its new length from its old one, markets
/// queried).
type CorruptRowCase = (MarketId, fn(usize) -> usize, &'static [Option<MarketId>]);

/// [P2] An order row that does not decode is the `BookCorrupt` error the
/// unbounded reader (`book_reader::read_open_orders`) returns, whoever's row
/// it is: rows are validated BEFORE the trader filter, never skipped (a
/// truncated row has no readable trader, a row cut by one byte still has).
#[tokio::test]
async fn a_truncated_order_row_is_book_corrupt_not_skipped() {
    let maker = addr(MAKER);
    for mode in [BookMode::OrderRows, BookMode::LevelAuthority] {
        let layout = match mode {
            BookMode::OrderRows => BookLayout::OrderRows,
            _ => BookLayout::LevelAuthority,
        };
        let cases: [CorruptRowCase; 2] = [
            // The maker's own row cut to 30 B (the trader bytes are gone).
            (1, |_| 30, &[Some(1), None]),
            // Another trader's row cut by one byte (its trader is readable).
            (2, |n| n - 1, &[None, Some(2)]),
        ];
        for (mid, cut, queries) in cases {
            let (_d, state) = db();
            seed_crowded(&state, mode);
            let (cf, key, value) = order_rows(&state, mode, mid).remove(0);
            state
                .put_cf_raw(cf, &key, &value[..cut(value.len())])
                .unwrap();
            for &market in queries {
                let want = book_reader::read_open_orders(&state, &maker, market, layout, 5000)
                    .expect_err("the unbounded reader rejects the row")
                    .to_string();
                assert!(want.contains("book order row"), "{want}");
                let s = server(&state, 100_000);
                let e = s
                    .state()
                    .get_open_orders(hex_addr(maker), market.map(|m| format!("0x{m:x}")))
                    .await
                    .expect_err("a corrupt row is an error, not a short list");
                assert!(
                    e.message().contains(&want),
                    "{mode:?} row of market {mid}, query {market:?}: {e:?} (want {want})"
                );
            }
        }
    }
}

fn put_big_proposal(state: &StateDb, id: u64, description_bytes: usize) -> usize {
    let p = Proposal {
        id,
        proposer: addr(1),
        title: format!("p{id}"),
        description: "x".repeat(description_bytes),
        proposal_type: ProposalType::TextProposal,
        status: ProposalStatus::Active,
        votes_for: U256::ZERO,
        votes_against: U256::ZERO,
        start_block: 1,
        end_block: 2,
        executable_after: 3,
        snapshot_block: 1,
        execution_payload: None,
    };
    let bytes = borsh::to_vec(&p).unwrap();
    state
        .put_cf_raw(CF_GOVERNANCE_PROPOSALS, &id.to_be_bytes(), &bytes)
        .unwrap();
    bytes.len()
}

/// [P2] A proposal row is charged by its size (one row per started 128 B,
/// the row itself included) before it is decoded: one large proposal is not
/// one row, even when a status filter drops it.
#[tokio::test]
async fn proposal_rows_are_charged_by_size_before_decoding() {
    let (_d, state) = db();
    let len = put_big_proposal(&state, 0, 64 * 1024);
    for status in [Some("Passed".to_string()), None] {
        let s = server(&state, 100);
        let e = s.state().get_proposals(status.clone()).await.unwrap_err();
        assert_scan_limit(&e, &format!("{status:?}: one {len} B proposal"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "{status:?}: examined {}",
            examined(&s)
        );
    }
    let s = server(&state, 10_000);
    assert_eq!(s.state().get_proposals(None).await.unwrap().len(), 1);
    assert_eq!(examined(&s), packed_rows(len) as u64, "{len} B");
    let s = server(&state, 10_000);
    assert!(s
        .state()
        .get_proposals(Some("Passed".into()))
        .await
        .unwrap()
        .is_empty());
}

/// Markets `1..=n` in the governance borsh layout (`StoredMarket`).
fn list_markets(state: &StateDb, n: u64) {
    use borsh::BorshSerialize;
    use torus_state::cf::CF_NATIVE_MARKETS;
    for mid in 1..=n {
        let mut row = Vec::new();
        "BTC".to_string().serialize(&mut row).unwrap();
        "USD".to_string().serialize(&mut row).unwrap();
        FixedPoint::SCALE.serialize(&mut row).unwrap();
        FixedPoint::SCALE.serialize(&mut row).unwrap();
        (5 * FixedPoint::SCALE).serialize(&mut row).unwrap();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, &mid.to_be_bytes(), &row)
            .unwrap();
    }
}

/// [P2] `getMarkets(offset)` walks every market row before the offset: each
/// row visited is charged, so a deep offset is refused at the budget.
#[tokio::test]
async fn markets_offset_is_charged_to_the_scan_budget() {
    let (_d, state) = db();
    list_markets(&state, 1000);

    let s = server(&state, 100);
    let e = s.state().get_markets(Some(999), Some(1)).await.unwrap_err();
    assert_scan_limit(&e, "offset 999 of 1000 markets, budget 100");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    // The first page fits: the same rows as before.
    let s = server(&state, 100);
    let page = s.state().get_markets(None, Some(10)).await.unwrap();
    assert_eq!(page.len(), 10);
    assert_eq!(page[0].market_id, "0x1");
    assert!(
        (1..=10).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    let s = server(&state, 10_000);
    let last = s.state().get_markets(Some(999), Some(1)).await.unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].market_id, "0x3e8");
}

/// [P2] `getBlockTrades` seeks every market's trade rows for the block, even
/// for an empty block: each market row visited (and each trade row, by size)
/// is charged, so a large market table is refused at the budget.
#[tokio::test]
async fn block_trades_are_charged_per_market_to_the_scan_budget() {
    use torus_state::trade_rows::TradeFill;
    let (_d, state) = db();
    list_markets(&state, 1000);
    let fill = TradeFill {
        trade_index: 0,
        market: 5,
        maker: addr(MAKER),
        taker: addr(2),
        price_raw: 10,
        qty_raw: 1,
        taker_side: 0,
    };
    let mut rows = torus_state::PackedCfBatch::default();
    torus_state::trade_rows::encode_block(7, 70, &[fill], &mut rows);
    for (cf, key, value) in rows.iter() {
        state.put_cf_raw(cf, key, value).unwrap();
    }

    for block in [7, 8] {
        let s = server(&state, 100);
        let e = s.state().get_block_trades(block).await.unwrap_err();
        assert_scan_limit(&e, &format!("block {block}: 1000 markets, budget 100"));
        assert!(
            (1..=101).contains(&examined(&s)),
            "block {block}: examined {}",
            examined(&s)
        );
    }

    // Under the budget: the same trades as the unbudgeted scan.
    let s = server(&state, 10_000);
    let t = s.state().get_block_trades(7).await.unwrap();
    assert_eq!(t.len(), crate::scan_trades_for_block(&state, 7).len());
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].market_id, "0x5");
    assert!(examined(&s) >= 1000, "examined {}", examined(&s));
    let s = server(&state, 10_000);
    assert!(s.state().get_block_trades(8).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Review round 3 (Codex): account positions, getProposal, unknown market
// rows, skipped trade rows; then the method sweep (exactness vs main)
// ---------------------------------------------------------------------------

/// `trader` holds one position in each market `1..=n`.
fn put_positions(state: &StateDb, trader: Address, n: u64) {
    let pm = PositionManager::new(state.clone());
    for m in 1..=n {
        pm.put_position(&position(trader, m, m % 2 == 0, 1))
            .unwrap();
    }
}

/// [P1] `getBalances` / `getLiquidatorVault` answer a fixed-size reply from
/// all of the account's positions: each position row, and each market row
/// (and oracle aggregate, with a block time) read for them, is charged to
/// the request budget, so a large position set is refused, not walked.
#[tokio::test]
async fn balances_and_liquidator_vault_stop_at_the_scan_budget() {
    let vault = torus_core::liquidation::LIQUIDATOR_VAULT;
    let (_d, state) = db();
    list_markets(&state, 300);
    put_positions(&state, addr(MAKER), 300);
    put_positions(&state, vault, 300);
    let maker = hex_addr(addr(MAKER));

    let s = server(&state, 100);
    let e = s.state().get_balances(maker.clone()).await.unwrap_err();
    assert_scan_limit(&e, "getBalances: 300 positions, budget 100");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );
    let s = server(&state, 100);
    let e = s.state().get_liquidator_vault().await.unwrap_err();
    assert_scan_limit(&e, "getLiquidatorVault: 300 positions, budget 100");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    // Under the budget: main's figures. No executed header (no block time):
    // one position row + one market row per market, no oracle read.
    let s = server(&state, 10_000);
    let b = s.state().get_balances(maker).await.unwrap();
    let f = crate::account::account_figures(&state, &addr(MAKER), None).unwrap();
    assert_eq!(b.account_value, dec_fp(f.account_value));
    assert_eq!(b.total_margin_used, dec_fp(f.total_margin_used));
    assert_eq!(b.maintenance_margin_used, dec_fp(f.maintenance_margin_used));
    assert_eq!(b.free_margin, dec_fp(f.free_margin));
    assert_eq!(b.withdrawable, dec_fp(f.withdrawable));
    assert_eq!(examined(&s), 600);
    let s = server(&state, 10_000);
    let v = s.state().get_liquidator_vault().await.unwrap();
    assert_eq!(v.open_positions, 300);
    assert_eq!(examined(&s), 300);
}

/// [P1] `getPosition`'s cross `liquidationPrice` (R23) values the whole
/// account: its position rows and the market row (and oracle aggregate,
/// with a block time) of each market held are charged to the request
/// budget, as `getBalances` charges them. An isolated position reads no
/// account rows.
#[tokio::test]
async fn cross_position_liquidation_price_stops_at_the_scan_budget() {
    let (_d, state) = db();
    list_markets(&state, 300);
    put_positions(&state, addr(MAKER), 300);
    // Cash so that market 1's short has a price (healthy at mark 0).
    PositionManager::new(state.clone())
        .put_native_balance(
            &addr(MAKER),
            &NativeBalance {
                available: fp(10_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let maker = hex_addr(addr(MAKER));

    let s = server(&state, 100);
    let e = s
        .state()
        .get_position(maker.clone(), "0x1".into())
        .await
        .unwrap_err();
    assert_scan_limit(&e, "getPosition cross: 300 positions, budget 100");
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    // Under the budget: main's price. No executed header (no block time):
    // one position row + one market row per market, no oracle read.
    let s = server(&state, 10_000);
    let p = s
        .state()
        .get_position(maker, "0x1".into())
        .await
        .unwrap()
        .unwrap();
    let want = crate::account::cross_liquidation_price(&state, &addr(MAKER), 1, None).unwrap();
    assert!(want.is_some());
    assert_eq!(p.liquidation_price, want.map(dec_fp));
    assert_eq!(examined(&s), 600);

    // Isolated: the reply's own estimate, nothing charged.
    let iso = other(0);
    PositionManager::new(state.clone())
        .put_position(&Position {
            margin_type: MarginType::Isolated,
            isolated_margin: fp(10),
            ..position(iso, 1, true, 1)
        })
        .unwrap();
    let s = server(&state, 0);
    let p = s
        .state()
        .get_position(hex_addr(iso), "0x1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.liquidation_price, Some(dec_fp(fp(90))));
    assert_eq!(examined(&s), 0);
}

/// [P2] `getProposal` decodes the whole stored proposal for its reply: the
/// row is charged one row per started 128 B before it is decoded, as
/// `getProposals` charges it. A missing id is one read; a row that does not
/// decode is main's error.
#[tokio::test]
async fn get_proposal_charges_the_row_by_size_before_decoding() {
    use torus_economics::governance::GovernanceManager;
    let (_d, state) = db();
    let len = put_big_proposal(&state, 0, 64 * 1024);

    let s = server(&state, 100);
    let e = s.state().get_proposal(0).await.unwrap_err();
    assert_scan_limit(&e, &format!("one {len} B proposal"));
    assert!(
        (1..=101).contains(&examined(&s)),
        "examined {}",
        examined(&s)
    );

    let s = server(&state, 10_000);
    assert!(s.state().get_proposal(0).await.unwrap().is_some());
    assert_eq!(examined(&s), packed_rows(len) as u64, "{len} B");
    let s = server(&state, 10_000);
    assert!(s.state().get_proposal(7).await.unwrap().is_none());
    assert_eq!(examined(&s), 1, "a missing id is one read");

    state
        .put_cf_raw(CF_GOVERNANCE_PROPOSALS, &1u64.to_be_bytes(), &[0])
        .unwrap();
    let want = format!(
        "internal error: {}",
        GovernanceManager::new(state.clone())
            .get_proposal(1)
            .unwrap_err()
    );
    let e = server(&state, 10_000)
        .state()
        .get_proposal(1)
        .await
        .unwrap_err();
    assert_eq!(e.message(), want);
}

fn layout_of(mode: BookMode) -> BookLayout {
    match mode {
        BookMode::Classic => BookLayout::Classic,
        BookMode::OrderRows => BookLayout::OrderRows,
        _ => BookLayout::LevelAuthority,
    }
}

/// [P2] A row layout's market is validated whole, as main's readers do
/// (`read_book_depth`, `read_open_orders` with a market): a key of no known
/// shape anywhere under the market, or a level row past the requested
/// depth that does not decode, is main's error, never an answer that
/// skipped it.
#[tokio::test]
async fn unknown_or_corrupt_market_rows_fail_as_main() {
    use torus_core::book_rows::level_row_key;
    use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
    use torus_types::Side;
    let maker = addr(MAKER);
    for mode in [BookMode::OrderRows, BookMode::LevelAuthority] {
        let layout = layout_of(mode);
        let (_d, state) = db();
        seed_crowded(&state, mode);
        // Sorts after every known row of market 1.
        let mut key = 1u64.to_be_bytes().to_vec();
        key.push(0xff);
        state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &[0]).unwrap();

        let want = book_reader::read_book_depth(&state, 1, layout)
            .unwrap_err()
            .to_string();
        assert!(want.contains("unrecognized"), "{want}");
        let s = server(&state, 100_000);
        let e = s
            .state()
            .get_order_book("0x1".into(), Some(1))
            .await
            .expect_err("main rejects the market");
        assert_eq!(e.message(), format!("internal error: {want}"), "{mode:?}");

        let want = book_reader::read_open_orders(&state, &maker, Some(1), layout, 5000)
            .unwrap_err()
            .to_string();
        let e = s
            .state()
            .get_open_orders(hex_addr(maker), Some("0x1".into()))
            .await
            .expect_err("main rejects the market");
        assert_eq!(e.message(), format!("internal error: {want}"), "{mode:?}");
    }

    // Mode 2: the worst bid level (past depth 10) does not decode.
    let (_d, state) = db();
    seed_deep_book(&state, BookMode::LevelAuthority);
    state
        .put_cf_raw(
            CF_NATIVE_ORDER_BOOKS,
            &level_row_key(1, Side::Buy, fp(1000)),
            &[0; 3],
        )
        .unwrap();
    let want = book_reader::read_book_depth(&state, 1, BookLayout::LevelAuthority)
        .unwrap_err()
        .to_string();
    let e = server(&state, 100_000)
        .state()
        .get_order_book("0x1".into(), Some(10))
        .await
        .expect_err("main rejects the corrupt level");
    assert_eq!(e.message(), format!("internal error: {want}"));
}

/// Market 1's newest fill (`getTradeHistory`, limit 1).
async fn trade_history(s: &RpcServer) -> jsonrpsee::core::RpcResult<Vec<crate::types::RpcTrade>> {
    s.state().get_trade_history("0x1".into(), Some(1)).await
}

/// Market 1's first fill in blocks `from..=to` (limit 1).
async fn trade_range(
    s: &RpcServer,
    from: &str,
    to: &str,
) -> jsonrpsee::core::RpcResult<Vec<crate::types::RpcTrade>> {
    s.state()
        .get_trade_history_range("0x1".into(), from.into(), to.into(), Some(1))
        .await
}

/// [P2] `getTradeHistory` / `getTradeHistoryRange` charge every trade row
/// they visit, by its size and before it is decoded or skipped: rows the
/// decoder skips (old-format keys) do not fill the `limit`, so they are
/// bounded by the budget, and one full block row is not one row.
#[tokio::test]
async fn trade_history_charges_every_row_visited() {
    use torus_state::cf::CF_NATIVE_TRADES;
    use torus_state::trade_rows::TradeFill;
    let (_d, state) = db();
    // 1,000 rows of market 1 the decoder skips (20-byte old-format keys).
    for block in 1..=1000u64 {
        let key = [
            &1u64.to_be_bytes()[..],
            &block.to_be_bytes()[..],
            &0u32.to_be_bytes()[..],
        ]
        .concat();
        state.put_cf_raw(CF_NATIVE_TRADES, &key, &[0]).unwrap();
    }

    let s = server(&state, 100);
    assert_scan_limit(&trade_history(&s).await.unwrap_err(), "1000 skipped rows");
    assert!((1..=101).contains(&examined(&s)), "{}", examined(&s));
    let s = server(&state, 100);
    assert_scan_limit(
        &trade_range(&s, "0x1", "0x3e8").await.unwrap_err(),
        "range: 1000 skipped rows",
    );
    assert!((1..=101).contains(&examined(&s)), "{}", examined(&s));

    // Under the budget: main's answer (every row skipped), each row charged.
    let s = server(&state, 10_000);
    assert!(trade_history(&s).await.unwrap().is_empty());
    assert_eq!(examined(&s), 1000);
    let s = server(&state, 10_000);
    assert!(trade_range(&s, "0x1", "0x3e8").await.unwrap().is_empty());
    assert_eq!(examined(&s), 1000);

    // One full block row (1,024 fills), charged by size even for limit 1.
    let (_d2, state) = db();
    let fills: Vec<TradeFill> = (0..1024)
        .map(|i| TradeFill {
            trade_index: i,
            market: 1,
            maker: addr(MAKER),
            taker: addr(2),
            price_raw: 10,
            qty_raw: 1,
            taker_side: 0,
        })
        .collect();
    let mut rows = torus_state::PackedCfBatch::default();
    torus_state::trade_rows::encode_block(5, 50, &fills, &mut rows);
    let mut row_bytes = 0;
    for (cf, key, value) in rows.iter() {
        if cf == CF_NATIVE_TRADES {
            row_bytes = value.len();
        }
        state.put_cf_raw(cf, key, value).unwrap();
    }
    assert!(
        row_bytes > 100 * CLASSIC_BLOB_BYTES_PER_ROW,
        "{row_bytes} B"
    );
    let s = server(&state, 100);
    assert_scan_limit(&trade_history(&s).await.unwrap_err(), "one full row");
    let s = server(&state, 100);
    assert_scan_limit(
        &trade_range(&s, "0x5", "0x5").await.unwrap_err(),
        "range: one full row",
    );
    let s = server(&state, 10_000);
    assert_eq!(trade_history(&s).await.unwrap().len(), 1);
    assert_eq!(examined(&s), packed_rows(row_bytes) as u64);
    let s = server(&state, 10_000);
    assert_eq!(trade_range(&s, "0x5", "0x5").await.unwrap().len(), 1);
    assert_eq!(examined(&s), packed_rows(row_bytes) as u64);
}

/// Sweep: `getLeader` looks the leader's key up in every validator row (main
/// decodes them all before searching; a row that does not decode makes it
/// fall back to the key): each row is charged to the request budget.
#[tokio::test]
async fn leader_lookup_stops_at_the_scan_budget() {
    use torus_economics::types::{ValidatorState, ValidatorStatus};
    use torus_state::cf::CF_STAKING_VALIDATORS;
    let pk = [7u8; 32];
    let validator = |n: u32, pubkey: [u8; 32]| ValidatorState {
        address: other(n),
        pubkey,
        commission_bps: 0,
        self_stake: U256::ZERO,
        total_delegated: U256::ZERO,
        status: ValidatorStatus::Active,
        jailed_until: None,
        last_commission_change_block: None,
        oracle_signer: None,
    };
    let (_d, state) = db();
    for n in 0..=1000u32 {
        let v = validator(n, if n == 1000 { pk } else { [1; 32] });
        state
            .put_cf_raw(
                CF_STAKING_VALIDATORS,
                v.address.as_slice(),
                &borsh::to_vec(&v).unwrap(),
            )
            .unwrap();
    }
    let leader = |max| {
        let mut s = server(&state, max);
        s.state.leader_vk_fn = Some(Arc::new(move || Some(pk)));
        s
    };

    let s = leader(100);
    let e = s.state().get_leader().await.unwrap_err();
    assert_scan_limit(&e, "1001 validators, budget 100");
    assert!((1..=101).contains(&examined(&s)), "{}", examined(&s));

    let s = leader(10_000);
    let l = s.state().get_leader().await.unwrap();
    assert_eq!(l.address, crate::types::hex_address(other(1000)));
    assert_eq!(examined(&s), 1001);

    // A row that does not decode: main's fallback (the key, not an error).
    state
        .put_cf_raw(CF_STAKING_VALIDATORS, other(5000).as_slice(), &[0])
        .unwrap();
    let s = leader(10_000);
    let l = s.state().get_leader().await.unwrap();
    assert_eq!(l.address, format!("0x{}", hex::encode(pk)));
}

/// Market 2's first order (classic: its blob) made undecodable: another
/// trader's row, after market 1's.
fn corrupt_market_2(state: &StateDb, mode: BookMode) {
    use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
    if matches!(mode, BookMode::Classic) {
        let blob = state
            .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &2u64.to_be_bytes())
            .unwrap()
            .unwrap();
        state
            .put_cf_raw(
                CF_NATIVE_ORDER_BOOKS,
                &2u64.to_be_bytes(),
                &blob[..blob.len() / 2],
            )
            .unwrap();
    } else {
        let (cf, key, value) = order_rows(state, mode, 2).remove(0);
        state
            .put_cf_raw(cf, &key, &value[..value.len() - 1])
            .unwrap();
    }
}

/// Sweep (exactness): a resting cloid hit is answered only after every
/// order (classic: every blob) was validated, as main's
/// `book_reader::find_by_cloid` does: a corrupt row of a later market is
/// main's error, not a hit.
#[tokio::test]
async fn order_by_cloid_validates_every_order_before_answering() {
    for mode in MODES {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        corrupt_market_2(&state, mode);
        let want = book_reader::find_by_cloid(&state, &addr(MAKER), CLOID, layout_of(mode))
            .expect_err("main rejects the corrupt row")
            .to_string();
        let e = server(&state, 100_000)
            .state()
            .get_order_by_cloid(
                hex_addr(addr(MAKER)),
                torus_types::cloid_serde::to_hex(CLOID),
            )
            .await
            .expect_err("a corrupt row is an error, not a hit");
        assert_eq!(e.message(), format!("internal error: {want}"), "{mode:?}");
    }
}

/// Sweep (exactness): under the row layouts main validates every order row
/// and then truncates to 5,000; the bounded read does too (it does not stop
/// at the 5,000th order of the trader).
#[tokio::test]
async fn open_orders_validate_rows_past_the_reply_limit() {
    use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
    use torus_core::book_rows::{level_row_key, BookMetaRow, ROW_TAG_ORDER};
    use torus_core::order_book::Order;
    use torus_core::position::OPEN_ORDER_MAX_LIMIT;
    use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};
    use torus_types::Side;
    let limit = OPEN_ORDER_MAX_LIMIT as u128;
    let maker = addr(MAKER);
    for (marker, layout) in [
        (1u8, BookLayout::OrderRows),
        (2, BookLayout::LevelAuthority),
    ] {
        let (_d, state) = db();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY, &[marker])
            .unwrap();
        let meta = BookMetaRow {
            next_seq: limit as u64 + 2,
            tick_size: fp(1),
            lot_size: fp(1),
            next_id: limit + 2,
            last_trade_price: None,
        };
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(1), &meta.encode())
            .unwrap();
        let cf = if marker == 1 {
            CF_NATIVE_ORDER_BOOKS
        } else {
            state
                .put_cf_raw(
                    CF_NATIVE_ORDER_BOOKS,
                    &level_row_key(1, Side::Buy, fp(100)),
                    &level_value(),
                )
                .unwrap();
            CF_BOOK_ORDER_ROWS
        };
        // The maker's 5,000 orders, then another trader's corrupt row.
        for id in 1..=limit + 1 {
            let order = Order {
                id,
                trader: if id <= limit { maker } else { addr(2) },
                side: Side::Buy,
                price: fp(100),
                remaining_qty: fp(1),
                original_qty: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                timestamp: 1,
                reduce_only: false,
                client_order_id: None,
            };
            let mut row = (id as u64).to_be_bytes().to_vec();
            row.extend(borsh::to_vec(&order).unwrap());
            if id > limit {
                row.pop();
            }
            let key = [
                &1u64.to_be_bytes()[..],
                &[ROW_TAG_ORDER],
                &id.to_be_bytes()[..],
            ]
            .concat();
            state.put_cf_raw(cf, &key, &row).unwrap();
        }
        for market in [None, Some(1u64)] {
            let want = book_reader::read_open_orders(&state, &maker, market, layout, 5000)
                .expect_err("main rejects the corrupt row")
                .to_string();
            let e = server(&state, 100_000)
                .state()
                .get_open_orders(hex_addr(maker), market.map(|m| format!("0x{m:x}")))
                .await
                .expect_err("a corrupt row after the 5,000th order is still an error");
            assert_eq!(
                e.message(),
                format!("internal error: {want}"),
                "{layout:?} {market:?}"
            );
        }
    }
}

/// Sweep (exactness): `getUserLimits` counts as main's `count_open_orders`
/// did: under the row layouts only the trader's own order rows are decoded
/// (another trader's corrupt row is not read), a classic blob that does not
/// decode is main's "borsh decode order book" error.
#[tokio::test]
async fn user_limits_count_as_main_did() {
    use borsh::BorshDeserialize;
    use torus_core::order_book::OrderBook;
    use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
    for mode in MODES {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        corrupt_market_2(&state, mode);
        let got = server(&state, 100_000)
            .state()
            .get_user_limits(hex_addr(addr(MAKER)))
            .await;
        if matches!(mode, BookMode::Classic) {
            let cut = state
                .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &2u64.to_be_bytes())
                .unwrap()
                .unwrap();
            let want = format!(
                "internal error: borsh decode order book: {}",
                OrderBook::try_from_slice(&cut).err().unwrap()
            );
            assert_eq!(got.unwrap_err().message(), want);
        } else {
            let l = got.unwrap_or_else(|e| panic!("{mode:?}: {e:?}"));
            assert_eq!(l.open_orders, 4, "{mode:?}: 3 resting + 1 stop");
        }
    }
    // The maker's own row corrupt: main's "book order row" error.
    for mode in [BookMode::OrderRows, BookMode::LevelAuthority] {
        let (_d, state) = db();
        seed_crowded(&state, mode);
        let (cf, key, value) = order_rows(&state, mode, 1).remove(0);
        let cut = &value[..value.len() - 1];
        state.put_cf_raw(cf, &key, cut).unwrap();
        let want = format!(
            "internal error: book order row: {}",
            OrderBook::decode_order_row(cut).unwrap_err()
        );
        let e = server(&state, 100_000)
            .state()
            .get_user_limits(hex_addr(addr(MAKER)))
            .await
            .unwrap_err();
        assert_eq!(e.message(), want, "{mode:?}");
    }
}

/// Sweep (exactness): classic `getOpenOrders` reports main's decode error
/// ("borsh decode order book"). Without a market, main skipped such a blob;
/// the bounded read reports it (fail loud, the one intended difference).
#[tokio::test]
async fn classic_open_orders_report_main_decode_error() {
    use borsh::BorshDeserialize;
    use torus_core::order_book::OrderBook;
    use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
    let (_d, state) = db();
    seed_crowded(&state, BookMode::Classic);
    corrupt_market_2(&state, BookMode::Classic);
    let cut = state
        .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &2u64.to_be_bytes())
        .unwrap()
        .unwrap();
    let want = format!(
        "internal error: borsh decode order book: {}",
        OrderBook::try_from_slice(&cut).err().unwrap()
    );
    for market in [Some("0x2".to_string()), None] {
        let e = server(&state, 100_000)
            .state()
            .get_open_orders(hex_addr(addr(MAKER)), market.clone())
            .await
            .unwrap_err();
        assert_eq!(e.message(), want, "{market:?}");
    }
}

/// Sweep (exactness): a proposal row that does not decode is main's error
/// for each form: the governance manager's with a status filter, "borsh
/// decode proposal" without.
#[tokio::test]
async fn proposals_report_main_decode_errors() {
    use torus_economics::governance::GovernanceManager;
    let (_d, state) = db();
    put_proposal(&state, 0, ProposalStatus::Passed);
    state
        .put_cf_raw(CF_GOVERNANCE_PROPOSALS, &1u64.to_be_bytes(), &[0])
        .unwrap();
    let want = format!(
        "internal error: {}",
        GovernanceManager::new(state.clone())
            .get_proposals_by_status(ProposalStatus::Passed)
            .unwrap_err()
    );
    let s = server(&state, 10_000);
    let e = s
        .state()
        .get_proposals(Some("Passed".into()))
        .await
        .unwrap_err();
    assert_eq!(e.message(), want);
    let e = s.state().get_proposals(None).await.unwrap_err();
    assert!(
        e.message()
            .starts_with("internal error: borsh decode proposal: "),
        "{e:?}"
    );
}
