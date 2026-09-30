//! s80: pure builders for the `newTrades` and `userFills` WebSocket streams.
//!
//! The execution thread publishes each executed block's fills once as an
//! `Arc<BlockFills>`; every subscription turns that block into its own
//! filtered array here. Field encodings match the backfill RPCs
//! (`torus_getTradeHistory*`, `torus_getUserTrades`): ids, heights and
//! timestamps are hex, FixedPoint values are decimal strings.

use alloy_primitives::Address;
use torus_state::trade_rows::{BlockFills, TradeFill};
use torus_types::FixedPoint;

use crate::types::{
    dec_fp, hex_address, hex_u128, hex_u64, parse_address, parse_u64, RpcStreamTrade, RpcTrade,
    RpcUserFill,
};

/// A validated `torus_subscribe` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// `newTrades {marketId?}`: every fill, optionally of one market.
    NewTrades(Option<u64>),
    /// `userFills {user}`: the fills where `user` is maker or taker.
    UserFills(Address),
}

/// Parse and validate a `torus_subscribe(kind, params)` call. Runs before the
/// subscription is accepted, so a bad request is rejected, never accepted.
pub fn parse_stream_kind(
    kind: &str,
    params: Option<&serde_json::Value>,
) -> Result<StreamKind, String> {
    let params = match params {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Object(m)) => Some(m),
        Some(other) => return Err(format!("{kind}: params must be an object, got {other}")),
    };
    let field = |name: &str| -> Result<Option<&str>, String> {
        match params.and_then(|m| m.get(name)) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s)),
            Some(other) => Err(format!("{kind}: {name} must be a hex string, got {other}")),
        }
    };
    match kind {
        "newTrades" => {
            let market = field("marketId")?
                .map(|s| parse_u64(s).map_err(|e| format!("newTrades: marketId: {e}")))
                .transpose()?;
            Ok(StreamKind::NewTrades(market))
        }
        "userFills" => {
            let user = field("user")?.ok_or("userFills: params.user is required")?;
            let user = parse_address(user).map_err(|e| format!("userFills: user: {e}"))?;
            Ok(StreamKind::UserFills(user))
        }
        other => Err(format!(
            "unknown subscription kind {other:?}; expected \"newTrades\" or \"userFills\""
        )),
    }
}

/// HL-style direction of a fill for one party: `start_raw` is the party's
/// signed position before the fill, `user_bought` its side, `qty_raw` the
/// fill size.
pub fn fill_dir(start_raw: i128, user_bought: bool, qty_raw: i128) -> &'static str {
    let was_long = start_raw > 0;
    if start_raw == 0 || was_long == user_bought {
        return if user_bought {
            "Open Long"
        } else {
            "Open Short"
        };
    }
    let flips = qty_raw.unsigned_abs() > start_raw.unsigned_abs();
    match (was_long, flips) {
        (true, false) => "Close Long",
        (true, true) => "Long > Short",
        (false, false) => "Close Short",
        (false, true) => "Short > Long",
    }
}

fn side_str(bought: bool) -> String {
    if bought { "buy" } else { "sell" }.to_string()
}

/// The `newTrades` rows of one block: every fill (or only `market`'s), in
/// trade_index order. `side` is the taker's side.
pub fn trades_for_market(block: &BlockFills, market: Option<u64>) -> Vec<RpcStreamTrade> {
    // Fills are recorded in trade_index order, so no sort is needed.
    block
        .fills
        .iter()
        .filter(|f| market.is_none_or(|m| f.market == m))
        .map(|f| RpcStreamTrade {
            trade: RpcTrade {
                trade_id: hex_u128(f.trade_index as u128),
                market_id: hex_u64(f.market),
                price: dec_fp(FixedPoint::from_raw(f.price_raw)),
                quantity: dec_fp(FixedPoint::from_raw(f.qty_raw)),
                side: side_str(f.taker_side == 0),
                block_number: hex_u64(block.height),
                timestamp: hex_u64(block.timestamp),
            },
            maker: hex_address(f.maker),
            taker: hex_address(f.taker),
        })
        .collect()
}

