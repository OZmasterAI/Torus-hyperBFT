//! s80: builders for the `newTrades` and `userFills` WebSocket streams.
//!
//! The execution thread publishes each executed block's fills once as an
//! `Arc<BlockFills>`; the notifier wraps it in a [`StreamBlock`]. A
//! `newTrades` array is serialized once per (block, filter) and shared by
//! every subscriber with that filter; `userFills` arrays are built per
//! subscriber. Field encodings match the backfill RPCs
//! (`torus_getTradeHistory*`, `torus_getUserTrades`): ids, heights and
//! timestamps are hex, FixedPoint values are decimal strings.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::Address;
use serde_json::value::RawValue;
use tokio::sync::OnceCell;
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

/// Node-local switch for all-markets `newTrades` (s80 fix 2).
pub const ENV_ALL_MARKET_TRADES: &str = "TORUS_ALL_MARKET_TRADES";

/// The `-32602` message for an all-markets `newTrades` on a node that has it off.
pub const ALL_MARKET_TRADES_DISABLED: &str =
    "newTrades without marketId is disabled on this node (validator); subscribe per \
     market or use an RPC node; operators: TORUS_ALL_MARKET_TRADES=1";

/// Whether all-markets `newTrades` is allowed, from `TORUS_ALL_MARKET_TRADES`
/// (`env`) and the node mode: unset → only on `--rpc-only` nodes; `"1"` → on;
/// `"0"` → off. Any other value logs a warning and uses the default.
pub fn all_market_trades_allowed(env: Option<&str>, rpc_only: bool) -> bool {
    match env.map(str::trim) {
        None => rpc_only,
        Some("1") => true,
        Some("0") => false,
        Some(other) => {
            tracing::warn!(
                "{ENV_ALL_MARKET_TRADES}={other:?} is not 0 or 1; using the default \
                 (all-markets newTrades {})",
                if rpc_only {
                    "on: RPC node"
                } else {
                    "off: validator"
                }
            );
            rpc_only
        }
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

/// A serialized `newTrades` array, or `None` when the filter matched no fill.
pub type TradesPayload = Option<Arc<RawValue>>;

/// One executed block as broadcast to the stream subscriptions (s80 fix 2).
///
/// Created empty by the notifier (O(1) on the execution thread). The first
/// subscription task that needs a `newTrades` filter serializes it; every
/// other subscriber with that filter reuses the same bytes.
pub struct StreamBlock {
    pub fills: Arc<BlockFills>,
    all_markets: OnceCell<TradesPayload>,
    /// The lock is held only to find or insert a market's cell, never while
    /// serializing.
    per_market: Mutex<HashMap<u64, Arc<OnceCell<TradesPayload>>>>,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
}

impl StreamBlock {
    pub fn new(fills: Arc<BlockFills>) -> Self {
        Self {
            fills,
            all_markets: OnceCell::new(),
            per_market: Mutex::new(HashMap::new()),
            #[cfg(test)]
            builds: Default::default(),
        }
    }

    /// The `newTrades` array of this block for `market` (all markets when
    /// `None`), serialized once per filter. Concurrent callers with the same
    /// filter wait for the first one's result.
    pub async fn new_trades_payload(
        &self,
        market: Option<u64>,
    ) -> Result<TradesPayload, serde_json::Error> {
        let build = || async { self.build(market) };
        match market {
            None => self.all_markets.get_or_try_init(build).await.cloned(),
            Some(m) => {
                let cell = self
                    .per_market
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(m)
                    .or_default()
                    .clone();
                cell.get_or_try_init(build).await.cloned()
            }
        }
    }

    fn build(&self, market: Option<u64>) -> Result<TradesPayload, serde_json::Error> {
        #[cfg(test)]
        self.builds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rows = trades_for_market(&self.fills, market);
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(Arc::from(serde_json::value::to_raw_value(&rows)?)))
    }

    /// How many payloads this block has serialized (tests only).
    #[cfg(test)]
    pub(crate) fn payload_builds(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The `userFills` rows of one block for `user`: one entry per (fill, role)
/// where `user` is maker or taker. `side`, `orderId`, `startPosition`,
/// `closedPnl` and `dir` are the user's own. Self-trades cannot happen (the
/// book's self-trade prevention cancels the resting order); if a fill ever had
/// `user` on both sides it would yield two entries, maker then taker.
///
/// The per-user fields come from `block.extras`. A block without them (it
/// executed before anyone subscribed) yields nothing.
pub fn fills_for_user(block: &BlockFills, user: Address) -> Vec<RpcUserFill> {
    let mut out = Vec::new();
    if block.extras.len() != block.fills.len() {
        return out;
    }
    for (f, e) in block.fills.iter().zip(&block.extras) {
        let taker_bought = f.taker_side == 0;
        if f.maker == user {
            out.push(user_fill(
                block,
                f,
                "maker",
                !taker_bought,
                e.maker_order_id,
                e.maker_start_raw,
                e.maker_pnl_raw,
            ));
        }
        if f.taker == user {
            out.push(user_fill(
                block,
                f,
                "taker",
                taker_bought,
                e.taker_order_id,
                e.taker_start_raw,
                e.taker_pnl_raw,
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
    use torus_state::trade_rows::FillExtras;

    const S: i128 = FixedPoint::SCALE;
    const A: Address = Address::repeat_byte(0xaa);
    const B: Address = Address::repeat_byte(0xbb);
    const C: Address = Address::repeat_byte(0xcc);

    /// Height 7:
    /// - #0 market 1: A (maker, long 3) sells 2.5 @100 to B (taker, flat).
    /// - #1 market 2: A (taker, long 0.5) sells 1 @50 to C (maker, short 2).
    /// - #2 market 1: B self-trades 1 @101 (maker long 2.5, taker long 1.5).
    pub(super) fn block() -> BlockFills {
        let fill = |trade_index, market, maker, taker, price_raw, qty_raw, taker_side| TradeFill {
            trade_index,
            market,
            maker,
            taker,
            price_raw,
            qty_raw,
            taker_side,
        };
        BlockFills {
            height: 7,
            timestamp: 1_700_000_007,
            fills: vec![
                fill(0, 1, A, B, 100 * S, 25 * S / 10, 0),
                fill(1, 2, C, A, 50 * S, S, 1),
                fill(2, 1, B, B, 101 * S, S, 0),
            ],
            extras: vec![
                FillExtras {
                    maker_order_id: 11,
                    taker_order_id: 22,
                    maker_start_raw: 3 * S,
                    taker_start_raw: 0,
                    maker_pnl_raw: -825 * S / 10,
                    taker_pnl_raw: 0,
                },
                FillExtras {
                    maker_order_id: 33,
                    taker_order_id: 44,
                    maker_start_raw: -2 * S,
                    taker_start_raw: S / 2,
                    maker_pnl_raw: 0,
                    taker_pnl_raw: 125 * S / 100,
                },
                FillExtras {
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

    /// s80 fix 1: a block executed before anyone subscribed has no extras;
    /// `userFills` skips it, `newTrades` still delivers it.
    #[test]
    fn streams_block_without_extras_skips_user_fills_only() {
        let full = block();
        let mut bare = block();
        bare.extras.clear();
        for user in [A, B, C] {
            assert!(fills_for_user(&bare, user).is_empty());
            assert!(!fills_for_user(&full, user).is_empty());
        }
        assert_eq!(
            to_json(&trades_for_market(&bare, None)),
            to_json(&trades_for_market(&full, None))
        );
    }

    // --- StreamBlock (s80 fix 2) --------------------------------------------

    /// Each `newTrades` filter is serialized once per block: later calls get
    /// the same cached bytes, and those bytes are exactly what the old
    /// per-subscriber serialization produced.
    #[tokio::test]
    async fn streams_stream_block_serializes_each_filter_once() {
        let sb = StreamBlock::new(std::sync::Arc::new(block()));
        assert_eq!(sb.payload_builds(), 0, "nothing is built up front");

        let m1 = sb.new_trades_payload(Some(1)).await.unwrap().unwrap();
        let m1_again = sb.new_trades_payload(Some(1)).await.unwrap().unwrap();
        assert!(std::sync::Arc::ptr_eq(&m1, &m1_again));
        assert_eq!(
            m1.get(),
            serde_json::to_string(&trades_for_market(&block(), Some(1))).unwrap()
        );

        let all = sb.new_trades_payload(None).await.unwrap().unwrap();
        let all_again = sb.new_trades_payload(None).await.unwrap().unwrap();
        assert!(std::sync::Arc::ptr_eq(&all, &all_again));
        assert_eq!(
            all.get(),
            serde_json::to_string(&trades_for_market(&block(), None)).unwrap()
        );

        // A market without fills yields no payload, also computed once.
        assert!(sb.new_trades_payload(Some(9)).await.unwrap().is_none());
        assert!(sb.new_trades_payload(Some(9)).await.unwrap().is_none());
        assert_eq!(sb.payload_builds(), 3, "one build per (block, filter)");
    }

    // --- all_market_trades_allowed (s80 fix 2) ------------------------------

    #[test]
    fn streams_all_market_trades_flag() {
        for (env, rpc_only, want) in [
            (None, false, false),
            (None, true, true),
            (Some("1"), false, true),
            (Some("1"), true, true),
            (Some("0"), false, false),
            (Some("0"), true, false),
            // Anything else warns and falls back to the default.
            (Some("yes"), false, false),
            (Some("yes"), true, true),
            (Some(""), false, false),
        ] {
            assert_eq!(
                all_market_trades_allowed(env, rpc_only),
                want,
                "env={env:?} rpc_only={rpc_only}"
            );
        }
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
        start_with(notifier, None).await
    }

    /// A node configured like `torus-node` with `TORUS_ALL_MARKET_TRADES=env`
    /// on a validator (`rpc_only = false`) or an RPC node.
    async fn start_node(env: Option<&str>, rpc_only: bool) -> Node {
        let allowed = all_market_trades_allowed(env, rpc_only);
        start_with(BlockNotifier::new(), Some(allowed)).await
    }

    async fn start_with(notifier: BlockNotifier, all_market_trades: Option<bool>) -> Node {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        let mempool = Arc::new(Mempool::new(state.clone(), MempoolConfig::default()));
        let executor = Arc::new(EvmExecutor::new(TORUS_CHAIN_ID));
        let mut server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            notifier.clone(),
        );
        if let Some(allowed) = all_market_trades {
            server.set_all_market_trades(allowed);
        }
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

        /// The next notification as `(subscription, raw result text, frame
        /// text)`, with the bytes exactly as sent.
        async fn recv_raw_result(&mut self) -> (Value, String, String) {
            #[derive(serde::Deserialize)]
            struct Frame {
                params: Params,
            }
            #[derive(serde::Deserialize)]
            struct Params {
                subscription: Value,
                result: Box<serde_json::value::RawValue>,
            }
            let t = match tokio::time::timeout(Duration::from_secs(5), self.rx.receive()).await {
                Ok(Ok(ReceivedMessage::Text(t))) => t,
                _ => panic!("no text frame"),
            };
            let f: Frame = serde_json::from_str(&t).unwrap_or_else(|e| panic!("{e}: {t}"));
            (f.params.subscription, f.params.result.get().to_string(), t)
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
        let b7 = super::tests::block();
        let mut f = b7.fills[1];
        f.taker = Address::repeat_byte(0xbb);
        BlockFills {
            height: 8,
            timestamp: 1_700_000_008,
            fills: vec![f],
            extras: vec![b7.extras[1]],
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

    // --- s80 fix 2: TORUS_ALL_MARKET_TRADES ----------------------------------

    /// A validator (default flag) rejects all-markets `newTrades` before
    /// accepting it, and still serves per-market `newTrades` and `userFills`.
    #[tokio::test]
    async fn validator_rejects_all_market_trades_by_default() {
        let node = start_node(None, false).await;
        let mut ws = connect(node.addr).await;
        for params in [json!(["newTrades"]), json!(["newTrades", {}])] {
            let rp = ws.request("torus_subscribe", params.clone()).await;
            assert_eq!(rp["error"]["code"], -32602, "{params}: {rp}");
            assert_eq!(
                rp["error"]["message"],
                "invalid params: newTrades without marketId is disabled on this node \
                 (validator); subscribe per market or use an RPC node; operators: \
                 TORUS_ALL_MARKET_TRADES=1",
                "{rp}"
            );
        }
        assert_eq!(node.subs.load(Ordering::Relaxed), 0);

        let m1 = ws
            .subscribe(json!(["newTrades", {"marketId": "0x1"}]))
            .await;
        let user = format!("0x{}", "aa".repeat(20));
        let uf = ws.subscribe(json!(["userFills", { "user": user }])).await;
        wait_subs(&node.subs, 2).await;
        node.notifier.notify_fills(Arc::new(super::tests::block()));
        let mut got = [false, false];
        for _ in 0..2 {
            let n = ws.recv().await;
            let sub = &n["params"]["subscription"];
            got[usize::from(*sub == uf)] = true;
            assert!(*sub == m1 || *sub == uf, "{n}");
        }
        assert_eq!(got, [true, true]);
    }

    /// The other flag cases: an RPC node allows all-markets by default, "1"
    /// allows it on a validator, "0" rejects it on an RPC node.
    #[tokio::test]
    async fn all_market_trades_flag_overrides_node_default() {
        for (env, rpc_only, allowed) in [
            (None, true, true),
            (Some("1"), false, true),
            (Some("0"), true, false),
        ] {
            let node = start_node(env, rpc_only).await;
            let mut ws = connect(node.addr).await;
            let rp = ws.request("torus_subscribe", json!(["newTrades"])).await;
            let case = format!("env={env:?} rpc_only={rpc_only}: {rp}");
            if allowed {
                assert!(rp.get("error").is_none(), "{case}");
                wait_subs(&node.subs, 1).await;
            } else {
                assert_eq!(rp["error"]["code"], -32602, "{case}");
                assert_eq!(node.subs.load(Ordering::Relaxed), 0, "{case}");
                // Per-market stays allowed.
                ws.subscribe(json!(["newTrades", {"marketId": "0x1"}]))
                    .await;
                wait_subs(&node.subs, 1).await;
            }
        }
    }

    /// Subscribers with the same filter receive byte-identical `result`
    /// payloads (equal to the old per-subscriber serialization), and each
    /// (block, filter) payload is built once.
    #[tokio::test]
    async fn same_filter_subscribers_share_one_payload() {
        let node = start_node(None, true).await;
        let mut ws = connect(node.addr).await;
        let m1a = ws
            .subscribe(json!(["newTrades", {"marketId": "0x1"}]))
            .await;
        let m1b = ws
            .subscribe(json!(["newTrades", {"marketId": "0x1"}]))
            .await;
        let alla = ws.subscribe(json!(["newTrades"])).await;
        let allb = ws.subscribe(json!(["newTrades", {}])).await;
        wait_subs(&node.subs, 4).await;
        // Observe the published block to inspect its payload cache.
        let mut probe = node.notifier.new_trades.subscribe();

        node.notifier.notify_fills(Arc::new(super::tests::block()));
        let mut frames = std::collections::HashMap::new();
        for _ in 0..4 {
            let (sub, result, frame) = ws.recv_raw_result().await;
            // The whole frame is byte-for-byte the pre-fix-2 layout.
            assert_eq!(
                frame,
                format!(
                    r#"{{"jsonrpc":"2.0","method":"torus_subscription","params":{{"subscription":{sub},"result":{result}}}}}"#
                )
            );
            assert!(frames.insert(sub.to_string(), result).is_none());
        }
        let get = |s: &Value| frames[&s.to_string()].clone();
        let b7 = super::tests::block();
        let want_m1 = serde_json::to_string(&trades_for_market(&b7, Some(1))).unwrap();
        let want_all = serde_json::to_string(&trades_for_market(&b7, None)).unwrap();
        assert_eq!(get(&m1a), want_m1);
        assert_eq!(get(&m1b), want_m1);
        assert_eq!(get(&alla), want_all);
        assert_eq!(get(&allb), want_all);

        let sb = probe.recv().await.unwrap();
        assert_eq!(
            sb.payload_builds(),
            2,
            "one build per filter, not per subscriber"
        );
        let p1 = sb.new_trades_payload(Some(1)).await.unwrap().unwrap();
        let p2 = sb.new_trades_payload(Some(1)).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&p1, &p2));
        assert_eq!(p1.get(), want_m1);
        assert_eq!(sb.payload_builds(), 2);
    }
}
