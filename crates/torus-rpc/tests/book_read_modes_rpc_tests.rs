//! `torus_getOrderBook` / `torus_getOpenOrders` / `torus_getMarkPrice` must
//! serve the book layout that is actually ON DISK — Classic whole-book blobs
//! (`TORUS_BOOK_ROWS` unset), per-order rows (=1) and level-authority rows
//! (=2) — instead of silently returning an empty book / `[]` / `0` for the two
//! row layouts.
//!
//! Fixtures are written by the REAL executor save path
//! (`NativeExecContext::new_with_mode` + `save_order_books`), so these tests
//! pin reader/writer agreement, not a hand-rolled guess at the layout.

use std::net::SocketAddr;
use std::sync::Arc;

use alloy_primitives::Address;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;
use jsonrpsee::server::ServerHandle;
use tempfile::TempDir;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_rpc::types::{hex_fp, RpcMarkPrice, RpcOpenOrder, RpcOrderBook};
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const MAKER: u8 = 1;
const TAKER: u8 = 3;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
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

/// Real order flow, persisted by the real save path in `mode`.
/// Market 1 ends with bids 100 (qty 3, after a partial fill) and 99 (qty 7),
/// ask 105 (qty 4); last trade price 100. Market 2 holds one bid.
fn seed(db: &StateDb, mode: BookMode) {
    let maker = addr(MAKER);
    let taker = addr(TAKER);
    let mut ctx = NativeExecContext::new_with_mode(
        db.clone(),
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
    assert!(ctx.fatal_error.is_none(), "seed load: {:?}", ctx.fatal_error);
    for t in [maker, taker] {
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
    let block = vec![
        (maker, NativeAction::PlaceOrder(gtc(1, true, 100, 5))),
        (maker, NativeAction::PlaceOrder(gtc(1, true, 99, 7))),
        (maker, NativeAction::PlaceOrder(gtc(1, false, 105, 4))),
        (maker, NativeAction::PlaceOrder(gtc(2, true, 50, 3))),
        (taker, NativeAction::PlaceOrder(gtc(1, false, 100, 2))),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &block);
    assert!(r.results.iter().all(|x| x.success), "seed block failed");
    ctx.save_order_books();
}

const MODES: [BookMode; 3] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
];

#[tokio::test]
async fn get_order_book_serves_every_mode() {
    for mode in MODES {
        let (_dir, state, mempool, executor) = setup();
        seed(&state, mode);
        let (handle, sock) = start_server(state, mempool, executor).await;
        let client = HttpClientBuilder::default()
            .build(format!("http://{sock}"))
            .unwrap();

        let book: RpcOrderBook = client
            .request("torus_getOrderBook", rpc_params!["0x1"])
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: getOrderBook failed: {e}"));

        assert_eq!(book.bids.len(), 2, "{mode:?}: two bid levels");
        assert_eq!(book.bids[0].price, hex_fp(fp(100)), "{mode:?}: best bid first");
        assert_eq!(book.bids[0].quantity, hex_fp(fp(3)), "{mode:?}: post-fill qty");
        assert_eq!(book.bids[0].order_count, 1, "{mode:?}");
        assert_eq!(book.bids[1].price, hex_fp(fp(99)), "{mode:?}");
        assert_eq!(book.bids[1].quantity, hex_fp(fp(7)), "{mode:?}");
        assert_eq!(book.asks.len(), 1, "{mode:?}: one ask level");
        assert_eq!(book.asks[0].price, hex_fp(fp(105)), "{mode:?}");
        assert_eq!(book.asks[0].quantity, hex_fp(fp(4)), "{mode:?}");

        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn get_open_orders_serves_every_mode() {
    for mode in MODES {
        let (_dir, state, mempool, executor) = setup();
        seed(&state, mode);
        let (handle, sock) = start_server(state, mempool, executor).await;
        let client = HttpClientBuilder::default()
            .build(format!("http://{sock}"))
            .unwrap();
        let maker = format!("0x{}", hex::encode(addr(MAKER).as_slice()));

        // Single market.
        let one: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", rpc_params![maker.clone(), "0x1"])
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: getOpenOrders(market) failed: {e}"));
        assert_eq!(one.len(), 3, "{mode:?}: 3 resting maker orders on market 1");

        // All markets — the branch that used to hard-filter `key.len() != 8`
        // and therefore ALWAYS returned [] under modes 1 and 2.
        let all: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", rpc_params![maker])
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: getOpenOrders(all) failed: {e}"));
        assert_eq!(all.len(), 4, "{mode:?}: 3 on market 1 + 1 on market 2");
        assert!(
            all.iter().any(|o| o.market_id == "0x2"),
            "{mode:?}: market 2 order missing"
        );

        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn get_mark_price_reports_last_trade_price_in_every_mode() {
    for mode in MODES {
        let (_dir, state, mempool, executor) = setup();
        seed(&state, mode);
        let (handle, sock) = start_server(state, mempool, executor).await;
        let client = HttpClientBuilder::default()
            .build(format!("http://{sock}"))
            .unwrap();

        let mp: RpcMarkPrice = client
            .request("torus_getMarkPrice", rpc_params!["0x1"])
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: getMarkPrice failed: {e}"));
        assert_eq!(
            mp.last_trade_price,
            hex_fp(fp(100)),
            "{mode:?}: last trade price must come off the persisted book"
        );

        // Market 2 never traded — zero, and still not an error.
        let mp2: RpcMarkPrice = client
            .request("torus_getMarkPrice", rpc_params!["0x2"])
            .await
            .unwrap();
        assert_eq!(mp2.last_trade_price, hex_fp(FixedPoint::ZERO), "{mode:?}");

        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn get_order_book_errors_instead_of_returning_an_empty_book() {
    // A mode-2 DB whose root CF holds level rows for a market with NO meta row
    // is undecodable. The handler must surface an error, never `{bids: [],
    // asks: []}` — the silent-empty behaviour IS the bug.
    let (_dir, state, mempool, executor) = setup();
    seed(&state, BookMode::LevelAuthority);
    state
        .delete_cf_raw(
            torus_state::cf::CF_NATIVE_ORDER_BOOKS,
            &torus_core::book_rows::book_meta_key(1),
        )
        .unwrap();

    let (handle, sock) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{sock}"))
        .unwrap();
    let res: Result<RpcOrderBook, _> = client
        .request("torus_getOrderBook", rpc_params!["0x1"])
        .await;
    assert!(
        res.is_err(),
        "undecodable layout must be an RPC error, got {res:?}"
    );
    handle.stop().unwrap();
}