/// The `userFills` rows of one block for `user`: one entry per (fill, role)
/// where `user` is maker or taker. A self-trade yields two entries, maker
/// first. `side`, `orderId`, `startPosition`, `closedPnl` and `dir` are the
/// user's own.
pub fn fills_for_user(block: &BlockFills, user: Address) -> Vec<RpcUserFill> {
    let mut out = Vec::new();
    for f in &block.fills {
        let taker_bought = f.taker_side == 0;
        if f.maker == user {
            out.push(user_fill(
                block,
                f,
                "maker",
                !taker_bought,
                f.maker_order_id,
                f.maker_start_raw,
                f.maker_pnl_raw,
            ));
        }
        if f.taker == user {
            out.push(user_fill(
                block,
                f,
                "taker",
                taker_bought,
                f.taker_order_id,
                f.taker_start_raw,
                f.taker_pnl_raw,
            ));
        }
    }
    out
}

/// One `userFills` row for the party with `role`, `bought`, `order_id`,
/// `start_raw` and `pnl_raw`.
fn user_fill(
    block: &BlockFills,
    f: &TradeFill,
    role: &str,
    bought: bool,
    order_id: u128,
    start_raw: i128,
    pnl_raw: i128,
) -> RpcUserFill {
    RpcUserFill {
        trade_id: hex_u128(f.trade_index as u128),
        market_id: hex_u64(f.market),
        side: side_str(bought),
        price: dec_fp(FixedPoint::from_raw(f.price_raw)),
        quantity: dec_fp(FixedPoint::from_raw(f.qty_raw)),
        role: role.to_string(),
        order_id: hex_u128(order_id),
        start_position: dec_fp(FixedPoint::from_raw(start_raw)),
        closed_pnl: dec_fp(FixedPoint::from_raw(pnl_raw)),
        dir: fill_dir(start_raw, bought, f.qty_raw).to_string(),
        block_number: hex_u64(block.height),
        timestamp: hex_u64(block.timestamp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const S: i128 = FixedPoint::SCALE;
    const A: Address = Address::repeat_byte(0xaa);
    const B: Address = Address::repeat_byte(0xbb);
    const C: Address = Address::repeat_byte(0xcc);

    /// Height 7:
    /// - #0 market 1: A (maker, long 3) sells 2.5 @100 to B (taker, flat).
    /// - #1 market 2: A (taker, long 0.5) sells 1 @50 to C (maker, short 2).
    /// - #2 market 1: B self-trades 1 @101 (maker long 2.5, taker long 1.5).
    pub(super) fn block() -> BlockFills {
        BlockFills {
            height: 7,
            timestamp: 1_700_000_007,
            fills: vec![
                TradeFill {
                    trade_index: 0,
                    market: 1,
                    maker: A,
                    taker: B,
                    price_raw: 100 * S,
                    qty_raw: 25 * S / 10,
                    taker_side: 0,
                    maker_order_id: 11,
                    taker_order_id: 22,
                    maker_start_raw: 3 * S,
                    taker_start_raw: 0,
                    maker_pnl_raw: -825 * S / 10,
                    taker_pnl_raw: 0,
                },
                TradeFill {
                    trade_index: 1,
                    market: 2,
                    maker: C,
                    taker: A,
                    price_raw: 50 * S,
                    qty_raw: S,
                    taker_side: 1,
                    maker_order_id: 33,
                    taker_order_id: 44,
                    maker_start_raw: -2 * S,
                    taker_start_raw: S / 2,
                    maker_pnl_raw: 0,
                    taker_pnl_raw: 125 * S / 100,
                },
                TradeFill {
                    trade_index: 2,
                    market: 1,
                    maker: B,
                    taker: B,
                    price_raw: 101 * S,
                    qty_raw: S,
                    taker_side: 0,
                    maker_order_id: 55,
                    taker_order_id: 66,
                    maker_start_raw: 25 * S / 10,
                    taker_start_raw: 15 * S / 10,
                    maker_pnl_raw: 0,
                    taker_pnl_raw: 0,
                },
            ],
        }
    }

    fn to_json<T: serde::Serialize>(v: &T) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }

    // --- fill_dir ---------------------------------------------------------

    #[test]
    fn streams_fill_dir_opens_from_flat() {
        assert_eq!(fill_dir(0, true, S), "Open Long");
        assert_eq!(fill_dir(0, false, S), "Open Short");
    }

    #[test]
    fn streams_fill_dir_same_direction_is_open() {
        assert_eq!(fill_dir(5 * S, true, 3 * S), "Open Long");
        assert_eq!(fill_dir(-5 * S, false, 3 * S), "Open Short");
    }

    #[test]
    fn streams_fill_dir_close_long_partly_and_exactly() {
        assert_eq!(fill_dir(8 * S, false, 2 * S), "Close Long");
        assert_eq!(fill_dir(8 * S, false, 8 * S), "Close Long");
    }

    #[test]
    fn streams_fill_dir_close_short_partly_and_exactly() {
        assert_eq!(fill_dir(-4 * S, true, S), "Close Short");
        assert_eq!(fill_dir(-4 * S, true, 4 * S), "Close Short");
    }

    #[test]
    fn streams_fill_dir_flips() {
        assert_eq!(fill_dir(6 * S, false, 10 * S), "Long > Short");
        assert_eq!(fill_dir(-6 * S, true, 10 * S), "Short > Long");
    }

    // --- trades_for_market ------------------------------------------------

    #[test]
    fn streams_trades_for_market_filters_one_market_in_order() {
        let rows = to_json(&trades_for_market(&block(), Some(1)));
        assert_eq!(
            rows,
            json!([
                {
                    "tradeId": "0x0", "marketId": "0x1",
                    "price": "100.00000000", "quantity": "2.50000000",
                    "side": "buy", "blockNumber": "0x7", "timestamp": "0x6553f107",
                    "maker": format!("0x{}", "aa".repeat(20)),
                    "taker": format!("0x{}", "bb".repeat(20)),
                },
                {
                    "tradeId": "0x2", "marketId": "0x1",
                    "price": "101.00000000", "quantity": "1.00000000",
                    "side": "buy", "blockNumber": "0x7", "timestamp": "0x6553f107",
                    "maker": format!("0x{}", "bb".repeat(20)),
                    "taker": format!("0x{}", "bb".repeat(20)),
                },
            ])
        );
    }

    #[test]
    fn streams_trades_for_market_none_keeps_all_with_taker_side() {
        let rows = trades_for_market(&block(), None);
        let ids: Vec<_> = rows.iter().map(|r| r.trade.trade_id.as_str()).collect();
        assert_eq!(ids, ["0x0", "0x1", "0x2"]);
        // #1: the taker (A) sold, so the public side is "sell".
        assert_eq!(rows[1].trade.side, "sell");
        assert_eq!(rows[1].trade.market_id, "0x2");
        assert_eq!(rows[1].maker, format!("0x{}", "cc".repeat(20)));
        assert_eq!(rows[1].taker, format!("0x{}", "aa".repeat(20)));
        assert!(trades_for_market(&block(), Some(9)).is_empty());
    }

    // --- fills_for_user ---------------------------------------------------

    #[test]
    fn streams_fills_for_user_taker_entry() {
        let rows = to_json(&fills_for_user(&block(), B));
        assert_eq!(
            rows[0],
            json!({
                "tradeId": "0x0", "marketId": "0x1", "side": "buy",
                "price": "100.00000000", "quantity": "2.50000000",
                "role": "taker", "orderId": "0x16",
                "startPosition": "0.00000000", "closedPnl": "0.00000000",
                "dir": "Open Long", "blockNumber": "0x7", "timestamp": "0x6553f107",
            })
        );
    }

    #[test]
    fn streams_fills_for_user_maker_entry_mirrors_taker() {
        let rows = to_json(&fills_for_user(&block(), A));
        assert_eq!(rows.as_array().unwrap().len(), 2);
        // #0: A is the maker, opposite the buying taker.
        assert_eq!(
            rows[0],
            json!({
                "tradeId": "0x0", "marketId": "0x1", "side": "sell",
                "price": "100.00000000", "quantity": "2.50000000",
                "role": "maker", "orderId": "0xb",
                "startPosition": "3.00000000", "closedPnl": "-82.50000000",
                "dir": "Close Long", "blockNumber": "0x7", "timestamp": "0x6553f107",
            })
        );
        // #1: A is the taker and sells 1 from long 0.5: a flip.
        assert_eq!(rows[1]["role"], "taker");
        assert_eq!(rows[1]["side"], "sell");
        assert_eq!(rows[1]["orderId"], "0x2c");
        assert_eq!(rows[1]["startPosition"], "0.50000000");
        assert_eq!(rows[1]["closedPnl"], "1.25000000");
        assert_eq!(rows[1]["dir"], "Long > Short");
        // C is the maker of #1, buying 1 against short 2.
        let c = fills_for_user(&block(), C);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].side.as_str(), c[0].dir.as_str()),
            ("buy", "Close Short")
        );
        assert_eq!(c[0].start_position, "-2.00000000");
    }

    #[test]
    fn streams_fills_for_user_self_trade_is_maker_then_taker() {
        let rows = fills_for_user(&block(), B);
        assert_eq!(rows.len(), 3);
        let (m, t) = (&rows[1], &rows[2]);
        assert_eq!((m.trade_id.as_str(), t.trade_id.as_str()), ("0x2", "0x2"));
        assert_eq!((m.role.as_str(), m.side.as_str()), ("maker", "sell"));
        assert_eq!(
            (m.order_id.as_str(), m.start_position.as_str()),
            ("0x37", "2.50000000")
        );
        assert_eq!(m.dir, "Close Long");
        assert_eq!((t.role.as_str(), t.side.as_str()), ("taker", "buy"));
        assert_eq!(
            (t.order_id.as_str(), t.start_position.as_str()),
            ("0x42", "1.50000000")
        );
        assert_eq!(t.dir, "Open Long");
    }

    #[test]
    fn streams_fills_for_absent_user_is_empty() {
        assert!(fills_for_user(&block(), Address::repeat_byte(0xdd)).is_empty());
    }

    // --- parse_stream_kind -------------------------------------------------

    #[test]
    fn streams_parse_new_trades() {
        assert_eq!(
            parse_stream_kind("newTrades", None),
            Ok(StreamKind::NewTrades(None))
        );
        assert_eq!(
            parse_stream_kind("newTrades", Some(&json!(null))),
            Ok(StreamKind::NewTrades(None))
        );
        assert_eq!(
            parse_stream_kind("newTrades", Some(&json!({}))),
            Ok(StreamKind::NewTrades(None))
        );
        assert_eq!(
            parse_stream_kind("newTrades", Some(&json!({"marketId": "0x2"}))),
            Ok(StreamKind::NewTrades(Some(2)))
        );
        assert!(parse_stream_kind("newTrades", Some(&json!({"marketId": "zz"}))).is_err());
        assert!(parse_stream_kind("newTrades", Some(&json!({"marketId": 2}))).is_err());
        assert!(parse_stream_kind("newTrades", Some(&json!([1]))).is_err());
    }

    #[test]
    fn streams_parse_user_fills() {
        let user = format!("0x{}", "aa".repeat(20));
        assert_eq!(
            parse_stream_kind("userFills", Some(&json!({ "user": user }))),
            Ok(StreamKind::UserFills(A))
        );
        assert!(parse_stream_kind("userFills", None).is_err());
        assert!(parse_stream_kind("userFills", Some(&json!({}))).is_err());
        assert!(parse_stream_kind("userFills", Some(&json!({"user": "0x1234"}))).is_err());
        assert!(parse_stream_kind("userFills", Some(&json!({"user": 5}))).is_err());
    }

    #[test]
    fn streams_parse_unknown_kind_is_rejected() {
        let err = parse_stream_kind("allMids", None).unwrap_err();
        assert!(err.contains("allMids"), "{err}");
    }
}

