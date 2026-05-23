//! JSON-RPC server for the Torus-hyperBFT blockchain.
//!
//! Implements Ethereum-compatible `eth_*`, `net_*`, and `web3_*` namespaces
//! using jsonrpsee 0.26 with WebSocket subscription support.

pub mod error;
pub mod eth;
pub mod net;
pub mod torus;
pub mod types;
pub mod web3;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alloy_primitives::B256;
use jsonrpsee::server::middleware::rpc::{self as rpc_mw, RpcServiceT};
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use tokio::sync::broadcast;
use torus_evm::EvmExecutor;
use torus_mempool::Mempool;
use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::StateDb;

use crate::eth::EthApiServer;
use crate::net::NetApiServer;
use crate::torus::TorusApiServer;
use crate::web3::Web3ApiServer;

/// Broadcast channels for WebSocket subscriptions.
#[derive(Clone)]
pub struct BlockNotifier {
    pub new_heads: broadcast::Sender<serde_json::Value>,
    pub new_logs: broadcast::Sender<Vec<serde_json::Value>>,
    pub pending_txs: broadcast::Sender<B256>,
    pub new_trades: broadcast::Sender<Vec<serde_json::Value>>,
}

impl BlockNotifier {
    pub fn new() -> Self {
        let (new_heads, _) = broadcast::channel(256);
        let (new_logs, _) = broadcast::channel(256);
        let (pending_txs, _) = broadcast::channel(1024);
        let (new_trades, _) = broadcast::channel(256);
        Self {
            new_heads,
            new_logs,
            pending_txs,
            new_trades,
        }
    }

    /// Notify subscribers of a new block head.
    pub fn notify_new_block(&self, head: serde_json::Value) {
        let _ = self.new_heads.send(head);
    }

    /// Notify subscribers of a pending transaction.
    pub fn notify_pending_tx(&self, hash: B256) {
        let _ = self.pending_txs.send(hash);
    }

    /// Notify subscribers of new trades from a committed block.
    pub fn notify_new_trades(&self, trades: Vec<serde_json::Value>) {
        if !trades.is_empty() {
            let _ = self.new_trades.send(trades);
        }
    }
}

impl Default for BlockNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-sender transaction submission rate limiter (Batch EK: EVM-FIND-19).
/// Counts submissions at send_raw_transaction time, not at block commit time.
#[derive(Clone)]
pub struct TxSubmitLimiter {
    /// (count, window_start) per sender address.
    inner: Arc<Mutex<HashMap<alloy_primitives::Address, (u32, Instant)>>>,
    max_per_window: u32,
}

impl TxSubmitLimiter {
    pub fn new(max_per_window: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_per_window,
        }
    }

    /// Returns true if the sender is within rate limits. Increments the counter.
    pub fn check_sender(&self, sender: &alloy_primitives::Address) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        let entry = map.entry(*sender).or_insert((0, now));
        // 10-second sliding window
        if now.duration_since(entry.1) >= std::time::Duration::from_secs(10) {
            entry.0 = 0;
            entry.1 = now;
        }
        if entry.0 >= self.max_per_window {
            return false;
        }
        entry.0 += 1;
        true
    }
}

/// Shared state for all RPC handlers.
#[derive(Clone)]
pub struct RpcState {
    pub(crate) state: StateDb,
    pub(crate) mempool: Arc<Mempool>,
    pub(crate) executor: Arc<EvmExecutor>,
    pub(crate) chain_id: u64,
    pub(crate) epoch_length: u64,
    pub(crate) latest_height: Arc<AtomicU64>,
    pub(crate) notifier: BlockNotifier,
    /// Block height up to which historical data has been pruned (0 = archive mode).
    pub(crate) pruned_up_to: Arc<AtomicU64>,
    /// Per-sender tx submission rate limiter (Batch EK: EVM-FIND-19).
    pub(crate) tx_submit_limiter: TxSubmitLimiter,
    /// Global cap on active WebSocket subscriptions (HIGH-NEW-05).
    pub(crate) active_subscriptions: Arc<AtomicUsize>,
    /// Prometheus metrics handle.
    pub(crate) metrics: Option<Arc<torus_telemetry::Metrics>>,
}

/// JSON-RPC server combining eth, net, and web3 namespaces.
pub struct RpcServer {
    state: RpcState,
}

impl RpcServer {
    /// Create a new RPC server. Scans the DB to find the latest block height.
    pub fn new(
        state_db: StateDb,
        mempool: Arc<Mempool>,
        executor: Arc<EvmExecutor>,
        chain_id: u64,
        epoch_length: u64,
        notifier: BlockNotifier,
    ) -> Self {
        let latest = find_latest_height(&state_db);
        Self {
            state: RpcState {
                state: state_db,
                mempool,
                executor,
                chain_id,
                epoch_length,
                latest_height: Arc::new(AtomicU64::new(latest)),
                notifier,
                pruned_up_to: Arc::new(AtomicU64::new(0)),
                tx_submit_limiter: TxSubmitLimiter::new(50), // 50 tx per 10s per sender
                active_subscriptions: Arc::new(AtomicUsize::new(0)),
                metrics: None,
            },
        }
    }

    /// Use an externally-created height counter (shared with the commit handler).
    pub fn set_latest_height_handle(&mut self, handle: Arc<AtomicU64>) {
        self.state.latest_height = handle;
    }

    /// Get a shared handle to the latest block height atomic.
    pub fn latest_height(&self) -> Arc<AtomicU64> {
        self.state.latest_height.clone()
    }

    /// Set the pruned-up-to height for RPC error handling.
    pub fn set_pruned_up_to(&self, pruned: Arc<AtomicU64>) {
        let val = pruned.load(Ordering::Relaxed);
        self.state.pruned_up_to.store(val, Ordering::Relaxed);
    }

