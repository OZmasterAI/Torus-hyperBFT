//! Deep-book storage round — CHARACTERIZATION ORACLES (RPC read surface).
//!
//! Pins the EXTERNAL behavior of the three RPC endpoints that read
//! CF_NATIVE_ORDER_BOOKS — getOrderBook, getOpenOrders, getMarkPrice —
//! against the logical book state. The expected values are COMPUTED from the
//! in-memory book (never hardcoded against the storage bytes), so these
//! assertions must pass unchanged across the storage-layout refactor.
//!
//! ONLY `persist_book` knows the storage format (it follows what
//! `save_order_books` actually writes).

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;
use jsonrpsee::server::ServerHandle;
use tempfile::TempDir;

use torus_core::order_book::OrderBook;
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_rpc::types::{hex_fp, RpcMarkPrice, RpcOpenOrder, RpcOrderBook};
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

fn limit(market_id: u64, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
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

/// Persist `book` exactly the way production (`save_order_books`) does —
/// since the deep-book round, per-order rows via the store. The ONLY
/// format-coupled code in this file (assertions unchanged from the
/// monolithic-blob baseline).
fn persist_book(state: &StateDb, book: &mut OrderBook) {
    torus_core::order_book_store::save_book_full(state, book).unwrap();
}

fn setup() -> (TempDir, StateDb, Arc<Mempool>, Arc<EvmExecutor>) {
    let dir = TempDir::new().unwrap();
    let state = StateDb::open(dir.path()).unwrap();
    (
        dir,
        state.clone(),
        Arc::new(Mempool::new(state.clone(), MempoolConfig::default())),
        Arc::new(EvmExecutor::new(TORUS_CHAIN_ID)),
    )
}

async fn start_server(
    state: StateDb,
    mempool: Arc<Mempool>,
    executor: Arc<EvmExecutor>,
) -> (ServerHandle, SocketAddr) {
    RpcServer::new(
        state,
        mempool,
        executor,
        TORUS_CHAIN_ID,
        100,
        BlockNotifier::new(),
    )
    .start("127.0.0.1:0".parse().unwrap())
    .await
    .unwrap()
}

/// A populated multi-trader book with same-price FIFO, a partial fill (sets
/// last_trade_price) and a priority-losing modify.
fn build_book(market_id: u64) -> OrderBook {
    let mut book = OrderBook::new(market_id, fp(1), fp(1));
    let ra = book.place_order(limit(market_id, true, 100, 5), addr(1), 10);
    book.place_order(limit(market_id, true, 100, 3), addr(2), 11);
    book.place_order(limit(market_id, true, 99, 2), addr(1), 12);
    book.place_order(limit(market_id, false, 105, 4), addr(3), 13);
    book.place_order(limit(market_id, false, 105, 6), addr(4), 14);
    book.place_order(limit(market_id, false, 106, 1), addr(5), 15);
    // Partial cross sets last_trade_price = 105.
    let r = book.place_order(limit(market_id, true, 105, 6), addr(6), 16);
    assert_eq!(r.fills.len(), 2);
    // addr(1)'s 100-bid loses priority (qty increase => cancel+reinsert).
    book.modify_order(ra.order_id, None, Some(fp(8))).unwrap();
    book.verify_invariants();
    book
}

/// getOrderBook returns exactly the levels of `to_snapshot()` (bids
/// descending, asks ascending, per-level quantity sums).
#[tokio::test]
async fn oracle_get_order_book_matches_logical_levels() {
    let (_dir, state, mempool, executor) = setup();
    let mut book = build_book(1);
    let snap = book.to_snapshot();
    persist_book(&state, &mut book);

    let (handle, sock) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{sock}"))
        .unwrap();
    let ob: RpcOrderBook = client
        .request("torus_getOrderBook", rpc_params!["1"])
        .await
        .unwrap();

    assert_eq!(ob.bids.len(), snap.bids.len(), "bid level count");
    for (got, want) in ob.bids.iter().zip(snap.bids.iter()) {
        assert_eq!(got.price, hex_fp(want.price));
        assert_eq!(got.quantity, hex_fp(want.quantity));
    }
    assert_eq!(ob.asks.len(), snap.asks.len(), "ask level count");
    for (got, want) in ob.asks.iter().zip(snap.asks.iter()) {
        assert_eq!(got.price, hex_fp(want.price));
        assert_eq!(got.quantity, hex_fp(want.quantity));
    }
    handle.stop().unwrap();
}

/// getOpenOrders (single-market AND all-markets form) returns exactly the
/// trader's resting orders — compared as id-sorted (id, price, qty, side)
/// tuples against the logical book.
#[tokio::test]
async fn oracle_get_open_orders_matches_logical_book() {
    let (_dir, state, mempool, executor) = setup();
    let mut book1 = build_book(1);
    let mut book2 = OrderBook::new(2, fp(1), fp(1));
    book2.set_next_order_id(1000);
    book2.place_order(limit(2, true, 50, 7), addr(1), 20);
    book2.verify_invariants();

    let expect = |books: &[&OrderBook], trader: &Address| -> Vec<(String, String, String, String)> {
        let mut v: Vec<_> = books
            .iter()
            .flat_map(|b| b.orders_for_trader(trader))
            .map(|o| {
                (
                    format!("0x{:x}", o.id),
                    hex_fp(o.price),
                    hex_fp(o.remaining_qty),
                    if o.side == torus_types::Side::Buy {
                        "buy".to_string()
                    } else {
                        "sell".to_string()
                    },
                )
            })
            .collect();
        v.sort();
        v
    };
    let expected_m1 = expect(&[&book1], &addr(1));
    let expected_all = expect(&[&book1, &book2], &addr(1));
    assert!(!expected_m1.is_empty());
    assert!(expected_all.len() > expected_m1.len());

    persist_book(&state, &mut book1);
    persist_book(&state, &mut book2);

    let (handle, sock) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{sock}"))
        .unwrap();

    let normalize = |orders: Vec<RpcOpenOrder>| -> Vec<(String, String, String, String)> {
        let mut v: Vec<_> = orders
            .into_iter()
            .map(|o| (o.order_id, o.price, o.remaining_qty, o.side))
            .collect();
        v.sort();
        v
    };

    let trader_hex = format!("{:?}", addr(1));
    let single: Vec<RpcOpenOrder> = client
        .request("torus_getOpenOrders", rpc_params![trader_hex.clone(), "1"])
        .await
        .unwrap();
    assert_eq!(normalize(single), expected_m1, "single-market open orders");

    let all: Vec<RpcOpenOrder> = client
        .request(
            "torus_getOpenOrders",
            rpc_params![trader_hex, None::<String>],
        )
        .await
        .unwrap();
    assert_eq!(normalize(all), expected_all, "all-markets open orders");

    handle.stop().unwrap();
}

/// getMarkPrice surfaces the book's last_trade_price.
#[tokio::test]
async fn oracle_get_mark_price_last_trade_from_book() {
    let (_dir, state, mempool, executor) = setup();
    let mut book = build_book(1);
    let ltp = book.last_trade_price().expect("scripted trade");
    persist_book(&state, &mut book);

    let (handle, sock) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{sock}"))
        .unwrap();
    let mp: RpcMarkPrice = client
        .request("torus_getMarkPrice", rpc_params!["1"])
        .await
        .unwrap();
    assert_eq!(mp.last_trade_price, hex_fp(ltp), "last trade price");
    handle.stop().unwrap();
}