/// End-to-end `torus_subscribe` tests over a raw WebSocket, so the exact
/// frames (including the lag close notification) are visible.
#[cfg(test)]
mod subscribe_ws_tests {
    use super::*;
    use crate::{BlockNotifier, RpcServer};
    use jsonrpsee::client_transport::ws::{Url, WsTransportClientBuilder};
    use jsonrpsee::core::client::{ReceivedMessage, TransportReceiverT, TransportSenderT};
    use jsonrpsee::server::ServerHandle;
    use serde_json::{json, Value};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
    use torus_mempool::{Mempool, MempoolConfig};
    use torus_state::StateDb;

    struct Node {
        _dir: TempDir,
        _handle: ServerHandle,
        addr: SocketAddr,
        notifier: BlockNotifier,
        subs: Arc<AtomicUsize>,
    }

    async fn start(notifier: BlockNotifier) -> Node {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        let mempool = Arc::new(Mempool::new(state.clone(), MempoolConfig::default()));
        let executor = Arc::new(EvmExecutor::new(TORUS_CHAIN_ID));
        let server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            notifier.clone(),
        );
        let subs = server.state().active_subscriptions.clone();
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        Node {
            _dir: dir,
            _handle: handle,
            addr,
            notifier,
            subs,
        }
    }

    struct Ws<S, R> {
        tx: S,
        rx: R,
        next_id: u64,
    }

    async fn connect(addr: SocketAddr) -> Ws<impl TransportSenderT, impl TransportReceiverT> {
        let url = Url::parse(&format!("ws://{addr}")).unwrap();
        let (tx, rx) = WsTransportClientBuilder::default()
            .build(url)
            .await
            .unwrap();
        Ws { tx, rx, next_id: 0 }
    }

    impl<S: TransportSenderT, R: TransportReceiverT> Ws<S, R> {
        /// Send a request and return its response (no notification may be
        /// in flight on this connection).
        async fn request(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let id = self.next_id;
            let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            self.tx.send(req.to_string()).await.unwrap();
            let rp = self.recv().await;
            assert_eq!(rp["id"], id, "unexpected frame: {rp}");
            rp
        }

        async fn subscribe(&mut self, params: Value) -> Value {
            let rp = self.request("torus_subscribe", params).await;
            assert!(rp.get("error").is_none(), "subscribe failed: {rp}");
            rp["result"].clone()
        }

        async fn recv(&mut self) -> Value {
            self.recv_within(Duration::from_secs(5))
                .await
                .expect("timed out waiting for a frame")
        }

        /// `None` on timeout. A timed-out receive is not cancel-safe (it can
        /// leave the soketto receiver mid-frame), so use it only as the last
        /// read on a connection.
        async fn recv_within(&mut self, d: Duration) -> Option<Value> {
            match tokio::time::timeout(d, self.rx.receive()).await {
                Err(_) => None,
                Ok(Ok(ReceivedMessage::Text(t))) => Some(serde_json::from_str(&t).unwrap()),
                Ok(other) => panic!("unexpected frame: {:?}", other.map(|_| ()).err()),
            }
        }
    }

    async fn wait_subs(subs: &AtomicUsize, want: usize) {
        for _ in 0..500 {
            if subs.load(Ordering::Relaxed) == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "active subscriptions {} != {want}",
            subs.load(Ordering::Relaxed)
        );
    }

    fn to_json<T: serde::Serialize>(v: &T) -> Value {
        serde_json::to_value(v).unwrap()
    }

    /// Height 8: one fill, market 2, C (maker) and B (taker). A is absent.
    fn block8() -> BlockFills {
        let mut f = super::tests::block().fills[1];
        f.taker = Address::repeat_byte(0xbb);
        BlockFills {
            height: 8,
            timestamp: 1_700_000_008,
            fills: vec![f],
        }
    }

    const QUIET: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn new_trades_stream_delivers_one_message_per_block() {
        let node = start(BlockNotifier::new()).await;
        let mut ws = connect(node.addr).await;
        let all = ws.subscribe(json!(["newTrades"])).await;
        let m1 = ws
            .subscribe(json!(["newTrades", {"marketId": "0x1"}]))
            .await;
        wait_subs(&node.subs, 2).await;

        let (b7, b8) = (super::tests::block(), block8());
        node.notifier.notify_fills(Arc::new(super::tests::block()));
        node.notifier.notify_fills(Arc::new(block8()));

        let mut got_all = Vec::new();
        let mut got_m1 = Vec::new();
        for _ in 0..3 {
            let n = ws.recv().await;
            assert_eq!(n["method"], "torus_subscription", "{n}");
            let sub = &n["params"]["subscription"];
            if *sub == all {
                got_all.push(n["params"]["result"].clone());
            } else if *sub == m1 {
                got_m1.push(n["params"]["result"].clone());
            } else {
                panic!("unknown subscription in {n}");
            }
        }
        assert!(ws.recv_within(QUIET).await.is_none(), "extra frame");
        assert_eq!(
            got_all,
            vec![
                to_json(&trades_for_market(&b7, None)),
                to_json(&trades_for_market(&b8, None))
            ]
        );
        // Block 8 has no market-1 fill, so the filtered stream sends nothing for it.
        assert_eq!(got_m1, vec![to_json(&trades_for_market(&b7, Some(1)))]);
        assert_eq!(got_m1[0].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn user_fills_stream_filters_by_address() {
        let node = start(BlockNotifier::new()).await;
        let mut ws = connect(node.addr).await;
        let user = format!("0x{}", "aa".repeat(20));
        let sub = ws.subscribe(json!(["userFills", { "user": user }])).await;

        node.notifier.notify_fills(Arc::new(super::tests::block()));
        node.notifier.notify_fills(Arc::new(block8()));

        let n = ws.recv().await;
        assert_eq!(n["method"], "torus_subscription");
        assert_eq!(n["params"]["subscription"], sub);
        let want = fills_for_user(&super::tests::block(), Address::repeat_byte(0xaa));
        assert_eq!(n["params"]["result"], to_json(&want));
        assert_eq!(want.len(), 2);
        // A is absent from block 8: no message.
        assert!(ws.recv_within(QUIET).await.is_none(), "extra frame");
    }

    #[tokio::test]
    async fn unknown_subscription_kind_is_rejected() {
        let node = start(BlockNotifier::new()).await;
        let mut ws = connect(node.addr).await;
        let rp = ws.request("torus_subscribe", json!(["allMids"])).await;
        assert_eq!(rp["error"]["code"], -32602, "{rp}");
        assert!(
            rp["error"]["message"].as_str().unwrap().contains("allMids"),
            "{rp}"
        );
        let rp = ws
            .request("torus_subscribe", json!(["newTrades", {"marketId": "zz"}]))
            .await;
        assert_eq!(rp["error"]["code"], -32602, "{rp}");
        assert_eq!(node.subs.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn user_fills_requires_valid_address() {
        let node = start(BlockNotifier::new()).await;
        let mut ws = connect(node.addr).await;
        for params in [
            json!(["userFills"]),
            json!(["userFills", {}]),
            json!(["userFills", {"user": "0x1234"}]),
        ] {
            let rp = ws.request("torus_subscribe", params.clone()).await;
            assert_eq!(rp["error"]["code"], -32602, "{params}: {rp}");
            assert!(rp.get("result").is_none(), "{rp}");
        }
        assert_eq!(node.subs.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn lagging_subscriber_gets_error_and_is_closed() {
        let node = start(BlockNotifier::with_trade_capacity(2)).await;
        let mut ws = connect(node.addr).await;
        let sub = ws.subscribe(json!(["newTrades"])).await;
        wait_subs(&node.subs, 1).await;

        // Current-thread runtime: no await between the sends, so the
        // subscription task cannot read before all 5 are queued (2 kept).
        for h in 1..=5u64 {
            let mut b = super::tests::block();
            b.height = h;
            node.notifier.notify_fills(Arc::new(b));
        }

        let n = ws.recv().await;
        assert_eq!(n["method"], "torus_subscription", "{n}");
        assert_eq!(n["params"]["subscription"], sub, "{n}");
        assert!(n["params"].get("result").is_none(), "{n}");
        assert_eq!(
            n["params"]["error"],
            "subscriber lagged: 3 blocks dropped; resubscribe and backfill with \
             torus_getTradeHistoryRange / torus_getUserTrades",
            "{n}"
        );
        wait_subs(&node.subs, 0).await;
        // The subscription is gone: a new block sends nothing, so the next
        // frame is the unsubscribe reply, which finds no subscription.
        node.notifier.notify_fills(Arc::new(super::tests::block()));
        let rp = ws.request("torus_unsubscribe", json!([sub])).await;
        assert_eq!(rp["result"], false, "{rp}");
        assert!(ws.recv_within(QUIET).await.is_none(), "frame after close");
    }

    /// jsonrpsee's own client drops the close error's text (it only removes
    /// the subscription), so a Rust subscriber sees the stream end. Raw
    /// WebSocket clients get the text (see the test above).
    #[tokio::test]
    async fn lagging_subscriber_on_jsonrpsee_client_sees_stream_end() {
        use jsonrpsee::core::client::SubscriptionClientT;
        let node = start(BlockNotifier::with_trade_capacity(2)).await;
        let client = jsonrpsee::ws_client::WsClientBuilder::default()
            .build(format!("ws://{}", node.addr))
            .await
            .unwrap();
        let mut sub: jsonrpsee::core::client::Subscription<Value> = client
            .subscribe(
                "torus_subscribe",
                jsonrpsee::rpc_params!["newTrades"],
                "torus_unsubscribe",
            )
            .await
            .unwrap();
        wait_subs(&node.subs, 1).await;
        for _ in 0..5 {
            node.notifier.notify_fills(Arc::new(super::tests::block()));
        }
        let next = tokio::time::timeout(Duration::from_secs(5), sub.next())
            .await
            .expect("stream did not end");
        assert!(next.is_none(), "expected end of stream, got {next:?}");
        wait_subs(&node.subs, 0).await;
    }

    #[tokio::test]
    async fn subscription_counter_returns_to_zero() {
        let node = start(BlockNotifier::new()).await;
        let mut ws = connect(node.addr).await;
        let user = format!("0x{}", "aa".repeat(20));
        let a = ws.subscribe(json!(["newTrades"])).await;
        ws.subscribe(json!(["userFills", { "user": user }])).await;
        wait_subs(&node.subs, 2).await;

        // Unsubscribe with no block ever published: the task must still exit.
        let rp = ws.request("torus_unsubscribe", json!([a])).await;
        assert_eq!(rp["result"], true, "{rp}");
        wait_subs(&node.subs, 1).await;

        // Closing the connection ends the other one.
        drop(ws);
        wait_subs(&node.subs, 0).await;
    }
}
