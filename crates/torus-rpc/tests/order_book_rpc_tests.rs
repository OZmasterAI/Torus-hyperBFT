//! Integration tests for the `getOrderBook` RPC handler's decode path.
//!
//! Deep-book storage round: the handler reconstructs books from PER-ORDER
//! ROWS (`torus_core::order_book_store`). A pre-round monolithic value
//! (either historical format) must error LOUDLY — the CF no longer holds
//! whole-book blobs, and silently misreading one would hide real book state.

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

/// The rows `save_order_books` actually writes decode into correctly-ordered,
/// quantity-summed price levels. (Assertions unchanged from the monolithic
/// baseline; only the persist path moved to the row store.)
#[tokio::test]
async fn get_order_book_decodes_persisted_book_rows() {
    let (_dir, state, mempool, executor) = setup();

    // Build a non-trivial book: two bids at the same price (must sum) + one lower
    // bid; two asks at distinct prices. Bids below asks so nothing crosses.
    let mut book = OrderBook::new(1, fp(1), fp(1));
    book.place_order(limit(1, true, 100, 5), torus_types::Address::from([1; 20]), 1);
    book.place_order(limit(1, true, 100, 3), torus_types::Address::from([2; 20]), 1);
    book.place_order(limit(1, true, 99, 2), torus_types::Address::from([3; 20]), 1);
    book.place_order(limit(1, false, 101, 4), torus_types::Address::from([4; 20]), 1);
    book.place_order(limit(1, false, 102, 1), torus_types::Address::from([5; 20]), 1);
    torus_core::order_book_store::save_book_full(&state, &mut book).unwrap();

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

/// LEGACY monolithic values (both historical formats) now error LOUDLY —
/// they are never silently decoded or reported as an empty book.
#[tokio::test]
async fn get_order_book_errors_loudly_on_legacy_values() {
    let (_dir, state, mempool, executor) = setup();

    // Historical format 1: a whole-book borsh-`OrderBook` blob.
    let mut legacy_book = OrderBook::new(2, fp(1), fp(1));
    legacy_book.place_order(limit(2, true, 50, 7), torus_types::Address::from([1; 20]), 1);
    state
        .put_cf_raw(
            CF_NATIVE_ORDER_BOOKS,
            &2u64.to_be_bytes(),
            &borsh::to_vec(&legacy_book).unwrap(),
        )
        .unwrap();

    // Historical format 2: an `OrderBookSnapshot` blob (test-only writer era).
    let snapshot = OrderBookSnapshot {
        bids: vec![PriceLevel {
            price: fp(50),
            quantity: fp(7),
        }],
        asks: vec![],
    };
    state
        .put_cf_raw(
            CF_NATIVE_ORDER_BOOKS,
            &3u64.to_be_bytes(),
            &borsh::to_vec(&snapshot).unwrap(),
        )
        .unwrap();

    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    for mid in ["2", "3"] {
        let res: Result<RpcOrderBook, _> = client
            .request("torus_getOrderBook", rpc_params![mid])
            .await;
        assert!(
            res.is_err(),
            "market {mid}: legacy monolithic value must error, not decode silently"
        );
    }

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