    /// Get a shared handle to the pruned-up-to atomic.
    pub fn pruned_up_to(&self) -> Arc<AtomicU64> {
        self.state.pruned_up_to.clone()
    }

    /// Set the Prometheus metrics handle for RPC instrumentation.
    pub fn set_metrics(&mut self, metrics: Arc<torus_telemetry::Metrics>) {
        self.state.metrics = Some(metrics);
    }

    /// Start the RPC server on the given address.
    pub async fn start(
        self,
        addr: SocketAddr,
    ) -> Result<(ServerHandle, SocketAddr), Box<dyn std::error::Error + Send + Sync>> {
        let layer = MetricsLayer {
            metrics: self.state.metrics.clone(),
        };
        let rpc_middleware = rpc_mw::RpcServiceBuilder::new().layer(layer);
        let server = ServerBuilder::default()
            .set_rpc_middleware(rpc_middleware)
            .build(addr)
            .await?;
        let local_addr = server.local_addr()?;
        let mut module = jsonrpsee::RpcModule::new(());
        module.merge(EthApiServer::into_rpc(self.state.clone()))?;
        module.merge(NetApiServer::into_rpc(self.state.clone()))?;
        module.merge(Web3ApiServer::into_rpc(self.state.clone()))?;
        module.merge(TorusApiServer::into_rpc(self.state.clone()))?;
        let handle = server.start(module);
        Ok((handle, local_addr))
    }

    /// Get a reference to the RPC state.
    pub fn state(&self) -> &RpcState {
        &self.state
    }
}

// ---------------------------------------------------------------------------
// RPC metrics middleware
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MetricsLayer {
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

impl<S: Clone> tower::Layer<S> for MetricsLayer {
    type Service = MetricsMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        MetricsMiddleware {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
struct MetricsMiddleware<S> {
    inner: S,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

impl<S> RpcServiceT for MetricsMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = rpc_mw::MethodResponse,
            BatchResponse = rpc_mw::MethodResponse,
            NotificationResponse = rpc_mw::MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = rpc_mw::MethodResponse;
    type BatchResponse = rpc_mw::MethodResponse;
    type NotificationResponse = rpc_mw::MethodResponse;

    fn call<'a>(
        &self,
        request: rpc_mw::Request<'a>,
    ) -> impl std::future::Future<Output = Self::MethodResponse> + Send + 'a {
        let metrics = self.metrics.clone();
        let method = request.method_name().to_string();
        let inner = self.inner.clone();
        let start = Instant::now();

        async move {
            let response = inner.call(request).await;
            if let Some(ref m) = metrics {
                let duration = start.elapsed();
                let status = if response.is_success() { "ok" } else { "error" };
                m.rpc_requests_total
                    .get_or_create(&vec![
                        ("method".into(), method),
                        ("status".into(), status.into()),
                    ])
                    .inc();
                m.rpc_request_duration_seconds
                    .observe(duration.as_secs_f64());
            }
            response
        }
    }

    fn batch<'a>(
        &self,
        batch: rpc_mw::Batch<'a>,
    ) -> impl std::future::Future<Output = Self::BatchResponse> + Send + 'a {
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        n: rpc_mw::Notification<'a>,
    ) -> impl std::future::Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

/// Find the latest block height using a reverse iterator on CF_BLOCK_HEADERS.
///
/// FIX 9 (EVM-FIND-14): Previous implementation scanned from block 0 upward (O(N)).
/// Now uses a reverse iterator to find the last key in O(1).
pub fn find_latest_height(state: &StateDb) -> u64 {
    let Ok(cf) = state.cf_handle(CF_BLOCK_HEADERS) else {
        return 0;
    };
    let mut iter = state.inner().iterator_cf(cf, rocksdb::IteratorMode::End);
    match iter.next() {
        Some(Ok((key, _))) if key.len() == 8 => {
            u64::from_be_bytes(key[..8].try_into().unwrap())
        }
        _ => 0,
    }
}

/// Public helper to update latest height after committing a new block.
pub fn set_latest_height(state: &RpcState, height: u64) {
    state.latest_height.store(height, Ordering::Relaxed);
}

