//! s114 cloid over RPC: `torus_getOpenOrders` shows each order's cloid as
//! `0x` + 32 hex digits, and `torus_getOrderByCloid(trader, cloid)` (HL
//! `orderStatus` by cloid) finds the trader's open order with that cloid —
//! a resting order (`"open"`) or a pending stop (`"triggerPending"`) — in
//! every book layout, or returns `null`.

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
use torus_rpc::types::{dec_fp, RpcOpenOrder};
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Above u64::MAX.
const C: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
const C_HEX: &str = "0x0123456789abcdeffedcba9876543210";
const STOP_HEX: &str = "0x0123456789abcdeffedcba9876543211";

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn hex_addr(n: u8) -> String {
    format!("0x{}", hex::encode(addr(n).as_slice()))
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn order(market_id: MarketId, price: i64, ot: OrderType, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: true,
        price: fp(price),
        quantity: fp(2),
        order_type: ot,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    })
}

/// Trader 1: a resting bid with C (market 1), a stop with C + 1 (market 2),
/// a bid without a cloid (market 1). Persisted by the real save path.
fn seed(dir: &TempDir, mode: BookMode) -> StateDb {
    let db = StateDb::open(dir.path()).unwrap();
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
    ctx.positions
        .put_native_balance(
            &addr(1),
            &NativeBalance {
                available: fp(10_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let block = vec![
        (addr(1), order(1, 100, OrderType::Limit, Some(C))),
        (
            addr(1),
            order(
                2,
                210,
                OrderType::StopMarket { trigger: fp(200) },
                Some(C + 1),
            ),
        ),
        (addr(1), order(1, 90, OrderType::Limit, None)),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &block);
    assert!(
        r.results.iter().all(|x| x.success),
        "{mode:?}: {:?}",
        r.results
    );
    ctx.save_order_books();
    db
}

async fn start_server(state: StateDb) -> (ServerHandle, SocketAddr) {
    RpcServer::new(
        state.clone(),
        Arc::new(Mempool::new(state, MempoolConfig::default())),
        Arc::new(EvmExecutor::new(TORUS_CHAIN_ID)),
        TORUS_CHAIN_ID,
        100,
        BlockNotifier::new(),
    )
    .start("127.0.0.1:0".parse().unwrap())
    .await
    .unwrap()
}

const MODES: [BookMode; 3] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
];

#[tokio::test]
async fn open_orders_and_order_by_cloid_serve_every_mode() {
    for mode in MODES {
        let dir = TempDir::new().unwrap();
        let state = seed(&dir, mode);
        let (handle, sock) = start_server(state).await;
        let client = HttpClientBuilder::default()
            .build(format!("http://{sock}"))
            .unwrap();

        let open: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", rpc_params![hex_addr(1)])
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        let mut cloids: Vec<Option<String>> =
            open.iter().map(|o| o.client_order_id.clone()).collect();
        cloids.sort();
        assert_eq!(cloids, vec![None, Some(C_HEX.to_string())], "{mode:?}");

        let get = |trader: String, cloid: &str| {
            let client = client.clone();
            let cloid = cloid.to_string();
            async move {
                client
                    .request::<Option<serde_json::Value>, _>(
                        "torus_getOrderByCloid",
                        rpc_params![trader, cloid],
                    )
                    .await
            }
        };
        let rest = get(hex_addr(1), C_HEX)
            .await
            .unwrap()
            .expect("resting order found");
        assert_eq!(rest["status"], "open", "{mode:?}: {rest}");
        assert_eq!(rest["order"]["marketId"], "0x1", "{mode:?}");
        assert_eq!(rest["order"]["clientOrderId"], C_HEX, "{mode:?}");
        assert_eq!(rest["order"]["price"], dec_fp(fp(100)), "{mode:?}");
        assert_eq!(rest["order"]["orderType"], "limit", "{mode:?}");
        assert!(
            rest.get("triggerPrice").is_none_or(|v| v.is_null()),
            "{mode:?}: {rest}"
        );

        let stop = get(hex_addr(1), STOP_HEX)
            .await
            .unwrap()
            .expect("stop found");
        assert_eq!(stop["status"], "triggerPending", "{mode:?}: {stop}");
        assert_eq!(stop["order"]["marketId"], "0x2", "{mode:?}");
        assert_eq!(stop["order"]["clientOrderId"], STOP_HEX, "{mode:?}");
        assert_eq!(stop["order"]["orderType"], "stop_market", "{mode:?}");
        assert_eq!(stop["order"]["price"], dec_fp(fp(210)), "{mode:?}: the cap");
        assert_eq!(stop["triggerPrice"], dec_fp(fp(200)), "{mode:?}");

        // Short hex forms parse; another trader or an unknown cloid: null.
        assert!(get(hex_addr(1), "0x5").await.unwrap().is_none(), "{mode:?}");
        assert!(get(hex_addr(2), C_HEX).await.unwrap().is_none(), "{mode:?}");
        // A malformed cloid is an error, not null.
        assert!(get(hex_addr(1), "nope").await.is_err(), "{mode:?}");
        assert!(
            get(hex_addr(1), "0x1ffffffffffffffffffffffffffffffff")
                .await
                .is_err(),
            "{mode:?}"
        );
        handle.stop().unwrap();
    }
}
