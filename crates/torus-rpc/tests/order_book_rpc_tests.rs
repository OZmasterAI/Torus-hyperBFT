//! Integration tests for the `getOrderBook` RPC handler's decode path.
//!
//! Regression: `save_order_books` persists a borsh-`OrderBook` blob, but the
//! handler used to decode every value as `OrderBookSnapshot`, so non-trivial
//! production books failed to borsh-decode. The handler now decodes `OrderBook`
//! first (aggregating resting orders into price levels) and falls back to the
//! historical `OrderBookSnapshot` layout.

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;
use jsonrpsee::server::ServerHandle;
use tempfile::TempDir;

use torus_core::order_book::OrderBook;
use torus_core::precompiles::{OrderBookSnapshot, PriceLevel};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_rpc::types::{hex_fp, RpcOrderBook};
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::StateDb;
use torus_types::{FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
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

fn write_raw(state: &StateDb, market_id: u64, bytes: &[u8]) {
    state
        .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes(), bytes)
        .unwrap();
}

/// The value `save_order_books` actually writes — a borsh-`OrderBook` blob with
/// resting orders — decodes into correctly-ordered, quantity-summed price levels.
#[tokio::test]
async fn get_order_book_decodes_persisted_orderbook_blob() {
    let (_dir, state, mempool, executor) = setup();

    // Build a non-trivial book: two bids at the same price (must sum) + one lower
    // bid; two asks at distinct prices. Bids below asks so nothing crosses.
    let mut book = OrderBook::new(1, fp(1), fp(1));
    book.place_order(limit(1, true, 100, 5), torus_types::Address::from([1; 20]), 1);
    book.place_order(limit(1, true, 100, 3), torus_types::Address::from([2; 20]), 1);
    book.place_order(limit(1, true, 99, 2), torus_types::Address::from([3; 20]), 1);
    book.place_order(limit(1, false, 101, 4), torus_types::Address::from([4; 20]), 1);
    book.place_order(limit(1, false, 102, 1), torus_types::Address::from([5; 20]), 1);
    write_raw(&state, 1, &borsh::to_vec(&book).unwrap());

    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let ob: RpcOrderBook = client
        .request("torus_getOrderBook", rpc_params!["1"])
        .await
        .unwrap();

    // Bids DESCENDING (best first), quantities summed per level.
    assert_eq!(ob.bids.len(), 2, "two bid levels");
    assert_eq!(ob.bids[0].price, hex_fp(fp(100)));
    assert_eq!(ob.bids[0].quantity, hex_fp(fp(8)), "5 + 3 summed at price 100");
    assert_eq!(ob.bids[1].price, hex_fp(fp(99)));
    assert_eq!(ob.bids[1].quantity, hex_fp(fp(2)));

    // Asks ASCENDING (best first).
    assert_eq!(ob.asks.len(), 2, "two ask levels");
    assert_eq!(ob.asks[0].price, hex_fp(fp(101)));
    assert_eq!(ob.asks[0].quantity, hex_fp(fp(4)));
    assert_eq!(ob.asks[1].price, hex_fp(fp(102)));
    assert_eq!(ob.asks[1].quantity, hex_fp(fp(1)));

    handle.stop().unwrap();
}

/// A legacy `OrderBookSnapshot` blob still decodes via the fallback path.
#[tokio::test]
async fn get_order_book_falls_back_to_legacy_snapshot() {
    let (_dir, state, mempool, executor) = setup();

    let snapshot = OrderBookSnapshot {
        bids: vec![PriceLevel {
            price: fp(50),
            quantity: fp(7),
        }],
        asks: vec![PriceLevel {
            price: fp(60),
            quantity: fp(9),
        }],
    };
    write_raw(&state, 2, &borsh::to_vec(&snapshot).unwrap());

    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let ob: RpcOrderBook = client
        .request("torus_getOrderBook", rpc_params!["2"])
        .await
        .unwrap();

    assert_eq!(ob.bids.len(), 1);
    assert_eq!(ob.bids[0].price, hex_fp(fp(50)));
    assert_eq!(ob.bids[0].quantity, hex_fp(fp(7)));
    assert_eq!(ob.asks.len(), 1);
    assert_eq!(ob.asks[0].price, hex_fp(fp(60)));
    assert_eq!(ob.asks[0].quantity, hex_fp(fp(9)));

    handle.stop().unwrap();
}

/// An absent book (empty CF entry) returns empty bids/asks, unchanged.
#[tokio::test]
async fn get_order_book_empty_when_absent() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let ob: RpcOrderBook = client
        .request("torus_getOrderBook", rpc_params!["999"])
        .await
        .unwrap();
    assert!(ob.bids.is_empty());
    assert!(ob.asks.is_empty());
    handle.stop().unwrap();
}