/// Scan CF_NATIVE_TRADES for all trades at a given block height across all markets.
/// Used by the on_commit_block handler to feed the `new_trades` broadcast channel.
pub fn scan_trades_for_block(state: &StateDb, block_height: u64) -> Vec<serde_json::Value> {
    use borsh::BorshDeserialize;
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_TRADES};

    let db = state.inner();
    let market_cf = match db.cf_handle(CF_NATIVE_MARKETS) {
        Some(cf) => cf,
        None => return vec![],
    };
    let trade_cf = match db.cf_handle(CF_NATIVE_TRADES) {
        Some(cf) => cf,
        None => return vec![],
    };

    let height_bytes = block_height.to_be_bytes();
    let mut trades = Vec::new();

    // Iterate all known market IDs and seek into CF_NATIVE_TRADES for each.
    for item in db.iterator_cf(market_cf, rocksdb::IteratorMode::Start) {
        let (market_key, _) = match item {
            Ok(kv) => kv,
            Err(_) => continue,
        };
        if market_key.len() != 8 {
            continue;
        }

        // Build seek key: market_id(8) + block_height(8) + trade_index(0)
        let mut start_key = [0u8; 20];
        start_key[..8].copy_from_slice(&market_key);
        start_key[8..16].copy_from_slice(&height_bytes);

        let iter = db.iterator_cf(
            trade_cf,
            rocksdb::IteratorMode::From(&start_key, rocksdb::Direction::Forward),
        );
        for item in iter {
            let (key, value) = match item {
                Ok(kv) => kv,
                Err(_) => break,
            };
            if key.len() < 16 {
                break;
            }
            // Verify market_id prefix matches
            if key[..8] != market_key[..] {
                break;
            }
            // Verify block_number matches
            if key[8..16] != height_bytes[..] {
                break;
            }

            // Deserialize StoredTrade (borsh: trade_id u128 + price_raw i128 +
            // quantity_raw i128 + side u8 + block_number u64 + timestamp u64)
            #[derive(BorshDeserialize)]
            struct StoredTrade {
                trade_id: u128,
                price_raw: i128,
                quantity_raw: i128,
                side: u8,
                block_number: u64,
                timestamp: u64,
            }

            if let Ok(t) = StoredTrade::try_from_slice(&value) {
                let market_id = u64::from_be_bytes(market_key[..8].try_into().unwrap());
                trades.push(serde_json::json!({
                    "marketId": format!("0x{:x}", market_id),
                    "tradeId": format!("0x{:x}", t.trade_id),
                    "price": format!("0x{:x}", t.price_raw),
                    "quantity": format!("0x{:x}", t.quantity_raw),
                    "side": if t.side == 0 { "buy" } else { "sell" },
                    "blockNumber": format!("0x{:x}", t.block_number),
                    "timestamp": format!("0x{:x}", t.timestamp),
                }));
            }
        }
    }

    trades
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use alloy_primitives::{Address, Bloom, B256, U256};
    use revm::state::AccountInfo;
    use tempfile::TempDir;
    use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
    use torus_mempool::{Mempool, MempoolConfig};
    use torus_state::cf::{CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_RECEIPTS};
    use torus_types::{Receipt, TorusBlockBody, TorusBlockHeader};

    fn test_header(height: u64, gas_used: u64, base_fee: u64) -> TorusBlockHeader {
        TorusBlockHeader {
            height,
            timestamp: 1_700_000_000 + height,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            evm_gas_used: gas_used,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: base_fee,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        }
    }

    fn store_header(state: &StateDb, header: &TorusBlockHeader) -> B256 {
        // Match commit_block format: block_hash(32) || header_json.
        let json_bytes = serde_json::to_vec(header).unwrap();
        let hash = alloy_primitives::keccak256(&header.canonical_header_bytes());
        let mut data = Vec::with_capacity(32 + json_bytes.len());
        data.extend_from_slice(hash.as_slice());
        data.extend_from_slice(&json_bytes);
        state
            .put_cf_raw(CF_BLOCK_HEADERS, &header.height.to_be_bytes(), &data)
            .unwrap();
        state
            .put_cf_raw(
                torus_state::cf::CF_BLOCK_HASH_TO_NUMBER,
                hash.as_slice(),
                &header.height.to_be_bytes(),
            )
            .unwrap();
        hash
    }

    fn store_body(state: &StateDb, height: u64, body: &TorusBlockBody) {
        state
            .put_cf_raw(
                CF_BLOCK_BODIES,
                &height.to_be_bytes(),
                &serde_json::to_vec(body).unwrap(),
            )
            .unwrap();
    }

    fn store_receipt(state: &StateDb, height: u64, receipt: &Receipt) {
        let mut key = [0u8; 12];
        key[..8].copy_from_slice(&height.to_be_bytes());
        key[8..12].copy_from_slice(&receipt.tx_index.to_be_bytes());
        state
            .put_cf_raw(CF_RECEIPTS, &key, &serde_json::to_vec(receipt).unwrap())
            .unwrap();
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

    #[test]
    fn hex_encoding_roundtrip() {
        assert_eq!(hex_u64(0), "0x0");
        assert_eq!(hex_u64(255), "0xff");
        assert_eq!(hex_u64(7777), "0x1e61");
        assert_eq!(
            U256::from(42u64),
            parse_u256(&hex_u256(U256::from(42u64))).unwrap()
        );
        assert_eq!(
            Address::from([0xab; 20]),
            parse_address(&hex_address(Address::from([0xab; 20]))).unwrap()
        );
    }

    #[test]
    fn block_tag_parsing() {
        assert_eq!(resolve_block_tag("latest", 100).unwrap(), 100);
        assert_eq!(resolve_block_tag("earliest", 100).unwrap(), 0);
        assert_eq!(resolve_block_tag("pending", 100).unwrap(), 100);
        assert_eq!(resolve_block_tag("0xa", 100).unwrap(), 10);
    }

    #[tokio::test]
    async fn web3_client_version() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            client
                .request::<String, _>("web3_clientVersion", jsonrpsee::rpc_params![])
                .await
                .unwrap(),
            "torus/v0.1.0"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_chain_id() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            client
                .request::<String, _>("eth_chainId", jsonrpsee::rpc_params![])
                .await
                .unwrap(),
            "0x1e62"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_block_number() {
        let (_dir, state, mempool, executor) = setup();
        for i in 0..5u64 {
            store_header(&state, &test_header(i, 0, 1_000_000_000));
        }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            client
                .request::<String, _>("eth_blockNumber", jsonrpsee::rpc_params![])
                .await
                .unwrap(),
            "0x4"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance() {
        let (_dir, state, mempool, executor) = setup();
        let addr_val = Address::from([0x11; 20]);
        let balance = U256::from(1_000_000_000_000_000_000u128);
        state
            .put_account(
                &addr_val,
                &AccountInfo {
                    balance,
                    nonce: 5,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
        store_header(&state, &test_header(0, 0, 0));
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            parse_u256(
                &client
                    .request::<String, _>(
                        "eth_getBalance",
                        jsonrpsee::rpc_params![hex_address(addr_val), "latest"]
                    )
                    .await
                    .unwrap()
            )
            .unwrap(),
            balance
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_block_by_number() {
        let (_dir, state, mempool, executor) = setup();
        let block_hash = store_header(&state, &test_header(0, 21000, 1_000_000_000));
        store_body(
            &state,
            0,
            &TorusBlockBody {
                native_actions: vec![],
                evm_transactions: vec![],
                core_writer_actions: vec![],
            },
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let block: RpcBlock = client
            .request::<Option<RpcBlock>, _>(
                "eth_getBlockByNumber",
                jsonrpsee::rpc_params!["0x0", false],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(block.number, "0x0");
        assert_eq!(block.hash, hex_b256(block_hash));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_transaction_receipt() {
        let (_dir, state, mempool, executor) = setup();
        store_header(
            &state,
            &TorusBlockHeader {
                evm_tx_count: 1,
                ..test_header(0, 21000, 1_000_000_000)
            },
        );
        let tx_hash = B256::from([0xaa; 32]);
        store_receipt(
            &state,
            0,
            &Receipt {
                tx_hash,
                block_number: 0,
                block_hash: B256::ZERO,
                tx_index: 0,
                cumulative_gas_used: 21000,
                gas_used: 21000,
                contract_address: None,
                logs: vec![],
                logs_bloom: Bloom::ZERO,
                status: true,
                effective_gas_price: 1_000_000_000,
            },
        );
        state
            .put_cf_raw(
                torus_state::cf::CF_TX_HASH_TO_LOCATION,
                tx_hash.as_slice(),
                &[0u8; 12],
            )
            .unwrap();
        store_body(
            &state,
            0,
            &TorusBlockBody {
                native_actions: vec![],
                evm_transactions: vec![],
                core_writer_actions: vec![],
            },
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let r: RpcReceipt = client
            .request::<Option<RpcReceipt>, _>(
                "eth_getTransactionReceipt",
                jsonrpsee::rpc_params![hex_b256(tx_hash)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.transaction_hash, hex_b256(tx_hash));
        assert_eq!(r.status, "0x1");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_call_simple_transfer() {
        let (_dir, state, mempool, executor) = setup();
        let sender = Address::from([0x11; 20]);
        state
            .put_account(
                &sender,
                &AccountInfo {
                    balance: U256::from(10u64.pow(18)),
                    nonce: 0,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
        store_header(&state, &test_header(0, 0, 0));
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let call = serde_json::json!({ "from": hex_address(sender), "to": hex_address(Address::from([0x22; 20])), "value": "0x0" });
        assert_eq!(
            client
                .request::<String, _>("eth_call", jsonrpsee::rpc_params![call, "latest"])
                .await
                .unwrap(),
            "0x"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_logs_with_filter() {
        let (_dir, state, mempool, executor) = setup();
        let log_addr = Address::from([0x33; 20]);
        let topic0 = B256::from([0x44; 32]);
        store_header(
            &state,
            &TorusBlockHeader {
                evm_tx_count: 1,
                ..test_header(0, 21000, 1_000_000_000)
            },
        );
        store_receipt(
            &state,
            0,
            &Receipt {
                tx_hash: B256::from([0x55; 32]),
                block_number: 0,
                block_hash: B256::ZERO,
                tx_index: 0,
                cumulative_gas_used: 21000,
                gas_used: 21000,
                contract_address: None,
                logs: vec![torus_types::Log {
                    address: log_addr,
                    topics: vec![topic0],
                    data: vec![1, 2, 3],
                }],
                logs_bloom: Bloom::ZERO,
                status: true,
                effective_gas_price: 1_000_000_000,
            },
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let filter = serde_json::json!({ "fromBlock": "0x0", "toBlock": "0x0", "address": hex_address(log_addr), "topics": [hex_b256(topic0)] });
        let logs: Vec<RpcLog> = client
            .request("eth_getLogs", jsonrpsee::rpc_params![filter])
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_fee_history() {
        let (_dir, state, mempool, executor) = setup();
        for i in 0..5u64 {
            store_header(&state, &test_header(i, i * 1000, 1_000_000_000 + i));
        }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let result: FeeHistory = client
            .request(
                "eth_feeHistory",
                jsonrpsee::rpc_params!["0x3", "0x4", [25.0, 75.0]],
            )
            .await
            .unwrap();
        assert_eq!(result.oldest_block, hex_u64(2));
        assert_eq!(result.gas_used_ratio.len(), 3);
        assert_eq!(result.base_fee_per_gas.len(), 4);
        handle.stop().unwrap();
    }

    // ========================================================================
    // Torus namespace tests (2.9.1 + 2.9.2)
    // ========================================================================

    use borsh::BorshSerialize;
    use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
    use torus_core::precompiles::{write_order_book_snapshot, OrderBookSnapshot, PriceLevel};
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_TRADES};
    use torus_types::FixedPoint;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn store_market(state: &StateDb, market_id: u64, base: &str, quote: &str) {
        let mut data = Vec::new();
        BorshSerialize::serialize(&base.to_string(), &mut data).unwrap();
        BorshSerialize::serialize(&quote.to_string(), &mut data).unwrap();
        let lot_raw: i128 = FixedPoint::SCALE; // 1.0
        let tick_raw: i128 = FixedPoint::SCALE / 100; // 0.01
        let margin_raw: i128 = FixedPoint::SCALE / 10; // 0.1
        BorshSerialize::serialize(&lot_raw, &mut data).unwrap();
        BorshSerialize::serialize(&tick_raw, &mut data).unwrap();
        BorshSerialize::serialize(&margin_raw, &mut data).unwrap();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, &market_id.to_be_bytes(), &data)
            .unwrap();
    }

    fn store_trade(
        state: &StateDb,
        market_id: u64,
        trade_id: u128,
        price_raw: i128,
        qty_raw: i128,
        side: u8,
        block: u64,
        ts: u64,
        index: u32,
    ) {
        let mut key = Vec::with_capacity(20);
        key.extend_from_slice(&market_id.to_be_bytes());
        key.extend_from_slice(&block.to_be_bytes());
        key.extend_from_slice(&index.to_be_bytes());

        let mut data = Vec::new();
        BorshSerialize::serialize(&trade_id, &mut data).unwrap();
        BorshSerialize::serialize(&price_raw, &mut data).unwrap();
        BorshSerialize::serialize(&qty_raw, &mut data).unwrap();
        BorshSerialize::serialize(&side, &mut data).unwrap();
        BorshSerialize::serialize(&block, &mut data).unwrap();
        BorshSerialize::serialize(&ts, &mut data).unwrap();
        state.put_cf_raw(CF_NATIVE_TRADES, &key, &data).unwrap();
    }

    #[tokio::test]
    async fn torus_get_order_book_with_orders() {
        let (_dir, state, mempool, executor) = setup();
        let snapshot = OrderBookSnapshot {
            bids: vec![
                PriceLevel {
                    price: fp(50000),
                    quantity: fp(10),
                },
                PriceLevel {
                    price: fp(49900),
                    quantity: fp(5),
                },
            ],
            asks: vec![
                PriceLevel {
                    price: fp(50100),
                    quantity: fp(8),
                },
                PriceLevel {
                    price: fp(50200),
                    quantity: fp(3),
                },
            ],
        };
        write_order_book_snapshot(&state, 1, &snapshot).unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let book: RpcOrderBook = client
            .request("torus_getOrderBook", jsonrpsee::rpc_params!["0x1"])
            .await
            .unwrap();
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.bids[0].price, hex_fp(fp(50000)));
        assert_eq!(book.asks[0].price, hex_fp(fp(50100)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_order_book_empty() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let book: RpcOrderBook = client
            .request("torus_getOrderBook", jsonrpsee::rpc_params!["0x99"])
            .await
            .unwrap();
        assert!(book.bids.is_empty());
        assert!(book.asks.is_empty());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_position_open() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x11; 20]);
        let pm = PositionManager::new(state.clone());
        pm.put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(5),
            entry_price: fp(50000),
            realized_pnl: fp(100),
            isolated_margin: fp(2500),
            margin_type: MarginType::Isolated,
        })
        .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let pos: Option<RpcPosition> = client
            .request(
                "torus_getPosition",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        let pos = pos.unwrap();
        assert_eq!(pos.side, "long");
        assert_eq!(pos.size, hex_fp(fp(5)));
        assert_eq!(pos.entry_price, hex_fp(fp(50000)));
        assert_eq!(pos.realized_pnl, hex_fp(fp(100)));
        assert_eq!(pos.margin_mode, "isolated");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_position_none() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x22; 20]);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let pos: Option<RpcPosition> = client
            .request(
                "torus_getPosition",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        assert!(pos.is_none());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_balances_basic() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x33; 20]);
        // Set native balance
        let pm = PositionManager::new(state.clone());
        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(10000),
                order_margin: fp(500),
            },
        )
        .unwrap();
        // Set EVM balance
        state
            .put_account(
                &trader,
                &AccountInfo {
                    balance: U256::from(2_000_000_000_000_000_000u128),
                    nonce: 0,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let bal: RpcBalances = client
            .request(
                "torus_getBalances",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        // native_balance = available + order_margin = 10500
        assert_eq!(bal.native_balance, hex_fp(fp(10000) + fp(500)));
        assert_eq!(
            bal.evm_balance,
            hex_u256(U256::from(2_000_000_000_000_000_000u128))
        );
        // total_margin_used = order_margin (no positions)
        assert_eq!(bal.total_margin_used, hex_fp(fp(500)));
        assert_eq!(bal.available_balance, hex_fp(fp(10000)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_balances_with_margin() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x44; 20]);
        let pm = PositionManager::new(state.clone());
        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(8000),
                order_margin: fp(1000),
            },
        )
        .unwrap();
        // Open position with isolated margin
        pm.put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(2),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(500),
            margin_type: MarginType::Isolated,
        })
        .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let bal: RpcBalances = client
            .request(
                "torus_getBalances",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        // total_margin_used = order_margin(1000) + isolated(500) = 1500
        assert_eq!(bal.total_margin_used, hex_fp(fp(1500)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_markets() {
        let (_dir, state, mempool, executor) = setup();
        store_market(&state, 1, "BTC", "USD");
        store_market(&state, 2, "ETH", "USD");
        store_market(&state, 3, "SOL", "USD");
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let markets: Vec<RpcMarketInfo> = client
            .request("torus_getMarkets", jsonrpsee::rpc_params![])
            .await
            .unwrap();
        assert_eq!(markets.len(), 3);
        assert_eq!(markets[0].base_asset, "BTC");
        assert_eq!(markets[1].base_asset, "ETH");
        assert_eq!(markets[2].base_asset, "SOL");
        assert_eq!(markets[0].status, "active");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_trade_history() {
        let (_dir, state, mempool, executor) = setup();
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        store_trade(&state, 1, 100, price, qty, 0, 10, 1700000010, 0);
        store_trade(&state, 1, 101, price + FixedPoint::SCALE, qty, 1, 11, 1700000011, 0);
        store_trade(&state, 1, 102, price - FixedPoint::SCALE, qty, 0, 12, 1700000012, 0);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcTrade> = client
            .request(
                "torus_getTradeHistory",
                jsonrpsee::rpc_params!["0x1", 100u32],
            )
            .await
            .unwrap();
        assert_eq!(trades.len(), 3);
        // Most recent first
        assert_eq!(trades[0].trade_id, hex_u128(102));
        assert_eq!(trades[1].trade_id, hex_u128(101));
        assert_eq!(trades[2].trade_id, hex_u128(100));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_trade_history_with_limit() {
        let (_dir, state, mempool, executor) = setup();
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        for i in 0..5u32 {
            store_trade(
                &state,
                1,
                i as u128,
                price,
                qty,
                0,
                i as u64,
                1700000000 + i as u64,
                0,
            );
        }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcTrade> = client
            .request(
                "torus_getTradeHistory",
                jsonrpsee::rpc_params!["0x1", 2u32],
            )
            .await
            .unwrap();
        assert_eq!(trades.len(), 2);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_invalid_market_id() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let result = client
            .request::<RpcOrderBook, _>("torus_getOrderBook", jsonrpsee::rpc_params!["not_hex"])
            .await;
        assert!(result.is_err());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_invalid_trader_address() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let result = client
            .request::<Option<RpcPosition>, _>(
                "torus_getPosition",
                jsonrpsee::rpc_params!["invalid_addr", "0x1"],
            )
            .await;
        assert!(result.is_err());
        handle.stop().unwrap();
    }

    // ========================================================================
    // Trading app RPC tests
    // ========================================================================

    use torus_core::order_book::OrderBook;
    use torus_state::cf::{CF_NATIVE_ORDER_BOOKS, CF_NATIVE_USER_TRADES};
    use torus_types::{OrderType, PlaceOrderParams, Side, TimeInForce};

    fn store_order_book_with_orders(
        state: &StateDb,
        market_id: u64,
        orders: &[(Address, Side, i64, i64)],
    ) {
        let mut book = OrderBook::new(market_id, fp(1), fp(1));
        for (trader, side, price, qty) in orders {
            book.place_order(
                PlaceOrderParams {
                    market_id, is_buy: *side == Side::Buy, price: fp(*price), quantity: fp(*qty),
                    order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
                    reduce_only: false, client_order_id: None,
                },
                *trader, 1700000000,
            );
        }
        let data = borsh::to_vec(&book).unwrap();
        state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes(), &data).unwrap();
    }

    fn store_user_trade(
        state: &StateDb, trader: &Address, trade_id: u128, market_id: u64,
        price_raw: i128, qty_raw: i128, side: u8, role: u8, block: u64, ts: u64, index: u32,
    ) {
        let desc_block = u64::MAX - block;
        let mut key = [0u8; 32];
        key[..20].copy_from_slice(trader.as_slice());
        key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        key[28..32].copy_from_slice(&index.to_be_bytes());
        let mut data = Vec::with_capacity(74);
        data.extend_from_slice(&trade_id.to_le_bytes());
        data.extend_from_slice(&market_id.to_le_bytes());
        data.extend_from_slice(&price_raw.to_le_bytes());
        data.extend_from_slice(&qty_raw.to_le_bytes());
        data.push(side);
        data.push(role);
        data.extend_from_slice(&block.to_le_bytes());
        data.extend_from_slice(&ts.to_le_bytes());
        state.put_cf_raw(CF_NATIVE_USER_TRADES, &key, &data).unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_orders_empty() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(Address::from([0xAA; 20]))])
            .await.unwrap();
        assert!(orders.is_empty());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_orders_single_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xBB; 20]);
        store_order_book_with_orders(&state, 1, &[
            (trader, Side::Buy, 49000, 2),
            (trader, Side::Buy, 48000, 3),
            (trader, Side::Sell, 51000, 1),
        ]);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(trader), "0x1"])
            .await.unwrap();
        assert_eq!(orders.len(), 3);
        assert_eq!(orders[0].order_type, "limit");
        assert_eq!(orders[0].time_in_force, "gtc");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_orders_filter_by_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xCC; 20]);
        store_order_book_with_orders(&state, 1, &[(trader, Side::Buy, 49000, 2)]);
        store_order_book_with_orders(&state, 2, &[
            (trader, Side::Buy, 2900, 5), (trader, Side::Sell, 3100, 3),
        ]);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(trader), "0x2"])
            .await.unwrap();
        assert_eq!(orders.len(), 2);
        let all: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(trader)])
            .await.unwrap();
        assert_eq!(all.len(), 3);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_interest_basic() {
        let (_dir, state, mempool, executor) = setup();
        let pm = PositionManager::new(state.clone());
        pm.put_position(&Position {
            trader: Address::from([0x11; 20]), market_id: 1, is_long: true, size: fp(10),
            entry_price: fp(50000), realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(5000), margin_type: MarginType::Cross,
        }).unwrap();
        pm.put_position(&Position {
            trader: Address::from([0x22; 20]), market_id: 1, is_long: false, size: fp(7),
            entry_price: fp(50000), realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(3500), margin_type: MarginType::Cross,
        }).unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let oi: RpcOpenInterest = client
            .request("torus_getOpenInterest", jsonrpsee::rpc_params!["0x1"])
            .await.unwrap();
        assert_eq!(oi.long_oi, hex_fp(fp(10)));
        assert_eq!(oi.short_oi, hex_fp(fp(7)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_mark_price_from_order_book() {
        let (_dir, state, mempool, executor) = setup();
        let mut book = OrderBook::new(1, fp(1), fp(1));
        book.place_order(PlaceOrderParams {
            market_id: 1, is_buy: true, price: fp(50000), quantity: fp(1),
            order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
            reduce_only: false, client_order_id: None,
        }, Address::from([0x11; 20]), 1700000000);
        book.place_order(PlaceOrderParams {
            market_id: 1, is_buy: false, price: fp(50000), quantity: fp(1),
            order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
            reduce_only: false, client_order_id: None,
        }, Address::from([0x22; 20]), 1700000001);
        state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes(), &borsh::to_vec(&book).unwrap()).unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let mp: RpcMarkPrice = client
            .request("torus_getMarkPrice", jsonrpsee::rpc_params!["0x1"])
            .await.unwrap();
        assert_eq!(mp.last_trade_price, hex_fp(fp(50000)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_user_trades_basic() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xDD; 20]);
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        store_user_trade(&state, &trader, 1, 1, price, qty, 0, 1, 10, 1700000010, 0);
        store_user_trade(&state, &trader, 2, 1, price, qty, 1, 0, 11, 1700000011, 0);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let trades: Vec<RpcUserTrade> = client
            .request("torus_getUserTrades", jsonrpsee::rpc_params![hex_address(trader)])
            .await.unwrap();
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].trade_id, hex_u128(2));
        assert_eq!(trades[1].trade_id, hex_u128(1));
        assert_eq!(trades[0].role, "maker");
        assert_eq!(trades[1].role, "taker");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_user_trades_filter_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xEE; 20]);
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        store_user_trade(&state, &trader, 1, 1, price, qty, 0, 1, 10, 1700000010, 0);
        store_user_trade(&state, &trader, 2, 2, price, qty, 0, 1, 11, 1700000011, 0);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let trades: Vec<RpcUserTrade> = client
            .request("torus_getUserTrades", jsonrpsee::rpc_params![hex_address(trader), "0x1"])
            .await.unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].market_id, hex_u64(1));
        handle.stop().unwrap();
    }
    // ========================================================================
    // 7 pending trading RPC tests (from writing-plan-trading-rpc-gaps.md)
    // ========================================================================

    // --- get_open_orders_multi_market ---
    // Unfiltered query returns orders spanning multiple markets.
    #[tokio::test]
    async fn get_open_orders_multi_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x11; 20]);
        // Place orders in three distinct markets.
        store_order_book_with_orders(&state, 1, &[(trader, Side::Buy, 49000, 2)]);
        store_order_book_with_orders(&state, 2, &[(trader, Side::Sell, 3100, 1)]);
        store_order_book_with_orders(&state, 3, &[
            (trader, Side::Buy, 1900, 5),
            (trader, Side::Sell, 2100, 3),
        ]);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        // No market_id param → all markets.
        let orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(trader)])
            .await.unwrap();
        assert_eq!(orders.len(), 4);
        // All must belong to the requesting trader; market IDs should span 1, 2, and 3.
        let mut market_ids: Vec<u64> = orders.iter()
            .map(|o| u64::from_str_radix(o.market_id.strip_prefix("0x").unwrap(), 16).unwrap())
            .collect();
        market_ids.sort_unstable();
        market_ids.dedup();
        assert_eq!(market_ids, vec![1, 2, 3]);
        handle.stop().unwrap();
    }

    // --- get_open_orders_after_cancel ---
    // A cancelled order must not appear in subsequent getOpenOrders responses.
    #[tokio::test]
    async fn get_open_orders_after_cancel() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x22; 20]);
        // Build a book with two buy orders.
        let mut book = OrderBook::new(1, fp(1), fp(1));
        let r1 = book.place_order(PlaceOrderParams {
            market_id: 1, is_buy: true, price: fp(48000), quantity: fp(2),
            order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
            reduce_only: false, client_order_id: None,
        }, trader, 1700000000);
        book.place_order(PlaceOrderParams {
            market_id: 1, is_buy: true, price: fp(47000), quantity: fp(3),
            order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
            reduce_only: false, client_order_id: None,
        }, trader, 1700000001);
        // Cancel the first order before persisting.
        book.cancel_order(r1.order_id).unwrap();
        let data = borsh::to_vec(&book).unwrap();
        state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes(), &data).unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(trader), "0x1"])
            .await.unwrap();
        // Only the second order should remain.
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].price, hex_fp(fp(47000)));
        handle.stop().unwrap();
    }

    // --- get_open_orders_after_fill ---
    // A fully-filled order must not appear in getOpenOrders results.
    #[tokio::test]
    async fn get_open_orders_after_fill() {
        let (_dir, state, mempool, executor) = setup();
        let maker = Address::from([0x33; 20]);
        let taker = Address::from([0x44; 20]);
        // Build book: maker places a sell resting on the book first.
        let mut book = OrderBook::new(1, fp(1), fp(1));
        book.place_order(PlaceOrderParams {
            market_id: 1, is_buy: false, price: fp(50000), quantity: fp(1),
            order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
            reduce_only: false, client_order_id: None,
        }, maker, 1700000000);
        // Taker buy at the same price fully fills the maker's sell.
        book.place_order(PlaceOrderParams {
            market_id: 1, is_buy: true, price: fp(50000), quantity: fp(1),
            order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
            reduce_only: false, client_order_id: None,
        }, taker, 1700000001);
        // Persist the post-fill book state.
        let data = borsh::to_vec(&book).unwrap();
        state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes(), &data).unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        // Neither party should have open orders after the complete fill.
        let maker_orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(maker), "0x1"])
            .await.unwrap();
        assert!(maker_orders.is_empty(), "maker's filled sell should be gone");
        let taker_orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(taker), "0x1"])
            .await.unwrap();
        assert!(taker_orders.is_empty(), "taker's filled buy should be gone");
        handle.stop().unwrap();
    }

    // --- get_open_orders_limit_500 ---
    // The endpoint must not return more than 500 orders regardless of how many
    // are in the book.
    //
    // The order book enforces MAX_ORDERS_PER_TRADER_PER_MARKET = 200, so we
    // spread orders across three markets (200 + 200 + 101 = 501) to produce
    // more than 500 total and verify the RPC cap fires.
    #[tokio::test]
    async fn get_open_orders_limit_500() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x55; 20]);

        // Markets 1 and 2: 200 buy orders each (hits per-market trader cap).
        for market_id in [1u64, 2u64] {
            let mut book = OrderBook::new(market_id, fp(1), fp(1));
            for i in 1u32..=200 {
                book.place_order(PlaceOrderParams {
                    market_id, is_buy: true,
                    price: fp(i as i64 * 100),
                    quantity: fp(1),
                    order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
                    reduce_only: false, client_order_id: None,
                }, trader, 1_700_000_000 + i as u64);
            }
            let data = borsh::to_vec(&book).unwrap();
            state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes(), &data).unwrap();
        }

        // Market 3: 101 sell orders — brings the total to 501.
        let mut book3 = OrderBook::new(3, fp(1), fp(1));
        for i in 1u32..=101 {
            book3.place_order(PlaceOrderParams {
                market_id: 3, is_buy: false,
                price: fp(i as i64 * 100 + 500_000),
                quantity: fp(1),
                order_type: OrderType::Limit, time_in_force: TimeInForce::GTC,
                reduce_only: false, client_order_id: None,
            }, trader, 1_700_000_000 + i as u64);
        }
        let data3 = borsh::to_vec(&book3).unwrap();
        state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &3u64.to_be_bytes(), &data3).unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request("torus_getOpenOrders", jsonrpsee::rpc_params![hex_address(trader)])
            .await.unwrap();
        // Must be capped at 500 — never 501.
        assert_eq!(orders.len(), 500);
        handle.stop().unwrap();
    }

    // --- get_open_interest_no_positions ---
    // An empty market returns zero long OI and zero short OI.
    #[tokio::test]
    async fn get_open_interest_no_positions() {
        let (_dir, state, mempool, executor) = setup();
        // No positions written to state — market 99 is entirely empty.
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let oi: RpcOpenInterest = client
            .request("torus_getOpenInterest", jsonrpsee::rpc_params!["0x63"])
            .await.unwrap();
        assert_eq!(oi.long_oi, hex_fp(FixedPoint::ZERO));
        assert_eq!(oi.short_oi, hex_fp(FixedPoint::ZERO));
        handle.stop().unwrap();
    }

    // --- get_mark_price_from_oracle ---
    // When an aggregated oracle price exists in CF_NATIVE_ORACLE the endpoint
    // must return it as both mark_price and index_price.  This is distinct
    // from the order-book-last-trade path tested by torus_get_mark_price_from_order_book.
    #[tokio::test]
    async fn get_mark_price_from_oracle() {
        use torus_state::cf::CF_NATIVE_ORACLE;
        let (_dir, state, mempool, executor) = setup();
        let market_id: u64 = 7;
        let oracle_price = fp(42000);

        // Write a StoredAggregatedPrice entry for market 7.
        // Binary layout: price(i128 16 BE) + block_number(u64 8 BE) + num_reporters(u32 4 BE).
        let mut key = Vec::with_capacity(11);
        key.extend_from_slice(b"agg");
        key.extend_from_slice(&market_id.to_be_bytes());
        let mut value = Vec::with_capacity(28);
        value.extend_from_slice(&oracle_price.raw().to_be_bytes());
        value.extend_from_slice(&1u64.to_be_bytes()); // block_number
        value.extend_from_slice(&3u32.to_be_bytes()); // num_reporters
        state.put_cf_raw(CF_NATIVE_ORACLE, &key, &value).unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let mp: RpcMarkPrice = client
            .request("torus_getMarkPrice", jsonrpsee::rpc_params!["0x7"])
            .await.unwrap();
        // Oracle price must surface as mark_price and index_price.
        assert_eq!(mp.mark_price, hex_fp(oracle_price));
        assert_eq!(mp.index_price, hex_fp(oracle_price));
        // No trades occurred in this book so last_trade_price must be zero.
        assert_eq!(mp.last_trade_price, hex_fp(FixedPoint::ZERO));
        handle.stop().unwrap();
    }

    // --- get_user_trades_newest_first ---
    // Trades must be returned in descending block order (newest block first).
    #[tokio::test]
    async fn get_user_trades_newest_first() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xFF; 20]);
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty   =     1i128 * FixedPoint::SCALE;
        // Insert three trades at blocks 5, 20, and 10 (out of order).
        store_user_trade(&state, &trader, 100, 1, price, qty, 0, 1,  5, 1_700_000_005, 0);
        store_user_trade(&state, &trader, 200, 1, price, qty, 1, 0, 20, 1_700_000_020, 0);
        store_user_trade(&state, &trader, 300, 1, price, qty, 0, 1, 10, 1_700_000_010, 0);

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}")).unwrap();
        let trades: Vec<RpcUserTrade> = client
            .request("torus_getUserTrades", jsonrpsee::rpc_params![hex_address(trader)])
            .await.unwrap();
        assert_eq!(trades.len(), 3);
        // Verify descending block order: block 20, then 10, then 5.
        let blocks: Vec<u64> = trades.iter()
            .map(|t| u64::from_str_radix(t.block_number.strip_prefix("0x").unwrap(), 16).unwrap())
            .collect();
        assert_eq!(blocks, vec![20, 10, 5]);
        // Verify trade IDs match the expected block ordering.
        assert_eq!(trades[0].trade_id, hex_u128(200));
        assert_eq!(trades[1].trade_id, hex_u128(300));
        assert_eq!(trades[2].trade_id, hex_u128(100));
        handle.stop().unwrap();
    }

}